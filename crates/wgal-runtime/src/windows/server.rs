//! The watchdog's named pipe server (plan §3.2, §9.2).
//!
//! # Who may connect
//!
//! The pipe's DACL ([`AccessList::pipe_dacl`]) already keeps out network
//! and anonymous logons and every account that is not listed; the server
//! checks again itself. Each connection first establishes its peer from the
//! client's token ([`PipeInstance::client_identity`]). A peer that is
//! neither LocalSystem, nor an elevated administrator, nor a listed user is
//! disconnected before anything it sent is read. The identity never travels
//! on the wire: the handler receives it with every request, and the runtime
//! decides what each peer may do.
//!
//! The identity is the token's, never the client's process ID. A process ID
//! (`GetNamedPipeClientProcessId`) only names a process, which may already
//! have exited and had its number reused by another, and checking that
//! process's token later says nothing certain about who opened the pipe.
//! Windows captures the client's security context when it opens the pipe,
//! and that context is what the server reads. Clients open the pipe at
//! `SecurityIdentification`
//! ([`PIPE_CLIENT_QOS`](wgal_platform::windows::PIPE_CLIENT_QOS)): enough
//! for the server to read the token, never to act as the client.
//!
//! # Connections
//!
//! At most [`ServerLimits::max_connections`] connections are served at once,
//! each on its own thread; another client waits in the pipe until one ends.
//! A connection reads one request at a time: a frame (`codec::read_frame`)
//! that must arrive completely within [`ServerLimits::idle_timeout`] of the
//! previous answer, or the idle client is dropped; then
//! `codec::decode_request`, the handler, `codec::encode_response`, and the
//! answer written as one frame within [`ServerLimits::write_timeout`]. A
//! request that does not decode is answered with a refusal carrying its
//! request ID when the ID can be read, and the connection goes on. A frame
//! that is empty, oversized, cut short or late ends the connection without
//! an answer: the stream can no longer be trusted to be in step. Nothing a
//! client sends is logged.
//!
//! # Stopping
//!
//! [`ServerHandle::stop`] ends a pending accept and every pending read and
//! write at once, and [`Server::run`] returns once every connection thread
//! has ended.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_platform::windows::{
    MAX_PIPE_INSTANCES, PipeError, PipeInstance, PipeListener, PipeStopper, TokenIdentity,
};
use wgal_protocol::{
    EncodeError, ErrorCode, ErrorReason, FrameError, Outcome, PROTOCOL_VERSION, ProtocolError,
    RequestEnvelope, RequestId, ResponseEnvelope, WINDOWS_PIPE_NAME, codec,
};

use super::access::AccessList;

/// How often a server with every connection busy checks whether it was
/// stopped.
const SLOT_POLL: Duration = Duration::from_millis(50);

/// Bounds for one server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLimits {
    /// Connections served at once: 1 to one less than
    /// [`MAX_PIPE_INSTANCES`], since one more instance always waits for the
    /// next client.
    pub max_connections: u32,
    /// Longest wait for the next whole request on a connection.
    pub idle_timeout: Duration,
    /// Longest wait for an answer to be taken by the pipe.
    pub write_timeout: Duration,
}

impl ServerLimits {
    pub const DEFAULT: Self = Self {
        // A GUI per signed-in session, with room for a reconnect.
        max_connections: 8,
        // The GUI asks for a snapshot every 5 s (plan §6.2); a connection
        // quiet for six of those is gone or stuck.
        idle_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(5),
    };

    fn valid(&self) -> bool {
        (1..MAX_PIPE_INSTANCES).contains(&self.max_connections)
            && !self.idle_timeout.is_zero()
            && !self.write_timeout.is_zero()
    }
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Who is on the other end of a connection, from the token Windows captured
/// when the client opened the pipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The token's user, as a canonical SID string.
    pub account: String,
    pub local_system: bool,
    /// The token is elevated and has the Administrators group enabled.
    pub elevated_admin: bool,
    /// The account is on the installation's access list.
    pub listed: bool,
}

