//! The loop's owner-thread part ([`wgal_runtime::driver::Engine`]) driven
//! step by step with a fake clock and a fake platform: which work it hands
//! out and when, the control rule, authorization, a whole recovery, the
//! notification job, shutdown and idling. Simulation only; the threads
//! themselves are tested in `loop_threads.rs`.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use wgal_core::{Clock, MemoryStore, ResumeOp, Startup, WorkKind};
use wgal_protocol::{
    DesiredState, ErrorCode, ErrorReason, FirstRecovery, Light, LightReason, NotificationState,
    OsState, PROTOCOL_VERSION, Request,
};
use wgal_runtime::driver::Options;
use wgal_runtime::events::EventLog;
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

/// Runs whole turns until `pick` matches an item handed out, which is kept
/// out and returned; everything else runs at once.
fn hand_out_until(
    sim: &mut Sim,
    mut pick: impl FnMut(&wgal_core::WorkItem) -> bool,
) -> wgal_core::WorkItem {
    for _ in 0..10_000 {
        let (work, _) = sim.turn_only();
        let idle = work.is_empty();
        let mut found = None;
        for item in work {
            if found.is_none() && pick(&item) {
                found = Some(item);
            } else {
                sim.finish(&item);
            }
        }
        if let Some(item) = found {
            return item;
        }
        if idle {
            let wait = sim.engine.idle_wait();
            sim.clock.advance(wait).unwrap();
        }
    }
    panic!("no such item was handed out");
}

fn is_round_of(name: &str) -> impl FnMut(&wgal_core::WorkItem) -> bool + '_ {
    move |item| item.tunnel_id == id(name) && matches!(item.kind, WorkKind::Round { .. })
}

fn is_stop_of(name: &str) -> impl FnMut(&wgal_core::WorkItem) -> bool + '_ {
    move |item| item.tunnel_id == id(name) && item.kind == WorkKind::Stop
}

#[test]
fn requests_are_answered_while_a_probe_is_out() {
    let mut sim = Sim::of(&["alpha"], None);
    let round = hand_out_until(&mut sim, is_round_of("alpha"));
    assert!(!sim.engine.is_idle());
    // The snapshot, a request for the same tunnel and a host-wide one are
    // answered while the round is out.
    let snapshot = sim.call(&user(), Request::GetSnapshot).unwrap();
    assert!(error_code(&snapshot).is_none());
    let probe = sim
        .call(
            &user(),
            Request::ProbeNow {
                tunnel_id: id("alpha"),
            },
        )
        .unwrap();
    assert!(accepted(&probe));
    let pause = sim
        .call(&user(), Request::SetMonitoringPaused { paused: false })
        .unwrap();
    assert!(accepted(&pause));
    sim.finish(&round);
    sim.settle();
    assert!(sim.engine.is_idle());
    assert_eq!(sim.status("alpha").light, Light::Green);
}

#[test]
fn a_request_for_a_tunnel_waits_while_its_stop_is_out() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    assert_eq!(work.len(), 1, "{work:?}");
    let stop = work[0].clone();
    assert!(stop.is_control() && stop.tunnel_id == id("alpha"));

    // A request for alpha waits for the stop's result...
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Connect {
                tunnel_id: id("alpha")
            }
        )
        .is_none()
    );
    assert_eq!(sim.engine.stats().held, 1);
    // ...while beta and the host are answered.
    assert!(
        sim.call(
            &user(),
            Request::SetAutoRecovery {
                tunnel_id: id("beta"),
                enabled: false,
            }
        )
        .is_some_and(|a| accepted(&a))
    );
    assert!(sim.call(&user(), Request::GetSnapshot).is_some());
    // Turns in between do not release it, and the stop is not handed out
    // twice.
    let (again, _) = sim.turn_only();
    assert!(again.is_empty(), "{again:?}");
    assert!(sim.released.is_empty());

    sim.finish(&stop);
    let answer = sim.released.remove(&call).expect("released with the stop");
    assert!(accepted(&answer), "{answer:?}");
    assert_eq!(sim.engine.stats().held, 0);
    // The connect was applied after the stop: WGAL starts the tunnel again.
    sim.run(60);
    assert_eq!(sim.lab.stops("alpha"), 1);
    assert_eq!(sim.lab.starts("alpha"), 1);
    let status = sim.status("alpha");
    assert_eq!(status.intent.desired, DesiredState::Connected);
    assert_eq!(status.os_state, OsState::Connected);
    assert_eq!(status.light, Light::Green);
}

