//! The watchdog's local socket server on Linux and macOS (plan §3.2, §9.2):
//! the counterpart of the Windows pipe server, written over two small
//! traits ([`Listen`], [`Conn`]) so that everything but binding the real
//! Unix socket is tested on every OS (the tests use loopback TCP).
//!
//! # Who may connect
//!
//! The platform decides who is on the other end of each connection from
//! what the kernel recorded (`SO_PEERCRED` on Linux, `getpeereid` on
//! macOS), never from anything the client sends, and maps it to a
//! [`Caller`] (see `crate::linux` and `crate::macos`). An identity that
//! cannot be read ends the connection before anything is read.
//!
//! On Windows the pipe's DACL keeps out every account that is not listed,
//! so the pipe server simply drops anyone else. A Unix socket's folder
//! cannot list single users, so here a caller that is not `allowed` is
//! answered instead: its one request gets the loop's own refusal
//! (`caller_not_allowed`, decided before the loop, see
//! `crate::driver::Client::call`) and the connection ends, so the window can
//! say why. Such callers have their own few slots
//! ([`ServerLimits::max_refusals`]) and a short deadline
//! ([`ServerLimits::refusal_timeout`]), so they can never take the slots of
//! the accounts that may use the watchdog.
//!
//! # Connections
//!
//! At most [`ServerLimits::max_connections`] allowed connections are served
//! at once, each on its own thread; one more is closed at once, unanswered
//! (the window counts that as no answer and asks again later). A connection
//! reads one request at a time: a frame (`codec::read_frame`) that must
//! arrive completely within [`ServerLimits::idle_timeout`] of the previous
//! answer, or the idle client is dropped; then `codec::decode_request`, the
//! handler, `codec::encode_response`, and the answer written as one frame
//! within [`ServerLimits::write_timeout`]. A request that does not decode is
//! answered with a refusal carrying its request ID when the ID can be read,
//! and the connection goes on. A frame that is empty, oversized, cut short
//! or late ends the connection without an answer. Nothing a client sends is
//! logged.
//!
//! # Stopping
//!
//! [`ServerHandle::stop`] ends the accept loop within [`ACCEPT_POLL`] and
//! shuts every open connection down; a pending read or write also looks for
//! the stop every [`IO_SLICE`]. [`Server::run`] returns once every
//! connection thread has ended.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_protocol::{
    EncodeError, ErrorCode, ErrorReason, FrameError, Outcome, PROTOCOL_VERSION, ProtocolError,
    RequestEnvelope, RequestId, ResponseEnvelope, codec,
};

use crate::ipc::Caller;

/// How often the accept loop looks for a new connection and for a stop.
pub const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// The longest a connection's read or write waits before it looks for a
/// stop again. A stop also shuts every connection down, which wakes a
/// pending read on Unix; this does not rely on it.
pub const IO_SLICE: Duration = Duration::from_millis(250);

/// A connected stream the server can serve.
pub trait Conn: Read + Write + Send + Sized + 'static {
    fn set_blocking(&self) -> io::Result<()>;
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Another handle to the same connection, to shut it down from the
    /// server's thread.
    fn duplicate(&self) -> io::Result<Self>;
    /// Ends both directions: a pending read or write on any handle returns.
    fn shut_down(&self);
}

/// A bound, listening socket.
pub trait Listen: Send + 'static {
    type Conn: Conn;
    /// Makes [`Self::accept_conn`] return `WouldBlock` when nobody waits.
    fn set_nonblocking(&self) -> io::Result<()>;
    fn accept_conn(&self) -> io::Result<Self::Conn>;
}

