//! Regression tests for the follow-up review of `bd57f48..49a8bfb`. Each
//! test is named
//! after the finding it covers and drives the watchdog with the fake clock
//! and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Lab, Rig, binding, id};
use wgal_core::{
    AdapterError, MemoryStore, PersistedState, ResumeOp, Startup, Timing, Watchdog, WorkItem,
    execute,
};
use wgal_protocol::{
    AdapterErrorKind, DataHealth, FirstRecovery, Light, LightReason, NotificationState, OsState,
    Outcome, RecoveryPhase, Request, StorageHealth, Takeover, TunnelStatus,
};
use wgal_testkit::{Call, FakeClock, Operation};

type Wd = Watchdog<Lab, FakeClock>;

/// One second, then a single plan/execute/apply pass.
fn step(wd: &mut Wd) {
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    for item in wd.plan() {
        let result = execute(wd.adapter(), wd.clock(), &item);
        wd.apply(result);
    }
}

fn run(wd: &mut Wd, secs: u64) {
    for _ in 0..secs {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
}

/// Advances one second at a time until something is planned.
fn next_items(wd: &mut Wd) -> Vec<WorkItem> {
    for _ in 0..120 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        let items = wd.plan();
        if !items.is_empty() {
            return items;
        }
    }
    panic!("nothing was planned for two minutes");
}

fn restart(lab: Lab, startup: Startup) -> Wd {
    Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        startup,
    )
}

fn calls(wd: &Wd, name: &str, pick: fn(&Call) -> bool) -> usize {
    wd.adapter()
        .fake
        .calls_for(&binding(name))
        .iter()
        .filter(|c| pick(c))
        .count()
}

fn is_stop(call: &Call) -> bool {
    matches!(call, Call::Stop(_))
}

fn is_start(call: &Call) -> bool {
    matches!(call, Call::Start(_))
}

fn first(wd: &mut Wd) -> TunnelStatus {
    wd.snapshot().tunnels[0].clone()
}

fn not_sent(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::Timeout, detail)
}

/// F2: the user's disconnect is never abandoned because a stop failed. The
/// error is shown, and the stop is retried until the tunnel is down.
#[test]
fn f2_disconnect_is_retried_until_the_tunnel_is_down() {
    let mut rig = Rig::new(&["a"]);
    for _ in 0..2 {
        rig.lab().fake.fail_next(
            &binding("a"),
            Operation::Stop,
            not_sent("the stop was not sent; ask again"),
        );
    }
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(2);
    let a = rig.status("a");
    assert_eq!(a.light, Light::Red);
    assert_eq!(a.light_reason, LightReason::ControlFailed);
    assert!(a.fault.is_some(), "the control error is shown meanwhile");

    rig.run(120);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(rig.stops("a"), 3, "two refused stops, then one that worked");
    assert_eq!(rig.starts("a"), 0, "a disconnect never starts the tunnel");
    let a = rig.status("a");
    assert_eq!(a.light_reason, LightReason::DisconnectedByUser);
    assert_ne!(a.takeover, Takeover::ExternallyControlled);
    assert!(a.fault.is_none());
    assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
}

/// F2: a stop refused while an earlier start is still pending is retried
/// once the start has settled; the tunnel that start brought up is stopped,
/// not mistaken for one started by something else.
#[test]
fn f2_stop_refused_while_a_start_is_pending_is_retried_once_it_settles() {
    let mut rig = Rig::new(&["a"]);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Stop,
        AdapterError::new(
            AdapterErrorKind::ManagerConflict,
            "the service is still starting",
        ),
    );
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(3);
    assert_eq!(rig.stops("a"), 1);

    // The earlier start finishes and the service is running.
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.run(90);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(rig.starts("a"), 0);
    assert_ne!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// F4: applying an earlier item of the same batch failed to save, so the
