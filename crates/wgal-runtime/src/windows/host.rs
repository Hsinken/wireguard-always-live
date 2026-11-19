//! Hosting the watchdog on Windows: building it from the installation's
//! folder, serving its pipe, watching it and stopping it. Nothing here needs
//! the SCM, so it is tested with a temporary folder, a fake platform and a
//! test pipe; [`super::service`] adds only the dispatcher and the status
//! reports.
//!
//! # The folder
//!
//! Everything lives in `<ProgramData>\WGAL`, found through the shell, never
//! an environment variable. The installer creates it owned by
//! Administrators with a protected DACL that gives SYSTEM and
//! Administrators full control and is inherited by everything inside:
//!
//! | Path | What | Written by |
//! |---|---|---|
//! | `access.txt` | the access list ([`super::access`]) | the installer |
//! | `state\` | the state store (`FileStore`) | the service |
//! | `notify\` | `notify.json` and the DPAPI-protected bot token | the service |
//! | `events\` | the event log, `events.jsonl` and its rotations | the service |
//! | `service.log`, `service.1.log` | the service's own log ([`super::log`]) | the service |
//!
//! The service runs as LocalSystem, so DPAPI protects the token for that
//! account on this machine only.
//!
//! - If `WGAL` itself is missing, a link or junction, not a folder, or owned
//!   or changeable by anyone but SYSTEM and Administrators, nothing in it can
//!   be trusted: the service does not start, exits with [`EXIT_FOLDER`], and
//!   writes nothing into it, not even its log. The exit code (`sc.exe query
//!   WGALWatchdog`) is the only trace.
//! - A missing subfolder is created and inherits the folder's DACL. One that
//!   exists and fails the same check stops the start ([`EXIT_SUBFOLDER`]),
//!   with a line in `service.log`.
//! - A bad `access.txt` alone does not stop the service. It runs with no
//!   listed users and no tunnels (only SYSTEM and elevated administrators
//!   can connect; nobody else can register a tunnel or turn on automatic
//!   actions), keeps monitoring what is registered, and says why in
//!   `service.log`, by line number and problem, never quoting the file.
//!
//! # Running
//!
//! The pipe server's handler maps each [`PeerIdentity`] to a [`Caller`] and
//! calls the loop with a deadline of [`CALL_DEADLINE`], so the answer comes
//! back before the GUI's own 10 s deadline gives up. [`Hosted::watch`] waits
//! for a stop, forwards wake-ups and network changes, and every
//! [`Health::DEFAULT`] interval asks the loop for a snapshot as the service
//! itself: a loop whose owner thread is gone (`not_running`, or
//! `shutting_down` while the service has not begun to stop), at the first
//! such check, or one that keeps failing to answer (it hangs), ends the
//! service with [`EXIT_LOOP_DIED`], so the SCM's recovery actions restart
//! it. A refusal for too many waiting requests is a failed check only when
//! the owner finished no turn since the last one (FIX-04, AR-07).
//!
//! Network changes come from [`Hosted::watch_network_changes`], which the
//! service calls once it runs (see `super::network`): changes on interfaces
//! that are not tunnels, coalesced, each written to `service.log`. If the
//! registration fails, the service runs on and says so in `service.log`;
//! only the grace period after a network change is then missing.
//!
//! The pipe is only the GUI's way in. If it cannot be created (another
//! process holds its name, for example one that squatted it while the
//! service was down or being upgraded) or its server stops, the service does
//! not stop: the loop goes on monitoring and recovering, the GUI shows the
//! watchdog as lost (it refuses a pipe LocalSystem does not own), and the
//! service tries to serve the pipe again after [`REBIND_DELAYS`], writing
//! the first three attempts in a row and every tenth to `service.log`. A
//! server that ends is served again at once if it had answered a request;
//! servers that keep ending without answering one wait the same delays, so
//! neither a failing bind nor a failing server can spin. A server thread
//! that panics reports its end too (from a drop guard) and counts as one
//! that answered nothing.
//!
//! # Stopping
//!
//! The pipe server stops first, then the loop (`Running::stop` with
//! [`STOP_DEADLINE`], which covers the adapter's 25 s for a stop or start
//! already handed out). The whole stop is bounded on its own as well: a loop
//! whose threads do not return within [`STOP_DEADLINE`] plus
//! [`STOP_MARGIN`] is given up, and the caller then ends the process so the
//! stuck threads go with it ([`Finished::clean`]). A stop the SCM asked for
//! always ends with success, even if the loop did not stop in time (that is
//! logged): a non-zero exit would make the SCM restart a service the owner
//! just stopped. A loop that died ends with [`EXIT_LOOP_DIED`].

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_core::{PlatformAdapter, Startup, StoreError, SystemClock, Timing, Watchdog};
use wgal_platform::windows::{LOCAL_SYSTEM_SID, program_data_dir};
use wgal_protocol::{
    ErrorReason, Outcome, PROTOCOL_VERSION, Request, RequestEnvelope, RequestId, Response,
};

