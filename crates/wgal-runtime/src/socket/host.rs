//! Hosting the watchdog on Linux and macOS: its folders, its socket, its
//! loop, and a clean stop on `SIGTERM` or `SIGINT` (what systemd and launchd
//! send). The platform's entry point (`crate::linux`, `crate::macos`) gives
//! the [`Layout`], the adapter, the secret store, the access list and how
//! to read a peer's identity; everything else is here, and mirrors the
//! Windows hosting (`crate::windows::host`) without the parts only Windows
//! has (the service control manager, wake-ups, network-change notices).
//!
//! # The folders
//!
//! [`Layout::root`] must be a real, private folder (mode 0700) owned by the
//! watchdog's user, or nothing is started and nothing is written there
//! ([`EXIT_FOLDER`]): systemd makes it (`StateDirectory=wgal`) on Linux,
//! the entry point on macOS. In it, `state`, `notify` and `events`
//! are created with mode 0700 when missing, and refused when they are not
//! such folders ([`EXIT_SUBFOLDER`]). The socket's folder is the platform's
//! to provide and check.
//!
//! # Running
//!
//! The socket server's handler calls the loop with [`CALL_DEADLINE`] for
//! every request, the loop's own screen refusing callers that are not
//! allowed. Every [`Health::DEFAULT`] interval the hosting asks the loop
//! for a snapshot as the watchdog itself; a loop whose owner thread is gone
//! (`not_running`, or `shutting_down` before the hosting began to stop), at
//! the first such check, or one that keeps failing to answer (it hangs),
//! ends the process with [`EXIT_LOOP_DIED`], so systemd or launchd start it
//! again. A refusal for too many waiting requests is a failed check only
//! when the owner finished no turn since the last one (FIX-04, AR-07, as on
//! Windows).
//!
//! The socket is only the clients' way in. If it cannot be served (another
//! instance answers on it, say) or its server ends, the loop goes on
//! monitoring and recovering, and the socket is served again after
//! [`REBIND_DELAYS`], the first three attempts in a row and every tenth
//! written to the log. A server thread that panics reports its end too
//! (from a drop guard) and counts as one that answered nothing.
//!
//! # Stopping
//!
//! The socket server stops first, then the loop, within [`STOP_DEADLINE`]
//! plus [`STOP_MARGIN`]; a loop that does not finish in time is given up
//! and the process ends with it (its last step resumes after a restart).
//!
//! # The log
//!
//! Lines go to standard error, where systemd's journal (`journalctl -u
//! wgal-watchdog`) or the LaunchAgent's log file keeps them. They name what
//! happened, never a request, a token or a peer.

use std::fs::{self, DirBuilder};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_core::{PlatformAdapter, Startup, SystemClock, Timing, Watchdog};
use wgal_platform::socket::{EntryKind, entry_of, trusted_folder};
use wgal_protocol::{
    ErrorReason, Outcome, PROTOCOL_VERSION, Request, RequestEnvelope, RequestId, Response,
};

use super::server::{Server, ServerError, ServerHandle, ServerLimits};
use crate::driver::{self, Client, Running};
use crate::events::EventLog;
use crate::ipc::{AccessList, Caller};
use crate::notify::{FileSettingsStore, Notifier, SecretStore, Transport};
use crate::store::FileStore;

pub const STATE_DIR: &str = "state";
pub const NOTIFY_DIR: &str = "notify";
pub const EVENTS_DIR: &str = "events";

/// How long the socket's handler waits for the loop: shorter than the
/// window's own 10 s, so the answer comes back first.
pub const CALL_DEADLINE: Duration = Duration::from_secs(8);

/// How long a stop waits for the loop, which covers a stop or start of a
/// tunnel already handed out (the adapters allow 25 s).
pub const STOP_DEADLINE: Duration = Duration::from_secs(30);

/// How much longer the whole stop waits before giving the loop up.
pub const STOP_MARGIN: Duration = Duration::from_secs(5);

/// How long a stop waits for the socket server's threads.
pub const SERVER_STOP: Duration = Duration::from_secs(10);

/// Waits before serving the socket again, by attempt; the last repeats.
pub const REBIND_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
];

/// The exit codes, as systemd shows them (`status=`).
pub const EXIT_FOLDER: u8 = 1;
pub const EXIT_SUBFOLDER: u8 = 2;
pub const EXIT_STORE: u8 = 3;
pub const EXIT_LOOP_START: u8 = 4;
/// Started by the wrong user (root on macOS, anyone else on Linux).
pub const EXIT_USER: u8 = 5;
pub const EXIT_LOOP_DIED: u8 = 11;