/// automatic stop planned later in that batch must not run.
#[test]
fn f4_a_save_failing_mid_batch_withdraws_its_later_automatic_stop() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("b");
    rig.run_until(44);
    rig.lab().break_for_good("b");
    let mut queued = false;
    for _ in 0..200 {
        step(&mut rig.wd);
        if rig.status("b").recovery == RecoveryPhase::Queued {
            queued = true;
            break;
        }
    }
    assert!(queued, "B's recovery was queued");
    assert_eq!(rig.stops("b"), 0);

    // The next pass plans A's round (due now) before B's stop. A's state
    // changed, so applying A's round saves, and that save fails.
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.store.set_failing(true);
    rig.wd.tick();
    assert_eq!(rig.wd.snapshot().storage, StorageHealth::Failed);
    assert_eq!(rig.stops("b"), 0, "no automatic stop after a failed save");

    rig.run(120);
    assert_eq!(rig.stops("b"), 0);
    assert_eq!(rig.starts("b"), 0);
}

/// F9: the watchdog died after its recovery's stop was accepted but before
/// it took effect. The new process first sees the tunnel connected; when the
/// stop lands, it still finishes the round instead of calling it an
/// external stop and leaving the tunnel down.
#[test]
fn f9_restart_with_a_recovery_stop_still_in_flight_finishes_the_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    let mut stopped = false;
    for _ in 0..200 {
        step(&mut rig.wd);
        if rig.stops("a") == 1 {
            stopped = true;
            break;
        }
    }
    assert!(stopped);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Recovery));

    // The stop was accepted but the tunnel is still up when the new process
    // looks at it.
    let mut wd = restart(Lab::new(&["a"]), Startup::Restored(saved));
    run(&mut wd, 5);
    assert_eq!(first(&mut wd).os_state, OsState::Connected);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    run(&mut wd, 90);

    assert_eq!(calls(&wd, "a", is_start), 1, "the round is finished");
    assert_eq!(calls(&wd, "a", is_stop), 0);
    let a = first(&mut wd);
    assert_ne!(a.takeover, Takeover::StoppedExternally);
    assert_eq!(a.light, Light::Green);
}

/// F9: the watchdog died after recording a disconnect, before the stop took
/// effect. The new process stops the tunnel instead of treating it as one
/// started by something else.
#[test]
fn f9_restart_with_a_disconnect_still_in_flight_stops_the_tunnel() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Disconnect));

    let mut wd = restart(Lab::new(&["a"]), Startup::Restored(saved));
    run(&mut wd, 60);
    assert_eq!(calls(&wd, "a", is_stop), 1);
    assert_eq!(calls(&wd, "a", is_start), 0);
    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    let a = first(&mut wd);
    assert_ne!(a.takeover, Takeover::ExternallyControlled);
    assert_eq!(a.light_reason, LightReason::DisconnectedByUser);
}

/// F9: once the disconnect is done, a later start by something else is
/// reported and not fought: the stop is not repeated.
#[test]
fn f9_a_finished_disconnect_is_not_repeated_after_an_external_start() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(20);
    assert_eq!(rig.stops("a"), 1);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.run(60);
    assert_eq!(rig.stops("a"), 1);
    assert_eq!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// F7: a round that found no interface, applied after it went stale, is no
/// evidence and is not counted as a failure.
#[test]
fn f7_missing_interface_applied_late_is_not_counted() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::ResolveInterface,
        AdapterError::new(AdapterErrorKind::NotFound, "no interface"),
    );
    let items = next_items(&mut rig.wd);
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(36)).unwrap();
    rig.wd.apply(result);
    let a = rig.status("a");
    assert_ne!(a.data_health, DataHealth::Suspect);
    assert_ne!(a.light_reason, LightReason::Confirming);
}

/// F7: the same round applied while still fresh is counted.
#[test]
fn f7_missing_interface_applied_in_time_is_counted() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::ResolveInterface,
        AdapterError::new(AdapterErrorKind::NotFound, "no interface"),
    );
    let items = next_items(&mut rig.wd);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.apply(result);
    assert_eq!(rig.status("a").data_health, DataHealth::Suspect);
}

