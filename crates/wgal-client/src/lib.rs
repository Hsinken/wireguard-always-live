//! The way to the WGAL watchdog that `wgal-mcp` and the `wgal` command
//! line share (owner decision of 2026-11-18): one connection per request,
//! as the window's (`apps/desktop/src-tauri/src/ipc.rs`). On Windows
//! through the watchdog's pipe, on Linux and macOS through its socket, each
//! opened only if the watchdog serves it. A request from `wgal-mcp` is
//! marked as coming from MCP; one from `wgal` carries no origin, so the
//! watchdog treats it as the window's. Either way the watchdog checks the
//! caller's OS account and the access list, as for the window.
//!
//! Every exchange has a deadline, as the window's (AR-05): it runs on a
//! thread of its own, and one the watchdog does not answer in time is left
//! to end by itself while the caller goes on. Those left behind are
//! counted, so a watchdog that stops answering cannot pile up threads and
//! handles. Only opening a connection that is busy is tried again, since
//! nothing was sent then; a request is never sent twice.

pub mod token;

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_protocol::{
    MAX_MESSAGE_BYTES, Origin, PROTOCOL_VERSION, Request, RequestEnvelope, RequestId,
    ResponseEnvelope, codec,
};

/// Which WGAL program talks to the watchdog: what its requests carry and
/// how its messages tell the reader to look again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    /// `wgal-mcp`, for the user's agents: every request marked `origin:
    /// mcp`, so the watchdog answers it only while MCP is on.
    Mcp,
    /// `wgal`, the command line, for the user: no origin, as the window.
    Cli,
}

impl Client {
    /// The origin its requests carry.
    pub fn origin(self) -> Option<Origin> {
        match self {
            Self::Mcp => Some(Origin::Mcp),
            Self::Cli => None,
        }
    }

    /// The program's name, as messages give it.
    pub fn program(self) -> &'static str {
        match self {
            Self::Mcp => "wgal-mcp",
            Self::Cli => "wgal",
        }
    }

    /// What starts its request IDs.
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Cli => "cli",
        }
    }

    /// How a message that a request may have been carried out ends: where
    /// to look.
    pub fn check_hint(self) -> &'static str {
        match self {
            Self::Mcp => "call health to see",
            Self::Cli => "run \"wgal status\" to see",
        }
    }

    /// What to do while earlier requests are still unanswered.
    fn wait_hint(self) -> &'static str {
        match self {
            Self::Mcp => "try again in a little while, or ask the user to check the WGAL window",
            Self::Cli => "try again in a little while",
        }
    }
}

/// Whether a request that got no answer reached the watchdog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Nothing was sent, so the watchdog cannot have acted on it.
    NotSent,
    /// It may have reached the watchdog, which may carry it out.
    Unknown,
}

/// Why a request got no answer from the watchdog, in words the reader can
/// act on, and whether it may have reached it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreachable {
    pub message: String,
    pub delivery: Delivery,
}

impl Unreachable {
    /// Nothing was sent.
    pub fn not_sent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            delivery: Delivery::NotSent,
        }
    }

    /// The request may have reached the watchdog.
    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            delivery: Delivery::Unknown,
        }
    }
}

/// Sends one request and returns the watchdog's answer.
pub trait Watchdog {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable>;

    /// [`Self::send`], given up on after at most `most` (never more than
    /// the usual deadline): for a look that must end within what a wait
    /// still has. An answer that comes later is left, as any late one.
    fn send_within(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        let _ = most;
        self.send(request)
    }

    /// Where the watchdog writes its event log and its own log.
    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable>;
}

/// The longest one exchange takes, busy retries included, as the window's:
/// the watchdog answers every request once it has recorded it (`Connect`,
/// `Disconnect` and `Reconnect` before the tunnel has changed), well within
/// it.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long to keep opening a connection while every one is in use, and the
/// pause between tries, as the window's.
const BUSY_FOR: Duration = Duration::from_secs(2);
const BUSY_PAUSE: Duration = Duration::from_millis(50);

/// Exchanges still running, late ones included. Each client sends one
/// request at a time, so more than one runs only while earlier ones the
/// watchdog did not answer in time are still waiting for it.
const MAX_IN_FLIGHT: usize = 4;

