use serde::{Deserialize, Serialize};

use crate::adapter::{DiscoveredTunnel, MAX_DISCOVERED};
use crate::bounded::BoundedVec;
use crate::document::{ImportOptions, ImportResult, SettingsDocument};
use crate::ids::{RequestId, TunnelId};
use crate::log::{EventEntry, EventFilter, MAX_EVENT_PAGE};
use crate::notify::{BotToken, ChatId};
use crate::probe::{MAX_OBSERVE_TARGETS, ProbeTarget};
use crate::settings::{HostSettings, ProbeTiming, RecoveryRetry};
use crate::status::Snapshot;
use crate::template::{LanguageTag, NotificationTemplates};
use crate::text::{Detail, DeviceName};

/// Version of this contract. Bump it for any change an older peer would misread;
/// peers reject envelopes that carry a different version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Upper bound, in bytes, for one serialized request or response. Enforced by
/// the functions in [`crate::codec`].
pub const MAX_MESSAGE_BYTES: u32 = 64 * 1024;

/// The watchdog's IPC endpoint on Windows (plan §3.2): a named pipe that only
/// local clients can open, carrying frames from [`crate::codec::write_frame`].
/// A fixed name, never configurable; the watchdog checks each client's OS
/// identity itself.
pub const WINDOWS_PIPE_NAME: &str = r"\\.\pipe\wgal-watchdog";

/// Contract parameters a peer was built with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ProtocolInfo {
    pub protocol_version: u32,
    pub max_message_bytes: u32,
}

impl ProtocolInfo {
    pub const CURRENT: Self = Self {
        protocol_version: PROTOCOL_VERSION,
        max_message_bytes: MAX_MESSAGE_BYTES,
    };
}

/// One request to the watchdog. `origin` says which WGAL program sends it:
/// left out (the window, `gui`), `mcp` by `wgal-mcp` for an agent (owner
/// decision of 2026-11-04). The watchdog refuses every `mcp` request while
/// MCP is off, and writes each change an `mcp` request makes to the log as
/// the agent's. It is the program's own word, not an identity: the caller's
/// identity and the access list decide what may be done, as for the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub origin: Option<Origin>,
}

impl RequestEnvelope {
    /// Which program sent it: the window when it does not say.
    pub fn origin(&self) -> Origin {
        self.origin.unwrap_or_default()
    }
}

/// Which WGAL program sends a request (see [`RequestEnvelope`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Gui,
    Mcp,
}

/// A field that must be given even though it may be `null`: without a
/// function of its own, serde would take a missing `Option` as `null`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

