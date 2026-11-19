//! Size limits on IPC messages, exercised through the public codec.

#![allow(clippy::unwrap_used)]

use std::net::Ipv4Addr;
use std::num::NonZeroU16;

use wgal_protocol::codec::{decode_request, decode_response, encode_request, encode_response};
use wgal_protocol::*;

const LIMIT: usize = MAX_MESSAGE_BYTES as usize;

const GET_SNAPSHOT: &str =
    r#"{"protocol_version":1,"request_id":"r1","request":{"op":"get_snapshot"}}"#;

/// `json` followed by spaces up to `len` bytes. Trailing whitespace is valid
/// JSON, so this makes a valid message of an exact size.
fn padded(json: &str, len: usize) -> Vec<u8> {
    let mut bytes = json.as_bytes().to_vec();
    assert!(bytes.len() <= len);
    bytes.resize(len, b' ');
    bytes
}

fn small_tunnel(i: usize) -> TunnelStatus {
    TunnelStatus {
        tunnel_id: TunnelId::new(format!("t{i}")).unwrap(),
        display_name: DisplayName::new(format!("Tunnel {i}")).unwrap(),
        light: Light::Gray,
        light_reason: LightReason::NotConfigured,
        light_since_unix_ms: 0,
        deadline_unix_ms: None,
        next_retry_unix_ms: None,
        last_verified_unix_ms: None,
        intent: UserIntent {
            desired: DesiredState::Connected,
            monitoring_enabled: true,
            auto_recovery_enabled: false,
            start_when_stopped_externally: false,
        },
        auto_recovery_allowed: false,
        notifications_enabled: true,
        takeover: Takeover::ObserveOnly,
        os_state: OsState::Connected,
        data_health: DataHealth::Unknown,
        recovery: RecoveryPhase::Idle,
        recovery_target: None,
        observe_targets: BoundedVec::new(),
        probe: ProbeTiming::DEFAULT,
        freshness_secs: ProbeTiming::DEFAULT.freshness_secs(),
        consecutive_failures: 0,
        fault: None,
        incident: None,
        retry: RecoveryRetry::DEFAULT,
        recovered_notification: None,
        auto_recovery_pending: false,
    }
}

fn small_host() -> HostStatus {
    HostStatus {
        watchdog_version: ProductVersion::current(),
        min_gui_version: ProductVersion::min_gui(),
        min_usable_gui_version: Some(ProductVersion::min_usable_gui()),
        mcp_enabled: false,
        mcp_telegram_enabled: false,
        workers: WorkerStatus {
            configured: None,
            suggested: MIN_WORKERS,
            limit: MIN_WORKERS,
            effective: MIN_WORKERS,
            running: 0,
        },
        time_zone: TimeZoneStatus {
            configured: None,
            effective: Some(TimeZoneName::new("UTC").unwrap()),
            utc_offset_secs: 0,
        },
    }
}

fn icmp_target(last_octet: u8) -> ProbeTarget {
    ProbeTarget {
        address: TargetAddress::new(Ipv4Addr::new(192, 0, 2, last_octet)).unwrap(),
        probe: ProbeKind::Icmp,
    }
}

fn snapshot_response(tunnels: Vec<TunnelStatus>) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r1").unwrap()),
        outcome: Outcome::Ok(Response::Snapshot {
            snapshot: Snapshot {
                generated_at_unix_ms: 0,
                storage: StorageHealth::Ok,
                capabilities: AdapterCapabilities::UNKNOWN,
                monitoring_paused: false,
                host: small_host(),
                notification: NotificationStatus::default(),
                tunnels: BoundedVec::try_from(tunnels).unwrap(),
            },
        }),
    }
}

/// The biggest target entry the types allow. The largest unicast address has
/// as many digits as the broadcast address, and the error result is longer
/// than any reply.
fn largest_target() -> TargetStatus {
    TargetStatus {
        target: ProbeTarget {
            address: TargetAddress::new(Ipv4Addr::new(223, 255, 255, 254)).unwrap(),
            probe: ProbeKind::TcpConnect {
                port: NonZeroU16::MAX,
            },
        },
        last_probe: Some(ProbeSummary {
            started_at_unix_ms: i64::MIN,
            finished_at_unix_ms: i64::MIN,
            result: ProbeResult::Error {
                error: AdapterErrorKind::LocalNetworkOffline,
            },
        }),
    }
}