#[test]
fn a_stop_planned_but_not_handed_out_holds_requests_too() {
    // Two workers: one round and one poll fill them.
    let lab = Lab::new(&["beta"]);
    lab.fake()
        .add_tunnel(binding("alpha"), "Tunnel alpha", OsState::Disconnected);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha", "beta"]);
    let options = Options {
        workers: 2,
        ..Options::default()
    };
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &options,
    );
    let beta_round = hand_out_until(&mut sim, is_round_of("beta"));

    sim.ok(Request::Connect {
        tunnel_id: id("alpha"),
    });
    let start = hand_out_until(&mut sim, |i| i.kind == WorkKind::Start);
    sim.finish(&start);
    let poll = hand_out_until(&mut sim, |i| i.kind == WorkKind::Poll);
    assert_eq!(sim.engine.stats().in_flight, 2);

    // Not locked: only a poll is out. The disconnect replaces the start
    // with a stop, which waits for a worker.
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    assert!(work.is_empty(), "{work:?}");
    assert_eq!(sim.engine.stats().ready, 1);
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Connect {
                tunnel_id: id("alpha")
            }
        )
        .is_none(),
        "waits for the planned stop"
    );
    assert!(sim.call(&user(), Request::GetSnapshot).is_some());

    sim.finish(&poll);
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(!sim.released.contains_key(&call));
    sim.finish(&stop);
    assert!(accepted(&sim.released.remove(&call).unwrap()));
    sim.finish(&beta_round);
    sim.run(60);
    assert_eq!(sim.status("alpha").intent.desired, DesiredState::Connected);
    assert_eq!(sim.status("alpha").os_state, OsState::Connected);
}

#[test]
fn rounds_never_take_the_worker_kept_for_a_stop() {
    let names = ["a1", "a2", "a3", "a4", "a5"];
    let lab = Lab::new(&names);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &names);
    let options = Options {
        workers: 3,
        ..Options::default()
    };
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &options,
    );
    // Three workers: the suggestion for five idle-looking tunnels is two.
    assert!(accepted(&sim.host_settings(Some(3), None)));
    // Make every round due at once and keep them out.
    for name in names {
        sim.ok(Request::ProbeNow {
            tunnel_id: id(name),
        });
    }
    let (rounds, _) = sim.turn_only();
    assert_eq!(rounds.len(), 2, "rounds use at most workers - 1");
    assert_eq!(sim.engine.stats().ready, 3);
    sim.ok(Request::Disconnect {
        tunnel_id: id("a5"),
    });
    let (work, _) = sim.turn_only();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].kind, WorkKind::Stop);
    for item in rounds.iter().chain(&work) {
        sim.finish(item);
    }
    sim.settle();
    assert!(sim.engine.is_idle());
}

#[test]
fn a_full_recovery_runs_through_the_loop() {
    let dir = TempDir::new("recovery");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.run(30);
    assert_eq!(sim.status("alpha").light, Light::Green);
    // Connected but dead: the manager still reports it up.
    sim.lab.kill_until_restart("alpha");
    let mut saw = Vec::new();
    for _ in 0..300 {
        sim.run(1);
        let status = sim.status("alpha");
        if saw.last() != Some(&status.light_reason) {
            saw.push(status.light_reason);
        }
        if sim.lab.starts("alpha") == 1 && status.light == Light::Green {
            break;
        }
    }
    assert_eq!(sim.lab.stops("alpha"), 1, "{saw:?}");
    assert_eq!(sim.lab.starts("alpha"), 1);
    let status = sim.status("alpha");
    assert_eq!(status.light_reason, LightReason::Verified);
    assert_eq!(
        status.incident.as_ref().map(|i| i.first_recovery),
        Some(FirstRecovery::Succeeded)
    );
    // The fake platform stops, starts and answers at once, so a snapshot a
    // second later is already green again; the event log below sees each
    // step, since it compares the snapshots of consecutive turns.
    assert!(saw.contains(&LightReason::Confirming), "{saw:?}");
    // Stable for two minutes: the incident closes, nothing was sent.
    sim.run(150);
    assert!(sim.status("alpha").incident.is_none());
    assert_eq!(sim.transport.sends(), 0);

    let lines = log_lines(dir.path());
    let opened = diags_named(&lines, "incident_opened");
    assert_eq!(opened.len(), 1);
    let incident = opened[0]["diag"]["incident"].clone();
    assert!(diags_named(&lines, "first_recovery").iter().any(|l| {
        l["diag"]["incident"] == incident && l["diag"]["first_recovery"] == "succeeded"
    }));
    let phases: Vec<&str> = diags_named(&lines, "recovery")
        .iter()
        .map(|l| l["diag"]["phase"].as_str().unwrap())
        .collect();
    // Queued and granted the control slot within one turn, so `queued` is
    // never seen between turns.
    for phase in ["stopping", "starting", "verifying", "idle"] {
        assert!(phases.contains(&phase), "{phases:?}");
    }
    let lights: Vec<&str> = events_named(&lines, "light_changed")
        .iter()
        .map(|l| l["event"]["reason"].as_str().unwrap())
        .collect();
    for reason in ["confirming", "reconnecting", "verifying", "verified"] {
        assert!(lights.contains(&reason), "{lights:?}");
    }
    assert!(
        diags_named(&lines, "incident_closed")
            .iter()
            .any(|l| l["diag"]["incident"] == incident)
    );
}

