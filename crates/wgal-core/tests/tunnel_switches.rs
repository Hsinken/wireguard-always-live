//! Wave three's per-tunnel switches and unregistering (owner requests of
//! 2026-11-03): pausing one tunnel's monitoring, its notification switch,
//! and forgetting a registration. Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Rig, binding, icmp, id, timing};
use wgal_core::{AdapterError, NotificationOutcome, execute};
use wgal_protocol::{
    AdapterErrorKind, DesiredState, ErrorCode, FirstRecovery, Light, LightReason, LogEvent,
    NotificationState, OsState, Outcome, ProbeTiming, Request, Takeover,
};
use wgal_testkit::Operation;

fn monitor(rig: &mut Rig, name: &str, enabled: bool) {
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id(name),
        enabled,
    });
}

fn notify(rig: &mut Rig, name: &str, enabled: bool) {
    rig.ok(Request::SetTunnelNotifications {
        tunnel_id: id(name),
        enabled,
    });
}

fn unregister(rig: &mut Rig, name: &str) -> Outcome {
    rig.request(Request::UnregisterTunnel {
        tunnel_id: id(name),
    })
}

fn refused(outcome: &Outcome) -> Option<ErrorCode> {
    match outcome {
        Outcome::Err(e) => Some(e.code),
        Outcome::Ok(_) => None,
    }
}

fn notification(rig: &mut Rig, name: &str) -> Option<NotificationState> {
    rig.status(name).incident.map(|i| i.notification)
}

// ---------------------------------------------------------------------------
// Pausing one tunnel's monitoring

/// The owner asked to confirm that probing costs something only for
/// monitored tunnels: a paused tunnel gets no call of any kind, not even a
/// state read, while the others go on. Resuming starts with a grace period
/// and a new streak, as the host-wide resume does.
#[test]
fn a_paused_tunnel_costs_nothing_and_the_others_go_on() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    monitor(&mut rig, "a", false);
    assert_eq!(rig.events_of("a"), [LogEvent::MonitoringPaused]);
    assert_eq!(rig.wd.monitored_count(), 1);
    assert_eq!(rig.wd.monitoring_load(), [(ProbeTiming::DEFAULT, 1)]);

    rig.lab().break_for_good("a");
    let calls = rig.adapter_calls("a");
    let probes_b = rig.probes("b");
    rig.run(3600);
    assert_eq!(rig.adapter_calls("a"), calls, "no probe, no state read");
    assert!(rig.probes("b") >= probes_b + 239, "b is still probed");
    let status = rig.status("a");
    assert_eq!(status.light, Light::Gray);
    assert_eq!(status.light_reason, LightReason::MonitoringOff);
    assert!(!status.intent.monitoring_enabled);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.incident, None);
    assert_eq!(rig.stops("a"), 0);
    assert!(rig.wd.notifications_due().is_empty());
    assert!(rig.events_of("a").is_empty());

    monitor(&mut rig, "a", true);
    let resumed = rig.now_secs();
    assert_eq!(rig.events_of("a"), [LogEvent::MonitoringResumed]);
    assert_eq!(rig.wd.monitored_count(), 2);
    while rig.stops("a") == 0 {
        rig.run(1);
        assert!(rig.now_secs() < resumed + 400, "never recovered");
    }
    // Grace (30 s), then 3 failed rounds over at least 60 s.
    assert!(rig.now_secs() >= resumed + 30 + 60);
    let failed = rig
        .events_of("a")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::ProbeFailed { .. }))
        .count();
    assert_eq!(failed, 5, "the rounds in the grace period do not count");
}

#[test]
fn monitoring_load_counts_monitored_tunnels_and_their_targets() {
    let mut rig = Rig::new(&["a", "b", "c"]);
    rig.set_targets("b", Some(icmp(2)), &[icmp(50), icmp(51)]);
    rig.set_probe("c", timing(60, 3, 1, 0));
    let slow = timing(60, 3, 1, 0);
    let default = ProbeTiming::DEFAULT;
    assert_eq!(rig.wd.monitored_count(), 3);
    assert_eq!(
        rig.wd.monitoring_load(),
        [(default, 1), (default, 3), (slow, 1)]
    );
    // Wanted down: its round only reads the state.
    rig.ok(Request::Disconnect { tunnel_id: id("c") });
    assert_eq!(rig.wd.monitoring_load()[2], (slow, 0));
    monitor(&mut rig, "a", false);
    assert_eq!(rig.wd.monitored_count(), 2);
    assert_eq!(rig.wd.monitoring_load(), [(default, 3), (slow, 0)]);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    assert_eq!(rig.wd.monitored_count(), 0);
    assert!(rig.wd.monitoring_load().is_empty());
    rig.ok(Request::SetMonitoringPaused { paused: false });
    assert_eq!(rig.wd.monitored_count(), 2);
}

