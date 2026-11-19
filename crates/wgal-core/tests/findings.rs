//! Regression tests for the WGW-001 wave-1 review of `bd57f48`. Each
//! test is named after the finding it covers and drives the watchdog with the
//! fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Lab, Rig, binding, icmp, id};
use wgal_core::{
    AdapterError, Clock, MemoryStore, NotificationOutcome, PersistedState, Startup, Timing,
    Watchdog, execute,
};
use wgal_protocol::{
    AdapterErrorKind, DataHealth, ErrorCode, FirstRecovery, Light, LightReason, NotificationState,
    OsState, Outcome, RecoveryPhase, Request, StorageHealth, Takeover,
};
use wgal_testkit::{Call, FakeClock, Operation};

type Wd = Watchdog<Lab, FakeClock>;

/// One second, then a single plan/execute/apply pass, so a test can stop
/// between steps that `tick` would run back to back.
fn step(wd: &mut Wd) {
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = wd.plan();
    for item in items {
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

fn restart(lab: Lab, startup: Startup) -> (Wd, MemoryStore) {
    let store = MemoryStore::new();
    let wd = Watchdog::new(
        lab,
        FakeClock::new(support::T0 + 600_000),
        Timing::default(),
        Box::new(store.clone()),
        startup,
    );
    (wd, store)
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

fn first(wd: &mut Wd) -> wgal_protocol::TunnelStatus {
    wd.snapshot().tunnels[0].clone()
}

/// A managed tunnel whose first recovery round failed at t = 150: the
/// incident's notification is pending.
fn failed_first_round() -> Rig {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::Failed);
    assert_eq!(incident.notification, NotificationState::Pending);
    rig
}

/// Finding 3: the newest state was lost after a reservation; the backup
/// still says `pending`. Restored from it, the alert can never be sent again
/// and nothing automatic acts on the possibly outdated intent.
#[test]
fn f03_backup_restore_never_resends_or_acts_on_uncertain_intent() {
    let mut rig = failed_first_round();
    let backup = rig.store.saved().unwrap();
    let due = rig.wd.notifications_due();
    rig.wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();

    let lab = Lab::new(&["a"]);
    lab.break_for_good("a");
    let (mut wd, store) = restart(lab, Startup::RestoredFromBackup(backup.clone()));
    let a = first(&mut wd);
    assert_eq!(
        a.incident.as_ref().unwrap().notification,
        NotificationState::Unknown
    );
    assert!(!a.intent.auto_recovery_enabled);
    assert!(!a.intent.start_when_stopped_externally);
    assert_eq!(a.takeover, Takeover::ObserveOnly);
    let stored = store.saved().unwrap();
    assert!(stored.next_incident > backup.next_incident + 999);
    assert_eq!(
        stored.tunnels[0].incident.as_ref().unwrap().notification,
        NotificationState::Unknown,
        "the conservative state is what is stored now"
    );

    run(&mut wd, 900);
    assert!(wd.notifications_due().is_empty());
    assert_eq!(calls(&wd, "a", is_stop), 0, "no automatic recovery");
    assert_eq!(calls(&wd, "a", is_start), 0);
}

/// Finding 3 for the messages added in 1.2.0: restored from a backup, a
/// `retries_exhausted` or `recovered` message that may already have gone
/// out is never sent.
#[test]
fn f03_backup_restore_never_resends_the_later_messages() {
    let rig = failed_first_round();
    let mut backup = rig.store.saved().unwrap();
    let tunnel = &mut backup.tunnels[0];
    let incident = tunnel.incident.as_mut().unwrap();
    incident.exhausted_notice.state = NotificationState::Pending;
    tunnel.recovered_notices.push(wgal_core::RecoveredNotice {
        incident_id: wgal_protocol::IncidentId::new("inc-000999").unwrap(),
        opened_at_unix_ms: support::T0,
        recovered_at_unix_ms: support::T0 + 60_000,
        notice: wgal_core::NoticeRecord {
            state: NotificationState::Reserved,
            ..wgal_core::NoticeRecord::default()
        },
    });

    let lab = Lab::new(&["a"]);
    lab.break_for_good("a");
    let (mut wd, store) = restart(lab, Startup::RestoredFromBackup(backup));
    let stored = store.saved().unwrap();
    let tunnel = &stored.tunnels[0];
    assert_eq!(
        tunnel.incident.as_ref().unwrap().exhausted_notice.state,
        NotificationState::Unknown
    );
    assert_eq!(
        tunnel.recovered_notices[0].notice.state,
        NotificationState::Unknown
    );
    run(&mut wd, 900);
    assert!(wd.notifications_due().is_empty());
}

/// Finding 4: recovery queued while storage worked does not start after a
/// later save (for another tunnel) fails.
#[test]
fn f04_queued_recovery_does_not_start_after_storage_fails() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let mut queued = false;
    for _ in 0..200 {
        step(&mut rig.wd);
        if rig.status("a").recovery == RecoveryPhase::Queued {
            queued = true;
            break;
        }
    }
    assert!(queued, "A's recovery was queued");
    assert_eq!(rig.stops("a"), 0);

    rig.store.set_failing(true);
    let outcome = rig.request(Request::SetStartWhenStoppedExternally {
        tunnel_id: id("b"),
        enabled: true,
    });
    assert!(matches!(outcome, Outcome::Err(e) if e.code == ErrorCode::StorageUnavailable));
    rig.run(120);
    assert_eq!(rig.wd.snapshot().storage, StorageHealth::Failed);
    assert_eq!(rig.stops("a"), 0, "no automatic stop without storage");
    assert_eq!(rig.starts("a"), 0, "no automatic start without storage");
}

/// Finding 6: once the round finds the interface rebuilt, the green proven
/// through the old interface is gone at once; the new one is checked soon.
#[test]
fn f06_rebuilt_interface_drops_green_immediately() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    assert_eq!(rig.status("a").light, Light::Green);
    rig.lab().rebuild_after_next_probe("a");
    rig.run_until(45);
    let status = rig.status("a");
    assert_ne!(status.light, Light::Green);
    assert_eq!(status.data_health, DataHealth::Unknown);
    rig.run(5);
    assert_eq!(
        rig.status("a").light,
        Light::Green,
        "verified again through the new interface"
    );
}

