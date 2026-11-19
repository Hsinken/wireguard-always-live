//! OS-independent core of the watchdog.
//!
//! Scheduling and recovery logic is written against the [`Clock`] and
//! [`PlatformAdapter`] traits defined here. Nothing in this crate spawns
//! processes, opens sockets or touches a tunnel; real platform adapters live in
//! `wgal-platform`, and deterministic fakes live in `wgal-testkit`.

pub mod clock;
pub mod platform;
pub mod record;
pub mod registry;
pub mod timing;
mod tunnel;
pub mod watchdog;
pub mod work;

pub use clock::{Clock, MonoInstant, SystemClock};
pub use platform::{
    AdapterError, DiscoveredBinding, InvalidBinding, MAX_BINDING_LEN, PlatformAdapter,
    ProbeOutcome, ProbeRequest, ResolvedInterface, RouteEvidence, TunnelBinding, TunnelMetrics,
};
pub use record::MAX_RECOVERED_NOTICES;
pub use record::ResumeOp;
pub use record::{
    External, IncidentRecord, MemoryStore, PersistedState, SCHEMA_VERSION, StateStore, StoreError,
    TunnelRecord,
};
pub use record::{NoticeRecord, PendingAutoRecovery, RecoveredNotice};
pub use timing::Timing;
pub use watchdog::MAX_EVENTS;
pub use watchdog::{
    DueNotification, NotificationError, NotificationOutcome, NotificationTicket, Startup, Watchdog,
};
pub use work::Executor;
pub use work::{WorkItem, WorkKind, WorkResult, execute};
