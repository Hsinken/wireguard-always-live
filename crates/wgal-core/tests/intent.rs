//! User intent, external control, broken probes, sleep, restarts and storage
//! failures (plan §6.2–§6.4, §9.1; T11, T12, T15–T17, T23, T25).

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Lab, Rig, binding, id};
use wgal_core::{
    AdapterError, MemoryStore, NotificationOutcome, Startup, Timing, Watchdog, WorkKind, execute,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, DataHealth, DesiredState, ErrorCode, Light, LightReason,
    NotificationState, OsState, Outcome, RecoveryPhase, Request, StorageHealth, Takeover,
};
use wgal_testkit::{Call, FakeClock, Operation};

fn rejected(outcome: Outcome) -> ErrorCode {
    match outcome {
        Outcome::Err(e) => e.code,
        Outcome::Ok(r) => panic!("expected a rejection, got {r:?}"),
    }
}

/// T11: a disconnect wins over a pending automatic recovery: the intent is
/// stored first, the pending notification is cancelled, and nothing starts
/// the tunnel again.
#[test]
fn disconnect_beats_pending_recovery_and_notification() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(160);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Backoff);
    assert_eq!(rig.wd.notifications_due().len(), 1);

    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    let saved = rig.store.saved().unwrap();
    assert_eq!(saved.tunnels[0].intent.desired, DesiredState::Disconnected);
    assert_eq!(
        saved.tunnels[0].incident.as_ref().unwrap().notification,
        NotificationState::Cancelled
    );
    assert!(rig.wd.notifications_due().is_empty());

    let starts = rig.starts("a");
    rig.run(3600);
    assert_eq!(rig.starts("a"), starts, "nothing started it again");
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    let status = rig.status("a");
    assert_eq!(status.light, Light::Gray);
    assert_eq!(status.light_reason, LightReason::DisconnectedByUser);
}

/// T11: a start planned before the disconnect is discarded, and even if a
/// runtime executes it anyway, the disconnect's stop follows it.
#[test]
fn start_planned_before_a_disconnect_is_followed_by_a_stop() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    // Plan until the start is the outstanding item.
    let start = loop {
        let items = rig.wd.plan();
        let item = items.into_iter().next().unwrap();
        if item.kind == WorkKind::Start {
            break item;
        }
        let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
        rig.wd.apply(result);
    };
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    let stale = execute(rig.wd.adapter(), rig.wd.clock(), &start);
    rig.wd.apply(stale);
    rig.run(120);

    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    let calls = rig.calls("a");
    let last_start = calls
        .iter()
        .rposition(|c| matches!(c, Call::Start(_)))
        .unwrap();
    let last_stop = calls
        .iter()
        .rposition(|c| matches!(c, Call::Stop(_)))
        .unwrap();
    assert!(last_stop > last_start, "the final action is a stop");
    assert_eq!(rig.starts("a"), 1, "no second start");
}

/// A probe planned before a disconnect is not credited afterwards.
#[test]
fn probe_results_from_an_older_generation_are_discarded() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let item = loop {
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
        if let Some(item) = rig.wd.plan().into_iter().next() {
            break item;
        }
    };
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.wd.apply(result);
    assert_eq!(rig.status("a").data_health, DataHealth::Healthy);
}

/// T12: a managed tunnel stopped outside WGAL is reported, not restarted.
#[test]
fn external_stop_is_reported_and_not_undone() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(1800);

    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::StoppedExternally);
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::StoppedExternally);
    assert_eq!(rig.starts("a"), 0);

    // The user takes it back.
    rig.ok(Request::Connect { tunnel_id: id("a") });
    rig.run(60);
    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::Managed);
    assert_eq!(status.light, Light::Green);
    assert_eq!(rig.starts("a"), 1);
}

#[test]
fn external_stop_is_undone_only_with_the_option_on() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::SetStartWhenStoppedExternally {
        tunnel_id: id("a"),
        enabled: true,
    });
    rig.run_until(44);
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(120);
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(rig.status("a").light, Light::Green);
}