#[cfg(unix)]
impl Conn for std::os::unix::net::UnixStream {
    fn set_blocking(&self) -> io::Result<()> {
        self.set_nonblocking(false)
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        std::os::unix::net::UnixStream::set_read_timeout(self, timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        std::os::unix::net::UnixStream::set_write_timeout(self, timeout)
    }
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn shut_down(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(unix)]
impl Listen for std::os::unix::net::UnixListener {
    type Conn = std::os::unix::net::UnixStream;
    fn set_nonblocking(&self) -> io::Result<()> {
        std::os::unix::net::UnixListener::set_nonblocking(self, true)
    }
    fn accept_conn(&self) -> io::Result<Self::Conn> {
        self.accept().map(|(stream, _)| stream)
    }
}

/// Bounds for one server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLimits {
    /// Allowed connections served at once (1 to 64).
    pub max_connections: u32,
    /// Connections of callers that are not allowed, answered at once (1 to
    /// 8).
    pub max_refusals: u32,
    /// Longest wait for the next whole request on an allowed connection.
    pub idle_timeout: Duration,
    /// Longest wait for the one request of a caller that is not allowed.
    pub refusal_timeout: Duration,
    /// Longest wait for an answer to be taken by the socket.
    pub write_timeout: Duration,
}

impl ServerLimits {
    pub const DEFAULT: Self = Self {
        // A window per signed-in user and an agent or two, each a few
        // requests at a time.
        max_connections: 16,
        max_refusals: 2,
        // The window asks for a snapshot every 5 s (plan §6.2); a connection
        // quiet for six of those is gone or stuck.
        idle_timeout: Duration::from_secs(30),
        refusal_timeout: Duration::from_secs(2),
        write_timeout: Duration::from_secs(5),
    };

    fn valid(&self) -> bool {
        (1..=64).contains(&self.max_connections)
            && (1..=8).contains(&self.max_refusals)
            && !self.idle_timeout.is_zero()
            && !self.refusal_timeout.is_zero()
            && !self.write_timeout.is_zero()
    }
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug)]
pub enum ServerError {
    /// [`ServerLimits`] out of range.
    InvalidLimits,
    /// Accepting connections failed; the socket can no longer be served.
    Accept(io::ErrorKind),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => write!(f, "socket server limits out of range"),
            Self::Accept(kind) => write!(f, "accepting connections failed: {kind}"),
        }
    }
}

impl std::error::Error for ServerError {}

/// What one server has done so far, for diagnostics. Counts only: nothing a
/// client sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerStats {
    /// Connections of allowed callers that were served.
    pub admitted: u64,
    /// Connections of callers that are not allowed, answered with a
    /// refusal or closed.
    pub refused: u64,
    /// Connections closed at once: the peer's identity could not be read,
    /// or every slot of its kind was busy.
    pub turned_away: u64,
    /// Requests answered, refusals included.
    pub requests: u64,
    /// Connections dropped because no whole request came in time.
    pub idle_timeouts: u64,
    /// Connections dropped for a frame that was empty, oversized or cut
    /// short, an answer the socket did not take in time, an I/O error, a
    /// thread that could not be started, or one that panicked.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    admitted: AtomicU64,
    refused: AtomicU64,
    turned_away: AtomicU64,
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
            turned_away: self.turned_away.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            idle_timeouts: self.idle_timeouts.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The open connections, by number, to shut down on a stop.
type Open<C> = Mutex<BTreeMap<u64, C>>;

/// The state a running server shares with its handles.
struct Shared<C> {
    stopped: AtomicBool,
    open: Open<C>,
    counters: Counters,
}

/// Stops a running [`Server`] and reads its counts, from any thread.
pub struct ServerHandle<C: Conn> {
    shared: Arc<Shared<C>>,
}

impl<C: Conn> Clone for ServerHandle<C> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<C: Conn> fmt::Debug for ServerHandle<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerHandle")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl<C: Conn> ServerHandle<C> {
    /// Stops accepting and shuts every open connection down.
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        for conn in lock(&self.shared.open).values() {
            conn.shut_down();
        }
    }

    pub fn stats(&self) -> ServerStats {
        self.shared.counters.snapshot()
    }

    /// Connections being served now.
    pub fn open_connections(&self) -> usize {
        lock(&self.shared.open).len()
    }
}

/// A bound socket, ready to [`run`](Self::run).
pub struct Server<L: Listen> {
    listener: L,
    limits: ServerLimits,
    shared: Arc<Shared<L::Conn>>,
}

impl<L: Listen> fmt::Debug for Server<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Free slots of one kind. A slot is held while its connection is served.
#[derive(Debug)]
struct Slots(Mutex<u32>);

impl Slots {
    fn try_take(slots: &Arc<Self>) -> Option<Slot> {
        let mut free = lock(&slots.0);
        if *free == 0 {
            return None;
        }
        *free -= 1;
        Some(Slot(Arc::clone(slots)))
    }
}

struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        *lock(&(self.0).0) += 1;
    }
}

