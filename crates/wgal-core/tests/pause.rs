//! The host-wide "pause monitoring" (plan §8 tray), with the fake clock and
//! fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use support::{Rig, binding, id};
use wgal_core::{MemoryStore, Startup, Timing, Watchdog};
use wgal_protocol::{
    DesiredState, ErrorCode, Light, LightReason, OsState, Outcome, Request, StorageHealth,
};
use wgal_testkit::FakeClock;

fn pause(rig: &mut Rig, paused: bool) {
    rig.ok(Request::SetMonitoringPaused { paused });
}

#[test]
fn paused_monitoring_probes_nothing_and_recovers_nothing() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    pause(&mut rig, true);
    assert!(rig.wd.snapshot().monitoring_paused);
    rig.lab().break_for_good("a");
    let probes = rig.probes("a") + rig.probes("b");
    rig.run(600);
    assert_eq!(
        rig.probes("a") + rig.probes("b"),
        probes,
        "probed while paused"
    );
    assert_eq!(rig.stops("a"), 0, "recovered while paused");
    assert!(rig.wd.notifications_due().is_empty());
    for name in ["a", "b"] {
        let status = rig.status(name);
        assert_eq!(status.light, Light::Gray);
        assert_eq!(status.light_reason, LightReason::MonitoringOff);
        // Each tunnel keeps its own setting for when monitoring resumes.
        assert!(status.intent.monitoring_enabled);
    }
    // Pausing never stops or starts a tunnel.
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    assert_eq!(rig.starts("a") + rig.starts("b"), 0);
}

#[test]
fn resuming_starts_with_a_grace_period_and_confirms_afresh() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run(40); // two or three failures, not yet confirmed
    assert_eq!(rig.stops("a"), 0);
    pause(&mut rig, true);
    rig.run(300);
    pause(&mut rig, false);
    let resumed = rig.now_secs();
    while rig.stops("a") == 0 {
        rig.run(1);
        assert!(
            rig.now_secs() < resumed + 400,
            "never recovered after resuming"
        );
    }
    // Grace (30 s), then 3 failed rounds over at least 60 s.
    assert!(
        rig.now_secs() >= resumed + 30 + 60,
        "recovery started {} s after resuming",
        rig.now_secs() - resumed
    );
}

#[test]
fn user_operations_still_work_while_paused() {
    let mut rig = Rig::new(&["a"]);
    pause(&mut rig, true);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(60);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(rig.status("a").intent.desired, DesiredState::Disconnected);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(60);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

#[test]
fn the_pause_survives_a_restart() {
    let mut rig = Rig::new(&["a"]);
    pause(&mut rig, true);
    let saved = rig.store.saved().unwrap();
    assert!(saved.monitoring_paused);

    let mut wd = Watchdog::new(
        support::Lab::new(&["a"]),
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..120 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
    }
    let snapshot = wd.snapshot();
    assert!(snapshot.monitoring_paused);
    assert_eq!(snapshot.tunnels[0].light_reason, LightReason::MonitoringOff);
    let probes = wd
        .adapter()
        .fake
        .calls()
        .iter()
        .filter(|c| matches!(c, wgal_testkit::Call::Probe(..)))
        .count();
    assert_eq!(probes, 0);
}

#[test]
fn a_pause_that_cannot_be_stored_is_refused_and_changes_nothing() {
    let mut rig = Rig::new(&["a"]);
    rig.store.set_failing(true);
    let outcome = rig.request(Request::SetMonitoringPaused { paused: true });
    assert!(
        matches!(&outcome, Outcome::Err(e) if e.code == ErrorCode::StorageUnavailable),
        "{outcome:?}"
    );
    let snapshot = rig.wd.snapshot();
    assert!(!snapshot.monitoring_paused);
    assert_eq!(snapshot.storage, StorageHealth::Failed);
    assert_ne!(rig.status("a").light_reason, LightReason::MonitoringOff);
}

#[test]
fn auto_recovery_allowed_follows_the_baseline() {
    let mut rig = Rig::new(&["a"]);
    // The rig's recovery target passed its baseline during start-up.
    assert!(rig.status("a").auto_recovery_allowed);
    assert!(rig.status("a").last_verified_unix_ms.is_some());
    // A new target has to pass its own baseline first.
    rig.set_targets("a", Some(support::icmp(9)), &[]);
    assert!(!rig.status("a").auto_recovery_allowed);
    assert!(rig.status("a").last_verified_unix_ms.is_none());
}

/// F/G review P3-1: an alert that was waiting (no token yet, a 429) is not
/// due while monitoring is paused, nor right after resuming before the fault
/// is seen again.
#[test]
fn a_waiting_alert_needs_fresh_evidence_after_a_pause() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.wd.notifications_due().is_empty() {
        rig.run(1);
        assert!(rig.now_secs() < 600, "no alert became due");
    }
    pause(&mut rig, true);
    assert!(rig.wd.notifications_due().is_empty(), "due while paused");
    rig.run(60);
    pause(&mut rig, false);
    assert!(
        rig.wd.notifications_due().is_empty(),
        "due right after resuming"
    );
    rig.run(300);
    assert!(
        !rig.wd.notifications_due().is_empty(),
        "never due again once the fault was seen again"
    );
}