/// The times one exchange keeps to, and the client's words.
#[derive(Clone, Copy, Debug)]
struct Limits {
    deadline: Duration,
    busy_for: Duration,
    busy_pause: Duration,
    client: Client,
}

impl Limits {
    fn of(client: Client) -> Self {
        Self {
            deadline: DEADLINE,
            busy_for: BUSY_FOR,
            busy_pause: BUSY_PAUSE,
            client,
        }
    }

    /// These limits with a deadline of at most `most` (at least a second,
    /// at most the usual one); the busy window never outlasts it.
    fn within(client: Client, most: Duration) -> Self {
        let deadline = most.clamp(Duration::from_secs(1), DEADLINE);
        Self {
            deadline,
            busy_for: BUSY_FOR.min(deadline),
            ..Self::of(client)
        }
    }
}

/// Counts the exchanges still running, late ones included: past the limit a
/// request is not sent at all.
#[derive(Debug)]
struct InFlight {
    running: AtomicUsize,
    limit: usize,
}

impl InFlight {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            running: AtomicUsize::new(0),
            limit,
        })
    }

    fn claim(self: &Arc<Self>) -> Option<Claim> {
        self.running
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Claim(Arc::clone(self)))
    }

    #[cfg(test)]
    fn running(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }
}

/// One running exchange, released when it ends.
struct Claim(Arc<InFlight>);

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The real watchdog, through its pipe (Windows) or socket (Linux, macOS).
#[derive(Debug)]
pub struct PipeWatchdog {
    client: Client,
    next: u64,
    in_flight: Arc<InFlight>,
}

impl PipeWatchdog {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            next: 0,
            in_flight: InFlight::new(MAX_IN_FLIGHT),
        }
    }

    fn envelope(&mut self, request: &Request) -> Result<RequestEnvelope, Unreachable> {
        self.next = self.next.wrapping_add(1);
        let request_id = RequestId::new(format!(
            "{}-{}-{}",
            self.client.id_prefix(),
            std::process::id(),
            self.next
        ))
        .map_err(|_| Unreachable::not_sent("could not name the request; nothing was sent"))?;
        Ok(RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            request: request.clone(),
            origin: self.client.origin(),
        })
    }
}

/// What a failed open means.
enum OpenFailure {
    /// Every connection is in use: nothing was sent, so it may be tried
    /// again.
    Busy,
    Fail(Unreachable),
}

/// Opens a connection with `open`, writes `envelope`, reads one answer back
/// and checks it answers this request, all on a thread of its own and
/// within `limits.deadline`. A late exchange is left to end by itself, and
/// its connection is closed when it does; until then it holds its place in
/// `in_flight`.
fn request_within<S, F, C>(
    mut open: F,
    classify: C,
    envelope: &RequestEnvelope,
    limits: Limits,
    in_flight: &Arc<InFlight>,
) -> Result<ResponseEnvelope, Unreachable>
where
    S: Read + Write,
    F: FnMut() -> io::Result<S> + Send + 'static,
    C: Fn(&io::Error) -> OpenFailure + Send + 'static,
{
    let frame = codec::encode_request(envelope).map_err(|_| {
        Unreachable::not_sent(
            "the request is larger than the 64 KiB a message may be; nothing was sent",
        )
    })?;
    let claim = in_flight.claim().ok_or_else(|| {
        Unreachable::not_sent(format!(
            "earlier requests to the WGAL watchdog are still unanswered, so this one was not sent; {}",
            limits.client.wait_hint()
        ))
    })?;
    let request_id = envelope.request_id.clone();
    let hint = limits.client.check_hint();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("wgal-ipc".to_owned())
        .spawn(move || {
            // Held until the exchange ends, however late.
            let _claim = claim;
            let result = connect(&mut open, &classify, limits)
                .and_then(|mut stream| exchange(&mut stream, &frame, &request_id, hint));
            // After the deadline nobody listens any more.
            let _ = tx.send(result);
        })
        .map_err(|_| {
            Unreachable::not_sent(
                "could not start the request to the WGAL watchdog; nothing was sent",
            )
        })?;
    match rx.recv_timeout(limits.deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Unreachable::unknown(format!(
            "the WGAL watchdog did not answer within {} seconds; the request may still be carried out, {hint}",
            limits.deadline.as_secs()
        ))),
        // The exchange ended without saying how (a panic).
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Unreachable::unknown(format!(
            "the request to the WGAL watchdog ended without an answer; it may have been carried out, {hint}"
        ))),
    }
}

