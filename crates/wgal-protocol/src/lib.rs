//! Versioned contract between the WGAL desktop GUI and the watchdog runtime.
//!
//! Rust is the source of truth. `cargo test -p wgal-protocol` regenerates the
//! TypeScript bindings in `apps/desktop/src/protocol/generated/`, and the JSON
//! fixtures in `fixtures/` are asserted by both the Rust and TypeScript tests,
//! so either side drifting from the other fails CI.
//!
//! Every collection and string in the contract is bounded, and messages go
//! through [`codec`], which enforces [`MAX_MESSAGE_BYTES`] in both directions.

mod adapter;
mod bounded;
pub mod codec;
mod document;
mod ids;
mod ipc;
mod log;
mod notify;
mod probe;
mod settings;
mod status;
mod template;
mod text;

pub use adapter::{
    AdapterCapabilities, AdapterErrorKind, Capability, DiscoveredTunnel, MAX_DISCOVERED,
};
pub use bounded::{BoundedVec, TooManyItems};
pub use codec::{DecodeError, EncodeError, FrameError};
pub use document::{
    AutoRecoveryImport, HostSettingsEntry, ImportItem, ImportMatch, ImportOptions, ImportOutcome,
    ImportResult, InvalidDocument, SETTINGS_DOCUMENT_VERSION, SettingsDocument, SettingsFormat,
    TunnelSettingsEntry,
};
pub use ids::{IncidentId, InvalidId, MAX_ID_LEN, RequestId, TunnelId};
pub use ipc::{
    ErrorCode, ErrorReason, MAX_MESSAGE_BYTES, MonitorSettings, Origin, Outcome, PROTOCOL_VERSION,
    ProtocolError, ProtocolInfo, Request, RequestEnvelope, RequestOp, Response, ResponseEnvelope,
    WINDOWS_PIPE_NAME,
};
pub use log::{EventEntry, EventFilter, LogEvent, MAX_EVENT_PAGE};
pub use notify::{
    BotToken, ChatId, CredentialState, InvalidNotificationValue, NotificationFailure,
    NotificationStatus, TestNotificationState, TestNotificationStatus,
};
pub use probe::{
    InvalidTarget, MAX_OBSERVE_TARGETS, ProbeKind, ProbeResult, ProbeSummary, ProbeTarget,
    TargetAddress, TargetStatus,
};
pub use settings::{
    HostSettings, HostStatus, InvalidSettingValue, MAX_WORKERS, MCP_VERSION_MARK, MIN_GUI_VERSION,
    MIN_USABLE_GUI_VERSION, MIN_WORKERS, ProbeTiming, ProductVersion, RecoveryRetry, TimeZoneName,
    TimeZoneStatus, WorkerCount, WorkerStatus, mcp_version_in,
};
pub use status::{
    DataHealth, DesiredState, Fault, FirstRecovery, IncidentStatus, Light, LightReason,
    MAX_TUNNELS, NoticeKind, NotificationState, OsState, RecoveredNotification, RecoveryPhase,
    Snapshot, StorageHealth, Takeover, TunnelStatus, UserIntent,
};
pub use template::{
    InvalidTemplate, LanguageTag, MessageTemplate, NotificationTemplates, TemplateTexts,
    TemplatesId,
};
pub use text::{Detail, DeviceName, DisplayName, InvalidText};
