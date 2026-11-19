//! Regression tests for the independent review of `3d77031` (2026-11-03):
//! a retry that is due while the tunnel is not settled, an inconclusive
//! recovery round that ends with the tunnel not settled (P2), when such a
//! round counts as failed after all, and a user's connect that fails during
//! an automatic-recovery incident (P3-3). Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Lab, Rig, binding, id};
use wgal_core::{AdapterError, MemoryStore, Startup, Timing, Watchdog, execute};
use wgal_protocol::{
    AdapterErrorKind, FirstRecovery, Light, LightReason, NotificationState, OsState, RecoveryPhase,
    Request,
};
use wgal_testkit::{Call, FakeClock, Operation};

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

fn queries(rig: &Rig) -> usize {
    rig.calls("a")
        .iter()
        .filter(|c| matches!(c, Call::QueryState(_)))
        .count()
}

fn incident(rig: &mut Rig) -> Option<(FirstRecovery, NotificationState)> {
    rig.status("a")
        .incident
        .map(|i| (i.first_recovery, i.notification))
}

fn set_os(rig: &Rig, state: OsState) {
    rig.lab().fake.set_os_state(&binding("a"), state);
}

fn persisted_retry(rig: &Rig) -> Option<i64> {
    rig.store.saved().unwrap().tunnels[0]
        .incident
        .as_ref()
        .and_then(|i| i.next_retry_unix_ms)
}

/// The first round failed with the tunnel left down; the retry is pending.
fn left_down_after_a_failed_round(rig: &mut Rig) {
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Verifying {
        rig.run(1);
        assert!(rig.now_secs() < 600, "recovery never reached verification");
    }
    set_os(rig, OsState::Disconnected);
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the round never failed");
    }
}

/// A managed tunnel with a dead data path whose first recovery round has
/// started and reached verification.
fn to_verifying(rig: &mut Rig) {
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Verifying {
        step(rig);
        assert!(rig.now_secs() < 700, "recovery never reached verification");
    }
}

/// P3-1, mutant M4: the due retry neither stops nor starts a tunnel that is
/// changing state, does not poll it in a busy loop, keeps the stored retry,
/// and runs once the tunnel settles down.
#[test]
fn p3_1_nothing_starts_or_stops_while_transitioning_at_the_retry() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let (starts, stops, reads) = (rig.starts("a"), rig.stops("a"), queries(&rig));
    let step_before = rig.status("a").incident.unwrap().backoff_step;
    set_os(&rig, OsState::Transitioning);
    rig.run(150); // the 60 s retry comes due in this window
    assert_eq!(rig.starts("a"), starts, "started while transitioning");
    assert_eq!(rig.stops("a"), stops, "stopped while transitioning");
    let reads = queries(&rig) - reads;
    assert!(reads <= 11, "{reads} state reads in 150 s: busy loop");
    let status = rig.status("a");
    assert_eq!(status.recovery, RecoveryPhase::Backoff);
    assert_eq!(status.incident.unwrap().backoff_step, step_before);
    assert!(
        persisted_retry(&rig).is_some(),
        "the stored retry was dropped"
    );
    set_os(&rig, OsState::Disconnected);
    rig.run(20);
    assert!(
        rig.starts("a") > starts,
        "not retried within 20 s of settling"
    );
    assert_eq!(
        rig.status("a").incident.unwrap().backoff_step,
        step_before + 1
    );
}

/// P3-1, mutant M2: a state the platform reports as `Unknown` at the retry
/// waits as well, then the retry runs.
#[test]
fn p3_1_an_unknown_state_at_the_retry_waits_then_retries() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let (starts, stops) = (rig.starts("a"), rig.stops("a"));
    set_os(&rig, OsState::Unknown);
    rig.run(150);
    assert_eq!(rig.starts("a"), starts, "started while unknown");
    assert_eq!(rig.stops("a"), stops, "stopped while unknown");
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Backoff);
    set_os(&rig, OsState::Disconnected);
    rig.run(20);
    assert!(rig.starts("a") > starts, "the retries ended for good");
}