/// Opens a connection, trying again for `limits.busy_for` while every one
/// is in use. Any other failure ends it at once.
fn connect<S>(
    open: &mut impl FnMut() -> io::Result<S>,
    classify: &impl Fn(&io::Error) -> OpenFailure,
    limits: Limits,
) -> Result<S, Unreachable> {
    let started = Instant::now();
    loop {
        let error = match open() {
            Ok(stream) => return Ok(stream),
            Err(error) => error,
        };
        match classify(&error) {
            OpenFailure::Busy if started.elapsed() < limits.busy_for => {
                thread::sleep(limits.busy_pause);
            }
            OpenFailure::Busy => {
                return Err(Unreachable::not_sent(
                    "every connection to the WGAL watchdog stayed busy, so nothing was sent; try again in a few seconds",
                ));
            }
            OpenFailure::Fail(unreachable) => return Err(unreachable),
        }
    }
}

/// One request frame out, one answer back, on a connection that is only
/// this request's; only an answer to this request is returned.
fn exchange<S: Read + Write>(
    stream: &mut S,
    frame: &[u8],
    request_id: &RequestId,
    hint: &str,
) -> Result<ResponseEnvelope, Unreachable> {
    // The watchdog acts only on a whole frame. A frame that is not one is
    // refused here, before anything is written: surely not sent. Once
    // `write_frame` touches the stream, any error it returns may come after
    // the whole frame went out (it flushes last), so the request may have
    // been carried out (Codex review of FIX-03, P2-03).
    if frame.is_empty() || frame.len() > MAX_MESSAGE_BYTES as usize {
        return Err(Unreachable::not_sent(
            "the request could not be sent to the watchdog; nothing was sent",
        ));
    }
    codec::write_frame(stream, frame).map_err(|_| {
        Unreachable::unknown(format!(
            "the request could not be sent to the watchdog in full; it may have been carried out, {hint}"
        ))
    })?;
    let answer = codec::read_frame(stream).map_err(|_| {
        Unreachable::unknown(format!(
            "the watchdog closed the connection without an answer; the request may have been carried out, {hint}"
        ))
    })?;
    let response = codec::decode_response(&answer).map_err(|_| {
        Unreachable::unknown(format!(
            "the watchdog's answer could not be read; the request may have been carried out, {hint}"
        ))
    })?;
    if response.request_id.as_ref() != Some(request_id) {
        return Err(Unreachable::unknown(format!(
            "the watchdog answered another request; this one may have been carried out, {hint}"
        )));
    }
    Ok(response)
}

/// Why connecting to the watchdog's socket failed, in words the reader can
/// act on.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn socket_unreachable(error: &io::Error, service: &str) -> Unreachable {
    use std::io::ErrorKind;
    if wgal_platform::socket::is_not_the_watchdog(error) {
        Unreachable::not_sent(
            "something other than the WGAL watchdog serves its socket; nothing was sent",
        )
    } else if matches!(
        error.kind(),
        ErrorKind::NotFound | ErrorKind::ConnectionRefused
    ) {
        Unreachable::not_sent(format!("the WGAL watchdog ({service}) is not running"))
    } else if error.kind() == ErrorKind::PermissionDenied {
        Unreachable::not_sent("this account may not use the WGAL watchdog on this computer")
    } else {
        Unreachable::not_sent(format!(
            "could not reach the WGAL watchdog: {}",
            error.kind()
        ))
    }
}

/// A failed socket connect: a full queue is busy, anything else as
/// [`socket_unreachable`] says.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn classify_socket(error: &io::Error, service: &str) -> OpenFailure {
    if error.kind() == io::ErrorKind::WouldBlock
        && !wgal_platform::socket::is_not_the_watchdog(error)
    {
        OpenFailure::Busy
    } else {
        OpenFailure::Fail(socket_unreachable(error, service))
    }
}

/// `winerror.h`: `ERROR_PIPE_BUSY`, every instance of the pipe in use.
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

