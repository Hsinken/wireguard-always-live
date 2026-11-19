//! Regression tests for the integrated A–E review of `121b42a` (2026-11-03).
//! Each test is named after the finding it covers and drives the watchdog
//! with the fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Rig, binding, id};
use wgal_core::{
    AdapterError, MemoryStore, NotificationOutcome, ResumeOp, Startup, Timing, Watchdog, execute,
};
use wgal_protocol::{
    AdapterErrorKind, Capability, DesiredState, ErrorCode, FirstRecovery, Light, LightReason,
    NotificationState, OsState, Outcome, Request, StorageHealth, Takeover,
};
use wgal_testkit::{Call, FakeClock, Operation};

fn incident_notification(rig: &mut Rig, name: &str) -> Option<(FirstRecovery, NotificationState)> {
    rig.status(name)
        .incident
        .map(|i| (i.first_recovery, i.notification))
}

/// P1-1: a request for a tunnel whose binding is lost must not hold the host
/// control slot, or every other tunnel's recovery and disconnect waits forever.
#[test]
fn p1_1_a_lost_binding_never_holds_the_control_slot() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.manage("b");
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::QueryState,
        AdapterError::new(AdapterErrorKind::NotFound, "service removed"),
    );
    rig.run(20);
    assert_eq!(rig.status("a").light_reason, LightReason::BindingLost);

    // Whatever the GUI asks of the lost tunnel is refused ...
    for request in [
        Request::Connect { tunnel_id: id("a") },
        Request::Disconnect { tunnel_id: id("a") },
        Request::Reconnect { tunnel_id: id("a") },
    ] {
        assert!(
            matches!(rig.request(request.clone()), Outcome::Err(e) if e.code == ErrorCode::InvalidState),
            "{request:?} was accepted for a lost binding"
        );
    }

    // ... b's recovery and b's disconnect still run.
    rig.lab().break_for_good("b");
    rig.run(600);
    assert!(rig.stops("b") >= 1, "b's recovery never ran");
    rig.ok(Request::Disconnect { tunnel_id: id("b") });
    rig.run(120);
    assert_eq!(
        rig.lab().fake.os_state(&binding("b")),
        wgal_protocol::OsState::Disconnected,
        "b's disconnect never ran"
    );
}

/// One second, then a single plan/execute/apply pass, so a test can act
/// between steps that `tick` would run back to back.
fn step(rig: &mut Rig) {
    let wd = &mut rig.wd;
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    for item in wd.plan() {
        let result = execute(wd.adapter(), wd.clock(), &item);
        wd.apply(result);
    }
}

/// P2-1: verification that only met probe-tool or local-offline errors did
/// not show the tunnel down, so it is no failed recovery and raises no alert.
#[test]
fn p2_1_errors_during_verification_are_not_a_failed_recovery() {
    for kind in [
        AdapterErrorKind::LocalNetworkOffline,
        AdapterErrorKind::PermissionDenied,
    ] {
        let mut rig = Rig::new(&["a"]);
        rig.manage("a");
        rig.run_until(44);
        rig.lab().break_until_restart("a");
        while rig.stops("a") == 0 {
            step(&mut rig);
            assert!(rig.now_secs() < 400, "recovery never started");
        }
        // The stop went out and nothing after it has run: everything the
        // verification sees is an error.
        rig.lab().fail_probes("a", Some(kind));
        for _ in 0..90 {
            step(&mut rig);
        }
        assert!(rig.starts("a") >= 1);
        let state = incident_notification(&mut rig, "a");
        assert_ne!(
            state.map(|s| s.0),
            Some(FirstRecovery::Failed),
            "{kind:?} during verification counted as a failed recovery"
        );
        assert!(
            rig.wd.notifications_due().is_empty(),
            "{kind:?} during verification made an alert due"
        );
    }
}

/// P2-2: a disconnected tunnel whose OS state cannot be read is unknown, not
/// normal gray.
#[test]
fn p2_2_unknown_state_of_a_disconnected_tunnel_is_not_gray() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(20);
    assert_eq!(
        rig.status("a").light_reason,
        LightReason::DisconnectedByUser
    );
    for _ in 0..60 {
        rig.lab().fake.fail_next(
            &binding("a"),
            Operation::QueryState,
            AdapterError::new(AdapterErrorKind::ParseError, "garbled"),
        );
    }
    rig.run(20);
    let status = rig.status("a");
    assert_eq!(status.intent.desired, DesiredState::Disconnected);
    assert_ne!(status.light, Light::Gray, "{:?}", status.light_reason);
}