#[test]
fn user_operations_still_work_while_a_tunnel_is_paused() {
    let mut rig = Rig::new(&["a"]);
    monitor(&mut rig, "a", false);
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(60);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert_eq!(rig.status("a").intent.desired, DesiredState::Disconnected);
    assert_eq!(rig.status("a").light_reason, LightReason::MonitoringOff);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(60);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(60);
    assert_eq!((rig.stops("a"), rig.starts("a")), (2, 2));
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

#[test]
fn a_tunnel_pause_that_cannot_be_stored_is_refused() {
    let mut rig = Rig::new(&["a"]);
    rig.events();
    rig.store.set_failing(true);
    let outcome = rig.request(Request::SetTunnelMonitoring {
        tunnel_id: id("a"),
        enabled: false,
    });
    assert_eq!(refused(&outcome), Some(ErrorCode::StorageUnavailable));
    let status = rig.status("a");
    assert!(status.intent.monitoring_enabled);
    assert_ne!(status.light_reason, LightReason::MonitoringOff);
    assert_eq!(rig.wd.monitored_count(), 1);
    assert!(rig.events_of("a").is_empty());
}

// ---------------------------------------------------------------------------
// The tunnel's notification switch

fn until_alert_due(rig: &mut Rig) {
    while rig.wd.notifications_due().is_empty() {
        rig.run(1);
        assert!(rig.now_secs() < 600, "no alert became due");
    }
}

/// Off cancels the pending alert as the global switch does; on again never
/// brings it back.
#[test]
fn turning_a_tunnels_notifications_off_cancels_its_pending_alert() {
    let mut rig = Rig::new(&["a"]);
    assert!(rig.status("a").notifications_enabled, "on by default");
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    until_alert_due(&mut rig);
    notify(&mut rig, "a", false);
    assert!(rig.wd.notifications_due().is_empty());
    assert!(!rig.status("a").notifications_enabled);
    assert_eq!(
        notification(&mut rig, "a"),
        Some(NotificationState::Cancelled)
    );
    assert!(!rig.store.saved().unwrap().tunnels[0].notifications_enabled);

    notify(&mut rig, "a", true);
    rig.run(600);
    assert!(rig.wd.notifications_due().is_empty(), "never re-armed");
    assert_eq!(
        notification(&mut rig, "a"),
        Some(NotificationState::Cancelled)
    );
}

/// An alert already being sent keeps its result, but is never eligible
/// again.
#[test]
fn an_alert_in_flight_is_cancelled_when_its_result_comes_back() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    until_alert_due(&mut rig);
    let due = rig.wd.notifications_due()[0].clone();
    let ticket = rig
        .wd
        .reserve_notification(&due.tunnel_id, &due.incident_id, due.kind)
        .unwrap();
    notify(&mut rig, "a", false);
    let incident = rig.store.saved().unwrap().tunnels[0]
        .incident
        .clone()
        .unwrap();
    assert_eq!(incident.notification, NotificationState::Reserved);
    assert!(incident.cancel_requested);
    rig.wd
        .record_notification(&ticket, NotificationOutcome::RetryLater)
        .unwrap();
    assert_eq!(
        notification(&mut rig, "a"),
        Some(NotificationState::Cancelled)
    );
}

/// Integration owner, 2026-11-03: an alert that becomes due while the
/// tunnel's switch is already off is cancelled at once, as with the global
/// switch off; another tunnel's alert is not touched.
#[test]
fn an_alert_due_while_the_tunnels_switch_is_off_is_cancelled_at_once() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.manage("b");
    notify(&mut rig, "a", false);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.lab().break_for_good("b");
    let failed = |rig: &mut Rig, name: &str| {
        rig.status(name).incident.map(|i| i.first_recovery) == Some(FirstRecovery::Failed)
    };
    while !(failed(&mut rig, "a") && failed(&mut rig, "b")) {
        rig.run(1);
        assert!(rig.now_secs() < 600, "the first rounds never failed");
        assert_ne!(
            notification(&mut rig, "a"),
            Some(NotificationState::Pending),
            "never pending"
        );
    }
    assert_eq!(
        notification(&mut rig, "a"),
        Some(NotificationState::Cancelled)
    );
    let due: Vec<_> = rig
        .wd
        .notifications_due()
        .into_iter()
        .map(|d| d.tunnel_id)
        .collect();
    assert_eq!(due, [id("b")]);
    notify(&mut rig, "a", true);
    rig.run(300);
    let due: Vec<_> = rig
        .wd
        .notifications_due()
        .into_iter()
        .map(|d| d.tunnel_id)
        .collect();
    assert_eq!(due, [id("b")], "a's alert never comes back");
}