/// A managed tunnel whose data path is dead for good, in a loop with
/// notifications set up.
fn faulted_sim() -> Sim {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &["alpha"]);
    manage(&mut wd, "alpha");
    let (notifier, _, _) = notifier(&mut wd);
    lab.kill_for_good("alpha");
    Sim::new(
        wd,
        notifier,
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    )
}

#[test]
fn the_notification_job_is_handed_out_once_and_its_result_applied_once() {
    let mut sim = faulted_sim();
    // Run turn by turn, keeping the job out when it comes.
    let mut job = None;
    for _ in 0..5000 {
        let turn = sim.engine.turn();
        for item in &turn.work {
            sim.finish(item);
        }
        if let Some(j) = turn.job {
            job = Some(j);
            break;
        }
        if turn.work.is_empty() {
            let wait = sim.engine.idle_wait();
            sim.clock.advance(wait).unwrap();
        }
    }
    let job = job.expect("the alert became due");
    assert!(!job.is_test());
    assert_eq!(
        sim.status("alpha").incident.unwrap().notification,
        NotificationState::Reserved
    );
    // No second job while one is out, however long it takes.
    for _ in 0..20 {
        sim.clock.advance(Duration::from_secs(5)).unwrap();
        let turn = sim.engine.turn();
        assert!(turn.job.is_none());
        for item in &turn.work {
            sim.finish(item);
        }
    }
    let delivery = job.run(&sim.transport);
    sim.engine.delivered(job, delivery);
    assert_eq!(sim.transport.sends(), 1);
    assert_eq!(
        sim.status("alpha").incident.unwrap().notification,
        NotificationState::Sent
    );
    // The same incident never sends again.
    sim.run(1800);
    assert_eq!(sim.transport.sends(), 1);
    assert!(sim.lab.starts("alpha") > 1, "recovery keeps retrying");
}

#[test]
fn stopping_during_a_recovery_stop_leaves_a_state_the_core_resumes() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));

    let busy = sim.engine.stop_planning();
    assert!(busy.is_empty());
    assert!(!sim.engine.is_idle());
    let refused = sim.call(&user(), Request::GetSnapshot).unwrap();
    assert_eq!(
        refusal_of(&refused),
        Some((ErrorCode::Busy, Some(ErrorReason::ShuttingDown)))
    );
    // Nothing new is handed out while stopping.
    sim.clock.advance(Duration::from_secs(20)).unwrap();
    let (work, job) = sim.turn_only();
    assert!(work.is_empty() && !job);

    // The deadline passes with the stop still out.
    let Sim {
        engine,
        lab,
        clock,
        store,
        ..
    } = sim;
    let (in_time, answers) = engine.shut_down();
    assert!(!in_time, "the stop was still out");
    assert!(answers.is_empty());
    let saved = store.saved().unwrap();
    let rec = &saved.tunnels[0];
    assert_eq!(rec.resume, Some(ResumeOp::Recovery));
    assert_eq!(
        rec.incident.as_ref().map(|i| i.first_recovery),
        Some(FirstRecovery::InProgress)
    );

    // The stop lands after all; the next watchdog finishes the round.
    wgal_core::PlatformAdapter::stop(&lab, &binding("alpha")).unwrap();
    assert_eq!(
        lab.fake().os_state(&binding("alpha")),
        OsState::Disconnected
    );
    let store = MemoryStore::new();
    let wd = watchdog(&lab, &clock, &store, Startup::Restored(saved));
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
    sim.run(60);
    assert_eq!(stop.kind, WorkKind::Stop);
    assert_eq!(lab.starts("alpha"), 1);
    let status = sim.status("alpha");
    assert_eq!(status.light, Light::Green, "{status:?}");
    assert_eq!(
        status.incident.as_ref().map(|i| i.first_recovery),
        Some(FirstRecovery::Succeeded)
    );
}

#[test]
fn stopping_right_after_a_recovery_stop_still_starts_the_tunnel() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    let probes = (sim.lab.probes("alpha"), sim.lab.probes("beta"));

    // The stop's result comes back; its polls and its start still run, and
    // nothing else does: no round, no verification, no other tunnel.
    sim.finish(&stop);
    let mut kinds = Vec::new();
    for _ in 0..100 {
        sim.clock.advance(Duration::from_secs(1)).unwrap();
        let (work, job) = sim.turn_only();
        assert!(!job);
        for item in &work {
            assert_eq!(item.tunnel_id, id("alpha"), "{item:?}");
            kinds.push(item.kind.clone());
            sim.finish(item);
        }
    }
    assert!(kinds.contains(&WorkKind::Start), "{kinds:?}");
    assert!(
        kinds
            .iter()
            .all(|k| matches!(k, WorkKind::Poll | WorkKind::Start)),
        "{kinds:?}"
    );
    assert!(sim.engine.is_idle());
    assert_eq!(sim.lab.starts("alpha"), 1);
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Connected
    );
    assert_eq!(
        (sim.lab.probes("alpha"), sim.lab.probes("beta")),
        probes,
        "nothing was probed"
    );
    let Sim { engine, store, .. } = sim;
    let (in_time, _) = engine.shut_down();
    assert!(in_time, "the start was sent");
    // Verification did not finish; the stored round resumes after a restart.
    assert_eq!(
        store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Recovery)
    );
}

