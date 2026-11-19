//! What the watchdog persists (plan §9.1) and the storage boundary.
//!
//! The core decides *what* is written and *when*; `wgal-runtime` decides how
//! (atomic replace, previous copy, single writer). Runtime observations such
//! as interface names, indexes and probe results are never persisted.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use wgal_protocol::{
    BoundedVec, DisplayName, FirstRecovery, HostSettings, IncidentId, MAX_OBSERVE_TARGETS,
    NotificationState, ProbeTarget, ProbeTiming, RecoveryRetry, TunnelId, UserIntent,
};

/// Version of [`PersistedState`]. Raise it for any change an older watchdog
/// would misread, and teach the loader to migrate from the previous one.
pub const SCHEMA_VERSION: u32 = 1;

/// Everything the watchdog must remember across restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedState {
    pub schema_version: u32,
    /// Identifies this installation in notifications. Not a secret.
    pub installation_id: String,
    /// Sequence for incident IDs; never reused.
    pub next_incident: u64,
    /// Monitoring of every tunnel is paused (`Request::SetMonitoringPaused`).
    #[serde(default)]
    pub monitoring_paused: bool,
    /// Worker threads and time zone (`Request::UpdateHostSettings`). The
    /// core keeps them for the runtime and does not interpret them. Missing
    /// in files written before wave three: automatic, the system's zone.
    #[serde(default)]
    pub host: HostSettings,
    pub tunnels: Vec<TunnelRecord>,
    /// Agents may use WGAL through `wgal-mcp` (`Request::SetMcpEnabled`,
    /// owner decision of 2026-11-04). Missing before 1.2.0: off.
    #[serde(default)]
    pub mcp_enabled: bool,
    /// Agents may also read the chat ID and change the device name, chat ID
    /// and bot token (`Request::SetMcpTelegramEnabled`, owner decision of
    /// 2026-11-06). Missing before 1.7.0: off.
    #[serde(default)]
    pub mcp_telegram_enabled: bool,
}

impl PersistedState {
    pub fn new(installation_id: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            installation_id,
            next_incident: 1,
            monitoring_paused: false,
            host: HostSettings::default(),
            tunnels: Vec::new(),
            mcp_enabled: false,
            mcp_telegram_enabled: false,
        }
    }
}

/// Something other than WGAL changed a tunnel the user had decided about
/// (plan §6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum External {
    /// Stopped outside WGAL while managed and wanted connected.
    StoppedExternally,
    /// Started or kept running outside WGAL after the user disconnected it.
    ExternallyControlled,
}

