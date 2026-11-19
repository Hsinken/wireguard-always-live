//! The GUI's side of the watchdog IPC (plan §3.2).
//!
//! On Windows the watchdog serves the fixed pipe [`WINDOWS_PIPE_NAME`], on
//! Linux and macOS a Unix socket at a fixed path (`wgal_platform::linux::
//! socket`, `wgal_platform::macos::socket`), all with frames from
//! [`codec::write_frame`]. Each request gets its own connection:
//! open, write one frame, read one frame, close. Requests are rare (a
//! snapshot every 5 s and the user's clicks) and the watchdog closes idle
//! connections soon anyway. A connection per request also means a request is
//! never sent again on a reused connection that broke after the watchdog may
//! already have read it, concurrent requests share no lock and cannot
//! interleave their frames, and a stuck exchange holds only its own handle.
//!
//! The exchange is generic over `Read + Write`, so everything but opening the
//! real pipe is tested against fakes. Errors are fixed texts and never quote
//! the request or bytes from the pipe: a request can carry the bot token.
//!
//! [`WINDOWS_PIPE_NAME`]: wgal_protocol::WINDOWS_PIPE_NAME

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_protocol::{
    DecodeError, ErrorCode, ErrorReason, FrameError, RequestId, ResponseEnvelope, codec,
};

/// The whole exchange, busy retries included. `Connect`, `Disconnect` and
/// `Reconnect` are answered once the watchdog has recorded them, not when the
/// tunnel has changed, so every request fits well within it.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

/// How long to keep trying while every instance of the pipe is in use (or
/// the socket's queue is full), and the pause between tries.
const BUSY_FOR: Duration = Duration::from_secs(2);
const BUSY_PAUSE: Duration = Duration::from_millis(50);

/// `winerror.h`: `ERROR_FILE_NOT_FOUND` (no such pipe), `ERROR_ACCESS_DENIED`
/// (the pipe's DACL refuses this account) and `ERROR_PIPE_BUSY` (every
/// instance is in use).
#[cfg(any(windows, test))]
const ERROR_FILE_NOT_FOUND: i32 = 2;
#[cfg(any(windows, test))]
const ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(any(windows, test))]
const ERROR_PIPE_BUSY: i32 = 231;

/// Why a request got no usable answer. Holds nothing from the request or the
/// pipe beyond an OS error code.
///
/// - `NotRunning`: the pipe does not exist; the watchdog is not installed or
///   not running.
/// - `Denied`: the pipe refused this account. The watchdog grants it only to
///   the accounts on the installation's access list (and to SYSTEM and
///   elevated administrators), and grants nobody at all while it cannot read
///   that list, so this is answered as `unauthorized`.
/// - `Busy`: every instance of the pipe stayed in use for the whole retry
///   window.
/// - `NotTheWatchdog`: a pipe of that name exists but LocalSystem does not
///   own it, or the socket or its folder is not the watchdog's user's (root
///   on Linux, the user on macOS), so it is not the WGAL watchdog; nothing
///   was sent and it is not tried again. On Linux and macOS a user the
///   watchdog does not admit is told so in its answer
///   (`caller_not_allowed`), not refused at the door.
/// - `Unreachable`: the pipe could not be opened for another reason, for
///   example its access control list.
/// - `Unsent`: writing the request failed, so the watchdog did not get a
///   whole frame to act on.
/// - `NoAnswer`: the connection ended before a whole answer arrived; the
///   watchdog may have carried the request out.
/// - `Malformed`: the answer is not a response envelope of this contract.
/// - `WrongVersion`: the answer is in another protocol version.
/// - `Mismatched`: the answer names another request, or none.
/// - `Timeout`: no answer within the deadline; the watchdog may still carry
///   the request out.
/// - `Backlog`: too many earlier exchanges are still waiting for the
///   watchdog.
/// - `NoThread`: no thread could be started for the exchange.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    NotRunning,
    NotTheWatchdog,
    Denied,
    Busy,
    Unreachable(Option<i32>),
    Unsent,
    NoAnswer,
    Malformed,
    WrongVersion,
    Mismatched,
    Timeout,
    Backlog,
    NoThread,
}

