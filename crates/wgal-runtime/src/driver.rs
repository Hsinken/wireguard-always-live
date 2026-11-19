//! The watchdog loop: the part of the runtime that drives the core for real
//! (plan §3.2, §5.3, §6, §7). The platform's service constructs the parts,
//! calls [`spawn`], serves its IPC endpoint through [`Client::call`] and
//! stops the loop with [`Running::stop`].
//!
//! Threads:
//!
//! - One owner thread holds the [`Watchdog`] and the [`Notifier`]. It plans,
//!   applies results, answers requests and writes the event log, and never
//!   runs planned platform work itself. (Discovery and registration still
//!   call the adapter from `Watchdog::handle`, bounded by the adapter's own
//!   deadlines.)
//! - Worker threads run the items from `Watchdog::plan`, at most the
//!   *effective* count (`WorkerStatus::effective`, owner's request 6 of
//!   2026-11-03): the user's choice (`HostSettings::workers`) or else
//!   [`suggested_workers`], within [`MIN_WORKERS`] and the *limit*, one more
//!   than the tunnels being monitored (a paused tunnel does not count), and
//!   within [`Options::workers`], a cap for tests. A thread is started only
//!   when an item is ready and no started one is idle (owner's request 7:
//!   nothing is reserved up front); when the effective count drops (the
//!   setting lowered, a tunnel paused or removed), the workers above it exit
//!   once idle, after their current item. Rounds never take the last worker
//!   of the effective count, which is kept for the stop, start or poll of
//!   the tunnel holding the host's control slot, so a restart never waits
//!   behind probes; while more rounds are out than a lowered count allows,
//!   slot work may still start one worker more than the rounds out. Each
//!   worker has a [`WORKER_STACK_BYTES`] stack.
//! - One thread sends notifications: [`Job::run`] blocks on HTTP, so it runs
//!   there, one at a time, and its [`Delivery`] goes back to the owner thread
//!   for `Notifier::finish`. Jobs are never formatted or logged.
//!
//! So the loop has at most `MAX_WORKERS` + 2 threads, whatever the load
//! (plan T28), and with nothing monitored only the owner and notification
//! threads, plus the idle workers the effective count still allows.
//!
//! Host settings: the owner thread answers `UpdateHostSettings` itself. A
//! time zone its database does not know is refused (`invalid_setting`);
//! otherwise the settings are stored through the core first (a failure is
//! `storage_unavailable` and changes nothing), then applied: the worker
//! count from the next turn, the zone at once. No zone means the system's,
//! read again every [`ZONE_REFRESH`] so a changed system zone is picked up
//! without a restart; it is UTC if the system's cannot be read. The zone is
//! applied in one place, `Engine::apply_time_zone`. Snapshots get the host
//! status ([`Engine::host_status`]).
//!
//! The event log ([`crate::events`]) gets, after every applied result,
//! delivery and accepted request and at the end of every turn, the core's
//! events (`Watchdog::take_events`) and then what changed between two
//! snapshots; accepted user operations add their own line first. The owner
//! thread also answers `ReadEventLog` from it.
//!
//! The control rule (see `Watchdog::plan`): from the moment a stop or start
//! of a tunnel is planned until its result is applied, requests naming that
//! tunnel wait on the owner thread, in order; requests for other tunnels and
//! for the whole host (snapshots among them) are answered meanwhile. Each
//! item passes `Watchdog::still_due` on the owner thread when, and only
//! when, a worker is free to start it at once.
//!
//! Wake-ups: the owner thread sleeps until a request, a result, a delivery,
//! a wake or network notice, or `Watchdog::next_due` (the next round, poll,
//! retry or deadline), and at most [`Options::max_wait`] (5 s by default), so
//! the notification service's own retry times are noticed too. After every
//! wake-up it plans once. With nothing due it does not wake: an idle
//! watchdog with one tunnel wakes for its round every 15 s and at the
//! `max_wait` ticks in between.
//!
//! Requests: the protocol version is checked, the caller authorized
//! ([`crate::ipc::authorize`]), then the owner thread answers the host
//! settings and the event log, `Notifier::handle` the notification requests
//! and `Watchdog::handle` the rest, and `Notifier::decorate` and the host
//! status fill the snapshot's notification and host parts. A request
//! that reached the owner thread is applied even if its caller's deadline
//! passed meanwhile, for example while it waited for a tunnel's stop: the
//! caller just gets `timeout`, which therefore means "outcome unknown", never
//! "not done". Only a request still waiting when the loop starts shutting
//! down is answered `busy` and dropped, unless it is a disconnect (below).
//!
//! Winding down ([`Running::begin_stop`]), which a service calls as soon as
//! it is asked to stop: no automatic recovery or start after an external
//! stop is queued any more, queued ones that sent nothing are dropped, and no
//! new notification is sent. Requests are still answered and monitoring
//! goes on; a stop already sent is still followed by its start.
//!
//! Shutdown ([`Running::stop`]) does that too, and more: new calls get
//! `busy`, waiting requests get `busy` except a user's disconnect, and no
//! round or new operation is started. A disconnect, waiting or arriving, is
//! still applied, and its own stop still goes out: it records that the
//! tunnel stays down before anything is sent and only moves the tunnel
//! towards what the user asked (plan §6.4); it also keeps the start of a
//! recovery the user overrode from going out. The loop waits, up to the
//! deadline, until each such stop is sent. Any other
//! request either adds work the shutdown would not carry out or changes
//! settings better changed by a running watchdog, so `busy` tells the caller
//! it was not applied.
//!
//! Only what finishes a stop already handed out still runs: its polls and
//! the start that follows it, so WGAL does not leave a tunnel down that it
//! stopped (the core's rule when storage fails, too). The owner thread waits
//! up to the deadline for the items and the notification already handed out
//! (a stop or start can take the adapter's 25 s), and for every stop of a
//! recovery or reconnect to settle and its start to be sent: a stop the
//! manager is still carrying out (a service in STOP_PENDING) is polled every
//! second until then, not taken for "nothing left to do". Results are
//! applied, which saves them; disconnects still waiting are applied; the
//! state is saved once more if a save had failed, the event log gets
//! `stopped` (with `in_time`) and `watchdog_stopped`, and the threads are
//! joined. It finished in time only if all of
//! that happened before the deadline: a start still owed, whether the
//! deadline cut it off or the core gave the stop up while the tunnel may be
//! down, makes it `false`. A stop that owes nothing any more (a disconnect
//! took it over, the binding is lost, or the tunnel is up after all) does
//! not. Work planned but never handed out reached nothing; whatever was under
//! way is in the stored state already (an unfinished recovery or user
//! operation, a reserved notification), and the core resumes it after a
//! restart.
//!
//! [`Engine`] is the owner thread's part on its own: it takes inputs and
//! returns the work to run, so tests drive it step by step with a fake clock
//! and decide themselves when each item runs and finishes. On its own it
//! only counts the workers it starts and retires; [`spawn_with`] gives it
//! the real threads.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use wgal_core::{
    AdapterError, Clock, Executor, MonoInstant, PlatformAdapter, Watchdog, WorkItem, WorkKind,
    WorkResult,
};
use wgal_protocol::{
    AdapterErrorKind, AutoRecoveryImport, BoundedVec, Capability, ChatId, CredentialState,
    DeviceName, DiscoveredTunnel, DisplayName, ErrorCode, ErrorReason, EventFilter, HostSettings,
    HostSettingsEntry, HostStatus, ImportItem, ImportMatch, ImportOptions, ImportOutcome,
    ImportResult, LogEvent, MonitorSettings, NotificationStatus, Origin, Outcome, PROTOCOL_VERSION,
    ProbeTiming, ProductVersion, ProtocolError, Request, RequestEnvelope, RequestId, Response,
    ResponseEnvelope, SETTINGS_DOCUMENT_VERSION, SettingsDocument, SettingsFormat, StorageHealth,
    TimeZoneName, TimeZoneStatus, TunnelId, TunnelSettingsEntry, WorkerStatus,
};

use crate::events::{Diag, Entry, EventLog, Lines, Tracker, TunnelLabel};
use crate::ipc::{AccessList, Caller, NOT_ALLOWED, authorize, named_tunnel};
use crate::notify::{Delivery, Job, Notifier, SecretStore, SettingsStore, Transport};

/// Fewest worker threads: one for rounds, one kept for stops and starts.
pub const MIN_WORKERS: usize = wgal_protocol::MIN_WORKERS as usize;
/// Most worker threads: one round worker for each of the most tunnels a
/// snapshot can carry, and the one kept for the control slot.
pub const MAX_WORKERS: usize = wgal_protocol::MAX_WORKERS as usize;
/// Stack of each worker thread. A worker runs the core's code for one item
/// and the adapter's checks around its platform calls; the calls that may
/// need more (Win32 on Windows, which the adapter runs on threads of its
/// own; ping and other tools, which run as child processes read by threads
/// of their own) do not run on it. Its deepest own use is the bounded tool
/// runner's 4 KiB read buffer. A quarter of Rust's 2 MiB default leaves
/// room many times over (see "Threads" in `docs/architecture.md`).
pub const WORKER_STACK_BYTES: usize = 512 * 1024;
/// How often the time zone is resolved again, so a changed system zone (or
/// a configured one the database lacked) is picked up without a restart.
pub const ZONE_REFRESH: Duration = Duration::from_secs(10 * 60);
/// What a probe that runs into its timeout may take beyond it, for the
/// suggestion: the platform's own deadline (Ubuntu's and macOS's ping exit
/// up to a second after the wait) and the route checks around the probe.
const PROBE_OVERHEAD_SECS: u64 = 1;
/// Largest UTC offset reported, as the contract promises (±14 hours).
const MAX_OFFSET_SECS: i32 = 14 * 3600;
/// Messages the owner thread takes in before it plans again.
const MAX_BATCH: usize = 64;

/// The worker count the watchdog suggests (`WorkerStatus::suggested`) for
/// `load`, each monitored tunnel's probe timing and how many targets one of
/// its rounds probes now (`Watchdog::monitoring_load`), within
/// [`MIN_WORKERS`]..=`limit`.
///
/// It is the fewest round workers k that keep every round on time when
/// every probe of every target runs into its timeout, plus one for the
/// control slot. Each tunnel has at most one round out and rounds are handed
/// out in the order they became due, so a round waits at most
/// W(k) = ⌈(m − k) / k⌉ × D for a worker, where m is the number of tunnels
/// whose rounds probe something and D the longest such round (each target
/// its timeout plus [`PROBE_OVERHEAD_SECS`]). On time means W(k) is within
/// every tunnel's slack: its probe timing's freshness (interval + timeout +
/// 18 s) minus its interval and minus what its own other targets take after
/// the recovery target, so a tunnel whose recovery target answers never
/// loses its green light for want of a worker. If even one round of another
/// tunnel is too long for that, every such tunnel gets its own worker. The
/// watchdog's freshness for a tunnel with observe targets is longer by what
/// they take (Codex review of wave three, P2-5); that allowance covers the
/// round's own span, so the suggestion does not spend it on queueing and
/// stays as it was, on the safe side. Worked examples in "Worker count
/// and the suggested value" in `docs/architecture.md`.
pub fn suggested_workers(load: &[(ProbeTiming, usize)], limit: usize) -> usize {
    let limit = limit.max(MIN_WORKERS);
    // A round that probes nothing only reads the OS state: no timeouts.
    let rounds: Vec<(u64, u64)> = load
        .iter()
        .filter(|(_, targets)| *targets > 0)
        .map(|(timing, targets)| {
            let probe = u64::from(timing.timeout_secs()) + PROBE_OVERHEAD_SECS;
            let targets = u64::try_from(*targets).unwrap_or(u64::MAX);
            let longest = targets.saturating_mul(probe);
            let slack = u64::from(timing.freshness_secs())
                .saturating_sub(u64::from(timing.interval_secs()))
                .saturating_sub((targets - 1).saturating_mul(probe));
            (longest, slack)
        })
        .collect();
    let m = rounds.len();
    let longest = rounds.iter().map(|r| r.0).max().unwrap_or(0);
    let slack = rounds.iter().map(|r| r.1).min().unwrap_or(0);
    let wait = |k: usize| {
        u64::try_from((m - k).div_ceil(k))
            .unwrap_or(u64::MAX)
            .saturating_mul(longest)
    };
    let round_workers = (1..m).find(|&k| wait(k) <= slack).unwrap_or(m);
    round_workers.saturating_add(1).clamp(MIN_WORKERS, limit)
}