/// F/G review P3-2: pausing does not throw away a stop already handed to the
/// platform, so it is not sent a second time.
#[test]
fn a_pause_keeps_the_result_of_a_stop_already_sent() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    let wd = &mut rig.wd;
    let items = wd.plan();
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert!(wd.still_due(item));
    let result = wgal_core::execute(wd.adapter(), wd.clock(), item);
    wd.handle(&Request::SetMonitoringPaused { paused: true });
    wd.apply(result);
    rig.run(60);
    assert_eq!(rig.stops("a"), 1, "the stop was sent again");
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
}

/// Drives a managed tunnel to a failed first round that leaves it down:
/// it goes down again while its recovery is verifying.
fn left_down_after_a_failed_round(rig: &mut Rig) {
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != wgal_protocol::RecoveryPhase::Verifying {
        rig.run(1);
        assert!(rig.now_secs() < 600, "recovery never reached verification");
    }
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    while rig.status("a").recovery != wgal_protocol::RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the round never failed");
    }
}

/// Re-review of d5ced49, N1: a pause and resume do not leave a tunnel that
/// WGAL's recovery left down without a retry, and its alert still comes due
/// when the retry fails.
#[test]
fn a_tunnel_left_down_is_retried_after_a_pause() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let starts = rig.starts("a");
    pause(&mut rig, true);
    rig.run(30);
    assert_eq!(rig.starts("a"), starts, "started while paused");
    pause(&mut rig, false);
    let mut due = false;
    for _ in 0..900 {
        rig.run(1);
        due |= !rig.wd.notifications_due().is_empty();
    }
    assert!(rig.starts("a") > starts, "never retried after resuming");
    assert!(due, "the alert never came due");
}

/// Plans one pass at a time until the retry's start is planned, and holds
/// it there, before it is sent: the moment a pause can still withdraw it.
fn hold_the_retrys_start(rig: &mut Rig) -> wgal_core::WorkItem {
    let wd = &mut rig.wd;
    for _ in 0..900 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        // The other tunnels' work in the same batch still runs.
        let mut held = None;
        for item in wd.plan() {
            if item.is_control() && held.is_none() {
                held = Some(item);
                continue;
            }
            if wd.still_due(&item) {
                let result = wgal_core::execute(wd.adapter(), wd.clock(), &item);
                wd.apply(result);
            }
        }
        if let Some(item) = held {
            return item;
        }
    }
    panic!("the retry was never planned");
}

/// Re-review of 4b843b6, N1a: a retry already queued when monitoring is
/// paused still runs after resuming. Since Codex P2-N17 a retry takes a free
/// control slot in the pass it is due, so it is held between being planned
/// and being sent.
#[test]
fn a_queued_retry_of_a_tunnel_left_down_survives_a_pause() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let starts = rig.starts("a");
    let item = hold_the_retrys_start(&mut rig);
    pause(&mut rig, true);
    assert!(!rig.wd.still_due(&item), "a withdrawn retry was still sent");
    rig.run(60);
    assert_eq!(rig.starts("a"), starts, "started while paused");
    pause(&mut rig, false);
    rig.run(300);
    assert!(rig.starts("a") > starts, "the queued retry was lost");
}

/// Codex P2-N18: a retry withdrawn by a pause before it sent anything uses
/// up nothing. With a limit of 1, the one retry still runs after resuming,
/// and only once it has failed too is the limit used up.
#[test]
fn a_retry_withdrawn_before_it_ran_uses_up_nothing() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(1), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let starts = rig.starts("a");
    let item = hold_the_retrys_start(&mut rig);
    assert_eq!(rig.status("a").incident.unwrap().retries_used, 1);
    pause(&mut rig, true);
    assert!(!rig.wd.still_due(&item));
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(
        incident.retries_used, 0,
        "the withdrawn retry is given back"
    );
    assert!(!incident.retries_exhausted);
    let stored = rig.store.saved().unwrap().tunnels[0]
        .incident
        .clone()
        .unwrap();
    assert_eq!(
        stored.backoff_step, stored.counted_from_step,
        "and stored so"
    );
    rig.events();
    pause(&mut rig, false);
    rig.run(300);
    assert_eq!(rig.starts("a"), starts + 1, "the one retry ran");
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(incident.retries_used, 1);
    assert!(incident.retries_exhausted, "used up once it ran and failed");
    let events = rig.events_of("a");
    assert!(events.contains(&wgal_protocol::LogEvent::RetriesExhausted { retries: 1 }));
}