use super::access::{ACCESS_DIR, AccessError, AccessList, Protection, check_folder};
use super::log::ServiceLog;
use super::network::{Coalescing, NetworkWatch};
use super::server::{PeerIdentity, Server, ServerError, ServerHandle};
use crate::driver::{self, Client, Running};
use crate::events::EventLog;
use crate::ipc::{self, Caller};
use crate::notify::{DpapiSecretStore, FileSettingsStore, Notifier, Transport};
use crate::store::FileStore;

pub const STATE_DIR: &str = "state";
pub const NOTIFY_DIR: &str = "notify";
pub const EVENTS_DIR: &str = "events";

/// How long the pipe handler waits for the loop: under the GUI's 10 s for
/// the whole exchange, which also covers opening the pipe and the frames.
pub const CALL_DEADLINE: Duration = Duration::from_secs(8);
/// How long the loop gets to stop: a stop or start already handed out can
/// take the adapter's 25 s.
pub const STOP_DEADLINE: Duration = Duration::from_secs(30);
/// How much longer than [`STOP_DEADLINE`] the service waits for the loop's
/// threads before it gives up on them.
pub const STOP_MARGIN: Duration = Duration::from_secs(5);
/// How long the pipe server gets to stop; a handler may still be waiting
/// for the loop for up to [`CALL_DEADLINE`].
pub const SERVER_STOP: Duration = Duration::from_secs(10);
/// How often a stop in progress reports that it is still going.
pub const PENDING_EVERY: Duration = Duration::from_secs(3);
/// Waits before each new attempt to serve the pipe after a failure; the
/// last one repeats.
pub const REBIND_DELAYS: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// Service-specific exit codes (`sc.exe query` shows them as
/// `SERVICE_EXIT_CODE`). Every one of them makes the SCM's recovery actions
/// restart the service, when the installer has turned those on for exits
/// with an error. Problems with the pipe never stop the service (see the
/// module documentation), so none of these is about the pipe.
///
/// ProgramData or the `WGAL` folder is missing or cannot be trusted.
pub const EXIT_FOLDER: u32 = 1;
/// A subfolder of `WGAL` cannot be created or trusted.
pub const EXIT_SUBFOLDER: u32 = 2;
/// The state store cannot be opened, for example because another watchdog
/// holds its lock.
pub const EXIT_STORE: u32 = 3;
/// The loop's threads could not start.
pub const EXIT_LOOP_START: u32 = 4;
/// The loop stopped answering while the service was running.
pub const EXIT_LOOP_DIED: u32 = 11;