/// The zone `name` stands for in the time zone database, if it knows it.
fn known_zone(name: &TimeZoneName) -> Option<TimeZone> {
    jiff::tz::db()
        .get(name.as_str())
        .ok()
        .filter(|zone| !zone.is_unknown())
}

/// The OS's time zone, if it can be read, as it is now (owner decisions of
/// 2026-11-19: a changed system zone takes effect at once on Linux and
/// macOS; on Windows within five minutes). jiff keeps the zone
/// `TimeZone::try_system` found for five minutes, so on Linux and macOS
/// every zone that lives in a file is read here instead, each time: with no
/// `TZ`, `/etc/localtime`; with a `TZ` that names a file ([`tz_file`]),
/// that file. Windows, which has no such file, goes through jiff; so does a
/// `TZ` that names a zone of the database or is a POSIX rule, which the
/// process's environment fixes for its whole life, and a file that cannot
/// be read or holds no zone.
fn system_time_zone() -> Option<TimeZone> {
    #[cfg(unix)]
    {
        let path = match std::env::var_os("TZ") {
            None => Some(std::path::PathBuf::from(LOCALTIME)),
            Some(tz) => tz.to_str().and_then(tz_file).map(std::path::PathBuf::from),
        };
        if let Some(zone) = path.and_then(|path| zone_of_file(&path)) {
            return Some(zone);
        }
    }
    TimeZone::try_system().ok()
}

/// Where Linux and macOS keep the system's zone.
#[cfg(unix)]
const LOCALTIME: &str = "/etc/localtime";

/// The file a `TZ` names, as jiff reads `TZ`, when it names one: not when
/// it is empty (UTC), a POSIX rule, a zone of the database, or a path whose
/// part after its last `zoneinfo/` is one (those are fixed for the life of
/// the process). A leading `:` is dropped. `TZ` is the watchdog's own
/// environment, set by whoever starts the service, never a caller's; jiff
/// reads the same file.
#[cfg(any(unix, test))]
fn tz_file(tz: &str) -> Option<&str> {
    let tz = tz.strip_prefix(':').unwrap_or(tz);
    let fixed = tz.is_empty()
        || TimeZone::posix(tz).is_ok()
        || jiff::tz::db().get(tz).is_ok()
        || tz
            .rsplit_once("zoneinfo/")
            .is_some_and(|(_, name)| jiff::tz::db().get(name).is_ok());
    (!fixed).then_some(tz)
}

/// The zone the file at `path` gives: [`zone_of_localtime`] with the link
/// it is, if it is one, and its contents, read with [`read_zone_file`].
#[cfg(unix)]
fn zone_of_file(path: &std::path::Path) -> Option<TimeZone> {
    zone_of_localtime(std::fs::read_link(path).ok().as_deref(), || {
        read_zone_file(path)
    })
}

/// The most of a zone file read: TZif files are a few KiB; a larger one is
/// not read at all.
#[cfg(any(unix, test))]
const MAX_ZONE_FILE_BYTES: u64 = 256 * 1024;

/// `path`'s contents, at most [`MAX_ZONE_FILE_BYTES`]; a larger file is an
/// error (`InvalidData`), not cut.
#[cfg(any(unix, test))]
fn read_zone_file(path: &std::path::Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_ZONE_FILE_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_ZONE_FILE_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(data)
}

/// The name a zone read from TZif contents carries: one `TimeZoneName`
/// refuses, so it is shown as its offset alone and reported as no
/// effective zone, as jiff's own unnamed system zone is. Public for the
/// tests that give the loop such a zone.
pub const UNNAMED_ZONE: &str = "unnamed local zone";

/// A host name as the device name messages give: control characters become
/// spaces, and a name that is then blank is none; only a name that is not
/// is fitted (a longer one cut to 61 characters and `...`), so a long run
/// of spaces never becomes a name of spaces and `...`. Not trimmed
/// otherwise, as a device name is not.
fn host_name_as_device_name(name: &str) -> Option<DeviceName> {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(DeviceName::fitted(&cleaned))
    }
}

/// The zone `/etc/localtime` (or a file `TZ` names) gives: the database's
/// zone of the name its link points to (the part after the last
/// `zoneinfo/`, without a leading `posix/` or `right/`), or else the TZif
/// data the file holds, unnamed ([`UNNAMED_ZONE`]). `None` when neither
/// works.
#[cfg(any(unix, test))]
fn zone_of_localtime(
    link: Option<&std::path::Path>,
    contents: impl FnOnce() -> io::Result<Vec<u8>>,
) -> Option<TimeZone> {
    let named = link.and_then(|target| {
        let target = target.to_str()?;
        let (_, name) = target.rsplit_once("zoneinfo/")?;
        let name = name
            .strip_prefix("posix/")
            .or_else(|| name.strip_prefix("right/"))
            .unwrap_or(name);
        jiff::tz::db().get(name).ok()
    });
    named
        .or_else(|| {
            contents()
                .ok()
                .and_then(|data| TimeZone::tzif(UNNAMED_ZONE, &data).ok())
        })
        .filter(|zone| !zone.is_unknown())
}

/// The computer's host name, as text. `gethostname` may panic on Windows
/// when the name changes between its two calls; that is taken as a name
/// that cannot be read now.
fn os_host_name() -> Option<String> {
    host_name_from(gethostname::gethostname)
}

/// What `get` gives, as text (bytes that are not UTF-8 become U+FFFD), or
/// `None` when it panics. Catching only works while panics unwind, which
/// the check below makes sure of in every build.
fn host_name_from(get: impl FnOnce() -> std::ffi::OsString + panic::UnwindSafe) -> Option<String> {
    panic::catch_unwind(get)
        .ok()
        .map(|name| name.to_string_lossy().into_owned())
}

// `host_name_from` catches a panic of `gethostname`: a build whose panics
// abort would let one end the watchdog instead.
#[cfg(panic = "abort")]
compile_error!("wgal-runtime needs panics to unwind: os_host_name catches gethostname's panic");

fn later(at: MonoInstant, by: Duration) -> MonoInstant {
    at.checked_add(by).unwrap_or(at)
}

fn small(n: usize) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}
/// How long a [`Running`] dropped without [`Running::stop`] gives its
/// threads to finish on their own.
const DROP_DEADLINE: Duration = Duration::from_secs(30);
/// What [`Running::stop`] allows the owner thread past the deadline for its
/// last save and the `stopped` line, which come after the deadline ends the
/// wait for work.
pub const FINISH_GRACE: Duration = Duration::from_secs(1);

/// How the loop runs. The defaults suit the service.
///
/// `workers` caps the effective worker count, clamped to
/// [`MIN_WORKERS`]..=[`MAX_WORKERS`]; the loop runs what the host settings
/// and the monitored tunnels ask for within that (see the module
/// documentation). Lower it only to test what happens when rounds have to
/// wait; users set the count with `UpdateHostSettings`. `max_wait` is
/// the longest the owner thread sleeps with nothing due. `max_calls` is how
/// many requests may wait for the owner thread at once; more are answered
/// `busy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub workers: usize,
    pub max_wait: Duration,
    pub max_calls: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            workers: MAX_WORKERS,
            max_wait: Duration::from_secs(5),
            max_calls: 64,
        }
    }
}

/// Identifies one request handed to an [`Engine`], so its answer can be
/// matched when the request had to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallId(pub u64);

/// Counters and current sizes of the loop, for tests and diagnostics (plan
/// T28: probe frequency, no busy loop, nothing piling up).
///
/// `turns` counts wake-ups of the owner thread, `plan_passes` the calls of
/// `Watchdog::plan`. `ready` items are planned and wait for a worker,
/// `in_flight` items run, `held` requests wait for a tunnel's stop or start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub turns: u64,
    pub plan_passes: u64,
    pub items_started: u64,
    pub items_finished: u64,
    pub jobs_started: u64,
    pub calls_answered: u64,
    pub calls_held: u64,
    pub event_write_failures: u64,
    pub ready: usize,
    pub in_flight: usize,
    pub held: usize,
    /// Worker threads started and not asked to exit.
    pub workers: usize,
    /// The most worker threads the loop may run now.
    pub effective: usize,
}

/// What one [`Engine::turn`] hands out: items to run now (each has passed
/// `Watchdog::still_due`, and a worker is free for each), the notification
/// to send, and answers to requests that had been waiting.
///
/// `Debug` shows counts only: a job is never formatted.
#[derive(Default)]
pub struct Turn {
    pub work: Vec<WorkItem>,
    pub job: Option<Job>,
    pub answers: Vec<(CallId, ResponseEnvelope)>,
}

impl fmt::Debug for Turn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Turn")
            .field("work", &self.work.len())
            .field("job", &self.job.is_some())
            .field("answers", &self.answers.len())
            .finish()
    }
}

/// An item handed out and not applied yet.
#[derive(Debug)]
struct Out {
    tunnel_id: TunnelId,
    seq: u64,
    /// A stop, start or poll: work of the tunnel holding the control slot.
    slot: bool,
    control: bool,
}

/// A request waiting for its tunnel's stop or start.
struct Held {
    call: CallId,
    caller: Caller,
    envelope: RequestEnvelope,
}

fn uses_slot(item: &WorkItem) -> bool {
    !matches!(item.kind, WorkKind::Round { .. })
}

/// Starts and retires the worker threads an [`Engine`] hands items to. The
/// threads take items, and requests to exit, from one shared queue.
trait Workers: Send {
    /// Starts one more worker thread; false if it could not be created.
    fn start(&mut self) -> bool;
    /// Makes one worker thread exit: an idle one, or the first to finish
    /// its item.
    fn retire(&mut self);
    /// Worker threads that exist now, retiring ones included.
    fn running(&self) -> usize;
}

/// Workers without threads, for an [`Engine`] driven step by step: starting
/// one always works, and items run wherever the caller runs them.
#[derive(Debug, Default)]
struct Counted(usize);

impl Workers for Counted {
    fn start(&mut self) -> bool {
        self.0 += 1;
        true
    }

    fn retire(&mut self) {
        self.0 = self.0.saturating_sub(1);
    }

    fn running(&self) -> usize {
        self.0
    }
}

/// How long automatic recovery an import asked for waits for the recovery
/// target's baseline (owner decision of 2026-11-04): ten minutes.
pub const IMPORT_BASELINE_WAIT: Duration = Duration::from_secs(10 * 60);

fn reject(code: ErrorCode, reason: Option<ErrorReason>, message: &str) -> Outcome {
    Outcome::Err(ProtocolError::new(code, reason, message))
}

fn refusal(
    envelope: &RequestEnvelope,
    code: ErrorCode,
    reason: Option<ErrorReason>,
    message: &str,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(envelope.request_id.clone()),
        outcome: reject(code, reason, message),
    }
}

/// Refused because the watchdog is shutting down.
fn shutting_down(envelope: &RequestEnvelope) -> ResponseEnvelope {
    refusal(
        envelope,
        ErrorCode::Busy,
        Some(ErrorReason::ShuttingDown),
        "the watchdog is shutting down",
    )
}

/// Refused because too many requests wait already; nothing was done.
fn too_many_waiting(envelope: &RequestEnvelope) -> ResponseEnvelope {
    refusal(
        envelope,
        ErrorCode::Busy,
        Some(ErrorReason::TooManyWaiting),
        "too many requests are waiting",
    )
}

/// What is refused without looking at the watchdog: another protocol
/// version, or a caller that may make no request at all. Such a request
/// never waits and never takes a place among the waiting ones.
fn screen(caller: &Caller, envelope: &RequestEnvelope) -> Option<ResponseEnvelope> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        // The code says it all.
        return Some(refusal(
            envelope,
            ErrorCode::UnsupportedVersion,
            None,
            "the watchdog speaks another protocol version",
        ));
    }
    (!caller.allowed).then(|| {
        refusal(
            envelope,
            ErrorCode::Unauthorized,
            Some(ErrorReason::CallerNotAllowed),
            NOT_ALLOWED,
        )
    })
}