#[test]
fn waiting_requests_are_refused_when_the_loop_stops() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    assert_eq!(work.len(), 1);
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Connect {
                tunnel_id: id("alpha")
            }
        )
        .is_none()
    );
    let answers = sim.engine.stop_planning();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, call);
    assert_eq!(
        refusal_of(&answers[0].1),
        Some((ErrorCode::Busy, Some(ErrorReason::ShuttingDown)))
    );
    // The refused connect was never applied.
    sim.finish(&work[0]);
    let saved = sim.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].intent.desired, DesiredState::Disconnected);
}

#[test]
fn unauthorized_callers_are_refused_every_request_and_change_nothing() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    let before = sim.store.saved();
    let calls = sim.lab.fake().calls().len();
    for elevated in [false, true] {
        for request in every_request("alpha") {
            let answer = sim.call(&stranger(elevated), request.clone()).unwrap();
            assert_eq!(
                refusal_of(&answer),
                Some((ErrorCode::Unauthorized, Some(ErrorReason::CallerNotAllowed))),
                "{}",
                kind(&request)
            );
            assert!(answer.request_id.is_some());
        }
    }
    assert_eq!(sim.store.saved(), before);
    assert_eq!(sim.lab.fake().calls().len(), calls, "no platform call");

    // Refused at once even for a tunnel whose stop is out.
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    assert_eq!(work.len(), 1);
    let answer = sim
        .call(
            &stranger(false),
            Request::Connect {
                tunnel_id: id("alpha"),
            },
        )
        .unwrap();
    assert_eq!(error_code(&answer), Some(ErrorCode::Unauthorized));
    assert_eq!(sim.engine.stats().held, 0);
}

fn access_sim(access: &[&str]) -> Sim {
    let lab = Lab::new(&["alpha"]);
    lab.fake()
        .add_tunnel(binding("beta"), "Tunnel beta", OsState::Connected);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let access = AccessList {
        tunnels: access.iter().map(|n| binding(n)).collect(),
    };
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
    sim.ok(Request::DiscoverTunnels);
    sim
}

fn code_for(
    sim: &mut Sim,
    caller: &wgal_runtime::ipc::Caller,
    request: Request,
) -> Option<ErrorCode> {
    let answer = sim.call(caller, request).unwrap();
    // A refusal here is always for want of administrator rights or a listed
    // tunnel.
    if let Some((code, reason)) = refusal_of(&answer) {
        assert_eq!(
            (code, reason),
            (
                ErrorCode::Unauthorized,
                Some(ErrorReason::NeedsAdministratorOrListedTunnel)
            )
        );
    }
    error_code(&answer)
}

#[test]
fn the_access_list_decides_registration_and_automatic_actions() {
    let register = || Request::RegisterTunnel {
        tunnel_id: id("beta"),
    };
    let auto = |enabled| Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled,
    };
    let external = |enabled| Request::SetStartWhenStoppedExternally {
        tunnel_id: id("alpha"),
        enabled,
    };

    // Nothing on the list: a user may only turn things off.
    let mut sim = access_sim(&[]);
    assert_eq!(
        code_for(&mut sim, &user(), register()),
        Some(ErrorCode::Unauthorized)
    );
    assert_eq!(sim.snapshot().tunnels.len(), 1);
    assert_eq!(
        code_for(&mut sim, &user(), auto(true)),
        Some(ErrorCode::Unauthorized)
    );
    assert!(!sim.status("alpha").intent.auto_recovery_enabled);
    assert_eq!(
        code_for(&mut sim, &user(), external(true)),
        Some(ErrorCode::Unauthorized)
    );
    assert_eq!(code_for(&mut sim, &user(), auto(false)), None);
    assert_eq!(code_for(&mut sim, &user(), external(false)), None);
    // An administrator may do all of it.
    assert_eq!(code_for(&mut sim, &admin(), auto(true)), None);
    assert!(sim.status("alpha").intent.auto_recovery_enabled);
    assert_eq!(code_for(&mut sim, &admin(), external(true)), None);
    assert_eq!(code_for(&mut sim, &admin(), register()), None);
    assert_eq!(sim.snapshot().tunnels.len(), 2);

    // Both on the list: a user may register and turn things on.
    let mut sim = access_sim(&["alpha", "beta"]);
    assert_eq!(code_for(&mut sim, &user(), register()), None);
    assert_eq!(sim.snapshot().tunnels.len(), 2);
    assert_eq!(code_for(&mut sim, &user(), auto(true)), None);
    assert_eq!(code_for(&mut sim, &user(), external(true)), None);
    let status = sim.status("alpha");
    assert!(status.intent.auto_recovery_enabled);
    assert!(status.intent.start_when_stopped_externally);
}