/// P2-3: failures from before a local-offline period are not counted again
/// once the network returns; confirmation needs 3 rounds over 60 s afresh.
#[test]
fn p2_3_offline_period_breaks_the_failure_streak() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(75); // two failed rounds
    assert_eq!(rig.stops("a"), 0);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    rig.lab().fail_probes("a", None);
    let back = rig.now_secs();
    while rig.stops("a") == 0 {
        rig.run(1);
        assert!(rig.now_secs() < back + 400, "never recovered");
    }
    // 30 s grace, then at least 60 s of fresh failures.
    assert!(
        rig.now_secs() >= back + 30 + 60,
        "recovery started {} s after the network returned",
        rig.now_secs() - back
    );
}

/// P2-4: a tunnel that discovery reports as controlled by another manager is
/// not registered for control.
#[test]
fn p2_4_discovered_unsupported_control_is_refused() {
    let lab = support::Lab::new(&["a"]);
    lab.fake.set_control(&binding("a"), Capability::Unsupported);
    let store = wgal_core::MemoryStore::new();
    let mut wd = wgal_core::Watchdog::new(
        lab,
        wgal_testkit::FakeClock::new(support::T0),
        wgal_core::Timing::default(),
        Box::new(store),
        wgal_core::Startup::Empty,
    );
    let _ = wd.handle(&Request::DiscoverTunnels);
    let registered = wd.handle(&Request::RegisterTunnel { tunnel_id: id("a") });
    assert!(
        matches!(&registered, Outcome::Err(e) if e.code == ErrorCode::InvalidState),
        "{registered:?}"
    );
    if matches!(registered, Outcome::Ok(_)) {
        // If it is registered at all, WGAL must never start or stop it.
        let _ = wd.handle(&Request::Disconnect { tunnel_id: id("a") });
        let _ = wd.handle(&Request::Connect { tunnel_id: id("a") });
        for _ in 0..120 {
            wd.clock()
                .advance(std::time::Duration::from_secs(1))
                .unwrap();
            wd.tick();
        }
        let controls = wd
            .adapter()
            .fake
            .calls()
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    wgal_testkit::Call::Start(_) | wgal_testkit::Call::Stop(_)
                )
            })
            .count();
        assert_eq!(controls, 0, "WGAL controlled a tunnel another manager owns");
    }
}

/// P2-6: storage that fails once and then works again does not block
/// recovery forever when nothing else changes.
#[test]
fn p2_6_a_transient_save_failure_does_not_block_recovery_for_good() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.store.set_failing(true);
    rig.run(120);
    assert_eq!(rig.wd.snapshot().storage, StorageHealth::Failed);
    assert_eq!(rig.stops("a"), 0);
    rig.store.set_failing(false);
    rig.run(600);
    assert!(rig.stops("a") >= 1, "recovery never resumed");
}

type Wd = Watchdog<support::Lab, FakeClock>;

fn restart(lab: support::Lab, startup: Startup) -> Wd {
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        startup,
    );
    wd.tick();
    wd
}

fn run(wd: &mut Wd, secs: u64) {
    for _ in 0..secs {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
}

fn starts(wd: &Wd, name: &str) -> usize {
    wd.adapter()
        .fake
        .calls_for(&binding(name))
        .iter()
        .filter(|c| matches!(c, Call::Start(_)))
        .count()
}

/// P2-5: after a restart, a recovery stop the old process sent that lands
/// 40 s later is still WGAL's own: the round finishes with a start instead of
/// marking the tunnel stopped externally and leaving it down.
#[test]
fn p2_5_a_restored_recovery_stop_landing_late_still_finishes_the_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    while rig.stops("a") == 0 {
        step(&mut rig);
        assert!(rig.now_secs() < 400, "recovery never started");
    }
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Recovery));

    let mut wd = restart(support::Lab::new(&["a"]), Startup::Restored(saved));
    run(&mut wd, 40);
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    run(&mut wd, 90);
    assert_eq!(starts(&wd, "a"), 1, "the round was not finished");
    let snapshot = wd.snapshot();
    assert_ne!(snapshot.tunnels[0].takeover, Takeover::StoppedExternally);
}

/// P3 (wake): a notification retried later waits until the fault is seen
/// again after a wake, instead of going out on evidence from before sleep.
#[test]
fn p3_after_a_wake_a_pending_alert_waits_for_fresh_evidence() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let mut due = rig.wd.notifications_due();
    while due.is_empty() {
        rig.run(1);
        assert!(rig.now_secs() < 600, "no alert became due");
        due = rig.wd.notifications_due();
    }
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::RetryLater)
        .unwrap();
    assert!(!rig.wd.notifications_due().is_empty());
    rig.wd.on_wake();
    assert!(
        rig.wd.notifications_due().is_empty(),
        "due right after a wake, before any new evidence"
    );
    rig.run(120);
    assert!(
        !rig.wd.notifications_due().is_empty(),
        "never due again once the fault was seen again"
    );
}