/// A failed pipe open, by its Windows error.
#[cfg(windows)]
fn classify_pipe(error: &io::Error) -> OpenFailure {
    use std::io::ErrorKind;
    if wgal_platform::windows::is_not_the_watchdog(error) {
        OpenFailure::Fail(Unreachable::not_sent(
            "something other than the WGAL service serves its pipe; nothing was sent",
        ))
    } else if error.raw_os_error() == Some(ERROR_PIPE_BUSY) {
        OpenFailure::Busy
    } else if error.kind() == ErrorKind::NotFound {
        OpenFailure::Fail(Unreachable::not_sent(
            "the WGAL background service (WGALWatchdog) is not running",
        ))
    } else if error.kind() == ErrorKind::PermissionDenied {
        OpenFailure::Fail(Unreachable::not_sent(
            "this account may not use the WGAL watchdog on this computer",
        ))
    } else {
        OpenFailure::Fail(Unreachable::not_sent(format!(
            "could not reach the WGAL watchdog: {}",
            error.kind()
        )))
    }
}

#[cfg(windows)]
impl Watchdog for PipeWatchdog {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.send_within(request, DEADLINE)
    }

    fn send_within(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        let envelope = self.envelope(request)?;
        request_within(
            wgal_platform::windows::connect_watchdog_pipe,
            classify_pipe,
            &envelope,
            Limits::within(self.client, most),
            &self.in_flight,
        )
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        let data = wgal_platform::windows::program_data_dir()
            .map_err(|_| Unreachable::not_sent("the ProgramData folder was not found"))?
            .join("WGAL");
        Ok(vec![
            (
                "event_log".to_owned(),
                data.join("events").join("events.jsonl"),
            ),
            ("event_log_folder".to_owned(), data.join("events")),
            ("service_log".to_owned(), data.join("service.log")),
        ])
    }
}

/// On Linux the watchdog is root's systemd service; its own log is in the
/// journal (`journalctl -u wgal-watchdog`), not a file.
#[cfg(target_os = "linux")]
impl Watchdog for PipeWatchdog {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.send_within(request, DEADLINE)
    }

    fn send_within(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        let envelope = self.envelope(request)?;
        request_within(
            wgal_platform::linux::socket::connect_watchdog_socket,
            |error: &io::Error| classify_socket(error, "wgal-watchdog.service"),
            &envelope,
            Limits::within(self.client, most),
            &self.in_flight,
        )
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        let events = PathBuf::from("/var/lib/wgal/events");
        Ok(vec![
            ("event_log".to_owned(), events.join("events.jsonl")),
            ("event_log_folder".to_owned(), events),
            (
                "service_log_journal_unit".to_owned(),
                PathBuf::from("wgal-watchdog.service"),
            ),
        ])
    }
}