impl Failure {
    /// The code the webview gets. A failure to reach the watchdog is
    /// `internal`, as before there was a pipe: on a snapshot the GUI shows the
    /// watchdog as lost for any error.
    pub(crate) fn code(self) -> ErrorCode {
        match self {
            Self::Timeout | Self::Backlog => ErrorCode::Timeout,
            Self::WrongVersion => ErrorCode::UnsupportedVersion,
            Self::Denied => ErrorCode::Unauthorized,
            Self::NotRunning
            | Self::NotTheWatchdog
            | Self::Busy
            | Self::Unreachable(_)
            | Self::Unsent
            | Self::NoAnswer
            | Self::Malformed
            | Self::Mismatched
            | Self::NoThread => ErrorCode::Internal,
        }
    }

    /// The reason the webview gets beside the code, so the window can say
    /// which of these it was from its language file: `None` only where the
    /// code says it all (another protocol version).
    pub(crate) fn reason(self) -> Option<ErrorReason> {
        match self {
            Self::NotRunning => Some(ErrorReason::NotRunning),
            Self::NotTheWatchdog => Some(ErrorReason::NotTheWatchdog),
            Self::Denied => Some(ErrorReason::CallerNotAllowed),
            Self::Busy | Self::Backlog => Some(ErrorReason::TooManyWaiting),
            Self::Unreachable(_) | Self::Unsent | Self::NoThread => {
                Some(ErrorReason::CannotReachWatchdog)
            }
            Self::NoAnswer => Some(ErrorReason::AnswerLost),
            Self::Malformed | Self::Mismatched => Some(ErrorReason::InvalidAnswer),
            Self::WrongVersion => None,
            Self::Timeout => Some(ErrorReason::NoAnswerInTime),
        }
    }

    fn message(self) -> String {
        let text = match self {
            Self::NotRunning => crate::NOT_RUNNING,
            Self::NotTheWatchdog => {
                "the pipe or socket is not served by the WGAL watchdog; nothing was sent to it"
            }
            Self::Denied => {
                "the background watchdog refused this account: it is not on the \
                 installation's access list, or the watchdog could not read that list"
            }
            Self::Busy => "every connection to the background watchdog stayed busy",
            Self::Unreachable(Some(code)) => {
                return format!("cannot connect to the background watchdog (OS error {code})");
            }
            Self::Unreachable(None) => "cannot connect to the background watchdog",
            Self::Unsent => "cannot send the request to the background watchdog",
            Self::NoAnswer => {
                "the background watchdog closed the connection without an answer; \
                 the request may have been carried out"
            }
            Self::Malformed => "the background watchdog's answer is not valid",
            Self::WrongVersion => "the background watchdog speaks another protocol version",
            Self::Mismatched => "the background watchdog answered another request",
            // The window adds that an operation may still be carried out.
            Self::Timeout => "the background watchdog did not answer in time",
            Self::Backlog => "earlier requests to the background watchdog are still unanswered",
            Self::NoThread => "cannot start the request to the background watchdog",
        };
        text.to_owned()
    }

    /// The answer the webview gets for `request_id` instead of the
    /// watchdog's.
    pub(crate) fn answer(self, request_id: RequestId) -> ResponseEnvelope {
        crate::refuse(
            Some(request_id),
            self.code(),
            self.reason(),
            &self.message(),
        )
    }
}

/// Counts the exchanges still running, late ones included, so a watchdog that
/// stops answering cannot pile up threads and pipe handles: past the limit a
/// request is answered at once.
#[derive(Debug)]
pub(crate) struct InFlight {
    running: AtomicUsize,
    limit: usize,
}

impl InFlight {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            running: AtomicUsize::new(0),
            limit,
        }
    }

    fn claim(&'static self) -> Option<Claim> {
        self.running
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Claim(self))
    }
}