/// Every operation the GUI may ask of the watchdog (plan §3.2).
///
/// This closed set is the allowlist. No variant may carry a command line,
/// executable or file path, or OS service/interface name: tunnels are addressed
/// only by [`TunnelId`], which the watchdog maps to a binding it discovered
/// itself. The watchdog still checks the caller's OS identity and its own
/// authorization rules for every request; being in this enum is not enough.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    GetSnapshot,
    /// Lists tunnels the official manager already has; answered with
    /// [`Response::Discovered`].
    DiscoverTunnels,
    /// Starts monitoring a discovered tunnel, observe-only with auto recovery
    /// off until a recovery target passes its baseline check.
    RegisterTunnel {
        tunnel_id: TunnelId,
    },
    /// Forgets WGAL's registration of the tunnel, and with it everything WGAL
    /// kept for it. Never starts, stops or changes the tunnel or its official
    /// configuration. Refused with `busy` while an operation of WGAL's on the
    /// tunnel is unfinished.
    UnregisterTunnel {
        tunnel_id: TunnelId,
    },
    UpdateMonitorSettings {
        tunnel_id: TunnelId,
        settings: MonitorSettings,
    },
    /// Pauses or resumes monitoring of one tunnel. While it is off, WGAL
    /// probes and reads nothing for the tunnel on its own and takes no
    /// automatic action, so it uses no worker; its registration, targets and
    /// settings stay. The tunnel is never started or stopped because of it,
    /// and user operations still work.
    SetTunnelMonitoring {
        tunnel_id: TunnelId,
        enabled: bool,
    },
    /// Probes the tunnel's targets now. A monitored tunnel's next round
    /// runs at once and is judged as any round. A tunnel that is not
    /// monitored, by its own switch or the host-wide pause, gets one round
    /// from watchdog 1.5.0 (owner decision of 2026-11-06), whose answers
    /// only update each target's `last_probe`: no streak, fault, light or
    /// operation follows from it, and monitoring stays off. An older
    /// watchdog accepts the request and probes nothing then.
    ProbeNow {
        tunnel_id: TunnelId,
    },
    Connect {
        tunnel_id: TunnelId,
    },
    /// Records the intent to stay disconnected before stopping the tunnel, so
    /// pending recovery cannot reconnect it.
    Disconnect {
        tunnel_id: TunnelId,
    },
    Reconnect {
        tunnel_id: TunnelId,
    },
    /// Turning auto recovery off cancels automatic actions only; it neither
    /// disconnects the tunnel nor stops monitoring.
    SetAutoRecovery {
        tunnel_id: TunnelId,
        enabled: bool,
    },
    /// The advanced option to start the tunnel again after something outside
    /// WGAL stopped it. Off by default (plan §6.4).
    SetStartWhenStoppedExternally {
        tunnel_id: TunnelId,
        enabled: bool,
    },
    /// Whether this tunnel's incidents may send the formal notification, when
    /// notifications are on globally (`UpdateNotificationSettings`). On by
    /// default; turning it off cancels the tunnel's pending notification.
    SetTunnelNotifications {
        tunnel_id: TunnelId,
        enabled: bool,
    },
    /// How automatic recovery of this tunnel tries again after its first
    /// round (`RecoveryRetry`; watchdog 1.2.0 and later). Takes effect at
    /// once: a retry already waiting is rescheduled from the end of the last
    /// round, a count already used up resumes if the new limit allows more,
    /// and one that the new limit has used up stops at the next retry.
    SetRecoveryRetry {
        tunnel_id: TunnelId,
        retry: RecoveryRetry,
    },
    /// Turns Telegram notifications on or off and sets what formal messages
    /// call this device and where they go (plan §7.1). Turning them off
    /// cancels every pending notification. The bot token is set separately.
    UpdateNotificationSettings {
        enabled: bool,
        device_name: DeviceName,
        chat_id: Option<ChatId>,
    },
    /// Hands the bot token to the watchdog's secret store. The GUI clears its
    /// copy once sent; snapshots only report a `CredentialState`.
    SetBotToken {
        token: BotToken,
    },
    ClearBotToken,
    /// Hands the watchdog the texts formal and test notifications are
    /// written with, from the GUI's language file (see
    /// [`NotificationTemplates`]). Replaces the set in use, the built-in one
    /// at first, from the next message written, and is kept across
    /// restarts. It sends nothing, and nothing already sent or settled is
    /// sent again. Refused with `storage_unavailable`, keeping the set in
    /// use, if it cannot be stored.
    SetNotificationTemplates {
        templates: NotificationTemplates,
    },
    /// Sets `wgal`'s shared language (owner decision of 2026-11-19;
    /// watchdog 4.0.0 and later): the language the command line writes in
    /// when a run does not name one with `--lang`. `null` clears it, back to
    /// never set, when `wgal` writes English. It is kept with the
    /// notification settings and reported as
    /// `NotificationStatus::cli_language`; it changes nothing the watchdog
    /// does or writes. The field must be given, `null` included. Refused
    /// with `storage_unavailable`, keeping the language in use, if it
    /// cannot be stored, and refused for an `mcp` request: only the user
    /// sets it, with `wgal language`.
    SetCliLanguage {
        #[serde(deserialize_with = "present")]
        language: Option<LanguageTag>,
    },
    /// Pauses or resumes monitoring of every tunnel at once (the tray's
    /// "pause monitoring", plan §8). While paused nothing is probed and no
    /// automatic recovery starts, but no tunnel is started or stopped because
    /// of it, user operations still work, and each tunnel keeps its own
    /// monitoring setting for when monitoring resumes. Not the same as
    /// stopping the background watchdog.
    SetMonitoringPaused {
        paused: bool,
    },
    /// Whether agents may use WGAL through `wgal-mcp` (owner decision of
    /// 2026-11-04; watchdog 1.2.0 and later). Off by default; only the
    /// window turns it on, since every `mcp` request is refused while it is
    /// off. Reported as `HostStatus::mcp_enabled`.
    SetMcpEnabled {
        enabled: bool,
    },
    /// Whether agents may also read the chat ID and change the device name,
    /// chat ID and bot token through `wgal-mcp` (owner decision of
    /// 2026-11-06; watchdog 1.7.0 and later). Off by default, and only the
    /// window turns it on: an `mcp` request that turns it on is refused,
    /// and so is an `mcp` request that changes any of the three while it is
    /// off. It takes effect only while MCP is on, and is kept as it is when
    /// MCP is turned off. The bot token is never given out either way.
    /// Reported as `HostStatus::mcp_telegram_enabled`.
    SetMcpTelegramEnabled {
        enabled: bool,
    },
    /// Sends a test notification through the same background sender and
    /// secret store as formal ones (plan §7.4). The envelope's `request_id`
    /// identifies the test: repeating it does not send again.
    SendTestNotification,
    /// Sets the worker thread count and the time zone. A zone the watchdog's
    /// time zone database does not know is refused with `invalid_setting`.
    UpdateHostSettings {
        settings: HostSettings,
    },
    /// Reads the event log, newest first: at most [`MAX_EVENT_PAGE`] entries
    /// with `seq` below `before_seq` (from the newest when it is `null`)
    /// that `filter` selects. Answered with [`Response::EventLog`].
    ReadEventLog {
        filter: EventFilter,
        #[cfg_attr(test, ts(type = "number | null"))]
        before_seq: Option<u64>,
    },
    /// WGAL's settings as a [`SettingsDocument`] (watchdog 1.2.0 and later),
    /// answered with [`Response::SettingsExport`]. Changes nothing.
    ExportSettings,
    /// Puts a [`SettingsDocument`] back (watchdog 1.2.0 and later): each
    /// tunnel entry matched as `options` say and set like the requests that
    /// set each part, every part judged and authorized on its own, so one
    /// refused part leaves the others. With `options.dry_run` nothing
    /// changes. Answered with [`Response::SettingsImport`].
    ImportSettings {
        document: SettingsDocument,
        options: ImportOptions,
    },
}