/// The owner thread's state (see the module documentation).
pub struct Engine<A, C, S, T> {
    watchdog: Watchdog<A, C>,
    notifier: Notifier<S, T>,
    access: AccessList,
    events: Option<EventLog>,
    tracker: Tracker,
    /// Starts and retires the worker threads.
    workers: Box<dyn Workers>,
    /// Worker threads started and not asked to exit.
    started: usize,
    /// [`Options::workers`].
    max_workers: usize,
    /// The zone log lines (and notifications) are written in.
    zone: TimeZone,
    /// Where the system's zone comes from: the OS, or a test's choice.
    system_zone: fn() -> Option<TimeZone>,
    /// Where the computer's host name comes from: the OS, or a test's
    /// choice (owner decision of 2026-11-19).
    host_name: fn() -> Option<String>,
    /// When the zone is resolved again.
    zone_refresh_at: MonoInstant,
    max_wait: Duration,
    max_held: usize,
    /// Planned, waiting for a free worker.
    ready: VecDeque<WorkItem>,
    out: Vec<Out>,
    /// Tunnels with a stop or start planned or out.
    locked: BTreeSet<TunnelId>,
    /// Tunnels whose stop was handed out, with no start or round since.
    stopped: BTreeSet<TunnelId>,
    /// Those of `stopped` whose operation owes a start: a recovery round or
    /// reconnect (seen from the core after the stop went out, so a disconnect
    /// turned into a reconnect counts too). Cleared when the start or a
    /// round is handed out.
    owed: BTreeSet<TunnelId>,
    held: VecDeque<Held>,
    job_out: bool,
    stopping: bool,
    /// [`Self::begin_stop`]: no new automatic action or notification.
    winding_down: bool,
    stats: Stats,
}

impl<A, C, S, T> fmt::Debug for Engine<A, C, S, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("started", &self.started)
            .field("zone", &self.zone.iana_name())
            .field("stopping", &self.stopping)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl<A, C, S, T> Engine<A, C, S, T>