#[test]
fn another_protocol_version_is_refused() {
    let mut sim = Sim::of(&["alpha"], None);
    let mut envelope = envelope("req-v2", Request::GetSnapshot);
    envelope.protocol_version = PROTOCOL_VERSION + 1;
    let answer = sim
        .engine
        .request(wgal_runtime::driver::CallId(1), &admin(), envelope)
        .unwrap();
    // The code says it all.
    assert_eq!(
        refusal_of(&answer),
        Some((ErrorCode::UnsupportedVersion, None))
    );
    assert_eq!(answer.request_id.unwrap().as_str(), "req-v2");
}

#[test]
fn notification_requests_go_to_the_notifier_and_snapshots_carry_its_status() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::UpdateNotificationSettings {
        enabled: true,
        device_name: wgal_protocol::DeviceName::new("Lab laptop").unwrap(),
        chat_id: Some(wgal_protocol::ChatId::new(CHAT).unwrap()),
    });
    sim.ok(Request::SetBotToken {
        token: wgal_protocol::BotToken::new(TOKEN).unwrap(),
    });
    let snapshot = sim.snapshot();
    assert!(snapshot.notification.enabled);
    assert_eq!(
        snapshot.notification.token,
        wgal_protocol::CredentialState::Stored
    );
    sim.ok(Request::SendTestNotification);
    sim.settle();
    assert_eq!(sim.transport.sends(), 1);
    assert!(sim.transport.texts()[0].contains("Test notification"));
    let test = sim.snapshot().notification.last_test.unwrap();
    assert_eq!(test.state, wgal_protocol::TestNotificationState::Accepted);
}

/// T28 in simulation: an idle, healthy tunnel. The loop wakes for its
/// rounds and its `max_wait` ticks only, plans once or twice per wake-up,
/// and nothing piles up, including the event log.
#[test]
fn an_idle_watchdog_wakes_only_when_needed() {
    for (max_wait, ticks) in [(Duration::from_secs(60), 0), (Duration::from_secs(5), 720)] {
        let dir = TempDir::new("idle");
        let lab = Lab::new(&["alpha"]);
        let clock = Arc::new(FakeClock::new(T0));
        let store = MemoryStore::new();
        let wd = prepared(&lab, &clock, &store, &["alpha"]);
        let options = Options {
            max_wait,
            ..Options::default()
        };
        let mut sim = Sim::new(
            wd,
            quiet_notifier(),
            &lab,
            &clock,
            &store,
            AccessList::default(),
            Some(EventLog::open(dir.path()).unwrap()),
            &options,
        );
        sim.run(60);
        let lines = log_lines(dir.path()).len();
        let probes = lab.probes("alpha");
        let before = sim.engine.stats();

        let wakes = sim.run(3600);
        let after = sim.engine.stats();
        let rounds = u64::try_from(lab.probes("alpha") - probes).unwrap();
        assert!((239..=241).contains(&rounds), "{rounds} rounds in an hour");
        // Rounds every 15 s plus, with a 5 s `max_wait`, the ticks between.
        assert!(wakes <= ticks.max(rounds) + 2, "{wakes} wake-ups");
        let passes = after.plan_passes - before.plan_passes;
        assert!(passes <= wakes + rounds + 2, "{passes} plan passes");
        assert_eq!((after.ready, after.in_flight, after.held), (0, 0, 0));
        assert_eq!(log_lines(dir.path()).len(), lines, "the log does not grow");
        assert_eq!(sim.status("alpha").light, Light::Green);
    }
}

#[test]
fn the_wait_is_the_time_to_the_next_round() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let options = Options {
        max_wait: Duration::from_secs(60),
        ..Options::default()
    };
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &options,
    );
    let round = hand_out_until(&mut sim, is_round_of("alpha"));
    sim.finish(&round);
    sim.settle();
    assert_eq!(sim.engine.idle_wait(), Duration::from_secs(15));
    sim.clock.advance(Duration::from_secs(10)).unwrap();
    assert_eq!(sim.engine.idle_wait(), Duration::from_secs(5));

    // With the default `max_wait`, never longer than 5 s.
    let mut sim = Sim::of(&["alpha"], None);
    let round = hand_out_until(&mut sim, is_round_of("alpha"));
    sim.finish(&round);
    sim.settle();
    assert_eq!(sim.engine.idle_wait(), Duration::from_secs(5));
}