/// One running exchange, released when it ends.
struct Claim(&'static InFlight);

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Every exchange the window starts. Two run at once in normal use (the
/// heartbeat and a click); eight leaves room for a slow watchdog.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) static IN_FLIGHT: InFlight = InFlight::new(8);

/// Forwards one encoded request and returns the watchdog's answer, or the
/// failure as an answer to the same request.
pub(crate) fn to_watchdog<S, F>(
    open: F,
    frame: Vec<u8>,
    request_id: RequestId,
    in_flight: &'static InFlight,
) -> ResponseEnvelope
where
    S: Read + Write,
    F: FnMut() -> io::Result<S> + Send + 'static,
{
    request_within(open, frame, request_id.clone(), DEADLINE, in_flight)
        .unwrap_or_else(|failure| failure.answer(request_id))
}

/// Opens a connection with `open`, writes `frame`, reads one frame back and
/// checks it answers `request_id`, all on a thread of its own and within
/// `deadline`. A late exchange is left to end by itself, and its connection
/// is closed when it does. The caller blocks, so it must not be the main
/// thread.
pub(crate) fn request_within<S, F>(
    mut open: F,
    frame: Vec<u8>,
    request_id: RequestId,
    deadline: Duration,
    in_flight: &'static InFlight,
) -> Result<ResponseEnvelope, Failure>
where
    S: Read + Write,
    F: FnMut() -> io::Result<S> + Send + 'static,
{
    let claim = in_flight.claim().ok_or(Failure::Backlog)?;
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("wgal-ipc".to_owned())
        .spawn(move || {
            // Held until the exchange ends, however late.
            let _claim = claim;
            let result = connect(&mut open, BUSY_FOR)
                .and_then(|mut stream| exchange(&mut stream, &frame, &request_id));
            // After a timeout nobody listens any more.
            let _ = tx.send(result);
        })
        .map_err(|_| Failure::NoThread)?;
    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Failure::Timeout),
        // The exchange panicked.
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Failure::NoAnswer),
    }
}

/// What to do after opening the connection failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    /// Busy: try again while the busy window lasts.
    Retry,
    Fail(Failure),
}

/// Opens a connection, trying again for `busy_for` while the endpoint is
/// busy. A missing endpoint or any other error ends it at once.
fn connect<S>(open: &mut impl FnMut() -> io::Result<S>, busy_for: Duration) -> Result<S, Failure> {
    let started = Instant::now();
    loop {
        let error = match open() {
            Ok(stream) => return Ok(stream),
            Err(error) => error,
        };
        match classify(&error) {
            Next::Retry if started.elapsed() < busy_for => thread::sleep(BUSY_PAUSE),
            Next::Retry => return Err(Failure::Busy),
            Next::Fail(failure) => return Err(failure),
        }
    }
}

/// A failed pipe open, by its Windows error.
#[cfg(any(windows, test))]
fn classify_pipe(error: &io::Error, not_the_watchdog: bool) -> Next {
    if not_the_watchdog {
        return Next::Fail(Failure::NotTheWatchdog);
    }
    match error.raw_os_error() {
        Some(ERROR_PIPE_BUSY) => Next::Retry,
        Some(ERROR_FILE_NOT_FOUND) => Next::Fail(Failure::NotRunning),
        Some(ERROR_ACCESS_DENIED) => Next::Fail(Failure::Denied),
        code => Next::Fail(Failure::Unreachable(code)),
    }
}

/// A failed socket connect on Linux or macOS, by its kind: no socket, or
/// nobody listening on it, is a watchdog that is not running; a full queue
/// is busy; a socket that is not the watchdog's is refused for good.
#[cfg(any(not(windows), test))]
fn classify_socket(error: &io::Error) -> Next {
    if wgal_platform::socket::is_not_the_watchdog(error) {
        return Next::Fail(Failure::NotTheWatchdog);
    }
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
            Next::Fail(Failure::NotRunning)
        }
        io::ErrorKind::PermissionDenied => Next::Fail(Failure::Denied),
        io::ErrorKind::WouldBlock => Next::Retry,
        _ => Next::Fail(Failure::Unreachable(error.raw_os_error())),
    }
}

