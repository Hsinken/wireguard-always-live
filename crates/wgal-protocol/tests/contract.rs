//! Golden-fixture checks for the wire format. The TypeScript side asserts the
//! same files in `apps/desktop/src/protocol/contract.test.ts`.
//!
//! After an intended format change, regenerate with
//! `WGAL_UPDATE_FIXTURES=1 cargo test -p wgal-protocol` and review the diff.

#![allow(clippy::unwrap_used)]

use std::fmt::Debug;
use std::fs;
use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wgal_protocol::*;

fn tunnel(id: &str) -> TunnelId {
    TunnelId::new(id).unwrap()
}

fn request_id(id: &str) -> RequestId {
    RequestId::new(id).unwrap()
}

fn incident(id: &str) -> IncidentId {
    IncidentId::new(id).unwrap()
}

fn name(text: &str) -> DisplayName {
    DisplayName::new(text).unwrap()
}

fn detail(text: &str) -> Detail {
    Detail::new(text).unwrap()
}

fn zone(name: &str) -> TimeZoneName {
    TimeZoneName::new(name).unwrap()
}

/// A valid timing other than the default, so fixtures show every field.
fn slow_timing() -> ProbeTiming {
    ProbeTiming::new(30, 3, 1, 90).unwrap()
}

fn icmp(a: u8, b: u8, c: u8, d: u8) -> ProbeTarget {
    ProbeTarget {
        address: TargetAddress::new(Ipv4Addr::new(a, b, c, d)).unwrap(),
        probe: ProbeKind::Icmp,
    }
}

fn tcp(a: u8, b: u8, c: u8, d: u8, port: u16) -> ProbeTarget {
    ProbeTarget {
        address: TargetAddress::new(Ipv4Addr::new(a, b, c, d)).unwrap(),
        probe: ProbeKind::TcpConnect {
            port: NonZeroU16::new(port).unwrap(),
        },
    }
}

fn probed(target: ProbeTarget, finished_at_unix_ms: i64, result: ProbeResult) -> TargetStatus {
    TargetStatus {
        target,
        last_probe: Some(ProbeSummary {
            started_at_unix_ms: finished_at_unix_ms - 20,
            finished_at_unix_ms,
            result,
        }),
    }
}

fn assert_fixture<T>(name: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{name}.json"));
    let expected = serde_json::to_string_pretty(value).unwrap() + "\n";
    if std::env::var_os("WGAL_UPDATE_FIXTURES").is_some() {
        fs::write(&path, &expected).unwrap();
    }
    let actual = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "{name}.json differs from the Rust serialization"
    );
    assert_eq!(&serde_json::from_str::<T>(&actual).unwrap(), value);
}

#[test]
fn protocol_info_fixture() {
    assert_fixture("protocol_info", &ProtocolInfo::CURRENT);
}

#[test]
fn request_get_snapshot_fixture() {
    assert_fixture(
        "request_get_snapshot",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0001"),
            request: Request::GetSnapshot,
        },
    );
}

#[test]
fn request_set_auto_recovery_fixture() {
    assert_fixture(
        "request_set_auto_recovery",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0002"),
            request: Request::SetAutoRecovery {
                tunnel_id: tunnel("office-1"),
                enabled: false,
            },
        },
    );
}

#[test]
fn request_update_monitor_settings_fixture() {
    // Addresses are from the RFC 5737 documentation ranges; fixtures never hold
    // real ones.
    assert_fixture(
        "request_update_monitor_settings",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0004"),
            request: Request::UpdateMonitorSettings {
                tunnel_id: tunnel("office-1"),
                settings: MonitorSettings {
                    recovery_target: Some(icmp(192, 0, 2, 1)),
                    observe_targets: BoundedVec::try_from(vec![
                        tcp(192, 0, 2, 10, 443),
                        icmp(198, 51, 100, 20),
                    ])
                    .unwrap(),
                    probe: slow_timing(),
                },
            },
        },
    );
}

#[test]
fn request_update_notification_settings_fixture() {
    assert_fixture(
        "request_update_notification_settings",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0006"),
            request: Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("辦公室筆電").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        },
    );
}

#[test]
fn request_set_bot_token_fixture() {
    // An invented token in the Bot API format; no real bot has it.
    assert_fixture(
        "request_set_bot_token",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0007"),
            request: Request::SetBotToken {
                token: BotToken::new("123456789:EXAMPLE-not-a-real-token_0000000000").unwrap(),
            },
        },
    );
}

#[test]
fn request_send_test_notification_fixture() {
    assert_fixture(
        "request_send_test_notification",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0009"),
            request: Request::SendTestNotification,
        },
    );
}

#[test]
fn request_set_mcp_telegram_enabled_fixture() {
    assert_fixture(
        "request_set_mcp_telegram_enabled",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0021"),
            request: Request::SetMcpTelegramEnabled { enabled: true },
        },
    );
}

#[test]
fn request_set_monitoring_paused_fixture() {
    assert_fixture(
        "request_set_monitoring_paused",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0010"),
            request: Request::SetMonitoringPaused { paused: true },
        },
    );
}

#[test]
fn request_set_tunnel_monitoring_fixture() {
    assert_fixture(
        "request_set_tunnel_monitoring",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0011"),
            request: Request::SetTunnelMonitoring {
                tunnel_id: tunnel("lab-2"),
                enabled: false,
            },
        },
    );
}

#[test]
fn request_set_tunnel_notifications_fixture() {
    assert_fixture(
        "request_set_tunnel_notifications",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0012"),
            request: Request::SetTunnelNotifications {
                tunnel_id: tunnel("lab-2"),
                enabled: false,
            },
        },
    );
}

#[test]
fn request_set_recovery_retry_fixture() {
    assert_fixture(
        "request_set_recovery_retry",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0018"),
            request: Request::SetRecoveryRetry {
                tunnel_id: tunnel("office-1"),
                retry: RecoveryRetry::new(Some(100), 60).unwrap(),
            },
        },
    );
}