/// Re-review of 4b843b6: a state that cannot be read when the retry is due
/// delays it; it does not end the retries.
#[test]
fn an_unreadable_state_at_the_retry_does_not_end_the_retries() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let starts = rig.starts("a");
    for _ in 0..8 {
        rig.lab().fake.fail_next(
            &binding("a"),
            wgal_testkit::Operation::QueryState,
            wgal_core::AdapterError::new(wgal_protocol::AdapterErrorKind::Timeout, "slow"),
        );
    }
    rig.run(900);
    assert!(rig.starts("a") > starts, "the retries ended for good");
}

/// Re-review of 50eb014, N3: a tunnel still transitioning when its retry is
/// due (a start that keeps the unit activating) is retried once it settles
/// down, not left down for good.
#[test]
fn a_transitioning_state_at_the_retry_does_not_end_the_retries() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let (starts, stops) = (rig.starts("a"), rig.stops("a"));
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.run(90);
    // The retry came due meanwhile; nothing touched the changing tunnel
    // (review of `3d77031`, P3-1).
    assert_eq!(rig.starts("a"), starts, "started while transitioning");
    assert_eq!(rig.stops("a"), stops, "stopped while transitioning");
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(900);
    assert!(rig.starts("a") > starts, "the retries ended for good");
}

/// Codex P2-N18: a retry the previous process counted and stored, and died
/// before sending, is started after the restart from its `resume` record
/// without being counted a second time.
#[test]
fn a_counted_retry_resumed_after_a_restart_is_not_counted_twice() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let _held = hold_the_retrys_start(&mut rig);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(wgal_core::ResumeOp::Recovery));
    assert_eq!(rig.status("a").incident.unwrap().retries_used, 1);

    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..300 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
    }
    let starts = wd
        .adapter()
        .fake
        .calls_for(&binding("a"))
        .iter()
        .filter(|c| matches!(c, wgal_testkit::Call::Start(_)))
        .count();
    assert!(starts >= 1, "the stored round was not resumed");
    let incident = wd.snapshot().tunnels[0].incident.clone().unwrap();
    assert_eq!(
        incident.retries_used,
        u32::try_from(starts).unwrap(),
        "each round counted once: the resumed one was counted before the restart"
    );
}

/// Holds the retry's start, then the runtime shuts down: the unsent retry
/// is withdrawn, its count given back, and that is stored. Returns the
/// stored state.
fn given_back_and_stored(rig: &mut Rig) -> wgal_core::PersistedState {
    let _held = hold_the_retrys_start(rig);
    rig.wd.hold_automatic_actions();
    rig.wd.plan();
    let saved = rig.store.saved().unwrap();
    let tunnel = &saved.tunnels[0];
    assert_eq!(tunnel.resume, Some(wgal_core::ResumeOp::Recovery));
    let incident = tunnel.incident.as_ref().unwrap();
    assert_eq!(
        incident.backoff_step, incident.counted_from_step,
        "given back"
    );
    assert!(incident.resume_uncounted, "and stored as not counted");
    saved
}

fn restarted_down(saved: wgal_core::PersistedState) -> support::Wd {
    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..300 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
    }
    wd
}

fn starts_of(wd: &support::Wd) -> usize {
    wd.adapter()
        .fake
        .calls_for(&binding("a"))
        .iter()
        .filter(|c| matches!(c, wgal_testkit::Call::Start(_)))
        .count()
}

/// Codex P2-N29: a counted retry given back before it was sent, and stored
/// so, is counted when the next process starts it from its `resume`
/// record: a restart never gives a retry away.
#[test]
fn a_retry_given_back_before_a_restart_is_counted_when_it_runs() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let saved = given_back_and_stored(&mut rig);
    let mut wd = restarted_down(saved);
    let starts = starts_of(&wd);
    assert!(starts >= 1, "the stored round was not resumed");
    let incident = wd.snapshot().tunnels[0].incident.clone().unwrap();
    assert_eq!(
        incident.retries_used,
        u32::try_from(starts).unwrap(),
        "every round that ran is counted"
    );
}

