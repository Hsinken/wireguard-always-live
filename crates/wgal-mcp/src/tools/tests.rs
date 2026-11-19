//! The tools against a scripted watchdog: what each sends, that arguments
//! out of range are refused with words that say the range, and that no
//! answer gives out the token, or the chat unless the window lets agents
//! set Telegram.

#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::path::PathBuf;

use serde_json::{Value, json};
use wgal_protocol::{
    BotToken, ChatId, ErrorCode, ErrorReason, Outcome, PROTOCOL_VERSION, ProductVersion,
    ProtocolError, Request, RequestId, Response, ResponseEnvelope, Snapshot,
};

use super::*;
use wgal_client::token::HIDDEN_TOKEN;
use wgal_client::{Unreachable, Watchdog};

/// The snapshot fixture, with a chat set, as the watchdog answers it.
fn snapshot() -> Snapshot {
    let text = include_str!("../../../wgal-protocol/fixtures/response_snapshot.json");
    let envelope: ResponseEnvelope = serde_json::from_str(text).unwrap();
    let Outcome::Ok(Response::Snapshot { mut snapshot }) = envelope.outcome else {
        panic!("not a snapshot");
    };
    snapshot.notification.chat_id = Some(ChatId::new("-1009876543210").unwrap());
    snapshot
}

#[derive(Default)]
struct Fake {
    sent: Vec<Request>,
    /// Answers for requests other than snapshots, in order; `Accepted` when
    /// empty.
    answers: VecDeque<Outcome>,
    /// Changes the snapshot before it is answered.
    tune: Option<fn(&mut Snapshot)>,
    /// Changes every snapshot after the first: a setting that changed.
    after_first: Option<fn(&mut Snapshot)>,
    /// Refuses snapshots from this one on (0-based).
    snapshots_fail_from: Option<usize>,
    /// Changes every snapshot after a `ProbeNow`: the probe's results.
    after_probe: Option<fn(&mut Snapshot)>,
    /// Requests whose answer is lost: each is recorded as sent and gets no
    /// answer, as when the watchdog does not answer in time.
    lose: Option<fn(&Request) -> bool>,
    probed: bool,
    snapshots: usize,
}

/// Applies `change` to every tunnel of the snapshot.
fn each(s: &mut Snapshot, change: fn(&mut TunnelStatus)) {
    let mut tunnels = std::mem::take(&mut s.tunnels).into_vec();
    tunnels.iter_mut().for_each(change);
    s.tunnels = BoundedVec::try_from(tunnels).unwrap();
}

impl Watchdog for Fake {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.sent.push(request.clone());
        if matches!(request, Request::ProbeNow { .. }) {
            self.probed = true;
        }
        if self.lose.is_some_and(|lose| lose(request)) {
            return Err(Unreachable::unknown(
                "the watchdog did not answer in time; the request may still be carried out",
            ));
        }
        let outcome = match request {
            Request::GetSnapshot
                if self
                    .snapshots_fail_from
                    .is_some_and(|from| self.snapshots >= from) =>
            {
                self.snapshots += 1;
                Outcome::Err(ProtocolError::new(
                    ErrorCode::Busy,
                    Some(ErrorReason::TooManyWaiting),
                    "busy",
                ))
            }
            Request::GetSnapshot => {
                let mut snapshot = snapshot();
                if let Some(tune) = self.tune {
                    tune(&mut snapshot);
                }
                if self.snapshots > 0
                    && let Some(change) = self.after_first
                {
                    change(&mut snapshot);
                }
                if self.probed
                    && let Some(change) = self.after_probe
                {
                    change(&mut snapshot);
                }
                self.snapshots += 1;
                Outcome::Ok(Response::Snapshot { snapshot })
            }
            _ => self
                .answers
                .pop_front()
                .unwrap_or(Outcome::Ok(Response::Accepted)),
        };
        Ok(ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(RequestId::new("r").unwrap()),
            outcome,
        })
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        Ok(vec![(
            "event_log".to_owned(),
            PathBuf::from(r"C:\ProgramData\WGAL\events\events.jsonl"),
        )])
    }
}

fn run(fake: &mut Fake, tool: &str, arguments: Value) -> Value {
    call(fake, tool, &arguments)
        .expect("a known tool")
        .into_value()
}

fn failed(result: &Value) -> bool {
    result["isError"] == json!(true)
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_owned()
}

#[test]
fn every_tool_is_listed_with_a_closed_schema() {
    let tools = definitions();
    assert_eq!(tools.len(), 22);
    for tool in &tools {
        assert_eq!(
            tool["inputSchema"]["additionalProperties"],
            json!(false),
            "{tool}"
        );
        let name = tool["name"].as_str().unwrap();
        assert!(
            name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{name}"
        );
        assert!(tool["description"].as_str().unwrap().len() > 40, "{name}");
    }
    for name in ["connect_tunnel", "disconnect_tunnel", "reconnect_tunnel"] {
        let tool = tools.iter().find(|t| t["name"] == json!(name)).unwrap();
        assert!(
            tool["description"]
                .as_str()
                .unwrap()
                .starts_with("WARNING:"),
            "{name}"
        );
        assert_eq!(tool["annotations"]["destructiveHint"], json!(true));
    }
    assert!(call(&mut Fake::default(), "rm_rf", &json!({})).is_none());
}

#[test]
fn health_holds_every_tunnel_and_never_the_chat_or_token() {
    let mut fake = Fake::default();
    let result = run(&mut fake, "health", json!({}));
    assert!(!failed(&result));
    let all = result.to_string();
    assert!(
        !all.contains("-1009876543210"),
        "the chat ID is never given out"
    );
    assert_eq!(
        result["structuredContent"]["notifications"]["chat_configured"],
        json!(true)
    );
    assert_eq!(
        result["structuredContent"]["tunnels"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(fake.sent, [Request::GetSnapshot]);
}

#[test]
fn arguments_are_checked_and_the_error_says_what_is_accepted() {
    let mut fake = Fake::default();
    let cases = [
        ("health", json!({ "x": 1 }), "takes no arguments"),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1" }),
            "enabled is required: true or false",
        ),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "../x", "enabled": true }),
            "1 to 64 ASCII letters",
        ),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1", "enabled": "yes" }),
            "must be true or false",
        ),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1", "enabled": true, "command": "calc" }),
            "unknown argument \"command\"; it takes: tunnel_id, enabled",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "retry": { "limit": 3601 } }),
            "retry.limit must be a whole number from 0 to 3600",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "retry": { "interval_secs": 5 } }),
            "retry.interval_secs must be a whole number from 10 to 10800",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe": { "interval_secs": 1 } }),
            "probe.interval_secs must be a whole number from 10 to 3600",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "example.com" } }),
            "is not an IPv4 address",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "127.0.0.1" } }),
            "127.0.0.1",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "192.0.2.1", "port": 70000 } }),
            "port must be a whole number from 1 to 65535",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1" }),
            "nothing to change",
        ),
        // null means something only for recovery_target and retry.limit.
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe": { "interval_secs": null } }),
            "probe.interval_secs cannot be null: give a whole number from 10 to 3600, or leave probe.interval_secs out to keep it",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "retry": { "interval_secs": null } }),
            "retry.interval_secs cannot be null",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "observe_targets": null }),
            "[] removes them all",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe": null }),
            "leave probe out to keep them all",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "192.0.2.1", "port": null } }),
            "recovery_target.port cannot be null: give a TCP port from 1 to 65535, or leave port out for ICMP",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "observe_targets": [{ "address": "192.0.2.10", "port": null }] }),
            "observe_targets[0].port cannot be null",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "192.0.2.1", "port": 443, "probe": { "kind": "icmp" } } }),
            "has both port and probe",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "192.0.2.1", "probe": { "kind": "tcp_connect", "port": null } } }),
            "recovery_target.probe must be",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": "192.0.2.1", "probe": { "kind": "udp" } } }),
            "recovery_target.probe must be",
        ),
        // A probe or retry setting given flat says where it belongs.
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe_interval_secs": 60 }),
            "unknown argument \"probe_interval_secs\": probe settings go inside probe, for example {\"probe\": {\"interval_secs\": 60}}; it takes: tunnel_id, recovery_target, observe_targets, probe, retry",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "retry_limit": "many" }),
            "retry settings go inside retry, for example {\"retry\": {\"limit\": ...}}",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "timeout": 3 }),
            "timeout_secs goes inside probe, for example {\"probe\": {\"timeout_secs\": 3}}",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "interval_secs": 60 }),
            "{\"probe\": {\"interval_secs\": 60}} or {\"retry\": {\"interval_secs\": 60}}",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe": { "probe_interval_secs": 60 } }),
            "unknown argument \"probe_interval_secs\": inside probe it is called \"interval_secs\"; it takes: interval_secs, timeout_secs, retries, min_fault_secs",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "retry": { "interval": 60 } }),
            "inside retry it is called \"interval_secs\"",
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "name": "x" }),
            "unknown argument \"name\"; it takes:",
        ),
        (
            "get_tunnel",
            json!({ "tunnel_id": "office-1", "verbose": true }),
            "unknown argument \"verbose\"; it takes: tunnel_id",
        ),
        (
            "update_host_settings",
            json!({ "workers": 99 }),
            "workers must be a whole number from 2 to 17",
        ),
        (
            "update_host_settings",
            json!({ "time_zone": "Asia Taipei" }),
            "Asia/Taipei",
        ),
        ("import_settings", json!({}), "document is required"),
        (
            "import_settings",
            json!({ "document": { "format": "wgal-settings", "version": 2 } }),
            "document.version is 2; this WGAL reads version 1 only",
        ),
        (
            "import_settings",
            json!({ "document": {}, "match_by": "uuid" }),
            "match_by must be \"id\" or \"name\"",
        ),
        (
            "connect_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 999 }),
            "wait_secs must be a whole number from 0 to 120",
        ),
    ];
    for (tool, arguments, words) in cases {
        let result = run(&mut fake, tool, arguments.clone());
        assert!(failed(&result), "{tool} {arguments} was not refused");
        assert!(
            text(&result).contains(words),
            "{tool} {arguments}: {}",
            text(&result)
        );
    }
    // A wrong argument sends nothing that changes anything.
    assert!(
        fake.sent.iter().all(|r| matches!(r, Request::GetSnapshot)),
        "{:?}",
        fake.sent
    );
}