/// T12: after the user disconnected, something else (On-Demand) starts the
/// tunnel. WGAL shows it, does not fight it, and does not probe through it.
#[test]
fn externally_started_tunnel_is_not_fought() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.run(30);
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );

    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Connected);
    let (stops, probes) = (rig.stops("a"), rig.probes("a"));
    rig.run(1800);
    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::ExternallyControlled);
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::ExternallyStarted);
    assert_eq!(
        status.os_state,
        OsState::Connected,
        "shown as the OS reports it"
    );
    assert_eq!(rig.stops("a"), stops, "no stop/start fighting");
    assert_eq!(
        rig.probes("a"),
        probes,
        "no probes that could trigger On-Demand"
    );
}

/// T23: a broken probe or an unreadable state is a capability problem, not a
/// dead target or a disconnected tunnel.
#[test]
fn broken_probe_is_not_a_target_failure() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::PermissionDenied));
    rig.run(900);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::ProbeUnavailable);
    assert_ne!(status.data_health, DataHealth::Failed);
    assert_eq!(status.incident, None);
    assert_eq!(
        status.fault.unwrap().kind,
        AdapterErrorKind::PermissionDenied
    );
    assert_eq!(rig.stops("a"), 0);
}

#[test]
fn unreadable_state_is_unknown_and_a_lost_binding_is_not_disconnected() {
    let mut rig = Rig::new(&["a", "b"]);
    for _ in 0..40 {
        rig.lab().fake.fail_next(
            &binding("a"),
            Operation::QueryState,
            AdapterError::new(AdapterErrorKind::ParseError, "unexpected output"),
        );
    }
    rig.run(20);
    let status = rig.status("a");
    assert_eq!(status.os_state, OsState::Unknown);
    assert_eq!(status.light_reason, LightReason::StatusUnknown);
    assert_eq!(status.fault.unwrap().kind, AdapterErrorKind::ParseError);

    rig.lab().fake.fail_next(
        &binding("b"),
        Operation::QueryState,
        AdapterError::new(AdapterErrorKind::NotFound, "service does not exist"),
    );
    rig.run(20);
    let status = rig.status("b");
    assert_eq!(status.takeover, Takeover::BindingLost);
    assert_eq!(status.light_reason, LightReason::BindingLost);
    assert_ne!(status.os_state, OsState::Disconnected);
    assert_eq!(
        rig.starts("b") + rig.stops("b"),
        0,
        "never recreated or restarted"
    );

    // Registering it again restores monitoring.
    rig.ok(Request::DiscoverTunnels);
    rig.ok(Request::RegisterTunnel { tunnel_id: id("b") });
    rig.run(20);
    assert_eq!(rig.status("b").light, Light::Green);
}

/// §5.2: a platform that cannot bind the probe to the interface never shows
/// green and never recovers automatically.
#[test]
fn unbound_probe_never_shows_green() {
    let lab = Lab::new(&["a"]);
    lab.fake.set_capabilities(AdapterCapabilities::UNKNOWN);
    let mut rig = Rig::with_lab(lab, &["a"]);
    rig.run(300);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::PathUnverified);
    assert_eq!(
        rejected(rig.request(Request::SetAutoRecovery {
            tunnel_id: id("a"),
            enabled: true,
        })),
        ErrorCode::InvalidState
    );
}

/// T15: time asleep is not counted as failures, and the grace period after
/// waking keeps the first failures from counting.
#[test]
fn sleep_and_wake_do_not_count_as_failures() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    rig.run_until(80);
    assert_eq!(rig.status("a").data_health, DataHealth::Suspect);

    // Ten minutes asleep: no ticks at all.
    rig.wd.clock().advance(Duration::from_secs(600)).unwrap();
    rig.wd.tick();
    let status = rig.status("a");
    assert_eq!(status.light_reason, LightReason::GracePeriod);
    assert_eq!(status.data_health, DataHealth::Stale);

    // A fresh streak starts after the grace period: 30 s of grace, then
    // three failures over 60 s.
    rig.run(85);
    assert_eq!(rig.stops("a"), 0);
    rig.run(10);
    assert_eq!(rig.stops("a"), 1);
}