/// The biggest tunnel entry the types allow: longest ID, text made only of `"`
/// so every byte doubles when escaped, full target lists, the longest enum
/// variants and numbers with the most digits. Extend this when a field is
/// added.
fn largest_tunnel(i: usize) -> TunnelStatus {
    TunnelStatus {
        tunnel_id: TunnelId::new(format!("{i:0>MAX_ID_LEN$}")).unwrap(),
        display_name: DisplayName::new("\"".repeat(DisplayName::MAX_BYTES)).unwrap(),
        light: Light::Yellow,
        light_reason: LightReason::LocalNetworkOffline,
        light_since_unix_ms: i64::MIN,
        deadline_unix_ms: Some(i64::MIN),
        next_retry_unix_ms: Some(i64::MIN),
        last_verified_unix_ms: Some(i64::MIN),
        intent: UserIntent {
            desired: DesiredState::Disconnected,
            monitoring_enabled: false,
            auto_recovery_enabled: false,
            start_when_stopped_externally: false,
        },
        auto_recovery_allowed: false,
        notifications_enabled: false,
        takeover: Takeover::ExternallyControlled,
        os_state: OsState::Transitioning,
        data_health: DataHealth::Healthy,
        recovery: RecoveryPhase::Verifying,
        recovery_target: Some(largest_target()),
        observe_targets: BoundedVec::try_from(vec![largest_target(); MAX_OBSERVE_TARGETS]).unwrap(),
        probe: largest_timing(),
        freshness_secs: u32::MAX,
        consecutive_failures: u32::MAX,
        fault: Some(Fault {
            kind: AdapterErrorKind::LocalNetworkOffline,
            message: Detail::new("\"".repeat(Detail::MAX_BYTES)).unwrap(),
        }),
        incident: Some(IncidentStatus {
            incident_id: IncidentId::new("i".repeat(MAX_ID_LEN)).unwrap(),
            opened_at_unix_ms: i64::MIN,
            first_recovery: FirstRecovery::NotAttempted,
            backoff_step: u32::MAX,
            stable_since_unix_ms: Some(i64::MIN),
            notification: NotificationState::NotEligible,
            retries_used: u32::MAX,
            retries_exhausted: false,
            exhausted_notification: NotificationState::NotEligible,
        }),
        retry: RecoveryRetry::new(
            Some(*RecoveryRetry::LIMIT.end()),
            *RecoveryRetry::INTERVAL_SECS.end(),
        )
        .unwrap(),
        recovered_notification: Some(RecoveredNotification {
            incident_id: IncidentId::new("i".repeat(MAX_ID_LEN)).unwrap(),
            notification: NotificationState::NotEligible,
        }),
        auto_recovery_pending: false,
    }
}

/// Every field at the end of its range, so each has the most digits.
fn largest_timing() -> ProbeTiming {
    ProbeTiming::new(
        *ProbeTiming::INTERVAL_SECS.end(),
        *ProbeTiming::TIMEOUT_SECS.end(),
        *ProbeTiming::RETRIES.end(),
        *ProbeTiming::MIN_FAULT_SECS.end(),
    )
    .unwrap()
}

/// The longest time zone name the type allows.
fn largest_zone() -> TimeZoneName {
    TimeZoneName::new(format!("A/{}", "b".repeat(TimeZoneName::MAX_BYTES - 2))).unwrap()
}

/// The biggest host status the types allow. Extend this when a field is
/// added.
fn largest_host() -> HostStatus {
    HostStatus {
        watchdog_version: ProductVersion::new("9".repeat(ProductVersion::MAX_BYTES)).unwrap(),
        min_gui_version: ProductVersion::new("9".repeat(ProductVersion::MAX_BYTES)).unwrap(),
        min_usable_gui_version: Some(
            ProductVersion::new("9".repeat(ProductVersion::MAX_BYTES)).unwrap(),
        ),
        mcp_enabled: false,
        mcp_telegram_enabled: false,
        workers: WorkerStatus {
            configured: Some(WorkerCount::new(MAX_WORKERS).unwrap()),
            suggested: u8::MAX,
            limit: u8::MAX,
            effective: u8::MAX,
            running: u8::MAX,
        },
        time_zone: TimeZoneStatus {
            configured: Some(largest_zone()),
            effective: Some(largest_zone()),
            utc_offset_secs: i32::MIN,
        },
    }
}