#[test]
fn a_partial_update_keeps_the_rest() {
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "update_tunnel_settings",
        json!({ "tunnel_id": "office-1", "retry": { "limit": null } }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let retry = fake.sent.iter().find_map(|r| match r {
        Request::SetRecoveryRetry { retry, .. } => Some(*retry),
        _ => None,
    });
    let before = snapshot().tunnels[0].retry;
    assert_eq!(retry.unwrap().limit(), None, "null keeps trying");
    assert_eq!(
        retry.unwrap().interval_secs(),
        before.interval_secs(),
        "the interval is kept"
    );
    assert!(
        !fake
            .sent
            .iter()
            .any(|r| matches!(r, Request::UpdateMonitorSettings { .. }))
    );
}

#[test]
fn notifications_keep_the_chat_without_saying_it() {
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "set_whole_computer_notifications",
        json!({ "enabled": false }),
    );
    assert!(!failed(&result));
    let sent = fake.sent.iter().find_map(|r| match r {
        Request::UpdateNotificationSettings {
            enabled, chat_id, ..
        } => Some((*enabled, chat_id.clone())),
        _ => None,
    });
    let (enabled, chat) = sent.unwrap();
    assert!(!enabled);
    assert_eq!(chat.unwrap().as_str(), "-1009876543210", "kept as it is");
    assert!(!result.to_string().contains("-1009876543210"));
}

#[test]
fn a_refusal_says_what_to_do() {
    let mut fake = Fake::default();
    fake.answers.push_back(Outcome::Err(ProtocolError::new(
        ErrorCode::Unauthorized,
        Some(ErrorReason::McpDisabled),
        "MCP is off",
    )));
    let result = run(
        &mut fake,
        "set_monitoring_paused",
        json!({ "paused": true }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).contains("turn MCP on in the WGAL window"),
        "{}",
        text(&result)
    );
    assert_eq!(
        result["structuredContent"]["details"]["reason"],
        json!("mcp_disabled")
    );
}

#[test]
fn an_operation_answers_the_state_it_settled_in() {
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "connect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 0 }),
    );
    assert!(!failed(&result), "{}", text(&result));
    assert!(fake.sent.contains(&Request::Connect {
        tunnel_id: TunnelId::new("office-1").unwrap()
    }));
    assert_eq!(
        result["structuredContent"]["tunnel"]["tunnel_id"],
        json!("office-1")
    );
    // A tunnel WGAL does not monitor is refused before anything is sent.
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "disconnect_tunnel",
        json!({ "tunnel_id": "nowhere", "wait_secs": 0 }),
    );
    assert!(failed(&result));
    assert!(text(&result).contains("call list_tunnels"));
    assert!(
        !fake
            .sent
            .iter()
            .any(|r| matches!(r, Request::Disconnect { .. }))
    );
}

#[test]
fn the_logs_are_given_by_location_only() {
    let mut fake = Fake::default();
    let result = run(&mut fake, "log_locations", json!({}));
    assert!(!failed(&result));
    assert_eq!(
        result["structuredContent"]["paths"]["event_log"],
        json!(r"C:\ProgramData\WGAL\events\events.jsonl")
    );
    assert!(fake.sent.is_empty(), "nothing is read for the agent");
}