/// A restart while the retry waits for a settled state keeps the retry.
#[test]
fn a_restart_while_the_retry_waits_keeps_it() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    set_os(&rig, OsState::Transitioning);
    rig.run(150);
    let saved = rig.store.saved().unwrap();
    let lab = Lab::new(&["a"]);
    lab.fake.set_os_state(&binding("a"), OsState::Transitioning);
    let mut wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    let starts = |wd: &Watchdog<Lab, FakeClock>| {
        wd.adapter()
            .fake
            .calls_for(&binding("a"))
            .iter()
            .filter(|c| matches!(c, Call::Start(_)))
            .count()
    };
    for _ in 0..60 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
    assert_eq!(starts(&wd), 0, "started while transitioning after restart");
    wd.adapter()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    for _ in 0..30 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
    assert!(starts(&wd) > 0, "the retry was lost across the restart");
}

/// Connected again while waiting: the retry is dropped, the fault is
/// confirmed again, and a still dead path gets its next round.
#[test]
fn connected_again_while_the_retry_waits_needs_a_new_confirmation() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let stops = rig.stops("a");
    set_os(&rig, OsState::Transitioning);
    rig.run(100);
    set_os(&rig, OsState::Connected);
    rig.run(20);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Idle);
    assert_eq!(persisted_retry(&rig), None);
    rig.run(400);
    assert!(rig.stops("a") > stops, "no new round after reconfirmation");
}

/// A disconnect while waiting is final: nothing starts the tunnel again and
/// the alert is cancelled.
#[test]
fn a_disconnect_while_the_retry_waits_is_never_undone() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    set_os(&rig, OsState::Transitioning);
    rig.run(100);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(60);
    let starts = rig.starts("a");
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    rig.run(1800);
    assert_eq!(rig.starts("a"), starts, "the scheduler undid a disconnect");
    assert_eq!(
        rig.status("a").incident.unwrap().notification,
        NotificationState::Cancelled
    );
    assert!(rig.wd.notifications_due().is_empty());
}

/// A pause while waiting keeps the overdue retry for after the pause.
#[test]
fn a_pause_while_the_retry_waits_keeps_it() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    let starts = rig.starts("a");
    set_os(&rig, OsState::Transitioning);
    rig.run(100);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    set_os(&rig, OsState::Disconnected);
    rig.run(120);
    assert_eq!(rig.starts("a"), starts, "started while paused");
    rig.ok(Request::SetMonitoringPaused { paused: false });
    rig.run(20);
    assert!(
        rig.starts("a") > starts,
        "the retry was lost across the pause"
    );
}

/// What P2 needs after the round: the inconclusive round alone is no failed
/// round, it backs off with a stored retry instead of ending idle, and once
/// the tunnel settles down the retry starts it, finds the first round failed
/// after all and makes the alert due, once.
fn left_down_is_retried_and_reported(rig: &mut Rig) {
    let status = rig.status("a");
    assert_eq!(status.recovery, RecoveryPhase::Backoff, "{status:?}");
    assert_eq!(
        incident(rig),
        Some((FirstRecovery::InProgress, NotificationState::NotEligible)),
        "the inconclusive round alone made the alert due"
    );
    assert!(rig.wd.notifications_due().is_empty());
    assert!(persisted_retry(rig).is_some(), "the retry is not stored");
    assert_eq!(rig.starts("a"), 1);

    rig.lab().fail_probes("a", None);
    set_os(rig, OsState::Disconnected);
    rig.run(1800);
    let status = rig.status("a");
    assert!(
        rig.starts("a") > 1,
        "left down for good: {:?}/{:?}",
        status.light,
        status.light_reason
    );
    assert_eq!(
        incident(rig),
        Some((FirstRecovery::Failed, NotificationState::Pending))
    );
    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1, "{due:?}");
    assert_eq!(due[0].first_recovery, FirstRecovery::Failed);
}