/// Owner decision of 2026-11-04: 0 to 3600 rounds after the first, or
/// `null` to keep trying, one every 10 to 10800 seconds.
#[test]
fn recovery_retry_is_checked_against_its_ranges() {
    let read = |json: serde_json::Value| serde_json::from_value::<RecoveryRetry>(json);
    for (limit, interval) in [(Some(0), 10), (Some(3600), 10800), (None, 60)] {
        let retry = read(serde_json::json!({ "limit": limit, "interval_secs": interval })).unwrap();
        assert_eq!(retry.limit(), limit);
        assert_eq!(retry.interval_secs(), interval);
    }
    for bad in [
        serde_json::json!({ "limit": 3601, "interval_secs": 60 }),
        serde_json::json!({ "limit": -1, "interval_secs": 60 }),
        serde_json::json!({ "limit": 5, "interval_secs": 9 }),
        serde_json::json!({ "limit": 5, "interval_secs": 10801 }),
        serde_json::json!({ "limit": 5 }),
        serde_json::json!({ "limit": 5, "interval_secs": 60, "backoff": [1, 2] }),
    ] {
        assert!(read(bad.clone()).is_err(), "{bad} should be rejected");
    }
    assert_eq!(
        RecoveryRetry::default(),
        RecoveryRetry::new(Some(100), 60).unwrap()
    );
    let retry = RecoveryRetry::new(Some(2), 60).unwrap();
    assert!(retry.allows(0) && retry.allows(1) && !retry.allows(2));
    assert!(!RecoveryRetry::new(Some(0), 60).unwrap().allows(0));
    assert!(RecoveryRetry::new(None, 60).unwrap().allows(u32::MAX));
}

/// A settings file with one tunnel of each kind of entry.
fn sample_document() -> SettingsDocument {
    SettingsDocument {
        format: SettingsFormat::WgalSettings,
        version: SETTINGS_DOCUMENT_VERSION,
        watchdog_version: ProductVersion::new("1.2.0").unwrap(),
        exported_at_unix_ms: 1_767_225_600_000,
        host: HostSettingsEntry {
            workers: None,
            time_zone: Some(zone("Asia/Taipei")),
            notifications_enabled: true,
        },
        tunnels: BoundedVec::try_from(vec![
            TunnelSettingsEntry {
                tunnel_id: tunnel("office-1"),
                name: name("Office"),
                monitoring: true,
                notifications: true,
                auto_recovery: true,
                start_when_stopped_externally: false,
                recovery_target: Some(icmp(192, 0, 2, 1)),
                observe_targets: BoundedVec::try_from(vec![tcp(198, 51, 100, 7, 443)]).unwrap(),
                probe: ProbeTiming::DEFAULT,
                retry: RecoveryRetry::DEFAULT,
            },
            TunnelSettingsEntry {
                tunnel_id: tunnel("lab-2"),
                name: name("實驗室"),
                monitoring: false,
                notifications: false,
                auto_recovery: false,
                start_when_stopped_externally: true,
                recovery_target: None,
                observe_targets: BoundedVec::new(),
                probe: slow_timing(),
                retry: RecoveryRetry::new(None, 300).unwrap(),
            },
        ])
        .unwrap(),
    }
}

#[test]
fn request_export_settings_fixture() {
    assert_fixture(
        "request_export_settings",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0019"),
            request: Request::ExportSettings,
        },
    );
}

#[test]
fn response_settings_export_fixture() {
    assert_fixture(
        "response_settings_export",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0019")),
            outcome: Outcome::Ok(Response::SettingsExport {
                document: sample_document(),
            }),
        },
    );
}

#[test]
fn request_import_settings_fixture() {
    assert_fixture(
        "request_import_settings",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0020"),
            request: Request::ImportSettings {
                document: sample_document(),
                options: ImportOptions {
                    match_by: ImportMatch::Name,
                    register_missing: true,
                    monitor_new: false,
                    dry_run: true,
                },
            },
        },
    );
}

#[test]
fn response_settings_import_fixture() {
    assert_fixture(
        "response_settings_import",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0020")),
            outcome: Outcome::Ok(Response::SettingsImport {
                result: ImportResult {
                    dry_run: true,
                    host: ImportOutcome::Applied,
                    notifications: Some(ImportOutcome::Applied),
                    tunnels: BoundedVec::try_from(vec![
                        ImportItem {
                            index: 0,
                            name: name("Office"),
                            tunnel_id: Some(tunnel("office-1")),
                            outcome: ImportOutcome::Registered,
                            auto_recovery: AutoRecoveryImport::WaitingForBaseline,
                        },
                        ImportItem {
                            index: 1,
                            name: name("實驗室"),
                            tunnel_id: None,
                            outcome: ImportOutcome::NotFound,
                            auto_recovery: AutoRecoveryImport::Off,
                        },
                    ])
                    .unwrap(),
                },
            }),
        },
    );
}

/// Owner decision of 2026-11-04: a settings file is read strictly. Another
/// version, another format, a field the contract does not have, or a secret
/// smuggled in is refused, with words that say why.
#[test]
fn a_settings_file_is_read_strictly() {
    let good = serde_json::to_value(sample_document()).unwrap();
    assert_eq!(
        serde_json::from_value::<SettingsDocument>(good.clone()).unwrap(),
        sample_document()
    );
    let mut later = good.clone();
    later["version"] = 2.into();
    let err = serde_json::from_value::<SettingsDocument>(later).unwrap_err();
    assert!(
        err.to_string()
            .contains("this settings file is version 2; this WGAL reads version 1 only"),
        "{err}"
    );
    for (path, value) in [
        ("/format", serde_json::json!("other")),
        ("/host/bot_token", serde_json::json!("123:abc")),
        ("/host/chat_id", serde_json::json!("-100")),
        ("/host/device_name", serde_json::json!("Laptop")),
        ("/tunnels/0/command", serde_json::json!("calc.exe")),
        ("/tunnels/0/retry/limit", serde_json::json!(3601)),
        ("/tunnels/0/probe/interval_secs", serde_json::json!(5)),
        ("/tunnels/0/tunnel_id", serde_json::json!("../x")),
        (
            "/tunnels/0/recovery_target/address",
            serde_json::json!("127.0.0.1"),
        ),
    ] {
        let mut bad = good.clone();
        let (parent, key) = path.rsplit_once('/').unwrap();
        bad.pointer_mut(if parent.is_empty() { "" } else { parent })
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.to_owned(), value);
        assert!(
            serde_json::from_value::<SettingsDocument>(bad).is_err(),
            "{path} should be refused"
        );
    }
    // At most one entry per possible tunnel.
    let mut many = good;
    let entry = many["tunnels"][0].clone();
    many["tunnels"] = serde_json::Value::Array(vec![entry; MAX_TUNNELS + 1]);
    assert!(serde_json::from_value::<SettingsDocument>(many).is_err());
}