#[test]
fn times_are_written_in_utc() {
    assert_eq!(utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc(-1_000), "1969-12-31T23:59:59Z");
    assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00Z");
    assert_eq!(utc(1_767_225_600_000), "2026-01-01T00:00:00Z");
    assert_eq!(utc(1_772_323_199_999), "2026-02-28T23:59:59Z");
    let result = run(&mut Fake::default(), "health", json!({}));
    let tunnel = &result["structuredContent"]["tunnels"][0];
    assert!(
        tunnel["last_probe"]["finished_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert!(tunnel["last_probe"]["result"].is_object());
}

/// The event log fixture, as the watchdog answers it: 9 entries, more.
fn event_log() -> Outcome {
    let text = include_str!("../../../wgal-protocol/fixtures/response_event_log.json");
    serde_json::from_str::<ResponseEnvelope>(text)
        .unwrap()
        .outcome
}

#[test]
fn the_event_log_is_read_a_page_at_a_time() {
    let mut fake = Fake {
        answers: VecDeque::from([event_log()]),
        ..Fake::default()
    };
    let result = run(&mut fake, "read_event_log", json!({ "limit": 3 }));
    assert!(!failed(&result), "{}", text(&result));
    let data = &result["structuredContent"];
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["seq"], json!(1042));
    assert_eq!(entries[0]["tunnel_name"], json!("Home"), "{}", entries[0]);
    assert!(entries[0]["at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(data["more"], json!(true));
    assert_eq!(data["next_before_seq"], json!(1040));
    assert_eq!(
        fake.sent[1],
        Request::ReadEventLog {
            filter: EventFilter::All,
            before_seq: None
        }
    );

    let mut fake = Fake {
        answers: VecDeque::from([event_log()]),
        ..Fake::default()
    };
    run(
        &mut fake,
        "read_event_log",
        json!({ "tunnel_id": "home-3", "before_seq": 1043 }),
    );
    assert_eq!(
        fake.sent[1],
        Request::ReadEventLog {
            filter: EventFilter::Tunnel {
                tunnel_id: TunnelId::new("home-3").unwrap()
            },
            before_seq: Some(1043)
        }
    );
    let mut fake = Fake::default();
    for (args, words) in [
        (
            json!({ "tunnel_id": "home-3", "scope": "system" }),
            "not both",
        ),
        (json!({ "scope": "everything" }), "scope must be"),
        (
            json!({ "limit": 0 }),
            "limit must be a whole number from 1 to 100",
        ),
    ] {
        let result = run(&mut fake, "read_event_log", args);
        assert!(failed(&result));
        assert!(text(&result).contains(words), "{}", text(&result));
    }
    assert!(fake.sent.is_empty(), "a wrong call sends nothing");
}

#[test]
fn a_probe_that_cannot_run_says_why_at_once() {
    type Case = (fn(&mut Snapshot), &'static str);
    let cases: [Case; 3] = [
        (
            |s| s.monitoring_paused = true,
            "monitoring is paused for every tunnel",
        ),
        (
            |s| each(s, |t| t.intent.monitoring_enabled = false),
            "monitoring of this tunnel is off",
        ),
        (
            |s| {
                each(s, |t| {
                    t.recovery_target = None;
                    t.observe_targets = BoundedVec::default();
                });
            },
            "no target to probe",
        ),
    ];
    for (tune, words) in cases {
        let mut fake = Fake {
            tune: Some(tune),
            ..Fake::default()
        };
        let start = std::time::Instant::now();
        let result = run(
            &mut fake,
            "probe_tunnel",
            json!({ "tunnel_id": "office-1" }),
        );
        assert!(failed(&result));
        assert!(text(&result).contains(words), "{}", text(&result));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "no waiting"
        );
        assert_eq!(fake.sent, [Request::GetSnapshot], "no probe is asked for");
    }
}

#[test]
fn the_tunnel_list_gives_readable_times_too() {
    let result = run(&mut Fake::default(), "list_tunnels", json!({}));
    let first = &result["structuredContent"]["registered"][0];
    assert!(first["light_since_unix_ms"].is_i64());
    assert!(first["light_since_at"].as_str().unwrap().ends_with('Z'));
    let probe = &first["last_probe"];
    assert!(
        probe["finished_at"].as_str().unwrap().ends_with('Z'),
        "{probe}"
    );
    assert_eq!(
        first["deadline_at"],
        first["deadline_unix_ms"]
            .as_i64()
            .map_or(json!(null), |ms| json!(utc(ms)))
    );
}

#[test]
fn one_tunnel_comes_in_full_under_the_names_health_uses() {
    let mut fake = Fake::default();
    let result = run(&mut fake, "get_tunnel", json!({ "tunnel_id": "office-1" }));
    assert!(!failed(&result), "{}", text(&result));
    assert_eq!(fake.sent, [Request::GetSnapshot], "WireGuard is not asked");
    let full = &result["structuredContent"]["tunnel"];
    // Every field health gives, with the same value.
    let short = &run(&mut Fake::default(), "health", json!({}))["structuredContent"]["tunnels"][0];
    for (key, value) in short.as_object().unwrap() {
        if key != "incident" {
            assert_eq!(&full[key], value, "{key}");
        }
    }
    // And the rest of its settings and state.
    assert_eq!(
        full["probe"],
        json!({ "interval_secs": 15, "timeout_secs": 2, "retries": 2, "min_fault_secs": 60 })
    );
    let observe =
        json!({ "address": "192.0.2.10", "probe": { "kind": "tcp_connect", "port": 443 } });
    assert_eq!(full["observe_targets"], json!([observe]));
    assert_eq!(full["observe_last_probes"][0]["target"], observe);
    assert!(
        full["observe_last_probes"][0]["last_probe"]["finished_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(full["takeover"], json!("managed"));
    assert_eq!(full["freshness_secs"], json!(35));
    // list_tunnels gives the same entry.
    let listed = run(&mut Fake::default(), "list_tunnels", json!({}));
    assert_eq!(&listed["structuredContent"]["registered"][0], full);
    // A tunnel WGAL does not monitor.
    let result = run(
        &mut Fake::default(),
        "get_tunnel",
        json!({ "tunnel_id": "ghost-9" }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).contains("does not monitor a tunnel \"ghost-9\""),
        "{}",
        text(&result)
    );
}

#[test]
fn settings_read_from_get_tunnel_can_be_given_back_as_they_are() {
    let full = run(
        &mut Fake::default(),
        "get_tunnel",
        json!({ "tunnel_id": "office-1" }),
    )["structuredContent"]["tunnel"]
        .clone();
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "update_tunnel_settings",
        json!({
            "tunnel_id": "office-1",
            "recovery_target": full["recovery_target"],
            "observe_targets": full["observe_targets"],
            "probe": full["probe"],
            "retry": full["retry"],
        }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let before = &snapshot().tunnels[0];
    let sent = fake.sent.iter().find_map(|r| match r {
        Request::UpdateMonitorSettings { settings, .. } => Some(settings.clone()),
        _ => None,
    });
    let sent = sent.unwrap();
    assert_eq!(
        sent.recovery_target,
        before.recovery_target.as_ref().map(|r| r.target)
    );
    assert_eq!(
        sent.observe_targets.iter().collect::<Vec<_>>(),
        before
            .observe_targets
            .iter()
            .map(|o| &o.target)
            .collect::<Vec<_>>()
    );
    assert_eq!(sent.probe, before.probe);
    assert!(
        fake.sent.iter().any(
            |r| matches!(r, Request::SetRecoveryRetry { retry, .. } if *retry == before.retry)
        )
    );
    let data = &result["structuredContent"];
    assert_eq!(data["changed"], json!([]));
    // The answer holds the tunnel in full, and nothing beside it.
    assert_eq!(data["tunnel"], full);
    assert!(data.get("probe").is_none() && data.get("observe_targets").is_none());
}

#[test]
fn null_removes_the_recovery_target_and_an_empty_list_the_watch_only_ones() {
    let mut fake = Fake::default();
    let result = run(
        &mut fake,
        "update_tunnel_settings",
        json!({ "tunnel_id": "office-1", "recovery_target": null, "observe_targets": [] }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let sent = fake.sent.iter().find_map(|r| match r {
        Request::UpdateMonitorSettings { settings, .. } => Some(settings.clone()),
        _ => None,
    });
    let sent = sent.unwrap();
    assert_eq!(sent.recovery_target, None);
    assert!(sent.observe_targets.is_empty());
    assert_eq!(
        sent.probe,
        snapshot().tunnels[0].probe,
        "the timing is kept"
    );
    assert!(
        !fake
            .sent
            .iter()
            .any(|r| matches!(r, Request::SetRecoveryRetry { .. })),
        "the retry is kept"
    );
}

#[test]
fn a_change_says_what_changed_and_a_no_op_says_so() {
    let mut fake = Fake {
        after_first: Some(|s| each(s, |t| t.intent.monitoring_enabled = false)),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "set_tunnel_monitoring",
        json!({ "tunnel_id": "office-1", "enabled": false }),
    );
    assert!(!failed(&result), "{}", text(&result));
    assert_eq!(
        result["structuredContent"]["changed"],
        json!([{ "field": "monitoring", "from": true, "to": false }])
    );
    let result = run(
        &mut Fake::default(),
        "update_tunnel_settings",
        json!({ "tunnel_id": "office-1", "probe": { "interval_secs": 15 } }),
    );
    assert_eq!(result["structuredContent"]["changed"], json!([]));
    assert!(
        result["structuredContent"]["note"]
            .as_str()
            .unwrap()
            .starts_with("Nothing changed")
    );
    let tunnel = &run(&mut Fake::default(), "health", json!({}))["structuredContent"]["tunnels"][0];
    assert_eq!(
        tunnel["traffic_verified"],
        json!(tunnel["light"] == json!("green"))
    );
}

#[test]
fn a_dry_run_import_lists_what_would_change() {
    let export = include_str!("../../../wgal-protocol/fixtures/response_settings_export.json");
    let import = include_str!("../../../wgal-protocol/fixtures/response_settings_import.json");
    let outcome = |text: &str| {
        serde_json::from_str::<ResponseEnvelope>(text)
            .unwrap()
            .outcome
    };
    let mut document =
        serde_json::from_str::<Value>(export).unwrap()["outcome"]["ok"]["document"].clone();
    document["tunnels"][0]["probe"]["interval_secs"] = json!(30);
    document["host"]["time_zone"] = json!("UTC");
    let mut fake = Fake {
        answers: VecDeque::from([outcome(import), outcome(export)]),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "import_settings",
        json!({ "document": document, "dry_run": true }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let changes = &result["structuredContent"]["changes"];
    assert_eq!(
        changes["host"]["changes"],
        json!([{ "field": "host.time_zone", "from": "Asia/Taipei", "to": "UTC" }])
    );
    assert_eq!(changes["notifications"]["changes"], json!([]));
    // The fixture's verdict: entry 0 would be registered as office-1,
    // entry 1 has no tunnel here.
    // Entry 0 would be registered without monitor_new, and its automatic
    // recovery waits for the baseline: monitoring and automatic recovery
    // are off as the watchdog would set them, the latter armed. The
    // current values are the snapshot's, whose watch-only target is not
    // the export fixture's (fields in the order a JSON object keeps them).
    let observe = |address: &str| json!([{ "address": address, "probe": { "kind": "tcp_connect", "port": 443 } }]);
    assert_eq!(
        changes["tunnels"][0]["changes"],
        json!([
            { "field": "auto_recovery", "from": true, "to": false },
            { "field": "auto_recovery_pending", "from": false, "to": true },
            { "field": "monitoring", "from": true, "to": false },
            { "field": "observe_targets", "from": observe("192.0.2.10"), "to": observe("198.51.100.7") },
            { "field": "probe.interval_secs", "from": 15, "to": 30 },
        ]),
        "{changes}"
    );
    assert_eq!(changes["tunnels"][1]["outcome"]["kind"], json!("not_found"));
    assert_eq!(changes["tunnels"][1]["changes"], json!(null), "{changes}");
    assert!(matches!(fake.sent.last(), Some(Request::ExportSettings)));
}

#[test]
fn agent_requests_can_be_left_out_of_the_event_log() {
    let fixture = include_str!("../../../wgal-protocol/fixtures/response_event_log.json");
    let mut page: Value = serde_json::from_str(fixture).unwrap();
    page["outcome"]["ok"]["entries"][0]["event"] =
        json!({ "kind": "agent_request", "op": "probe_now" });
    let outcome = serde_json::from_value::<ResponseEnvelope>(page)
        .unwrap()
        .outcome;
    let mut fake = Fake {
        answers: VecDeque::from([outcome]),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "read_event_log",
        json!({ "skip_agent_requests": true, "limit": 100 }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let entries = result["structuredContent"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 8, "the agent_request line is left out");
    assert!(
        entries
            .iter()
            .all(|e| e["event"]["kind"] != json!("agent_request"))
    );
    assert_eq!(result["structuredContent"]["more"], json!(true));
    assert_eq!(result["structuredContent"]["next_before_seq"], json!(1034));
}

#[test]
fn a_switch_still_passes_the_watchdogs_refusal_on() {
    let mut fake = Fake::default();
    fake.answers.push_back(Outcome::Err(ProtocolError::new(
        ErrorCode::UnknownTunnel,
        Some(ErrorReason::TunnelNotRegistered),
        "no such tunnel",
    )));
    let result = run(
        &mut fake,
        "set_one_tunnel_notifications",
        json!({ "tunnel_id": "ghost-9", "enabled": true }),
    );
    assert!(failed(&result));
    assert_eq!(
        result["structuredContent"]["details"]["reason"],
        json!("tunnel_not_registered"),
        "{}",
        text(&result)
    );
    assert!(matches!(
        fake.sent[1],
        Request::SetTunnelNotifications { .. }
    ));
}

#[test]
fn a_field_that_goes_away_is_a_change_too() {
    let mut out = Vec::new();
    diff(
        "",
        &json!({ "probe": { "kind": "tcp_connect", "port": 443 } }),
        &json!({ "probe": { "kind": "icmp" } }),
        &mut out,
    );
    assert_eq!(
        out,
        [
            json!({ "field": "probe.kind", "from": "tcp_connect", "to": "icmp" }),
            json!({ "field": "probe.port", "from": 443, "to": null }),
        ]
    );
}

#[test]
fn cancelling_a_pending_recovery_is_a_change() {
    let mut fake = Fake {
        tune: Some(|s| {
            each(s, |t| {
                t.intent.auto_recovery_enabled = false;
                t.auto_recovery_pending = true;
            })
        }),
        after_first: Some(|s| each(s, |t| t.auto_recovery_pending = false)),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "set_auto_recovery",
        json!({ "tunnel_id": "office-1", "enabled": false }),
    );
    assert!(!failed(&result), "{}", text(&result));
    assert_eq!(
        result["structuredContent"]["changed"],
        json!([{ "field": "auto_recovery_pending", "from": true, "to": false }])
    );
}

#[test]
fn a_preview_entry_the_document_lacks_predicts_nothing() {
    let export = include_str!("../../../wgal-protocol/fixtures/response_settings_export.json");
    let import = include_str!("../../../wgal-protocol/fixtures/response_settings_import.json");
    let mut answer: Value = serde_json::from_str(import).unwrap();
    answer["outcome"]["ok"]["result"]["tunnels"][0]["index"] = json!(9);
    let answer = serde_json::from_value::<ResponseEnvelope>(answer)
        .unwrap()
        .outcome;
    let export_outcome = serde_json::from_str::<ResponseEnvelope>(export)
        .unwrap()
        .outcome;
    let document =
        serde_json::from_str::<Value>(export).unwrap()["outcome"]["ok"]["document"].clone();
    let mut fake = Fake {
        answers: VecDeque::from([answer, export_outcome]),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "import_settings",
        json!({ "document": document, "dry_run": true }),
    );
    let entry = &result["structuredContent"]["changes"]["tunnels"][0];
    assert_eq!(entry["changes"], json!(null), "{entry}");
    assert!(entry["note"].as_str().unwrap().contains("does not have"));
}

#[test]
fn a_tunnel_already_waiting_for_its_baseline_is_no_change() {
    let export = include_str!("../../../wgal-protocol/fixtures/response_settings_export.json");
    let import = include_str!("../../../wgal-protocol/fixtures/response_settings_import.json");
    let mut answer: Value = serde_json::from_str(import).unwrap();
    // office-1 is registered already and would be applied as it is.
    answer["outcome"]["ok"]["result"]["tunnels"][0]["outcome"] = json!({ "kind": "applied" });
    let answer = serde_json::from_value::<ResponseEnvelope>(answer)
        .unwrap()
        .outcome;
    let export_outcome = serde_json::from_str::<ResponseEnvelope>(export)
        .unwrap()
        .outcome;
    let mut document =
        serde_json::from_str::<Value>(export).unwrap()["outcome"]["ok"]["document"].clone();
    // The document has the tunnel's settings as they are now.
    document["tunnels"][0]["observe_targets"] =
        json!([{ "address": "192.0.2.10", "probe": { "kind": "tcp_connect", "port": 443 } }]);
    let mut fake = Fake {
        answers: VecDeque::from([answer, export_outcome]),
        // It waits for its baseline: the switch is off, armed.
        tune: Some(|s| {
            each(s, |t| {
                t.intent.auto_recovery_enabled = false;
                t.auto_recovery_pending = true;
            });
        }),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "import_settings",
        json!({ "document": document, "dry_run": true }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let entry = &result["structuredContent"]["changes"]["tunnels"][0];
    assert_eq!(entry["changes"], json!([]), "{entry}");
}

#[test]
fn a_watchdog_from_1_5_0_checks_an_unmonitored_tunnel_once() {
    let cases: [fn(&mut Snapshot); 2] = [
        |s| {
            s.host.watchdog_version = ProductVersion::new("1.5.0").unwrap();
            each(s, |t| t.intent.monitoring_enabled = false);
        },
        |s| {
            s.host.watchdog_version = ProductVersion::new("1.5.0").unwrap();
            s.monitoring_paused = true;
        },
    ];
    for tune in cases {
        let mut fake = Fake {
            tune: Some(tune),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "probe_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 0 }),
        );
        assert!(!failed(&result), "{}", text(&result));
        let data = &result["structuredContent"];
        assert_eq!(data["one_off"], json!(true));
        assert!(
            data["note"].as_str().unwrap().contains("one-off check"),
            "{data}"
        );
        assert!(
            fake.sent
                .iter()
                .any(|r| matches!(r, Request::ProbeNow { .. }))
        );
    }
}

#[test]
fn a_one_off_check_that_gets_its_answer_says_so() {
    let mut fake = Fake {
        tune: Some(|s| {
            s.host.watchdog_version = ProductVersion::new("1.5.0").unwrap();
            each(s, |t| t.intent.monitoring_enabled = false);
        }),
        // The answer comes in after the request.
        after_first: Some(|s| {
            each(s, |t| {
                if let Some(probe) = t
                    .recovery_target
                    .as_mut()
                    .and_then(|r| r.last_probe.as_mut())
                {
                    probe.finished_at_unix_ms += 60_000;
                }
            });
        }),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "probe_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 5 }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let data = &result["structuredContent"];
    assert_eq!(data["probed"], json!(true), "{data}");
    assert_eq!(data["one_off"], json!(true));
    assert!(
        data["note"]
            .as_str()
            .unwrap()
            .starts_with("A one-off check"),
        "{data}"
    );
}

/// A token of the test kit's shape; never a real one.
const TOKEN: &str = "123456789:EXAMPLE-not-a-real-token_0000000000";

/// A 2.1.0 watchdog whose window lets agents set Telegram.
fn telegram_allowed(s: &mut Snapshot) {
    s.host.watchdog_version = ProductVersion::new("2.1.0").unwrap();
    s.host.mcp_telegram_enabled = true;
}

/// A 2.1.0 watchdog whose window does not.
fn telegram_not_allowed(s: &mut Snapshot) {
    s.host.watchdog_version = ProductVersion::new("2.1.0").unwrap();
}

#[test]
fn telegram_waits_for_the_window_and_sends_nothing_before() {
    for (tune, words, reason) in [
        (
            None,
            "This watchdog (1.0.0) does not let agents set Telegram; watchdog 2.1.0 or later does",
            Value::Null,
        ),
        (
            Some(telegram_not_allowed as fn(&mut Snapshot)),
            TELEGRAM_HOW_TO_ALLOW,
            json!("mcp_telegram_disabled"),
        ),
    ] {
        let mut fake = Fake {
            tune,
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "set_telegram_settings",
            json!({ "bot_token": TOKEN, "chat_id": "@wgal_alerts" }),
        );
        assert!(failed(&result));
        assert!(
            text(&result).starts_with(
                "set_telegram_settings: agents may not set Telegram now, so nothing was sent. "
            ),
            "{}",
            text(&result)
        );
        assert!(text(&result).contains(words), "{}", text(&result));
        assert_eq!(fake.sent, [Request::GetSnapshot], "the token is not sent");
        assert!(!result.to_string().contains("EXAMPLE-not-a-real-token"));
        let details = &result["structuredContent"]["details"];
        assert_eq!(details["reason"], reason);
        let hint = details["hint"].as_str().unwrap();
        assert!(hint.contains(words), "{hint}");

        // health says the same, so an agent that stops there can too.
        let health = run(&mut fake, "health", json!({}));
        let n = &health["structuredContent"]["notifications"];
        assert_eq!(n["agent_may_set"], json!(false));
        assert_eq!(n["how_to_allow"], json!(hint));
        assert!(n.get("chat_id").is_none());
        assert!(!health.to_string().contains("-1009876543210"));
    }
}

#[test]
fn telegram_is_changed_and_cleared_while_the_window_lets_agents() {
    let kept_enabled = snapshot().notification.enabled;
    let sent_by = |arguments: Value| {
        let mut fake = Fake {
            tune: Some(telegram_allowed),
            ..Fake::default()
        };
        let result = run(&mut fake, "set_telegram_settings", arguments.clone());
        assert!(!failed(&result), "{arguments}: {}", text(&result));
        assert!(
            !result.to_string().contains("EXAMPLE-not-a-real-token"),
            "the token is never repeated"
        );
        fake.sent
            .into_iter()
            .filter(|r| !matches!(r, Request::GetSnapshot))
            .collect::<Vec<_>>()
    };
    let settings = |device: &str, chat: Option<&str>| Request::UpdateNotificationSettings {
        enabled: kept_enabled,
        device_name: DeviceName::new(device).unwrap(),
        chat_id: chat.map(|c| ChatId::new(c).unwrap()),
    };
    let device = snapshot().notification.device_name;
    let device = device.as_str();
    let chat = Some("-1009876543210");
    let token = || Request::SetBotToken {
        token: BotToken::new(TOKEN).unwrap(),
    };

    assert_eq!(
        sent_by(
            json!({ "device_name": " Office PC ", "chat_id": "@wgal_alerts", "bot_token": TOKEN })
        ),
        [token(), settings(" Office PC ", Some("@wgal_alerts"))]
    );
    assert_eq!(
        sent_by(json!({ "bot_token": TOKEN })),
        [token()],
        "the rest is kept"
    );
    assert_eq!(
        sent_by(json!({ "chat_id": null, "bot_token": "" })),
        [Request::ClearBotToken, settings(device, None)]
    );
    assert_eq!(
        sent_by(json!({ "clear": ["device_name", "bot_token"] })),
        [Request::ClearBotToken, settings("", chat)]
    );
    assert_eq!(
        sent_by(json!({ "device_name": "", "clear": ["chat_id"] })),
        [settings("", None)]
    );

    // health gives the chat ID now, and still never the token.
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        ..Fake::default()
    };
    let health = run(&mut fake, "health", json!({}));
    let n = &health["structuredContent"]["notifications"];
    assert_eq!(n["agent_may_set"], json!(true));
    assert_eq!(n["chat_id"], json!("-1009876543210"));
    assert!(n.get("how_to_allow").is_none());

    // What changed, the token only as replaced.
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "bot_token": TOKEN }),
    );
    let changed = &result["structuredContent"]["changed"];
    assert_eq!(
        changed,
        &json!([{ "field": "bot_token", "from": snapshot().notification.token.value(), "to": "replaced" }])
    );
}

#[test]
fn telegram_arguments_are_checked_first_and_the_token_is_never_repeated() {
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        ..Fake::default()
    };
    let cases = [
        (
            json!({ "bot_token": "SECRETVALUE" }),
            "bot_token is not a Telegram bot token (invalid bot token: expected <bot id>:<secret>): it is <bot id>:<secret>, as @BotFather gives it",
        ),
        (
            json!({ "bot_token": "123:SECRETVALUE" }),
            "the secret must be 30 to 40",
        ),
        (
            json!({ "bot_token": format!("12:{}", "a".repeat(35)) }),
            "the bot id must be 3 to 15 digits",
        ),
        (
            json!({ "chat_id": format!("@{}", "a".repeat(38)) }),
            "a public @username of 1 to 37",
        ),
        (
            json!({ "chat_id": "-99999999999999999999" }),
            "a numeric chat ID of 1 to 19 digits",
        ),
        (
            json!({ "bot_token": 5 }),
            "bot_token must be a string; null or \"\" clears it",
        ),
        (
            json!({ "chat_id": "12ab" }),
            "chat_id must be a numeric chat ID",
        ),
        (
            json!({ "bot_token": TOKEN, "clear": ["bot_token"] }),
            "bot_token is both given and listed in clear",
        ),
        (json!({ "clear": "bot_token" }), "clear must be a list"),
        (
            json!({ "clear": ["SECRETVALUE"] }),
            "clear[0] is not a setting name",
        ),
        (
            json!({ "clear": ["chat_id", "chat_id"] }),
            "clear lists chat_id twice",
        ),
        (
            json!({ "token": "123:SECRETVALUE" }),
            "an argument it does not take (did you mean bot_token?); its name is not repeated here, as it may hold a secret. It takes: device_name, chat_id, bot_token, clear",
        ),
        (json!({ "chatId": "-100123" }), "(did you mean chat_id?)"),
        (
            json!({ TOKEN: "" }),
            "an argument it does not take; its name is not repeated here",
        ),
        (json!({ "SECRETVALUE": 1 }), "its name is not repeated here"),
        (
            json!({}),
            "nothing to change: give device_name, chat_id or bot_token, or list them in clear",
        ),
    ];
    for (arguments, words) in cases {
        let result = run(&mut fake, "set_telegram_settings", arguments.clone());
        assert!(failed(&result), "{arguments} was not refused");
        assert!(
            text(&result).contains(words),
            "{arguments}: {}",
            text(&result)
        );
        let all = result.to_string();
        assert!(
            !all.contains("SECRETVALUE") && !all.contains("EXAMPLE-not-a-real-token"),
            "{all}"
        );
    }
    assert!(fake.sent.is_empty(), "{:?}", fake.sent);
}

#[test]
fn telegram_says_what_was_done_before_a_refusal() {
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        ..Fake::default()
    };
    fake.answers.push_back(Outcome::Ok(Response::Accepted));
    fake.answers.push_back(Outcome::Err(ProtocolError::new(
        ErrorCode::StorageUnavailable,
        Some(ErrorReason::StorageSaveFailed),
        "the change could not be stored",
    )));
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "bot_token": TOKEN, "chat_id": "@wgal_alerts" }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).ends_with("(before this, the Bot Token was replaced)"),
        "{}",
        text(&result)
    );

    let mut fake = Fake {
        tune: Some(telegram_allowed),
        ..Fake::default()
    };
    fake.answers.push_back(Outcome::Err(ProtocolError::new(
        ErrorCode::Unauthorized,
        Some(ErrorReason::McpTelegramDisabled),
        "agents may not change the device name, chat ID or bot token",
    )));
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "clear": ["bot_token"] }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).ends_with(&format!(
            "(unauthorized, mcp_telegram_disabled). {TELEGRAM_HOW_TO_ALLOW}"
        )),
        "{}",
        text(&result)
    );
    assert_eq!(
        result["structuredContent"]["details"]["hint"],
        json!(TELEGRAM_HOW_TO_ALLOW),
        "the same words as health and the tool's own refusal"
    );
}

#[test]
fn nothing_shaped_like_a_token_is_repeated_by_any_tool() {
    assert!(looks_like_token(TOKEN));
    assert!(looks_like_token(&format!("x{TOKEN}y")));
    assert!(!looks_like_token("office-1"));
    assert!(!looks_like_token("123:short"));
    let mut fake = Fake::default();
    for (tool, arguments) in [
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1", "enabled": true, TOKEN: 1 }),
        ),
        ("health", json!({ TOKEN: 1 })),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": TOKEN, "enabled": true }),
        ),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1", "enabled": TOKEN }),
        ),
        ("update_host_settings", json!({ "time_zone": [TOKEN] })),
        ("update_host_settings", json!({ "workers": TOKEN })),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "recovery_target": { "address": TOKEN } }),
        ),
        (
            "import_settings",
            json!({ "document": {}, "match_by": TOKEN }),
        ),
        ("read_event_log", json!({ "scope": TOKEN })),
    ] {
        let result = run(&mut fake, tool, arguments.clone());
        assert!(failed(&result), "{tool} {arguments}");
        assert!(
            !result.to_string().contains("EXAMPLE-not-a-real-token"),
            "{tool}: {}",
            text(&result)
        );
    }
}