/// Every event kind with its fields at their largest. The match below stops
/// compiling when a kind is added, so it cannot be left out of the list.
fn every_event_at_its_largest() -> Vec<LogEvent> {
    let incident_id = || IncidentId::new("i".repeat(MAX_ID_LEN)).unwrap();
    let events = vec![
        LogEvent::WatchdogStarted,
        LogEvent::WatchdogStopped,
        LogEvent::MonitoringPaused,
        LogEvent::MonitoringResumed,
        LogEvent::TunnelRegistered,
        LogEvent::TunnelRemoved,
        LogEvent::SettingsChanged,
        LogEvent::AutoRecoveryChanged { enabled: false },
        LogEvent::TunnelNotificationsChanged { enabled: false },
        LogEvent::UserConnect,
        LogEvent::UserDisconnect,
        LogEvent::UserReconnect,
        LogEvent::ProbeFailed {
            streak: u32::MAX,
            needed: u32::MAX,
        },
        LogEvent::ProbeRecovered { after: u32::MAX },
        LogEvent::FaultConfirmed {
            incident_id: incident_id(),
        },
        LogEvent::RecoveryStarted { attempt: u32::MAX },
        LogEvent::RecoverySucceeded,
        LogEvent::RecoveryFailed {
            next_retry_unix_ms: Some(i64::MIN),
        },
        LogEvent::RetriesExhausted { retries: u32::MAX },
        LogEvent::RestartedAfterExternalStop,
        LogEvent::ControlFailed {
            error: AdapterErrorKind::LocalNetworkOffline,
        },
        LogEvent::TrafficVerified,
        LogEvent::LightChanged {
            light: Light::Yellow,
            reason: LightReason::LocalNetworkOffline,
        },
        LogEvent::StoppedExternally,
        LogEvent::StartedExternally,
        LogEvent::LocalNetworkOffline,
        LogEvent::LocalNetworkOnline,
        LogEvent::IncidentClosed {
            incident_id: incident_id(),
        },
        LogEvent::NotificationSent {
            incident_id: incident_id(),
            notice: NoticeKind::RetriesExhausted,
        },
        LogEvent::NotificationFailed {
            incident_id: incident_id(),
            failure: NotificationFailure::CredentialUnavailable,
            notice: NoticeKind::RetriesExhausted,
        },
        LogEvent::NotificationCancelled {
            incident_id: incident_id(),
            notice: NoticeKind::RetriesExhausted,
        },
        LogEvent::TestNotification {
            state: TestNotificationState::Accepted,
        },
        LogEvent::StorageFailed,
        LogEvent::StorageRecovered,
        LogEvent::LinesLost { count: u32::MAX },
        LogEvent::AgentRequest {
            op: RequestOp::SetStartWhenStoppedExternally,
        },
    ];
    for event in &events {
        match event {
            LogEvent::WatchdogStarted
            | LogEvent::WatchdogStopped
            | LogEvent::MonitoringPaused
            | LogEvent::MonitoringResumed
            | LogEvent::TunnelRegistered
            | LogEvent::TunnelRemoved
            | LogEvent::SettingsChanged
            | LogEvent::AutoRecoveryChanged { .. }
            | LogEvent::TunnelNotificationsChanged { .. }
            | LogEvent::UserConnect
            | LogEvent::UserDisconnect
            | LogEvent::UserReconnect
            | LogEvent::ProbeFailed { .. }
            | LogEvent::ProbeRecovered { .. }
            | LogEvent::FaultConfirmed { .. }
            | LogEvent::RecoveryStarted { .. }
            | LogEvent::RecoverySucceeded
            | LogEvent::RecoveryFailed { .. }
            | LogEvent::RetriesExhausted { .. }
            | LogEvent::RestartedAfterExternalStop
            | LogEvent::ControlFailed { .. }
            | LogEvent::TrafficVerified
            | LogEvent::LightChanged { .. }
            | LogEvent::StoppedExternally
            | LogEvent::StartedExternally
            | LogEvent::LocalNetworkOffline
            | LogEvent::LocalNetworkOnline
            | LogEvent::IncidentClosed { .. }
            | LogEvent::NotificationSent { .. }
            | LogEvent::NotificationFailed { .. }
            | LogEvent::NotificationCancelled { .. }
            | LogEvent::TestNotification { .. }
            | LogEvent::StorageFailed
            | LogEvent::StorageRecovered
            | LogEvent::LinesLost { .. }
            | LogEvent::AgentRequest { .. } => {}
        }
    }
    events
}

/// The biggest log entry the types allow: the event kind that encodes
/// longest, with the largest numbers and the longest tunnel ID.
fn largest_entry() -> EventEntry {
    let event = every_event_at_its_largest()
        .into_iter()
        .max_by_key(|event| serde_json::to_vec(event).unwrap().len())
        .unwrap();
    EventEntry {
        seq: u64::MAX,
        at_unix_ms: i64::MIN,
        tunnel_id: Some(TunnelId::new("t".repeat(MAX_ID_LEN)).unwrap()),
        removed_tunnel: false,
        event,
    }
}

fn event_log_response(entries: Vec<EventEntry>) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r1").unwrap()),
        outcome: Outcome::Ok(Response::EventLog {
            entries: BoundedVec::try_from(entries).unwrap(),
            more: false,
        }),
    }
}

/// The biggest notification status the types allow. Extend this when a field
/// is added.
fn largest_notification() -> NotificationStatus {
    NotificationStatus {
        enabled: false,
        device_name: DeviceName::new("\u{1F600}".repeat(DeviceName::MAX_CHARS)).unwrap(),
        chat_id: Some(ChatId::new(format!("@{}", "a".repeat(ChatId::MAX_BYTES - 1))).unwrap()),
        token: CredentialState::Unavailable,
        last_test: Some(TestNotificationStatus {
            request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
            state: TestNotificationState::Accepted,
            requested_at_unix_ms: i64::MIN,
            finished_at_unix_ms: Some(i64::MIN),
            failure: Some(NotificationFailure::CredentialUnavailable),
        }),
        last_failure: Some(NotificationFailure::CredentialUnavailable),
        templates_id: Some(TemplatesId::new("t".repeat(TemplatesId::MAX_BYTES)).unwrap()),
        cli_language: Some(LanguageTag::new("l".repeat(LanguageTag::MAX_BYTES)).unwrap()),
        host_name: Some(DeviceName::new("\u{1F600}".repeat(DeviceName::MAX_CHARS)).unwrap()),
    }
}