/// Where the watchdog keeps its data and its socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub root: PathBuf,
    pub socket: PathBuf,
    /// The socket's permission bits: 0o666 on Linux, where the watchdog
    /// answers every local user and admits only the listed ones; 0o600 on
    /// macOS, where only the user may connect.
    pub socket_mode: u32,
}

impl Layout {
    pub fn sub(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

/// Reads a peer's identity from its connection: `None` when it cannot.
pub type Identify = Arc<dyn Fn(&UnixStream) -> Option<Caller> + Send + Sync>;

/// What the platform's entry point gives the hosting.
pub struct Platform<A, S> {
    pub adapter: A,
    pub secrets: S,
    /// The tunnels callers without administrator rights may register and
    /// hand to automatic actions (empty on macOS, where every caller is
    /// the user and counts as elevated).
    pub access: AccessList,
    pub identify: Identify,
    /// The watchdog itself, for its health checks.
    pub own: Caller,
    /// This process's user, which owns the folders.
    pub uid: u32,
}

impl<A, S> std::fmt::Debug for Platform<A, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform")
            .field("access", &self.access)
            .field("own", &self.own)
            .finish_non_exhaustive()
    }
}

/// One line on standard error, with the time in UTC.
pub fn log(line: &str) {
    let now = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ");
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{now} wgal-watchdog: {line}");
}

/// Why the start failed, with its exit code.
fn failed(code: u8, line: &str) -> ExitCode {
    log(line);
    ExitCode::from(code)
}

/// Checks `root` and creates or checks its subfolders.
fn prepare(layout: &Layout, uid: u32) -> Result<(), ExitCode> {
    match entry_of(&layout.root) {
        Ok(entry) if trusted_folder(&entry, uid, true) => {}
        Ok(_) => {
            return Err(failed(
                EXIT_FOLDER,
                "the data folder is not a private folder of this user (mode 0700); not started",
            ));
        }
        Err(e) => {
            return Err(failed(
                EXIT_FOLDER,
                &format!("the data folder cannot be read ({}); not started", e.kind()),
            ));
        }
    }
    for name in [STATE_DIR, NOTIFY_DIR, EVENTS_DIR] {
        let path = layout.sub(name);
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(failed(
                    EXIT_SUBFOLDER,
                    &format!(
                        "the {name} folder cannot be made ({}); not started",
                        e.kind()
                    ),
                ));
            }
        }
        let trusted = entry_of(&path).is_ok_and(|entry| trusted_folder(&entry, uid, true));
        if !trusted {
            return Err(failed(
                EXIT_SUBFOLDER,
                &format!(
                    "the {name} folder is not a private folder of this user (mode 0700); not \
                     started"
                ),
            ));
        }
    }
    Ok(())
}

fn startup_note(startup: &Startup) -> &'static str {
    match startup {
        Startup::Empty => "no stored state; starting empty",
        Startup::Restored(_) => "stored state loaded",
        Startup::RestoredFromBackup(_) => {
            "the current state file was damaged; the previous copy was loaded"
        }
        Startup::StorageFailed => {
            "stored state could not be loaded; storage is read-only and automatic actions are off"
        }
    }
}

/// The Telegram transport, or one that never sends in a build without it.
#[cfg(feature = "telegram")]
fn transport() -> Box<dyn Transport + Send> {
    Box::new(crate::notify::HttpTransport::new())
}

#[cfg(not(feature = "telegram"))]
fn transport() -> Box<dyn Transport + Send> {
    #[derive(Debug)]
    struct Offline;
    impl Transport for Offline {
        fn send_message(
            &self,
            _: &wgal_protocol::BotToken,
            _: &wgal_protocol::ChatId,
            _: &str,
        ) -> crate::notify::Delivery {
            crate::notify::Delivery::NotSent
        }
    }
    Box::new(Offline)
}

/// Which file a socket path is now: its device and inode, if it is a
/// socket.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.file_type()
        .is_socket()
        .then(|| (meta.dev(), meta.ino()))
}

/// Binds the socket at `path` with `mode`. A socket left by an earlier run
/// is removed first, unless something still answers on it (another
/// instance); anything at the path that is not a socket is left alone and
/// the bind fails.
fn bind(path: &Path, mode: u32) -> io::Result<UnixListener> {
    match entry_of(path) {
        Ok(entry) if entry.kind == EntryKind::Socket => {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another process answers on the socket",
                ));
            }
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "something other than a socket is at the socket's path",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

