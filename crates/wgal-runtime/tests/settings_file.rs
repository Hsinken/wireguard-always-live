//! The settings file (owner decision of 2026-11-04): what an export holds
//! and leaves out, and how an import matches, authorizes and applies each
//! entry, or only says what it would do. Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use wgal_protocol::{
    AutoRecoveryImport, ErrorCode, ImportMatch, ImportOptions, ImportOutcome, ImportResult,
    Outcome, ProbeTiming, RecoveryRetry, Request, Response, SettingsDocument, TunnelId,
};
use wgal_runtime::driver::Options;
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

fn export(sim: &mut Sim) -> SettingsDocument {
    match sim.ok(Request::ExportSettings).outcome {
        Outcome::Ok(Response::SettingsExport { document }) => document,
        other => panic!("not an export: {other:?}"),
    }
}

fn import(
    sim: &mut Sim,
    caller: &wgal_runtime::ipc::Caller,
    document: SettingsDocument,
    match_by: ImportMatch,
    dry_run: bool,
) -> ImportResult {
    let answer = sim
        .call(
            caller,
            Request::ImportSettings {
                document,
                options: ImportOptions {
                    match_by,
                    register_missing: true,
                    monitor_new: false,
                    dry_run,
                },
            },
        )
        .expect("an import never waits");
    match answer.outcome {
        Outcome::Ok(Response::SettingsImport { result }) => result,
        other => panic!("not an import result: {other:?}"),
    }
}

fn tunnel_of(sim: &mut Sim, name: &str) -> Option<wgal_protocol::TunnelStatus> {
    sim.snapshot()
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id(name))
        .cloned()
}

#[test]
fn an_export_holds_every_tunnel_and_no_secret() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    let document = export(&mut sim);
    assert_eq!(document.tunnels.len(), 2);
    let alpha = &document.tunnels[0];
    assert_eq!(alpha.tunnel_id, id("alpha"));
    assert_eq!(alpha.probe, ProbeTiming::DEFAULT);
    assert_eq!(alpha.retry, RecoveryRetry::DEFAULT);
    assert!(alpha.recovery_target.is_some());
    let text = serde_json::to_string(&document).unwrap();
    for word in ["device_name", "chat", "token", "bot"] {
        assert!(!text.contains(word), "{word} in {text}");
    }
}

#[test]
fn an_import_by_id_puts_the_settings_back_and_a_dry_run_changes_nothing() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    let saved = export(&mut sim);
    // Something is changed after the export.
    sim.ok(Request::SetRecoveryRetry {
        tunnel_id: id("alpha"),
        retry: RecoveryRetry::new(Some(3), 600).unwrap(),
    });
    sim.ok(Request::SetTunnelNotifications {
        tunnel_id: id("beta"),
        enabled: false,
    });

    let dry = import(&mut sim, &admin(), saved.clone(), ImportMatch::Id, true);
    assert!(dry.dry_run);
    assert_eq!(dry.tunnels.len(), 2);
    for item in dry.tunnels.iter() {
        assert_eq!(item.outcome, ImportOutcome::Applied, "{item:?}");
    }
    assert_eq!(
        tunnel_of(&mut sim, "alpha").unwrap().retry,
        RecoveryRetry::new(Some(3), 600).unwrap(),
        "a dry run changes nothing"
    );

    let done = import(&mut sim, &admin(), saved, ImportMatch::Id, false);
    assert!(!done.dry_run);
    assert_eq!(done.host, ImportOutcome::Applied);
    assert_eq!(
        tunnel_of(&mut sim, "alpha").unwrap().retry,
        RecoveryRetry::DEFAULT
    );
    assert!(tunnel_of(&mut sim, "beta").unwrap().notifications_enabled);
}

/// Alpha's probe timing and retry changed after the export.
fn changed_since_the_export(sim: &mut Sim) -> (SettingsDocument, ProbeTiming, RecoveryRetry) {
    sim.settle();
    let saved = export(sim);
    let probe = ProbeTiming::new(30, 3, 1, 30).unwrap();
    let retry = RecoveryRetry::new(Some(3), 600).unwrap();
    let alpha = tunnel_of(sim, "alpha").unwrap();
    sim.ok(Request::UpdateMonitorSettings {
        tunnel_id: id("alpha"),
        settings: wgal_protocol::MonitorSettings {
            recovery_target: alpha.recovery_target.map(|t| t.target),
            observe_targets: wgal_protocol::BoundedVec::default(),
            probe,
        },
    });
    sim.ok(Request::SetRecoveryRetry {
        tunnel_id: id("alpha"),
        retry,
    });
    (saved, probe, retry)
}