/// The biggest template set the types allow: every text at its limit, made
/// of the placeholders it must use and otherwise only `"`, so every other
/// byte doubles when escaped (a line break doubles too, and nothing else
/// may be escaped longer). Extend this when a field is added.
fn largest_templates() -> NotificationTemplates {
    let quotes = |placeholders: &[&str], n: usize| {
        let names: String = placeholders.iter().map(|p| format!("{{{p}}}")).collect();
        MessageTemplate::new(format!("{names}{}", "\"".repeat(n - names.len()))).unwrap()
    };
    let max = MessageTemplate::MAX_BYTES;
    NotificationTemplates::new(
        TemplatesId::new("t".repeat(TemplatesId::MAX_BYTES)).unwrap(),
        TemplateTexts {
            formal: quotes(NotificationTemplates::FORMAL_PLACEHOLDERS, max),
            result_failed: quotes(&[], max),
            result_not_executed: quotes(&[], max),
            retry_limited: quotes(NotificationTemplates::RETRY_LIMITED_PLACEHOLDERS, max),
            retry_forever: quotes(NotificationTemplates::RETRY_FOREVER_PLACEHOLDERS, max),
            retry_none: quotes(&[], max),
            exhausted: quotes(NotificationTemplates::EXHAUSTED_PLACEHOLDERS, max),
            recovered: quotes(NotificationTemplates::RECOVERED_PLACEHOLDERS, max),
            test: quotes(NotificationTemplates::TEST_PLACEHOLDERS, max),
            unnamed_device: quotes(&[], NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES),
        },
    )
    .unwrap()
}

/// Every error code. The match below stops compiling when one is added, so
/// it cannot be left out of the list.
fn every_error_code() -> Vec<ErrorCode> {
    let codes = vec![
        ErrorCode::UnsupportedVersion,
        ErrorCode::MalformedRequest,
        ErrorCode::MessageTooLarge,
        ErrorCode::Unauthorized,
        ErrorCode::Timeout,
        ErrorCode::UnknownTunnel,
        ErrorCode::InvalidState,
        ErrorCode::InvalidSetting,
        ErrorCode::StorageUnavailable,
        ErrorCode::Busy,
        ErrorCode::Internal,
    ];
    for code in &codes {
        match code {
            ErrorCode::UnsupportedVersion
            | ErrorCode::MalformedRequest
            | ErrorCode::MessageTooLarge
            | ErrorCode::Unauthorized
            | ErrorCode::Timeout
            | ErrorCode::UnknownTunnel
            | ErrorCode::InvalidState
            | ErrorCode::InvalidSetting
            | ErrorCode::StorageUnavailable
            | ErrorCode::Busy
            | ErrorCode::Internal => {}
        }
    }
    codes
}

/// Every error reason, kept complete the same way.
fn every_error_reason() -> Vec<ErrorReason> {
    let reasons = vec![
        ErrorReason::TunnelNotRegistered,
        ErrorReason::TunnelNotDiscovered,
        ErrorReason::BindingLost,
        ErrorReason::IdConflict,
        ErrorReason::TooManyTunnels,
        ErrorReason::TooManyDiscovered,
        ErrorReason::ManagedByAnotherTool,
        ErrorReason::ConfigNotProtected,
        ErrorReason::ManagerQueryFailed,
        ErrorReason::CannotControl,
        ErrorReason::BaselineNotPassed,
        ErrorReason::OperationInProgress,
        ErrorReason::UnknownTimeZone,
        ErrorReason::StorageLoadFailed,
        ErrorReason::StorageSaveFailed,
        ErrorReason::SecretStoreUnavailable,
        ErrorReason::TestNotificationInProgress,
        ErrorReason::CallerNotAllowed,
        ErrorReason::NeedsAdministratorOrListedTunnel,
        ErrorReason::ShuttingDown,
        ErrorReason::TooManyWaiting,
        ErrorReason::NotRunning,
        ErrorReason::NoAnswerInTime,
        ErrorReason::EventLogUnavailable,
        ErrorReason::NotAvailableHere,
        ErrorReason::AnswerTooLarge,
        ErrorReason::NotTheWatchdog,
        ErrorReason::CannotReachWatchdog,
        ErrorReason::AnswerLost,
        ErrorReason::InvalidAnswer,
        ErrorReason::McpDisabled,
        ErrorReason::McpTelegramDisabled,
    ];
    for reason in &reasons {
        match reason {
            ErrorReason::TunnelNotRegistered
            | ErrorReason::TunnelNotDiscovered
            | ErrorReason::BindingLost
            | ErrorReason::IdConflict
            | ErrorReason::TooManyTunnels
            | ErrorReason::TooManyDiscovered
            | ErrorReason::ManagedByAnotherTool
            | ErrorReason::ConfigNotProtected
            | ErrorReason::ManagerQueryFailed
            | ErrorReason::CannotControl
            | ErrorReason::BaselineNotPassed
            | ErrorReason::OperationInProgress
            | ErrorReason::UnknownTimeZone
            | ErrorReason::StorageLoadFailed
            | ErrorReason::StorageSaveFailed
            | ErrorReason::SecretStoreUnavailable
            | ErrorReason::TestNotificationInProgress
            | ErrorReason::CallerNotAllowed
            | ErrorReason::NeedsAdministratorOrListedTunnel
            | ErrorReason::McpDisabled
            | ErrorReason::McpTelegramDisabled
            | ErrorReason::ShuttingDown
            | ErrorReason::TooManyWaiting
            | ErrorReason::NotRunning
            | ErrorReason::NoAnswerInTime
            | ErrorReason::EventLogUnavailable
            | ErrorReason::NotAvailableHere
            | ErrorReason::AnswerTooLarge
            | ErrorReason::NotTheWatchdog
            | ErrorReason::CannotReachWatchdog
            | ErrorReason::AnswerLost
            | ErrorReason::InvalidAnswer => {}
        }
    }
    reasons
}

fn error_response(error: ProtocolError) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap()),
        outcome: Outcome::Err(error),
    }
}