#[cfg(windows)]
fn classify(error: &io::Error) -> Next {
    classify_pipe(error, wgal_platform::windows::is_not_the_watchdog(error))
}

#[cfg(not(windows))]
fn classify(error: &io::Error) -> Next {
    classify_socket(error)
}

/// One request frame out, one response frame back. Only an answer that
/// decodes as a response of this contract version and names `request_id` is
/// returned; anything else is a failure, never an answer.
fn exchange<S: Read + Write>(
    stream: &mut S,
    frame: &[u8],
    request_id: &RequestId,
) -> Result<ResponseEnvelope, Failure> {
    codec::write_frame(stream, frame).map_err(|_| Failure::Unsent)?;
    let answer = codec::read_frame(stream).map_err(|error| match error {
        FrameError::Closed | FrameError::Truncated | FrameError::Io(_) => Failure::NoAnswer,
        FrameError::Empty | FrameError::TooLarge { .. } => Failure::Malformed,
    })?;
    let response = codec::decode_response(&answer).map_err(|error| match error {
        DecodeError::UnsupportedVersion { .. } => Failure::WrongVersion,
        DecodeError::TooLarge { .. } | DecodeError::Malformed { .. } => Failure::Malformed,
    })?;
    if response.request_id.as_ref() != Some(request_id) {
        return Err(Failure::Mismatched);
    }
    Ok(response)
}

/// Opens the watchdog's pipe through `wgal_platform`: with exactly the
/// access the pipe grants listed users, at identification level (the
/// watchdog can tell who the GUI runs as but never act as that user), and
/// only if LocalSystem owns it. A pipe another account created under the
/// name while the service was not running is refused before anything is
/// written, so it never sees a request or a bot token. The name is the
/// contract's constant: never configurable and never from the webview.
#[cfg(windows)]
pub(crate) fn open_pipe() -> io::Result<std::fs::File> {
    wgal_platform::windows::connect_watchdog_pipe()
}

/// Connects to the watchdog's socket through `wgal_platform`: only if its
/// folder and the socket are the watchdog's user's and the kernel names that
/// user as the peer (root on Linux, this user on macOS), so a socket anyone
/// else made never sees a request or a bot token. The path is fixed: never
/// configurable and never from the webview.
#[cfg(target_os = "linux")]
pub(crate) fn open_socket() -> io::Result<std::os::unix::net::UnixStream> {
    wgal_platform::linux::socket::connect_watchdog_socket()
}

