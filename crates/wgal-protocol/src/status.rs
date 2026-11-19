use serde::{Deserialize, Serialize};

use crate::adapter::{AdapterCapabilities, AdapterErrorKind};
use crate::bounded::BoundedVec;
use crate::ids::{IncidentId, TunnelId};
use crate::notify::NotificationStatus;
use crate::probe::{MAX_OBSERVE_TARGETS, TargetStatus};
use crate::settings::{HostStatus, ProbeTiming, RecoveryRetry};
use crate::text::{Detail, DisplayName};

/// Most tunnels one snapshot can carry. With every other field bounded too, the
/// largest valid snapshot still fits in one message (see `tests/limits.rs`).
pub const MAX_TUNNELS: usize = 16;

/// Point-in-time view of every registered tunnel, produced by the watchdog.
/// The GUI renders it as-is and keeps no tunnel state of its own.
///
/// `monitoring_paused` is the host-wide pause (see
/// `Request::SetMonitoringPaused`); while it is set every tunnel shows
/// `monitoring_off`. `host` reports the host settings: worker threads and time
/// zone. `notification` reports the notification settings and the latest
/// test, never the bot token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Snapshot {
    #[cfg_attr(test, ts(type = "number"))]
    pub generated_at_unix_ms: i64,
    pub storage: StorageHealth,
    pub capabilities: AdapterCapabilities,
    pub monitoring_paused: bool,
    pub host: HostStatus,
    pub notification: NotificationStatus,
    #[cfg_attr(test, ts(as = "Vec<TunnelStatus>"))]
    pub tunnels: BoundedVec<TunnelStatus, MAX_TUNNELS>,
}

/// One registered tunnel, with each state layer reported separately (plan
/// §6.1): what the user wants, whether WGAL may control the tunnel, what the
/// OS reports, whether traffic flows, where recovery stands, and the open
/// incident. `light` is computed by the watchdog from these; the GUI shows it
/// and never derives its own.
///
/// `light_reason` says why the light is what it is, and `light_since_unix_ms`
/// when the light or reason last changed. `deadline_unix_ms` is set while the
/// light is yellow: when the connect, verify or confirmation period ends.
/// `next_retry_unix_ms` is set while a red tunnel waits for its next automatic
/// recovery attempt.
///
/// `last_verified_unix_ms` is when a bound, route-checked probe of the
/// recovery target last got an answer in this watchdog process (the "last
/// success" column). `auto_recovery_allowed` says whether turning auto
/// recovery on would be accepted now: the recovery target passed its baseline
/// check and the tunnel can be controlled. `notifications_enabled` is the
/// tunnel's own notification switch, under the global one in
/// `NotificationStatus`.
///
/// `probe` is the tunnel's probe timing (see `ProbeTiming`), and
/// `freshness_secs` how old a probe result may be and still count; the GUI
/// uses it instead of a fixed 35 s. `consecutive_failures` is the number of
/// failed rounds in the current streak; against `retries + 1` the GUI shows
/// how close the tunnel is to a confirmed fault. `retry` is how automatic
/// recovery tries again after its first round (see `RecoveryRetry`; the
/// default from a watchdog older than 1.2.0, which retried on a fixed
/// backoff instead). `recovered_notification` is the message that the
/// last incident automatic recovery worked on is over (see
/// `RecoveredNotification`; `null` until one closed, and before 1.2.0).
/// `auto_recovery_pending` says an import asked for automatic recovery and
/// it turns on once the recovery target passes its baseline check, if that
/// happens within ten minutes of the import.
///
/// Each tunnel has at most one `recovery_target`, the only target whose
/// failure can trigger recovery. `observe_targets` are displayed only: one of
/// them going down never restarts the tunnel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TunnelStatus {
    pub tunnel_id: TunnelId,
    pub display_name: DisplayName,
    pub light: Light,
    pub light_reason: LightReason,
    #[cfg_attr(test, ts(type = "number"))]
    pub light_since_unix_ms: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub deadline_unix_ms: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub next_retry_unix_ms: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub last_verified_unix_ms: Option<i64>,
    pub intent: UserIntent,
    pub auto_recovery_allowed: bool,
    pub notifications_enabled: bool,
    pub takeover: Takeover,
    pub os_state: OsState,
    pub data_health: DataHealth,
    pub recovery: RecoveryPhase,
    pub recovery_target: Option<TargetStatus>,
    #[cfg_attr(test, ts(as = "Vec<TargetStatus>"))]
    pub observe_targets: BoundedVec<TargetStatus, MAX_OBSERVE_TARGETS>,
    pub probe: ProbeTiming,
    pub freshness_secs: u32,
    pub consecutive_failures: u32,
    pub fault: Option<Fault>,
    pub incident: Option<IncidentStatus>,
    #[serde(default)]
    pub retry: RecoveryRetry,
    #[serde(default)]
    pub recovered_notification: Option<RecoveredNotification>,
    #[serde(default)]
    pub auto_recovery_pending: bool,
}