/// The encoded length of a value, to pick the variant that encodes
/// longest.
fn encoded_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).unwrap().len()
}

#[test]
fn request_exactly_at_the_limit_is_accepted() {
    let envelope = decode_request(&padded(GET_SNAPSHOT, LIMIT)).unwrap();
    assert_eq!(envelope.request, Request::GetSnapshot);
}

#[test]
fn request_over_the_limit_is_rejected_unparsed() {
    let err = decode_request(&padded(GET_SNAPSHOT, LIMIT + 1)).unwrap_err();
    assert_eq!(err, DecodeError::TooLarge { len: LIMIT + 1 });
    assert_eq!(err.code(), ErrorCode::MessageTooLarge);
    assert_eq!(err.request_id(), None);

    // Not parsed at all: oversized garbage is reported as too large, not malformed.
    let garbage = vec![b'['; 10 * LIMIT];
    assert_eq!(
        decode_request(&garbage).unwrap_err(),
        DecodeError::TooLarge { len: 10 * LIMIT }
    );
}

#[test]
fn response_over_the_limit_is_rejected_unparsed() {
    let json = String::from_utf8(encode_response(&snapshot_response(vec![])).unwrap()).unwrap();
    assert!(decode_response(&padded(&json, LIMIT)).is_ok());
    assert_eq!(
        decode_response(&padded(&json, LIMIT + 1)).unwrap_err(),
        DecodeError::TooLarge { len: LIMIT + 1 }
    );
}

#[test]
fn snapshot_with_too_many_tunnels_is_rejected() {
    let full: Vec<_> = (0..MAX_TUNNELS).map(small_tunnel).collect();
    let mut over = full.clone();
    over.push(small_tunnel(MAX_TUNNELS));

    assert_eq!(
        BoundedVec::<TunnelStatus, MAX_TUNNELS>::try_from(over.clone()).unwrap_err(),
        TooManyItems {
            max: MAX_TUNNELS,
            len: MAX_TUNNELS + 1
        }
    );

    let ok = encode_response(&snapshot_response(full)).unwrap();
    assert!(decode_response(&ok).is_ok());

    // Build the over-long list as raw JSON, since the types cannot hold it.
    let mut json = serde_json::to_value(snapshot_response(vec![])).unwrap();
    json["outcome"]["ok"]["snapshot"]["tunnels"] = serde_json::to_value(&over).unwrap();
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(
        bytes.len() < LIMIT,
        "test input must fail on the count, not the size"
    );

    let err = decode_response(&bytes).unwrap_err();
    assert!(matches!(err, DecodeError::Malformed { .. }), "{err:?}");
    assert_eq!(err.request_id().map(RequestId::as_str), Some("r1"));
}

#[test]
fn over_long_or_control_character_text_is_rejected() {
    let long_name = "x".repeat(DisplayName::MAX_BYTES + 1);
    let mut json = serde_json::to_value(snapshot_response(vec![small_tunnel(0)])).unwrap();
    json["outcome"]["ok"]["snapshot"]["tunnels"][0]["display_name"] = long_name.into();
    assert!(matches!(
        decode_response(&serde_json::to_vec(&json).unwrap()),
        Err(DecodeError::Malformed { .. })
    ));

    json["outcome"]["ok"]["snapshot"]["tunnels"][0]["display_name"] = "two\nlines".into();
    assert!(matches!(
        decode_response(&serde_json::to_vec(&json).unwrap()),
        Err(DecodeError::Malformed { .. })
    ));
}

#[test]
fn deep_nesting_within_the_limit_is_rejected_without_overflowing_the_stack() {
    let nested = format!(
        r#"{{"protocol_version":1,"request_id":"r1","request":{}}}"#,
        "[".repeat(LIMIT / 2)
    );
    assert!(nested.len() <= LIMIT);
    assert!(matches!(
        decode_request(nested.as_bytes()),
        Err(DecodeError::Malformed { .. })
    ));
}

#[test]
fn largest_valid_snapshot_fits_in_one_message() {
    let tunnels: Vec<_> = (0..MAX_TUNNELS).map(largest_tunnel).collect();
    let mut envelope = snapshot_response(tunnels);
    envelope.request_id = Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap());
    if let Outcome::Ok(Response::Snapshot { snapshot }) = &mut envelope.outcome {
        snapshot.generated_at_unix_ms = i64::MIN;
        snapshot.storage = StorageHealth::Failed;
        snapshot.monitoring_paused = false;
        snapshot.host = largest_host();
        snapshot.notification = largest_notification();
    }

    let bytes = encode_response(&envelope).unwrap();
    // Shown with `--nocapture`; AGENTS.md records the number.
    println!("largest snapshot: {} of {LIMIT} bytes", bytes.len());
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), envelope);
}

#[test]
fn largest_valid_event_log_page_fits_in_one_message() {
    let entry = largest_entry();
    assert!(
        matches!(entry.event, LogEvent::NotificationFailed { .. }),
        "the largest kind is now {:?}",
        entry.event
    );
    let mut envelope = event_log_response(vec![entry; MAX_EVENT_PAGE]);
    envelope.request_id = Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap());
    if let Outcome::Ok(Response::EventLog { more, .. }) = &mut envelope.outcome {
        *more = false;
    }

    let bytes = encode_response(&envelope).unwrap();
    // Shown with `--nocapture`; AGENTS.md records the number.
    println!("largest event log page: {} of {LIMIT} bytes", bytes.len());
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), envelope);
}