impl PeerIdentity {
    pub fn new(token: &TokenIdentity, access: &AccessList) -> Self {
        Self {
            account: token.sid.clone(),
            local_system: token.local_system,
            elevated_admin: token.elevated && token.administrator,
            listed: access.lists_user(&token.sid),
        }
    }

    /// LocalSystem, an elevated administrator or a listed user. Anyone else
    /// is disconnected before a request is read.
    pub fn admitted(&self) -> bool {
        self.local_system || self.elevated_admin || self.listed
    }
}

#[derive(Debug)]
pub enum ServerError {
    /// [`ServerLimits`] out of range.
    InvalidLimits,
    Pipe(PipeError),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => write!(f, "pipe server limits out of range"),
            Self::Pipe(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ServerError {}

/// What one server has done so far, for diagnostics. Counts only: nothing a
/// client sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerStats {
    /// Connections whose peer was admitted.
    pub admitted: u64,
    /// Connections closed before anything was read: the peer was not
    /// admitted, or its identity could not be read.
    pub refused: u64,
    /// Requests answered, refusals included.
    pub requests: u64,
    /// Connections dropped because no whole request came in time.
    pub idle_timeouts: u64,
    /// Connections dropped for a frame that was empty, oversized or cut
    /// short, an answer the pipe did not take in time, an I/O error, or a
    /// thread that could not be started.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    admitted: AtomicU64,
    refused: AtomicU64,
    requests: AtomicU64,
    idle_timeouts: AtomicU64,
    dropped: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

impl Counters {
    fn snapshot(&self) -> ServerStats {
        ServerStats {
            admitted: self.admitted.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            idle_timeouts: self.idle_timeouts.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

/// Stops a running [`Server`] and reads its counts, from any thread.
#[derive(Clone, Debug)]
pub struct ServerHandle {
    stopper: PipeStopper,
    counters: Arc<Counters>,
}

impl ServerHandle {
    pub fn stop(&self) {
        self.stopper.stop();
    }

    pub fn stats(&self) -> ServerStats {
        self.counters.snapshot()
    }
}

/// A bound pipe, ready to [`run`](Self::run).
#[derive(Debug)]
pub struct Server {
    listener: PipeListener,
    access: Arc<AccessList>,
    limits: ServerLimits,
    counters: Arc<Counters>,
}

impl Server {
    /// The watchdog's own pipe: [`WINDOWS_PIPE_NAME`], owned by SYSTEM with
    /// the DACL for `access` ([`AccessList::pipe_sddl`]). Only a process
    /// running as SYSTEM can create it.
    pub fn watchdog(access: AccessList, limits: ServerLimits) -> Result<Self, ServerError> {
        let sddl = access.pipe_sddl();
        Self::bind(WINDOWS_PIPE_NAME, &sddl, access, limits)
    }

    /// Creates the pipe `name` with the security descriptor `sddl`. Fails
    /// with [`PipeError::NameInUse`] while any other process holds the name.
    pub fn bind(
        name: &str,
        sddl: &str,
        access: AccessList,
        limits: ServerLimits,
    ) -> Result<Self, ServerError> {
        if !limits.valid() {
            return Err(ServerError::InvalidLimits);
        }
        let listener = PipeListener::bind(name, sddl, limits.max_connections + 1)
            .map_err(ServerError::Pipe)?;
        Ok(Self {
            listener,
            access: Arc::new(access),
            limits,
            counters: Arc::default(),
        })
    }

    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            stopper: self.listener.stopper(),
            counters: Arc::clone(&self.counters),
        }
    }

    /// Serves connections until stopped, calling `handler` with each
    /// admitted request and its peer. Returns once every connection thread
    /// has ended. An error accepting connections (the pipe could no longer
    /// be served) stops the open connections too and is returned.
    pub fn run<H>(mut self, handler: H) -> Result<(), ServerError>
    where
        H: Fn(&PeerIdentity, RequestEnvelope) -> ResponseEnvelope + Send + Sync + 'static,
    {
        let stopper = self.listener.stopper();
        let slots = Arc::new(Slots {
            free: Mutex::new(self.limits.max_connections),
            freed: Condvar::new(),
        });
        let connection = Arc::new(Connection {
            access: Arc::clone(&self.access),
            limits: self.limits,
            counters: Arc::clone(&self.counters),
            handler,
        });
        let mut threads: Vec<JoinHandle<()>> = Vec::new();
        let result = loop {
            threads.retain(|thread| !thread.is_finished());
            let Some(slot) = Slots::acquire(&slots, &stopper) else {
                break Ok(());
            };
            let pipe = match self.listener.accept() {
                Ok(Some(pipe)) => pipe,
                Ok(None) => break Ok(()),
                Err(error) => {
                    stopper.stop();
                    break Err(ServerError::Pipe(error));
                }
            };
            let connection = Arc::clone(&connection);
            let spawned = thread::Builder::new()
                .name("wgal-pipe-connection".to_owned())
                .spawn(move || {
                    connection.serve(pipe);
                    // Released only once the instance is closed, so the
                    // connections never outnumber the pipe's instances.
                    drop(slot);
                });
            match spawned {
                Ok(thread) => threads.push(thread),
                // The closure, with the pipe, is dropped: the client sees
                // the end of the stream.
                Err(_) => bump(&self.counters.dropped),
            }
        };
        for thread in threads {
            // A connection that panicked has already closed its pipe.
            let _ = thread.join();
        }
        result
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Free connection slots. One is held from before an accept until the
/// connection's instance is closed.
#[derive(Debug)]
struct Slots {
    free: Mutex<u32>,
    freed: Condvar,
}

impl Slots {
    /// Waits for a free slot; `None` once the server is stopped.
    fn acquire(slots: &Arc<Self>, stopper: &PipeStopper) -> Option<Slot> {
        let mut free = lock(&slots.free);
        loop {
            if stopper.is_stopped() {
                return None;
            }
            if *free > 0 {
                *free -= 1;
                return Some(Slot(Arc::clone(slots)));
            }
            free = slots
                .freed
                .wait_timeout(free, SLOT_POLL)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
        }
    }
}

struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        *lock(&self.0.free) += 1;
        self.0.freed.notify_one();
    }
}

/// What every connection thread shares.
struct Connection<H> {
    access: Arc<AccessList>,
    limits: ServerLimits,
    counters: Arc<Counters>,
    handler: H,
}

impl<H> Connection<H>
where
    H: Fn(&PeerIdentity, RequestEnvelope) -> ResponseEnvelope,
{
    fn serve(&self, mut pipe: PipeInstance) {
        let peer = match pipe.client_identity() {
            Ok(token) => PeerIdentity::new(&token, &self.access),
            Err(_) => {
                bump(&self.counters.refused);
                return;
            }
        };
        if !peer.admitted() {
            bump(&self.counters.refused);
            return;
        }
        bump(&self.counters.admitted);
        loop {
            let Some(deadline) = Instant::now().checked_add(self.limits.idle_timeout) else {
                return;
            };
            let frame = match codec::read_frame(&mut Timed {
                pipe: &mut pipe,
                deadline,
            }) {
                Ok(frame) => frame,
                Err(FrameError::Closed) => return,
                Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::ConnectionAborted => return,
                Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
                    bump(&self.counters.idle_timeouts);
                    return;
                }
                Err(_) => {
                    bump(&self.counters.dropped);
                    return;
                }
            };
            let response = match codec::decode_request(&frame) {
                Ok(request) => (self.handler)(&peer, request),
                // The decode error names the problem and where, never the
                // bytes. Its code says it all.
                Err(error) => refusal(
                    error.request_id().cloned(),
                    error.code(),
                    None,
                    &error.to_string(),
                ),
            };
            bump(&self.counters.requests);
            let Some(bytes) = encode(&response) else {
                bump(&self.counters.dropped);
                return;
            };
            let Some(deadline) = Instant::now().checked_add(self.limits.write_timeout) else {
                return;
            };
            if let Err(e) = codec::write_frame(
                &mut Timed {
                    pipe: &mut pipe,
                    deadline,
                },
                &bytes,
            ) {
                if e.kind() != io::ErrorKind::ConnectionAborted {
                    bump(&self.counters.dropped);
                }
                return;
            }
        }
    }
}

fn refusal(
    request_id: Option<RequestId>,
    code: ErrorCode,
    reason: Option<ErrorReason>,
    message: &str,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        outcome: Outcome::Err(ProtocolError::new(code, reason, message)),
    }
}

/// The answer's bytes. One that cannot be encoded is replaced by a refusal
/// with the same request ID. The contract keeps every valid answer within
/// the limit (`wgal-protocol`'s `tests/limits.rs`), so this only guards
/// against a future field breaking that.
fn encode(response: &ResponseEnvelope) -> Option<Vec<u8>> {
    match codec::encode_response(response) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            // `message_too_large` alone would read as the request's fault.
            let (code, reason, message) = match error {
                EncodeError::TooLarge => (
                    ErrorCode::MessageTooLarge,
                    Some(ErrorReason::AnswerTooLarge),
                    "the answer is too large",
                ),
                EncodeError::Serialize(_) => {
                    (ErrorCode::Internal, None, "the answer could not be encoded")
                }
            };
            let refused = refusal(response.request_id.clone(), code, reason, message);
            codec::encode_response(&refused).ok()
        }
    }
}