/// Where the installation keeps its files.
#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// `<ProgramData>\WGAL`.
    pub fn installation() -> Result<Self, StartError> {
        program_data_dir()
            .map(|dir| Self {
                root: dir.join(ACCESS_DIR),
            })
            .map_err(|e| StartError::ProgramData(e.code))
    }

    #[cfg(test)]
    pub(crate) fn at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sub(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

/// Why the service did not start. The description never names a path
/// outside the installation, a SID or anything from a file.
#[derive(Debug)]
pub enum StartError {
    ProgramData(u32),
    Folder(AccessError),
    Subfolder {
        name: &'static str,
        error: AccessError,
    },
    Store(StoreError),
    Loop(io::Error),
}

impl StartError {
    pub fn exit_code(&self) -> u32 {
        match self {
            Self::ProgramData(_) | Self::Folder(_) => EXIT_FOLDER,
            Self::Subfolder { .. } => EXIT_SUBFOLDER,
            Self::Store(_) => EXIT_STORE,
            Self::Loop(_) => EXIT_LOOP_START,
        }
    }
}

/// What is wrong with a folder, in words that fit "the folder ... ".
fn folder_problem(error: &AccessError) -> String {
    match error {
        AccessError::Missing => "does not exist".to_owned(),
        AccessError::ReparsePoint => "is a link or junction".to_owned(),
        AccessError::WrongKind => "is not a folder".to_owned(),
        AccessError::NotProtected => {
            "is owned or changeable by someone other than SYSTEM and Administrators".to_owned()
        }
        AccessError::Security(code) => {
            format!("has security that could not be read (Windows error {code})")
        }
        AccessError::Io(e) => format!("could not be opened: {e}"),
        other => format!("could not be used: {other}"),
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProgramData(code) => {
                write!(
                    f,
                    "the ProgramData folder was not found (Windows error {code})"
                )
            }
            Self::Folder(e) => write!(f, "the folder {ACCESS_DIR} {}", folder_problem(e)),
            Self::Subfolder { name, error } => {
                write!(
                    f,
                    "the folder {ACCESS_DIR}\\{name} {}",
                    folder_problem(error)
                )
            }
            Self::Store(e) => write!(f, "the state store could not be opened: {e}"),
            Self::Loop(e) => write!(f, "the watchdog loop could not start: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// What the SCM's control handler and the pipe server tell the service.
#[derive(Debug)]
pub enum Control {
    /// Stop, shutdown or preshutdown.
    Stop,
    /// The machine resumed from sleep.
    Wake,
    /// The host's network changed (coalesced; never a tunnel's own change).
    NetworkChange,
    /// The pipe server's thread ended.
    ServerEnded(ServerEnd),
}

/// How the pipe server's thread ended.
#[derive(Debug)]
pub enum ServerEnd {
    /// `run` returned.
    Returned(Result<(), ServerError>),
    /// It unwound from a panic before `run` returned (AR-07).
    Panicked,
}

/// Sends [`Control::ServerEnded`] when the pipe server's thread ends, also
/// when it unwinds from a panic, so the service always cleans the server
/// up and serves the pipe again (AR-07).
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
        // The service may already be gone.
        let _ = self.controls.send(Control::ServerEnded(end));
    }
}

/// The caller the loop sees for a pipe peer. `allowed` is the server's own
/// admission rule (SYSTEM, an elevated administrator or a listed user, as
/// `ipc::Caller` defines it), so every peer that reaches the handler is
/// allowed; `elevated` is SYSTEM or an elevated administrator.
pub fn caller(peer: &PeerIdentity) -> Caller {
    Caller {
        account: peer.account.clone(),
        allowed: peer.admitted(),
        elevated: peer.local_system || peer.elevated_admin,
    }
}

/// The service itself, for its health checks.
fn service_caller() -> Caller {
    Caller {
        account: LOCAL_SYSTEM_SID.to_owned(),
        allowed: true,
        elevated: true,
    }
}

/// The Telegram transport, or one that never sends in a build without it.
#[cfg(feature = "telegram")]
pub fn transport() -> Box<dyn Transport + Send> {
    Box::new(crate::notify::HttpTransport::new())
}

#[cfg(not(feature = "telegram"))]
pub fn transport() -> Box<dyn Transport + Send> {
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

/// Checks the folder and each subfolder, creating the missing ones.
fn prepare(layout: &Layout, protection: Protection) -> Result<(), StartError> {
    for name in [STATE_DIR, NOTIFY_DIR, EVENTS_DIR] {
        let path = layout.sub(name);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(StartError::Subfolder {
                    name,
                    error: AccessError::Io(e),
                });
            }
        }
        check_folder(&path, protection).map_err(|error| StartError::Subfolder { name, error })?;
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

/// How the service watches its loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    /// Time between checks.
    pub every: Duration,
    /// Failed checks in a row that end the service.
    pub failures: u32,
    /// How long one check waits for the loop's answer.
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

/// The times the hosting works with. Only tests change them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) health: Health,
    pub(crate) rebind: [Duration; 5],
    pub(crate) server_stop: Duration,
    pub(crate) loop_stop: Duration,
    pub(crate) loop_margin: Duration,
}

impl Limits {
    pub(crate) const DEFAULT: Self = Self {
        health: Health::DEFAULT,
        rebind: REBIND_DELAYS,
        server_stop: SERVER_STOP,
        loop_stop: STOP_DEADLINE,
        loop_margin: STOP_MARGIN,
    };
}

/// Creates the pipe server for an access list: [`Server::watchdog`] in the
/// service, a test pipe in tests. Called again for every new attempt.
pub(crate) type Binder = Box<dyn FnMut(AccessList) -> Result<Server, ServerError>>;

/// A pipe server running on its own thread.
#[derive(Debug)]
struct Pipe {
    handle: ServerHandle,
    thread: JoinHandle<()>,
}

/// The parts the service is made of, running.
pub struct Hosted {
    client: Client,
    /// `None` only once stopping has begun (or in tests that stop it early).
    running: Option<Running>,
    /// `None` while the pipe could not be served.
    pipe: Option<Pipe>,
    binder: Binder,
    access: AccessList,
    /// Where the pipe server reports that it ended.
    controls: Sender<Control>,
    /// Failed attempts to serve the pipe since it was last served.
    bind_failures: usize,
    /// Pipe servers in a row that ended without answering a request. A
    /// successful bind does not reset it, only a server that answered, so a
    /// server that fails right after every bind backs off too.
    server_ends: usize,
    /// When to try serving the pipe again, while it is not served.
    next_bind: Option<Instant>,
    /// The interface notices, once registered.
    network: Option<NetworkWatch>,
    log: ServiceLog,
    limits: Limits,
    /// Tests only: the next pipe server's thread panics instead of serving.
    #[cfg(test)]
    panic_next_server: bool,
}

impl fmt::Debug for Hosted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hosted")
            .field("running", &self.running.is_some())
            .field("pipe", &self.pipe.is_some())
            .field("network", &self.network.is_some())
            .field("bind_failures", &self.bind_failures)
            .field("server_ends", &self.server_ends)
            .finish_non_exhaustive()
    }
}