#[test]
fn every_event_kind_round_trips_through_the_codec() {
    for event in every_event_at_its_largest() {
        let entry = EventEntry {
            event,
            ..largest_entry()
        };
        let envelope = event_log_response(vec![entry]);
        let bytes = encode_response(&envelope).unwrap();
        assert_eq!(decode_response(&bytes).unwrap(), envelope);
    }
}

#[test]
fn event_log_page_with_too_many_entries_is_rejected() {
    let small = EventEntry {
        seq: 1,
        at_unix_ms: 0,
        tunnel_id: None,
        removed_tunnel: false,
        event: LogEvent::WatchdogStarted,
    };
    let full = vec![small.clone(); MAX_EVENT_PAGE];
    let mut over = full.clone();
    over.push(small);
    assert!(decode_response(&encode_response(&event_log_response(full)).unwrap()).is_ok());

    // Build the over-long page as raw JSON, since the types cannot hold it.
    let mut json = serde_json::to_value(event_log_response(vec![])).unwrap();
    json["outcome"]["ok"]["entries"] = serde_json::to_value(&over).unwrap();
    let bytes = serde_json::to_vec(&json).unwrap();
    assert!(
        bytes.len() < LIMIT,
        "test input must fail on the count, not the size"
    );

    let err = decode_response(&bytes).unwrap_err();
    assert!(matches!(err, DecodeError::Malformed { .. }), "{err:?}");
    assert_eq!(err.request_id().map(RequestId::as_str), Some("r1"));
}

#[test]
fn largest_valid_discovery_fits_in_one_message() {
    let tunnels: Vec<_> = (0..MAX_DISCOVERED)
        .map(|i| DiscoveredTunnel {
            tunnel_id: TunnelId::new(format!("{i:0>MAX_ID_LEN$}")).unwrap(),
            display_name: DisplayName::new("\"".repeat(DisplayName::MAX_BYTES)).unwrap(),
            control: Capability::Unsupported,
        })
        .collect();
    let envelope = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap()),
        outcome: Outcome::Ok(Response::Discovered {
            tunnels: BoundedVec::try_from(tunnels).unwrap(),
        }),
    };

    let bytes = encode_response(&envelope).unwrap();
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), envelope);
}

#[test]
fn largest_template_request_fits_in_one_message() {
    let envelope = RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
        request: Request::SetNotificationTemplates {
            templates: largest_templates(),
        },
    };
    let bytes = encode_request(&envelope).unwrap();
    // Shown with `--nocapture`.
    println!("largest template request: {} of {LIMIT} bytes", bytes.len());
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_request(&bytes).unwrap(), envelope);
}

#[test]
fn largest_error_response_fits_in_one_message() {
    let code = every_error_code()
        .into_iter()
        .max_by_key(encoded_len)
        .unwrap();
    let reason = every_error_reason()
        .into_iter()
        .max_by_key(encoded_len)
        .unwrap();
    assert_eq!(reason, ErrorReason::NeedsAdministratorOrListedTunnel);
    let envelope = error_response(ProtocolError::new(
        code,
        Some(reason),
        &"\"".repeat(Detail::MAX_BYTES),
    ));
    let bytes = encode_response(&envelope).unwrap();
    // Shown with `--nocapture`.
    println!("largest error response: {} of {LIMIT} bytes", bytes.len());
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), envelope);
}

#[test]
fn every_error_code_and_reason_round_trips_through_the_codec() {
    for code in every_error_code() {
        let envelope = error_response(ProtocolError::new(code, None, "x"));
        let bytes = encode_response(&envelope).unwrap();
        assert_eq!(decode_response(&bytes).unwrap(), envelope);
    }
    for reason in every_error_reason() {
        let envelope = error_response(ProtocolError::new(ErrorCode::Busy, Some(reason), "x"));
        let bytes = encode_response(&envelope).unwrap();
        assert_eq!(decode_response(&bytes).unwrap(), envelope);
    }
}

#[test]
fn an_invalid_template_set_is_a_malformed_request() {
    let request = |formal: &str| {
        serde_json::json!({
            "protocol_version": 1,
            "request_id": "r8",
            "request": {
                "op": "set_notification_templates",
                "templates": {
                    "id": "zh-TW/1",
                    "formal": formal,
                    "result_failed": "x",
                    "result_not_executed": "x",
                    "retry_limited": "{interval} {limit}",
                    "retry_forever": "{interval}",
                    "retry_none": "x",
                    "exhausted": "{device} {tunnel} {incident} {time_zone} {started_at} {retries}",
                    "recovered": "{device} {tunnel} {incident} {time_zone} {started_at} {recovered_at}",
                    "test": "{device} {time_zone} {time}",
                    "unnamed_device": "x",
                },
            },
        })
        .to_string()
    };
    let formal = "{device} {tunnel} {incident} {time_zone} {started_at} {result} {retry}";
    assert!(decode_request(request(formal).as_bytes()).is_ok());
    let with_endpoint = format!("{formal} {{endpoint}}");
    let twice = format!("{formal} {{device}}");
    let unclosed = format!("{formal} {{device");
    let spaced = format!(" {formal}");
    for bad in [
        with_endpoint.as_str(),
        twice.as_str(),
        unclosed.as_str(),
        spaced.as_str(),
        "{device} {result}",
        // The form before 1.3.0, without the zone.
        "{device} {tunnel} {incident} {started_at} {result} {retry}",
    ] {
        let err = decode_request(request(bad).as_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::MalformedRequest, "{bad}");
        assert_eq!(err.request_id().map(RequestId::as_str), Some("r8"));
    }
    let too_long = "x".repeat(MessageTemplate::MAX_BYTES + 1);
    let err = decode_request(request(&too_long).as_bytes()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MalformedRequest);
}

