use serde::{Deserialize, Serialize};

use crate::adapter::AdapterErrorKind;
use crate::ids::{IncidentId, TunnelId};
use crate::ipc::RequestOp;
use crate::notify::{NotificationFailure, TestNotificationState};
use crate::status::{Light, LightReason, NoticeKind};

/// Most entries one event log page can carry. With every field bounded, the
/// largest page still fits in one message (see `tests/limits.rs`).
pub const MAX_EVENT_PAGE: usize = 100;

/// One line of the watchdog's event log, as the GUI shows it. The watchdog
/// keeps about the last 72 hours, deleting older lines when it writes new
/// ones.
///
/// `seq` increases by one for every line the watchdog writes, across
/// restarts and file rotation; pages are read backwards from it
/// (`Request::ReadEventLog`). `at_unix_ms` is when the line was written.
/// `tunnel_id` is `null` for host-wide events and for lines about a tunnel
/// that is no longer registered; `removed_tunnel` tells the two apart: it is
/// `true` only for a line about a tunnel that is no longer registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct EventEntry {
    #[cfg_attr(test, ts(type = "number"))]
    pub seq: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub at_unix_ms: i64,
    pub tunnel_id: Option<TunnelId>,
    pub removed_tunnel: bool,
    pub event: LogEvent,
}

/// Which lines `Request::ReadEventLog` returns: every line, one registered
/// tunnel's, the watchdog's own lifecycle (`watchdog_started` and
/// `watchdog_stopped`), so the GUI can list when the background watchdog
/// went away and came back on their own, or `system`: every line that is
/// about no tunnel (the watchdog's start and stop, the host-wide pause,
/// host and notification settings, the local network, storage, test
/// notifications, lost lines). `system` is new in watchdog 1.1.0; an older
/// watchdog refuses it as a malformed request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventFilter {
    All,
    Tunnel { tunnel_id: TunnelId },
    Watchdog,
    System,
}