#[test]
fn request_unregister_tunnel_fixture() {
    assert_fixture(
        "request_unregister_tunnel",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0013"),
            request: Request::UnregisterTunnel {
                tunnel_id: tunnel("lab-2"),
            },
        },
    );
}

#[test]
fn request_update_host_settings_fixture() {
    assert_fixture(
        "request_update_host_settings",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0014"),
            request: Request::UpdateHostSettings {
                settings: HostSettings {
                    workers: Some(WorkerCount::new(3).unwrap()),
                    time_zone: Some(zone("Asia/Taipei")),
                },
            },
        },
    );
}

#[test]
fn request_read_event_log_fixture() {
    assert_fixture(
        "request_read_event_log",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0015"),
            request: Request::ReadEventLog {
                filter: EventFilter::Tunnel {
                    tunnel_id: tunnel("home-3"),
                },
                before_seq: Some(1042),
            },
        },
    );
}

/// The watchdog's built-in message templates: the English texts it writes
/// with until a GUI or `wgal` hands over a set (`wgal-runtime`'s
/// `src/notify/message.rs`; English is the default language, owner decision
/// of 2026-11-18), as a set. `request_set_notification_templates.json` is
/// the reference that both the GUI's en language file and the watchdog's
/// built-in set must match, text for text. The ID is the one the GUI gives
/// its English set (its language and a hash of the texts); the built-in set
/// is reported as `templates_id: null`.
fn builtin_templates() -> NotificationTemplates {
    let text = |text: &str| MessageTemplate::new(text).unwrap();
    NotificationTemplates::new(
        TemplatesId::new("en/92c719a8").unwrap(),
        TemplateTexts {
            formal: text(
                "[WG Always Live] Fault alert\n\
                 Device: {device}\n\
                 Tunnel: {tunnel}\n\
                 Incident: {incident}\n\
                 Time zone: {time_zone}\n\
                 Fault started: {started_at}\n\
                 Recovery result: {result}\n\
                 {retry}",
            ),
            result_failed: text("The first automatic recovery failed (still no traffic after restarting the tunnel)"),
            result_not_executed: text("Automatic recovery could not run (the tunnel cannot be started or stopped)"),
            retry_limited: text(
                "WG Always Live will try again every {interval} seconds, up to {limit} times; you will be notified again once traffic is back and stable for 2 minutes.",
            ),
            retry_forever: text(
                "WG Always Live will keep trying every {interval} seconds; you will be notified again once traffic is back and stable for 2 minutes.",
            ),
            retry_none: text(
                "WG Always Live will not retry automatically; please handle it manually. You will be notified again once traffic is back and stable for 2 minutes.",
            ),
            exhausted: text(
                "[WG Always Live] Retries stopped\n\
                 Device: {device}\n\
                 Tunnel: {tunnel}\n\
                 Incident: {incident}\n\
                 Time zone: {time_zone}\n\
                 Fault started: {started_at}\n\
                 Retried automatically {retries} times and reached the limit; automatic recovery has stopped. Please handle it manually; you will be notified again once traffic is back and stable for 2 minutes.",
            ),
            recovered: text(
                "[WG Always Live] Recovered\n\
                 Device: {device}\n\
                 Tunnel: {tunnel}\n\
                 Incident: {incident}\n\
                 Time zone: {time_zone}\n\
                 Fault started: {started_at}\n\
                 Recovered at: {recovered_at}\n\
                 Traffic is back and has been stable for 2 minutes.",
            ),
            test: text(
                "[WG Always Live] Test notification\n\
                 This is a test message and does not mean any VPN fault.\n\
                 Device: {device}\n\
                 Time zone: {time_zone}\n\
                 Time: {time}",
            ),
            unnamed_device: text("(unnamed device)"),
        },
    )
    .unwrap()
}

#[test]
fn request_set_notification_templates_fixture() {
    assert_fixture(
        "request_set_notification_templates",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0017"),
            request: Request::SetNotificationTemplates {
                templates: builtin_templates(),
            },
        },
    );
}

#[test]
fn request_set_cli_language_fixture() {
    assert_fixture(
        "request_set_cli_language",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0022"),
            request: Request::SetCliLanguage {
                language: Some(LanguageTag::new("zh-TW").unwrap()),
            },
        },
    );
}

#[test]
fn request_clear_cli_language_fixture() {
    assert_fixture(
        "request_clear_cli_language",
        &RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id("req-0023"),
            request: Request::SetCliLanguage { language: None },
        },
    );
}