/// How [`Hosted::finish`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finished {
    /// The service-specific exit code to report, or `None` for success.
    pub exit_code: Option<u32>,
    /// Every thread of the pipe server and the loop ended in time. When
    /// false, threads are still stuck; the caller ends the process once it
    /// has reported the service stopped, so they go with it.
    pub clean: bool,
}

/// Builds and starts the watchdog from `layout` with `adapter`, then serves
/// the pipe through `bind`. The pipe server reports its end on `controls`.
/// A pipe that cannot be served does not fail the start (see the module
/// documentation). On failure everything already started is stopped again,
/// and the reason is in `service.log` unless the folder itself failed. The
/// service passes [`Limits::DEFAULT`].
pub(crate) fn start<A, B>(
    layout: &Layout,
    protection: Protection,
    adapter: A,
    transport: Box<dyn Transport + Send>,
    bind: B,
    controls: Sender<Control>,
    limits: Limits,
) -> Result<Hosted, StartError>
where
    A: PlatformAdapter + 'static,
    B: FnMut(AccessList) -> Result<Server, ServerError> + 'static,
{
    // Nothing is written into a folder that fails this.
    check_folder(layout.root(), protection).map_err(StartError::Folder)?;
    let mut log = ServiceLog::new(layout.root());
    log.write("starting");
    let (client, running, access) = match build(layout, protection, adapter, transport, &mut log) {
        Ok(built) => built,
        Err(error) => {
            log.write(&format!("not started: {error}"));
            return Err(error);
        }
    };
    let mut hosted = Hosted {
        client,
        running: Some(running),
        pipe: None,
        binder: Box::new(bind),
        access,
        controls,
        bind_failures: 0,
        server_ends: 0,
        next_bind: None,
        network: None,
        log,
        limits,
        #[cfg(test)]
        panic_next_server: false,
    };
    hosted.serve_pipe();
    hosted.log.write("running");
    Ok(hosted)
}