/// What an event log line records. The watchdog writes each one when the
/// thing named happens; the GUI lists them and never derives tunnel state
/// from them (the snapshot is the state).
///
/// Watchdog, registration and settings:
/// - `watchdog_started`, `watchdog_stopped`: the background watchdog
///   started, or is stopping in an orderly way (a crash writes nothing).
/// - `monitoring_paused`, `monitoring_resumed`: monitoring was paused or
///   resumed, host-wide (`Request::SetMonitoringPaused`) when the line has
///   no tunnel, else for that tunnel (`Request::SetTunnelMonitoring`).
/// - `tunnel_registered`, `tunnel_removed`: the tunnel was registered or
///   unregistered.
/// - `settings_changed`: a tunnel's targets or probe timing changed, or,
///   on a line with no tunnel, the host settings.
/// - `auto_recovery_changed`, `tunnel_notifications_changed`: the tunnel's
///   auto recovery or notification switch was turned on or off (`enabled`).
///
/// The user's operations: `user_connect`, `user_disconnect` and
/// `user_reconnect`, written when the watchdog accepts the request.
///
/// Probes and faults:
/// - `probe_failed`: a failed round that counts toward confirming a fault;
///   `streak` failed rounds so far, `needed` to confirm (`retries + 1`).
/// - `probe_recovered`: a successful round ended a streak of `after` failed
///   rounds.
/// - `fault_confirmed`: the fault was confirmed and opened `incident_id`.
/// - `traffic_verified`: the first verified round after the tunnel was
///   started or monitoring of it began.
/// - `light_changed`: the tunnel's light became `light`, for `reason`.
/// - `local_network_offline`, `local_network_online`: this host lost or
///   regained its own network; failures meanwhile are not tunnel faults.
///
/// Recovery and control:
/// - `recovery_started`: automatic recovery attempt `attempt` began; 1 is
///   the incident's first.
/// - `recovery_succeeded`: traffic was verified after an automatic recovery.
/// - `recovery_failed`: an automatic recovery did not restore traffic; the
///   next attempt is due at `next_retry_unix_ms`, `null` if none is planned.
/// - `retries_exhausted`: the tunnel's retry limit (`RecoveryRetry`) is used
///   up after `retries` rounds following the first, so automatic recovery
///   stops trying for this incident (watchdog 1.2.0 and later).
/// - `restarted_after_external_stop`: WGAL started the tunnel again after an
///   external stop, because `start_when_stopped_externally` is on.
/// - `control_failed`: a start or stop could not be carried out (`error`).
/// - `stopped_externally`, `started_externally`: something outside WGAL
///   stopped or started the tunnel.
///
/// Incidents and notifications:
/// - `incident_closed`: traffic stayed healthy through the closing period
///   and `incident_id` was closed.
/// - `notification_sent`: Telegram accepted the incident's notification
///   `notice` (`alert` in lines from before watchdog 1.2.0).
/// - `notification_failed`: it was not accepted, for `failure`.
/// - `notification_cancelled`: it was cancelled before it was sent.
/// - `test_notification`: a test notification was requested (`sending`) or
///   reached its result (`state`).
///
/// Storage:
/// - `storage_failed`, `storage_recovered`: the watchdog could no longer,
///   or again, persist its state (plan §9.1).
/// - `lines_lost`: `count` lines before this one could not be written.
///
/// Agents (owner decision of 2026-11-04):
/// - `agent_request`: an agent asked for `op` through `wgal-mcp` and the
///   watchdog accepted it; what it changed follows in the lines after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogEvent {
    WatchdogStarted,
    WatchdogStopped,
    MonitoringPaused,
    MonitoringResumed,
    TunnelRegistered,
    TunnelRemoved,
    SettingsChanged,
    AutoRecoveryChanged {
        enabled: bool,
    },
    TunnelNotificationsChanged {
        enabled: bool,
    },
    UserConnect,
    UserDisconnect,
    UserReconnect,
    ProbeFailed {
        streak: u32,
        needed: u32,
    },
    ProbeRecovered {
        after: u32,
    },
    FaultConfirmed {
        incident_id: IncidentId,
    },
    RecoveryStarted {
        attempt: u32,
    },
    RecoverySucceeded,
    RecoveryFailed {
        #[cfg_attr(test, ts(type = "number | null"))]
        next_retry_unix_ms: Option<i64>,
    },
    RetriesExhausted {
        retries: u32,
    },
    RestartedAfterExternalStop,
    ControlFailed {
        error: AdapterErrorKind,
    },
    TrafficVerified,
    LightChanged {
        light: Light,
        reason: LightReason,
    },
    StoppedExternally,
    StartedExternally,
    LocalNetworkOffline,
    LocalNetworkOnline,
    IncidentClosed {
        incident_id: IncidentId,
    },
    NotificationSent {
        incident_id: IncidentId,
        #[serde(default)]
        notice: NoticeKind,
    },
    NotificationFailed {
        incident_id: IncidentId,
        failure: NotificationFailure,
        #[serde(default)]
        notice: NoticeKind,
    },
    NotificationCancelled {
        incident_id: IncidentId,
        #[serde(default)]
        notice: NoticeKind,
    },
    TestNotification {
        state: TestNotificationState,
    },
    StorageFailed,
    StorageRecovered,
    LinesLost {
        count: u32,
    },
    AgentRequest {
        op: RequestOp,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_by_kind() {
        let json = serde_json::to_string(&LogEvent::ProbeFailed {
            streak: 2,
            needed: 3,
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"probe_failed","streak":2,"needed":3}"#);
        assert_eq!(
            serde_json::to_string(&LogEvent::WatchdogStarted).unwrap(),
            r#"{"kind":"watchdog_started"}"#
        );
    }

    // Lines are part of an answer, read leniently (owner decision of
    // 2026-11-04): a field a newer watchdog added is ignored, while an
    // unknown kind, a missing field or an invalid value is still refused.
    #[test]
    fn unknown_kinds_and_bad_fields_are_rejected_and_new_fields_ignored() {
        for bad in [
            r#"{"kind":"execute","command":"calc.exe"}"#,
            r#"{"kind":"probe_failed","streak":2}"#,
            r#"{"kind":"fault_confirmed","incident_id":"a/b"}"#,
        ] {
            assert!(
                serde_json::from_str::<LogEvent>(bad).is_err(),
                "{bad} should be rejected"
            );
        }
        assert_eq!(
            serde_json::from_str::<LogEvent>(
                r#"{"kind":"probe_failed","streak":2,"needed":3,"detail":"x"}"#
            )
            .unwrap(),
            LogEvent::ProbeFailed {
                streak: 2,
                needed: 3
            }
        );
    }

    #[test]
    fn event_filters_have_a_fixed_shape() {
        let cases = [
            (EventFilter::All, r#"{"kind":"all"}"#),
            (EventFilter::Watchdog, r#"{"kind":"watchdog"}"#),
            (EventFilter::System, r#"{"kind":"system"}"#),
            (
                EventFilter::Tunnel {
                    tunnel_id: TunnelId::new("home-3").unwrap(),
                },
                r#"{"kind":"tunnel","tunnel_id":"home-3"}"#,
            ),
        ];
        for (filter, json) in cases {
            assert_eq!(serde_json::to_string(&filter).unwrap(), json);
            assert_eq!(serde_json::from_str::<EventFilter>(json).unwrap(), filter);
        }
        for bad in [
            r#"{"kind":"tunnel"}"#,
            r#"{"kind":"tunnel","tunnel_id":"home-3","path":"x"}"#,
            r#"{"kind":"everything"}"#,
        ] {
            assert!(serde_json::from_str::<EventFilter>(bad).is_err(), "{bad}");
        }
    }
}