/// What the owner thread does once stopping (`own` in driver.rs): turn,
/// let what was handed out finish, and wait `stopping_wait` (the next poll)
/// up to the deadline, `deadline` fake seconds from now. Returns what a
/// shutdown reports as finished in time.
fn stopping_loop(sim: &mut Sim, deadline: u64) -> bool {
    let until = sim.clock.now().since_origin() + Duration::from_secs(deadline);
    for _ in 0..10_000 {
        let (work, _) = sim.turn_only();
        if !work.is_empty() {
            // Results come back as messages; the owner turns again at once.
            for item in &work {
                sim.finish(item);
            }
            continue;
        }
        let now = sim.clock.now().since_origin();
        let Some(wait) = sim.engine.stopping_wait().filter(|_| now < until) else {
            return sim.engine.nothing_left();
        };
        sim.clock.advance(wait.min(until - now)).unwrap();
    }
    panic!("the stopping loop never ended");
}

/// Review of wave two, P1-1: a recovery's stop that the manager is still
/// carrying out (STOP_PENDING) when the service stops is polled until it
/// settles, and its start is sent before the loop ends.
#[test]
fn stopping_while_a_recovery_stop_is_pending_still_sends_the_start() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.slow_stop("alpha", 2);
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&stop);
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Transitioning
    );
    assert!(!sim.engine.is_idle(), "a start is owed");
    assert!(stopping_loop(&mut sim, 30), "not finished in time");
    assert_eq!(
        sim.lab.starts("alpha"),
        1,
        "WGAL left the tunnel it stopped down"
    );
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Connected
    );
}

/// P1-1 for a user's reconnect, the same stop then start.
#[test]
fn stopping_while_a_reconnect_stop_is_pending_still_sends_the_start() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    sim.lab.slow_stop("alpha", 3);
    sim.ok(Request::Reconnect {
        tunnel_id: id("alpha"),
    });
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&stop);
    assert!(stopping_loop(&mut sim, 30));
    assert_eq!(sim.lab.starts("alpha"), 1);
}

/// P1-1: a disconnect turned into a reconnect by a connect while its stop
/// was out owes a start too, though its stop did not when it went out.
#[test]
fn a_disconnect_turned_into_a_reconnect_still_gets_its_start_at_shutdown() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    sim.lab.slow_stop("alpha", 3);
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Connect {
                tunnel_id: id("alpha")
            }
        )
        .is_none()
    );
    sim.finish(&stop);
    assert!(accepted(&sim.released.remove(&call).unwrap()));
    assert!(sim.engine.stop_planning().is_empty());
    assert!(stopping_loop(&mut sim, 30));
    assert_eq!(sim.lab.starts("alpha"), 1);
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Connected
    );
}

/// P1-1: a stop still pending when the deadline passes is reported as not
/// finished in time; nothing is started.
#[test]
fn a_stop_still_pending_at_the_deadline_is_not_finished_in_time() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.slow_stop("alpha", 1000);
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&stop);
    assert!(!stopping_loop(&mut sim, 10));
    assert_eq!(sim.lab.starts("alpha"), 0);
}

/// P1-1: a stop the core gives up on (it never settles within a control
/// phase) ends the wait, but is not "finished in time": its start was never
/// sent.
#[test]
fn a_stop_the_core_gives_up_on_is_not_finished_in_time() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.slow_stop("alpha", 1000);
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&stop);
    let started = sim.now_secs();
    assert!(!stopping_loop(&mut sim, 120));
    let took = sim.now_secs() - started;
    assert!(
        (30..60).contains(&took),
        "{took} s: the wait ended with the phase"
    );
    assert!(sim.engine.is_idle(), "nothing more can come of it");
    assert!(!sim.engine.nothing_left());
    assert_eq!(sim.lab.starts("alpha"), 0);
}

/// A user's disconnect owes no start: the loop does not wait for its stop to
/// settle.
#[test]
fn a_disconnect_still_pending_does_not_hold_up_the_shutdown() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    sim.lab.slow_stop("alpha", 1000);
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&stop);
    let started = sim.now_secs();
    assert!(stopping_loop(&mut sim, 30));
    assert!(sim.now_secs() - started < 2);
    assert_eq!(sim.lab.starts("alpha"), 0);
}

/// A managed tunnel whose recovery's stop is out and a user's disconnect
/// waits behind it; returns the stop and the disconnect's call.
fn disconnect_behind_a_recovery_stop(
    sim: &mut Sim,
) -> (wgal_core::WorkItem, wgal_runtime::driver::CallId) {
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(sim, is_stop_of("alpha"));
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Disconnect {
                tunnel_id: id("alpha")
            }
        )
        .is_none()
    );
    (stop, call)
}

/// Review of wave two, N-P3-1 (reproduction r4): a recovery's stop that a
/// user's disconnect took over owes no start; a later shutdown is in time.
#[test]
fn n_p3_1_a_recovery_stop_taken_over_by_a_disconnect_owes_nothing() {
    let mut sim = Sim::of(&["alpha"], None);
    let (stop, call) = disconnect_behind_a_recovery_stop(&mut sim);
    sim.finish(&stop);
    assert!(accepted(&sim.released.remove(&call).unwrap()));
    sim.run(5);
    assert!(sim.engine.nothing_left(), "nothing is owed");
    assert!(sim.engine.stop_planning().is_empty());
    assert!(stopping_loop(&mut sim, 30));
    assert_eq!(sim.lab.starts("alpha"), 0, "the disconnect was kept");
}