/// Finding 7: a successful round applied 36 s after it ran is too old to
/// make the tunnel healthy.
#[test]
fn f07_success_applied_late_is_stale() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(36)).unwrap();
    rig.wd.apply(result);
    let status = rig.status("a");
    assert_ne!(status.data_health, DataHealth::Healthy);
    assert_ne!(status.light, Light::Green);
}

/// Finding 7: a success applied 30 s late still counts, but it is dated by
/// the probe, so it goes stale 35 s after the probe, not after the apply.
#[test]
fn f07_late_success_is_dated_by_the_probe() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = rig.wd.plan();
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(30)).unwrap();
    rig.wd.apply(result);
    assert_eq!(rig.status("a").data_health, DataHealth::Healthy);
    rig.wd.clock().advance(Duration::from_secs(10)).unwrap();
    assert_eq!(
        rig.status("a").data_health,
        DataHealth::Stale,
        "40 s after the probe"
    );
}

/// Finding 7: a failed round applied too late is no evidence either; it is
/// not counted towards a fault.
#[test]
fn f07_failure_applied_late_is_not_counted() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(36)).unwrap();
    rig.wd.apply(result);
    assert_ne!(rig.status("a").data_health, DataHealth::Suspect);
}

/// Finding 7: the recovery target answered early, but slow observe probes
/// delivered the round 40 s later. The answer is judged by when it came.
#[test]
fn f07_slow_observe_probes_do_not_refresh_an_old_reply() {
    let mut rig = Rig::new(&["a"]);
    rig.set_targets("a", Some(icmp(1)), &[icmp(9)]);
    rig.run_until(80);
    assert_eq!(rig.status("a").light, Light::Green);
    let items = loop {
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
        let items = rig.wd.plan();
        if !items.is_empty() {
            break items;
        }
    };
    let mut result = execute(rig.wd.adapter(), rig.wd.clock(), &items[0]);
    rig.wd.clock().advance(Duration::from_secs(40)).unwrap();
    let now = rig.wd.clock().now();
    if let wgal_core::work::WorkOutcome::Round {
        probes: Some(set), ..
    } = &mut result.outcome
    {
        for probe in &mut set.probes {
            if probe.role != wgal_core::work::TargetRole::Recovery {
                probe.finished = now;
            }
        }
    }
    result.finished = now;
    rig.wd.apply(result);
    assert_ne!(rig.status("a").data_health, DataHealth::Healthy);
}