/// The status light (plan §6.1). Green only for a fresh, interface-bound probe
/// success; yellow for a time-limited connect, verify or confirmation period;
/// red for a confirmed fault, failed recovery, backoff, or control or path
/// error; gray for a tunnel that is not enabled. A watchdog that has gone
/// silent is shown by the GUI as an overlay, never as one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Light {
    Green,
    Yellow,
    Red,
    Gray,
}

/// Why the light is what it is. The watchdog picks the light and the reason
/// together; the light usually shown with each reason is listed below, but the
/// GUI displays both as given and never infers one from the other.
///
/// - Green: `verified`, a fresh probe bound to this tunnel's interface, with
///   matching route evidence, got a reply.
/// - Yellow, always with a deadline: `starting`, `reconnecting`, `verifying`
///   (traffic check after a start, at most 45 s), `confirming` (recent
///   failures, not yet `retries + 1` rounds over `min_fault_secs` under the
///   tunnel's probe timing), `grace_period` (after the watchdog starts, the
///   host wakes or the network changes).
/// - Red: `fault_confirmed` (recovery queued, or auto recovery is off),
///   `recovery_failed` (backing off, next retry set), `control_failed` (a
///   start or stop could not be carried out), `path_unverified` (the probe
///   cannot be bound to the interface or the route is ambiguous; auto recovery
///   stays off), `route_conflict` (the target leaves through another
///   interface), `probe_unavailable` (the probe tool is missing, not permitted
///   or unparseable; the target is not assumed down), `baseline_failed` (the
///   recovery target has not passed its first check), `binding_lost` (the
///   official manager no longer has this tunnel; register it again),
///   `stopped_externally` (stopped outside WGAL while the user wants it
///   connected), `externally_started` (started outside WGAL after the user
///   disconnected it), `local_network_offline`, `status_unknown` (the OS
///   state could not be read).
/// - Gray: `disconnected_by_user`, `not_connected` (not running and not
///   managed by WGAL), `monitoring_off` (paused for this tunnel or host-wide),
///   `not_configured` (no recovery target yet).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum LightReason {
    Verified,
    Starting,
    Reconnecting,
    Verifying,
    Confirming,
    GracePeriod,
    FaultConfirmed,
    RecoveryFailed,
    RetriesExhausted,
    ControlFailed,
    PathUnverified,
    RouteConflict,
    ProbeUnavailable,
    BaselineFailed,
    BindingLost,
    StoppedExternally,
    ExternallyStarted,
    LocalNetworkOffline,
    StatusUnknown,
    DisconnectedByUser,
    NotConnected,
    MonitoringOff,
    NotConfigured,
}

/// What the user asked for. Only the user changes this; recovery never does.
///
/// `start_when_stopped_externally` is the advanced option to start the tunnel
/// again when something outside WGAL stopped it (plan §6.4). It is off by
/// default, and the GUI must warn before it is turned on that a deliberate
/// stop in the official app will then be undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct UserIntent {
    pub desired: DesiredState,
    pub monitoring_enabled: bool,
    pub auto_recovery_enabled: bool,
    pub start_when_stopped_externally: bool,
}

/// Whether WGAL may control the tunnel (plan §6.4, §8).
///
/// - `observe_only`: registered and monitored; WGAL never starts or stops it
///   on its own. Every tunnel starts here.
/// - `managed`: the user handed control to WGAL after the recovery target
///   passed its baseline check.
/// - `stopped_externally`: it was managed, then stopped by something other
///   than WGAL. WGAL does not start it again until the user takes it back,
///   unless `start_when_stopped_externally` is on.
/// - `externally_controlled`: something else (On-Demand, another tool) keeps
///   changing its state. WGAL reports what the OS shows and does not fight it.
/// - `binding_lost`: the official manager no longer has the tunnel. WGAL never
///   recreates it; the user registers it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
    ObserveOnly,
    Managed,
    StoppedExternally,
    ExternallyControlled,
    BindingLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Connected,
    /// The user disconnected on purpose. Recovery must not reconnect it.
    Disconnected,
}