/// P2 (reproduction rv6): a probe error in the first verification round,
/// then the restarted tunnel stays unsettled past the deadline, then settles
/// down.
#[test]
fn p2_a_round_ending_with_the_tunnel_in_transition_is_retried() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.stops("a") == 0 {
        step(&mut rig);
        assert!(rig.now_secs() < 600, "recovery never started");
    }
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::PermissionDenied));
    while rig.status("a").recovery != RecoveryPhase::Verifying {
        step(&mut rig);
        assert!(rig.now_secs() < 700);
    }
    step(&mut rig); // the first verification round meets a probe error
    set_os(&rig, OsState::Transitioning);
    for _ in 0..60 {
        step(&mut rig);
    }
    left_down_is_retried_and_reported(&mut rig);
}

/// P2 (reproduction rv7): unsettled from the first verification round to
/// past the deadline.
#[test]
fn p2_a_tunnel_unsettled_through_the_whole_verification_is_retried() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    set_os(&rig, OsState::Transitioning);
    for _ in 0..60 {
        step(&mut rig);
    }
    left_down_is_retried_and_reported(&mut rig);
}

/// P2 (reproduction rv8): the state cannot be read through the whole
/// verification while the tunnel is in fact down.
#[test]
fn p2_a_state_unreadable_through_the_whole_verification_is_retried() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    for _ in 0..12 {
        rig.lab().fake.fail_next(
            &binding("a"),
            Operation::QueryState,
            AdapterError::new(AdapterErrorKind::Timeout, "slow"),
        );
    }
    set_os(&rig, OsState::Disconnected);
    for _ in 0..50 {
        step(&mut rig);
    }
    left_down_is_retried_and_reported(&mut rig);
}

/// P2, decision: the first round left the tunnel down, so it failed, even
/// when the retry then brings the tunnel back. As after any failed first
/// round, the alert is due from the moment that is known.
#[test]
fn p2_a_first_round_that_left_the_tunnel_down_failed_even_if_the_retry_works() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    set_os(&rig, OsState::Transitioning);
    for _ in 0..60 {
        step(&mut rig);
    }
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Backoff);
    rig.lab().heal("a");
    set_os(&rig, OsState::Disconnected);
    let mut due_while_retrying = false;
    while rig.starts("a") < 2 {
        step(&mut rig);
        due_while_retrying |= !rig.wd.notifications_due().is_empty();
        assert!(rig.now_secs() < 1200, "never retried");
    }
    rig.run(30);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green, "the retry did not work");
    assert_eq!(
        incident(&mut rig),
        Some((FirstRecovery::Failed, NotificationState::Pending))
    );
    assert!(due_while_retrying, "the alert never came due");
}

/// P2, the limit of the decision: a tunnel that never settles again gives no
/// evidence either way. WGAL waits, touches nothing, shows the unknown state
/// in red and raises no alert.
#[test]
fn p2_a_tunnel_that_never_settles_is_shown_unknown_without_an_alert() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    set_os(&rig, OsState::Transitioning);
    rig.run(1800);
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(rig.stops("a"), 1);
    let status = rig.status("a");
    assert_eq!(status.recovery, RecoveryPhase::Backoff);
    assert_eq!(
        (status.light, status.light_reason),
        (Light::Red, LightReason::StatusUnknown)
    );
    assert_eq!(
        incident(&mut rig),
        Some((FirstRecovery::InProgress, NotificationState::NotEligible))
    );
    assert!(rig.wd.notifications_due().is_empty());
    assert!(persisted_retry(&rig).is_some());
}