/// The targets and probe timing of one tunnel, as the GUI may change them.
/// Changing the recovery target resets its baseline check and turns auto
/// recovery off; changing only the observe targets or the timing does
/// neither. Monitoring itself is switched by `SetTunnelMonitoring`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct MonitorSettings {
    pub recovery_target: Option<ProbeTarget>,
    #[cfg_attr(test, ts(as = "Vec<ProbeTarget>"))]
    pub observe_targets: BoundedVec<ProbeTarget, MAX_OBSERVE_TARGETS>,
    pub probe: ProbeTiming,
}

/// `request_id` is `null` only when answering a request whose ID could not be
/// read, for example one rejected for its size before parsing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: Option<RequestId>,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a response is built once and encoded at once; boxing the snapshot only adds an allocation"
)]
pub enum Outcome {
    Ok(Response),
    Err(ProtocolError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a response is built once and encoded at once; boxing the snapshot only adds an allocation"
)]
pub enum Response {
    /// The operation was accepted. Its effect is reported through later snapshots,
    /// not through this response.
    Accepted,
    Snapshot {
        snapshot: Snapshot,
    },
    Discovered {
        #[cfg_attr(test, ts(as = "Vec<DiscoveredTunnel>"))]
        tunnels: BoundedVec<DiscoveredTunnel, MAX_DISCOVERED>,
    },
    /// One page of the event log, newest first. `more` says whether older
    /// matching entries exist: ask for them with `before_seq` set to the
    /// last entry's `seq`.
    EventLog {
        #[cfg_attr(test, ts(as = "Vec<EventEntry>"))]
        entries: BoundedVec<EventEntry, MAX_EVENT_PAGE>,
        more: bool,
    },
    /// The settings `Request::ExportSettings` asked for.
    SettingsExport {
        document: SettingsDocument,
    },
    /// What `Request::ImportSettings` did, or would do in a dry run.
    SettingsImport {
        result: ImportResult,
    },
}