/// Finding 8: the target's route moved to B while A's probe ran. The reply
/// proves nothing about A, so A is not green and the conflict is shown.
#[test]
fn f08_route_moving_during_the_probe_is_a_conflict() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.run_until(44);
    assert_eq!(rig.status("a").light, Light::Green);
    rig.lab().reroute_during_next_probe("a", 1, "b");
    rig.run_until(45);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::RouteConflict);
    assert_eq!(status.fault.unwrap().kind, AdapterErrorKind::RouteMismatch);
}

/// Finding 9: the watchdog died after its recovery stopped the tunnel and
/// before it started it. The new process finishes the round.
#[test]
fn f09_restart_after_an_acknowledged_stop_finishes_the_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    let mut stopped = false;
    for _ in 0..200 {
        step(&mut rig.wd);
        if rig
            .calls("a")
            .last()
            .is_some_and(|c| matches!(c, Call::Stop(_)))
        {
            stopped = true;
            break;
        }
    }
    assert!(stopped);
    assert_eq!(rig.starts("a"), 0);
    let saved = rig.store.saved().unwrap();

    let lab = Lab::new(&["a"]);
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let (mut wd, _) = restart(lab, Startup::Restored(saved));
    run(&mut wd, 60);
    assert_eq!(calls(&wd, "a", is_start), 1, "the round is finished");
    assert_eq!(
        calls(&wd, "a", is_stop),
        0,
        "a stopped tunnel is not stopped again"
    );
    let a = first(&mut wd);
    assert_eq!(a.light, Light::Green);
    assert_eq!(a.incident.unwrap().first_recovery, FirstRecovery::Succeeded);
}

/// Finding 9: the recovery's start failed and left the tunnel down; after a
/// restart the backoff runs out and the next round starts it, without ever
/// reporting a retry time that has passed.
#[test]
fn f09_restart_after_a_failed_start_continues_the_backoff() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Start,
        AdapterError::new(AdapterErrorKind::PermissionDenied, "start refused"),
    );
    rig.run_until(110);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Backoff);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    let saved = rig.store.saved().unwrap();

    let lab = Lab::new(&["a"]);
    lab.fake.set_os_state(&binding("a"), OsState::Disconnected);
    let (mut wd, _) = restart(lab, Startup::Restored(saved));
    let mut green = false;
    for _ in 0..180 {
        run(&mut wd, 1);
        let snapshot = wd.snapshot();
        let a = &snapshot.tunnels[0];
        if let Some(retry) = a.next_retry_unix_ms {
            assert!(
                retry >= snapshot.generated_at_unix_ms,
                "a retry time that has passed is never shown"
            );
        }
        if a.light == Light::Green {
            green = true;
            break;
        }
    }
    assert!(green, "the tunnel was started again and verified");
    assert_eq!(calls(&wd, "a", is_start), 1);
}

/// Finding 10: a pending alert restored while the tunnel was recovering is
/// not sent before fresh evidence that the fault goes on, and a healthy
/// probe keeps deferring it.
#[test]
fn f10_restored_pending_alert_waits_for_fresh_fault_evidence() {
    let mut rig = failed_first_round();
    rig.lab().heal("a");
    rig.run(30);
    assert!(
        rig.wd.notifications_due().is_empty(),
        "deferred while stable"
    );
    let saved = rig.store.saved().unwrap();

    let (mut wd, _) = restart(Lab::new(&["a"]), Startup::Restored(saved));
    assert!(
        wd.notifications_due().is_empty(),
        "nothing due on restored state"
    );
    run(&mut wd, 20);
    assert!(
        wd.notifications_due().is_empty(),
        "still deferred after a healthy probe"
    );

    wd.adapter().break_for_good("a");
    run(&mut wd, 40);
    assert_eq!(
        wd.notifications_due().len(),
        1,
        "due again once a failure is counted"
    );
}