/// What the official WireGuard manager reports. Says nothing about whether
/// traffic actually flows; see [`DataHealth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum OsState {
    Connected,
    Disconnected,
    Transitioning,
    Unknown,
}

/// Whether traffic through the tunnel was recently verified, judged from the
/// recovery target only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum DataHealth {
    Healthy,
    /// Recent failures, not yet confirmed as a fault.
    Suspect,
    /// Fault confirmed.
    Failed,
    /// No usable evidence either way.
    Unknown,
    /// The last evidence is too old to rely on.
    Stale,
}

/// Where automatic recovery of this tunnel stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    Idle,
    Queued,
    Stopping,
    Starting,
    Verifying,
    Backoff,
    /// Recovery is not allowed right now, for example because the path is
    /// unverified or the tunnel was stopped externally.
    Blocked,
}

/// Why the tunnel is in trouble, as a category plus a [`Detail`] message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Fault {
    pub kind: AdapterErrorKind,
    pub message: Detail,
}

/// The tunnel's open incident (plan §7.2, §7.3): one confirmed fault, from
/// confirmation until traffic has been stably healthy for the closing period.
/// A new fault after that opens a new incident with a new ID.
///
/// `backoff_step` counts automatic recovery attempts after the first one.
/// `retries_used` counts those against the tunnel's `RecoveryRetry` limit
/// (since the incident opened or since a manual connect or reconnect
/// started the count again), and `retries_exhausted` says the limit is
/// used up, so no automatic round follows in this incident (both from
/// watchdog 1.2.0; 0 and `false` before). `notification` is the alert's
/// state and `exhausted_notification` that of the message saying the limit
/// is used up (`not_eligible` before 1.2.0). `stable_since_unix_ms` is set
/// while traffic is healthy again but the closing period has not passed
/// yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct IncidentStatus {
    pub incident_id: IncidentId,
    #[cfg_attr(test, ts(type = "number"))]
    pub opened_at_unix_ms: i64,
    pub first_recovery: FirstRecovery,
    pub backoff_step: u32,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub stable_since_unix_ms: Option<i64>,
    pub notification: NotificationState,
    #[serde(default)]
    pub retries_used: u32,
    #[serde(default)]
    pub retries_exhausted: bool,
    #[serde(default)]
    pub exhausted_notification: NotificationState,
}

/// Outcome of the incident's first automatic recovery round. `not_attempted`
/// means auto recovery was off, which is never reported as a failed recovery;
/// `not_executed` means the stop or start itself could not be carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum FirstRecovery {
    NotAttempted,
    InProgress,
    Succeeded,
    Failed,
    NotExecuted,
}

/// Where one of an incident's notifications stands (plan §7.3). `reserved`
/// is persisted before the request is sent; a `reserved` found at startup
/// becomes `unknown` and is never sent again automatically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NotificationState {
    #[default]
    NotEligible,
    Pending,
    Reserved,
    Sent,
    Rejected,
    Unknown,
    Cancelled,
}

/// Which of an incident's messages a notification is (owner decision of
/// 2026-11-04, beside plan §7.2's one alert): `alert`, the first automatic
/// recovery failed or could not be carried out; `retries_exhausted`, a
/// limited retry count is used up and automatic recovery stopped;
/// `recovered`, traffic came back and stayed healthy through the closing
/// period. Each is sent at most once per incident.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    #[default]
    Alert,
    RetriesExhausted,
    Recovered,
}

/// The `recovered` notification of an incident that has closed, kept with
/// the tunnel until the next one replaces it: `incident_id` the closed
/// incident, `notification` how far sending it got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct RecoveredNotification {
    pub incident_id: IncidentId,
    pub notification: NotificationState,
}

/// Whether the watchdog can persist intent and incidents (plan §9.1). While
/// `failed`, it keeps monitoring but starts no automatic recovery and sends no
/// notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum StorageHealth {
    Ok,
    Failed,
}