/// Codex P2-N29: the same with a limit lowered to 0 before the restart: the
/// resumed round is not run, and the limit is used up.
#[test]
fn a_retry_given_back_does_not_run_past_a_lowered_limit() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let mut saved = given_back_and_stored(&mut rig);
    saved.tunnels[0].retry = wgal_protocol::RecoveryRetry::new(Some(0), 60).unwrap();
    let mut wd = restarted_down(saved);
    assert_eq!(starts_of(&wd), 0, "run past the limit");
    let status = wd.snapshot().tunnels[0].clone();
    assert!(status.incident.unwrap().retries_exhausted);
}

fn retries_used_of(wd: &mut support::Wd, name: &str) -> u32 {
    wd.snapshot()
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id(name))
        .and_then(|t| t.incident.as_ref())
        .map_or(0, |i| i.retries_used)
}

/// Codex P2-N46: a first recovery round planned but not sent yet is
/// withdrawn when the host wakes before its stop goes out: the fault it was
/// queued on is no evidence now. Nothing is stopped; the fault is confirmed
/// afresh, and only then recovered.
#[test]
fn a_planned_round_is_withdrawn_when_the_host_wakes_before_it_is_sent() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let item = hold_the_retrys_start(&mut rig);
    rig.wake();
    assert!(
        !rig.wd.still_due(&item),
        "stopped on evidence from before the wake"
    );
    assert_eq!(rig.stops("a"), 0);
    rig.run(300);
    assert!(rig.stops("a") >= 1, "never recovered once confirmed afresh");
}

/// Codex P2-N46: the same for a retry, whose count is given back.
#[test]
fn a_planned_retry_is_withdrawn_and_given_back_when_the_host_wakes() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != wgal_protocol::RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the first round never failed");
    }
    let before = retries_used_of(&mut rig.wd, "a");
    let stops = rig.stops("a");
    let item = hold_the_retrys_start(&mut rig);
    assert_eq!(retries_used_of(&mut rig.wd, "a"), before + 1);
    rig.wake();
    assert!(
        !rig.wd.still_due(&item),
        "retried on evidence from before the wake"
    );
    assert_eq!(rig.stops("a"), stops);
    assert_eq!(
        retries_used_of(&mut rig.wd, "a"),
        before,
        "the count is given back"
    );
}

/// Codex P2-N42: a first round that waits in a backoff without having run
/// (restored so here) is still the incident's first round when it runs: it
/// uses up no retry.
#[test]
fn a_first_round_waiting_in_backoff_is_not_counted_as_a_retry() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(1), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let mut saved = rig.store.saved().unwrap();
    let tunnel = &mut saved.tunnels[0];
    tunnel.resume = None;
    let incident = tunnel.incident.as_mut().unwrap();
    incident.first_recovery = wgal_protocol::FirstRecovery::InProgress;
    incident.backoff_step = 0;
    incident.counted_from_step = 0;
    incident.last_round_end_unix_ms = None;
    incident.wait_from_unix_ms = incident.last_round_end_unix_ms;
    incident.next_retry_unix_ms = Some(support::T0 + 600_000);

    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..300 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
        if starts_of(&wd) > 0 {
            break;
        }
    }
    assert_eq!(starts_of(&wd), 1, "the first round never ran");
    assert_eq!(
        retries_used_of(&mut wd, "a"),
        0,
        "the first round was counted as a retry"
    );
}

/// A stored state whose first round ended inconclusive, the tunnel left
/// down and the next round due at the restart, under `limit` retries.
fn inconclusive_first_round_stored(limit: u16) -> wgal_core::PersistedState {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let mut saved = rig.store.saved().unwrap();
    let tunnel = &mut saved.tunnels[0];
    tunnel.retry = wgal_protocol::RecoveryRetry::new(Some(limit), 60).unwrap();
    tunnel.resume = None;
    let incident = tunnel.incident.as_mut().unwrap();
    incident.first_recovery = wgal_protocol::FirstRecovery::InProgress;
    incident.backoff_step = 0;
    incident.counted_from_step = 0;
    incident.last_round_end_unix_ms = Some(support::T0 + 500_000);
    incident.wait_from_unix_ms = incident.last_round_end_unix_ms;
    incident.next_retry_unix_ms = Some(support::T0 + 600_000);
    saved
}

fn restarted_until_started(saved: wgal_core::PersistedState, secs: u64) -> support::Wd {
    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..secs {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
        if starts_of(&wd) > 0 {
            break;
        }
    }
    wd
}