/// The shared language is given every time, `null` to clear it: a request
/// without it, with another type or with a field more is refused, so a
/// malformed request never clears it (owner decision of 2026-11-19).
#[test]
fn the_cli_language_request_is_strict() {
    let decode = |request: serde_json::Value| {
        serde_json::from_value::<RequestEnvelope>(serde_json::json!({
            "protocol_version": 1,
            "request_id": "r1",
            "request": request,
        }))
    };
    let set = decode(serde_json::json!({ "op": "set_cli_language", "language": "ja" })).unwrap();
    assert_eq!(
        set.request,
        Request::SetCliLanguage {
            language: Some(LanguageTag::new("ja").unwrap())
        }
    );
    let clear = decode(serde_json::json!({ "op": "set_cli_language", "language": null })).unwrap();
    assert_eq!(clear.request, Request::SetCliLanguage { language: None });
    assert_eq!(clear.request.op(), RequestOp::SetCliLanguage);
    assert!(!clear.request.reads_only());
    for bad in [
        serde_json::json!({ "op": "set_cli_language" }),
        serde_json::json!({ "op": "set_cli_language", "language": 7 }),
        serde_json::json!({ "op": "set_cli_language", "language": ["en"] }),
        serde_json::json!({ "op": "set_cli_language", "language": "zh--TW" }),
        serde_json::json!({ "op": "set_cli_language", "language": "../en" }),
        serde_json::json!({ "op": "set_cli_language", "language": "en", "path": "x" }),
    ] {
        assert!(decode(bad.clone()).is_err(), "{bad} should be refused");
    }
}

/// An answer from a watchdog before 4.0.0 has no `cli_language`: it reads
/// as never set.
#[test]
fn notification_status_without_a_cli_language_reads_as_none() {
    let json = serde_json::json!({
        "enabled": true,
        "device_name": "x",
        "chat_id": null,
        "token": "stored",
        "last_test": null,
        "last_failure": null,
        "templates_id": null,
    });
    let read = serde_json::from_value::<NotificationStatus>(json).unwrap();
    assert_eq!(read.cli_language, None);
    // Nor a host name before 4.1.0.
    assert_eq!(read.host_name, None);
}

/// Filled in as the watchdog fills them, the built-in templates give the
/// messages it has sent so far, character for character, but for the
/// alert's last line, which now says what automatic recovery does next
/// (owner decision of 2026-11-04), and the time zone, which is now a line
/// of its own instead of following each time (owner request of 2026-11-05).
#[test]
fn builtin_templates_write_the_same_messages_as_before() {
    let t = builtin_templates();
    let started_at = "2026-01-01 08:00:00";
    let zone = "Asia/Taipei, UTC+08:00";
    for (result, text) in [
        (
            t.result_failed(),
            "The first automatic recovery failed (still no traffic after restarting the tunnel)",
        ),
        (
            t.result_not_executed(),
            "Automatic recovery could not run (the tunnel cannot be started or stopped)",
        ),
    ] {
        let result = result.render(&[]);
        let formal = t.formal().render(&[
            ("device", "辦公室筆電"),
            ("tunnel", "*Office* <b>`x`</b> [l](http://e)"),
            ("incident", "inc-000007"),
            ("time_zone", zone),
            ("started_at", started_at),
            ("result", &result),
            (
                "retry",
                &t.retry_limited()
                    .render(&[("interval", "60"), ("limit", "100")]),
            ),
        ]);
        assert_eq!(
            formal,
            format!(
                "[WG Always Live] Fault alert\n\
                 Device: 辦公室筆電\n\
                 Tunnel: *Office* <b>`x`</b> [l](http://e)\n\
                 Incident: inc-000007\n\
                 Time zone: {zone}\n\
                 Fault started: {started_at}\n\
                 Recovery result: {text}\n\
                 WG Always Live will try again every 60 seconds, up to 100 times; you will be notified again once traffic is back and stable for 2 minutes."
            )
        );
    }
    assert_eq!(
        t.retry_forever().render(&[("interval", "300")]),
        "WG Always Live will keep trying every 300 seconds; you will be notified again once traffic is back and stable for 2 minutes."
    );
    assert_eq!(
        t.exhausted().render(&[
            ("device", "辦公室筆電"),
            ("tunnel", "Office"),
            ("incident", "inc-000007"),
            ("time_zone", zone),
            ("started_at", started_at),
            ("retries", "100"),
        ]),
        format!(
            "[WG Always Live] Retries stopped\n\
             Device: 辦公室筆電\n\
             Tunnel: Office\n\
             Incident: inc-000007\n\
             Time zone: {zone}\n\
             Fault started: {started_at}\n\
             Retried automatically 100 times and reached the limit; automatic recovery has stopped. Please handle it manually; you will be notified again once traffic is back and stable for 2 minutes."
        )
    );
    assert!(
        t.recovered()
            .render(&[
                ("recovered_at", "R"),
                ("started_at", "S"),
                ("time_zone", "Z")
            ])
            .contains("Time zone: Z\nFault started: S\nRecovered at: R\n")
    );
    let unnamed = t.unnamed_device().render(&[]);
    assert_eq!(unnamed, "(unnamed device)");
    assert_eq!(
        t.test().render(&[
            ("device", &unnamed),
            ("time_zone", "UTC"),
            ("time", "2026-01-01 00:00:00")
        ]),
        "[WG Always Live] Test notification\n\
         This is a test message and does not mean any VPN fault.\n\
         Device: (unnamed device)\n\
         Time zone: UTC\n\
         Time: 2026-01-01 00:00:00"
    );
}

#[test]
fn response_event_log_fixture() {
    let entry = |seq, at_unix_ms, tunnel_id: Option<&str>, event| EventEntry {
        seq,
        at_unix_ms,
        tunnel_id: tunnel_id.map(tunnel),
        removed_tunnel: false,
        event,
    };
    let home = Some("home-3");
    let entries = vec![
        entry(
            1042,
            1_767_225_970_000,
            home,
            LogEvent::RetriesExhausted { retries: 5 },
        ),
        entry(
            1041,
            1_767_225_361_500,
            home,
            LogEvent::NotificationSent {
                incident_id: incident("inc-0007"),
                notice: NoticeKind::Alert,
            },
        ),
        entry(
            1040,
            1_767_225_360_000,
            home,
            LogEvent::RecoveryFailed {
                next_retry_unix_ms: Some(1_767_225_420_000),
            },
        ),
        entry(
            1039,
            1_767_225_360_000,
            home,
            LogEvent::LightChanged {
                light: Light::Red,
                reason: LightReason::RecoveryFailed,
            },
        ),
        entry(
            1038,
            1_767_225_300_000,
            home,
            LogEvent::RecoveryStarted { attempt: 1 },
        ),
        entry(
            1037,
            1_767_225_300_000,
            home,
            LogEvent::FaultConfirmed {
                incident_id: incident("inc-0007"),
            },
        ),
        entry(
            1036,
            1_767_225_300_000,
            home,
            LogEvent::ProbeFailed {
                streak: 5,
                needed: 3,
            },
        ),
        EventEntry {
            removed_tunnel: true,
            ..entry(1035, 1_767_225_010_000, None, LogEvent::TunnelRemoved)
        },
        entry(1034, 1_767_225_000_000, None, LogEvent::WatchdogStarted),
    ];
    assert_fixture(
        "response_event_log",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0016")),
            outcome: Outcome::Ok(Response::EventLog {
                entries: BoundedVec::try_from(entries).unwrap(),
                more: true,
            }),
        },
    );
}