/// What stops the hosting or reports to it.
#[derive(Debug)]
enum Control {
    Stop,
    ServerEnded(ServerEnd),
}

/// How the socket server's thread ended.
#[derive(Debug)]
enum ServerEnd {
    /// `run` returned.
    Returned(Result<(), ServerError>),
    /// It unwound from a panic before `run` returned (AR-07).
    Panicked,
}

/// Sends [`Control::ServerEnded`] when the socket server's thread ends,
/// also when it unwinds from a panic, so the hosting always cleans the
/// server up and serves the socket again (AR-07).
struct EndNotice {
    controls: Sender<Control>,
    returned: Option<Result<(), ServerError>>,
}

impl EndNotice {
    fn returned(&mut self, result: Result<(), ServerError>) {
        self.returned = Some(result);
    }
}

impl Drop for EndNotice {
    fn drop(&mut self) {
        let end = self
            .returned
            .take()
            .map_or(ServerEnd::Panicked, ServerEnd::Returned);
        // The hosting may already be gone.
        let _ = self.controls.send(Control::ServerEnded(end));
    }
}

/// How the hosting watches its loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub every: Duration,
    pub failures: u32,
    pub deadline: Duration,
}

impl Health {
    /// A hung loop is noticed within about 90 s; a loop that is gone at the
    /// first check after it went (FIX-04, AR-07).
    pub const DEFAULT: Self = Self {
        every: Duration::from_secs(30),
        failures: 3,
        deadline: CALL_DEADLINE,
    };
}

/// A socket server running on its own thread.
struct Served {
    handle: ServerHandle<UnixStream>,
    thread: JoinHandle<()>,
}

/// Runs the watchdog until `SIGTERM` or `SIGINT`, or until its loop dies,
/// and returns the process's exit code.
pub fn run<A, S>(layout: &Layout, platform: Platform<A, S>) -> ExitCode
where
    A: PlatformAdapter + 'static,
    S: SecretStore + 'static,
{
    log("starting");
    if let Err(code) = prepare(layout, platform.uid) {
        return code;
    }
    let (store, startup, _) = match FileStore::open(&layout.sub(STATE_DIR)) {
        Ok(opened) => opened,
        Err(e) => {
            return failed(
                EXIT_STORE,
                &format!("the state store cannot be opened: {e}"),
            );
        }
    };
    log(startup_note(&startup));
    let watchdog = Watchdog::new(
        platform.adapter,
        SystemClock::new(),
        Timing::default(),
        Box::new(store),
        startup,
    );
    let notifier = Notifier::new(
        FileSettingsStore::new(&layout.sub(NOTIFY_DIR)),
        platform.secrets,
    );
    let events = match EventLog::open(&layout.sub(EVENTS_DIR)) {
        Ok(events) => Some(events),
        Err(e) => {
            log(&format!(
                "the event log is unavailable, monitoring goes on without it: {}",
                e.kind()
            ));
            None
        }
    };
    let (client, running) =
        match driver::spawn(watchdog, notifier, transport(), platform.access, events) {
            Ok(spawned) => spawned,
            Err(e) => {
                return failed(
                    EXIT_LOOP_START,
                    &format!("the watchdog loop cannot start: {}", e.kind()),
                );
            }
        };
    let (controls, inbox) = mpsc::channel();
    if let Err(e) = watch_signals(controls.clone()) {
        log(&format!(
            "SIGTERM and SIGINT cannot be caught ({}); a stop ends the process at once",
            e.kind()
        ));
    }
    let mut hosting = Hosting {
        layout: layout.clone(),
        client,
        identify: platform.identify,
        own: platform.own,
        controls,
        served: None,
        bound: None,
        bind_failures: 0,
        server_ends: 0,
        next_bind: None,
    };
    hosting.serve();
    log("running");
    let died = hosting.watch(&inbox, Health::DEFAULT, &running);
    // A stop that was asked for ends in success even when threads were
    // stuck (that is logged), so systemd or launchd do not restart what
    // the user stopped; a dead loop does not.
    hosting.finish(running);
    if died {
        ExitCode::from(EXIT_LOOP_DIED)
    } else {
        ExitCode::SUCCESS
    }
}

/// Turns `SIGTERM` and `SIGINT` into [`Control::Stop`].
fn watch_signals(controls: Sender<Control>) -> io::Result<()> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])?;
    thread::Builder::new()
        .name("wgal-signals".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = controls.send(Control::Stop);
            }
        })?;
    Ok(())
}