/// A connection being served: registered, so a stop can shut it down, and
/// holding its slot. Dropped when its thread ends, a panic included, or
/// when the thread cannot start: the connection is shut down (the client
/// sees the end of the stream), its registration removed, and then its
/// slot freed.
struct Served<C: Conn> {
    shared: Arc<Shared<C>>,
    id: u64,
    _slot: Slot,
}

impl<C: Conn> Drop for Served<C> {
    fn drop(&mut self) {
        if let Some(conn) = lock(&self.shared.open).remove(&self.id) {
            conn.shut_down();
        }
    }
}

impl<L: Listen> Server<L> {
    /// Takes a bound listener. It is made non-blocking here, so a stop is
    /// seen within [`ACCEPT_POLL`].
    pub fn new(listener: L, limits: ServerLimits) -> Result<Self, ServerError> {
        if !limits.valid() {
            return Err(ServerError::InvalidLimits);
        }
        listener
            .set_nonblocking()
            .map_err(|e| ServerError::Accept(e.kind()))?;
        Ok(Self {
            listener,
            limits,
            shared: Arc::new(Shared {
                stopped: AtomicBool::new(false),
                open: Mutex::new(BTreeMap::new()),
                counters: Counters::default(),
            }),
        })
    }

    pub fn handle(&self) -> ServerHandle<L::Conn> {
        ServerHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Serves connections until stopped. `identify` reads each peer's
    /// identity from its connection (`None`: it cannot be read), and
    /// `handler` answers each request of it. Returns once every connection
    /// thread has ended. An error accepting connections stops the open
    /// connections too and is returned.
    pub fn run<I, H>(self, identify: I, handler: H) -> Result<(), ServerError>
    where
        I: Fn(&L::Conn) -> Option<Caller>,
        H: Fn(&Caller, RequestEnvelope) -> ResponseEnvelope + Send + Sync + 'static,
    {
        let allowed = Arc::new(Slots(Mutex::new(self.limits.max_connections)));
        let refusals = Arc::new(Slots(Mutex::new(self.limits.max_refusals)));
        let handler = Arc::new(handler);
        let mut threads: Vec<JoinHandle<()>> = Vec::new();
        let mut next_id: u64 = 0;
        let counters = &self.shared.counters;
        let result = loop {
            threads.retain(|thread| !thread.is_finished());
            if self.shared.stopped.load(Ordering::Acquire) {
                break Ok(());
            }
            let conn = match self.listener.accept_conn() {
                Ok(conn) => conn,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // The peer gave up before it was accepted.
                Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => continue,
                Err(e) => {
                    self.handle().stop();
                    break Err(ServerError::Accept(e.kind()));
                }
            };
            // Dropping `conn` closes it: the client sees the end of the
            // stream without an answer.
            if conn.set_blocking().is_err() {
                bump(&counters.turned_away);
                continue;
            }
            let Some(caller) = identify(&conn) else {
                bump(&counters.turned_away);
                continue;
            };
            let (slots, idle) = if caller.allowed {
                (&allowed, self.limits.idle_timeout)
            } else {
                (&refusals, self.limits.refusal_timeout)
            };
            let Some(slot) = Slots::try_take(slots) else {
                bump(&counters.turned_away);
                continue;
            };
            let Ok(registered) = conn.duplicate() else {
                bump(&counters.dropped);
                continue;
            };
            let id = next_id;
            next_id += 1;
            lock(&self.shared.open).insert(id, registered);
            // From here the guard ends the connection, however its thread
            // ends, or if it cannot start.
            let guard = Served {
                shared: Arc::clone(&self.shared),
                id,
                _slot: slot,
            };
            let handler = Arc::clone(&handler);
            let limits = self.limits;
            let spawned = thread::Builder::new()
                .name("wgal-socket-connection".to_owned())
                .spawn(move || {
                    let shared = &guard.shared;
                    if caller.allowed {
                        bump(&shared.counters.admitted);
                    } else {
                        bump(&shared.counters.refused);
                    }
                    let served = panic::catch_unwind(AssertUnwindSafe(|| {
                        serve(conn, &caller, idle, &limits, shared, &*handler);
                    }));
                    if served.is_err() {
                        bump(&shared.counters.dropped);
                    }
                    drop(guard);
                });
            match spawned {
                Ok(thread) => threads.push(thread),
                // The closure, with the guard, is dropped: the connection is
                // shut down and its slot freed.
                Err(_) => bump(&counters.dropped),
            }
            // A stop may have come while this connection was being set up:
            // shut it down too, since it was not open when the stop looked.
            if self.shared.stopped.load(Ordering::Acquire) {
                self.handle().stop();
            }
        };
        for thread in threads {
            // A connection that panicked has already been shut down by its
            // handle being dropped with the thread.
            let _ = thread.join();
        }
        result
    }
}

/// Serves one connection: requests in turn while the caller is allowed,
/// one request otherwise.
fn serve<C, H>(
    mut conn: C,
    caller: &Caller,
    idle: Duration,
    limits: &ServerLimits,
    shared: &Shared<C>,
    handler: &H,
) where
    C: Conn,
    H: Fn(&Caller, RequestEnvelope) -> ResponseEnvelope,
{
    let counters = &shared.counters;
    let stopped = &shared.stopped;
    loop {
        let Some(deadline) = Instant::now().checked_add(idle) else {
            return;
        };
        let frame = match codec::read_frame(&mut Timed {
            conn: &mut conn,
            deadline,
            stopped,
        }) {
            Ok(frame) => frame,
            Err(FrameError::Closed) => return,
            Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::ConnectionAborted => return,
            Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
                bump(&counters.idle_timeouts);
                return;
            }
            Err(_) => {
                bump(&counters.dropped);
                return;
            }
        };
        let response = match codec::decode_request(&frame) {
            Ok(request) => handler(caller, request),
            // The decode error names the problem and where, never the bytes.
            Err(error) => refusal(
                error.request_id().cloned(),
                error.code(),
                None,
                &error.to_string(),
            ),
        };
        bump(&counters.requests);
        let Some(bytes) = encode(&response) else {
            bump(&counters.dropped);
            return;
        };
        let Some(deadline) = Instant::now().checked_add(limits.write_timeout) else {
            return;
        };
        if let Err(e) = codec::write_frame(
            &mut Timed {
                conn: &mut conn,
                deadline,
                stopped,
            },
            &bytes,
        ) {
            if e.kind() != io::ErrorKind::ConnectionAborted {
                bump(&counters.dropped);
            }
            return;
        }
        if !caller.allowed {
            return;
        }
    }
}