/// Which kind a request is, by its `op` name: what an `agent_request` log
/// line names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum RequestOp {
    GetSnapshot,
    DiscoverTunnels,
    RegisterTunnel,
    UnregisterTunnel,
    UpdateMonitorSettings,
    SetTunnelMonitoring,
    ProbeNow,
    Connect,
    Disconnect,
    Reconnect,
    SetAutoRecovery,
    SetStartWhenStoppedExternally,
    SetTunnelNotifications,
    SetRecoveryRetry,
    UpdateNotificationSettings,
    SetBotToken,
    ClearBotToken,
    SetMonitoringPaused,
    SetMcpEnabled,
    SetMcpTelegramEnabled,
    SendTestNotification,
    SetNotificationTemplates,
    SetCliLanguage,
    UpdateHostSettings,
    ReadEventLog,
    ExportSettings,
    ImportSettings,
}

impl Request {
    /// The request's kind.
    pub fn op(&self) -> RequestOp {
        match self {
            Self::GetSnapshot => RequestOp::GetSnapshot,
            Self::DiscoverTunnels => RequestOp::DiscoverTunnels,
            Self::RegisterTunnel { .. } => RequestOp::RegisterTunnel,
            Self::UnregisterTunnel { .. } => RequestOp::UnregisterTunnel,
            Self::UpdateMonitorSettings { .. } => RequestOp::UpdateMonitorSettings,
            Self::SetTunnelMonitoring { .. } => RequestOp::SetTunnelMonitoring,
            Self::ProbeNow { .. } => RequestOp::ProbeNow,
            Self::Connect { .. } => RequestOp::Connect,
            Self::Disconnect { .. } => RequestOp::Disconnect,
            Self::Reconnect { .. } => RequestOp::Reconnect,
            Self::SetAutoRecovery { .. } => RequestOp::SetAutoRecovery,
            Self::SetStartWhenStoppedExternally { .. } => RequestOp::SetStartWhenStoppedExternally,
            Self::SetTunnelNotifications { .. } => RequestOp::SetTunnelNotifications,
            Self::SetRecoveryRetry { .. } => RequestOp::SetRecoveryRetry,
            Self::UpdateNotificationSettings { .. } => RequestOp::UpdateNotificationSettings,
            Self::SetBotToken { .. } => RequestOp::SetBotToken,
            Self::ClearBotToken => RequestOp::ClearBotToken,
            Self::SetMonitoringPaused { .. } => RequestOp::SetMonitoringPaused,
            Self::SetMcpEnabled { .. } => RequestOp::SetMcpEnabled,
            Self::SetMcpTelegramEnabled { .. } => RequestOp::SetMcpTelegramEnabled,
            Self::SendTestNotification => RequestOp::SendTestNotification,
            Self::SetNotificationTemplates { .. } => RequestOp::SetNotificationTemplates,
            Self::SetCliLanguage { .. } => RequestOp::SetCliLanguage,
            Self::UpdateHostSettings { .. } => RequestOp::UpdateHostSettings,
            Self::ReadEventLog { .. } => RequestOp::ReadEventLog,
            Self::ExportSettings => RequestOp::ExportSettings,
            Self::ImportSettings { .. } => RequestOp::ImportSettings,
        }
    }

    /// Whether the request only reads: it changes nothing the watchdog
    /// keeps.
    pub fn reads_only(&self) -> bool {
        matches!(
            self,
            Self::GetSnapshot
                | Self::DiscoverTunnels
                | Self::ReadEventLog { .. }
                | Self::ExportSettings
                | Self::ImportSettings {
                    options: ImportOptions { dry_run: true, .. },
                    ..
                }
        )
    }
}

/// A structured rejection: a stable `code` for the GUI to act on, a
/// `reason` that says more precisely why, `null` when the code says it
/// all, and a [`Detail`] `message`.
///
/// The GUI shows the sentence its language file has for the reason, or for
/// the code when there is no reason. The `message` is English text for
/// diagnostics, shown only among the details (plan §1.1: the system's own
/// text is kept in the details), never as the sentence for the user; like
/// any `Detail` it never holds a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub reason: Option<ErrorReason>,
    pub message: Detail,
}