where
    A: PlatformAdapter,
    C: Clock,
    S: SecretStore,
    T: SettingsStore,
{
    /// Takes over the parts, applies the stored time zone and writes
    /// `watchdog_started`, with every tunnel's state as diag lines, to the
    /// event log. No worker thread is started until work needs one.
    pub fn new(
        watchdog: Watchdog<A, C>,
        notifier: Notifier<S, T>,
        access: AccessList,
        events: Option<EventLog>,
        options: &Options,
    ) -> Self {
        let tracker = Tracker::default();
        let now = watchdog.clock().now();
        let mut engine = Self {
            watchdog,
            notifier,
            access,
            events,
            tracker,
            workers: Box::new(Counted::default()),
            started: 0,
            max_workers: options.workers.clamp(MIN_WORKERS, MAX_WORKERS),
            zone: TimeZone::UTC,
            system_zone: system_time_zone,
            host_name: os_host_name,
            zone_refresh_at: now,
            max_wait: options.max_wait,
            max_held: options.max_calls,
            ready: VecDeque::new(),
            out: Vec::new(),
            locked: BTreeSet::new(),
            stopped: BTreeSet::new(),
            owed: BTreeSet::new(),
            held: VecDeque::new(),
            job_out: false,
            stopping: false,
            winding_down: false,
            stats: Stats::default(),
        };
        engine.reset_zone(now);
        let wall = engine.watchdog.clock().wall_unix_ms();
        if let Some(log) = engine.events.as_mut() {
            log.event(wall, None, &LogEvent::WatchdogStarted);
        }
        engine.log();
        engine
    }

    /// Takes the system's time zone from `source` instead of the OS, and
    /// resolves the zone again at once. For tests, which must not depend on
    /// the zone of the machine they run on.
    pub fn with_system_zone(mut self, source: fn() -> Option<TimeZone>) -> Self {
        self.system_zone = source;
        let now = self.watchdog.clock().now();
        self.reset_zone(now);
        self
    }

    /// Takes the computer's host name from `source` instead of the OS, and
    /// reads it again at once. For tests, which must not depend on the
    /// machine they run on.
    pub fn with_host_name(mut self, source: fn() -> Option<String>) -> Self {
        self.host_name = source;
        self.refresh_host_name();
        self
    }

    pub fn watchdog(&self) -> &Watchdog<A, C> {
        &self.watchdog
    }

    /// Runs the items [`Self::turn`] hands out, on any thread.
    pub fn executor(&self) -> Executor<A, C> {
        self.watchdog.executor()
    }

    /// The zone log times (and notification times) are written in now: the
    /// configured one, or else the system's (UTC if that cannot be read).
    pub fn time_zone(&self) -> &TimeZone {
        &self.zone
    }

    /// One more than the tunnels being monitored, within
    /// [`MIN_WORKERS`]..=[`MAX_WORKERS`].
    fn worker_limit(&self) -> usize {
        self.watchdog
            .monitored_count()
            .saturating_add(1)
            .clamp(MIN_WORKERS, MAX_WORKERS)
    }

    /// The suggestion and the effective count (see the module
    /// documentation), with the limit.
    fn worker_counts(&self) -> (usize, usize, usize) {
        let limit = self.worker_limit();
        let suggested = suggested_workers(&self.watchdog.monitoring_load(), limit);
        let effective = self
            .watchdog
            .host_settings()
            .workers
            .map_or(suggested, |count| usize::from(count.get()))
            .clamp(MIN_WORKERS, limit)
            .min(self.max_workers);
        (suggested, limit, effective)
    }

    fn effective_workers(&self) -> usize {
        self.worker_counts().2
    }

    /// The snapshot's host part at `wall`: the worker counts and the time
    /// zone.
    pub fn host_status(&self, wall: i64) -> HostStatus {
        let (suggested, limit, effective) = self.worker_counts();
        let settings = self.watchdog.host_settings();
        let offset = Timestamp::from_millisecond(wall)
            .map_or(0, |at| self.zone.to_offset(at).seconds())
            .clamp(-MAX_OFFSET_SECS, MAX_OFFSET_SECS);
        HostStatus {
            watchdog_version: ProductVersion::current(),
            min_gui_version: ProductVersion::min_gui(),
            min_usable_gui_version: Some(ProductVersion::min_usable_gui()),
            mcp_enabled: self.watchdog.mcp_enabled(),
            mcp_telegram_enabled: self.watchdog.mcp_telegram_enabled(),
            workers: WorkerStatus {
                configured: settings.workers,
                suggested: small(suggested),
                limit: small(limit),
                effective: small(effective),
                running: small(self.workers.running()),
            },
            time_zone: TimeZoneStatus {
                configured: settings.time_zone.clone(),
                effective: self
                    .zone
                    .iana_name()
                    .and_then(|name| TimeZoneName::new(name).ok()),
                utc_offset_secs: offset,
            },
        }
    }

    /// The zone the settings ask for now: the configured one if the
    /// database knows it, else the system's, else UTC.
    fn resolve_zone(&self) -> TimeZone {
        self.watchdog
            .host_settings()
            .time_zone
            .as_ref()
            .and_then(known_zone)
            .unwrap_or_else(|| self.system_zone_now())
    }

    fn system_zone_now(&self) -> TimeZone {
        (self.system_zone)()
            .filter(|zone| !zone.is_unknown())
            .unwrap_or(TimeZone::UTC)
    }

    /// Resolves the zone again and applies it if it changed.
    fn refresh_zone(&mut self, now: MonoInstant) {
        self.zone_refresh_at = later(now, ZONE_REFRESH);
        let zone = self.resolve_zone();
        if zone != self.zone {
            self.apply_time_zone(zone);
        }
        self.refresh_host_name();
    }

    /// Reads the host name again and hands it to the notification service:
    /// fitted as a device name is (control characters become spaces, more
    /// than 64 characters are cut), `None` when it cannot be read or is
    /// blank. Kept only in memory.
    fn refresh_host_name(&mut self) {
        let name = (self.host_name)().and_then(|name| host_name_as_device_name(&name));
        self.notifier.set_host_name(name);
    }

    /// Resolves the zone and applies it whatever it was before, so every
    /// user of it starts from the same one.
    fn reset_zone(&mut self, now: MonoInstant) {
        self.zone_refresh_at = later(now, ZONE_REFRESH);
        let zone = self.resolve_zone();
        self.apply_time_zone(zone);
        self.refresh_host_name();
    }

    /// The one place the time zone changes: everything that writes times
    /// for people takes it from here.
    fn apply_time_zone(&mut self, zone: TimeZone) {
        if let Some(log) = self.events.as_mut() {
            log.set_time_zone(zone.clone());
        }
        // Telegram messages are written in the same zone as the log.
        self.notifier.set_time_zone(zone.clone());
        self.zone = zone;
    }

    pub fn stats(&self) -> Stats {
        Stats {
            ready: self.ready.len(),
            in_flight: self.out.len(),
            held: self.held.len(),
            workers: self.started,
            effective: self.effective_workers(),
            ..self.stats
        }
    }

    /// Nothing more can come of what was sent: no item or notification is
    /// out, and no stop of WGAL's still waits for the start of its operation
    /// (review of wave two, P1-1).
    pub fn is_idle(&self) -> bool {
        self.out.is_empty()
            && !self.job_out
            && !self.start_owed()
            && !self.watchdog.disconnect_stop_unsent()
    }

    /// A stop WGAL sent for an operation that starts again has not been
    /// followed by that start yet, and the core still means to send it.
    fn start_owed(&self) -> bool {
        self.owed.iter().any(|id| self.watchdog.owes_start(id))
    }

    /// Idle, and every stop WGAL sent for an operation that starts again was
    /// followed by its start: none was cut off by a deadline or given up by
    /// the core. What a shutdown reports as "finished in time".
    pub fn nothing_left(&self) -> bool {
        self.is_idle() && self.owed.is_empty()
    }

    /// While shutting down, how long the owner thread may wait for a message
    /// before the next turn; `None` once idle. A stop the manager is still
    /// carrying out is polled every second, so this is the time to the next
    /// poll, not to the deadline.
    pub fn stopping_wait(&self) -> Option<Duration> {
        (!self.is_idle()).then(|| self.idle_wait())
    }

    /// Notes the stops that owe a start, as the core sees them now.
    /// Forgets the ones that owe nothing any more: no longer between their
    /// stop and its start, and not possibly left down against the user's
    /// wish (taken over by a disconnect, binding lost, or up after all). One
    /// the core gave up on while the tunnel may be down stays, so a shutdown
    /// reports it (review of wave two, N-P3-1).
    fn note_owed(&mut self) {
        for id in &self.stopped {
            if !self.owed.contains(id) && self.watchdog.owes_start(id) {
                self.owed.insert(id.clone());
            }
        }
        let watchdog = &self.watchdog;
        self.owed
            .retain(|id| watchdog.owes_start(id) || watchdog.may_be_left_down(id));
    }

    /// A request from a verified caller. Answered now, or `None` when it
    /// names a tunnel whose stop or start is planned or out: then it is
    /// answered in the [`Turn`] or the return value of [`Self::finished`]
    /// that releases it, under the same `call`.
    pub fn request(
        &mut self,
        call: CallId,
        caller: &Caller,
        envelope: RequestEnvelope,
    ) -> Option<ResponseEnvelope> {
        if let Some(refused) = screen(caller, &envelope) {
            return Some(refused);
        }
        if self.stopping && !matches!(envelope.request, Request::Disconnect { .. }) {
            return Some(shutting_down(&envelope));
        }
        if self.must_wait(&envelope.request) {
            if self.held.len() >= self.max_held {
                return Some(too_many_waiting(&envelope));
            }
            self.stats.calls_held = self.stats.calls_held.saturating_add(1);
            self.held.push_back(Held {
                call,
                caller: caller.clone(),
                envelope,
            });
            return None;
        }
        Some(self.answer(caller, envelope))
    }

    fn answer(&mut self, caller: &Caller, envelope: RequestEnvelope) -> ResponseEnvelope {
        self.stats.calls_answered = self.stats.calls_answered.saturating_add(1);
        let origin = envelope.origin();
        let RequestEnvelope {
            request_id,
            request,
            ..
        } = envelope;
        // Owner decision of 2026-11-04: an agent's request is answered only
        // while MCP is on, which only the user turns on: in the window, or
        // with `wgal mcp on` (owner decision of 2026-11-18).
        let mcp_off = origin == Origin::Mcp && !self.watchdog.mcp_enabled();
        let authorized = if mcp_off {
            Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                Some(ErrorReason::McpDisabled),
                "MCP is off; turn it on in the WGAL window (its MCP tab), or with \"wgal mcp on\" in a terminal, to let agents use WGAL",
            ))
        } else if origin == Origin::Mcp && matches!(request, Request::SetCliLanguage { .. }) {
            // Only the user sets `wgal`'s shared language, with `wgal
            // language` (owner decision of 2026-11-19): `wgal-mcp` has no
            // tool for it, and an agent's line in the event log never names
            // it.
            Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                None,
                "agents may not set the command line's language; the user sets it with \"wgal language\" in a terminal",
            ))
        } else if let Some(error) = (origin == Origin::Mcp)
            .then(|| self.agent_telegram_refusal(&request))
            .flatten()
        {
            Err(error)
        } else {
            authorize(caller, &self.access, &self.watchdog, &request)
        };
        let outcome = match authorized {
            Err(error) => Outcome::Err(error),
            Ok(()) => {
                // The zone the snapshot and the log page give, and the host
                // name, as they are now.
                if matches!(request, Request::GetSnapshot | Request::ReadEventLog { .. }) {
                    let now = self.watchdog.clock().now();
                    self.refresh_zone(now);
                }
                let notification_before = notification_setting(&request)
                    .then(|| NotificationSeen::of(&self.notifier.status()));
                let mut outcome = match self.handle_own(caller, &request_id, &request) {
                    Some(outcome) => outcome,
                    None => match self
                        .notifier
                        .handle(&mut self.watchdog, &request_id, &request)
                    {
                        Some(outcome) => outcome,
                        None => self.watchdog.handle(&request),
                    },
                };
                if matches!(outcome, Outcome::Ok(_)) {
                    if origin == Origin::Mcp && !request.reads_only() {
                        self.log_agent_request(&request);
                    }
                    if let Some(before) = notification_before
                        && before != NotificationSeen::of(&self.notifier.status())
                    {
                        self.log_host_event(&LogEvent::SettingsChanged);
                    }
                    self.log_request(&request);
                }
                self.notifier.decorate(&mut outcome);
                if let Outcome::Ok(Response::Snapshot { snapshot }) = &mut outcome {
                    snapshot.host = self.host_status(snapshot.generated_at_unix_ms);
                }
                outcome
            }
        };
        ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id),
            outcome,
        }
    }

    /// Answers the requests the owner thread keeps for itself: the host
    /// settings, the event log and the settings file. `None` for any other
    /// request.
    fn handle_own(
        &mut self,
        caller: &Caller,
        request_id: &RequestId,
        request: &Request,
    ) -> Option<Outcome> {
        match request {
            Request::UpdateHostSettings { settings } => Some(self.update_host_settings(settings)),
            Request::ReadEventLog { filter, before_seq } => {
                Some(self.read_event_log(filter, *before_seq))
            }
            Request::ExportSettings => Some(self.export_settings()),
            Request::ImportSettings { document, options } => {
                Some(self.import_settings(caller, request_id, document, *options))
            }
            _ => None,
        }
    }

    /// `ExportSettings` (owner decision of 2026-11-04): WGAL's settings as a
    /// file, from the snapshot and the host and notification settings. No
    /// device name, Bot Token or Chat ID is part of it.
    fn export_settings(&mut self) -> Outcome {
        let snapshot = self.watchdog.snapshot();
        let host = self.watchdog.host_settings().clone();
        let tunnels: Vec<TunnelSettingsEntry> = snapshot
            .tunnels
            .iter()
            .map(|t| {
                // Asked for and waiting for the baseline counts as asked
                // for, with the "start when stopped externally" the import
                // asked for with it (Codex P2-N22).
                let pending = self.watchdog.pending_auto_recovery(&t.tunnel_id);
                (t, pending)
            })
            .map(|(t, pending)| TunnelSettingsEntry {
                tunnel_id: t.tunnel_id.clone(),
                name: t.display_name.clone(),
                monitoring: t.intent.monitoring_enabled,
                notifications: t.notifications_enabled,
                auto_recovery: t.intent.auto_recovery_enabled || pending.is_some(),
                start_when_stopped_externally: pending
                    .map_or(t.intent.start_when_stopped_externally, |p| {
                        p.start_when_stopped_externally
                    }),
                recovery_target: t.recovery_target.as_ref().map(|r| r.target),
                observe_targets: BoundedVec::try_from(
                    t.observe_targets
                        .iter()
                        .map(|o| o.target)
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default(),
                probe: t.probe,
                retry: t.retry,
            })
            .collect();
        let Ok(tunnels) = BoundedVec::try_from(tunnels) else {
            return reject(ErrorCode::Internal, None, "too many tunnels to export");
        };
        Outcome::Ok(Response::SettingsExport {
            document: SettingsDocument {
                format: SettingsFormat::WgalSettings,
                version: SETTINGS_DOCUMENT_VERSION,
                watchdog_version: ProductVersion::current(),
                exported_at_unix_ms: self.watchdog.clock().wall_unix_ms(),
                host: HostSettingsEntry {
                    workers: host.workers,
                    time_zone: host.time_zone,
                    notifications_enabled: self.notifier.status().enabled,
                },
                tunnels,
            },
        })
    }

    /// `ImportSettings` (owner decision of 2026-11-04): each entry is
    /// matched to a tunnel the official manager has now, then set with the
    /// same requests the GUI sends, each authorized for `caller` on its own,
    /// so a refused part says so and the rest go on. Each tunnel's part, and
    /// the host's, is set all together or not at all (Codex P2-N21).
    /// Automatic recovery that
    /// cannot turn on yet because the recovery target's baseline has not
    /// passed waits for it, for [`IMPORT_BASELINE_WAIT`]. With
    /// `options.dry_run` nothing is changed and the result says what would be.
    fn import_settings(
        &mut self,
        caller: &Caller,
        request_id: &RequestId,
        document: &SettingsDocument,
        options: ImportOptions,
    ) -> Outcome {
        let discovered = match self.watchdog.handle(&Request::DiscoverTunnels) {
            Outcome::Ok(Response::Discovered { tunnels }) => tunnels.to_vec(),
            Outcome::Err(error) => return Outcome::Err(error),
            Outcome::Ok(_) => return reject(ErrorCode::Internal, None, "discovery gave no list"),
        };
        let key = |name: &DisplayName, id: &TunnelId| match options.match_by {
            ImportMatch::Name => name.as_str().to_owned(),
            ImportMatch::Id => id.as_str().to_owned(),
        };
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for entry in document.tunnels.iter() {
            *seen.entry(key(&entry.name, &entry.tunnel_id)).or_default() += 1;
        }
        let mut items = Vec::with_capacity(document.tunnels.len());
        for (index, entry) in document.tunnels.iter().enumerate() {
            let candidates: Vec<&DiscoveredTunnel> = discovered
                .iter()
                .filter(|d| {
                    key(&d.display_name, &d.tunnel_id) == key(&entry.name, &entry.tunnel_id)
                })
                .collect();
            let (tunnel_id, outcome, auto_recovery) = if seen
                .get(&key(&entry.name, &entry.tunnel_id))
                .copied()
                .unwrap_or(0)
                > 1
            {
                (
                    None,
                    ImportOutcome::DuplicateInFile,
                    AutoRecoveryImport::Off,
                )
            } else {
                match candidates.as_slice() {
                    [] => (None, ImportOutcome::NotFound, AutoRecoveryImport::Off),
                    [found] => {
                        let (outcome, auto) = self.import_tunnel(caller, entry, found, options);
                        (Some(found.tunnel_id.clone()), outcome, auto)
                    }
                    _ => (None, ImportOutcome::Ambiguous, AutoRecoveryImport::Off),
                }
            };
            items.push(ImportItem {
                index: u16::try_from(index).unwrap_or(u16::MAX),
                name: entry.name.clone(),
                tunnel_id,
                outcome,
                auto_recovery,
            });
        }
        let host = self.import_host(&document.host, options.dry_run);
        let notifications = self.import_notifications(request_id, &document.host, options.dry_run);
        Outcome::Ok(Response::SettingsImport {
            result: ImportResult {
                dry_run: options.dry_run,
                host,
                notifications: Some(notifications),
                tunnels: BoundedVec::try_from(items).unwrap_or_default(),
            },
        })
    }

    /// One entry of an import, matched to `found` (see
    /// [`Self::import_settings`]).
    fn import_tunnel(
        &mut self,
        caller: &Caller,
        entry: &TunnelSettingsEntry,
        found: &DiscoveredTunnel,
        options: ImportOptions,
    ) -> (ImportOutcome, AutoRecoveryImport) {
        let id = found.tunnel_id.clone();
        let refused = |e: &ProtocolError| ImportOutcome::Refused {
            code: e.code,
            reason: e.reason,
        };
        let registered = self.watchdog.registered_binding(&id).is_some();
        let register = Request::RegisterTunnel {
            tunnel_id: id.clone(),
        };
        let mut outcome = ImportOutcome::Applied;
        if !registered {
            if !options.register_missing {
                return (ImportOutcome::NotRegistered, AutoRecoveryImport::Off);
            }
            if found.control != Capability::Supported {
                return (ImportOutcome::Unsupported, AutoRecoveryImport::Off);
            }
            if let Err(e) = authorize(caller, &self.access, &self.watchdog, &register) {
                return (refused(&e), AutoRecoveryImport::Off);
            }
            outcome = ImportOutcome::Registered;
        }
        // A tunnel registered by this import stays unmonitored unless asked.
        let monitoring = if registered {
            entry.monitoring
        } else {
            options.monitor_new && entry.monitoring
        };
        let auto_on = Request::SetAutoRecovery {
            tunnel_id: id.clone(),
            enabled: true,
        };
        let start_on = Request::SetStartWhenStoppedExternally {
            tunnel_id: id.clone(),
            enabled: true,
        };
        // A tunnel this import registers is judged by the binding it is
        // registered with, which the access list is checked against for
        // registering it too, so a dry run, which registers nothing, says
        // what the import would do (Codex P3-N24).
        let may = |request: &Request| {
            let request = if registered { request } else { &register };
            authorize(caller, &self.access, &self.watchdog, request)
        };
        let may_auto = may(&auto_on);
        let may_start = !entry.start_when_stopped_externally || may(&start_on).is_ok();
        // Whether the recovery target's baseline holds after the import: it
        // does only if the tunnel is registered, the target stays the same
        // and it passed before.
        let snapshot = self.watchdog.snapshot();
        let status = snapshot.tunnels.iter().find(|t| t.tunnel_id == id);
        let baseline_holds = status.is_some_and(|t| {
            t.auto_recovery_allowed
                && t.recovery_target.as_ref().map(|r| r.target) == entry.recovery_target
        });
        let auto = match (&may_auto, entry.auto_recovery) {
            (_, false) => AutoRecoveryImport::Off,
            (Err(e), true) => AutoRecoveryImport::Refused {
                code: e.code,
                reason: e.reason,
            },
            (Ok(()), true) if entry.recovery_target.is_none() => AutoRecoveryImport::Refused {
                code: ErrorCode::InvalidState,
                reason: Some(ErrorReason::BaselineNotPassed),
            },
            (Ok(()), true) if baseline_holds => AutoRecoveryImport::On,
            (Ok(()), true) => AutoRecoveryImport::WaitingForBaseline,
        };
        if options.dry_run {
            return (outcome, auto);
        }
        let start = entry.start_when_stopped_externally && may_start;
        let start_as_filed = Request::SetStartWhenStoppedExternally {
            tunnel_id: id.clone(),
            enabled: start,
        };
        let auto_off = Request::SetAutoRecovery {
            tunnel_id: id.clone(),
            enabled: false,
        };
        let until =
            self.watchdog.clock().wall_unix_ms().saturating_add(
                i64::try_from(IMPORT_BASELINE_WAIT.as_millis()).unwrap_or(i64::MAX),
            );
        let settings = [
            Request::UpdateMonitorSettings {
                tunnel_id: id.clone(),
                settings: MonitorSettings {
                    recovery_target: entry.recovery_target,
                    observe_targets: entry.observe_targets.clone(),
                    probe: entry.probe,
                },
            },
            Request::SetRecoveryRetry {
                tunnel_id: id.clone(),
                retry: entry.retry,
            },
            Request::SetTunnelNotifications {
                tunnel_id: id.clone(),
                enabled: entry.notifications,
            },
            Request::SetTunnelMonitoring {
                tunnel_id: id.clone(),
                enabled: monitoring,
            },
        ];
        // Registered, set and stored together, or not at all: a refused
        // step, or a save that fails, leaves the tunnel as it was (Codex
        // P2-N21). A part the access list does not allow was refused above
        // and is reported as such; the rest is set.
        let applied = self.watchdog.all_or_nothing(|wd| {
            let ok = |outcome: Outcome| match outcome {
                Outcome::Ok(_) => Ok(()),
                Outcome::Err(e) => Err(e),
            };
            if !registered {
                ok(wd.handle(&register))?;
            }
            for request in &settings {
                ok(wd.handle(request))?;
            }
            match auto {
                AutoRecoveryImport::On => {
                    ok(wd.handle(&auto_on))?;
                    ok(wd.handle(&start_as_filed))?;
                }
                AutoRecoveryImport::WaitingForBaseline => {
                    wd.arm_auto_recovery(&id, start, until)?;
                }
                AutoRecoveryImport::Off | AutoRecoveryImport::Refused { .. } => {
                    // Off in the file, or refused: off here too, with the
                    // other switch as the file has it where allowed.
                    ok(wd.handle(&auto_off))?;
                    ok(wd.handle(&start_as_filed))?;
                }
            }
            Ok(auto)
        });
        match applied {
            Ok(auto) => (outcome, auto),
            Err(e) => (refused(&e), AutoRecoveryImport::Off),
        }
    }

    /// The host settings of an import, the worker threads and time zone:
    /// one save of the watchdog's state, so set whole or not at all.
    fn import_host(&mut self, host: &HostSettingsEntry, dry_run: bool) -> ImportOutcome {
        let refused = |e: ProtocolError| ImportOutcome::Refused {
            code: e.code,
            reason: e.reason,
        };
        if let Some(name) = &host.time_zone
            && known_zone(name).is_none()
        {
            return ImportOutcome::Refused {
                code: ErrorCode::InvalidSetting,
                reason: Some(ErrorReason::UnknownTimeZone),
            };
        }
        if dry_run {
            return ImportOutcome::Applied;
        }
        let settings = HostSettings {
            workers: host.workers,
            time_zone: host.time_zone.clone(),
        };
        if &settings != self.watchdog.host_settings()
            && let Outcome::Err(e) = self.update_host_settings(&settings)
        {
            return refused(e);
        }
        ImportOutcome::Applied
    }

    /// The notification switch of an import, keeping this computer's device
    /// name and chat: the same request as the window's switch, stored apart
    /// from the host settings, so its outcome is its own (Codex P2-N32).
    fn import_notifications(
        &mut self,
        request_id: &RequestId,
        host: &HostSettingsEntry,
        dry_run: bool,
    ) -> ImportOutcome {
        let now = self.notifier.status();
        if dry_run || now.enabled == host.notifications_enabled {
            return ImportOutcome::Applied;
        }
        let request = Request::UpdateNotificationSettings {
            enabled: host.notifications_enabled,
            device_name: now.device_name,
            chat_id: now.chat_id,
        };
        match self
            .notifier
            .handle(&mut self.watchdog, request_id, &request)
        {
            Some(Outcome::Err(e)) => ImportOutcome::Refused {
                code: e.code,
                reason: e.reason,
            },
            _ => ImportOutcome::Applied,
        }
    }

    /// Writes that an agent asked for `request` through `wgal-mcp` and it
    /// was accepted, about the tunnel it names, if any; what it changed
    /// follows (owner decision of 2026-11-04).
    fn log_agent_request(&mut self, request: &Request) {
        let wall = self.watchdog.clock().wall_unix_ms();
        let event = LogEvent::AgentRequest { op: request.op() };
        let label = named_tunnel(request).map(|id| label_of(&self.watchdog, &self.tracker, id));
        if let Some(log) = self.events.as_mut() {
            log.event(wall, label.as_ref(), &event);
        }
    }

    /// Why an agent's request about Telegram is refused (owner decision of
    /// 2026-11-06), or `None`. Only the window lets agents read the chat ID
    /// and change the device name, chat ID and bot token, so an agent may
    /// never turn that switch on, and while it is off an agent may change
    /// none of the three: `UpdateNotificationSettings` only turns
    /// notifications on or off, with the device name and chat ID they
    /// have. Turning the switch off, which only narrows what agents may do,
    /// is left to them like turning MCP off.
    fn agent_telegram_refusal(&self, request: &Request) -> Option<ProtocolError> {
        let refused = |message: &str| {
            Some(ProtocolError::new(
                ErrorCode::Unauthorized,
                Some(ErrorReason::McpTelegramDisabled),
                message,
            ))
        };
        let changes = match request {
            Request::SetMcpTelegramEnabled { enabled: true } => {
                return refused(
                    "only the user can let agents set Telegram: in the WGAL window (its MCP tab), or with \"wgal mcp telegram on\" in a terminal",
                );
            }
            Request::SetBotToken { .. } | Request::ClearBotToken => true,
            Request::UpdateNotificationSettings {
                device_name,
                chat_id,
                ..
            } => {
                let now = self.notifier.status();
                *device_name != now.device_name || *chat_id != now.chat_id
            }
            _ => false,
        };
        if changes && !self.watchdog.mcp_telegram_enabled() {
            return refused(
                "agents may not change the device name, chat ID or bot token; the user can allow it in the WGAL window (its MCP tab), or with \"wgal mcp telegram on\" in a terminal",
            );
        }
        None
    }

    /// Writes one line about no tunnel now.
    fn log_host_event(&mut self, event: &LogEvent) {
        let wall = self.watchdog.clock().wall_unix_ms();
        if let Some(log) = self.events.as_mut() {
            log.event(wall, None, event);
        }
    }

    /// `UpdateHostSettings` (see the module documentation).
    fn update_host_settings(&mut self, settings: &HostSettings) -> Outcome {
        let zone = match &settings.time_zone {
            Some(name) => match known_zone(name) {
                Some(zone) => Some(zone),
                None => {
                    return reject(
                        ErrorCode::InvalidSetting,
                        Some(ErrorReason::UnknownTimeZone),
                        "the time zone is not in the watchdog's time zone database",
                    );
                }
            },
            None => None,
        };
        let changed = settings != self.watchdog.host_settings();
        if self.watchdog.set_host_settings(settings.clone()).is_err() {
            return reject(
                ErrorCode::StorageUnavailable,
                Some(self.watchdog.storage_reason()),
                "the host settings could not be stored",
            );
        }
        // The worker count applies from the next turn: work is handed out
        // within it, and workers above it exit once idle.
        let now = self.watchdog.clock().now();
        self.zone_refresh_at = later(now, ZONE_REFRESH);
        let zone = zone.unwrap_or_else(|| self.system_zone_now());
        if zone != self.zone {
            self.apply_time_zone(zone);
        }
        if changed {
            self.log_host_event(&LogEvent::SettingsChanged);
        }
        Outcome::Ok(Response::Accepted)
    }

    /// `ReadEventLog`: one page from the event log, each label mapped back
    /// to the tunnel registered under it now (see [`EventLog::read_page`]).
    fn read_event_log(&mut self, filter: &EventFilter, before_seq: Option<u64>) -> Outcome {
        let Some(log) = &self.events else {
            return reject(
                ErrorCode::Internal,
                Some(ErrorReason::EventLogUnavailable),
                "this watchdog keeps no event log",
            );
        };
        let registered = self.watchdog.snapshot().tunnels;
        let ids: BTreeMap<&str, &TunnelId> = registered
            .iter()
            .filter_map(|t| Some((self.watchdog.log_label(&t.tunnel_id)?, &t.tunnel_id)))
            .collect();
        let wanted = match filter {
            EventFilter::All => Lines::All,
            EventFilter::Watchdog => Lines::Watchdog,
            EventFilter::System => Lines::System,
            // A tunnel that is not registered has no label: no line is its.
            EventFilter::Tunnel { tunnel_id } => match self.watchdog.log_label(tunnel_id) {
                Some(label) => Lines::Tunnel(label),
                None => {
                    return Outcome::Ok(Response::EventLog {
                        entries: BoundedVec::new(),
                        more: false,
                    });
                }
            },
        };
        let (entries, more) = log.read_page(before_seq, wanted, |label| {
            ids.get(label).map(|id| (*id).clone())
        });
        match BoundedVec::try_from(entries) {
            Ok(entries) => Outcome::Ok(Response::EventLog { entries, more }),
            // Pages are cut to fit before this; nothing the user can act on.
            Err(_) => reject(ErrorCode::Internal, None, "the event log page is too long"),
        }
    }

    /// After an accepted request: the user's operation, if it is one, then
    /// whatever came of the request (see [`Self::log`]). Requests that only
    /// read write nothing.
    fn log_request(&mut self, request: &Request) {
        let (tunnel_id, event) = match request {
            Request::GetSnapshot
            | Request::DiscoverTunnels
            | Request::ReadEventLog { .. }
            | Request::ExportSettings => {
                return;
            }
            Request::SetMcpEnabled { .. } | Request::SetMcpTelegramEnabled { .. } => {
                self.log_host_event(&LogEvent::SettingsChanged);
                self.log();
                return;
            }
            Request::Connect { tunnel_id } => (tunnel_id, LogEvent::UserConnect),
            Request::Disconnect { tunnel_id } => (tunnel_id, LogEvent::UserDisconnect),
            Request::Reconnect { tunnel_id } => (tunnel_id, LogEvent::UserReconnect),
            _ => {
                self.log();
                return;
            }
        };
        let wall = self.watchdog.clock().wall_unix_ms();
        let label = label_of(&self.watchdog, &self.tracker, tunnel_id);
        if let Some(log) = self.events.as_mut() {
            log.event(wall, Some(&label), &event);
        }
        self.log();
    }

    /// Writes what happened since the last call: the core's events, oldest
    /// first, then what changed between the last snapshot and this one. The
    /// core's events are taken even without a log, so they never pile up.
    fn log(&mut self) {
        let core = self.watchdog.take_events();
        let effective = self.effective_workers();
        let Some(log) = self.events.as_mut() else {
            return;
        };
        let wall = self.watchdog.clock().wall_unix_ms();
        for (tunnel_id, event) in &core {
            // Asked right after `take_events`: the core still answers for a
            // tunnel whose removal it just reported.
            let label = tunnel_id
                .as_ref()
                .map(|id| label_of(&self.watchdog, &self.tracker, id));
            log.event(wall, label.as_ref(), event);
        }
        let mut snapshot = self.watchdog.snapshot();
        snapshot.notification = self.notifier.status();
        let at = snapshot.generated_at_unix_ms;
        let watchdog = &self.watchdog;
        let label = |id: &TunnelId| TunnelLabel::stored(watchdog.log_label(id));
        for (tunnel, entry) in self.tracker.observe(&snapshot, effective, label) {
            match entry {
                Entry::Event(event) => log.event(at, tunnel.as_ref(), &event),
                Entry::Diag(diag) => log.diag(at, tunnel.as_ref(), &diag),
            }
        }
        self.stats.event_write_failures = log.failures();
    }

    /// Applies the result of an item [`Self::turn`] handed out. Returns the
    /// answers to requests it released.
    pub fn finished(&mut self, result: WorkResult) -> Vec<(CallId, ResponseEnvelope)> {
        self.stats.items_finished = self.stats.items_finished.saturating_add(1);
        let out = self
            .out
            .iter()
            .position(|o| o.tunnel_id == result.tunnel_id && o.seq == result.seq)
            .map(|at| self.out.swap_remove(at));
        let tunnel = result.tunnel_id.clone();
        self.watchdog.apply(result);
        self.note_owed();
        // Before the requests it releases, so the lines keep their order.
        self.log();
        if out.is_some_and(|o| o.control) {
            self.unlock(&tunnel)
        } else {
            Vec::new()
        }
    }

    /// Records what came of the job [`Self::turn`] handed out.
    pub fn delivered(&mut self, job: Job, delivery: Delivery) {
        self.job_out = false;
        self.notifier.finish(&mut self.watchdog, job, delivery);
        // Now, while the notifier's last failure is this delivery's.
        self.log();
    }

    /// The host woke from sleep.
    pub fn on_wake(&mut self) {
        self.watchdog.on_wake();
    }

    /// The host's network changed.
    pub fn on_network_change(&mut self) {
        self.watchdog.on_network_change();
    }

    /// Plans what is due, hands out what may start, picks the next
    /// notification and writes what changed to the event log. Call it after
    /// every input and whenever [`Self::idle_wait`] has passed.
    pub fn turn(&mut self) -> Turn {
        self.stats.turns = self.stats.turns.saturating_add(1);
        self.stats.plan_passes = self.stats.plan_passes.saturating_add(1);
        let now = self.watchdog.clock().now();
        if now >= self.zone_refresh_at {
            self.refresh_zone(now);
        }
        for item in self.watchdog.plan() {
            if self.stopping && !self.finishes_a_stop(&item) {
                // Dropped unsent while shutting down.
                continue;
            }
            if item.is_control() {
                self.locked.insert(item.tunnel_id.clone());
            }
            self.ready.push_back(item);
        }
        let mut answers = Vec::new();
        let work = self.dispatch(&mut answers);
        self.note_owed();
        let job = if self.stopping || self.winding_down || self.job_out {
            None
        } else {
            // A message is written with the zone and host name as they are
            // now (owner decision of 2026-11-19): a changed system zone takes
            // effect at once, not at the next ten-minute look. Both are read
            // once it is certain a message is written, right before its text.
            self.notifier
                .next_message(&mut self.watchdog)
                .map(|pending| {
                    self.refresh_zone(now);
                    self.notifier.write(pending)
                })
        };
        if job.is_some() {
            self.job_out = true;
            self.stats.jobs_started = self.stats.jobs_started.saturating_add(1);
        }
        self.log();
        Turn { work, job, answers }
    }

    fn rounds_out(&self) -> usize {
        self.out.iter().filter(|o| !o.slot).count()
    }

    /// Whether a worker can take an item at once: a started one is idle, or
    /// one more may be started. Rounds never take the last worker of
    /// `effective`, which is kept for the stop, start or poll of the tunnel
    /// holding the control slot. While more rounds are out than a lowered
    /// `effective` allows, slot work may still have one worker more than
    /// those rounds, so it does not wait behind them.
    fn worker_for(&mut self, is_slot: bool, effective: usize) -> bool {
        let rounds_out = self.rounds_out();
        if !is_slot && rounds_out + 1 >= effective {
            return false;
        }
        if self.out.len() < self.started {
            return true;
        }
        let most = if is_slot {
            effective.max(rounds_out + 1)
        } else {
            effective
        };
        if self.started < most && self.workers.start() {
            self.started += 1;
            return true;
        }
        false
    }

    /// Asks idle workers beyond what is needed now to exit: `effective`, or
    /// one more than the rounds still out if that is more (see
    /// [`Self::worker_for`]). A busy one is asked once it is idle, after its
    /// current item.
    fn retire_idle(&mut self, effective: usize) {
        let keep = effective.max(self.rounds_out() + 1);
        while self.started > keep && self.out.len() < self.started {
            self.workers.retire();
            self.started -= 1;
        }
    }

    /// Hands out ready items while workers are free, starting workers as
    /// needed up to the effective count: slot work first, rounds only while
    /// a worker stays free for slot work. Each item passes `still_due` right
    /// before it goes; a withdrawn stop or start releases the requests
    /// waiting for it. Then retires idle workers above the count.
    fn dispatch(&mut self, answers: &mut Vec<(CallId, ResponseEnvelope)>) -> Vec<WorkItem> {
        let effective = self.effective_workers();
        let (slot, rounds): (VecDeque<WorkItem>, VecDeque<WorkItem>) =
            self.ready.drain(..).partition(uses_slot);
        let mut started = Vec::new();
        let mut waiting = VecDeque::new();
        let mut withdrawn = Vec::new();
        for item in slot.into_iter().chain(rounds) {
            let is_slot = uses_slot(&item);
            if !self.worker_for(is_slot, effective) {
                waiting.push_back(item);
                continue;
            }
            if self.watchdog.still_due(&item) {
                self.stats.items_started = self.stats.items_started.saturating_add(1);
                match item.kind {
                    WorkKind::Stop => {
                        self.stopped.insert(item.tunnel_id.clone());
                    }
                    WorkKind::Start | WorkKind::Round { .. } => {
                        self.stopped.remove(&item.tunnel_id);
                        self.owed.remove(&item.tunnel_id);
                    }
                    WorkKind::Poll => {}
                }
                self.out.push(Out {
                    tunnel_id: item.tunnel_id.clone(),
                    seq: item.seq,
                    slot: is_slot,
                    control: item.is_control(),
                });
                started.push(item);
            } else if item.is_control() {
                withdrawn.push(item.tunnel_id);
            }
        }
        self.ready = waiting;
        self.retire_idle(effective);
        for tunnel in withdrawn {
            answers.extend(self.unlock(&tunnel));
        }
        started
    }

    /// Whether `request` waits: it names a tunnel whose stop or start is
    /// planned or out, or it is an import, which may change any tunnel,
    /// and one is (Codex P2-N28: a change under a stop or start already
    /// out would discard its result and send it again).
    fn must_wait(&self, request: &Request) -> bool {
        match request {
            Request::ImportSettings { .. } => !self.locked.is_empty(),
            _ => named_tunnel(request).is_some_and(|id| self.locked.contains(id)),
        }
    }

    /// The tunnel's stop or start is over; requests waiting for it are
    /// answered, in order, unless another one is planned or out.
    fn unlock(&mut self, tunnel: &TunnelId) -> Vec<(CallId, ResponseEnvelope)> {
        let busy = self
            .ready
            .iter()
            .any(|i| i.is_control() && &i.tunnel_id == tunnel)
            || self.out.iter().any(|o| o.control && &o.tunnel_id == tunnel);
        if busy {
            return Vec::new();
        }
        self.locked.remove(tunnel);
        let mut answers = Vec::new();
        let mut waiting = VecDeque::new();
        while let Some(held) = self.held.pop_front() {
            if self.must_wait(&held.envelope.request) {
                waiting.push_back(held);
            } else {
                answers.push((held.call, self.answer(&held.caller, held.envelope)));
            }
        }
        self.held = waiting;
        answers
    }

    /// How long the owner thread may sleep before the next turn if nothing
    /// arrives: until `Watchdog::next_due`, at most `max_wait`. Never zero
    /// unless `max_wait` is.
    pub fn idle_wait(&self) -> Duration {
        let now = self.watchdog.clock().now();
        self.watchdog.next_due().map_or(self.max_wait, |at| {
            at.saturating_duration_since(now).min(self.max_wait)
        })
    }

    /// Whether a shutting-down loop still runs `item`: a poll, the start
    /// that follows a stop already handed out, or the stop of a user's
    /// disconnect.
    fn finishes_a_stop(&self, item: &WorkItem) -> bool {
        match item.kind {
            WorkKind::Poll => true,
            WorkKind::Start => self.stopped.contains(&item.tunnel_id),
            WorkKind::Stop => self.watchdog.stopping_for_disconnect(&item.tunnel_id),
            WorkKind::Round { .. } => false,
        }
    }

    /// Winds down (see the module documentation): no new automatic action
    /// and no new notification; requests are answered as usual, and a stop
    /// already sent still gets its start. Idempotent.
    pub fn begin_stop(&mut self) {
        self.winding_down = true;
        self.watchdog.hold_automatic_actions();
    }

    /// Starts shutting down (see the module documentation): winds down,
    /// drops items planned but not handed out (they reached nothing) unless
    /// they finish a stop already sent, and answers waiting requests `busy`
    /// without applying them, except disconnects, which stay waiting for
    /// their tunnel's stop and are applied when it is in.
    pub fn stop_planning(&mut self) -> Vec<(CallId, ResponseEnvelope)> {
        self.begin_stop();
        self.stopping = true;
        let ready = std::mem::take(&mut self.ready);
        self.ready = ready
            .into_iter()
            .filter(|item| self.finishes_a_stop(item))
            .collect();
        let (ready, out) = (&self.ready, &self.out);
        self.locked.retain(|id| {
            ready.iter().any(|i| i.is_control() && &i.tunnel_id == id)
                || out.iter().any(|o| o.control && &o.tunnel_id == id)
        });
        let (disconnects, others): (VecDeque<Held>, VecDeque<Held>) = self
            .held
            .drain(..)
            .partition(|held| matches!(held.envelope.request, Request::Disconnect { .. }));
        self.held = disconnects;
        others
            .into_iter()
            .map(|held| (held.call, shutting_down(&held.envelope)))
            .collect()
    }

    /// Finishes shutting down. Disconnects still waiting for a stop that
    /// did not come back in time are applied now (their intent is stored;
    /// the next watchdog carries them out), then the state is saved again if
    /// a save had failed and the event log gets `stopped` (diag, with
    /// `in_time`) and `watchdog_stopped`. Returns whether it all finished in
    /// time ([`Self::nothing_left`], judged after those disconnects) and
    /// their answers.
    pub fn shut_down(mut self) -> (bool, Vec<(CallId, ResponseEnvelope)>) {
        let held: Vec<Held> = self.held.drain(..).collect();
        let answers: Vec<_> = held
            .into_iter()
            .map(|held| {
                let answer = if matches!(held.envelope.request, Request::Disconnect { .. }) {
                    self.answer(&held.caller, held.envelope)
                } else {
                    shutting_down(&held.envelope)
                };
                (held.call, answer)
            })
            .collect();
        self.note_owed();
        let in_time = self.nothing_left();
        if self.watchdog.storage_health() != StorageHealth::Ok {
            // Failure is recorded in the snapshot's storage health.
            let _ = self.watchdog.save();
        }
        self.log();
        let wall = self.watchdog.clock().wall_unix_ms();
        if let Some(log) = self.events.as_mut() {
            log.diag(wall, None, &Diag::Stopped { in_time });
            log.event(wall, None, &LogEvent::WatchdogStopped);
        }
        (in_time, answers)
    }
}