/// Drives a managed tunnel with a dead data path until its first recovery
/// round is verifying, one plan/execute/apply pass at a time.
fn to_verifying(rig: &mut Rig) {
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    while rig.status("a").recovery != wgal_protocol::RecoveryPhase::Verifying {
        step(rig);
        assert!(rig.now_secs() < 600, "recovery never reached verification");
    }
}

/// Re-review NF-1: the restarted tunnel going down again during
/// verification is a failed round, not an inconclusive one: red backoff,
/// the alert due, and recovery keeps retrying (plan §6.3, §7.2).
#[test]
fn nf1_a_tunnel_down_during_verification_is_a_failed_round() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    let (mut due, mut failed) = (false, false);
    for _ in 0..600 {
        step(&mut rig);
        due |= !rig.wd.notifications_due().is_empty();
        failed |= incident_notification(&mut rig, "a").map(|s| s.0) == Some(FirstRecovery::Failed);
    }
    assert!(failed, "the round was not recorded as failed");
    assert!(due, "no alert became due");
    // Recovery retried, and the second restart (a new interface) works.
    assert!(rig.starts("a") >= 2, "the tunnel was left down");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

/// Re-review NF-2: Connected with no interface during verification counts
/// as failure evidence, as it does in monitoring.
#[test]
fn nf2_no_interface_during_verification_is_a_failed_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    for _ in 0..2000 {
        rig.lab().fake.fail_next(
            &binding("a"),
            Operation::ResolveInterface,
            AdapterError::new(AdapterErrorKind::NotFound, "no interface"),
        );
    }
    let mut due = false;
    for _ in 0..900 {
        rig.run(1);
        due |= !rig.wd.notifications_due().is_empty();
    }
    assert_eq!(
        incident_notification(&mut rig, "a").map(|s| s.0),
        Some(FirstRecovery::Failed)
    );
    assert!(due, "no alert became due");
    // And recovery keeps retrying at low frequency instead of giving up
    // behind a path that can never be shown (re-review of 64a3030).
    assert!(
        rig.stops("a") >= 2,
        "recovery stopped after the first round"
    );
}

/// Re-review of 64a3030, N2: a probe error from the verification does not keep
/// a tunnel WGAL left down from being started again.
#[test]
fn n2_a_probe_error_before_the_tunnel_went_down_does_not_keep_it_down() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::PermissionDenied));
    step(&mut rig);
    rig.lab().fail_probes("a", None);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    for _ in 0..600 {
        step(&mut rig);
    }
    assert!(rig.starts("a") >= 2, "the tunnel was left down");
}

/// Re-review NF-3: verification that only ever meets errors cannot restart
/// the tunnel every couple of minutes; inconclusive rounds back off too.
///
/// The errors alone never make the round a failed one. Since the review of
/// `3d77031` (P2), the fault confirmed again through the dead path after the
/// restart does: the first recovery failed after all, and the alert is due.
#[test]
fn nf3_inconclusive_rounds_back_off() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    // Rounds wait the tunnel's retry interval (owner decision of
    // 2026-11-04); 5 minutes here, like the old backoff's third step.
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(None, 300).unwrap(),
    });
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let start = rig.now_secs();
    let mut after_first_round = None;
    while rig.now_secs() < start + 1200 {
        let verifying = rig.status("a").recovery == wgal_protocol::RecoveryPhase::Verifying;
        rig.lab()
            .fail_probes("a", verifying.then_some(AdapterErrorKind::PermissionDenied));
        step(&mut rig);
        let still = rig.status("a").recovery == wgal_protocol::RecoveryPhase::Verifying;
        if verifying && !still && after_first_round.is_none() {
            after_first_round = incident_notification(&mut rig, "a");
        }
    }
    let stops = rig.stops("a");
    assert!(stops >= 2, "{stops} stops: recovery never retried");
    assert!(stops <= 4, "{stops} stops in 20 minutes: no backoff");
    assert_eq!(
        after_first_round,
        Some((FirstRecovery::InProgress, NotificationState::NotEligible)),
        "errors during verification alone made the round a failed one"
    );
    assert_eq!(
        incident_notification(&mut rig, "a"),
        Some((FirstRecovery::Failed, NotificationState::Pending))
    );
}