#[test]
fn network_change_restarts_the_grace_period() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    rig.run_until(95);
    rig.wd.on_network_change();
    rig.run(60);
    assert_eq!(rig.stops("a"), 0, "the streak restarted after the change");
}

/// §5.2: the interface was rebuilt during the round, so the reply may have
/// come through something else and is ignored, and nothing proven through
/// the old interface still counts (review finding 6).
#[test]
fn round_with_a_rebuilt_interface_is_discarded() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.lab().rebuild_after_next_probe("a");
    rig.run_until(46);
    assert_eq!(
        rig.status("a").data_health,
        DataHealth::Unknown,
        "the failed probe through the rebuilt interface did not count,          and the old interface's success no longer does either"
    );
}

/// T16: with the local network down WGAL does not restart tunnels; once it
/// is back, a grace period passes before failures count again.
#[test]
fn local_network_offline_stops_restarts() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(1800);
    let status = rig.status("a");
    assert_eq!(status.light_reason, LightReason::LocalNetworkOffline);
    assert_eq!(rig.stops("a"), 0);

    rig.lab().fail_probes("a", None);
    rig.lab().break_until_restart("a");
    rig.run(80);
    assert_eq!(rig.stops("a"), 0, "grace after coming back online");
    rig.run(60);
    assert_eq!(rig.stops("a"), 1);
}

/// T17: intent, backoff, incident and notification state survive a restart
/// of the watchdog; a sent notification is not due again, and the next
/// round still waits for the backoff.
#[test]
fn restart_preserves_intent_backoff_incident_and_notification() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.ok(Request::Disconnect { tunnel_id: id("b") });
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);
    let due = rig.wd.notifications_due();
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 7 })
        .unwrap();
    let before = rig.status("a").incident.unwrap();
    let saved = rig.store.saved().unwrap();

    // A new process 10 s later, with the wall clock jumped back an hour. The
    // platform still has A broken and B stopped.
    let lab = Lab::new(&["a", "b"]);
    lab.break_for_good("a");
    lab.fake.set_os_state(&binding("b"), OsState::Disconnected);
    let clock = FakeClock::new(support::T0 + 160_000 - 3_600_000);
    let store = MemoryStore::new();
    let mut wd = Watchdog::new(
        lab,
        clock,
        Timing::default(),
        Box::new(store.clone()),
        Startup::Restored(saved),
    );
    // The next retry is reported with the red light, once a round has read
    // the tunnel's state again.
    wd.tick();
    let snapshot = wd.snapshot();
    let a = snapshot
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id("a"))
        .unwrap();
    let b = snapshot
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id("b"))
        .unwrap();
    assert_eq!(b.intent.desired, DesiredState::Disconnected);
    let incident = a.incident.clone().unwrap();
    assert_eq!(incident.incident_id, before.incident_id);
    assert_eq!(incident.notification, NotificationState::Sent);
    assert_eq!(a.recovery, RecoveryPhase::Backoff);
    let retry_in = a.next_retry_unix_ms.unwrap() - snapshot.generated_at_unix_ms;
    assert!(
        (0..=60_000).contains(&retry_in),
        "a backward wall jump never stretches the wait past the backoff: {retry_in}"
    );
    assert!(wd.notifications_due().is_empty());

    // No restart right away: the fault has to be confirmed again first, after
    // the startup grace period.
    for _ in 0..80 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
    let stops = |wd: &Watchdog<Lab, FakeClock>| {
        let calls = wd.adapter().fake.calls_for(&binding("a"));
        calls.iter().filter(|c| matches!(c, Call::Stop(_))).count()
    };
    assert_eq!(stops(&wd), 0);
    assert_eq!(
        wd.adapter()
            .fake
            .calls_for(&binding("b"))
            .iter()
            .filter(|c| matches!(c, Call::Start(_)))
            .count(),
        0
    );
    for _ in 0..60 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
    assert_eq!(stops(&wd), 1, "the next round, with its backoff step kept");
    let a = wd.snapshot().tunnels[0].clone();
    assert_eq!(a.incident.unwrap().backoff_step, 1);
}