/// How the event log names a tunnel: its stored label, or for one the
/// watchdog no longer has, the label last seen for it.
fn label_of<A: PlatformAdapter, C: Clock>(
    watchdog: &Watchdog<A, C>,
    tracker: &Tracker,
    id: &TunnelId,
) -> TunnelLabel {
    match watchdog.log_label(id) {
        Some(label) => TunnelLabel::stored(Some(label)),
        None => tracker
            .label(id)
            .cloned()
            .unwrap_or_else(|| TunnelLabel::stored(None)),
    }
}

enum Msg {
    Call {
        call: CallId,
        caller: Caller,
        envelope: RequestEnvelope,
        reply: SyncSender<ResponseEnvelope>,
    },
    Done(WorkResult),
    Delivered(Job, Delivery),
    Wake,
    NetworkChange,
    BeginStop,
    Stop {
        until: Instant,
    },
}

#[derive(Debug)]
struct Shared {
    accepting: AtomicBool,
    /// Calls sent to the owner thread and not answered yet.
    calls: AtomicUsize,
    max_calls: usize,
    next_call: AtomicU64,
    stats: Mutex<Stats>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hands requests to the loop. Cheap to clone; one per IPC connection is
/// fine.
#[derive(Clone)]
pub struct Client {
    inbox: Sender<Msg>,
    shared: Arc<Shared>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Answers one decoded request from a verified caller within
    /// `deadline`: `timeout` otherwise, `busy` if the loop is shutting down or
    /// too many requests wait. Another protocol version and a caller that is
    /// not `allowed` are refused here, without the loop. Checks the protocol
    /// version, authorizes,
    /// dispatches (notifier first, then the core), and fills the snapshot's
    /// notification part. A request that reached the loop is applied even if
    /// `deadline` passes first (see the module documentation).
    pub fn call(
        &self,
        caller: &Caller,
        envelope: RequestEnvelope,
        deadline: Duration,
    ) -> ResponseEnvelope {
        if let Some(refused) = screen(caller, &envelope) {
            return refused;
        }
        if !self.shared.accepting.load(Ordering::Acquire) {
            return shutting_down(&envelope);
        }
        if self.shared.calls.fetch_add(1, Ordering::AcqRel) >= self.shared.max_calls {
            self.shared.calls.fetch_sub(1, Ordering::AcqRel);
            return too_many_waiting(&envelope);
        }
        let (reply, answer) = mpsc::sync_channel(1);
        let call = CallId(self.shared.next_call.fetch_add(1, Ordering::Relaxed));
        let request_id = envelope.request_id.clone();
        let msg = Msg::Call {
            call,
            caller: caller.clone(),
            envelope,
            reply,
        };
        let refuse = |code, reason, message: &str| ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id.clone()),
            outcome: reject(code, Some(reason), message),
        };
        if self.inbox.send(msg).is_err() {
            self.shared.calls.fetch_sub(1, Ordering::AcqRel);
            return refuse(
                ErrorCode::Busy,
                ErrorReason::NotRunning,
                "the watchdog is not running",
            );
        }
        match answer.recv_timeout(deadline) {
            Ok(response) => response,
            Err(RecvTimeoutError::Timeout) => refuse(
                ErrorCode::Timeout,
                ErrorReason::NoAnswerInTime,
                "the watchdog did not answer in time",
            ),
            Err(RecvTimeoutError::Disconnected) => refuse(
                ErrorCode::Busy,
                ErrorReason::ShuttingDown,
                "the watchdog is shutting down",
            ),
        }
    }

    /// The OS resumed from sleep.
    pub fn on_wake(&self) {
        // A loop that is gone has nothing to restart.
        let _ = self.inbox.send(Msg::Wake);
    }

    /// The host's network changed (another Wi-Fi, a cable).
    pub fn on_network_change(&self) {
        let _ = self.inbox.send(Msg::NetworkChange);
    }
}