/// Codex P2-N47: after a restart, the round that follows a first round
/// which ended inconclusive is a retry, counted as one: the end of the
/// incident's last round is restored with it.
#[test]
fn the_round_after_an_ended_first_round_is_a_retry_after_a_restart() {
    let mut wd = restarted_until_started(inconclusive_first_round_stored(5), 300);
    assert_eq!(starts_of(&wd), 1, "the retry never ran");
    assert_eq!(retries_used_of(&mut wd, "a"), 1, "not counted as a retry");
}

/// Codex P2-N47: and a limit of 0 stops it, as it would have before the
/// restart.
#[test]
fn a_limit_of_zero_stops_the_round_after_an_ended_first_round_after_a_restart() {
    let mut wd = restarted_until_started(inconclusive_first_round_stored(0), 300);
    assert_eq!(starts_of(&wd), 0, "run past a limit of 0");
    let incident = wd.snapshot().tunnels[0].incident.clone().unwrap();
    assert!(incident.retries_exhausted);
}

/// Codex P2-N48: a round withdrawn before it sent anything leaves nothing to
/// resume: when the user then stops the tunnel in the official app, WGAL
/// does not start it again.
#[test]
fn a_withdrawn_round_does_not_start_a_tunnel_the_user_stops() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let item = hold_the_retrys_start(&mut rig);
    rig.wake();
    assert!(!rig.wd.still_due(&item));
    assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(180);
    assert_eq!(rig.starts("a"), 0, "started a tunnel the user stopped");
}

/// Codex P2-N49: a retry counted before a restart and resumed after it is
/// given back when its verification meets the host offline, as one run in
/// a single process is.
#[test]
fn a_resumed_retry_that_meets_the_host_offline_is_given_back() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let _held = hold_the_retrys_start(&mut rig);
    let saved = rig.store.saved().unwrap();
    let stored = saved.tunnels[0].incident.clone().unwrap();
    assert_eq!(
        stored.backoff_step - stored.counted_from_step,
        1,
        "counted and stored"
    );
    assert!(!stored.resume_uncounted);

    let lab = support::Lab::new(&["a"]);
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    lab.fail_probes(
        "a",
        Some(wgal_protocol::AdapterErrorKind::LocalNetworkOffline),
    );
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..300 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
    }
    assert_eq!(starts_of(&wd), 1, "the stored retry was not resumed");
    assert_eq!(
        retries_used_of(&mut wd, "a"),
        0,
        "a round that met the host offline is counted"
    );
}

/// A stored incident whose first round ended inconclusive `ago` before the
/// restart, with the tunnel seen connected then, so nothing waits for a
/// retry; retries every 3 hours.
fn idle_after_an_inconclusive_first_round(ago: std::time::Duration) -> wgal_core::PersistedState {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(Some(5), 60).unwrap(),
    });
    left_down_after_a_failed_round(&mut rig);
    let mut saved = rig.store.saved().unwrap();
    let tunnel = &mut saved.tunnels[0];
    tunnel.retry = wgal_protocol::RecoveryRetry::new(Some(5), 10_800).unwrap();
    tunnel.resume = None;
    let incident = tunnel.incident.as_mut().unwrap();
    incident.first_recovery = wgal_protocol::FirstRecovery::InProgress;
    incident.notification = wgal_protocol::NotificationState::NotEligible;
    incident.backoff_step = 0;
    incident.counted_from_step = 0;
    let ago = i64::try_from(ago.as_millis()).unwrap();
    incident.last_round_end_unix_ms = Some(support::T0 + 600_000 - ago);
    incident.wait_from_unix_ms = incident.last_round_end_unix_ms;
    incident.next_retry_unix_ms = None;
    saved
}

/// Restarts with the tunnel connected but carrying no traffic, and runs
/// until its first stop or `secs`.
fn restarted_connected_dead(saved: wgal_core::PersistedState, secs: u64) -> support::Wd {
    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for _ in 0..secs {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
        let stopped = wd
            .adapter()
            .fake
            .calls_for(&binding("a"))
            .iter()
            .any(|c| matches!(c, wgal_testkit::Call::Stop(_)));
        if stopped {
            break;
        }
    }
    wd
}

fn stops_of(wd: &support::Wd) -> usize {
    wd.adapter()
        .fake
        .calls_for(&binding("a"))
        .iter()
        .filter(|c| matches!(c, wgal_testkit::Call::Stop(_)))
        .count()
}