/// F12: an external stop right after one of WGAL's own operations, first
/// seen as transitioning, is still an external stop.
#[test]
fn f12_external_stop_right_after_an_operation_is_noticed() {
    for restart_option in [false, true] {
        let mut rig = Rig::new(&["a"]);
        rig.manage("a");
        rig.ok(Request::SetStartWhenStoppedExternally {
            tunnel_id: id("a"),
            enabled: restart_option,
        });
        rig.run_until(44);
        rig.ok(Request::Reconnect { tunnel_id: id("a") });
        rig.run(3);
        let a = rig.status("a");
        assert_eq!(a.recovery, RecoveryPhase::Idle);
        assert_eq!(a.light, Light::Green);
        assert_eq!(rig.starts("a"), 1);

        // Stopped outside WGAL before a monitoring round saw it connected.
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Transitioning);
        rig.run(16);
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        rig.run(16);
        if restart_option {
            assert_eq!(rig.starts("a"), 2, "started again, as allowed");
            assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
        } else {
            assert_eq!(rig.status("a").takeover, Takeover::StoppedExternally);
            assert_eq!(rig.starts("a"), 1);
        }
    }
}

/// F13: the host slept while a verification round was in flight. Its stale
/// result is no evidence, and the sleep does not let the old deadline turn
/// into a failed recovery or make the alert eligible.
#[test]
fn f13_stale_verification_result_after_a_sleep_does_not_fail_the_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(110);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Verifying);

    let items = next_items(&mut rig.wd);
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(120)).unwrap();
    rig.wd.apply(result);
    let a = rig.status("a");
    assert_eq!(a.recovery, RecoveryPhase::Verifying);
    let incident = a.incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::InProgress);
    assert_eq!(incident.notification, NotificationState::NotEligible);

    rig.lab().heal("a");
    rig.run(10);
    assert_eq!(
        rig.status("a").incident.unwrap().first_recovery,
        FirstRecovery::Succeeded
    );
    assert!(rig.wd.notifications_due().is_empty());
}

// Second follow-up review (`49a8bfb..942a99d`).

fn sent_late(what: &str) -> AdapterError {
    not_sent(&format!("{what} was sent but did not finish in time"))
}

/// A user's connect whose start was sent but timed out; the tunnel still
/// looks down. Returns at the moment the start failed.
fn connect_with_a_late_start(rig: &mut Rig) {
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(16);
    assert_eq!(rig.status("a").os_state, OsState::Disconnected);
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Start, sent_late("the start"));
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(1);
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(rig.status("a").os_state, OsState::Disconnected);
}

/// Review-2 finding 1: a disconnect right after a start that timed out is
/// not taken as done because the tunnel still looks down. When the late
/// start lands, the tunnel is stopped again, not marked as started by
/// something else. Both with and without another round seeing it down
/// before the disconnect.
#[test]
fn r2_1_a_late_start_after_a_disconnect_is_stopped_not_external() {
    for seen_down_again in [false, true] {
        let mut rig = Rig::new(&["a"]);
        connect_with_a_late_start(&mut rig);
        if seen_down_again {
            rig.run(16);
        }
        rig.ok(Request::Disconnect { tunnel_id: id("a") });
        assert_eq!(
            rig.store.saved().unwrap().tunnels[0].resume,
            Some(ResumeOp::Disconnect),
            "the disconnect is unfinished while the start may land"
        );
        rig.run(3);

        // The start the SCM was still working on lands.
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Transitioning);
        rig.run(16);
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Connected);
        rig.run(40);

        assert_eq!(
            rig.lab().fake.os_state(&binding("a")),
            OsState::Disconnected,
            "seen_down_again = {seen_down_again}"
        );
        assert_eq!(rig.starts("a"), 1, "a disconnect never starts the tunnel");
        let a = rig.status("a");
        assert_ne!(a.takeover, Takeover::ExternallyControlled);
        assert_eq!(a.light_reason, LightReason::DisconnectedByUser);
    }
}