#[test]
fn response_snapshot_fixture() {
    let snapshot = Snapshot {
        generated_at_unix_ms: 1_767_225_600_000,
        storage: StorageHealth::Ok,
        capabilities: AdapterCapabilities {
            query_state: Capability::Supported,
            start_stop: Capability::Supported,
            bound_icmp_probe: Capability::Supported,
            bound_tcp_probe: Capability::Unknown,
            read_metrics: Capability::Unsupported,
            verified_concurrent_tunnels: 1,
            background_operation: Capability::Unknown,
        },
        monitoring_paused: false,
        host: HostStatus {
            watchdog_version: ProductVersion::new("1.0.0").unwrap(),
            min_gui_version: ProductVersion::new("1.0.0").unwrap(),
            min_usable_gui_version: Some(ProductVersion::new("1.0.0").unwrap()),
            mcp_enabled: false,
            mcp_telegram_enabled: false,
            workers: WorkerStatus {
                configured: None,
                suggested: 4,
                limit: 4,
                effective: 4,
                running: 3,
            },
            time_zone: TimeZoneStatus {
                configured: Some(zone("Asia/Taipei")),
                effective: Some(zone("Asia/Taipei")),
                utc_offset_secs: 8 * 3600,
            },
        },
        notification: NotificationStatus {
            enabled: true,
            device_name: DeviceName::new("辦公室筆電").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            token: CredentialState::Stored,
            last_test: Some(TestNotificationStatus {
                request_id: request_id("req-0009"),
                state: TestNotificationState::Accepted,
                requested_at_unix_ms: 1_767_225_500_000,
                finished_at_unix_ms: Some(1_767_225_501_200),
                failure: None,
            }),
            last_failure: None,
            templates_id: Some(TemplatesId::new("zh-TW/3f2a91c0").unwrap()),
            cli_language: Some(LanguageTag::new("zh-TW").unwrap()),
            host_name: Some(DeviceName::new("office-pc").unwrap()),
        },
        tunnels: BoundedVec::try_from(vec![
            TunnelStatus {
                tunnel_id: tunnel("office-1"),
                display_name: name("Office"),
                light: Light::Green,
                light_reason: LightReason::Verified,
                light_since_unix_ms: 1_767_225_540_000,
                deadline_unix_ms: None,
                next_retry_unix_ms: None,
                last_verified_unix_ms: Some(1_767_225_599_000),
                intent: UserIntent {
                    desired: DesiredState::Connected,
                    monitoring_enabled: true,
                    auto_recovery_enabled: true,
                    start_when_stopped_externally: false,
                },
                auto_recovery_allowed: true,
                notifications_enabled: true,
                takeover: Takeover::Managed,
                os_state: OsState::Connected,
                data_health: DataHealth::Healthy,
                recovery: RecoveryPhase::Idle,
                recovery_target: Some(probed(
                    icmp(192, 0, 2, 1),
                    1_767_225_599_000,
                    ProbeResult::Reply { rtt_ms: 12 },
                )),
                observe_targets: BoundedVec::try_from(vec![probed(
                    tcp(192, 0, 2, 10, 443),
                    1_767_225_599_100,
                    ProbeResult::NoReply,
                )])
                .unwrap(),
                probe: ProbeTiming::DEFAULT,
                freshness_secs: 35,
                consecutive_failures: 0,
                fault: None,
                incident: None,
                retry: RecoveryRetry::DEFAULT,
                recovered_notification: None,
                auto_recovery_pending: false,
            },
            TunnelStatus {
                tunnel_id: tunnel("lab-2"),
                display_name: name("實驗室"),
                light: Light::Red,
                light_reason: LightReason::ProbeUnavailable,
                light_since_unix_ms: 1_767_225_480_000,
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
                notifications_enabled: false,
                takeover: Takeover::ObserveOnly,
                os_state: OsState::Connected,
                data_health: DataHealth::Unknown,
                recovery: RecoveryPhase::Blocked,
                recovery_target: Some(probed(
                    tcp(192, 0, 2, 2, 443),
                    1_767_225_598_000,
                    ProbeResult::Error {
                        error: AdapterErrorKind::PermissionDenied,
                    },
                )),
                observe_targets: BoundedVec::new(),
                probe: slow_timing(),
                freshness_secs: 51,
                consecutive_failures: 0,
                fault: Some(Fault {
                    kind: AdapterErrorKind::PermissionDenied,
                    message: detail("probe tool not permitted"),
                }),
                incident: None,
                retry: RecoveryRetry::new(None, 300).unwrap(),
                recovered_notification: None,
                auto_recovery_pending: false,
            },
            TunnelStatus {
                tunnel_id: tunnel("home-3"),
                display_name: name("Home"),
                light: Light::Red,
                light_reason: LightReason::RecoveryFailed,
                light_since_unix_ms: 1_767_225_420_000,
                deadline_unix_ms: None,
                next_retry_unix_ms: Some(1_767_225_660_000),
                last_verified_unix_ms: Some(1_767_225_100_000),
                intent: UserIntent {
                    desired: DesiredState::Connected,
                    monitoring_enabled: true,
                    auto_recovery_enabled: true,
                    start_when_stopped_externally: false,
                },
                auto_recovery_allowed: true,
                notifications_enabled: true,
                takeover: Takeover::Managed,
                os_state: OsState::Connected,
                data_health: DataHealth::Failed,
                recovery: RecoveryPhase::Backoff,
                recovery_target: Some(probed(
                    icmp(203, 0, 113, 1),
                    1_767_225_597_000,
                    ProbeResult::NoReply,
                )),
                observe_targets: BoundedVec::new(),
                probe: ProbeTiming::DEFAULT,
                freshness_secs: 35,
                consecutive_failures: 9,
                fault: None,
                incident: Some(IncidentStatus {
                    incident_id: incident("inc-0007"),
                    opened_at_unix_ms: 1_767_225_300_000,
                    first_recovery: FirstRecovery::Failed,
                    backoff_step: 1,
                    stable_since_unix_ms: None,
                    notification: NotificationState::Sent,
                    retries_used: 1,
                    retries_exhausted: false,
                    exhausted_notification: NotificationState::NotEligible,
                }),
                retry: RecoveryRetry::new(Some(5), 120).unwrap(),
                recovered_notification: Some(RecoveredNotification {
                    incident_id: incident("inc-0006"),
                    notification: NotificationState::Sent,
                }),
                auto_recovery_pending: false,
            },
        ])
        .unwrap(),
    };
    assert_fixture(
        "response_snapshot",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0001")),
            outcome: Outcome::Ok(Response::Snapshot { snapshot }),
        },
    );
}