/// Finding 11: a disconnect during a send cancels the alert for good, even
/// if the send then reports it may be retried.
#[test]
fn f11_disconnect_during_a_send_cancels_it_for_good() {
    let mut rig = failed_first_round();
    let due = rig.wd.notifications_due();
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.wd
        .record_notification(&ticket, NotificationOutcome::RetryLater)
        .unwrap();
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(300);
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(incident.notification, NotificationState::Cancelled);
    assert!(rig.wd.notifications_due().is_empty());
}

/// Finding 11: turning notifications off during a send does the same.
#[test]
fn f11_turning_notifications_off_during_a_send_cancels_it_for_good() {
    let mut rig = failed_first_round();
    let due = rig.wd.notifications_due();
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.wd.cancel_pending_notifications().unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::RetryLater)
        .unwrap();
    rig.run(300);
    assert_eq!(
        rig.status("a").incident.unwrap().notification,
        NotificationState::Cancelled
    );
    assert!(rig.wd.notifications_due().is_empty());
}

/// Finding 12: an external stop polled as connected, transitioning,
/// disconnected is still an external stop.
#[test]
fn f12_external_stop_through_transitioning_is_noticed() {
    for restart_option in [false, true] {
        let mut rig = Rig::new(&["a"]);
        rig.manage("a");
        rig.ok(Request::SetStartWhenStoppedExternally {
            tunnel_id: id("a"),
            enabled: restart_option,
        });
        rig.run_until(44);
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Transitioning);
        rig.run_until(59);
        rig.lab()
            .fake
            .set_os_state(&binding("a"), OsState::Disconnected);
        rig.run_until(62);
        if restart_option {
            assert_eq!(rig.starts("a"), 1, "started again, as allowed");
            assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
        } else {
            assert_eq!(rig.status("a").takeover, Takeover::StoppedExternally);
            assert_eq!(rig.starts("a"), 0);
        }
    }
}

/// Finding 13: the host slept through a recovery's verification. The sleep
/// is no failed verification: after the wake, verification gets its full
/// window again, and no alert becomes pending because of the sleep.
#[test]
fn f13_sleep_during_verification_is_not_a_failed_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(110);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Verifying);

    rig.wd.clock().advance(Duration::from_secs(120)).unwrap();
    rig.wd.on_wake();
    rig.run(20);
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::InProgress);
    assert_eq!(incident.notification, NotificationState::NotEligible);

    rig.lab().heal("a");
    rig.run(10);
    let status = rig.status("a");
    assert_eq!(
        status.incident.unwrap().first_recovery,
        FirstRecovery::Succeeded
    );
    assert!(rig.wd.notifications_due().is_empty());
}

/// Finding 14: a tunnel stuck transitioning is yellow only until the phase
/// deadline, then a visible timeout.
#[test]
fn f14_transitioning_past_its_deadline_is_a_visible_timeout() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.run_until(46);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Yellow);
    assert!(status.deadline_unix_ms.is_some());
    rig.run_until(76);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::StatusUnknown);
    assert_eq!(status.fault.unwrap().kind, AdapterErrorKind::Timeout);
}

#[test]
fn backup_restore_keeps_settled_notifications() {
    // The conservative restore only touches what is not settled.
    let mut rig = failed_first_round();
    let due = rig.wd.notifications_due();
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 5 })
        .unwrap();
    let state: PersistedState = rig.store.saved().unwrap();
    let (mut wd, _) = restart(Lab::new(&["a"]), Startup::RestoredFromBackup(state));
    assert_eq!(
        first(&mut wd).incident.unwrap().notification,
        NotificationState::Sent
    );
}

fn unsafe_config() -> Result<(), AdapterError> {
    Err(AdapterError::new(
        AdapterErrorKind::PermissionDenied,
        "configuration is writable by an unprivileged user",
    ))
}