/// Review-2 finding 1: the disconnect arrives while the start is still out,
/// so the start's result is discarded; the start may land all the same.
#[test]
fn r2_1_a_start_still_out_when_the_user_disconnects_cannot_defeat_it() {
    let mut rig = Rig::new(&["a"]);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(16);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_control());
    assert!(rig.wd.still_due(&items[0]));
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Start, sent_late("the start"));
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.wd.apply(result);
    rig.run(3);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.run(16);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.run(40);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(rig.starts("a"), 1);
    assert_ne!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// Review-2 finding 1, bounded: once a late start can no longer land, the
/// disconnect is done, and a later start by something else is reported and
/// not fought.
#[test]
fn r2_1_after_the_late_start_window_an_external_start_is_not_fought() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(120);
    assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
    let stops = rig.stops("a");

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.run(60);
    assert_eq!(rig.stops("a"), stops);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    assert_eq!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// Review-2 finding 1 across a restart: the new process cannot know whether
/// the old one left a start that may still land.
#[test]
fn r2_1_a_late_start_from_before_a_restart_does_not_defeat_a_disconnect() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    let saved = rig.store.saved().unwrap();

    let lab = Lab::new(&["a"]);
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let mut wd = restart(lab, Startup::Restored(saved));
    run(&mut wd, 16);
    assert_eq!(first(&mut wd).os_state, OsState::Disconnected);
    assert!(matches!(
        wd.handle(&Request::Disconnect { tunnel_id: id("a") }),
        Outcome::Ok(_)
    ));
    run(&mut wd, 3);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    run(&mut wd, 40);

    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(calls(&wd, "a", is_start), 0);
    assert_ne!(first(&mut wd).takeover, Takeover::ExternallyControlled);
}

/// Review-2 finding 2: a recovery's stop timed out while the tunnel was still
/// up, and the same stop then lands (transitioning, then down). That is
/// WGAL's own stop, not an external one: the backoff goes on and the
/// recovery round starts the tunnel when it is due, with or without "start
/// when stopped externally".
#[test]
fn r2_2_a_late_recovery_stop_is_not_an_external_stop() {
    for restart_option in [false, true] {
        let mut rig = Rig::new(&["a"]);
        rig.manage("a");
        rig.ok(Request::SetStartWhenStoppedExternally {
            tunnel_id: id("a"),
            enabled: restart_option,
        });
        rig.run_until(44);
        rig.lab().break_until_restart("a");
        rig.lab()
            .fake
            .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
        for _ in 0..200 {
            rig.run(1);
            if rig.stops("a") == 1 {
                break;
            }
        }
        assert_eq!(rig.stops("a"), 1);
        let a = rig.status("a");
        assert_eq!(a.recovery, RecoveryPhase::Backoff);
        assert_eq!(a.os_state, OsState::Connected);

        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Transitioning);
        rig.run(16);
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        rig.run(16);
        let a = rig.status("a");
        assert_ne!(
            a.takeover,
            Takeover::StoppedExternally,
            "restart_option = {restart_option}"
        );
        assert_eq!(a.recovery, RecoveryPhase::Backoff, "the backoff goes on");
        assert_eq!(rig.starts("a"), 0, "nothing starts before the retry");

        rig.run(90);
        assert_eq!(rig.starts("a"), 1, "the recovery round starts it");
        assert_eq!(rig.stops("a"), 1, "a tunnel already down is not stopped");
        assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
        assert_eq!(rig.status("a").light, Light::Green);
    }
}

/// Review-2 finding 3: a connect that needs no command leaves nothing to
/// resume, so a later external stop is never undone, whether or not
/// automatic recovery is on, while "start when stopped externally" is off.
#[test]
fn r2_3_connect_while_connected_leaves_nothing_to_resume() {
    for auto in [false, true] {
        let mut rig = Rig::new(&["a"]);
        if auto {
            rig.manage("a");
        }
        rig.run_until(44);
        rig.ok(Request::Connect { tunnel_id: id("a") });
        assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
        rig.run(1);

        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        rig.run(30);
        assert_eq!(rig.starts("a"), 0, "auto = {auto}");
        assert_eq!(
            rig.lab().fake.os_state(&binding("a")),
            OsState::Disconnected
        );
        if auto {
            assert_eq!(rig.status("a").takeover, Takeover::StoppedExternally);
        }
    }
}