#[cfg(target_os = "macos")]
pub(crate) fn open_socket() -> io::Result<std::os::unix::net::UnixStream> {
    wgal_platform::macos::socket::connect_watchdog_socket()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use wgal_protocol::{
        BotToken, MAX_MESSAGE_BYTES, Outcome, PROTOCOL_VERSION, ProtocolError, Request,
        RequestEnvelope, Response,
    };

    use super::*;

    fn id(text: &str) -> RequestId {
        RequestId::new(text).unwrap()
    }

    fn request(request: Request) -> Vec<u8> {
        codec::encode_request(&RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: id("r1"),
            request,
        })
        .unwrap()
    }

    fn response(request_id: Option<&str>, outcome: Outcome) -> ResponseEnvelope {
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.map(id),
            outcome,
        }
    }

    fn accepted(request_id: Option<&str>) -> ResponseEnvelope {
        response(request_id, Outcome::Ok(Response::Accepted))
    }

    fn framed(message: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        codec::write_frame(&mut out, message).unwrap();
        out
    }

    fn framed_response(envelope: &ResponseEnvelope) -> Vec<u8> {
        framed(&codec::encode_response(envelope).unwrap())
    }

    /// The client's end of a fake pipe: records what is written and answers
    /// with fixed bytes, then end of stream. Tells when it is closed.
    struct Pipe {
        written: Arc<Mutex<Vec<u8>>>,
        answer: io::Cursor<Vec<u8>>,
        closed: Arc<AtomicBool>,
    }

    impl Pipe {
        fn answering(answer: Vec<u8>) -> Self {
            Self {
                written: Arc::default(),
                answer: io::Cursor::new(answer),
                closed: Arc::default(),
            }
        }
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.answer.read(buf)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Pipe {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn exchange_with(answer: Vec<u8>) -> Result<ResponseEnvelope, Failure> {
        exchange(
            &mut Pipe::answering(answer),
            &request(Request::GetSnapshot),
            &id("r1"),
        )
    }

    #[test]
    fn one_request_frame_goes_out_and_its_answer_comes_back() {
        let frame = request(Request::GetSnapshot);
        let mut pipe = Pipe::answering(framed_response(&accepted(Some("r1"))));
        let written = Arc::clone(&pipe.written);
        assert_eq!(
            exchange(&mut pipe, &frame, &id("r1")),
            Ok(accepted(Some("r1")))
        );
        assert_eq!(*written.lock().unwrap(), framed(&frame));

        // An error from the watchdog is its answer too, passed on as given.
        let refused = response(
            Some("r1"),
            Outcome::Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                Some(ErrorReason::CallerNotAllowed),
                "not on the access list",
            )),
        );
        assert_eq!(exchange_with(framed_response(&refused)), Ok(refused));
    }

    #[test]
    fn an_answer_to_another_request_or_to_none_is_refused() {
        assert_eq!(
            exchange_with(framed_response(&accepted(Some("r2")))),
            Err(Failure::Mismatched)
        );
        assert_eq!(
            exchange_with(framed_response(&accepted(None))),
            Err(Failure::Mismatched)
        );
        let unpaired = response(
            None,
            Outcome::Err(ProtocolError::new(
                ErrorCode::MalformedRequest,
                None,
                "unreadable",
            )),
        );
        assert_eq!(
            exchange_with(framed_response(&unpaired)),
            Err(Failure::Mismatched)
        );
    }

    #[test]
    fn a_missing_or_cut_short_answer_is_no_answer() {
        assert_eq!(exchange_with(Vec::new()), Err(Failure::NoAnswer));
        let whole = framed_response(&accepted(Some("r1")));
        for cut in 1..whole.len() {
            assert_eq!(
                exchange_with(whole[..cut].to_vec()),
                Err(Failure::NoAnswer),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn an_empty_oversized_or_garbage_answer_is_refused() {
        assert_eq!(exchange_with(vec![0; 4]), Err(Failure::Malformed));

        // The body of an oversized frame is never read.
        let mut over = (MAX_MESSAGE_BYTES + 1).to_le_bytes().to_vec();
        over.extend_from_slice(b"{}");
        let mut pipe = Pipe::answering(over);
        assert_eq!(
            exchange(&mut pipe, &request(Request::GetSnapshot), &id("r1")),
            Err(Failure::Malformed)
        );
        assert_eq!(pipe.answer.position(), 4);

        for garbage in [
            &b"not json"[..],
            br#"{"protocol_version":1,"request_id":"r1"}"#,
            br#"{"protocol_version":1,"request_id":"r1","outcome":{"ok":{"kind":"connected"}}}"#,
        ] {
            assert_eq!(
                exchange_with(framed(garbage)),
                Err(Failure::Malformed),
                "{}",
                String::from_utf8_lossy(garbage)
            );
        }
        // A field a newer watchdog added is ignored: answers are read
        // leniently, so this window can still say it is too old (owner
        // decision of 2026-11-04).
        assert_eq!(
            exchange_with(framed(
                br#"{"protocol_version":1,"request_id":"r1","outcome":{"ok":{"kind":"accepted"}},"extra":1}"#
            )),
            exchange_with(framed_response(&accepted(Some("r1"))))
        );
        assert_eq!(
            exchange_with(framed(
                br#"{"protocol_version":2,"request_id":"r1","outcome":{"ok":{"kind":"accepted"}}}"#
            )),
            Err(Failure::WrongVersion)
        );
    }

    #[test]
    fn a_request_that_cannot_be_written_is_unsent() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("nothing is read after a failed write");
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            exchange(&mut Broken, &request(Request::GetSnapshot), &id("r1")),
            Err(Failure::Unsent)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_busy_pipe_is_tried_again_for_a_while_and_nothing_else_is() {
        let mut calls = 0;
        let mut busy_twice = || {
            calls += 1;
            if calls <= 2 {
                Err(io::Error::from_raw_os_error(ERROR_PIPE_BUSY))
            } else {
                Ok(())
            }
        };
        assert_eq!(connect(&mut busy_twice, BUSY_FOR), Ok(()));
        assert_eq!(calls, 3);

        let started = Instant::now();
        let window = Duration::from_millis(150);
        let mut always_busy = || Err::<(), _>(io::Error::from_raw_os_error(ERROR_PIPE_BUSY));
        assert_eq!(connect(&mut always_busy, window), Err(Failure::Busy));
        assert!(started.elapsed() >= window);
        assert!(started.elapsed() < window + Duration::from_secs(1));

        for (code, failure) in [
            (Some(ERROR_FILE_NOT_FOUND), Failure::NotRunning),
            (Some(ERROR_ACCESS_DENIED), Failure::Denied),
            (Some(6), Failure::Unreachable(Some(6))),
            (None, Failure::Unreachable(None)),
        ] {
            let mut calls = 0;
            let mut failing = || {
                calls += 1;
                Err::<(), _>(match code {
                    Some(code) => io::Error::from_raw_os_error(code),
                    None => io::Error::other("no OS code"),
                })
            };
            assert_eq!(connect(&mut failing, BUSY_FOR), Err(failure));
            assert_eq!(calls, 1, "{failure:?} is not retried");
        }
    }

    #[test]
    fn a_pipe_and_a_socket_are_retried_only_while_busy() {
        use io::ErrorKind as K;
        for (code, next) in [
            (ERROR_PIPE_BUSY, Next::Retry),
            (ERROR_FILE_NOT_FOUND, Next::Fail(Failure::NotRunning)),
            (ERROR_ACCESS_DENIED, Next::Fail(Failure::Denied)),
            (6, Next::Fail(Failure::Unreachable(Some(6)))),
        ] {
            assert_eq!(
                classify_pipe(&io::Error::from_raw_os_error(code), false),
                next
            );
        }
        assert_eq!(
            classify_pipe(&io::Error::from_raw_os_error(ERROR_PIPE_BUSY), true),
            Next::Fail(Failure::NotTheWatchdog)
        );
        for (error, next) in [
            (
                io::Error::from(K::NotFound),
                Next::Fail(Failure::NotRunning),
            ),
            (
                io::Error::from(K::ConnectionRefused),
                Next::Fail(Failure::NotRunning),
            ),
            (
                io::Error::from(K::PermissionDenied),
                Next::Fail(Failure::Denied),
            ),
            (io::Error::from(K::WouldBlock), Next::Retry),
            (
                wgal_platform::socket::not_the_watchdog(),
                Next::Fail(Failure::NotTheWatchdog),
            ),
            (
                io::Error::from(K::InvalidInput),
                Next::Fail(Failure::Unreachable(None)),
            ),
        ] {
            assert_eq!(classify_socket(&error), next, "{error:?}");
        }
    }

    #[test]
    fn a_whole_exchange_on_its_own_thread_closes_its_pipe() {
        static LANE: InFlight = InFlight::new(2);
        let pipe = Pipe::answering(framed_response(&accepted(Some("r1"))));
        let closed = Arc::clone(&pipe.closed);
        let mut pipe = Some(pipe);
        let open = move || pipe.take().ok_or_else(|| io::Error::other("opened twice"));
        assert_eq!(
            request_within(
                open,
                request(Request::GetSnapshot),
                id("r1"),
                DEADLINE,
                &LANE
            ),
            Ok(accepted(Some("r1")))
        );
        assert!(closed.load(Ordering::SeqCst));
        // The thread hands the answer back before it leaves its lane, so the
        // count drops a moment after the answer arrives.
        assert!(
            wait_until(|| LANE.running.load(Ordering::SeqCst) == 0),
            "the exchange thread still holds its lane ({} running)",
            LANE.running.load(Ordering::SeqCst)
        );
    }

    /// A pipe whose watchdog never answers until `release` is sent or
    /// dropped.
    struct Stuck {
        release: mpsc::Receiver<()>,
        closed: Arc<AtomicBool>,
    }

    impl Read for Stuck {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.release.recv();
            Ok(0)
        }
    }

    impl Write for Stuck {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Stuck {
        fn drop(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn stuck() -> (
        impl FnMut() -> io::Result<Stuck> + Send + 'static,
        mpsc::Sender<()>,
        Arc<AtomicBool>,
    ) {
        let (release, wait) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let mut pipe = Some(Stuck {
            release: wait,
            closed: Arc::clone(&closed),
        });
        let open = move || pipe.take().ok_or_else(|| io::Error::other("opened twice"));
        (open, release, closed)
    }

    fn wait_until(what: impl Fn() -> bool) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if what() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        what()
    }

    #[test]
    fn past_the_deadline_the_answer_is_timeout_and_the_late_pipe_closes_by_itself() {
        static LANE: InFlight = InFlight::new(2);
        let (open, release, closed) = stuck();
        let started = Instant::now();
        let deadline = Duration::from_millis(100);
        assert_eq!(
            request_within(
                open,
                request(Request::GetSnapshot),
                id("r1"),
                deadline,
                &LANE
            ),
            Err(Failure::Timeout)
        );
        assert!(started.elapsed() >= deadline);
        assert!(started.elapsed() < Duration::from_secs(2));
        // Still running: the handle stays open until the exchange ends.
        assert!(!closed.load(Ordering::SeqCst));
        assert_eq!(LANE.running.load(Ordering::SeqCst), 1);

        release.send(()).unwrap();
        assert!(wait_until(|| closed.load(Ordering::SeqCst)));
        assert!(wait_until(|| LANE.running.load(Ordering::SeqCst) == 0));
    }

    #[test]
    fn a_watchdog_that_stops_answering_cannot_pile_up_exchanges() {
        static LANE: InFlight = InFlight::new(1);
        let (open, release, _closed) = stuck();
        let short = Duration::from_millis(50);
        assert_eq!(
            request_within(open, request(Request::GetSnapshot), id("r1"), short, &LANE),
            Err(Failure::Timeout)
        );
        let never_opened = || -> io::Result<Pipe> { panic!("no exchange starts past the limit") };
        assert_eq!(
            request_within(
                never_opened,
                request(Request::GetSnapshot),
                id("r1"),
                short,
                &LANE
            ),
            Err(Failure::Backlog)
        );

        drop(release);
        assert!(wait_until(|| LANE.running.load(Ordering::SeqCst) == 0));
        let pipe = Pipe::answering(framed_response(&accepted(Some("r1"))));
        let mut pipe = Some(pipe);
        let open = move || pipe.take().ok_or_else(|| io::Error::other("opened twice"));
        assert!(
            request_within(
                open,
                request(Request::GetSnapshot),
                id("r1"),
                DEADLINE,
                &LANE
            )
            .is_ok()
        );
    }

    #[test]
    fn every_failure_answers_the_request_with_a_code_and_never_its_contents() {
        static LANE: InFlight = InFlight::new(2);
        let secret = "123456789:EXAMPLE-not-a-real-token_0000000000";
        let frame = request(Request::SetBotToken {
            token: BotToken::new(secret).unwrap(),
        });
        // A watchdog that answers garbage after reading the token.
        let mut pipe = Some(Pipe::answering(framed(b"garbage")));
        let open = move || pipe.take().ok_or_else(|| io::Error::other("opened twice"));
        let answer = to_watchdog(open, frame, id("r1"), &LANE);
        assert_eq!(answer.request_id, Some(id("r1")));
        assert!(!format!("{answer:?}").contains("EXAMPLE"));

        let missing =
            || -> io::Result<Pipe> { Err(io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND)) };
        let answer = to_watchdog(missing, request(Request::GetSnapshot), id("r1"), &LANE);
        assert_eq!(
            answer,
            crate::refuse(
                Some(id("r1")),
                ErrorCode::Internal,
                Some(ErrorReason::NotRunning),
                crate::NOT_RUNNING
            )
        );

        use ErrorReason as R;
        for (failure, code, reason) in [
            (
                Failure::NotRunning,
                ErrorCode::Internal,
                Some(R::NotRunning),
            ),
            (
                Failure::NotTheWatchdog,
                ErrorCode::Internal,
                Some(R::NotTheWatchdog),
            ),
            (
                Failure::Denied,
                ErrorCode::Unauthorized,
                Some(R::CallerNotAllowed),
            ),
            (Failure::Busy, ErrorCode::Internal, Some(R::TooManyWaiting)),
            (
                Failure::Unreachable(Some(6)),
                ErrorCode::Internal,
                Some(R::CannotReachWatchdog),
            ),
            (
                Failure::Unreachable(None),
                ErrorCode::Internal,
                Some(R::CannotReachWatchdog),
            ),
            (
                Failure::Unsent,
                ErrorCode::Internal,
                Some(R::CannotReachWatchdog),
            ),
            (Failure::NoAnswer, ErrorCode::Internal, Some(R::AnswerLost)),
            (
                Failure::Malformed,
                ErrorCode::Internal,
                Some(R::InvalidAnswer),
            ),
            (Failure::WrongVersion, ErrorCode::UnsupportedVersion, None),
            (
                Failure::Mismatched,
                ErrorCode::Internal,
                Some(R::InvalidAnswer),
            ),
            (
                Failure::Timeout,
                ErrorCode::Timeout,
                Some(R::NoAnswerInTime),
            ),
            (
                Failure::Backlog,
                ErrorCode::Timeout,
                Some(R::TooManyWaiting),
            ),
            (
                Failure::NoThread,
                ErrorCode::Internal,
                Some(R::CannotReachWatchdog),
            ),
        ] {
            let answer = failure.answer(id("r9"));
            assert_eq!(answer.request_id, Some(id("r9")));
            assert_eq!(answer.protocol_version, PROTOCOL_VERSION);
            match answer.outcome {
                Outcome::Err(error) => {
                    assert_eq!(error.code, code, "{failure:?}");
                    assert_eq!(error.reason, reason, "{failure:?}");
                    assert!(!error.message.as_str().is_empty());
                    assert!(!error.message.as_str().contains("EXAMPLE"));
                    assert!(!error.message.as_str().contains("  "), "{failure:?}");
                }
                Outcome::Ok(_) => panic!("{failure:?} must never read as an answer"),
            }
        }
    }

    /// A pipe LocalSystem does not own is refused by `wgal_platform` (its own
    /// tests serve one); here that refusal must end the connect at once and
    /// read as "not the watchdog", whatever OS code it may carry.
    #[cfg(windows)]
    #[test]
    fn a_pipe_not_owned_by_the_service_is_refused_and_not_retried() {
        let mut calls = 0;
        let mut squatted = || {
            calls += 1;
            Err::<(), _>(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                wgal_platform::windows::NotTheWatchdog,
            ))
        };
        assert_eq!(
            connect(&mut squatted, BUSY_FOR),
            Err(Failure::NotTheWatchdog)
        );
        assert_eq!(calls, 1);
        assert_eq!(Failure::NotTheWatchdog.code(), ErrorCode::Internal);
        assert_eq!(
            Failure::NotTheWatchdog.reason(),
            Some(ErrorReason::NotTheWatchdog)
        );
    }
}