/// One registered tunnel. `binding` is the manager's own name for it and is
/// validated again as a `TunnelBinding` when loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelRecord {
    pub tunnel_id: TunnelId,
    pub binding: String,
    pub display_name: DisplayName,
    pub intent: UserIntent,
    pub recovery_target: Option<ProbeTarget>,
    pub observe_targets: BoundedVec<ProbeTarget, MAX_OBSERVE_TARGETS>,
    /// How the tunnel is probed and when its failures count as a fault.
    /// Missing in files written before wave three, which get the defaults:
    /// the plan's fixed timing.
    #[serde(default)]
    pub probe: ProbeTiming,
    /// How automatic recovery tries again after the first round (owner
    /// decision of 2026-11-04). Missing in files written before 1.2.0,
    /// which get the default: 100 more rounds, one a minute.
    #[serde(default)]
    pub retry: RecoveryRetry,
    /// Whether this tunnel's incidents may send the formal notification,
    /// under the global switch. Missing in older files: on.
    #[serde(default = "enabled")]
    pub notifications_enabled: bool,
    /// The recovery target has answered a bound, route-checked probe at least
    /// once. Auto recovery cannot be enabled before (plan §5.1, T10).
    pub baseline_passed: bool,
    pub external: Option<External>,
    /// The manager no longer has the tunnel. Only registering it again clears
    /// this.
    pub binding_lost: bool,
    /// Raised by every user operation and takeover change, so actions and
    /// results planned under an older generation are discarded.
    pub generation: u64,
    pub incident: Option<IncidentRecord>,
    /// An operation that ends by starting the tunnel was accepted and has not
    /// finished. Stored before its stop is sent, so a watchdog that restarts
    /// in between finds the tunnel down on purpose and finishes the start
    /// instead of leaving it stopped.
    #[serde(default)]
    pub resume: Option<ResumeOp>,
    /// Wall time `resume` stops counting, when it is kept only until a
    /// deadline: a late stop or start of WGAL's may land until then. A restart
    /// keeps this deadline and never grants a new one; past it, the record is
    /// dropped. Written when the state is saved and read back on load only.
    #[serde(default)]
    pub resume_until_unix_ms: Option<i64>,
    /// How the event log names this tunnel: `t-` and 8 random hex digits,
    /// drawn when the tunnel is registered (or when a record from before this
    /// field is loaded) and kept for good. It is unrelated to the tunnel ID,
    /// binding and name, so a log that leaves the host does not reveal them
    /// (plan §9.3; review of wave two, P3-1). Missing in older files.
    #[serde(default)]
    pub log_label: Option<String>,
    /// The `recovered` messages of incidents automatic recovery worked on
    /// that closed (owner decision of 2026-11-04), oldest first: each one
    /// still to be sent or in flight, and the last one whatever became of
    /// it. A newer incident never replaces or skips an older one's message
    /// (Codex P2-N20); at most [`MAX_RECOVERED_NOTICES`] are kept, and when
    /// more are waiting the oldest one not in flight is dropped. Missing
    /// before 1.2.0.
    #[serde(default)]
    pub recovered_notices: Vec<RecoveredNotice>,
    /// An import asked for automatic recovery before the recovery target
    /// passed its baseline check (owner decision of 2026-11-04). Missing
    /// before 1.2.0.
    #[serde(default)]
    pub auto_recovery_after_baseline: Option<PendingAutoRecovery>,
}

/// Automatic recovery an import asked for, waiting for the baseline: it
/// turns on, with "start when stopped externally" as the file has it, at
/// the first baseline that passes before `until_unix_ms`; after that it
/// stays off. The user's own switch, or a new recovery target, drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAutoRecovery {
    pub start_when_stopped_externally: bool,
    pub until_unix_ms: i64,
}

/// How far one notification got (plan §7.3), for the ones added in 1.2.0;
/// the alert keeps its fields in [`IncidentRecord`] as before.
/// `cancel_requested`: cancelled while a send was in flight, so whatever
/// that send reports it is never eligible again. `message_id`: Telegram's,
/// once accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeRecord {
    pub state: NotificationState,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub message_id: Option<i64>,
}

/// How many `recovered` messages a tunnel keeps (see
/// `TunnelRecord::recovered_notices`).
pub const MAX_RECOVERED_NOTICES: usize = 16;

/// The `recovered` message of a closed incident: which incident, when it
/// opened and when traffic came back, and how far sending it got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveredNotice {
    pub incident_id: IncidentId,
    pub opened_at_unix_ms: i64,
    pub recovered_at_unix_ms: i64,
    pub notice: NoticeRecord,
}

fn enabled() -> bool {
    true
}

/// An unfinished operation to complete after a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeOp {
    /// An automatic recovery round.
    Recovery,
    /// A user's connect or reconnect.
    Connect,
    /// A user's disconnect whose stop has not been confirmed yet. It is
    /// retried until the tunnel is seen down (follow-up review F2, F9).
    Disconnect,
}