/// N-P3-1 (r5): the same an hour later with monitoring paused, when no
/// round has run since.
#[test]
fn n_p3_1_nothing_is_owed_after_an_hour_paused() {
    let mut sim = Sim::of(&["alpha"], None);
    let (stop, _) = disconnect_behind_a_recovery_stop(&mut sim);
    sim.finish(&stop);
    sim.ok(Request::SetMonitoringPaused { paused: true });
    sim.run(3600);
    assert!(sim.engine.stop_planning().is_empty());
    assert!(stopping_loop(&mut sim, 30));
}

/// N-P3-1 (r6): a binding lost during a recovery's stop owes nothing:
/// nothing can be started any more.
#[test]
fn n_p3_1_a_binding_lost_during_a_recovery_stop_owes_nothing() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    sim.lab.fake().fail_next(
        &binding("alpha"),
        wgal_testkit::Operation::Stop,
        wgal_core::AdapterError::new(wgal_protocol::AdapterErrorKind::NotFound, "gone"),
    );
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    sim.finish(&stop);
    sim.run(600);
    assert_eq!(sim.status("alpha").light_reason, LightReason::BindingLost);
    assert!(sim.engine.stop_planning().is_empty());
    assert!(stopping_loop(&mut sim, 30));
}

/// Review of wave two, N-P3-2 (reproduction r10): a user's disconnect
/// waiting behind a recovery's stop when the service stops is not refused:
/// it is applied as soon as the stop is in, so the recovery's start never
/// goes out and the tunnel stays down as the user asked.
#[test]
fn n_p3_2_a_disconnect_waiting_at_shutdown_is_applied_and_no_start_goes_out() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.lab.slow_stop("alpha", 2);
    let (stop, call) = disconnect_behind_a_recovery_stop(&mut sim);
    assert!(
        sim.engine.stop_planning().is_empty(),
        "the disconnect is not refused"
    );
    sim.finish(&stop);
    assert!(accepted(&sim.released.remove(&call).unwrap()));
    assert!(stopping_loop(&mut sim, 30));
    assert_eq!(sim.lab.starts("alpha"), 0);
    let saved = sim.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].intent.desired, DesiredState::Disconnected);
}

/// N-P3-2: a disconnect still waiting when the deadline ends the wait (the
/// stop never came back) is applied before the last save, so the next
/// watchdog carries it out instead of finishing the recovery.
#[test]
fn n_p3_2_a_disconnect_still_waiting_at_the_end_is_stored() {
    let mut sim = Sim::of(&["alpha"], None);
    let (_stop, call) = disconnect_behind_a_recovery_stop(&mut sim);
    assert!(sim.engine.stop_planning().is_empty());
    let Sim { engine, store, .. } = sim;
    let (in_time, answers) = engine.shut_down();
    assert!(!in_time, "the stop never came back");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, call);
    assert!(accepted(&answers[0].1), "{:?}", answers[0].1);
    let rec = &store.saved().unwrap().tunnels[0];
    assert_eq!(rec.intent.desired, DesiredState::Disconnected);
    assert_eq!(rec.resume, Some(ResumeOp::Disconnect));
}

/// N-P3-2: a disconnect that arrives while the loop is stopping is applied
/// too; anything else is refused.
#[test]
fn n_p3_2_only_disconnects_are_applied_while_stopping() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    assert!(sim.engine.stop_planning().is_empty());
    let refused = sim
        .call(
            &user(),
            Request::Reconnect {
                tunnel_id: id("beta"),
            },
        )
        .unwrap();
    assert_eq!(
        refusal_of(&refused),
        Some((ErrorCode::Busy, Some(ErrorReason::ShuttingDown)))
    );
    let answer = sim
        .call(
            &user(),
            Request::Disconnect {
                tunnel_id: id("alpha"),
            },
        )
        .unwrap();
    assert!(accepted(&answer), "{answer:?}");
    assert!(stopping_loop(&mut sim, 30));
    let saved = sim.store.saved().unwrap();
    let alpha = saved
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id("alpha"))
        .unwrap();
    assert_eq!(alpha.intent.desired, DesiredState::Disconnected);
    // Its own stop went out during the shutdown (review r11); nothing else
    // was stopped.
    assert_eq!(sim.lab.stops("alpha"), 1);
    assert_eq!(sim.lab.stops("beta"), 0);
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Disconnected
    );
}