/// Decision on eligibility, connected case: the fault confirmed again with no
/// success since an inconclusive first round shows the restart did not bring
/// traffic back. The round counts as failed then, and only then.
#[test]
fn an_inconclusive_round_counts_as_failed_once_the_fault_is_confirmed_again() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::PermissionDenied));
    while rig.status("a").recovery == RecoveryPhase::Verifying {
        step(&mut rig);
        assert!(rig.now_secs() < 900);
    }
    assert_eq!(rig.status("a").os_state, OsState::Connected);
    assert_eq!(
        incident(&mut rig),
        Some((FirstRecovery::InProgress, NotificationState::NotEligible))
    );
    // The probe works again and shows the path still dead.
    rig.lab().fail_probes("a", None);
    let mut confirmed_at = None;
    for _ in 0..120 {
        step(&mut rig);
        if confirmed_at.is_none() && incident(&mut rig).map(|i| i.0) == Some(FirstRecovery::Failed)
        {
            confirmed_at = Some(rig.now_secs());
        }
    }
    assert!(confirmed_at.is_some(), "never counted as failed");
    assert_eq!(
        incident(&mut rig),
        Some((FirstRecovery::Failed, NotificationState::Pending))
    );
    assert_eq!(rig.wd.notifications_due().len(), 1);
}

/// Decision on eligibility: traffic seen after the inconclusive round means
/// the restart may have worked; a fault confirmed after that does not make
/// the first round a failed one. The next round decides.
#[test]
fn a_success_after_an_inconclusive_round_leaves_it_to_the_next_round() {
    let mut rig = Rig::new(&["a"]);
    to_verifying(&mut rig);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::PermissionDenied));
    while rig.status("a").recovery == RecoveryPhase::Verifying {
        step(&mut rig);
        assert!(rig.now_secs() < 900);
    }
    rig.lab().fail_probes("a", None);
    rig.lab().heal("a");
    while rig.status("a").light != Light::Green {
        step(&mut rig);
        assert!(rig.now_secs() < 1200, "never healthy again");
    }
    // Dead again before the incident could close.
    rig.lab().break_for_good("a");
    let stops = rig.stops("a");
    while rig.stops("a") == stops {
        step(&mut rig);
        assert!(rig.now_secs() < 2400, "no second round");
        let state = incident(&mut rig).expect("still the same incident");
        assert_eq!(
            state,
            (FirstRecovery::InProgress, NotificationState::NotEligible),
            "counted as failed despite the traffic seen after the round"
        );
    }
    // The second round's verification gets no reply: now it failed.
    rig.run(120);
    assert_eq!(
        incident(&mut rig),
        Some((FirstRecovery::Failed, NotificationState::Pending))
    );
}

/// P3-3: a user's connect that replaces the pending retry and fails leaves
/// the alert due and does not end the automatic retries.
#[test]
fn p3_3_a_failed_connect_during_an_incident_keeps_the_retries_and_the_alert() {
    let mut rig = Rig::new(&["a"]);
    left_down_after_a_failed_round(&mut rig);
    assert_eq!(rig.wd.notifications_due().len(), 1);
    set_os(&rig, OsState::Transitioning);
    rig.run(100);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Start,
        AdapterError::new(AdapterErrorKind::ManagerConflict, "busy"),
    );
    let starts = rig.starts("a");
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(5);
    assert_eq!(rig.starts("a"), starts + 1, "the user's start was refused");
    assert_eq!(rig.status("a").light_reason, LightReason::ControlFailed);
    set_os(&rig, OsState::Disconnected);
    // The alert of the failed first round is still due.
    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1, "{due:?}");
    assert_eq!(due[0].tunnel_id, id("a"));
    assert!(persisted_retry(&rig).is_some());
    rig.run(600);
    assert!(
        rig.starts("a") > starts + 1,
        "the automatic retries ended with the user's failed connect"
    );
}

/// P3-3, the scope of the change: a connect that fails outside any incident
/// stays the user's own failure; nothing retries it automatically.
#[test]
fn p3_3_a_failed_connect_without_an_incident_is_not_retried() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(20);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Start,
        AdapterError::new(AdapterErrorKind::ManagerConflict, "busy"),
    );
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(600);
    assert_eq!(rig.starts("a"), 1);
    let status = rig.status("a");
    assert_eq!(status.light_reason, LightReason::ControlFailed);
    assert!(status.incident.is_none());
    // No retry is scheduled: recovery waits for a confirmed fault.
    assert_eq!(status.recovery, RecoveryPhase::Blocked);
    assert_eq!(status.next_retry_unix_ms, None);
}