/// Withdrawn between the two snapshots: the answer gives no chat ID, not
/// even in what changed.
#[test]
fn telegram_hides_the_chat_once_the_window_withdraws_it() {
    fn withdrawn(s: &mut Snapshot) {
        s.host.mcp_telegram_enabled = false;
        s.notification.chat_id = Some(ChatId::new("@someone_else").unwrap());
    }
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        after_first: Some(withdrawn),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "bot_token": TOKEN }),
    );
    assert!(!failed(&result), "{}", text(&result));
    let all = result.to_string();
    assert!(
        !all.contains("someone_else") && !all.contains("-1009876543210"),
        "{all}"
    );
    let n = &result["structuredContent"]["notifications"];
    assert_eq!(n["agent_may_set"], json!(false));
    assert_eq!(n["how_to_allow"], json!(TELEGRAM_HOW_TO_ALLOW));
}

#[test]
fn telegram_says_every_change_done_before_a_later_failure() {
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        snapshots_fail_from: Some(1),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "bot_token": TOKEN, "device_name": "Office PC", "chat_id": null }),
    );
    // Every change was accepted, so only the read after them failed: not
    // a failure of the tool (AR-05), and it says each change.
    assert!(!failed(&result), "{}", text(&result));
    let data = &result["structuredContent"];
    assert_eq!(data["accepted"], json!(true));
    assert_eq!(data["state_read"], json!(false));
    assert_eq!(
        data["done"],
        json!([
            "the Bot Token was replaced",
            "the device name was changed",
            "the chat ID was cleared"
        ])
    );
    assert!(
        data["note"].as_str().unwrap().contains(
            "the Bot Token was replaced, the device name was changed, the chat ID was cleared"
        ),
        "{data}"
    );
}