/// Codex P2-N50: after a restart, an incident not waiting for a retry waits
/// only what is left of the interval by the wall clock: its first round
/// ended four hours ago, the interval is three hours, so the retry follows
/// the fault confirmed again, counted as a retry. Codex P2-N52: that first
/// round is not taken for failed meanwhile, so no alert comes due.
#[test]
fn after_a_restart_a_served_interval_is_not_waited_again() {
    let saved = idle_after_an_inconclusive_first_round(std::time::Duration::from_secs(4 * 3600));
    let mut wd = restarted_connected_dead(saved, 600);
    assert_eq!(
        stops_of(&wd),
        1,
        "waited the interval again after the restart"
    );
    let incident = wd.snapshot().tunnels[0].incident.clone().unwrap();
    assert_eq!(incident.retries_used, 1, "not counted as a retry");
    assert_eq!(
        incident.first_recovery,
        wgal_protocol::FirstRecovery::InProgress,
        "the first round was taken for failed"
    );
    assert_eq!(
        incident.notification,
        wgal_protocol::NotificationState::NotEligible
    );
}

/// Codex P2-N50: and one that ended an hour ago still waits the two hours
/// left.
#[test]
fn after_a_restart_the_rest_of_the_interval_is_still_waited() {
    let saved = idle_after_an_inconclusive_first_round(std::time::Duration::from_secs(3600));
    let wd = restarted_connected_dead(saved, 600);
    assert_eq!(stops_of(&wd), 0, "retried before the interval was over");
}

fn retry_of(limit: u16, interval: u16) -> wgal_protocol::RecoveryRetry {
    wgal_protocol::RecoveryRetry::new(Some(limit), interval).unwrap()
}

/// A stored incident whose first round failed an hour ago, retried every
/// three hours, so the next retry is due in two.
fn waiting_for_a_retry() -> wgal_core::PersistedState {
    let mut saved = idle_after_an_inconclusive_first_round(std::time::Duration::from_secs(3600));
    let incident = saved.tunnels[0].incident.as_mut().unwrap();
    incident.first_recovery = wgal_protocol::FirstRecovery::Failed;
    let end = incident.last_round_end_unix_ms.unwrap();
    incident.next_retry_unix_ms = Some(end + 3 * 3_600_000);
    saved
}

fn restarted(saved: wgal_core::PersistedState, down: bool) -> support::Wd {
    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    if down {
        lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    }
    Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    )
}

fn run_wd(wd: &mut support::Wd, secs: u64) {
    for _ in 0..secs {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        wd.tick();
    }
}

fn accept(wd: &mut support::Wd, request: Request) {
    assert!(
        matches!(wd.handle(&request), wgal_protocol::Outcome::Ok(_)),
        "{request:?} was refused"
    );
}

/// Codex P2-N53: after a restart, a shorter interval is measured from the
/// end of the last round as stored, not from the wait fixed at the restart:
/// the round ended an hour ago, so a 60 s interval is long over.
#[test]
fn after_a_restart_a_shorter_interval_applies_at_once() {
    let saved = idle_after_an_inconclusive_first_round(std::time::Duration::from_secs(3600));
    let mut saved = saved;
    saved.tunnels[0].retry = retry_of(5, 10_800);
    let mut wd = restarted(saved, false);
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 60),
        },
    );
    run_wd(&mut wd, 600);
    assert!(
        stops_of(&wd) >= 1,
        "still waited the interval fixed at the restart"
    );
}

/// Codex P2-N53: a retry waiting in a backoff restored after a restart
/// keeps its time when the same settings are saved again, and a shorter
/// interval is measured from the end of the last round.
#[test]
fn a_restored_backoff_keeps_its_time_and_follows_a_new_interval() {
    let mut wd = restarted(waiting_for_a_retry(), true);
    run_wd(&mut wd, 5);
    let due = wd.snapshot().tunnels[0].next_retry_unix_ms;
    assert!(due.is_some());
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 10_800),
        },
    );
    assert_eq!(
        wd.snapshot().tunnels[0].next_retry_unix_ms,
        due,
        "the same settings moved the retry"
    );
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 60),
        },
    );
    run_wd(&mut wd, 120);
    assert!(
        starts_of(&wd) >= 1,
        "the shorter interval was measured from now"
    );
}

/// Codex P2-N54: a backoff restored after a restart and then dropped by a
/// pause of monitoring still holds the next round back for what is left of
/// the interval, when the fault is confirmed again.
#[test]
fn a_restored_wait_holds_after_its_backoff_is_dropped() {
    let mut wd = restarted(waiting_for_a_retry(), false);
    run_wd(&mut wd, 5);
    accept(&mut wd, Request::SetMonitoringPaused { paused: true });
    run_wd(&mut wd, 10);
    accept(&mut wd, Request::SetMonitoringPaused { paused: false });
    run_wd(&mut wd, 600);
    assert_eq!(stops_of(&wd), 0, "retried two hours early");
}