/// Builds the watchdog and starts its loop.
fn build<A>(
    layout: &Layout,
    protection: Protection,
    adapter: A,
    transport: Box<dyn Transport + Send>,
    log: &mut ServiceLog,
) -> Result<(Client, Running, AccessList), StartError>
where
    A: PlatformAdapter + 'static,
{
    prepare(layout, protection)?;
    let access = match AccessList::load_dir(layout.root(), protection) {
        Ok(list) => {
            log.write(&format!(
                "access list: {} listed users, {} tunnels",
                list.users().len(),
                list.tunnels().len()
            ));
            list
        }
        Err(error) => {
            log.write(&format!(
                "access list refused, so no users are listed and no tunnels allowed: {error}"
            ));
            AccessList::default()
        }
    };
    let (store, startup, _) = FileStore::open(&layout.sub(STATE_DIR)).map_err(StartError::Store)?;
    log.write(startup_note(&startup));
    let watchdog = Watchdog::new(
        adapter,
        SystemClock::new(),
        Timing::default(),
        Box::new(store),
        startup,
    );
    let notify = layout.sub(NOTIFY_DIR);
    let notifier = Notifier::new(
        FileSettingsStore::new(&notify),
        DpapiSecretStore::new(&notify),
    );
    let events = match EventLog::open(&layout.sub(EVENTS_DIR)) {
        Ok(events) => Some(events),
        Err(e) => {
            log.write(&format!(
                "the event log is unavailable, monitoring goes on without it: {}",
                e.kind()
            ));
            None
        }
    };
    let tunnels = ipc::AccessList {
        tunnels: access.tunnels().clone(),
    };
    let (client, running) =
        driver::spawn(watchdog, notifier, transport, tunnels, events).map_err(StartError::Loop)?;
    Ok((client, running, access))
}

/// Why [`Hosted::watch`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The SCM asked the service to stop (stop, shutdown or preshutdown).
    Requested,
    /// The loop is gone, or failed [`Health::failures`] checks in a row.
    LoopDied,
}

impl Ending {
    /// The service-specific exit code, or `None` for success.
    pub fn exit_code(self) -> Option<u32> {
        match self {
            Self::Requested => None,
            Self::LoopDied => Some(EXIT_LOOP_DIED),
        }
    }
}

/// What one health check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Check {
    /// The loop answered with a snapshot.
    Answered,
    /// The loop's owner thread is gone (`not_running`), or the loop is
    /// stopping on its own (`shutting_down`: the service starts no stop of
    /// its own before [`Hosted::watch`] returns).
    NotRunning,
    /// Refused unasked: too many requests wait (`too_many_waiting`).
    TooManyWaiting,
    /// No snapshot for any other reason (no answer in time, shutting down).
    Missed,
}