// Review of `e125918`.

/// A disconnect whose stop timed out while the tunnel still looks up, then a
/// connect. Returns the time the stop failed.
fn connect_after_a_late_stop(rig: &mut Rig) -> u64 {
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(1);
    let failed_at = rig.now_secs();
    assert_eq!(rig.status("a").os_state, OsState::Connected);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    failed_at
}

/// Review of `e125918`, first P1: the connect verifies against the tunnel
/// that is still up, then the older stop lands. The connect stays unfinished
/// until then, so the tunnel is started again.
#[test]
fn r3_1_connect_survives_an_older_stop_landing() {
    for auto in [false, true] {
        let mut rig = Rig::new(&["a"]);
        if auto {
            rig.manage("a");
        }
        connect_after_a_late_stop(&mut rig);
        rig.run(2);
        assert_eq!(rig.status("a").light, Light::Green, "auto = {auto}");
        assert_eq!(rig.starts("a"), 1);
        assert_eq!(
            rig.store.saved().unwrap().tunnels[0].resume,
            Some(ResumeOp::Connect),
            "still unfinished while the stop may land"
        );

        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        rig.run(30);
        assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
        assert_eq!(rig.starts("a"), 2);
        let a = rig.status("a");
        assert_ne!(a.takeover, Takeover::StoppedExternally);
        assert_eq!(a.light, Light::Green);
        assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
    }
}

/// The pending connect ends exactly with the stop's window, without another
/// reading; a later stop is external and is not undone (review-2 finding 3).
#[test]
fn r3_1_pending_connect_ends_with_the_late_stop_window() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    let failed_at = connect_after_a_late_stop(&mut rig);
    rig.run_until(failed_at + 59);
    assert_eq!(
        rig.store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Connect)
    );
    rig.run(1);
    assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(30);
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(rig.status("a").takeover, Takeover::StoppedExternally);
}

/// A user's reconnect whose own stop timed out and then lands still starts
/// the tunnel, while a recovery waits for its backoff
/// (`r2_2_a_late_recovery_stop_is_not_an_external_stop`).
#[test]
fn r3_1_reconnect_whose_stop_lands_late_still_starts() {
    let mut rig = Rig::new(&["a"]);
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(1);
    assert_eq!(rig.status("a").light_reason, LightReason::ControlFailed);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(30);
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

/// Review of `e125918`, second P1: a confirmed disconnect's record ends
/// exactly at the late-start deadline. An external start after it, before the
/// next round reads the tunnel, is reported and not stopped.
#[test]
fn r3_2_external_start_right_after_the_deadline_is_not_stopped() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    let failed_at = rig.now_secs();
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run_until(failed_at + 59);
    assert_eq!(
        rig.store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Disconnect)
    );
    rig.run(1);
    assert_eq!(rig.store.saved().unwrap().tunnels[0].resume, None);
    let stops = rig.stops("a");
    assert_eq!(stops, 1);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.run(35);
    assert_eq!(rig.stops("a"), stops, "not fought after the window");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    assert_eq!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// A reading taken before the deadline still counts when it is applied after
/// it: the late start did land in time, so the tunnel is stopped.
#[test]
fn r3_2_reading_from_before_the_deadline_still_counts() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    let failed_at = rig.now_secs();
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run_until(failed_at + 59);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(2)).unwrap();
    assert!(rig.wd.plan().is_empty());
    rig.wd.apply(result);
    rig.run(5);

    assert_eq!(rig.stops("a"), 2);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_ne!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// A round planned before the deadline whose reading is taken after it: the
/// tunnel coming up then is someone else's doing and is not stopped.
#[test]
fn r3_2_reading_from_after_the_deadline_is_external() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    let failed_at = rig.now_secs();
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run_until(failed_at + 59);

    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    rig.wd.clock().advance(Duration::from_secs(2)).unwrap();
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.apply(result);
    rig.run(5);

    assert_eq!(rig.stops("a"), 1);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    assert_eq!(rig.status("a").takeover, Takeover::ExternallyControlled);
}