/// Codex P2-N56: a manual reconnect that fails puts the tunnel back on the
/// incident's backoff from that failure; saving the same settings again
/// measures the wait from there too, not from the end of the automatic
/// round before it, so the retry keeps its time.
#[test]
fn a_failed_reconnect_starts_the_wait_that_the_same_settings_keep() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: retry_of(5, 600),
    });
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != wgal_protocol::RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the first round never failed");
    }
    rig.run(120);
    let stops = rig.stops("a");
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    while rig.stops("a") == stops
        || rig.status("a").recovery != wgal_protocol::RecoveryPhase::Backoff
    {
        rig.run(1);
        assert!(rig.now_secs() < 1500, "the reconnect never failed");
    }
    let stored = |rig: &Rig| {
        rig.store.saved().unwrap().tunnels[0]
            .incident
            .as_ref()
            .unwrap()
            .next_retry_unix_ms
    };
    let due = stored(&rig);
    assert!(due.is_some());
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: retry_of(5, 600),
    });
    assert_eq!(
        stored(&rig),
        due,
        "measured from the automatic round before the reconnect"
    );
}

/// Codex P2-N56, P2-N58: after a restart, the wait is measured from where
/// it started as stored, a reconnect that failed ten minutes ago after the
/// round an hour ago, not from the end of that round: the retry is due in
/// 170 minutes, and saving the same settings again keeps it.
#[test]
fn a_restored_wait_follows_the_stored_retry_time() {
    let mut saved = waiting_for_a_retry();
    let incident = saved.tunnels[0].incident.as_mut().unwrap();
    incident.wait_from_unix_ms = Some(support::T0 + 600_000 - 10 * 60_000);
    incident.next_retry_unix_ms = Some(support::T0 + 600_000 + 170 * 60_000);
    let mut wd = restarted(saved, true);
    run_wd(&mut wd, 5);
    let due = wd.snapshot().tunnels[0].next_retry_unix_ms;
    let expected = support::T0 + 600_000 + 170 * 60_000;
    assert!(
        due.is_some_and(|due| (due - expected).abs() <= 1_000),
        "{due:?} is not 170 minutes after the restart"
    );
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 10_800),
        },
    );
    assert_eq!(
        wd.snapshot().tunnels[0].next_retry_unix_ms,
        due,
        "measured from the end of the round, not the stored retry time"
    );
}

const MINUTE: i64 = 60_000;
const RESTART: i64 = support::T0 + 600_000;

/// A stored incident whose wait started `ago` minutes before the restart,
/// under `interval` minutes, with the retry stored at `due` minutes from the
/// restart (stale or none) and the round's end `end_ago` minutes before it.
fn stored_wait(
    ago: i64,
    end_ago: i64,
    interval: u16,
    due: Option<i64>,
) -> wgal_core::PersistedState {
    let mut saved = waiting_for_a_retry();
    let tunnel = &mut saved.tunnels[0];
    tunnel.retry = retry_of(5, interval * 60);
    let incident = tunnel.incident.as_mut().unwrap();
    incident.wait_from_unix_ms = Some(RESTART - ago * MINUTE);
    incident.last_round_end_unix_ms = Some(RESTART - end_ago * MINUTE);
    incident.next_retry_unix_ms = due.map(|due| RESTART + due * MINUTE);
    saved
}

fn shown_retry(wd: &mut support::Wd) -> Option<i64> {
    wd.snapshot().tunnels[0].next_retry_unix_ms
}

fn near(actual: Option<i64>, expected: i64) -> bool {
    actual.is_some_and(|actual| (actual - expected).abs() <= 2_000)
}

/// Codex P2-N57: an interval shortened while the retry was not scheduled (a
/// pause) is measured from where the wait started after a restart, not
/// from the stale retry time: started 61 minutes ago, now every 120, so it
/// is due in 59, not in 119.
#[test]
fn a_restart_measures_a_changed_interval_from_the_start_of_the_wait() {
    let mut wd = restarted(stored_wait(61, 61, 120, Some(119)), true);
    run_wd(&mut wd, 5);
    let due = shown_retry(&mut wd);
    assert!(near(due, RESTART + 59 * MINUTE), "{due:?}");
}

/// Codex P2-N57: time already waited past the interval still counts when
/// the interval is lengthened after a restart: started two hours ago under
/// one hour, now three, so it is due in one hour, not two.
#[test]
fn time_waited_before_a_restart_counts_toward_a_longer_interval() {
    let lab = support::Lab::new(&["a"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let store = MemoryStore::new();
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(RESTART),
        Timing::default(),
        Box::new(store.clone()),
        Startup::Restored(stored_wait(120, 120, 60, Some(-60))),
    );
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 180 * 60),
        },
    );
    let stored = store.saved().unwrap().tunnels[0]
        .incident
        .as_ref()
        .unwrap()
        .next_retry_unix_ms;
    assert!(near(stored, RESTART + 60 * MINUTE), "{stored:?}");
}