/// Shaped like a token (one digit, a colon, 30 more) and short enough for
/// the device name's 32 characters; WGAL's own check now needs 3 digits.
const SHORT_TOKEN: &str = "1:EXAMPLEnotarealtoken0000000000";

#[test]
fn a_token_is_hidden_at_any_length_and_wherever_it_comes_from() {
    assert_eq!(SHORT_TOKEN.chars().count(), 32);
    assert!(looks_like_token(SHORT_TOKEN));
    let long_prefix = format!("000000000000000000000{TOKEN}");
    assert!(looks_like_token(&long_prefix));
    assert_eq!(
        without_tokens(&format!("a {long_prefix} b")),
        format!("a {HIDDEN_TOKEN} b")
    );
    // Two that touch: one replacement, nothing of either left.
    let touching = format!("{SHORT_TOKEN}{SHORT_TOKEN}");
    assert_eq!(without_tokens(&touching), HIDDEN_TOKEN);

    let mut fake = Fake::default();
    let result = run(&mut fake, "health", json!({ long_prefix.clone(): 1 }));
    assert!(failed(&result));
    assert!(
        !result.to_string().contains("EXAMPLE-not-a-real-token"),
        "{result}"
    );

    // A device name typed into the window that holds a token is hidden too.
    let mut fake = Fake {
        tune: Some(|s: &mut Snapshot| {
            s.notification.device_name = DeviceName::new(SHORT_TOKEN).unwrap();
        }),
        ..Fake::default()
    };
    let result = run(&mut fake, "health", json!({}));
    assert!(!failed(&result));
    assert!(
        !result.to_string().contains("EXAMPLEnotarealtoken"),
        "{result}"
    );
    assert_eq!(
        result["structuredContent"]["notifications"]["device_name"],
        json!(HIDDEN_TOKEN)
    );
}