struct Hosting {
    layout: Layout,
    client: Client,
    identify: Identify,
    own: Caller,
    controls: Sender<Control>,
    served: Option<Served>,
    /// The socket file this instance bound last (device and inode), the
    /// only one a stop removes.
    bound: Option<(u64, u64)>,
    bind_failures: usize,
    server_ends: usize,
    next_bind: Option<Instant>,
}

fn logged(n: usize) -> bool {
    n <= 3 || n.is_multiple_of(10)
}

fn rebind_delay(n: usize) -> Duration {
    REBIND_DELAYS[n.saturating_sub(1).min(REBIND_DELAYS.len() - 1)]
}

/// What one health check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// The loop answered with a snapshot.
    Answered,
    /// The loop's owner thread is gone (`not_running`), or the loop is
    /// stopping on its own (`shutting_down`: the hosting starts no stop of
    /// its own before [`Hosting::watch`] returns).
    NotRunning,
    /// Refused unasked: too many requests wait (`too_many_waiting`).
    TooManyWaiting,
    /// No snapshot for any other reason (no answer in time).
    Missed,
}

/// Asks the loop for a snapshot as the watchdog itself, within `deadline`.
fn check(client: &Client, own: &Caller, deadline: Duration) -> Check {
    let Ok(request_id) = RequestId::new("wgal-health") else {
        return Check::Answered;
    };
    let answer = client.call(
        own,
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            request: Request::GetSnapshot,
            origin: None,
        },
        deadline,
    );
    match answer.outcome {
        Outcome::Ok(Response::Snapshot { .. }) => Check::Answered,
        Outcome::Err(e)
            if matches!(
                e.reason,
                Some(ErrorReason::NotRunning | ErrorReason::ShuttingDown)
            ) =>
        {
            Check::NotRunning
        }
        Outcome::Err(e) if e.reason == Some(ErrorReason::TooManyWaiting) => Check::TooManyWaiting,
        _ => Check::Missed,
    }
}

/// What the checks so far say about the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Healthy,
    /// It missed this many checks in a row; `waiting` when this one was
    /// refused for too many waiting requests while it finished no turn;
    /// `dead` when that makes [`Health::failures`].
    Missed {
        in_a_row: u32,
        waiting: bool,
        dead: bool,
    },
    /// Its owner thread is gone.
    Gone,
}

/// The health checks in a row, and the owner's turn count at the last one
/// (FIX-04, AR-07; the same rules as `crate::windows::host`). A loop whose
/// owner thread is gone is dead at once. A refusal for too many waiting
/// requests is a miss only when the owner finished no turn since the last
/// check: requests that timed out keep their places until the owner
/// answers them, so a hung owner is refused this way at every check, while
/// a busy one keeps turning.
#[derive(Debug)]
struct Checks {
    misses: u32,
    turns: u64,
}

impl Checks {
    fn new(turns: u64) -> Self {
        Self { misses: 0, turns }
    }

    fn record(&mut self, check: Check, turns: u64, failures: u32) -> Verdict {
        let turned = turns != self.turns;
        self.turns = turns;
        let waiting = match check {
            Check::Answered => {
                self.misses = 0;
                return Verdict::Healthy;
            }
            Check::NotRunning => return Verdict::Gone,
            Check::TooManyWaiting if turned => {
                self.misses = 0;
                return Verdict::Healthy;
            }
            Check::TooManyWaiting => true,
            Check::Missed => false,
        };
        self.misses += 1;
        Verdict::Missed {
            in_a_row: self.misses,
            waiting,
            dead: self.misses >= failures,
        }
    }
}

/// The log line for a missed health check.
fn missed(in_a_row: u32, waiting: bool) -> String {
    if waiting {
        format!(
            "the watchdog loop did not answer ({in_a_row} in a row; too many requests wait and it \
             finished no turn since the last check)"
        )
    } else {
        format!("the watchdog loop did not answer ({in_a_row} in a row)")
    }
}