#[test]
fn a_notification_switch_that_cannot_be_stored_is_refused() {
    let mut rig = Rig::new(&["a"]);
    rig.store.set_failing(true);
    let outcome = rig.request(Request::SetTunnelNotifications {
        tunnel_id: id("a"),
        enabled: false,
    });
    assert_eq!(refused(&outcome), Some(ErrorCode::StorageUnavailable));
    assert!(rig.status("a").notifications_enabled);
}

// ---------------------------------------------------------------------------
// Unregistering

#[test]
fn unregistering_forgets_the_tunnel_and_sends_nothing_to_the_platform() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    let calls = rig.lab().fake.calls().len();
    assert!(matches!(unregister(&mut rig, "a"), Outcome::Ok(_)));
    assert_eq!(rig.lab().fake.calls().len(), calls, "nothing sent");
    let names: Vec<_> = rig
        .wd
        .snapshot()
        .tunnels
        .iter()
        .map(|t| t.tunnel_id.clone())
        .collect();
    assert_eq!(names, [id("b")]);
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels.len(), 1);
    assert_eq!(saved.tunnels[0].tunnel_id, id("b"));
    assert_eq!(rig.wd.tunnel_count(), 1);
    assert_eq!(rig.wd.registered_binding(&id("a")), None);

    let a_calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(rig.adapter_calls("a"), a_calls, "never read again");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
    let outcome = rig.request(Request::Connect { tunnel_id: id("a") });
    assert_eq!(refused(&outcome), Some(ErrorCode::UnknownTunnel));
    assert_eq!(
        refused(&unregister(&mut rig, "a")),
        Some(ErrorCode::UnknownTunnel)
    );
}

/// Refused with `busy` while WGAL has something unfinished on the tunnel:
/// an operation waiting for the control slot or under way, a stop whose
/// result is not back, a disconnect whose stop has to be tried again.
/// Accepted once it is over.
#[test]
fn unregistering_waits_until_nothing_is_under_way() {
    let busy = Some(ErrorCode::Busy);
    let mut rig = Rig::new(&["a", "b", "c"]);

    // A disconnect waiting for the slot, then its stop out.
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    assert_eq!(refused(&unregister(&mut rig, "a")), busy, "queued");
    let stop = rig.wd.plan().into_iter().next().unwrap();
    assert!(rig.wd.still_due(&stop));
    assert_eq!(refused(&unregister(&mut rig, "a")), busy, "stop out");
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &stop);
    rig.wd.apply(result);
    rig.run(5);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    assert!(matches!(unregister(&mut rig, "a"), Outcome::Ok(_)));

    // A disconnect whose stop failed and will be tried again.
    rig.lab().fake.fail_next(
        &binding("b"),
        Operation::Stop,
        AdapterError::new(AdapterErrorKind::PermissionDenied, "refused"),
    );
    rig.ok(Request::Disconnect { tunnel_id: id("b") });
    rig.run(1);
    assert_eq!(rig.stops("b"), 1);
    assert_eq!(refused(&unregister(&mut rig, "b")), busy, "retry owed");
    rig.run(10);
    assert_eq!(
        rig.lab().fake.os_state(&binding("b")),
        OsState::Disconnected
    );
    assert!(matches!(unregister(&mut rig, "b"), Outcome::Ok(_)));

    // A connect being verified.
    rig.lab()
        .fake
        .set_os_state(&binding("c"), OsState::Disconnected);
    rig.run(15);
    rig.ok(Request::Connect { tunnel_id: id("c") });
    let mut verifying = false;
    for _ in 0..4 {
        for item in rig.wd.plan() {
            assert!(rig.wd.still_due(&item));
            let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
            rig.wd.apply(result);
        }
        if rig.status("c").light_reason == LightReason::Verifying {
            verifying = true;
            break;
        }
    }
    assert!(verifying, "the connect never reached its verification");
    assert_eq!(refused(&unregister(&mut rig, "c")), busy, "verifying");
    rig.run(10);
    assert!(matches!(unregister(&mut rig, "c"), Outcome::Ok(_)));
    assert_eq!(rig.wd.tunnel_count(), 0);
}