/// A late recovery stop read as landed before its deadline stays WGAL's own
/// even when the reading is applied after the deadline.
#[test]
fn r3_2_late_stop_read_in_time_and_applied_late_is_not_external() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
    for _ in 0..200 {
        rig.run(1);
        if rig.stops("a") == 1 {
            break;
        }
    }
    assert_eq!(rig.stops("a"), 1);
    let failed_at = rig.now_secs();
    rig.run_until(failed_at + 59);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(2)).unwrap();
    assert!(rig.wd.plan().is_empty());
    rig.wd.apply(result);
    assert_ne!(rig.status("a").takeover, Takeover::StoppedExternally);

    rig.run(30);
    assert_eq!(rig.starts("a"), 1, "the recovery round starts it");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

// Review of `2694ab3`.

/// Restarts with the wall clock at `wall`, on a store the test can inspect.
fn restart_at(state: PersistedState, wall: i64) -> (Wd, MemoryStore) {
    let store = MemoryStore::new();
    let wd = Watchdog::new(
        Lab::new(&["a"]),
        FakeClock::new(wall),
        Timing::default(),
        Box::new(store.clone()),
        Startup::Restored(state),
    );
    (wd, store)
}

/// A verified connect kept because an older stop may still land, as saved,
/// and the wall time that stop's window ends.
fn saved_verified_connect() -> (PersistedState, i64) {
    let mut rig = Rig::new(&["a"]);
    assert!(!rig.status("a").intent.auto_recovery_enabled);
    connect_after_a_late_stop(&mut rig);
    rig.run(2);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Connect));
    let due = saved.tunnels[0].resume_until_unix_ms.unwrap();
    (saved, due)
}

/// Review of `2694ab3`, P1: restarted after the late-stop window, the kept
/// connect is gone. An external stop is left alone, with automatic recovery
/// off, instead of being taken for WGAL's own stop and undone.
#[test]
fn r4_restart_after_the_deadline_leaves_an_external_stop_alone() {
    for late_by_ms in [1_000, 600_000] {
        let (saved, due) = saved_verified_connect();
        let (mut wd, store) = restart_at(saved, due + late_by_ms);
        assert_eq!(store.saved().unwrap().tunnels[0].resume, None);
        run(&mut wd, 5);
        assert_eq!(first(&mut wd).os_state, OsState::Connected);

        wd.adapter()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        run(&mut wd, 60);
        assert_eq!(calls(&wd, "a", is_start), 0, "late by {late_by_ms} ms");
        assert_eq!(
            wd.adapter().fake.os_state(&binding("a")),
            OsState::Disconnected
        );
    }
}

/// Review of `2694ab3`, P1: restarted inside the window, the record keeps the
/// original deadline, not a new one. The older stop landing before it still
/// finishes the connect; a stop after it is external and is left alone.
#[test]
fn r4_restart_inside_the_window_keeps_the_original_deadline() {
    let (saved, due) = saved_verified_connect();
    let (mut wd, store) = restart_at(saved.clone(), due - 5_000);
    run(&mut wd, 4);
    let kept = store.saved().unwrap();
    assert_eq!(kept.tunnels[0].resume, Some(ResumeOp::Connect));
    assert_eq!(kept.tunnels[0].resume_until_unix_ms, Some(due));
    run(&mut wd, 1);
    let ended = store.saved().unwrap();
    assert_eq!(ended.tunnels[0].resume, None);
    assert_eq!(ended.tunnels[0].resume_until_unix_ms, None);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    run(&mut wd, 60);
    assert_eq!(calls(&wd, "a", is_start), 0, "past the original deadline");

    let (mut wd, _) = restart_at(saved, due - 30_000);
    run(&mut wd, 5);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    run(&mut wd, 20);
    assert_eq!(calls(&wd, "a", is_start), 1, "inside the original deadline");
    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Connected
    );
}