impl Hosting {
    /// Serves the socket, or schedules another attempt.
    fn serve(&mut self) {
        self.next_bind = None;
        let failure = match bind(&self.layout.socket, self.layout.socket_mode) {
            Ok(listener) => match Server::new(listener, ServerLimits::DEFAULT) {
                Ok(server) => {
                    self.bound = socket_identity(&self.layout.socket);
                    let handle = server.handle();
                    let client = self.client.clone();
                    let identify = Arc::clone(&self.identify);
                    let controls = self.controls.clone();
                    let spawned = thread::Builder::new()
                        .name("wgal-socket-server".to_owned())
                        .spawn(move || {
                            // Made in the thread, so a thread that never
                            // started reports nothing (Codex review, P3-02).
                            let mut notice = EndNotice {
                                controls,
                                returned: None,
                            };
                            notice.returned(server.run(
                                move |conn| identify(conn),
                                move |caller, envelope| {
                                    client.call(caller, envelope, CALL_DEADLINE)
                                },
                            ));
                            // `notice` reports the end when it is dropped
                            // here, or while unwinding.
                        });
                    match spawned {
                        Ok(thread) => {
                            self.served = Some(Served { handle, thread });
                            if self.bind_failures > 0 {
                                log(&format!(
                                    "the socket is served again after {} failed attempts",
                                    self.bind_failures
                                ));
                            }
                            self.bind_failures = 0;
                            return;
                        }
                        Err(e) => format!("its thread could not start: {}", e.kind()),
                    }
                }
                Err(e) => e.to_string(),
            },
            Err(e) => format!("{} ({})", e.kind(), e),
        };
        self.bind_failures += 1;
        let attempt = self.bind_failures;
        let delay = rebind_delay(attempt);
        self.next_bind = Some(Instant::now() + delay);
        if logged(attempt) {
            log(&format!(
                "the socket could not be served (attempt {attempt}): {failure}; the watchdog goes \
                 on without it and tries again in {} s",
                delay.as_secs()
            ));
        }
    }

    /// The socket server ended on its own: see `crate::windows::host`'s
    /// `pipe_ended`. A server whose thread panicked is stopped first and
    /// counts as one that answered nothing.
    fn server_ended(&mut self, end: ServerEnd) {
        let panicked = matches!(end, ServerEnd::Panicked);
        let answered = match self.served.take() {
            Some(served) => {
                if panicked {
                    served.handle.stop();
                }
                let answered = !panicked && served.handle.stats().requests > 0;
                // A panic is in the join's result, ignored.
                let _ = served.thread.join();
                answered
            }
            None => false,
        };
        self.server_ends = if answered { 0 } else { self.server_ends + 1 };
        let ends = self.server_ends;
        let delay = if ends == 0 {
            Duration::ZERO
        } else {
            rebind_delay(ends)
        };
        self.next_bind = Some(Instant::now() + delay);
        if ends == 0 || logged(ends) {
            let what = match end {
                ServerEnd::Returned(Ok(())) => "the socket server stopped".to_owned(),
                ServerEnd::Returned(Err(error)) => format!("the socket server failed: {error}"),
                ServerEnd::Panicked => "the socket server's thread panicked".to_owned(),
            };
            log(&format!(
                "{what} ({ends} in a row without answering a request); serving the socket again \
                 in {} s",
                delay.as_secs()
            ));
        }
    }

    /// Waits for a stop, serves the socket again when due, and checks the
    /// loop. True when the loop died.
    fn watch(&mut self, inbox: &Receiver<Control>, health: Health, running: &Running) -> bool {
        let mut checks = Checks::new(running.stats().turns);
        let mut next_check = Instant::now() + health.every;
        loop {
            let until = self.next_bind.map_or(next_check, |at| at.min(next_check));
            match inbox.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Control::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    log("stopping as asked");
                    return false;
                }
                Ok(Control::ServerEnded(end)) => self.server_ended(end),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if self.next_bind.is_some_and(|at| now >= at) {
                self.serve();
            }
            if now >= next_check {
                next_check = Instant::now() + health.every;
                let found = check(&self.client, &self.own, health.deadline);
                match checks.record(found, running.stats().turns, health.failures) {
                    Verdict::Healthy => {}
                    Verdict::Missed {
                        in_a_row,
                        waiting,
                        dead,
                    } => {
                        log(&missed(in_a_row, waiting));
                        if dead {
                            log("the watchdog loop stopped answering, stopping for a restart");
                            return true;
                        }
                    }
                    Verdict::Gone => {
                        log("the watchdog loop is not running, stopping for a restart");
                        return true;
                    }
                }
            }
        }
    }

    /// Stops the socket server, then the loop. True when every thread ended
    /// in time.
    fn finish(mut self, running: Running) -> bool {
        running.begin_stop();
        let server_stopped = match self.served.take() {
            Some(served) => {
                served.handle.stop();
                wait_join(served.thread, SERVER_STOP)
            }
            None => true,
        };
        if !server_stopped {
            log("the socket server did not stop in time");
        }
        // Only the socket this instance bound: never one it refused to
        // touch (another instance's, or something that is not a socket).
        if let Some(bound) = self.bound
            && socket_identity(&self.layout.socket) == Some(bound)
        {
            let _ = fs::remove_file(&self.layout.socket);
        }
        let loop_stopped = stop_loop(running, STOP_DEADLINE, STOP_MARGIN);
        log(if loop_stopped {
            "stopped"
        } else {
            "stopped; the watchdog loop did not finish in time (the process ends with it; its \
             last step resumes after a restart)"
        });
        server_stopped && loop_stopped
    }
}