/// Review of wave two, N-P3-4: once winding down, a fault confirmed later
/// starts no recovery and no alert goes out, while requests are answered,
/// monitoring goes on, and a user's own reconnect still runs.
#[test]
fn n_p3_4_winding_down_starts_nothing_automatic_but_answers_requests() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &["alpha"]);
    manage(&mut wd, "alpha");
    let (notifier, _, _) = notifier(&mut wd);
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
    sim.engine.begin_stop();
    sim.engine.begin_stop();
    sim.lab.kill_for_good("alpha");
    let probes = sim.lab.probes("alpha");
    sim.run(600);
    assert_eq!(sim.lab.stops("alpha"), 0, "a recovery was started");
    assert!(sim.lab.probes("alpha") > probes + 30, "monitoring stopped");
    assert_eq!(
        sim.status("alpha").light_reason,
        LightReason::FaultConfirmed
    );
    assert_eq!(sim.transport.sends(), 0);
    sim.ok(Request::Reconnect {
        tunnel_id: id("alpha"),
    });
    sim.run(5);
    assert_eq!((sim.lab.stops("alpha"), sim.lab.starts("alpha")), (1, 1));
}

/// N-P3-4: a recovery whose stop is already out when the loop winds down
/// still gets its start (the core does not take an operation with a stop
/// out for one that sent nothing).
#[test]
fn n_p3_4_winding_down_keeps_a_recovery_whose_stop_is_out() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    sim.engine.begin_stop();
    sim.clock.advance(Duration::from_secs(1)).unwrap();
    let (work, _) = sim.turn_only();
    assert!(work.is_empty());
    sim.finish(&stop);
    sim.run(60);
    assert_eq!(
        sim.lab.starts("alpha"),
        1,
        "WGAL left the tunnel it stopped down"
    );
    assert_eq!(sim.status("alpha").light, Light::Green);
}

/// N-P3-4: an alert that is due when the loop winds down is not sent by
/// it; it stays pending for the next watchdog, which sends it once the
/// fault is seen again.
#[test]
fn n_p3_4_winding_down_sends_no_new_notification() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &["alpha"]);
    manage(&mut wd, "alpha");
    let (notifier, _, _) = notifier(&mut wd);
    lab.kill_for_good("alpha");
    for _ in 0..600 {
        tick_for(&mut wd, 1);
        if !wd.notifications_due().is_empty() {
            break;
        }
    }
    assert!(!wd.notifications_due().is_empty(), "the alert is due");
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
    sim.engine.begin_stop();
    sim.run(120);
    assert_eq!(sim.transport.sends(), 0);
    assert_eq!(sim.engine.stats().jobs_started, 0);
    assert_eq!(
        sim.status("alpha").incident.unwrap().notification,
        NotificationState::Pending
    );
}

/// Review r12: a disconnect waiting behind a recovery's start when the
/// service stops is applied once the start is in, and its own stop goes out
/// during the shutdown: the tunnel ends down, as the user asked.
#[test]
fn a_disconnect_held_behind_a_recovery_start_sends_its_stop_at_shutdown() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    sim.finish(&stop);
    let start = hand_out_until(&mut sim, |i| i.kind == WorkKind::Start);
    let call = sim.next_call_id();
    assert!(
        sim.call(
            &user(),
            Request::Disconnect {
                tunnel_id: id("alpha")
            }
        )
        .is_none()
    );
    assert!(sim.engine.stop_planning().is_empty());
    sim.finish(&start);
    assert!(accepted(&sim.released.remove(&call).unwrap()));
    assert!(stopping_loop(&mut sim, 30));
    assert_eq!(sim.lab.starts("alpha"), 1);
    assert_eq!(sim.lab.stops("alpha"), 2, "the disconnect's stop went out");
    assert_eq!(
        sim.lab.fake().os_state(&binding("alpha")),
        OsState::Disconnected
    );
    let saved = sim.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].intent.desired, DesiredState::Disconnected);
}

/// A disconnect that has to wait for the control slot while another
/// tunnel's recovery finishes at shutdown: the loop waits until its stop is
/// sent too, within the deadline.
#[test]
fn a_disconnect_queued_behind_a_recovery_at_shutdown_still_sends_its_stop() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    // Both phases pending for a while, so there are moments with nothing out
    // and no start owed while alpha still holds the slot.
    sim.lab.slow_stop("alpha", 2);
    sim.lab.slow_start("alpha", 3);
    sim.lab.kill_until_restart("alpha");
    let stop = hand_out_until(&mut sim, is_stop_of("alpha"));
    assert!(sim.engine.stop_planning().is_empty());
    let answer = sim
        .call(
            &user(),
            Request::Disconnect {
                tunnel_id: id("beta"),
            },
        )
        .unwrap();
    assert!(accepted(&answer));
    sim.finish(&stop);
    assert!(stopping_loop(&mut sim, 30), "not finished in time");
    assert_eq!(sim.lab.starts("alpha"), 1, "alpha's start");
    assert_eq!(sim.lab.stops("beta"), 1, "beta's disconnect");
    assert_eq!(
        sim.lab.fake().os_state(&binding("beta")),
        OsState::Disconnected
    );
}