#[test]
fn response_discovered_fixture() {
    let found = |id: &str, display: &str, control| DiscoveredTunnel {
        tunnel_id: tunnel(id),
        display_name: name(display),
        control,
    };
    assert_fixture(
        "response_discovered",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0005")),
            outcome: Outcome::Ok(Response::Discovered {
                tunnels: BoundedVec::try_from(vec![
                    found("office-1", "Office", Capability::Supported),
                    found("lab-2", "實驗室", Capability::Supported),
                    found("home-3", "Home (NetworkManager)", Capability::Unsupported),
                ])
                .unwrap(),
            }),
        },
    );
}

#[test]
fn response_error_fixture() {
    assert_fixture(
        "response_error",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request_id("req-0003")),
            outcome: Outcome::Err(ProtocolError {
                code: ErrorCode::UnknownTunnel,
                reason: Some(ErrorReason::TunnelNotRegistered),
                message: detail("tunnel is not registered"),
            }),
        },
    );
}

#[test]
fn response_message_too_large_fixture() {
    // An oversized request is rejected before parsing, so its ID is unknown.
    // The code says it all, so there is no reason.
    assert_fixture(
        "response_message_too_large",
        &ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: None,
            outcome: Outcome::Err(ProtocolError {
                code: ErrorCode::MessageTooLarge,
                reason: None,
                message: detail("request exceeds 65536 bytes"),
            }),
        },
    );
}

#[test]
fn errors_carry_a_reason_from_a_closed_set() {
    let error = ProtocolError::new(
        ErrorCode::Unauthorized,
        Some(ErrorReason::NeedsAdministratorOrListedTunnel),
        "line 1\nline 2",
    );
    let json = serde_json::json!({
        "code": "unauthorized",
        "reason": "needs_administrator_or_listed_tunnel",
        "message": "line 1 line 2",
    });
    assert_eq!(serde_json::to_value(&error).unwrap(), json);
    assert_eq!(
        serde_json::from_value::<ProtocolError>(json).unwrap(),
        error
    );

    let long = ProtocolError::new(ErrorCode::Internal, None, &"x".repeat(1000));
    assert_eq!(long.message.as_str().len(), Detail::MAX_BYTES);

    // An answer from a watchdog that sends no reasons yet still decodes.
    let without = serde_json::json!({ "code": "busy", "message": "x" });
    assert_eq!(
        serde_json::from_value::<ProtocolError>(without).unwrap(),
        ProtocolError::new(ErrorCode::Busy, None, "x")
    );
    for bad in [
        serde_json::json!({ "code": "busy", "reason": "something_new", "message": "x" }),
        serde_json::json!({ "code": "busy", "reason": "ShuttingDown", "message": "x" }),
    ] {
        assert!(
            serde_json::from_value::<ProtocolError>(bad.clone()).is_err(),
            "{bad} should be rejected"
        );
    }
    // A field a newer watchdog added is ignored (answers are read leniently).
    let later = serde_json::json!({ "code": "busy", "reason": null, "message": "x", "text": "y" });
    assert_eq!(
        serde_json::from_value::<ProtocolError>(later).unwrap(),
        ProtocolError::new(ErrorCode::Busy, None, "x")
    );
}

/// The contract's limits as the GUI needs them. `contract.test.ts` checks the
/// TypeScript constants against `limits.json`.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    max_tunnels: usize,
    max_discovered: usize,
    max_observe_targets: usize,
    max_event_page: usize,
    min_workers: u8,
    max_workers: u8,
    probe_interval_secs: Range<u16>,
    probe_timeout_secs: Range<u8>,
    probe_retries: Range<u8>,
    probe_min_fault_secs: Range<u16>,
    default_probe_timing: ProbeTiming,
    recovery_retry_limit: Range<u16>,
    recovery_retry_interval_secs: Range<u16>,
    default_recovery_retry: RecoveryRetry,
    min_gui_version: String,
    min_usable_gui_version: String,
    template_placeholders: TemplatePlaceholders,
    bot_id_digits: Range<usize>,
    bot_secret_chars: Range<usize>,
    chat_id_digits: Range<usize>,
    chat_username_chars: Range<usize>,
    device_name_max_chars: usize,
}

/// `NotificationTemplates`' placeholder names, per template.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplatePlaceholders {
    formal: Vec<String>,
    result_failed: Vec<String>,
    result_not_executed: Vec<String>,
    retry_limited: Vec<String>,
    retry_forever: Vec<String>,
    retry_none: Vec<String>,
    exhausted: Vec<String>,
    recovered: Vec<String>,
    test: Vec<String>,
    unnamed_device: Vec<String>,
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Range<T> {
    min: T,
    max: T,
}