/// Finding 1: a tunnel whose configuration fails the registration check is
/// never registered, and nothing is stopped or started.
#[test]
fn f01_unsafe_configuration_is_never_registered() {
    let lab = Lab::new(&["a"]);
    lab.fake.set_registration(&binding("a"), unsafe_config());
    let (mut wd, _) = restart(lab, Startup::Empty);
    wd.handle(&Request::DiscoverTunnels);
    let outcome = wd.handle(&Request::RegisterTunnel { tunnel_id: id("a") });
    assert!(matches!(outcome, Outcome::Err(e) if e.code == ErrorCode::InvalidState));
    assert!(wd.snapshot().tunnels.is_empty());
    let log = wd.adapter().fake.calls_for(&binding("a"));
    assert!(log.contains(&Call::CheckRegistration(binding("a"))));
    assert_eq!(calls(&wd, "a", is_stop) + calls(&wd, "a", is_start), 0);
}

/// Finding 1: the configuration became unsafe after registration. A
/// reconnect is refused before any stop or start, and the reason is shown.
#[test]
fn f01_reconnect_is_refused_before_any_control_when_the_check_fails() {
    let mut rig = Rig::new(&["a"]);
    rig.lab()
        .fake
        .set_registration(&binding("a"), unsafe_config());
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(5);
    assert_eq!(rig.stops("a") + rig.starts("a"), 0);
    assert!(
        rig.calls("a")
            .contains(&Call::CheckRegistration(binding("a")))
    );
    let status = rig.status("a");
    assert_eq!(status.light_reason, LightReason::ControlFailed);
    assert_eq!(
        status.fault.unwrap().kind,
        AdapterErrorKind::PermissionDenied
    );
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

/// Finding 1: automatic recovery is held to the same check; a refused round
/// is reported as not executed and touches nothing.
#[test]
fn f01_recovery_is_refused_before_any_control_when_the_check_fails() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab()
        .fake
        .set_registration(&binding("a"), unsafe_config());
    rig.lab().break_for_good("a");
    rig.run_until(150);
    assert_eq!(rig.stops("a") + rig.starts("a"), 0);
    let incident = rig.status("a").incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::NotExecuted);
}

/// Finding 5: the interface was rebuilt in place during the round: same
/// name, index and address, new incarnation. The round is discarded and the
/// old green is gone.
#[test]
fn f05_rebuild_in_place_is_detected_by_incarnation() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    assert_eq!(rig.status("a").light, Light::Green);
    let before = rig.lab().fake.interface(&binding("a")).unwrap();
    rig.lab().rebuild_in_place_after_next_probe("a");
    rig.run_until(45);
    let after = rig.lab().fake.interface(&binding("a")).unwrap();
    assert_eq!(
        (&before.name, before.index, before.source),
        (&after.name, after.index, after.source)
    );
    assert_ne!(before.incarnation, after.incarnation);
    let status = rig.status("a");
    assert_ne!(status.light, Light::Green);
    assert_eq!(status.data_health, DataHealth::Unknown);
}

/// Finding 5: an observed OS state change starts a new operation generation
/// and voids what was proven through the interface before it.
#[test]
fn f05_os_state_change_advances_the_generation_and_voids_old_evidence() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    let generation = rig.store.saved().unwrap().tunnels[0].generation;
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Transitioning);
    rig.run_until(45);
    let status = rig.status("a");
    assert_eq!(status.os_state, OsState::Transitioning);
    assert_eq!(status.data_health, DataHealth::Unknown);
    assert!(rig.store.saved().unwrap().tunnels[0].generation > generation);
}

/// Finding 1: a connect, whose first command is a start, is refused the same
/// way.
#[test]
fn f01_connect_is_refused_before_any_start_when_the_check_fails() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(5);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    rig.lab()
        .fake
        .set_registration(&binding("a"), unsafe_config());
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(5);
    assert_eq!(rig.starts("a"), 0);
    assert_eq!(rig.status("a").light_reason, LightReason::ControlFailed);
}