/// A connection read and written under one deadline for a whole frame: each
/// read or write waits at most for what is left of it, in slices of
/// [`IO_SLICE`] between which it ends with `ConnectionAborted` once the
/// server is stopped.
struct Timed<'a, C: Conn> {
    conn: &'a mut C,
    deadline: Instant,
    stopped: &'a AtomicBool,
}

impl<C: Conn> Timed<'_, C> {
    /// How long the next wait may be: the rest of the deadline, at most a
    /// slice. Fails once the deadline has passed or the server is stopped.
    fn next_wait(&self) -> io::Result<Duration> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(io::ErrorKind::TimedOut.into())
        } else {
            Ok(left.min(IO_SLICE))
        }
    }
}

/// A socket's own timeout reads as `WouldBlock` on Unix and `TimedOut` on
/// Windows: the slice passed with nothing done.
fn slice_passed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

impl<C: Conn> Read for Timed<'_, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let wait = self.next_wait()?;
            self.conn.set_read_timeout(Some(wait))?;
            match self.conn.read(buf) {
                Err(e) if slice_passed(&e) => {}
                done => return done,
            }
        }
    }
}

impl<C: Conn> Write for Timed<'_, C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let wait = self.next_wait()?;
            self.conn.set_write_timeout(Some(wait))?;
            match self.conn.write(buf) {
                Err(e) if slice_passed(&e) => {}
                done => return done,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush()
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
/// with the same request ID (as on Windows; the contract keeps every valid
/// answer within the limit).
fn encode(response: &ResponseEnvelope) -> Option<Vec<u8>> {
    match codec::encode_response(response) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            let (code, reason, message) = match error {
                // `message_too_large` alone would read as the request's
                // fault.
                EncodeError::TooLarge => (
                    ErrorCode::MessageTooLarge,
                    Some(ErrorReason::AnswerTooLarge),
                    "the answer is too large",
                ),
                EncodeError::Serialize(_) => {
                    (ErrorCode::Internal, None, "the answer could not be encoded")
                }
            };
            codec::encode_response(&refusal(response.request_id.clone(), code, reason, message))
                .ok()
        }
    }
}

#[cfg(test)]
mod tests;