fn range<T: Copy>(r: RangeInclusive<T>) -> Range<T> {
    Range {
        min: *r.start(),
        max: *r.end(),
    }
}

#[test]
fn limits_fixture() {
    assert_fixture(
        "limits",
        &Limits {
            max_tunnels: MAX_TUNNELS,
            max_discovered: MAX_DISCOVERED,
            max_observe_targets: MAX_OBSERVE_TARGETS,
            max_event_page: MAX_EVENT_PAGE,
            min_workers: MIN_WORKERS,
            max_workers: MAX_WORKERS,
            probe_interval_secs: range(ProbeTiming::INTERVAL_SECS),
            probe_timeout_secs: range(ProbeTiming::TIMEOUT_SECS),
            probe_retries: range(ProbeTiming::RETRIES),
            probe_min_fault_secs: range(ProbeTiming::MIN_FAULT_SECS),
            default_probe_timing: ProbeTiming::DEFAULT,
            recovery_retry_limit: range(RecoveryRetry::LIMIT),
            recovery_retry_interval_secs: range(RecoveryRetry::INTERVAL_SECS),
            default_recovery_retry: RecoveryRetry::DEFAULT,
            min_gui_version: MIN_GUI_VERSION.to_owned(),
            min_usable_gui_version: MIN_USABLE_GUI_VERSION.to_owned(),
            template_placeholders: TemplatePlaceholders {
                formal: names(NotificationTemplates::FORMAL_PLACEHOLDERS),
                result_failed: names(NotificationTemplates::RESULT_FAILED_PLACEHOLDERS),
                result_not_executed: names(NotificationTemplates::RESULT_NOT_EXECUTED_PLACEHOLDERS),
                retry_limited: names(NotificationTemplates::RETRY_LIMITED_PLACEHOLDERS),
                retry_forever: names(NotificationTemplates::RETRY_FOREVER_PLACEHOLDERS),
                retry_none: names(NotificationTemplates::RETRY_NONE_PLACEHOLDERS),
                exhausted: names(NotificationTemplates::EXHAUSTED_PLACEHOLDERS),
                recovered: names(NotificationTemplates::RECOVERED_PLACEHOLDERS),
                test: names(NotificationTemplates::TEST_PLACEHOLDERS),
                unnamed_device: names(NotificationTemplates::UNNAMED_DEVICE_PLACEHOLDERS),
            },
            bot_id_digits: range(BotToken::BOT_ID_DIGITS),
            bot_secret_chars: range(BotToken::SECRET_CHARS),
            chat_id_digits: range(ChatId::DIGITS),
            chat_username_chars: range(ChatId::USERNAME_CHARS),
            device_name_max_chars: DeviceName::MAX_CHARS,
        },
    );
}

#[test]
fn rejects_operations_outside_the_allowlist() {
    let unknown_op = r#"{"protocol_version":1,"request_id":"r1",
        "request":{"op":"execute","command":"calc.exe"}}"#;
    assert!(serde_json::from_str::<RequestEnvelope>(unknown_op).is_err());
}

#[test]
fn snapshots_cannot_carry_a_bot_token() {
    let token = "123456789:EXAMPLE-not-a-real-token_0000000000";
    let json = serde_json::json!({
        "enabled": true,
        "device_name": "x",
        "chat_id": null,
        "token": token,
        "last_test": null,
        "last_failure": null,
        "templates_id": null,
    });
    assert!(serde_json::from_value::<NotificationStatus>(json).is_err());
    let json = serde_json::json!({
        "enabled": true,
        "device_name": "x",
        "chat_id": null,
        "token": "stored",
        "bot_token": token,
        "last_test": null,
        "last_failure": null,
        "templates_id": null,
    });
    // Answers are read leniently (owner decision of 2026-11-04), so a field
    // the contract does not have is dropped as it is read: the token never
    // reaches what the GUI is handed.
    let read = serde_json::from_value::<NotificationStatus>(json).unwrap();
    assert!(!serde_json::to_string(&read).unwrap().contains(token));
}

/// Owner decision of 2026-11-04: a minimum version is what makes every
/// function work, so a GUI must be able to read a newer watchdog's answers
/// to say it is too old. Fields an answer has that this contract does not
/// are ignored at every level; requests stay strict
/// (`rejects_smuggled_fields`).
#[test]
fn answers_from_a_newer_watchdog_are_read_without_their_new_fields() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path.join(name)).unwrap()).unwrap()
    };
    let mut snapshot = read("response_snapshot.json");
    let expected: ResponseEnvelope = serde_json::from_value(snapshot.clone()).unwrap();
    snapshot["later"] = serde_json::json!(1);
    snapshot["outcome"]["ok"]["later"] = serde_json::json!("x");
    let body = &mut snapshot["outcome"]["ok"]["snapshot"];
    body["later"] = serde_json::json!([1, 2]);
    body["host"]["later"] = serde_json::json!({ "a": 1 });
    body["host"]["workers"]["later"] = serde_json::json!(true);
    body["notification"]["later"] = serde_json::json!(null);
    body["capabilities"]["later"] = serde_json::json!("supported");
    let tunnel = &mut body["tunnels"][0];
    tunnel["later"] = serde_json::json!(0);
    tunnel["intent"]["later"] = serde_json::json!(0);
    let decoded: ResponseEnvelope = serde_json::from_value(snapshot).unwrap();
    assert_eq!(decoded, expected);

    let mut log = read("response_event_log.json");
    let expected: ResponseEnvelope = serde_json::from_value(log.clone()).unwrap();
    log["outcome"]["ok"]["entries"][0]["later"] = serde_json::json!(1);
    log["outcome"]["ok"]["entries"][0]["event"]["later"] = serde_json::json!(1);
    assert_eq!(
        serde_json::from_value::<ResponseEnvelope>(log).unwrap(),
        expected
    );

    let mut error = read("response_error.json");
    let expected: ResponseEnvelope = serde_json::from_value(error.clone()).unwrap();
    error["outcome"]["err"]["later"] = serde_json::json!(1);
    assert_eq!(
        serde_json::from_value::<ResponseEnvelope>(error).unwrap(),
        expected
    );
}