/// On macOS the watchdog is the user's own LaunchAgent; its folder and log
/// are the user's.
#[cfg(target_os = "macos")]
impl Watchdog for PipeWatchdog {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.send_within(request, DEADLINE)
    }

    fn send_within(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        let envelope = self.envelope(request)?;
        request_within(
            wgal_platform::macos::socket::connect_watchdog_socket,
            |error: &io::Error| classify_socket(error, "its LaunchAgent"),
            &envelope,
            Limits::within(self.client, most),
            &self.in_flight,
        )
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        use wgal_platform::macos::socket::{data_dir, home_dir};
        let home =
            home_dir().ok_or_else(|| Unreachable::not_sent("this user has no home folder"))?;
        let data =
            data_dir(&home).ok_or_else(|| Unreachable::not_sent("this user has no home folder"))?;
        Ok(vec![
            (
                "event_log".to_owned(),
                data.join("events").join("events.jsonl"),
            ),
            ("event_log_folder".to_owned(), data.join("events")),
            (
                "service_log".to_owned(),
                home.join("Library/Logs/WGAL/watchdog.log"),
            ),
        ])
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl Watchdog for PipeWatchdog {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.envelope(request)?;
        Err(Unreachable::not_sent(format!(
            "{} reaches the watchdog only on Windows, Linux and macOS",
            self.client.program()
        )))
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        Err(Unreachable::not_sent(format!(
            "{} knows the log locations only on Windows, Linux and macOS",
            self.client.program()
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::io::{Error, ErrorKind};
    use std::sync::Mutex;
    use wgal_protocol::{Outcome, Response};

    #[test]
    fn a_socket_failure_is_told_in_words_the_reader_can_act_on() {
        let says = |error: Error| socket_unreachable(&error, "the service").message;
        assert!(says(wgal_platform::socket::not_the_watchdog()).contains("nothing was sent"));
        assert!(says(Error::from(ErrorKind::NotFound)).contains("not running"));
        assert!(says(Error::from(ErrorKind::ConnectionRefused)).contains("not running"));
        assert!(says(Error::from(ErrorKind::PermissionDenied)).contains("may not use"));
        assert!(says(Error::from(ErrorKind::InvalidInput)).contains("could not reach"));
        // A full queue is busy, so it is tried again; nothing else is.
        assert!(matches!(
            classify_socket(&Error::from(ErrorKind::WouldBlock), "the service"),
            OpenFailure::Busy
        ));
        assert!(matches!(
            classify_socket(&wgal_platform::socket::not_the_watchdog(), "the service"),
            OpenFailure::Fail(u) if u.delivery == Delivery::NotSent
        ));
    }

    /// Short times, so the tests are quick.
    const QUICK: Limits = Limits {
        deadline: Duration::from_millis(300),
        busy_for: Duration::from_millis(200),
        busy_pause: Duration::from_millis(10),
        client: Client::Mcp,
    };

    fn envelope(id: &str) -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            request: Request::Connect {
                tunnel_id: wgal_protocol::TunnelId::new("office-1").unwrap(),
            },
            origin: Some(Origin::Mcp),
        }
    }

    /// A connection to a fake watchdog: what is written is kept, and reads
    /// give `answer`; with no answer, a read waits until `hold` is dropped
    /// (a watchdog that does not answer), then the connection ends.
    struct Conn {
        written: Arc<Mutex<Vec<u8>>>,
        answer: io::Cursor<Vec<u8>>,
        hold: Option<mpsc::Receiver<()>>,
    }

    impl Read for Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if let Some(hold) = &self.hold {
                let _ = hold.recv();
                return Err(Error::from(ErrorKind::BrokenPipe));
            }
            self.answer.read(buf)
        }
    }

    impl Write for Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A connection whose writes go through but whose flush fails: the
    /// whole frame may have reached the watchdog.
    struct FlushFails(ErrorKind);

    impl Read for FlushFails {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for FlushFails {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(Error::from(self.0))
        }
    }

    /// Codex review of FIX-03: a write that fails after the whole frame
    /// was written (here in the flush) is not "not sent", and is never sent
    /// again.
    #[test]
    fn a_write_that_fails_after_the_frame_went_out_is_unknown() {
        // Whatever the error says: `InvalidInput` too (P2-03).
        for kind in [ErrorKind::BrokenPipe, ErrorKind::InvalidInput] {
            let in_flight = InFlight::new(MAX_IN_FLIGHT);
            let opens = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&opens);
            let failure = request_within(
                move || {
                    counted.fetch_add(1, Ordering::AcqRel);
                    Ok(FlushFails(kind))
                },
                classify,
                &envelope("w-1"),
                QUICK,
                &in_flight,
            )
            .unwrap_err();
            assert_eq!(failure.delivery, Delivery::Unknown, "{kind:?}: {failure:?}");
            assert!(failure.message.contains("may have been carried out"));
            assert_eq!(opens.load(Ordering::Acquire), 1);
            assert!(all_ended(&in_flight));
        }
    }

    /// The answer frame to request `id`.
    fn answer_to(id: &str) -> Vec<u8> {
        let response = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(RequestId::new(id).unwrap()),
            outcome: Outcome::Ok(Response::Accepted),
        };
        let mut frame = Vec::new();
        codec::write_frame(&mut frame, &codec::encode_response(&response).unwrap()).unwrap();
        frame
    }

    fn busy() -> Error {
        Error::from(ErrorKind::WouldBlock)
    }

    fn classify(error: &Error) -> OpenFailure {
        classify_socket(error, "the service")
    }

    /// Waits up to 2 s for every exchange left behind to end.
    fn all_ended(in_flight: &InFlight) -> bool {
        let start = Instant::now();
        while in_flight.running() > 0 && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        in_flight.running() == 0
    }

    /// AR-05: a watchdog that takes the request and never answers is given
    /// up on at the deadline, and the answer says the request may have been
    /// carried out; the exchange left behind ends by itself later.
    #[test]
    fn a_watchdog_that_does_not_answer_is_given_up_on_in_time() {
        let in_flight = InFlight::new(MAX_IN_FLIGHT);
        let (release, hold) = mpsc::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut hold = Some(hold);
        let kept = Arc::clone(&written);
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let started = Instant::now();
        let result = request_within(
            move || {
                counted.fetch_add(1, Ordering::AcqRel);
                Ok(Conn {
                    written: Arc::clone(&kept),
                    answer: io::Cursor::new(Vec::new()),
                    hold: hold.take(),
                })
            },
            classify,
            &envelope("a-1"),
            QUICK,
            &in_flight,
        );
        let took = started.elapsed();
        let failure = result.unwrap_err();
        assert_eq!(failure.delivery, Delivery::Unknown, "{failure:?}");
        assert!(
            failure.message.contains("did not answer within"),
            "{failure:?}"
        );
        assert!(took < Duration::from_secs(2), "took {took:?}");
        assert!(!written.lock().unwrap().is_empty(), "the request was sent");
        assert_eq!(opens.load(Ordering::Acquire), 1, "and not sent again");
        assert_eq!(in_flight.running(), 1, "left to end by itself");
        drop(release);
        assert!(all_ended(&in_flight));
    }

    /// AR-05: exchanges left behind are capped; past the cap a request is
    /// not sent at all (nothing is opened), and once they end requests go
    /// through again.
    #[test]
    fn exchanges_left_behind_are_capped() {
        let in_flight = InFlight::new(MAX_IN_FLIGHT);
        let opens = Arc::new(AtomicUsize::new(0));
        let mut releases = Vec::new();
        for n in 0..MAX_IN_FLIGHT {
            let (release, hold) = mpsc::channel();
            releases.push(release);
            let mut hold = Some(hold);
            let counted = Arc::clone(&opens);
            let failure = request_within(
                move || {
                    counted.fetch_add(1, Ordering::AcqRel);
                    Ok(Conn {
                        written: Arc::default(),
                        answer: io::Cursor::new(Vec::new()),
                        hold: hold.take(),
                    })
                },
                classify,
                &envelope(&format!("a-{n}")),
                QUICK,
                &in_flight,
            )
            .unwrap_err();
            assert_eq!(failure.delivery, Delivery::Unknown);
        }
        assert_eq!(in_flight.running(), MAX_IN_FLIGHT);
        let counted = Arc::clone(&opens);
        let started = Instant::now();
        let failure = request_within(
            move || -> io::Result<Conn> {
                counted.fetch_add(1, Ordering::AcqRel);
                Err(Error::from(ErrorKind::NotFound))
            },
            classify,
            &envelope("a-over"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert_eq!(failure.delivery, Delivery::NotSent, "{failure:?}");
        assert!(failure.message.contains("still unanswered"), "{failure:?}");
        assert!(started.elapsed() < QUICK.deadline, "answered at once");
        assert_eq!(
            opens.load(Ordering::Acquire),
            MAX_IN_FLIGHT,
            "nothing opened"
        );
        drop(releases);
        assert!(all_ended(&in_flight));
        let answered = request_within(
            || {
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(answer_to("a-next")),
                    hold: None,
                })
            },
            classify,
            &envelope("a-next"),
            QUICK,
            &in_flight,
        );
        assert!(answered.is_ok(), "{answered:?}");
    }

    /// AR-05: opening a busy connection is tried again, since nothing was
    /// sent; a connection that ends after the request was written is not,
    /// and neither is a request whose connection stayed busy too long.
    #[test]
    fn only_a_busy_open_is_tried_again() {
        let in_flight = InFlight::new(MAX_IN_FLIGHT);
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let answer = request_within(
            move || {
                if counted.fetch_add(1, Ordering::AcqRel) < 2 {
                    return Err(busy());
                }
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(answer_to("b-1")),
                    hold: None,
                })
            },
            classify,
            &envelope("b-1"),
            QUICK,
            &in_flight,
        )
        .unwrap();
        assert_eq!(answer.request_id, Some(RequestId::new("b-1").unwrap()));
        assert_eq!(
            opens.load(Ordering::Acquire),
            3,
            "two busy opens, then sent"
        );

        // Sent, then the connection ended without an answer: not again.
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let failure = request_within(
            move || {
                counted.fetch_add(1, Ordering::AcqRel);
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(Vec::new()),
                    hold: None,
                })
            },
            classify,
            &envelope("b-2"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert_eq!(failure.delivery, Delivery::Unknown);
        assert!(failure.message.contains("may have been carried out"));
        assert_eq!(opens.load(Ordering::Acquire), 1);

        // An answer to another request is not this one's.
        let failure = request_within(
            || {
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(answer_to("someone-else")),
                    hold: None,
                })
            },
            classify,
            &envelope("b-3"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert_eq!(failure.delivery, Delivery::Unknown);

        // Busy for longer than the busy window: given up, nothing sent.
        let started = Instant::now();
        let failure = request_within(
            || -> io::Result<Conn> { Err(busy()) },
            classify,
            &envelope("b-4"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert_eq!(failure.delivery, Delivery::NotSent, "{failure:?}");
        assert!(failure.message.contains("stayed busy"), "{failure:?}");
        assert!(started.elapsed() >= QUICK.busy_for);

        // A missing watchdog is not tried again.
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let failure = request_within(
            move || -> io::Result<Conn> {
                counted.fetch_add(1, Ordering::AcqRel);
                Err(Error::from(ErrorKind::NotFound))
            },
            classify,
            &envelope("b-5"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert_eq!(failure.delivery, Delivery::NotSent);
        assert!(failure.message.contains("not running"));
        assert_eq!(opens.load(Ordering::Acquire), 1);
        assert!(all_ended(&in_flight));
    }

    /// A look within a wait keeps to what the wait still has, but never
    /// less than a second nor more than the usual deadline.
    #[test]
    fn a_shorter_deadline_is_kept_within_its_bounds() {
        let within = |most| Limits::within(Client::Cli, most);
        assert_eq!(
            within(Duration::from_secs(4)).deadline,
            Duration::from_secs(4)
        );
        assert_eq!(
            within(Duration::from_millis(10)).deadline,
            Duration::from_secs(1)
        );
        assert_eq!(within(Duration::from_secs(60)).deadline, DEADLINE);
        assert!(within(Duration::from_secs(1)).busy_for <= Duration::from_secs(1));
        assert_eq!(within(DEADLINE).busy_for, BUSY_FOR);
    }

    /// Each client marks its requests as its own: `wgal-mcp`'s as MCP's,
    /// `wgal`'s with no origin, as the window's; and each names its own way
    /// to look again in what it says.
    #[test]
    fn each_client_marks_its_requests_and_words_its_hints() {
        let mut mcp = PipeWatchdog::new(Client::Mcp);
        let e = mcp.envelope(&Request::GetSnapshot).unwrap();
        assert_eq!(e.origin, Some(Origin::Mcp));
        assert!(e.request_id.as_str().starts_with("mcp-"), "{e:?}");
        let mut cli = PipeWatchdog::new(Client::Cli);
        let e = cli.envelope(&Request::GetSnapshot).unwrap();
        assert_eq!(e.origin, None);
        assert!(e.request_id.as_str().starts_with("cli-"), "{e:?}");
        let second = cli.envelope(&Request::GetSnapshot).unwrap();
        assert_ne!(e.request_id, second.request_id);

        let in_flight = InFlight::new(MAX_IN_FLIGHT);
        let cli_limits = Limits {
            client: Client::Cli,
            ..QUICK
        };
        let failure = request_within(
            || {
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(Vec::new()),
                    hold: None,
                })
            },
            classify,
            &envelope("h-1"),
            cli_limits,
            &in_flight,
        )
        .unwrap_err();
        assert!(
            failure.message.ends_with("run \"wgal status\" to see"),
            "{failure:?}"
        );
        let failure = request_within(
            || {
                Ok(Conn {
                    written: Arc::default(),
                    answer: io::Cursor::new(Vec::new()),
                    hold: None,
                })
            },
            classify,
            &envelope("h-2"),
            QUICK,
            &in_flight,
        )
        .unwrap_err();
        assert!(
            failure.message.ends_with(", call health to see"),
            "{failure:?}"
        );
        assert!(all_ended(&in_flight));
    }
}