/// The open incident of a tunnel (plan §7.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentRecord {
    pub incident_id: IncidentId,
    pub opened_at_unix_ms: i64,
    pub first_recovery: FirstRecovery,
    /// Automatic recovery rounds started after the first one.
    pub backoff_step: u32,
    /// `backoff_step` when the retry count last started: 0 when the
    /// incident opened, the step of the moment a manual connect or reconnect
    /// started it again. `backoff_step - counted_from_step` rounds count
    /// against the tunnel's `RecoveryRetry` limit. Missing before 1.2.0: 0.
    #[serde(default)]
    pub counted_from_step: u32,
    /// The retry limit is used up: no automatic round follows in this
    /// incident until a manual connect or reconnect, or a change of the
    /// tunnel's `RecoveryRetry` that allows more. Missing before 1.2.0.
    #[serde(default)]
    pub retries_exhausted: bool,
    /// The message that a limited retry count is used up (owner decision
    /// of 2026-11-04): eligible when an automatic round used up a count of
    /// at least one; with a limit of 0 the alert says it instead. Missing
    /// before 1.2.0.
    #[serde(default)]
    pub exhausted_notice: NoticeRecord,
    /// The last thing that may have brought the tunnel back was the user's
    /// connect or reconnect, so its recovery is not reported as automatic.
    /// Cleared when an automatic round starts. Missing before 1.2.0.
    #[serde(default)]
    pub manual_last: bool,
    /// The round the tunnel's `resume` record would start again was counted
    /// against the retry limit and given back before it sent anything:
    /// starting it from that record counts it again, in this process or
    /// after a restart (Codex P2-N18, P2-N29). Missing before 1.2.0.
    #[serde(default)]
    pub resume_uncounted: bool,
    /// Wall time the incident's current wait for its next automatic round
    /// started: the end of its last automatic round, or a manual connect or
    /// reconnect that failed and put the tunnel back on its backoff. Kept
    /// while no retry is scheduled too, so a restart measures every wait
    /// from it with the interval of the time (Codex P2-N57, P2-N58).
    /// Missing before 1.2.0, when the end of the last round stands in.
    #[serde(default)]
    pub wait_from_unix_ms: Option<i64>,
    /// Wall time the last automatic round ended, for records.
    pub last_round_end_unix_ms: Option<i64>,
    /// Wall time the next automatic round is due. On restart the remaining
    /// wait is recomputed and never exceeds the current backoff.
    pub next_retry_unix_ms: Option<i64>,
    pub notification: NotificationState,
    /// Set by the notification package once Telegram accepted the message.
    pub notification_message_id: Option<i64>,
    /// The notification was cancelled (disconnect, notifications turned off)
    /// while a send was in flight. Whatever that send reports, the incident is
    /// never eligible again.
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Why state could not be stored or loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Io(io::ErrorKind),
    /// The store refuses to write, for example because a path is a symlink,
    /// another watchdog holds the lock, or loading failed and the files on
    /// disk must not be overwritten.
    Refused(&'static str),
    /// Stored data could not be understood.
    Corrupt(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "storage I/O error: {kind}"),
            Self::Refused(why) => write!(f, "storage refused: {why}"),
            Self::Corrupt(why) => write!(f, "stored state is corrupt: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind())
    }
}

/// Durable storage for [`PersistedState`]. `save` returns only once the state
/// is durable; a failure must leave the previously saved state intact.
pub trait StateStore: Send + fmt::Debug {
    fn save(&mut self, state: &PersistedState) -> Result<(), StoreError>;
}

/// In-memory [`StateStore`] for tests and tools. Clones share the saved state
/// and the failure switch, so a test can keep one to inspect or break the
/// store after handing another to the watchdog.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    saved: Arc<Mutex<Option<PersistedState>>>,
    failing: Arc<AtomicBool>,
    /// Saves left before every save fails (see [`MemoryStore::fail_after`]).
    left: Arc<Mutex<Option<u32>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last state saved successfully.
    pub fn saved(&self) -> Option<PersistedState> {
        self.saved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// While set, every save fails as a full disk would. Clearing it also
    /// ends [`Self::fail_after`].
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
        if !failing {
            *self.left.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// The next `saves` saves succeed, then every one fails, as a disk that
    /// fills up part way through a change would.
    pub fn fail_after(&self, saves: u32) {
        *self.left.lock().unwrap_or_else(PoisonError::into_inner) = Some(saves);
    }
}

impl StateStore for MemoryStore {
    fn save(&mut self, state: &PersistedState) -> Result<(), StoreError> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(StoreError::Io(io::ErrorKind::StorageFull));
        }
        if let Some(left) = self
            .left
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            if *left == 0 {
                return Err(StoreError::Io(io::ErrorKind::StorageFull));
            }
            *left -= 1;
        }
        *self.saved.lock().unwrap_or_else(PoisonError::into_inner) = Some(state.clone());
        Ok(())
    }
}