fn wait_join(thread: JoinHandle<()>, limit: Duration) -> bool {
    let started = Instant::now();
    while !thread.is_finished() {
        if started.elapsed() >= limit {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    thread.join().is_ok()
}

fn stop_loop(running: Running, deadline: Duration, margin: Duration) -> bool {
    let (done, finished) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("wgal-stop".to_owned())
        .spawn(move || {
            let _ = done.send(running.stop(deadline));
        });
    if spawned.is_err() {
        return false;
    }
    finished.recv_timeout(deadline + margin).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    //! FIX-04 (AR-07): the health check rules and the server thread's end
    //! notice. The whole hosting needs root and a real socket folder; it is
    //! not run here.

    use super::*;

    #[test]
    fn a_gone_loop_is_dead_at_once() {
        let mut checks = Checks::new(5);
        assert_eq!(checks.record(Check::NotRunning, 5, 3), Verdict::Gone);
    }

    #[test]
    fn too_many_waiting_with_turns_is_a_busy_loop_never_a_miss() {
        let mut checks = Checks::new(0);
        for turns in 1..=100 {
            assert_eq!(
                checks.record(Check::TooManyWaiting, turns, 3),
                Verdict::Healthy
            );
        }
    }

    #[test]
    fn too_many_waiting_without_a_turn_is_a_miss_and_three_end_it() {
        let mut checks = Checks::new(7);
        let verdicts: Vec<Verdict> = [Check::TooManyWaiting, Check::Missed, Check::TooManyWaiting]
            .into_iter()
            .map(|c| checks.record(c, 7, 3))
            .collect();
        assert_eq!(
            verdicts,
            [
                Verdict::Missed {
                    in_a_row: 1,
                    waiting: true,
                    dead: false
                },
                Verdict::Missed {
                    in_a_row: 2,
                    waiting: false,
                    dead: false
                },
                Verdict::Missed {
                    in_a_row: 3,
                    waiting: true,
                    dead: true
                },
            ]
        );
    }

    #[test]
    fn an_answer_or_a_turn_ends_a_run_of_misses() {
        let mut checks = Checks::new(1);
        let miss = |in_a_row| Verdict::Missed {
            in_a_row,
            waiting: false,
            dead: false,
        };
        assert_eq!(checks.record(Check::Missed, 1, 3), miss(1));
        assert_eq!(checks.record(Check::Missed, 1, 3), miss(2));
        assert_eq!(checks.record(Check::Answered, 1, 3), Verdict::Healthy);
        assert_eq!(checks.record(Check::Missed, 1, 3), miss(1));
        assert_eq!(checks.record(Check::TooManyWaiting, 2, 3), Verdict::Healthy);
        assert_eq!(checks.record(Check::Missed, 2, 3), miss(1));
    }

    #[test]
    fn a_server_thread_reports_its_end_whether_it_returns_or_panics() {
        let (controls, inbox) = mpsc::channel();
        let mut notice = EndNotice {
            controls: controls.clone(),
            returned: None,
        };
        thread::spawn(move || notice.returned(Ok(())))
            .join()
            .unwrap();
        assert!(matches!(
            inbox.recv_timeout(Duration::from_secs(5)),
            Ok(Control::ServerEnded(ServerEnd::Returned(Ok(()))))
        ));

        let notice = EndNotice {
            controls,
            returned: None,
        };
        let panicked = thread::spawn(move || {
            let _notice = notice;
            panic!("a socket server thread that panics (test)");
        })
        .join();
        assert!(panicked.is_err());
        assert!(matches!(
            inbox.recv_timeout(Duration::from_secs(5)),
            Ok(Control::ServerEnded(ServerEnd::Panicked))
        ));
    }
}