/// The same for a confirmed disconnect: restarted after the late-start window,
/// an external start is reported and not stopped.
#[test]
fn r4_restart_after_the_deadline_does_not_stop_an_external_start() {
    let mut rig = Rig::new(&["a"]);
    connect_with_a_late_start(&mut rig);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(3);
    assert_eq!(rig.stops("a"), 1);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Disconnect));
    let due = saved.tunnels[0].resume_until_unix_ms.unwrap();

    let (mut wd, _) = restart_at(saved.clone(), due + 1_000);
    run(&mut wd, 30);
    assert_eq!(calls(&wd, "a", is_stop), 0);
    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Connected
    );
    assert_eq!(first(&mut wd).takeover, Takeover::ExternallyControlled);

    // Inside the window the disconnect is finished, and its record still ends
    // at the original deadline, not a full window after the restart.
    let (mut wd, store) = restart_at(saved, due - 5_000);
    run(&mut wd, 4);
    assert_eq!(calls(&wd, "a", is_stop), 1);
    assert_eq!(
        store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Disconnect)
    );
    run(&mut wd, 1);
    assert_eq!(store.saved().unwrap().tunnels[0].resume, None);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    run(&mut wd, 30);
    assert_eq!(calls(&wd, "a", is_stop), 1);
    assert_eq!(first(&mut wd).takeover, Takeover::ExternallyControlled);
}

/// A request that replaces what is to be resumed does not inherit the
/// deadline kept for the old record.
#[test]
fn r4_a_new_request_does_not_inherit_a_kept_deadline() {
    for request in [
        Request::Connect { tunnel_id: id("a") },
        Request::Reconnect { tunnel_id: id("a") },
        Request::Disconnect { tunnel_id: id("a") },
    ] {
        let mut rig = Rig::new(&["a"]);
        connect_after_a_late_stop(&mut rig);
        rig.run(2);
        let kept = rig.store.saved().unwrap();
        assert!(kept.tunnels[0].resume_until_unix_ms.is_some());

        rig.ok(request.clone());
        let saved = rig.store.saved().unwrap();
        assert!(saved.tunnels[0].resume.is_some(), "{request:?}");
        assert_eq!(saved.tunnels[0].resume_until_unix_ms, None, "{request:?}");
    }
}

/// A clock set back across the restart cannot stretch the deadline past one
/// window.
#[test]
fn r4_a_clock_set_back_cannot_stretch_the_deadline() {
    let (saved, due) = saved_verified_connect();
    let (mut wd, store) = restart_at(saved, due - 3_600_000);
    run(&mut wd, 59);
    assert_eq!(
        store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Connect)
    );
    run(&mut wd, 1);
    assert_eq!(store.saved().unwrap().tunnels[0].resume, None);
}

// Review of `b5e4828`.

/// A confirmed disconnect kept for a late start, as saved, and its deadline.
fn saved_confirmed_disconnect(rig: &mut Rig) -> i64 {
    connect_with_a_late_start(rig);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(3);
    assert_eq!(rig.stops("a"), 1);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Disconnect));
    saved.tunnels[0].resume_until_unix_ms.unwrap()
}

/// The tunnel a restarted watchdog finds up is stopped, not taken for one
/// started by something else, and nothing starts it.
fn assert_final_stop_survives(state: PersistedState, wall: i64) {
    assert_eq!(state.tunnels[0].resume, Some(ResumeOp::Disconnect));
    assert_eq!(state.tunnels[0].resume_until_unix_ms, None);
    let (mut wd, _) = restart_at(state, wall);
    run(&mut wd, 30);
    assert_eq!(calls(&wd, "a", is_stop), 1);
    assert_eq!(calls(&wd, "a", is_start), 0);
    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_ne!(first(&mut wd).takeover, Takeover::ExternallyControlled);
}

/// Review of `b5e4828`, P1: a late start lands inside the window, and the
/// watchdog dies around the stop that answers it. Restarted past the old
/// deadline, it still finishes the disconnect: after the reading that saw the
/// tunnel up, and after the stop went out with no result applied.
#[test]
fn r5_crash_around_a_redriven_stop_keeps_the_disconnect() {
    let mut rig = Rig::new(&["a"]);
    let due = saved_confirmed_disconnect(&mut rig);

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    assert!(!items[0].is_control());
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.apply(result);
    assert_final_stop_survives(rig.store.saved().unwrap(), due + 1_000);

    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_control());
    assert!(rig.wd.still_due(&items[0]));
    let _sent_but_never_applied = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    assert_final_stop_survives(rig.store.saved().unwrap(), due + 1_000);
}