#[test]
fn request_with_too_many_observe_targets_is_rejected() {
    let targets = |n: usize| -> Vec<ProbeTarget> {
        (0..n)
            .map(|i| icmp_target(u8::try_from(i + 1).unwrap()))
            .collect()
    };
    let request = |n: usize| {
        serde_json::json!({
            "protocol_version": 1,
            "request_id": "r9",
            "request": {
                "op": "update_monitor_settings",
                "tunnel_id": "office-1",
                "settings": {
                    "recovery_target": null,
                    "observe_targets": targets(n),
                    "probe": ProbeTiming::DEFAULT,
                },
            },
        })
        .to_string()
    };

    assert!(decode_request(request(MAX_OBSERVE_TARGETS).as_bytes()).is_ok());
    let err = decode_request(request(MAX_OBSERVE_TARGETS + 1).as_bytes()).unwrap_err();
    assert!(matches!(err, DecodeError::Malformed { .. }), "{err:?}");
    assert_eq!(err.request_id().map(RequestId::as_str), Some("r9"));
}

#[test]
fn requests_round_trip_through_the_codec() {
    let envelope = RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
        request: Request::UpdateMonitorSettings {
            tunnel_id: TunnelId::new("t".repeat(MAX_ID_LEN)).unwrap(),
            settings: MonitorSettings {
                recovery_target: Some(largest_target().target),
                observe_targets: BoundedVec::try_from(vec![
                    largest_target().target;
                    MAX_OBSERVE_TARGETS
                ])
                .unwrap(),
                probe: largest_timing(),
            },
        },
    };
    let bytes = encode_request(&envelope).unwrap();
    assert!(bytes.len() <= LIMIT);
    assert_eq!(decode_request(&bytes).unwrap(), envelope);
}

#[test]
fn notification_requests_round_trip_through_the_codec() {
    let token = format!("{}:{}", "9".repeat(15), "x".repeat(40));
    assert_eq!(token.len(), BotToken::MAX_BYTES);
    for request in [
        Request::SetBotToken {
            token: BotToken::new(token).unwrap(),
        },
        Request::ClearBotToken,
        Request::SendTestNotification,
        Request::SetMonitoringPaused { paused: true },
        Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("\u{1F600}".repeat(DeviceName::MAX_CHARS)).unwrap(),
            chat_id: Some(ChatId::new(format!("-{}", "9".repeat(19))).unwrap()),
        },
    ] {
        let envelope = RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
            request,
        };
        let bytes = encode_request(&envelope).unwrap();
        assert!(bytes.len() <= LIMIT);
        assert_eq!(decode_request(&bytes).unwrap(), envelope);
    }
}

#[test]
fn tunnel_settings_and_log_requests_round_trip_through_the_codec() {
    let tunnel_id = || TunnelId::new("t".repeat(MAX_ID_LEN)).unwrap();
    for request in [
        Request::UnregisterTunnel {
            tunnel_id: tunnel_id(),
        },
        Request::SetTunnelMonitoring {
            tunnel_id: tunnel_id(),
            enabled: false,
        },
        Request::SetTunnelNotifications {
            tunnel_id: tunnel_id(),
            enabled: false,
        },
        Request::UpdateHostSettings {
            settings: HostSettings {
                workers: Some(WorkerCount::new(MAX_WORKERS).unwrap()),
                time_zone: Some(largest_zone()),
            },
        },
        Request::UpdateHostSettings {
            settings: HostSettings::default(),
        },
        Request::ReadEventLog {
            filter: EventFilter::Tunnel {
                tunnel_id: tunnel_id(),
            },
            before_seq: Some(u64::MAX),
        },
        Request::ReadEventLog {
            filter: EventFilter::All,
            before_seq: None,
        },
        Request::ReadEventLog {
            filter: EventFilter::Watchdog,
            before_seq: None,
        },
    ] {
        let envelope = RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
            request,
        };
        let bytes = encode_request(&envelope).unwrap();
        assert!(bytes.len() <= LIMIT);
        assert_eq!(decode_request(&bytes).unwrap(), envelope);
    }
}

#[test]
fn out_of_range_settings_are_malformed_requests() {
    let probe = r#"{"protocol_version":1,"request_id":"r5","request":{"op":"update_monitor_settings",
        "tunnel_id":"office-1","settings":{"recovery_target":null,"observe_targets":[],
        "probe":{"interval_secs":5,"timeout_secs":2,"retries":2,"min_fault_secs":60}}}}"#;
    let workers = r#"{"protocol_version":1,"request_id":"r6","request":{"op":"update_host_settings",
        "settings":{"workers":18,"time_zone":null}}}"#;
    let zone = r#"{"protocol_version":1,"request_id":"r7","request":{"op":"update_host_settings",
        "settings":{"workers":null,"time_zone":"../etc/localtime"}}}"#;
    for (json, id) in [(probe, "r5"), (workers, "r6"), (zone, "r7")] {
        let err = decode_request(json.as_bytes()).unwrap_err();
        assert_eq!(err.code(), ErrorCode::MalformedRequest, "{id}");
        assert_eq!(err.request_id().map(RequestId::as_str), Some(id));
    }
}