/// The answers of a 1.1.0 watchdog, frozen in `fixtures/compat/`, still
/// decode: what was added since has a default.
#[test]
fn answers_from_a_1_1_0_watchdog_still_decode() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/compat");
    for name in [
        "response_snapshot-1.1.0.json",
        "response_event_log-1.1.0.json",
        "response_discovered-1.1.0.json",
    ] {
        let text = fs::read_to_string(path.join(name)).unwrap();
        let decoded = codec::decode_response(text.as_bytes());
        assert!(decoded.is_ok(), "{name}: {decoded:?}");
    }
    let text = fs::read_to_string(path.join("response_snapshot-1.1.0.json")).unwrap();
    let Ok(ResponseEnvelope {
        outcome: Outcome::Ok(Response::Snapshot { snapshot }),
        ..
    }) = codec::decode_response(text.as_bytes())
    else {
        panic!("not a snapshot");
    };
    assert_eq!(snapshot.host.min_usable_gui_version, None);
}

#[test]
fn rejects_smuggled_fields() {
    let extra_in_request = r#"{"protocol_version":1,"request_id":"r1",
        "request":{"op":"connect","tunnel_id":"office-1","command":"calc.exe"}}"#;
    let extra_in_envelope = r#"{"protocol_version":1,"request_id":"r1",
        "request":{"op":"get_snapshot"},"service_name":"anything"}"#;
    assert!(serde_json::from_str::<RequestEnvelope>(extra_in_request).is_err());
    assert!(serde_json::from_str::<RequestEnvelope>(extra_in_envelope).is_err());
}

#[test]
fn rejects_invalid_tunnel_ids_in_requests() {
    for bad in ["", "wg0; reboot", "../x", "WireGuardTunnel$office"] {
        let json = serde_json::json!({
            "protocol_version": 1,
            "request_id": "r1",
            "request": { "op": "disconnect", "tunnel_id": bad },
        });
        assert!(
            serde_json::from_value::<RequestEnvelope>(json).is_err(),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn rejects_probe_targets_that_cannot_be_a_tunnel_host() {
    for (address, port) in [
        ("127.0.0.1", 443),
        ("0.0.0.0", 443),
        ("224.0.0.1", 443),
        ("255.255.255.255", 443),
        ("example.com", 443),
        ("192.0.2.1", 0),
    ] {
        let json = serde_json::json!({
            "protocol_version": 1,
            "request_id": "r1",
            "request": {
                "op": "update_monitor_settings",
                "tunnel_id": "office-1",
                "settings": {
                    "recovery_target": {
                        "address": address,
                        "probe": { "kind": "tcp_connect", "port": port },
                    },
                    "observe_targets": [],
                    "probe": ProbeTiming::DEFAULT,
                },
            },
        });
        assert!(
            serde_json::from_value::<RequestEnvelope>(json).is_err(),
            "{address}:{port} should be rejected"
        );
    }
}

#[test]
fn rejects_probe_timing_out_of_range_or_in_the_old_shape() {
    let request = |settings: serde_json::Value| {
        serde_json::json!({
            "protocol_version": 1,
            "request_id": "r1",
            "request": {
                "op": "update_monitor_settings",
                "tunnel_id": "office-1",
                "settings": settings,
            },
        })
    };
    let timing = |interval: u32, timeout: u32, retries: u32, min_fault: u32| {
        serde_json::json!({
            "recovery_target": null,
            "observe_targets": [],
            "probe": {
                "interval_secs": interval,
                "timeout_secs": timeout,
                "retries": retries,
                "min_fault_secs": min_fault,
            },
        })
    };
    assert!(serde_json::from_value::<RequestEnvelope>(request(timing(10, 1, 0, 0))).is_ok());
    assert!(serde_json::from_value::<RequestEnvelope>(request(timing(3600, 10, 10, 3600))).is_ok());
    for bad in [
        timing(9, 2, 2, 60),
        timing(3601, 2, 2, 60),
        timing(15, 0, 2, 60),
        timing(15, 11, 2, 60),
        timing(15, 2, 11, 60),
        timing(15, 2, 2, 3601),
        // The earlier shape, with the monitoring switch and no timing.
        serde_json::json!({
            "monitoring_enabled": true,
            "recovery_target": null,
            "observe_targets": [],
        }),
    ] {
        assert!(
            serde_json::from_value::<RequestEnvelope>(request(bad.clone())).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn rejects_out_of_range_host_settings() {
    let request = |workers: serde_json::Value, time_zone: &str| {
        serde_json::json!({
            "protocol_version": 1,
            "request_id": "r1",
            "request": {
                "op": "update_host_settings",
                "settings": { "workers": workers, "time_zone": time_zone },
            },
        })
    };
    assert!(serde_json::from_value::<RequestEnvelope>(request(2.into(), "UTC")).is_ok());
    assert!(serde_json::from_value::<RequestEnvelope>(request(17.into(), "Etc/GMT+8")).is_ok());
    for (workers, time_zone) in [
        (serde_json::Value::from(1), "UTC"),
        (18.into(), "UTC"),
        (2.into(), "../../etc/passwd"),
        (2.into(), "C:\\Windows\\win.ini"),
        (2.into(), ""),
    ] {
        assert!(
            serde_json::from_value::<RequestEnvelope>(request(workers.clone(), time_zone)).is_err(),
            "{workers} {time_zone:?} should be rejected"
        );
    }
}

/// ts-rs leaves `", "` at the end of the line before a documented field, so
/// contract types carry their documentation on the type, not on fields.
#[test]
fn generated_files_have_no_trailing_whitespace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dirs = [
        root.join("../../apps/desktop/src/protocol/generated"),
        root.join("fixtures"),
        root.join("fixtures/compat"),
    ];
    let mut offenders = Vec::new();
    for dir in dirs {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.ends_with([' ', '\t']) {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "trailing whitespace in {offenders:#?}"
    );
}