/// Review of `b5e4828`, P1: restored inside the window, the record is redriven
/// before any reading. Its deadline is removed in storage before the stop goes
/// out, so a second crash past the deadline cannot drop the disconnect.
#[test]
fn r5_redriving_a_restored_record_stores_the_removal_before_the_stop() {
    let mut rig = Rig::new(&["a"]);
    let due = saved_confirmed_disconnect(&mut rig);
    let saved = rig.store.saved().unwrap();

    let (mut wd, store) = restart_at(saved, due - 5_000);
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = wd.plan();
    assert_eq!(items.len(), 1);
    assert!(items[0].is_control());
    assert_eq!(calls(&wd, "a", is_stop), 0, "nothing has been sent yet");
    assert_final_stop_survives(store.saved().unwrap(), due + 1_000);
}

/// A save that fails does not hold back the user's stop.
#[test]
fn r5_a_failed_save_does_not_hold_back_the_redriven_stop() {
    let mut rig = Rig::new(&["a"]);
    let due = saved_confirmed_disconnect(&mut rig);
    let saved = rig.store.saved().unwrap();

    let (mut wd, store) = restart_at(saved, due - 5_000);
    store.set_failing(true);
    run(&mut wd, 2);
    assert_eq!(calls(&wd, "a", is_stop), 1);
    assert_eq!(
        wd.adapter().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(first(&mut wd).light_reason, LightReason::DisconnectedByUser);
}

/// Review-2 finding 3: a connect that does send a start still records it, so
/// a restart finishes it.
#[test]
fn r2_3_connect_that_starts_the_tunnel_is_still_resumable() {
    let mut rig = Rig::new(&["a"]);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(16);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    assert_eq!(
        rig.store.saved().unwrap().tunnels[0].resume,
        Some(ResumeOp::Connect)
    );
}

/// Codex P2-N51: a fault confirmed while a reconnect's stop may still land
/// late does not replace that connect with a recovery: when the late stop
/// lands, the connect still starts the tunnel, even if the host woke in
/// between.
#[test]
fn a_connect_whose_stop_may_land_late_is_not_replaced_by_a_recovery() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", support::timing(10, 2, 0, 0));
    rig.run(20);
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(1);
    assert_eq!(rig.status("a").light_reason, LightReason::ControlFailed);
    // The traffic stops and the fault is confirmed within the window.
    rig.lab().break_until_restart("a");
    let mut held = None;
    'plan: for _ in 0..40 {
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
        for item in rig.wd.plan() {
            if item.is_control() {
                held = Some(item);
                break 'plan;
            }
            if rig.wd.still_due(&item) {
                let result = wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item);
                rig.wd.apply(result);
            }
        }
    }
    if let Some(item) = held {
        // A recovery was planned anyway; the host wakes before it is sent.
        rig.wake();
        assert!(!rig.wd.still_due(&item));
    }
    // The late stop lands.
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(30);
    assert!(rig.starts("a") >= 1, "the connect was lost");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

/// Codex P2-N55: a recovery held back while the user's connect was
/// unsettled comes as soon as the connect's record ends, not at the next
/// monitoring round an hour later.
#[test]
fn a_recovery_held_for_a_connect_comes_when_the_connect_settles() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", support::timing(3600, 2, 0, 0));
    rig.run(20);
    rig.lab()
        .fake
        .fail_next(&binding("a"), Operation::Stop, sent_late("the stop"));
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.lab().break_until_restart("a");
    rig.run(1);
    assert_eq!(rig.stops("a"), 1, "the reconnect's stop");
    rig.run(120);
    assert!(
        rig.stops("a") >= 2,
        "the recovery waited for the next hourly round"
    );
}