#[test]
fn a_rejected_bot_token_is_never_quoted() {
    let leaky = r#"{"protocol_version":1,"request_id":"r1",
        "request":{"op":"set_bot_token","token":"123456:secret-token-value!AAAAAAAAAAAAAAAAAAAAA"}}"#;
    let err = decode_request(leaky.as_bytes()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MalformedRequest);
    assert!(!format!("{err} {err:?}").contains("secret"), "{err:?}");
}

#[test]
fn unsupported_version_is_reported_with_the_request_id() {
    let v2 = r#"{"protocol_version":2,"request_id":"r7","request":{"op":"something_new"}}"#;
    let err = decode_request(v2.as_bytes()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnsupportedVersion);
    assert_eq!(err.request_id().map(RequestId::as_str), Some("r7"));
}

#[test]
fn rejections_never_quote_the_input() {
    let leaky = r#"{"protocol_version":1,"request_id":"r1",
        "request":{"op":"connect","tunnel_id":"secret-token-value!"}}"#;
    let err = decode_request(leaky.as_bytes()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::MalformedRequest);
    assert!(!format!("{err} {err:?}").contains("secret"), "{err:?}");
}

/// The biggest settings file the types allow (owner decision of
/// 2026-11-04): every tunnel at its longest, made of the longest names,
/// targets and numbers.
fn largest_document() -> SettingsDocument {
    let entry = |i: usize| TunnelSettingsEntry {
        tunnel_id: TunnelId::new(format!("{i:0>MAX_ID_LEN$}")).unwrap(),
        name: DisplayName::new("\"".repeat(DisplayName::MAX_BYTES)).unwrap(),
        monitoring: false,
        notifications: false,
        auto_recovery: false,
        start_when_stopped_externally: false,
        recovery_target: Some(largest_target().target),
        observe_targets: BoundedVec::try_from(vec![largest_target().target; MAX_OBSERVE_TARGETS])
            .unwrap(),
        probe: largest_timing(),
        retry: RecoveryRetry::new(
            Some(*RecoveryRetry::LIMIT.end()),
            *RecoveryRetry::INTERVAL_SECS.end(),
        )
        .unwrap(),
    };
    SettingsDocument {
        format: SettingsFormat::WgalSettings,
        version: SETTINGS_DOCUMENT_VERSION,
        watchdog_version: ProductVersion::new("9".repeat(ProductVersion::MAX_BYTES)).unwrap(),
        exported_at_unix_ms: i64::MIN,
        host: HostSettingsEntry {
            workers: Some(WorkerCount::new(MAX_WORKERS).unwrap()),
            time_zone: Some(largest_zone()),
            notifications_enabled: false,
        },
        tunnels: BoundedVec::try_from((0..MAX_TUNNELS).map(entry).collect::<Vec<_>>()).unwrap(),
    }
}

#[test]
fn the_largest_settings_import_and_its_result_fit_in_one_message() {
    let request = RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("r".repeat(MAX_ID_LEN)).unwrap(),
        request: Request::ImportSettings {
            document: largest_document(),
            options: ImportOptions {
                match_by: ImportMatch::Name,
                register_missing: false,
                monitor_new: false,
                dry_run: false,
            },
        },
    };
    let bytes = encode_request(&request).unwrap();
    println!("largest settings import: {} of {LIMIT} bytes", bytes.len());
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_request(&bytes).unwrap(), request);

    let refused = || ImportOutcome::Refused {
        code: ErrorCode::StorageUnavailable,
        reason: Some(ErrorReason::NeedsAdministratorOrListedTunnel),
    };
    let item = |i: usize| ImportItem {
        index: u16::MAX,
        name: DisplayName::new("\"".repeat(DisplayName::MAX_BYTES)).unwrap(),
        tunnel_id: Some(TunnelId::new(format!("{i:0>MAX_ID_LEN$}")).unwrap()),
        outcome: refused(),
        auto_recovery: AutoRecoveryImport::Refused {
            code: ErrorCode::StorageUnavailable,
            reason: Some(ErrorReason::NeedsAdministratorOrListedTunnel),
        },
    };
    let answer = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap()),
        outcome: Outcome::Ok(Response::SettingsImport {
            result: ImportResult {
                dry_run: false,
                host: refused(),
                notifications: Some(refused()),
                tunnels: BoundedVec::try_from((0..MAX_TUNNELS).map(item).collect::<Vec<_>>())
                    .unwrap(),
            },
        }),
    };
    let bytes = encode_response(&answer).unwrap();
    println!(
        "largest settings import result: {} of {LIMIT} bytes",
        bytes.len()
    );
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), answer);

    let export = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("r".repeat(MAX_ID_LEN)).unwrap()),
        outcome: Outcome::Ok(Response::SettingsExport {
            document: largest_document(),
        }),
    };
    let bytes = encode_response(&export).unwrap();
    assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
    assert_eq!(decode_response(&bytes).unwrap(), export);
}