impl ProtocolError {
    /// A rejection with `message` cut to fit a [`Detail`] and its control
    /// characters replaced by spaces ([`Detail::lossy`]).
    pub fn new(code: ErrorCode, reason: Option<ErrorReason>, message: &str) -> Self {
        Self {
            code,
            reason,
            message: Detail::lossy(message),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    MalformedRequest,
    MessageTooLarge,
    /// The caller's verified OS identity may not make this request.
    Unauthorized,
    /// The request did not complete within its deadline.
    Timeout,
    UnknownTunnel,
    /// The tunnel's current state does not allow this request, for example
    /// enabling auto recovery before the recovery target passed its baseline.
    InvalidState,
    /// A setting the watchdog cannot apply, for example a time zone its time
    /// zone database does not know.
    InvalidSetting,
    /// The request needs a durable record (such as a disconnect intent) and
    /// the watchdog cannot write one right now.
    StorageUnavailable,
    Busy,
    Internal,
}

/// Why a request was refused, more precisely than its [`ErrorCode`], so
/// the GUI can show the sentence its language file has for it. A
/// [`ProtocolError`] carries none when the code says it all.
///
/// - Tunnels: `tunnel_not_registered`, `tunnel_not_discovered` (not in the
///   latest discovery), `binding_lost` (the official manager no longer has
///   it), `id_conflict` (the ID names another tunnel now),
///   `too_many_tunnels`, `too_many_discovered`.
/// - Registration: `managed_by_another_tool`, `config_not_protected` (a
///   user without administrator rights could change its configuration),
///   `manager_query_failed` (the official manager could not be asked).
/// - Control: `cannot_control` (it cannot be started or stopped here),
///   `baseline_not_passed` (the recovery target has not passed its baseline
///   check), `operation_in_progress` (WGAL has an operation on the tunnel
///   under way).
/// - Settings: `unknown_time_zone` (not in the watchdog's time zone
///   database).
/// - Storage: `storage_load_failed` (the stored state could not be loaded,
///   so it is not overwritten), `storage_save_failed`,
///   `secret_store_unavailable` (the bot token's store).
/// - Notifications: `test_notification_in_progress`.
/// - Access: `caller_not_allowed` (the account is not on the installation's
///   access list), `needs_administrator_or_listed_tunnel` (the tunnel is
///   not on that list, so only an administrator may do this),
///   `mcp_disabled` (an agent's request while MCP is off),
///   `mcp_telegram_disabled` (an agent's change to the device name, chat
///   ID or bot token while the window does not let agents make it, or an
///   agent's request to let them).
/// - The watchdog: `shutting_down`, `too_many_waiting` (nothing was done;
///   try again), `not_running`, `no_answer_in_time` (it may still carry the
///   request out), `event_log_unavailable`, `not_available_here` (this
///   watchdog does not answer the request), `answer_too_large`.
/// - Reaching the watchdog, said by the GUI's own bridge:
///   `not_the_watchdog` (something else serves its pipe; nothing was sent),
///   `cannot_reach_watchdog` (nothing was sent), `answer_lost` (the
///   connection ended without an answer; the request may have been carried
///   out), `invalid_answer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ErrorReason {
    TunnelNotRegistered,
    TunnelNotDiscovered,
    BindingLost,
    IdConflict,
    TooManyTunnels,
    TooManyDiscovered,
    ManagedByAnotherTool,
    ConfigNotProtected,
    ManagerQueryFailed,
    CannotControl,
    BaselineNotPassed,
    OperationInProgress,
    UnknownTimeZone,
    StorageLoadFailed,
    StorageSaveFailed,
    SecretStoreUnavailable,
    TestNotificationInProgress,
    CallerNotAllowed,
    NeedsAdministratorOrListedTunnel,
    McpDisabled,
    McpTelegramDisabled,
    ShuttingDown,
    TooManyWaiting,
    NotRunning,
    NoAnswerInTime,
    EventLogUnavailable,
    NotAvailableHere,
    AnswerTooLarge,
    NotTheWatchdog,
    CannotReachWatchdog,
    AnswerLost,
    InvalidAnswer,
}