#[test]
fn a_device_name_shaped_like_a_token_is_not_taken() {
    let mut fake = Fake {
        tune: Some(telegram_allowed),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "set_telegram_settings",
        json!({ "device_name": SHORT_TOKEN, "bot_token": SHORT_TOKEN }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).contains("device_name is shaped like a Bot Token, so it was not taken"),
        "{}",
        text(&result)
    );
    assert!(
        !result.to_string().contains("EXAMPLEnotarealtoken"),
        "{result}"
    );
    assert!(fake.sent.is_empty(), "{:?}", fake.sent);
}

/// A settings document's names are taken as they are (owner decision of
/// 2026-11-06): markup or invisible characters do not stop an import.
#[test]
fn an_imported_name_is_taken_as_it_is() {
    let mut fake = Fake::default();
    let export = run(&mut fake, "export_settings", json!({}));
    let mut document = export["structuredContent"]["document"].clone();
    if document["tunnels"].as_array().is_none_or(Vec::is_empty) {
        return;
    }
    for name in ["<img src=x>", "Office\u{202E}NPV"] {
        document["tunnels"][0]["name"] = json!(name);
        let mut fake = Fake::default();
        run(
            &mut fake,
            "import_settings",
            json!({ "document": document, "dry_run": true }),
        );
        assert!(
            fake.sent
                .iter()
                .any(|r| matches!(r, Request::ImportSettings { .. })),
            "{name}: {:?}",
            fake.sent
        );
    }
}

/// What the schema advertises for chat_id and bot_token is what the tool
/// takes: "" to clear, or what the contract takes (Codex review of
/// 2026-11-06). Whitespace is left out: the tool trims it, a schema cannot.
#[test]
fn the_telegram_schema_patterns_take_what_the_tool_takes() {
    let chat = regex::Regex::new(&chat_id_pattern()).unwrap();
    let token = regex::Regex::new(&bot_token_pattern()).unwrap();
    let samples = [
        "".to_owned(),
        "0".to_owned(),
        "-0".to_owned(),
        "007".to_owned(),
        "7".to_owned(),
        "-1001234567890".to_owned(),
        "9".repeat(19),
        "9".repeat(20),
        "@a".to_owned(),
        "@abcd".to_owned(),
        format!("@{}", "a".repeat(37)),
        format!("@{}", "a".repeat(38)),
        "@1abc".to_owned(),
        "@".to_owned(),
        TOKEN.to_owned(),
        format!("{}:{}", "9".repeat(3), "a".repeat(30)),
        format!("{}:{}", "9".repeat(15), "a".repeat(40)),
        format!("{}:{}", "9".repeat(2), "a".repeat(35)),
        format!("{}:{}", "9".repeat(16), "a".repeat(35)),
        format!("{}:{}", "9".repeat(10), "a".repeat(29)),
        format!("{}:{}", "9".repeat(10), "a".repeat(41)),
        "<script>".to_owned(),
    ];
    for s in &samples {
        let takes_chat = s.is_empty() || ChatId::new(s.as_str()).is_ok();
        assert_eq!(chat.is_match(s), takes_chat, "chat_id {s:?}");
        let takes_token = s.is_empty() || BotToken::new(s.as_str()).is_ok();
        assert_eq!(token.is_match(s), takes_token, "bot_token {s:?}");
    }
    let tools = definitions();
    let props = &tools
        .iter()
        .find(|t| t["name"] == json!("set_telegram_settings"))
        .unwrap()["inputSchema"]["properties"];
    assert_eq!(props["chat_id"]["pattern"], json!(chat_id_pattern()));
    assert_eq!(props["bot_token"]["pattern"], json!(bot_token_pattern()));
}

/// The device name is any text the user likes, of at most 64 characters:
/// a longer one is cut to 61 and "...", and the answer says so (owner
/// decision of 2026-11-06). A name shaped like a token is still refused.
#[test]
fn a_device_name_is_any_text_cut_to_64_characters() {
    let sent_name = |name: &str| {
        let mut fake = Fake {
            tune: Some(telegram_allowed),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "set_telegram_settings",
            json!({ "device_name": name }),
        );
        assert!(!failed(&result), "{name:?}: {}", text(&result));
        let sent = fake.sent.iter().find_map(|r| match r {
            Request::UpdateNotificationSettings { device_name, .. } => {
                Some(device_name.as_str().to_owned())
            }
            _ => None,
        });
        (sent.unwrap(), text(&result))
    };
    for name in [
        "<b>Office</b>",
        "<script>alert(1)</script>",
        "Office\u{202E}CP",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "\u{2764}\u{FE0F}",
        "می\u{200C}خواهم",
        "क्\u{200D}ष",
    ] {
        let (sent, answer) = sent_name(name);
        assert_eq!(sent, name);
        assert!(!answer.contains("was cut"), "{answer}");
    }
    let (sent, _) = sent_name(&"x".repeat(64));
    assert_eq!(sent, "x".repeat(64));
    let (sent, answer) = sent_name(&"測".repeat(70));
    assert_eq!(sent, format!("{}...", "測".repeat(61)));
    assert!(
        answer.contains("device_name had more than 64 characters, so it was cut to 61 and '...'"),
        "{answer}"
    );
    let (sent, _) = sent_name("Office\nPC");
    assert_eq!(sent, "Office PC");
}

/// The name is taken as given, not trimmed, as the window takes it (Codex
/// review of 2026-11-06): spaces stay, and a 65th character, even U+FEFF,
/// cuts it.
#[test]
fn a_device_name_is_not_trimmed() {
    let sent_name = |name: &str| {
        let mut fake = Fake {
            tune: Some(telegram_allowed),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "set_telegram_settings",
            json!({ "device_name": name }),
        );
        assert!(!failed(&result), "{name:?}: {}", text(&result));
        fake.sent.iter().find_map(|r| match r {
            Request::UpdateNotificationSettings { device_name, .. } => {
                Some(device_name.as_str().to_owned())
            }
            _ => None,
        })
    };
    assert_eq!(sent_name("  Office  ").as_deref(), Some("  Office  "));
    assert_eq!(sent_name("   ").as_deref(), Some("   "));
    assert_eq!(sent_name("").as_deref(), Some(""));
    let name = format!("{}\u{FEFF}", "x".repeat(64));
    assert_eq!(sent_name(&name), Some(format!("{}...", "x".repeat(61))));
}

/// After a reconnect, the tool checks traffic once and says whether it came
/// back (owner decision of 2026-11-06, from the MCP agent test).
#[test]
fn a_reconnect_checks_traffic_once_and_says_whether_it_came_back() {
    fn answered(s: &mut Snapshot) {
        // The snapshot that shows the probe is taken after it.
        s.generated_at_unix_ms = 1_767_225_700_100;
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_700_000,
                    finished_at_unix_ms: 1_767_225_700_020,
                    result: ProbeResult::Reply { rtt_ms: 21 },
                });
            }
        });
    }
    fn silent(s: &mut Snapshot) {
        s.generated_at_unix_ms = 1_767_225_700_100;
        fn no_reply(r: &mut TargetStatus) {
            r.last_probe = Some(wgal_protocol::ProbeSummary {
                started_at_unix_ms: 1_767_225_700_000,
                finished_at_unix_ms: 1_767_225_700_020,
                result: ProbeResult::NoReply,
            });
        }
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                no_reply(r);
            }
            let mut observe = std::mem::take(&mut t.observe_targets).into_vec();
            observe.iter_mut().for_each(no_reply);
            t.observe_targets = BoundedVec::try_from(observe).unwrap();
        });
    }
    let reconnect = |after_probe: fn(&mut Snapshot)| {
        let mut fake = Fake {
            after_probe: Some(after_probe),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "reconnect_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
        );
        assert!(!failed(&result), "{}", text(&result));
        let id = TunnelId::new("office-1").unwrap();
        assert!(fake.sent.contains(&Request::Reconnect {
            tunnel_id: id.clone()
        }));
        assert!(fake.sent.contains(&Request::ProbeNow { tunnel_id: id }));
        result
    };

    let result = reconnect(answered);
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["checked"], json!(true));
    assert_eq!(traffic["recovered"], json!(true));
    // Only this check's probe: the watch-only target's older one is left out.
    assert_eq!(traffic["targets"].as_array().unwrap().len(), 1);
    assert!(
        text(&result).contains("traffic came back"),
        "{}",
        text(&result)
    );

    let result = reconnect(silent);
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["recovered"], json!(false));
    assert_eq!(traffic["targets"].as_array().unwrap().len(), 2);
    assert!(
        text(&result).contains("traffic has not come back"),
        "{}",
        text(&result)
    );
}