/// Codex P2-N58: a reconnect that failed 140 minutes before the restart
/// started the wait, and a brief recovery since cleared the retry time:
/// after the restart the wait is still measured from that failure, due in
/// 40 minutes, not from the round 200 minutes ago, which would be overdue.
#[test]
fn a_reconnects_wait_survives_a_brief_recovery_and_a_restart() {
    let mut wd = restarted(stored_wait(140, 200, 180, None), false);
    run_wd(&mut wd, 600);
    assert_eq!(stops_of(&wd), 0, "retried 40 minutes early");
}

/// Codex P2-N59: a first round started again after a restart and withdrawn
/// by a pause before it sent anything is due at once; saving the same
/// settings again does not push it out, and it runs once monitoring
/// resumes.
#[test]
fn a_withdrawn_first_round_keeps_its_time_over_the_same_settings() {
    let mut saved = waiting_for_a_retry();
    let tunnel = &mut saved.tunnels[0];
    tunnel.retry = retry_of(5, 600);
    tunnel.resume = Some(wgal_core::ResumeOp::Recovery);
    let incident = tunnel.incident.as_mut().unwrap();
    incident.first_recovery = wgal_protocol::FirstRecovery::InProgress;
    incident.wait_from_unix_ms = None;
    incident.last_round_end_unix_ms = None;
    incident.next_retry_unix_ms = None;
    let mut wd = restarted(saved, true);
    let mut held = None;
    'plan: for _ in 0..300 {
        wd.clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        for item in wd.plan() {
            if item.is_control() {
                held = Some(item);
                break 'plan;
            }
            if wd.still_due(&item) {
                let result = wgal_core::execute(wd.adapter(), wd.clock(), &item);
                wd.apply(result);
            }
        }
    }
    let item = held.expect("the first round was never started again");
    accept(&mut wd, Request::SetMonitoringPaused { paused: true });
    assert!(!wd.still_due(&item));
    accept(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 600),
        },
    );
    accept(&mut wd, Request::SetMonitoringPaused { paused: false });
    run_wd(&mut wd, 60);
    assert!(
        starts_of(&wd) >= 1,
        "the withdrawn first round was pushed out"
    );
}

/// Codex P2-N60: a wall clock set back or forward an hour while the retry
/// waits does not move the wait apart from the retry time: after 90 of 120
/// minutes, the interval lengthened to 180 and a restart, the retry is due
/// in 90 minutes either way.
#[test]
fn a_wall_clock_set_while_waiting_does_not_move_the_wait() {
    for shift in [-60, 60] {
        let mut rig = Rig::new(&["a"]);
        rig.ok(Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 120 * 60),
        });
        left_down_after_a_failed_round(&mut rig);
        rig.run(90 * 60);
        rig.wd.clock().shift_wall(shift * MINUTE).unwrap();
        rig.ok(Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: retry_of(5, 180 * 60),
        });
        let saved = rig.store.saved().unwrap();
        let wall = wgal_core::Clock::wall_unix_ms(rig.wd.clock());
        let lab = support::Lab::new(&["a"]);
        lab.break_for_good("a");
        lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
        let mut wd = Watchdog::new(
            lab,
            FakeClock::new(wall),
            Timing::default(),
            Box::new(MemoryStore::new()),
            Startup::Restored(saved),
        );
        run_wd(&mut wd, 5);
        let due = shown_retry(&mut wd);
        assert!(
            due.is_some_and(|due| (due - (wall + 90 * MINUTE)).abs() <= 60_000),
            "set by {shift} minutes: due {due:?}, 90 minutes from {wall}"
        );
    }
}

/// Codex P2-N61: a state from 1.1.0 has no start of its wait; a stored
/// retry time still says what is left: a reconnect failed after the round
/// ended, so 50 s are left, not none.
#[test]
fn a_state_without_the_start_of_its_wait_keeps_its_retry_time() {
    let mut saved = waiting_for_a_retry();
    let tunnel = &mut saved.tunnels[0];
    tunnel.retry = retry_of(5, 60);
    let incident = tunnel.incident.as_mut().unwrap();
    incident.wait_from_unix_ms = None;
    incident.last_round_end_unix_ms = Some(RESTART - 130_000);
    incident.next_retry_unix_ms = Some(RESTART + 50_000);
    let mut wd = restarted(saved, true);
    run_wd(&mut wd, 5);
    let due = shown_retry(&mut wd);
    assert!(near(due, RESTART + 50_000), "{due:?}");
}