/// The loop's threads. Stop them with [`Self::stop`]; dropping this without
/// stopping lets them wind down on their own.
pub struct Running {
    threads: Option<Threads>,
}

struct Threads {
    inbox: Sender<Msg>,
    shared: Arc<Shared>,
    owner: JoinHandle<()>,
    /// Whether the owner thread finished in time, sent as its last act.
    finished: Receiver<bool>,
    /// The worker threads; the owner thread adds to them.
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// The notification thread.
    helpers: Vec<JoinHandle<()>>,
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Running")
            .field("stopped", &self.threads.is_none())
            .finish_non_exhaustive()
    }
}

impl Running {
    /// Refuses new calls, lets in-flight work finish within `deadline` (a
    /// control call can take up to the adapter's 25 s), persists, joins every
    /// thread. Returns whether it finished in time.
    ///
    /// It returns by `deadline` plus [`FINISH_GRACE`] whatever the threads do.
    /// A thread still busy then (a worker inside a platform call, the
    /// notification thread inside HTTP, or the owner thread itself, stuck in
    /// an adapter call made from a request, a save that blocks, or a bug) is
    /// not waited for and the result is `false`. Such a thread cannot be
    /// stopped from outside, so the caller must then end the process; the
    /// state is as last saved, and the core resumes from it after a restart.
    pub fn stop(mut self, deadline: Duration) -> bool {
        let Some(threads) = self.threads.take() else {
            return false;
        };
        let now = Instant::now();
        let until = now.checked_add(deadline).unwrap_or(now);
        threads.shared.accepting.store(false, Ordering::Release);
        // The owner thread stops on its own if every sender is gone.
        let _ = threads.inbox.send(Msg::Stop { until });
        let wait = until
            .checked_add(FINISH_GRACE)
            .unwrap_or(until)
            .saturating_duration_since(Instant::now());
        let in_time = match threads.finished.recv_timeout(wait) {
            Ok(in_time) => {
                // It sent its result as its last act; this returns at once.
                let _ = threads.owner.join();
                in_time
            }
            // It panicked: it is gone, nothing was finished.
            Err(RecvTimeoutError::Disconnected) => {
                let _ = threads.owner.join();
                false
            }
            // Stuck: leave it, and the helpers it still feeds, to the
            // process's end.
            Err(RecvTimeoutError::Timeout) => return false,
        };
        let mut joined = true;
        // The owner thread is done, so no worker is added any more.
        let workers = std::mem::take(&mut *lock(&threads.workers));
        for helper in workers.into_iter().chain(threads.helpers) {
            // In time, every helper is idle and ends now that its queue is
            // closed.
            if in_time || helper.is_finished() {
                joined &= helper.join().is_ok();
            } else {
                joined = false;
            }
        }
        in_time && joined
    }