/// The pipe as a stream for the codec, every read and write ending at
/// `deadline`. Flushing does nothing: a write is complete once the pipe has
/// taken the bytes, and `FlushFileBuffers` would wait, without a deadline,
/// until the client had read them.
struct Timed<'a> {
    pipe: &'a mut PipeInstance,
    deadline: Instant,
}

impl io::Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pipe.read(buf, self.deadline)
    }
}

impl io::Write for Timed<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pipe.write(buf, self.deadline)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgal_platform::windows::PIPE_CLIENT_QOS;

    fn token(elevated: bool, administrator: bool) -> TokenIdentity {
        TokenIdentity {
            sid: "S-1-5-21-1000000000-2000000000-3000000000-1001".to_owned(),
            local_system: false,
            elevated,
            administrator,
        }
    }

    #[test]
    fn only_system_elevated_administrators_and_listed_users_are_admitted() {
        let nobody = AccessList::default();
        let listed =
            AccessList::parse("user S-1-5-21-1000000000-2000000000-3000000000-1001").unwrap();
        assert!(!PeerIdentity::new(&token(false, false), &nobody).admitted());
        // A UAC-filtered administrator, or an elevated token without the
        // Administrators group, is an ordinary user.
        assert!(!PeerIdentity::new(&token(false, true), &nobody).admitted());
        assert!(!PeerIdentity::new(&token(true, false), &nobody).admitted());
        assert!(PeerIdentity::new(&token(true, true), &nobody).admitted());
        assert!(PeerIdentity::new(&token(false, false), &listed).admitted());
        let system = TokenIdentity {
            sid: "S-1-5-18".to_owned(),
            local_system: true,
            elevated: true,
            administrator: true,
        };
        let peer = PeerIdentity::new(&system, &nobody);
        assert!(peer.admitted() && peer.local_system && !peer.listed);
    }

    #[test]
    fn limits_leave_one_instance_for_the_next_client() {
        assert!(ServerLimits::DEFAULT.valid());
        for max_connections in [0, MAX_PIPE_INSTANCES] {
            let limits = ServerLimits {
                max_connections,
                ..ServerLimits::DEFAULT
            };
            assert!(!limits.valid());
        }
        assert!(
            !ServerLimits {
                idle_timeout: Duration::ZERO,
                ..ServerLimits::DEFAULT
            }
            .valid()
        );
    }

    #[test]
    fn clients_are_identified_never_impersonated() {
        // SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION.
        assert_eq!(PIPE_CLIENT_QOS, 0x0010_0000 | 0x0001_0000);
    }
}