/// No check where none can tell anything: a tunnel left down, or one with
/// no target. Nothing is probed, and the answer says why.
#[test]
fn a_reconnect_says_why_traffic_was_not_checked() {
    fn down(s: &mut Snapshot) {
        each(s, |t| t.os_state = OsState::Disconnected);
    }
    fn no_targets(s: &mut Snapshot) {
        each(s, |t| {
            t.recovery_target = None;
            t.observe_targets = BoundedVec::new();
        });
    }
    for (tune, why) in [
        (
            down as fn(&mut Snapshot),
            "not connected after the reconnect",
        ),
        (no_targets as fn(&mut Snapshot), "no target to probe"),
    ] {
        let mut fake = Fake {
            tune: Some(tune),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "reconnect_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
        );
        assert!(!failed(&result), "{}", text(&result));
        let traffic = &result["structuredContent"]["traffic"];
        assert_eq!(traffic["checked"], json!(false));
        assert_eq!(traffic["recovered"], Value::Null);
        assert!(traffic["why"].as_str().unwrap().contains(why), "{traffic}");
        assert!(
            !fake
                .sent
                .iter()
                .any(|r| matches!(r, Request::ProbeNow { .. }))
        );
    }
}

/// Only probes that started after the check began count (Codex review of
/// 2026-11-06): a round already under way that publishes a reply later, or
/// a clock that moved backwards, leaves the check inconclusive.
#[test]
fn a_reconnect_check_counts_only_its_own_probes() {
    fn earlier(s: &mut Snapshot) {
        let began = s.generated_at_unix_ms;
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_599_000,
                    finished_at_unix_ms: 1_767_225_600_500,
                    result: ProbeResult::Reply { rtt_ms: 9 },
                });
            }
        });
        assert!(1_767_225_599_000 < began);
    }
    let mut fake = Fake {
        after_probe: Some(earlier),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "reconnect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
    );
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["checked"], json!(false), "{traffic}");
    assert_eq!(traffic["recovered"], Value::Null);
}

/// An answer proves traffic through the tunnel only where the computer can
/// bind the probe to it (Codex review of 2026-11-06).
#[test]
fn a_reconnect_check_claims_traffic_only_through_a_verified_path() {
    fn unknown(s: &mut Snapshot) {
        s.capabilities.bound_icmp_probe = Capability::Unknown;
    }
    fn answered(s: &mut Snapshot) {
        // The snapshot that shows the probe is taken after it.
        s.generated_at_unix_ms = 1_767_225_700_100;
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_700_000,
                    finished_at_unix_ms: 1_767_225_700_020,
                    result: ProbeResult::Reply { rtt_ms: 21 },
                });
            }
        });
    }
    let mut fake = Fake {
        tune: Some(unknown),
        after_probe: Some(answered),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "reconnect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
    );
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["checked"], json!(true), "{traffic}");
    assert_eq!(traffic["recovered"], Value::Null);
    assert_eq!(
        traffic["targets"][0]["through_tunnel_verified"],
        json!(false)
    );
    assert!(
        text(&result).contains("not provably through the tunnel"),
        "{}",
        text(&result)
    );
}

/// The cases the wall clock alone cannot tell apart (Codex review of
/// 2026-11-06) stay inconclusive: a cached reply stamped after the start
/// (the clock moved back before it), a probe in the start's millisecond,
/// and a snapshot older than the start.
#[test]
fn a_reconnect_check_is_inconclusive_when_time_cannot_tell() {
    fn stamped_later(s: &mut Snapshot) {
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_602_000,
                    finished_at_unix_ms: 1_767_225_602_010,
                    result: ProbeResult::Reply { rtt_ms: 9 },
                });
            }
        });
    }
    // Started in the start's own millisecond (the fixture's generated_at).
    fn same_millisecond(s: &mut Snapshot) {
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_600_000,
                    finished_at_unix_ms: 1_767_225_600_030,
                    result: ProbeResult::Reply { rtt_ms: 9 },
                });
            }
        });
    }
    fn older_snapshot(s: &mut Snapshot) {
        s.generated_at_unix_ms -= 5_000;
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_700_000,
                    finished_at_unix_ms: 1_767_225_700_020,
                    result: ProbeResult::Reply { rtt_ms: 9 },
                });
            }
        });
    }
    // A later snapshot whose new reply is stamped after the snapshot itself.
    fn reply_from_the_future(s: &mut Snapshot) {
        s.generated_at_unix_ms += 1;
        each(s, |t| {
            if let Some(r) = t.recovery_target.as_mut() {
                r.last_probe = Some(wgal_protocol::ProbeSummary {
                    started_at_unix_ms: 1_767_225_602_000,
                    finished_at_unix_ms: 1_767_225_602_010,
                    result: ProbeResult::Reply { rtt_ms: 9 },
                });
            }
        });
    }
    for (tune, after_probe, sends_probe) in [
        (Some(stamped_later as fn(&mut Snapshot)), None, false),
        (None, Some(same_millisecond as fn(&mut Snapshot)), true),
        (None, Some(older_snapshot as fn(&mut Snapshot)), true),
        (None, Some(reply_from_the_future as fn(&mut Snapshot)), true),
    ] {
        let mut fake = Fake {
            tune,
            after_probe,
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "reconnect_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
        );
        assert!(!failed(&result), "{}", text(&result));
        let traffic = &result["structuredContent"]["traffic"];
        assert_eq!(traffic["checked"], json!(false), "{traffic}");
        assert_eq!(traffic["recovered"], Value::Null, "{traffic}");
        assert_eq!(
            fake.sent
                .iter()
                .any(|r| matches!(r, Request::ProbeNow { .. })),
            sends_probe
        );
    }
}

// --- FIX-03 (AR-05, N-05) ---------------------------------------------------

/// AR-05: an operation whose answer is lost, or that the watchdog answers
/// busy, is sent once and never again, and the failure says whether the
/// request reached the watchdog.
#[test]
fn an_operation_whose_answer_is_lost_or_busy_is_not_sent_again() {
    let connects = |fake: &Fake| {
        fake.sent
            .iter()
            .filter(|r| matches!(r, Request::Connect { .. }))
            .count()
    };
    let mut fake = Fake {
        lose: Some(|r| matches!(r, Request::Connect { .. })),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "connect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 0 }),
    );
    assert!(failed(&result));
    assert_eq!(connects(&fake), 1, "never sent again");
    assert!(
        text(&result).contains("may still be carried out"),
        "{}",
        text(&result)
    );
    assert_eq!(
        result["structuredContent"]["details"]["delivery"],
        json!("unknown"),
        "{result}"
    );

    let mut fake = Fake::default();
    fake.answers.push_back(Outcome::Err(ProtocolError::new(
        ErrorCode::Busy,
        Some(ErrorReason::TooManyWaiting),
        "busy",
    )));
    let result = run(
        &mut fake,
        "connect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 0 }),
    );
    assert!(failed(&result));
    assert_eq!(connects(&fake), 1, "a busy answer is not sent again");
}