    /// Winds the loop down at once, without waiting: no automatic recovery
    /// or start after an external stop is queued any more, queued ones that
    /// sent nothing are dropped, and no new notification is sent, while
    /// requests are still answered, monitoring goes on and a stop already
    /// sent still gets its start. For a service to call as soon as it is
    /// asked to stop, before closing its IPC endpoint and calling
    /// [`Self::stop`], so nothing new starts in between. Idempotent; it
    /// cannot be undone.
    pub fn begin_stop(&self) {
        if let Some(threads) = &self.threads {
            // A loop that is gone has nothing to wind down.
            let _ = threads.inbox.send(Msg::BeginStop);
        }
    }

    pub fn stats(&self) -> Stats {
        self.threads
            .as_ref()
            .map_or_else(Stats::default, |t| *lock(&t.shared.stats))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(threads) = self.threads.take() {
            threads.shared.accepting.store(false, Ordering::Release);
            let now = Instant::now();
            let until = now.checked_add(DROP_DEADLINE).unwrap_or(now);
            let _ = threads.inbox.send(Msg::Stop { until });
        }
    }
}

/// Starts the loop with [`Options::default`].
pub fn spawn<A, C, S, T>(
    watchdog: Watchdog<A, C>,
    notifier: Notifier<S, T>,
    transport: Box<dyn Transport + Send>,
    access: AccessList,
    events: Option<EventLog>,
) -> io::Result<(Client, Running)>
where
    A: PlatformAdapter + 'static,
    C: Clock + 'static,
    S: SecretStore + 'static,
    T: SettingsStore + 'static,
{
    spawn_with(
        watchdog,
        notifier,
        transport,
        access,
        events,
        &Options::default(),
    )
}

/// Starts the loop's owner and notification threads (see the module
/// documentation); worker threads start later, as work needs them. Fails
/// only if a thread cannot be created; the threads already started then
/// end.
pub fn spawn_with<A, C, S, T>(
    watchdog: Watchdog<A, C>,
    notifier: Notifier<S, T>,
    transport: Box<dyn Transport + Send>,
    access: AccessList,
    events: Option<EventLog>,
    options: &Options,
) -> io::Result<(Client, Running)>
where
    A: PlatformAdapter + 'static,
    C: Clock + 'static,
    S: SecretStore + 'static,
    T: SettingsStore + 'static,
{
    let mut engine = Engine::new(watchdog, notifier, access, events, options);
    let (inbox_tx, inbox) = mpsc::channel();
    let (work_tx, work_rx) = mpsc::channel::<Task>();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let shared = Arc::new(Shared {
        accepting: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        max_calls: options.max_calls,
        next_call: AtomicU64::new(0),
        stats: Mutex::new(engine.stats()),
    });

    // No worker is started here: the engine starts them as work needs them.
    let workers = Arc::new(Mutex::new(Vec::new()));
    engine.workers = Box::new(Pool {
        queue: Arc::new(Mutex::new(work_rx)),
        tasks: work_tx.clone(),
        executor: engine.executor(),
        done: inbox_tx.clone(),
        handles: Arc::clone(&workers),
        alive: Arc::new(AtomicUsize::new(0)),
        named: 0,
    });
    let done = inbox_tx.clone();
    let helpers = vec![
        thread::Builder::new()
            .name("wgal-notify".to_owned())
            .spawn(move || deliver(&job_rx, transport.as_ref(), &done))?,
    ];
    let owner_shared = Arc::clone(&shared);
    let (finished_tx, finished) = mpsc::sync_channel(1);
    let owner = thread::Builder::new()
        .name("wgal-watchdog".to_owned())
        .spawn(move || {
            let in_time = own(engine, &inbox, &work_tx, &job_tx, &owner_shared);
            // Nobody may be waiting any more.
            let _ = finished_tx.send(in_time);
        })?;
    let client = Client {
        inbox: inbox_tx.clone(),
        shared: Arc::clone(&shared),
    };
    let running = Running {
        threads: Some(Threads {
            inbox: inbox_tx,
            shared,
            owner,
            finished,
            workers,
            helpers,
        }),
    };
    Ok((client, running))
}

/// What a worker thread takes from the queue: an item, or the request to
/// exit that [`Workers::retire`] sends.
enum Task {
    Run(WorkItem),
    Exit,
}

/// Counts a worker thread while it exists, whether it ends by exiting, by
/// its queue closing, or because it could not be started.
struct Alive(Arc<AtomicUsize>);

impl Alive {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(count))
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The worker threads (see the module documentation). The engine hands out
/// an item only when a started worker is free for it, and asks a worker to
/// exit only when one is idle, so an item never waits behind an exit
/// request in the queue: the items and exit requests in the queue are never
/// more than the idle workers.
struct Pool<A, C> {
    queue: Arc<Mutex<Receiver<Task>>>,
    /// For exit requests; the owner thread sends the items.
    tasks: Sender<Task>,
    executor: Executor<A, C>,
    done: Sender<Msg>,
    /// Joined by [`Running::stop`]; finished ones are dropped as new ones
    /// start, so the list does not grow with threads that came and went.
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    alive: Arc<AtomicUsize>,
    /// Numbers the threads' names.
    named: u64,
}

impl<A: PlatformAdapter + 'static, C: Clock + 'static> Workers for Pool<A, C> {
    fn start(&mut self) -> bool {
        let queue = Arc::clone(&self.queue);
        let executor = self.executor.clone();
        let done = self.done.clone();
        let alive = Alive::new(&self.alive);
        let spawned = thread::Builder::new()
            .name(format!("wgal-work-{}", self.named))
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                let _alive = alive;
                work(&queue, &executor, &done);
            });
        self.named = self.named.wrapping_add(1);
        match spawned {
            Ok(handle) => {
                let mut handles = lock(&self.handles);
                handles.retain(|h| !h.is_finished());
                handles.push(handle);
                true
            }
            Err(_) => false,
        }
    }

    fn retire(&mut self) {
        // The queue closes only when the loop is gone, and every worker with it.
        let _ = self.tasks.send(Task::Exit);
    }

    fn running(&self) -> usize {
        self.alive.load(Ordering::Acquire)
    }
}

/// A worker: runs items until it is asked to exit or its queue closes.
fn work<A: PlatformAdapter, C: Clock>(
    queue: &Mutex<Receiver<Task>>,
    executor: &Executor<A, C>,
    done: &Sender<Msg>,
) {
    loop {
        let next = lock(queue).recv();
        let Ok(Task::Run(item)) = next else {
            return;
        };
        // An adapter that panics must not leave the item out forever.
        let result =
            panic::catch_unwind(AssertUnwindSafe(|| executor.run(&item))).unwrap_or_else(|_| {
                let error = AdapterError::new(
                    AdapterErrorKind::ParseError,
                    "the platform adapter failed unexpectedly",
                );
                executor.failed(&item, error)
            });
        if done.send(Msg::Done(result)).is_err() {
            return;
        }
    }
}

/// The notification thread: sends one job at a time until its queue closes.
fn deliver(jobs: &Receiver<Job>, transport: &(dyn Transport + Send), done: &Sender<Msg>) {
    while let Ok(job) = jobs.recv() {
        // A transport that panicked may have sent the message; it is never
        // sent again (plan §7.3).
        let delivery = panic::catch_unwind(AssertUnwindSafe(|| job.run(transport)))
            .unwrap_or(Delivery::Unknown);
        if done.send(Msg::Delivered(job, delivery)).is_err() {
            return;
        }
    }
}