/// Asks the loop for a snapshot as the service itself, within `deadline`.
fn check(client: &Client, deadline: Duration) -> Check {
    let Ok(request_id) = RequestId::new("wgal-service-health") else {
        return Check::Answered;
    };
    let request = RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id,
        request: Request::GetSnapshot,
    };
    match client.call(&service_caller(), request, deadline).outcome {
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
pub(crate) enum Verdict {
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
/// (FIX-04, AR-07). A loop whose owner thread is gone is dead at once. A
/// refusal for too many waiting requests is a miss only when the owner
/// finished no turn since the last check: requests that timed out keep
/// their places until the owner answers them, so a hung owner is refused
/// this way at every check, while a busy one keeps turning.
#[derive(Debug)]
pub(crate) struct Checks {
    misses: u32,
    turns: u64,
}

impl Checks {
    pub(crate) fn new(turns: u64) -> Self {
        Self { misses: 0, turns }
    }

    pub(crate) fn record(&mut self, check: Check, turns: u64, failures: u32) -> Verdict {
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

impl Hosted {
    /// Serves until the SCM asks to stop or the loop stops answering.
    /// Wake-ups go to the loop; a pipe that is not served is tried again.
    pub fn watch(&mut self, controls: &Receiver<Control>) -> Ending {
        let mut checks = Checks::new(self.turns());
        let mut next_check = Instant::now() + self.limits.health.every;
        loop {
            let until = self.next_bind.map_or(next_check, |at| at.min(next_check));
            match controls.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Control::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    return Ending::Requested;
                }
                Ok(Control::Wake) => {
                    self.log.write("resumed from sleep");
                    self.client.on_wake();
                }
                Ok(Control::NetworkChange) => {
                    self.log
                        .write("the network changed; the grace period starts again");
                    self.client.on_network_change();
                }
                Ok(Control::ServerEnded(end)) => self.pipe_ended(end),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if self.next_bind.is_some_and(|at| now >= at) {
                self.serve_pipe();
            }
            if now >= next_check {
                next_check = Instant::now() + self.limits.health.every;
                let found = check(&self.client, self.limits.health.deadline);
                match checks.record(found, self.turns(), self.limits.health.failures) {
                    Verdict::Healthy => {}
                    Verdict::Missed {
                        in_a_row,
                        waiting,
                        dead,
                    } => {
                        self.log.write(&missed(in_a_row, waiting));
                        if dead {
                            return Ending::LoopDied;
                        }
                    }
                    Verdict::Gone => {
                        self.log.write("the watchdog loop is not running");
                        return Ending::LoopDied;
                    }
                }
            }
        }
    }

    /// The owner thread's turns so far (0 once the loop is gone).
    fn turns(&self) -> u64 {
        self.running.as_ref().map_or(0, |r| r.stats().turns)
    }

    /// Registers for the host's interface notices and forwards network
    /// changes, coalesced, as [`Control::NetworkChange`]. A failure is
    /// written to `service.log`; the service runs on without them.
    pub fn watch_network_changes(&mut self) {
        match NetworkWatch::start(self.controls.clone(), Coalescing::DEFAULT) {
            Ok(watch) => {
                self.network = Some(watch);
                self.log.write("watching network changes");
            }
            Err(error) => self.log.write(&format!(
                "network changes are not watched (a network change does not start the grace \
                 period again): {error}"
            )),
        }
    }

    /// Creates the pipe and starts its server. On failure the loop goes on
    /// without it and another attempt is scheduled.
    fn serve_pipe(&mut self) {
        self.next_bind = None;
        let failure = match (self.binder)(self.access.clone()) {
            Ok(server) => {
                let handle = server.handle();
                let handler = self.client.clone();
                let controls = self.controls.clone();
                #[cfg(test)]
                let panics = std::mem::take(&mut self.panic_next_server);
                let spawned = thread::Builder::new()
                    .name("wgal-pipe-server".to_owned())
                    .spawn(move || {
                        // Made in the thread, so a thread that never
                        // started reports nothing (Codex review, P3-02).
                        let mut notice = EndNotice {
                            controls,
                            returned: None,
                        };
                        #[cfg(test)]
                        if panics {
                            drop(server);
                            drop(handler);
                            panic!("a pipe server thread that panics (test)");
                        }
                        notice.returned(server.run(move |peer, envelope| {
                            handler.call(&caller(peer), envelope, CALL_DEADLINE)
                        }));
                        // `notice` reports the end when it is dropped here,
                        // or while unwinding.
                    });
                match spawned {
                    Ok(thread) => {
                        self.pipe = Some(Pipe { handle, thread });
                        if self.bind_failures > 0 {
                            self.log.write(&format!(
                                "the pipe is served again after {} failed attempts",
                                self.bind_failures
                            ));
                        }
                        self.bind_failures = 0;
                        return;
                    }
                    // The server went down with the closure.
                    Err(e) => format!("its thread could not start: {e}"),
                }
            }
            Err(error) => error.to_string(),
        };
        self.bind_failures += 1;
        let attempt = self.bind_failures;
        let delay = self.rebind_delay(attempt);
        self.next_bind = Some(Instant::now() + delay);
        if logged(attempt) {
            self.log.write(&format!(
                "the pipe could not be served (attempt {attempt}): {failure}; the watchdog \
                 goes on without it and tries again in {}",
                seconds(delay)
            ));
        }
    }

    /// The wait before the `n`th new attempt in a row (1-based); the last
    /// delay repeats.
    fn rebind_delay(&self, n: usize) -> Duration {
        let delays = &self.limits.rebind;
        delays[n.saturating_sub(1).min(delays.len() - 1)]
    }

    /// The pipe server ended on its own: serve the pipe again, at once if
    /// it had answered a request, otherwise after the same delays as a
    /// failed bind, so a server that keeps ending cannot spin. A server
    /// whose thread panicked is stopped first, which also ends the
    /// connections it handed out (their reads and writes fail; their
    /// threads were left running by the panic), and counts as one that
    /// answered nothing.
    fn pipe_ended(&mut self, end: ServerEnd) {
        let panicked = matches!(end, ServerEnd::Panicked);
        let answered = match self.pipe.take() {
            Some(pipe) => {
                if panicked {
                    pipe.handle.stop();
                }
                let answered = !panicked && pipe.handle.stats().requests > 0;
                // It has ended (its notice is sent last), so this hardly
                // waits; a panic is in the join's result, ignored.
                let _ = pipe.thread.join();
                answered
            }
            None => false,
        };
        self.server_ends = if answered { 0 } else { self.server_ends + 1 };
        let ends = self.server_ends;
        let delay = if ends == 0 {
            Duration::ZERO
        } else {
            self.rebind_delay(ends)
        };
        self.next_bind = Some(Instant::now() + delay);
        if ends == 0 || logged(ends) {
            let what = match end {
                ServerEnd::Returned(Ok(())) => "the pipe server stopped".to_owned(),
                ServerEnd::Returned(Err(error)) => format!("the pipe server failed: {error}"),
                ServerEnd::Panicked => "the pipe server's thread panicked".to_owned(),
            };
            self.log.write(&format!(
                "{what} ({ends} in a row without answering a request); serving the pipe again \
                 in {}",
                seconds(delay)
            ));
        }
    }

    /// Winds the loop down at once (`Running::begin_stop`), then stops the
    /// pipe server, then the loop, calling `pending` at least every
    /// [`PENDING_EVERY`] meanwhile. Never takes longer than the server's
    /// stop limit plus the loop's deadline and margin.
    pub fn finish(mut self, ending: Ending, pending: &mut dyn FnMut()) -> Finished {
        self.log.write(match ending {
            Ending::Requested => "stopping as asked",
            Ending::LoopDied => "the watchdog loop stopped answering, stopping for a restart",
        });
        // First, so nothing new and automatic starts while the pipe server
        // below is being stopped; a stop already sent still gets its start.
        if let Some(running) = &self.running {
            running.begin_stop();
        }
        // Cancels the registration; nothing reads the notices any more.
        drop(self.network.take());
        let server_stopped = match self.pipe.take() {
            Some(pipe) => {
                pipe.handle.stop();
                wait_join(pipe.thread, self.limits.server_stop, pending)
            }
            None => true,
        };
        if !server_stopped {
            self.log.write("the pipe server did not stop in time");
        }
        let loop_stopped = match self.running.take() {
            Some(running) => stop_loop(
                running,
                self.limits.loop_stop,
                self.limits.loop_margin,
                pending,
            ),
            // Already stopped (only in tests).
            None => true,
        };
        self.log.write(if loop_stopped {
            "stopped"
        } else {
            "stopped; the watchdog loop did not finish in time (the process ends with it; its \
             last step resumes after a restart)"
        });
        Finished {
            exit_code: ending.exit_code(),
            clean: server_stopped && loop_stopped,
        }
    }

    #[cfg(test)]
    pub(crate) fn client(&self) -> Client {
        self.client.clone()
    }

    /// Stops the loop behind the service's back, as if it had died.
    #[cfg(test)]
    pub(crate) fn kill_loop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop(STOP_DEADLINE);
        }
    }

    /// The next pipe server's thread panics instead of serving.
    #[cfg(test)]
    pub(crate) fn panic_next_server(&mut self) {
        self.panic_next_server = true;
    }

    #[cfg(test)]
    pub(crate) fn server(&self) -> Option<&ServerHandle> {
        self.pipe.as_ref().map(|pipe| &pipe.handle)
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

/// Which of a run of repeated failures go to `service.log`: the first
/// three, then every tenth.
fn logged(n: usize) -> bool {
    n <= 3 || n.is_multiple_of(10)
}

/// A delay for the log: whole seconds, or milliseconds below one second.
fn seconds(delay: Duration) -> String {
    if delay.as_secs() == 0 && !delay.is_zero() {
        format!("{} ms", delay.as_millis())
    } else {
        format!("{} s", delay.as_secs())
    }
}

/// Joins `thread` if it finishes within `limit`.
fn wait_join(thread: JoinHandle<()>, limit: Duration, pending: &mut dyn FnMut()) -> bool {
    let started = Instant::now();
    let mut reported = started;
    while !thread.is_finished() {
        if started.elapsed() >= limit {
            return false;
        }
        if reported.elapsed() >= PENDING_EVERY {
            pending();
            reported = Instant::now();
        }
        thread::sleep(Duration::from_millis(20));
    }
    thread.join().is_ok()
}

/// `Running::stop(deadline)` on a thread of its own, so the stop can be
/// reported as still going, and given up `margin` after `deadline` even if
/// `Running::stop` itself does not return: the thread is then left to the
/// process's end.
fn stop_loop(
    running: Running,
    deadline: Duration,
    margin: Duration,
    pending: &mut dyn FnMut(),
) -> bool {
    let (done, finished) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("wgal-service-stop".to_owned())
        .spawn(move || {
            let _ = done.send(running.stop(deadline));
        });
    if spawned.is_err() {
        // `running` went down with the closure: dropping it lets the loop
        // wind down on its own.
        return false;
    }
    let give_up = Instant::now() + deadline + margin;
    loop {
        let left = give_up.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match finished.recv_timeout(left.min(PENDING_EVERY)) {
            Ok(in_time) => return in_time,
            Err(RecvTimeoutError::Timeout) => pending(),
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

#[cfg(test)]
mod tests;