/// Codex P2-N21: a tunnel's part of an import is stored in one save, so a
/// disk that fills up part way through never leaves half of it set.
#[test]
fn a_tunnels_part_of_an_import_is_set_all_together() {
    let mut sim = Sim::of(&["alpha"], None);
    let (saved, _, _) = changed_since_the_export(&mut sim);
    // One more save works, then none: before, the probe timing was put
    // back and the retry was not.
    sim.store.fail_after(1);
    let result = import(&mut sim, &admin(), saved, ImportMatch::Id, false);
    sim.store.set_failing(false);
    assert_eq!(result.tunnels[0].outcome, ImportOutcome::Applied);
    let alpha = tunnel_of(&mut sim, "alpha").unwrap();
    assert_eq!(alpha.probe, ProbeTiming::DEFAULT);
    assert_eq!(alpha.retry, RecoveryRetry::DEFAULT);
}

/// Codex P2-N21: when that one save fails, nothing of the tunnel's part is
/// set, in memory either, and the result says it was refused.
#[test]
fn a_tunnels_part_that_cannot_be_stored_changes_nothing() {
    let mut sim = Sim::of(&["alpha"], None);
    let (saved, probe, retry) = changed_since_the_export(&mut sim);
    sim.store.set_failing(true);
    let result = import(&mut sim, &admin(), saved, ImportMatch::Id, false);
    sim.store.set_failing(false);
    assert!(
        matches!(
            result.tunnels[0].outcome,
            ImportOutcome::Refused {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ),
        "{result:?}"
    );
    let alpha = tunnel_of(&mut sim, "alpha").unwrap();
    assert_eq!(alpha.probe, probe, "nothing set");
    assert_eq!(alpha.retry, retry, "nothing set");
}

/// Codex P2-N22: an export made while automatic recovery waits for the
/// baseline has the "start when stopped externally" the import asked for,
/// either way, so importing it again asks for the same.
#[test]
fn an_export_while_waiting_for_the_baseline_keeps_what_the_import_asked_for() {
    for (asked, before) in [(true, false), (false, true)] {
        let mut sim = Sim::of(&["alpha"], None);
        sim.settle();
        sim.ok(Request::SetStartWhenStoppedExternally {
            tunnel_id: id("alpha"),
            enabled: before,
        });
        let mut document = export(&mut sim);
        let mut entries = document.tunnels.to_vec();
        entries[0].auto_recovery = true;
        entries[0].start_when_stopped_externally = asked;
        // A new recovery target: its baseline has not passed yet.
        entries[0].recovery_target = Some(icmp(9));
        document.tunnels = entries.try_into().unwrap();
        let result = import(&mut sim, &admin(), document, ImportMatch::Id, false);
        assert_eq!(
            result.tunnels[0].auto_recovery,
            AutoRecoveryImport::WaitingForBaseline
        );
        let again = export(&mut sim);
        assert!(again.tunnels[0].auto_recovery);
        assert_eq!(
            again.tunnels[0].start_when_stopped_externally, asked,
            "asked for {asked}, set to {before} before"
        );
    }
}

/// Codex P3-N24: a dry run says what the import would do for a tunnel it
/// would register, judging the automatic options by the binding it would be
/// registered with, as the import itself does.
#[test]
fn a_dry_run_judges_a_tunnel_it_would_register_as_the_import_does() {
    let mut source = Sim::of(&["alpha", "beta"], None);
    source.settle();
    source.ok(Request::SetAutoRecovery {
        tunnel_id: id("beta"),
        enabled: true,
    });
    let document = export(&mut source);

    let lab = Lab::new(&["alpha", "beta"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = wgal_core::MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let mut access = AccessList::default();
    access.tunnels.insert(binding("beta"));
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        access,
        None,
        &Options::default(),
    );
    sim.settle();
    let dry = import(&mut sim, &user(), document.clone(), ImportMatch::Name, true);
    assert!(
        tunnel_of(&mut sim, "beta").is_none(),
        "a dry run registers nothing"
    );
    // An import that cannot be stored registers nothing either (Codex
    // P2-N21): the registration is undone with the rest.
    store.set_failing(true);
    let failed = import(
        &mut sim,
        &user(),
        document.clone(),
        ImportMatch::Name,
        false,
    );
    store.set_failing(false);
    assert!(
        matches!(
            failed.tunnels[1].outcome,
            ImportOutcome::Refused {
                code: ErrorCode::StorageUnavailable,
                ..
            }
        ),
        "{failed:?}"
    );
    assert!(tunnel_of(&mut sim, "beta").is_none(), "registration undone");
    let done = import(&mut sim, &user(), document, ImportMatch::Name, false);
    assert_eq!(dry.tunnels[1].outcome, ImportOutcome::Registered);
    assert_eq!(dry.tunnels[1].outcome, done.tunnels[1].outcome);
    assert_eq!(
        dry.tunnels[1].auto_recovery, done.tunnels[1].auto_recovery,
        "{dry:?}"
    );
    assert_eq!(
        done.tunnels[1].auto_recovery,
        AutoRecoveryImport::WaitingForBaseline
    );
}

/// Codex P2-N28: an import may change any tunnel, so it waits while any
/// tunnel's stop or start is planned or out, as a request that names that
/// tunnel does; it is answered once the stop is over.
#[test]
fn an_import_waits_while_a_stop_or_start_is_out() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    let saved = export(&mut sim);
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    let stop = work
        .iter()
        .find(|i| i.is_control())
        .cloned()
        .expect("the stop is handed out");
    let held = sim.call(
        &admin(),
        Request::ImportSettings {
            document: saved,
            options: ImportOptions {
                match_by: ImportMatch::Id,
                register_missing: true,
                monitor_new: false,
                dry_run: false,
            },
        },
    );
    assert!(held.is_none(), "the import did not wait for the stop");
    sim.finish(&stop);
    assert!(
        sim.released
            .values()
            .any(|answer| matches!(answer.outcome, Outcome::Ok(Response::SettingsImport { .. }))),
        "the import was not answered once the stop was over"
    );
}

/// Codex P2-N32: the host settings and the notification switch are stored
/// apart, so each has its own outcome: a switch that cannot be stored is
/// refused on its own, and the host settings it came with are set.
#[test]
fn the_host_settings_and_the_notification_switch_are_reported_apart() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = wgal_core::MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let settings = wgal_runtime::notify::MemorySettingsStore::new();
    let notifier = wgal_runtime::notify::Notifier::new(
        settings.clone(),
        wgal_runtime::notify::MemorySecretStore::new(),
    );
    let mut sim = Sim::new(
        wd,
        notifier,
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    );
    sim.settle();
    let mut document = export(&mut sim);
    let workers = wgal_protocol::WorkerCount::new(5).unwrap();
    document.host.workers = Some(workers);
    document.host.notifications_enabled = !document.host.notifications_enabled;

    let dry = import(&mut sim, &admin(), document.clone(), ImportMatch::Id, true);
    assert_eq!(dry.host, ImportOutcome::Applied);
    assert_eq!(dry.notifications, Some(ImportOutcome::Applied));

    settings.set_failing(true);
    let result = import(&mut sim, &admin(), document, ImportMatch::Id, false);
    settings.set_failing(false);
    assert_eq!(result.host, ImportOutcome::Applied);
    assert!(
        matches!(
            result.notifications,
            Some(ImportOutcome::Refused {
                code: ErrorCode::StorageUnavailable,
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(sim.host().workers.configured, Some(workers));
}

#[test]
fn an_import_by_name_says_what_it_could_not_match() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    let mut document = export(&mut sim);
    let mut entries = document.tunnels.to_vec();
    // An entry no tunnel here has, and two entries for the same name.
    let mut unknown = entries[0].clone();
    unknown.name = wgal_protocol::DisplayName::new("Nowhere").unwrap();
    unknown.tunnel_id = TunnelId::new("nowhere").unwrap();
    let twin = entries[1].clone();
    entries.push(unknown);
    entries.push(twin);
    document.tunnels = entries.try_into().unwrap();
    let result = import(&mut sim, &admin(), document, ImportMatch::Name, false);
    let outcomes: Vec<_> = result.tunnels.iter().map(|i| i.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        [
            ImportOutcome::Applied,
            ImportOutcome::DuplicateInFile,
            ImportOutcome::NotFound,
            ImportOutcome::DuplicateInFile,
        ]
    );
    assert_eq!(result.tunnels[2].tunnel_id, None);
}

/// A tunnel the official manager has but WGAL does not monitor yet is
/// registered only within the installation's access list, left unmonitored
/// unless asked, and its automatic recovery waits for the baseline.
#[test]
fn a_missing_tunnel_is_registered_within_the_access_list() {
    let mut source = Sim::of(&["alpha", "beta"], None);
    source.settle();
    source.ok(Request::SetAutoRecovery {
        tunnel_id: id("beta"),
        enabled: true,
    });
    let document = export(&mut source);

    let lab = Lab::new(&["alpha", "beta"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = wgal_core::MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    );
    sim.settle();

    // A plain user, with no tunnel on the access list: not registered.
    let refused = import(
        &mut sim,
        &user(),
        document.clone(),
        ImportMatch::Name,
        false,
    );
    let beta = &refused.tunnels[1];
    assert!(
        matches!(
            beta.outcome,
            ImportOutcome::Refused {
                code: ErrorCode::Unauthorized,
                ..
            }
        ),
        "{beta:?}"
    );
    assert!(tunnel_of(&mut sim, "beta").is_none());

    // An administrator: registered, left unmonitored, and automatic
    // recovery waits for a baseline it cannot pass while unmonitored.
    let done = import(&mut sim, &admin(), document, ImportMatch::Name, false);
    let beta = &done.tunnels[1];
    assert_eq!(beta.outcome, ImportOutcome::Registered);
    assert_eq!(beta.auto_recovery, AutoRecoveryImport::WaitingForBaseline);
    let status = tunnel_of(&mut sim, "beta").unwrap();
    assert!(!status.intent.monitoring_enabled, "new ones stay off");
    assert!(status.auto_recovery_pending);
    assert!(!status.intent.auto_recovery_enabled);
}

/// Owner decision of 2026-11-04: while MCP is off, the watchdog refuses
/// every request `wgal-mcp` sends; once the window turns it on, an agent's
/// changes are written to the log as the agent's.
#[test]
fn agents_are_answered_only_while_mcp_is_on() {
    use wgal_protocol::{ErrorReason, LogEvent, Origin, RequestOp};

    let dir = std::env::temp_dir().join(format!("wgal-mcp-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = wgal_runtime::events::EventLog::open(&dir).unwrap();
    let mut sim = Sim::of(&["alpha"], Some(log));
    sim.settle();
    let ask = |sim: &mut Sim, request: Request| {
        let mut envelope = envelope("mcp-1", request);
        envelope.origin = Some(Origin::Mcp);
        sim.engine
            .request(sim.next_call_id(), &user(), envelope)
            .expect("never waits")
    };
    let refused = ask(&mut sim, Request::GetSnapshot);
    let Outcome::Err(error) = refused.outcome else {
        panic!("answered while off: {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::Unauthorized);
    assert_eq!(error.reason, Some(ErrorReason::McpDisabled));
    // An agent cannot turn it on itself.
    assert!(matches!(
        ask(&mut sim, Request::SetMcpEnabled { enabled: true }).outcome,
        Outcome::Err(_)
    ));
    assert!(!sim.host().mcp_enabled);

    sim.ok(Request::SetMcpEnabled { enabled: true });
    assert!(sim.host().mcp_enabled);
    assert!(matches!(
        ask(&mut sim, Request::GetSnapshot).outcome,
        Outcome::Ok(Response::Snapshot { .. })
    ));
    assert!(matches!(
        ask(
            &mut sim,
            Request::SetTunnelNotifications {
                tunnel_id: id("alpha"),
                enabled: false,
            }
        )
        .outcome,
        Outcome::Ok(_)
    ));
    let lines = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    let agent: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains("agent_request"))
        .collect();
    assert_eq!(agent.len(), 1, "{lines}");
    assert!(
        agent[0].contains("set_tunnel_notifications"),
        "{}",
        agent[0]
    );
    let _ = (
        LogEvent::AgentRequest {
            op: RequestOp::GetSnapshot,
        },
        std::fs::remove_dir_all(&dir),
    );
}

/// Owner decision of 2026-11-06: an agent may change the device name, chat
/// ID and bot token only while the window lets it, never lets itself, and
/// may still turn notifications on or off without it. The switch is kept
/// while MCP is off, without effect. The token here is the test kit's, in
/// the in-memory secret store.
#[test]
fn agents_change_telegram_only_while_the_window_lets_them() {
    use wgal_protocol::{BotToken, ChatId, CredentialState, DeviceName, ErrorReason, Origin};

    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    let settings =
        |enabled: bool, device: &str, chat: Option<&str>| Request::UpdateNotificationSettings {
            enabled,
            device_name: DeviceName::new(device).unwrap(),
            chat_id: chat.map(|c| ChatId::new(c).unwrap()),
        };
    sim.ok(Request::SetMcpEnabled { enabled: true });
    sim.ok(settings(false, "Lab laptop", Some(CHAT)));
    let ask = |sim: &mut Sim, request: Request| {
        let mut envelope = envelope("mcp-1", request);
        envelope.origin = Some(Origin::Mcp);
        sim.engine
            .request(sim.next_call_id(), &user(), envelope)
            .expect("never waits")
            .outcome
    };
    let reason = |outcome: Outcome| match outcome {
        Outcome::Err(error) => {
            assert_eq!(error.code, ErrorCode::Unauthorized);
            error.reason
        }
        Outcome::Ok(response) => panic!("accepted: {response:?}"),
    };
    let accepted = |outcome: Outcome| assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    let token = || Request::SetBotToken {
        token: BotToken::new(TOKEN).unwrap(),
    };
    let off = Some(ErrorReason::McpTelegramDisabled);

    // Not allowed: none of the three changes, but notifications still
    // turn on and off with the device name and chat they have.
    assert_eq!(reason(ask(&mut sim, token())), off);
    assert_eq!(reason(ask(&mut sim, Request::ClearBotToken)), off);
    assert_eq!(
        reason(ask(&mut sim, settings(false, "Other", Some(CHAT)))),
        off
    );
    assert_eq!(
        reason(ask(&mut sim, settings(false, "Lab laptop", None))),
        off
    );
    assert_eq!(
        reason(ask(
            &mut sim,
            settings(true, "Lab laptop", Some("@another_chat"))
        )),
        off
    );
    accepted(ask(&mut sim, settings(true, "Lab laptop", Some(CHAT))));
    let n = sim.snapshot().notification;
    assert!(n.enabled);
    assert_eq!(
        (
            n.device_name.as_str(),
            n.chat_id.as_ref().map(ChatId::as_str)
        ),
        ("Lab laptop", Some(CHAT))
    );
    assert_eq!(n.token, CredentialState::NotSet);
    // An agent cannot let itself.
    assert_eq!(
        reason(ask(
            &mut sim,
            Request::SetMcpTelegramEnabled { enabled: true }
        )),
        off
    );
    assert!(!sim.host().mcp_telegram_enabled);

    // The window lets it: the token is replaced and cleared, the device
    // name and chat ID changed and cleared.
    sim.ok(Request::SetMcpTelegramEnabled { enabled: true });
    assert!(sim.host().mcp_telegram_enabled);
    accepted(ask(&mut sim, token()));
    assert_eq!(sim.snapshot().notification.token, CredentialState::Stored);
    accepted(ask(&mut sim, settings(true, "", None)));
    let n = sim.snapshot().notification;
    assert_eq!((n.device_name.as_str(), n.chat_id), ("", None));
    accepted(ask(&mut sim, Request::ClearBotToken));
    assert_eq!(sim.snapshot().notification.token, CredentialState::NotSet);

    // MCP off keeps the switch, without effect; on again, it is back.
    sim.ok(Request::SetMcpEnabled { enabled: false });
    assert!(sim.host().mcp_telegram_enabled);
    assert_eq!(
        reason(ask(&mut sim, token())),
        Some(ErrorReason::McpDisabled)
    );
    sim.ok(Request::SetMcpEnabled { enabled: true });
    accepted(ask(&mut sim, token()));

    // An agent may narrow what it may do, like turning MCP off.
    accepted(ask(
        &mut sim,
        Request::SetMcpTelegramEnabled { enabled: false },
    ));
    assert!(!sim.host().mcp_telegram_enabled);
    assert_eq!(reason(ask(&mut sim, Request::ClearBotToken)), off);
    assert_eq!(sim.snapshot().notification.token, CredentialState::Stored);
}