/// Sends one answer and forgets the call.
fn reply(
    shared: &Shared,
    replies: &mut BTreeMap<CallId, SyncSender<ResponseEnvelope>>,
    call: CallId,
    answer: ResponseEnvelope,
) {
    if let Some(reply) = replies.remove(&call) {
        // The caller may have given up waiting.
        let _ = reply.try_send(answer);
        shared.calls.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The owner thread. Returns whether everything handed out came back in time.
fn own<A, C, S, T>(
    mut engine: Engine<A, C, S, T>,
    inbox: &Receiver<Msg>,
    work: &Sender<Task>,
    jobs: &Sender<Job>,
    shared: &Shared,
) -> bool
where
    A: PlatformAdapter + 'static,
    C: Clock + 'static,
    S: SecretStore,
    T: SettingsStore,
{
    let mut replies = BTreeMap::new();
    let mut stop_at: Option<Instant> = None;
    loop {
        // The turn starts the workers its items need, within the effective
        // count; one that cannot be created leaves its item waiting.
        let turn = engine.turn();
        for item in turn.work {
            if let Err(mpsc::SendError(Task::Run(item))) = work.send(Task::Run(item)) {
                // No worker is left (cannot happen while this thread runs);
                // the item must not stay out forever.
                let error = AdapterError::new(
                    AdapterErrorKind::ParseError,
                    "no worker is left to run platform work",
                );
                let failed = engine.executor().failed(&item, error);
                for (call, answer) in engine.finished(failed) {
                    reply(shared, &mut replies, call, answer);
                }
            }
        }
        if let Some(job) = turn.job
            && let Err(mpsc::SendError(job)) = jobs.send(job)
        {
            // Nothing was sent, but the reservation stands; never retried.
            engine.delivered(job, Delivery::Unknown);
        }
        for (call, answer) in turn.answers {
            reply(shared, &mut replies, call, answer);
        }
        *lock(&shared.stats) = engine.stats();

        let wait = match stop_at {
            Some(until) => {
                let now = Instant::now();
                let Some(wait) = engine.stopping_wait().filter(|_| now < until) else {
                    break;
                };
                wait.min(until.saturating_duration_since(now))
            }
            None => engine.idle_wait(),
        };
        let first = match inbox.recv_timeout(wait) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                // Every client and the handle are gone: stop now.
                if stop_at.is_none() {
                    for (call, answer) in engine.stop_planning() {
                        reply(shared, &mut replies, call, answer);
                    }
                    stop_at = Some(Instant::now());
                }
                None
            }
        };
        let more = std::iter::from_fn(|| inbox.try_recv().ok()).take(MAX_BATCH);
        for msg in first.into_iter().chain(more) {
            match msg {
                Msg::Call {
                    call,
                    caller,
                    envelope,
                    reply: to,
                } => {
                    replies.insert(call, to);
                    if let Some(answer) = engine.request(call, &caller, envelope) {
                        reply(shared, &mut replies, call, answer);
                    }
                }
                Msg::Done(result) => {
                    for (call, answer) in engine.finished(result) {
                        reply(shared, &mut replies, call, answer);
                    }
                }
                Msg::Delivered(job, delivery) => engine.delivered(job, delivery),
                Msg::Wake => engine.on_wake(),
                Msg::NetworkChange => engine.on_network_change(),
                Msg::BeginStop => engine.begin_stop(),
                Msg::Stop { until } => {
                    if stop_at.is_none() {
                        for (call, answer) in engine.stop_planning() {
                            reply(shared, &mut replies, call, answer);
                        }
                    }
                    stop_at = Some(stop_at.map_or(until, |at| at.min(until)));
                }
            }
        }
    }
    let (in_time, answers) = engine.shut_down();
    for (call, answer) in answers {
        reply(shared, &mut replies, call, answer);
    }
    in_time
}

/// Whether `request` changes a notification setting the user chose: the
/// switch, the device name, the Chat ID or the bot token. The templates a
/// window hands over on its own are not one of them.
fn notification_setting(request: &Request) -> bool {
    matches!(
        request,
        Request::UpdateNotificationSettings { .. }
            | Request::SetBotToken { .. }
            | Request::ClearBotToken
            | Request::ImportSettings { .. }
    )
}

/// The notification settings a change of which is written to the event log
/// as `settings_changed` about no tunnel (the log tab's 系統事件, Codex
/// P2-N9). Compared only; the line says nothing of what they are, and the
/// token is only whether one is stored.
#[derive(PartialEq)]
struct NotificationSeen {
    enabled: bool,
    device_name: DeviceName,
    chat_id: Option<ChatId>,
    token: CredentialState,
}

impl NotificationSeen {
    fn of(status: &NotificationStatus) -> Self {
        Self {
            enabled: status.enabled,
            device_name: status.device_name.clone(),
            chat_id: status.chat_id.clone(),
            token: status.token,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::Path;

    use super::*;

    fn named(link: &str) -> Option<String> {
        zone_of_localtime(
            Some(Path::new(link)),
            || Err(io::ErrorKind::NotFound.into()),
        )
        .and_then(|zone| zone.iana_name().map(str::to_owned))
    }

    /// The forms `/etc/localtime` links take on Ubuntu, macOS and others,
    /// read each time without jiff's cache (owner decision of 2026-11-19).
    #[test]
    fn the_zone_is_the_one_localtime_links_to() {
        assert_eq!(
            named("../usr/share/zoneinfo/Asia/Taipei").as_deref(),
            Some("Asia/Taipei")
        );
        assert_eq!(
            named("/usr/share/zoneinfo/Europe/Berlin").as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            named("/var/db/timezone/zoneinfo/America/New_York").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(
            named("/usr/share/zoneinfo/posix/Asia/Tokyo").as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(
            named("/usr/share/zoneinfo/right/UTC").as_deref(),
            Some("UTC")
        );
    }

    /// A link to a name the database lacks, or no link, falls back to the
    /// file's TZif data; data that is not TZif gives nothing.
    #[test]
    fn without_a_known_name_the_file_is_read_and_may_give_nothing() {
        assert_eq!(named("/usr/share/zoneinfo/Nowhere/Atlantis"), None);
        assert_eq!(named("/etc/somewhere-else"), None);
        let read = std::cell::Cell::new(false);
        let zone = zone_of_localtime(None, || {
            read.set(true);
            Ok(b"not a TZif file".to_vec())
        });
        assert!(read.get());
        assert!(zone.is_none());
    }

    /// A host name that cannot be read is not an error: `None`.
    #[test]
    fn the_host_name_is_text_when_there_is_one() {
        if let Some(name) = os_host_name() {
            assert!(!name.contains('\u{0}'));
        }
    }

    /// Blank is judged before a long name is cut, so spaces never become a
    /// name of spaces and `...`; control characters become spaces; a real
    /// name keeps its spaces and is cut as a device name is.
    #[test]
    fn a_host_name_is_fitted_after_it_is_judged_blank() {
        for blank in ["", "   ", &" ".repeat(65), "\t\n", &"\u{7}".repeat(70)] {
            assert_eq!(host_name_as_device_name(blank), None, "{blank:?}");
        }
        assert_eq!(
            host_name_as_device_name("office-pc").unwrap().as_str(),
            "office-pc"
        );
        assert_eq!(
            host_name_as_device_name(" lab\tpc ").unwrap().as_str(),
            " lab pc "
        );
        let long = host_name_as_device_name(&"x".repeat(80)).unwrap();
        assert_eq!(long.as_str().chars().count(), 64);
        assert!(long.as_str().ends_with("..."));
    }

    /// A TZif file of one zone at +08:00, as `/etc/localtime` may hold when
    /// it is a copy rather than a link.
    fn tzif_plus_eight() -> Vec<u8> {
        let mut data = b"TZif".to_vec();
        data.push(0); // version 1
        data.extend_from_slice(&[0; 15]);
        // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        for count in [0u32, 0, 0, 0, 1, 4] {
            data.extend_from_slice(&count.to_be_bytes());
        }
        data.extend_from_slice(&28_800i32.to_be_bytes()); // utoff
        data.push(0); // isdst
        data.push(0); // desigidx
        data.extend_from_slice(b"CST\0");
        data
    }

    #[test]
    fn a_copied_tzif_file_gives_its_zone() {
        let zone = zone_of_localtime(None, || Ok(tzif_plus_eight())).unwrap();
        let now = jiff::Timestamp::from_second(1_767_225_600).unwrap();
        assert_eq!(zone.to_offset(now).seconds(), 8 * 3600);
        // A link to a name the database lacks falls back to the same data.
        let zone = zone_of_localtime(
            Some(Path::new("/usr/share/zoneinfo/Nowhere/Atlantis")),
            || Ok(tzif_plus_eight()),
        )
        .unwrap();
        assert_eq!(zone.to_offset(now).seconds(), 8 * 3600);
        // A file that cannot be read, or is not TZif, gives nothing.
        assert!(zone_of_localtime(None, || Err(io::ErrorKind::PermissionDenied.into())).is_none());
        assert!(zone_of_localtime(None, || Ok(b"not a zone file".to_vec())).is_none());
        assert!(zone_of_localtime(None, || Ok(Vec::new())).is_none());
    }

    /// A zone read from TZif contents has no name: messages give its offset
    /// alone and the snapshot no effective zone, never a made-up name.
    #[test]
    fn a_copied_zone_is_unnamed() {
        let zone = zone_of_localtime(None, || Ok(tzif_plus_eight())).unwrap();
        let name = zone.iana_name().unwrap();
        assert!(TimeZoneName::new(name).is_err(), "{name}");
    }

    /// A file of its own for each test, removed when dropped.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn with(name: &str, data: &[u8]) -> Self {
            let path =
                std::env::temp_dir().join(format!("wgal-zone-{}-{name}", std::process::id()));
            std::fs::write(&path, data).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The reader takes a file of up to the limit whole, refuses a larger
    /// one with `InvalidData` rather than cutting it, and passes on the
    /// error of one it cannot open.
    #[test]
    fn a_zone_file_is_read_up_to_its_limit() {
        const { assert!(MAX_ZONE_FILE_BYTES >= 64 * 1024) };
        let limit = usize::try_from(MAX_ZONE_FILE_BYTES).unwrap();
        let small = Scratch::with("small", &tzif_plus_eight());
        assert_eq!(read_zone_file(&small.0).unwrap(), tzif_plus_eight());
        let full = Scratch::with("full", &vec![0; limit]);
        assert_eq!(read_zone_file(&full.0).unwrap().len(), limit);
        let over = Scratch::with("over", &vec![0; limit + 1]);
        assert_eq!(
            read_zone_file(&over.0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let gone = std::env::temp_dir().join(format!("wgal-zone-{}-gone", std::process::id()));
        assert_eq!(
            read_zone_file(&gone).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        // Through the reader, a TZif file gives its zone and a larger one none.
        let zone = zone_of_localtime(None, || read_zone_file(&small.0)).unwrap();
        let now = jiff::Timestamp::from_second(1_767_225_600).unwrap();
        assert_eq!(zone.to_offset(now).seconds(), 8 * 3600);
        assert!(zone_of_localtime(None, || read_zone_file(&over.0)).is_none());
    }

    /// Which `TZ` values name a file read each time, and which name a zone
    /// fixed for the life of the process (left to jiff).
    #[test]
    fn a_tz_that_names_a_file_is_read_as_one() {
        for (tz, file) in [
            (":/etc/localtime", Some("/etc/localtime")),
            ("/etc/localtime", Some("/etc/localtime")),
            ("/srv/zones/office", Some("/srv/zones/office")),
            ("zones/office", Some("zones/office")),
            ("Asia/Taipei", None),
            (":Asia/Taipei", None),
            ("/usr/share/zoneinfo/Asia/Taipei", None),
            (":../zoneinfo/America/New_York", None),
            ("UTC", None),
            ("", None),
            (":", None),
            ("CST-8", None),
            ("EST5EDT,M3.2.0/2,M11.1.0/2", None),
        ] {
            assert_eq!(tz_file(tz), file, "{tz:?}");
        }
    }

    /// `gethostname` may panic on Windows: the wrapper gives `None` then,
    /// and the name otherwise, bytes that are not UTF-8 replaced. (A build
    /// whose panics abort does not compile: see `host_name_from`.)
    #[test]
    fn a_host_name_reader_that_panics_gives_none() {
        assert_eq!(host_name_from(|| panic!("as gethostname may")), None);
        assert_eq!(
            host_name_from(|| std::ffi::OsString::from("office-pc")).as_deref(),
            Some("office-pc")
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let name = host_name_from(|| std::ffi::OsString::from_vec(b"lab\xffpc".to_vec()));
            assert_eq!(name.as_deref(), Some("lab\u{FFFD}pc"));
        }
    }
}