#[test]
fn unregistering_that_cannot_be_stored_keeps_everything() {
    let mut rig = Rig::new(&["a"]);
    rig.events();
    rig.store.set_failing(true);
    assert_eq!(
        refused(&unregister(&mut rig, "a")),
        Some(ErrorCode::StorageUnavailable)
    );
    assert_eq!(rig.wd.tunnel_count(), 1);
    assert!(rig.wd.log_label(&id("a")).is_some());
    assert!(rig.events_of("a").is_empty(), "nothing was removed");
    rig.store.set_failing(false);
    rig.run(20);
    assert_eq!(rig.status("a").light, Light::Green, "monitoring went on");
}

/// Registering it again later is a fresh registration: observe-only, the
/// default settings, a new log label.
#[test]
fn a_tunnel_registered_again_starts_afresh() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", timing(30, 3, 4, 90));
    notify(&mut rig, "a", false);
    monitor(&mut rig, "a", false);
    let old_label = rig.wd.log_label(&id("a")).unwrap().to_owned();
    assert!(matches!(unregister(&mut rig, "a"), Outcome::Ok(_)));
    rig.ok(Request::RegisterTunnel { tunnel_id: id("a") });
    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::ObserveOnly);
    assert!(!status.intent.auto_recovery_enabled);
    assert!(status.intent.monitoring_enabled);
    assert!(status.notifications_enabled);
    assert_eq!(status.probe, ProbeTiming::DEFAULT);
    assert_eq!(status.recovery_target, None);
    assert_eq!(status.incident, None);
    assert_ne!(rig.wd.log_label(&id("a")).unwrap(), old_label);
}

/// A round still out when the tunnel was unregistered is dropped when it
/// comes back, even after the tunnel was registered again under the same
/// ID: sequence numbers are never reused.
#[test]
fn a_round_out_for_an_unregistered_tunnel_is_dropped() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    let old = rig.wd.plan().into_iter().next().unwrap();
    assert!(rig.wd.still_due(&old));
    let old_result = execute(rig.wd.adapter(), rig.wd.clock(), &old);
    assert!(
        matches!(unregister(&mut rig, "a"), Outcome::Ok(_)),
        "a round out is nothing unfinished"
    );
    rig.ok(Request::RegisterTunnel { tunnel_id: id("a") });
    rig.set_targets("a", Some(icmp(1)), &[]);
    let new = rig.wd.plan().into_iter().next().unwrap();
    assert_ne!(new.seq, old.seq);
    assert!(rig.wd.still_due(&new));
    rig.wd.apply(old_result);
    assert!(!rig.status("a").auto_recovery_allowed, "dropped");
    let new_result = execute(rig.wd.adapter(), rig.wd.clock(), &new);
    rig.wd.apply(new_result);
    assert!(
        rig.status("a").auto_recovery_allowed,
        "the new round still counted: its target passed the baseline"
    );
}

/// Integration owner, 2026-11-03: the runtime looks labels up right after
/// taking the events, so an unregistered tunnel's label answers until the
/// `take_events` call after the one that handed over its `tunnel_removed`.
#[test]
fn an_unregistered_tunnels_label_answers_until_the_next_take_events() {
    let mut rig = Rig::new(&["a", "b"]);
    let label = rig.wd.log_label(&id("a")).unwrap().to_owned();
    rig.events();
    assert!(matches!(unregister(&mut rig, "a"), Outcome::Ok(_)));
    assert_eq!(rig.wd.log_label(&id("a")), Some(label.as_str()));
    let events = rig.events();
    assert_eq!(events, [(Some(id("a")), LogEvent::TunnelRemoved)]);
    assert_eq!(
        rig.wd.log_label(&id("a")),
        Some(label.as_str()),
        "right after taking the events"
    );
    rig.events();
    assert_eq!(rig.wd.log_label(&id("a")), None, "forgotten now");
    assert!(rig.wd.log_label(&id("b")).is_some());
}

/// Registered again before the events were taken: the ID names the new
/// registration, with its own label.
#[test]
fn a_tunnel_registered_again_before_the_events_are_taken_has_its_new_label() {
    let mut rig = Rig::new(&["a"]);
    let old = rig.wd.log_label(&id("a")).unwrap().to_owned();
    assert!(matches!(unregister(&mut rig, "a"), Outcome::Ok(_)));
    rig.ok(Request::RegisterTunnel { tunnel_id: id("a") });
    let new = rig.wd.log_label(&id("a")).unwrap().to_owned();
    assert_ne!(new, old);
    assert!(
        rig.events()
            .contains(&(Some(id("a")), LogEvent::TunnelRemoved))
    );
    rig.events();
    assert_eq!(rig.wd.log_label(&id("a")), Some(new.as_str()));
}