/// AR-05: once the watchdog has accepted a change, a status read that fails
/// afterwards does not make the tool say it failed: it answers that the
/// change was accepted and its state was not read.
#[test]
fn an_accepted_change_whose_state_cannot_be_read_says_so() {
    type Case = (&'static str, Value, usize, Option<fn(&mut Snapshot)>);
    let office = || json!({ "tunnel_id": "office-1" });
    let wait = || json!({ "tunnel_id": "office-1", "wait_secs": 0 });
    let cases: Vec<Case> = vec![
        ("register_tunnel", office(), 0, None),
        ("unregister_tunnel", office(), 0, None),
        (
            "set_tunnel_monitoring",
            json!({ "tunnel_id": "office-1", "enabled": false }),
            1,
            None,
        ),
        ("set_monitoring_paused", json!({ "paused": true }), 0, None),
        ("connect_tunnel", wait(), 1, None),
        ("disconnect_tunnel", wait(), 1, None),
        ("reconnect_tunnel", wait(), 1, None),
        ("probe_tunnel", wait(), 1, None),
        (
            "set_auto_recovery",
            json!({ "tunnel_id": "office-1", "enabled": true, "start_when_stopped_externally": true }),
            1,
            None,
        ),
        (
            "update_tunnel_settings",
            json!({ "tunnel_id": "office-1", "probe": { "interval_secs": 60 }, "retry": { "limit": 3 } }),
            1,
            None,
        ),
        (
            "set_whole_computer_notifications",
            json!({ "enabled": false }),
            1,
            None,
        ),
        ("update_host_settings", json!({ "workers": null }), 1, None),
        ("send_test_notification", json!({ "wait_secs": 0 }), 1, None),
        (
            "set_telegram_settings",
            json!({ "device_name": "Office PC" }),
            1,
            Some(telegram_allowed),
        ),
    ];
    for (tool, arguments, fail_from, tune) in cases {
        let mut fake = Fake {
            snapshots_fail_from: Some(fail_from),
            tune,
            ..Fake::default()
        };
        let result = run(&mut fake, tool, arguments);
        assert!(!failed(&result), "{tool}: {}", text(&result));
        let data = &result["structuredContent"];
        assert_eq!(data["accepted"], json!(true), "{tool}: {data}");
        assert_eq!(data["state_read"], json!(false), "{tool}: {data}");
        assert!(
            data["note"].as_str().unwrap().contains("could not be read"),
            "{tool}: {data}"
        );
        assert!(
            !data["done"].as_array().unwrap().is_empty(),
            "{tool}: {data}"
        );
    }
}

/// AR-05: a tool that sends several changes says which were made when a
/// later one fails or its answer is lost.
#[test]
fn a_multi_step_tool_says_what_was_done_before_a_later_step_failed() {
    let stored = || {
        Outcome::Err(ProtocolError::new(
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed),
            "the change could not be stored",
        ))
    };
    let mut fake = Fake::default();
    fake.answers.push_back(Outcome::Ok(Response::Accepted));
    fake.answers.push_back(stored());
    let result = run(
        &mut fake,
        "set_auto_recovery",
        json!({ "tunnel_id": "office-1", "enabled": true, "start_when_stopped_externally": true }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).ends_with("(before this, automatic recovery was turned on)"),
        "{}",
        text(&result)
    );
    assert_eq!(
        result["structuredContent"]["done"],
        json!(["automatic recovery was turned on"])
    );

    let mut fake = Fake::default();
    fake.answers.push_back(Outcome::Ok(Response::Accepted));
    fake.answers.push_back(stored());
    let result = run(
        &mut fake,
        "update_tunnel_settings",
        json!({ "tunnel_id": "office-1", "probe": { "interval_secs": 60 }, "retry": { "limit": 3 } }),
    );
    assert!(failed(&result));
    assert!(
        text(&result).ends_with(
            "(before this, the recovery target, watch-only targets and probe timing were set)"
        ),
        "{}",
        text(&result)
    );

    // The second step's answer lost: it may have been made too.
    let mut fake = Fake {
        lose: Some(|r| matches!(r, Request::SetRecoveryRetry { .. })),
        ..Fake::default()
    };
    let result = run(
        &mut fake,
        "update_tunnel_settings",
        json!({ "tunnel_id": "office-1", "probe": { "interval_secs": 60 }, "retry": { "limit": 3 } }),
    );
    assert!(failed(&result));
    let said = text(&result);
    assert!(said.contains("may still be carried out"), "{said}");
    assert!(
        said.ends_with(
            "(before this, the recovery target, watch-only targets and probe timing were set)"
        ),
        "{said}"
    );
    assert_eq!(
        result["structuredContent"]["details"]["delivery"],
        json!("unknown")
    );
}

/// The recovery target's and the watch-only target's new results.
fn fresh(s: &mut Snapshot, recovery: ProbeResult, observe: ProbeResult) {
    s.generated_at_unix_ms = 1_767_225_700_100;
    let summary = |result| {
        Some(wgal_protocol::ProbeSummary {
            started_at_unix_ms: 1_767_225_700_000,
            finished_at_unix_ms: 1_767_225_700_020,
            result,
        })
    };
    let mut tunnels = std::mem::take(&mut s.tunnels).into_vec();
    for t in &mut tunnels {
        if let Some(r) = t.recovery_target.as_mut() {
            r.last_probe = summary(recovery);
        }
        let mut observed = std::mem::take(&mut t.observe_targets).into_vec();
        for o in &mut observed {
            o.last_probe = summary(observe);
        }
        t.observe_targets = BoundedVec::try_from(observed).unwrap();
    }
    s.tunnels = BoundedVec::try_from(tunnels).unwrap();
}

/// A probe that could not run: the path is not the tunnel's.
const ROUTE: ProbeResult = ProbeResult::Error {
    error: wgal_protocol::AdapterErrorKind::RouteMismatch,
};

/// N-05: after a reconnect, probes that could not run are not "traffic has
/// not come back"; only probes that ran and got no answer are. In the
/// fixture the recovery target is ICMP (a bound probe on this computer) and
/// the watch-only one TCP (not known to be bound).
#[test]
fn a_reconnect_check_tells_a_probe_error_from_no_answer() {
    fn all_error(s: &mut Snapshot) {
        fresh(s, ROUTE, ROUTE);
    }
    fn no_reply_and_error(s: &mut Snapshot) {
        fresh(s, ProbeResult::NoReply, ROUTE);
    }
    fn all_no_reply(s: &mut Snapshot) {
        fresh(s, ProbeResult::NoReply, ProbeResult::NoReply);
    }
    fn bound_reply(s: &mut Snapshot) {
        fresh(s, ProbeResult::Reply { rtt_ms: 21 }, ROUTE);
    }
    fn unbound_reply(s: &mut Snapshot) {
        fresh(s, ProbeResult::NoReply, ProbeResult::Refused { rtt_ms: 3 });
    }
    // The new results; checked, recovered and reason; a word of why (none:
    // no why) and of the note; through_tunnel_verified of the two targets.
    type Case = (
        fn(&mut Snapshot),
        bool,
        Option<bool>,
        &'static str,
        Option<&'static str>,
        &'static str,
        [bool; 2],
    );
    let cases: [Case; 5] = [
        (
            all_error,
            false,
            None,
            "probe_error",
            Some("could not run"),
            "could not run",
            [false, false],
        ),
        (
            no_reply_and_error,
            false,
            None,
            "probe_error",
            Some("could not run"),
            "could not run",
            [false, false],
        ),
        (
            all_no_reply,
            true,
            Some(false),
            "no_reply",
            None,
            "traffic has not come back",
            [false, false],
        ),
        (
            bound_reply,
            true,
            Some(true),
            "recovered",
            None,
            "traffic came back",
            [true, false],
        ),
        (
            unbound_reply,
            true,
            None,
            "path_unverified",
            Some("cannot confirm"),
            "not provably through the tunnel",
            [false, false],
        ),
    ];
    for (after_probe, checked, recovered, reason, why, note, verified) in cases {
        let mut fake = Fake {
            after_probe: Some(after_probe),
            ..Fake::default()
        };
        let result = run(
            &mut fake,
            "reconnect_tunnel",
            json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
        );
        assert!(!failed(&result), "{}", text(&result));
        let traffic = &result["structuredContent"]["traffic"];
        assert_eq!(traffic["checked"], json!(checked), "{reason}: {traffic}");
        assert_eq!(
            traffic["recovered"],
            json!(recovered),
            "{reason}: {traffic}"
        );
        assert_eq!(traffic["reason"], json!(reason), "{traffic}");
        match why {
            Some(why) => assert!(
                traffic["why"].as_str().unwrap().contains(why),
                "{reason}: {traffic}"
            ),
            None => assert!(traffic.get("why").is_none(), "{reason}: {traffic}"),
        }
        let said = result["structuredContent"]["note"].as_str().unwrap();
        assert!(said.contains(note), "{reason}: {said}");
        let targets = traffic["targets"].as_array().unwrap();
        assert_eq!(targets.len(), 2, "{reason}: {traffic}");
        for (target, verified) in targets.iter().zip(verified) {
            assert_eq!(
                target["through_tunnel_verified"],
                json!(verified),
                "{reason}: {target}"
            );
        }
        // Whether this computer binds each probe to the tunnel, apart.
        assert_eq!(targets[0]["bound_probe"], json!(true), "{traffic}");
        assert_eq!(targets[1]["bound_probe"], json!(false), "{traffic}");
    }
}

/// Codex review of FIX-03, P3-01: the check's probe request whose answer was
/// lost may have started the probe, so it is not "not started"; a probe the
/// watchdog refused is.
#[test]
fn a_reconnect_check_whose_probe_answer_is_lost_is_not_said_not_started() {
    let mut lost = Fake {
        lose: Some(|r| matches!(r, Request::ProbeNow { .. })),
        ..Fake::default()
    };
    let result = run(
        &mut lost,
        "reconnect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
    );
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["reason"], json!("probe_unknown"), "{result}");
    assert_eq!(traffic["recovered"], Value::Null, "{traffic}");
    assert!(
        traffic["why"]
            .as_str()
            .unwrap()
            .contains("may or may not have started"),
        "{traffic}"
    );
    assert_eq!(
        lost.sent
            .iter()
            .filter(|r| matches!(r, Request::ProbeNow { .. }))
            .count(),
        1,
        "never sent again"
    );

    let mut refused = Fake {
        answers: VecDeque::from([
            Outcome::Ok(Response::Accepted),
            Outcome::Err(ProtocolError::new(
                ErrorCode::Busy,
                Some(ErrorReason::TooManyWaiting),
                "busy",
            )),
        ]),
        ..Fake::default()
    };
    let result = run(
        &mut refused,
        "reconnect_tunnel",
        json!({ "tunnel_id": "office-1", "wait_secs": 1 }),
    );
    let traffic = &result["structuredContent"]["traffic"];
    assert_eq!(traffic["reason"], json!("probe_not_started"), "{result}");
}