/// T17: a notification reserved but not settled before a crash becomes
/// `unknown` and is never sent again automatically.
#[test]
fn reserved_notification_becomes_unknown_after_a_restart() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);
    let due = rig.wd.notifications_due();
    rig.wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    let saved = rig.store.saved().unwrap();

    let store = MemoryStore::new();
    let mut wd = Watchdog::new(
        Lab::new(&["a"]),
        FakeClock::new(support::T0),
        Timing::default(),
        Box::new(store.clone()),
        Startup::Restored(saved),
    );
    let a = wd.snapshot().tunnels[0].clone();
    assert_eq!(a.incident.unwrap().notification, NotificationState::Unknown);
    assert!(wd.notifications_due().is_empty());
    let stored = store.saved().unwrap();
    assert_eq!(
        stored.tunnels[0].incident.as_ref().unwrap().notification,
        NotificationState::Unknown
    );
}

/// T25: when intent cannot be stored the request is refused and nothing
/// changes; while storage is failed, monitoring continues but no automatic
/// recovery starts.
#[test]
fn storage_failure_fails_closed() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.store.set_failing(true);
    assert_eq!(
        rejected(rig.request(Request::Disconnect { tunnel_id: id("a") })),
        ErrorCode::StorageUnavailable
    );
    assert_eq!(rig.status("a").intent.desired, DesiredState::Connected);
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);

    rig.run_until(44);
    rig.lab().break_for_good("a");
    let probes = rig.probes("a");
    rig.run(600);
    let snapshot = rig.wd.snapshot();
    assert_eq!(snapshot.storage, StorageHealth::Failed);
    assert_eq!(rig.stops("a"), 0, "no automatic recovery without storage");
    assert!(rig.probes("a") > probes + 30, "monitoring continues");
    assert!(rig.wd.notifications_due().is_empty());

    rig.store.set_failing(false);
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    rig.ok(Request::SetStartWhenStoppedExternally {
        tunnel_id: id("a"),
        enabled: false,
    });
    assert_eq!(rig.wd.snapshot().storage, StorageHealth::Ok);
    rig.run(20);
    assert_eq!(rig.stops("a"), 1, "recovery resumes once storage works");
}

#[test]
fn unloadable_state_is_never_overwritten() {
    let store = MemoryStore::new();
    let mut wd = Watchdog::new(
        Lab::new(&["a"]),
        FakeClock::new(support::T0),
        Timing::default(),
        Box::new(store.clone()),
        Startup::StorageFailed,
    );
    assert_eq!(wd.snapshot().storage, StorageHealth::Failed);
    wd.handle(&Request::DiscoverTunnels);
    let outcome = wd.handle(&Request::RegisterTunnel { tunnel_id: id("a") });
    assert_eq!(rejected(outcome), ErrorCode::StorageUnavailable);
    assert_eq!(store.saved(), None);
}

#[test]
fn registration_never_touches_the_tunnel_and_starts_observe_only() {
    let mut rig = Rig::new(&["a"]);
    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::ObserveOnly);
    assert!(!status.intent.auto_recovery_enabled);
    assert!(!status.intent.start_when_stopped_externally);
    assert_eq!(rig.starts("a") + rig.stops("a"), 0);
    assert_eq!(
        rejected(rig.request(Request::RegisterTunnel {
            tunnel_id: id("zzz")
        })),
        ErrorCode::UnknownTunnel
    );
    assert_eq!(
        rejected(rig.request(Request::Connect {
            tunnel_id: id("zzz")
        })),
        ErrorCode::UnknownTunnel
    );
}

#[test]
fn reconnect_is_refused_while_another_operation_runs() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    assert_eq!(
        rejected(rig.request(Request::Reconnect { tunnel_id: id("a") })),
        ErrorCode::Busy
    );
    let status = rig.status("a");
    assert_eq!(status.light, Light::Yellow);
    assert_eq!(status.light_reason, LightReason::Reconnecting);
    assert!(
        status.deadline_unix_ms.is_some(),
        "yellow always has a deadline"
    );
}
