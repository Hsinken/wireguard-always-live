//! The core's events for the GUI's event log (`Watchdog::take_events`, wave
//! three): what is noted, in which order, when, and the bound on how many
//! are kept. Also the local-offline mark on wake and network changes (review
//! R982-P3-b). Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use support::{Rig, binding, id};
use wgal_core::AdapterError;
use wgal_core::watchdog::MAX_EVENTS;
use wgal_protocol::{
    AdapterErrorKind, IncidentId, Light, LightReason, LogEvent, OsState, Request, TunnelId,
};
use wgal_testkit::Operation;

fn of(events: &[(Option<TunnelId>, LogEvent)], name: &str) -> Vec<LogEvent> {
    events
        .iter()
        .filter(|(t, _)| t.as_ref() == Some(&id(name)))
        .map(|(_, e)| e.clone())
        .collect()
}

/// Runs until an event matching `done` was noted for `name`, collecting
/// that tunnel's events.
fn run_until_noted(
    rig: &mut Rig,
    name: &str,
    events: &mut Vec<LogEvent>,
    limit: u64,
    done: impl Fn(&LogEvent) -> bool,
) {
    while !events.iter().any(&done) {
        rig.run(1);
        events.extend(rig.events_of(name));
        assert!(rig.now_secs() < limit, "never noted; got {events:?}");
    }
}

#[test]
fn the_first_verified_round_is_noted() {
    let mut rig = Rig::new(&["a"]);
    assert_eq!(rig.events_of("a"), [LogEvent::TrafficVerified]);
}

/// A fault, the rounds that confirm it, a first recovery that fails, the
/// backoff, a second recovery that succeeds, and the incident closing.
#[test]
fn a_fault_is_confirmed_recovered_after_a_failed_round_and_closed() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab().break_for_good("a");
    let mut events = Vec::new();
    run_until_noted(&mut rig, "a", &mut events, 400, |e| {
        matches!(e, LogEvent::RecoveryFailed { .. })
    });
    let retry = rig.status("a").next_retry_unix_ms;
    assert!(retry.is_some());
    // From now on a restart brings the tunnel back.
    rig.lab().heal("a");
    rig.lab().break_until_restart("a");
    run_until_noted(&mut rig, "a", &mut events, 1200, |e| {
        matches!(e, LogEvent::IncidentClosed { .. })
    });

    let incident = IncidentId::new("inc-000001").unwrap();
    let failed = |streak| LogEvent::ProbeFailed { streak, needed: 3 };
    assert_eq!(
        events,
        [
            failed(1),
            failed(2),
            failed(3),
            failed(4),
            failed(5),
            LogEvent::FaultConfirmed {
                incident_id: incident.clone()
            },
            LogEvent::RecoveryStarted { attempt: 1 },
            LogEvent::RecoveryFailed {
                next_retry_unix_ms: retry
            },
            LogEvent::RecoveryStarted { attempt: 2 },
            LogEvent::RecoverySucceeded,
            LogEvent::TrafficVerified,
            LogEvent::IncidentClosed {
                incident_id: incident
            },
        ]
    );
}

/// A stop the platform refuses during recovery.
#[test]
fn a_restart_that_cannot_be_carried_out_notes_the_control_failure() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Stop,
        AdapterError::new(AdapterErrorKind::PermissionDenied, "refused"),
    );
    rig.lab().break_until_restart("a");
    let mut events = Vec::new();
    run_until_noted(&mut rig, "a", &mut events, 400, |e| {
        matches!(e, LogEvent::RecoveryFailed { .. })
    });
    let tail = &events[events.len() - 3..];
    assert_eq!(tail[0], LogEvent::RecoveryStarted { attempt: 1 });
    assert_eq!(
        tail[1],
        LogEvent::ControlFailed {
            error: AdapterErrorKind::PermissionDenied
        }
    );
    assert!(matches!(
        tail[2],
        LogEvent::RecoveryFailed {
            next_retry_unix_ms: Some(_)
        }
    ));
}

/// D-3: an outage of the host's own network is noted once when it starts
/// and once when it ends, and never counts as a tunnel fault.
#[test]
fn a_local_network_outage_is_noted_once_and_never_counts() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    assert_eq!(rig.events_of("a"), [LogEvent::LocalNetworkOffline]);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::LocalNetworkOffline);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(rig.stops("a"), 0);
    rig.lab().fail_probes("a", None);
    rig.run(15);
    assert_eq!(
        rig.events_of("a"),
        [LogEvent::LocalNetworkOnline, LogEvent::TrafficVerified]
    );
    rig.run(300);
    assert_eq!(rig.stops("a"), 0);
    assert_eq!(rig.status("a").light, Light::Green);
}

/// The host's lines about no tunnel (the event log's 系統事件, owner request
/// of 2026-11-04, Codex P2-N9).
fn host_events(events: &[(Option<TunnelId>, LogEvent)]) -> Vec<LogEvent> {
    events
        .iter()
        .filter(|(t, _)| t.is_none())
        .map(|(_, e)| e.clone())
        .collect()
}

/// An outage of the host's own network is also one line about no tunnel:
/// when the first tunnel finds the host offline, and when the last one
/// finds it back, however many tunnels note it on their own.
#[test]
fn a_local_network_outage_is_one_host_line_from_the_first_tunnel_to_the_last() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.manage("b");
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.lab()
        .fail_probes("b", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    let events = rig.events();
    assert_eq!(host_events(&events), [LogEvent::LocalNetworkOffline]);
    assert_eq!(of(&events, "a"), [LogEvent::LocalNetworkOffline]);
    assert_eq!(of(&events, "b"), [LogEvent::LocalNetworkOffline]);
    // One back: the host line waits for the other.
    rig.lab().fail_probes("a", None);
    rig.run(15);
    let events = rig.events();
    assert_eq!(of(&events, "a")[0], LogEvent::LocalNetworkOnline);
    assert!(host_events(&events).is_empty());
    rig.lab().fail_probes("b", None);
    rig.run(15);
    let events = rig.events();
    assert_eq!(of(&events, "b")[0], LogEvent::LocalNetworkOnline);
    assert_eq!(host_events(&events), [LogEvent::LocalNetworkOnline]);
    // The next outage is written again.
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    assert_eq!(host_events(&rig.events()), [LogEvent::LocalNetworkOffline]);
}

/// A tunnel that stops being monitored while it had found the host offline
/// cannot say the host is back: the host-wide outage ends without a line,
/// and the next one is written again.
#[test]
fn a_paused_tunnel_ends_no_host_outage_it_cannot_vouch_for() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    assert_eq!(host_events(&rig.events()), [LogEvent::LocalNetworkOffline]);
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("a"),
        enabled: false,
    });
    rig.run(60);
    let events = rig.events();
    assert!(
        !host_events(&events).contains(&LogEvent::LocalNetworkOnline),
        "{events:?}"
    );
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("a"),
        enabled: true,
    });
    rig.run(300);
    // Still offline: the outage is written again, once.
    let events = rig.events();
    assert_eq!(
        host_events(&events)
            .iter()
            .filter(|e| **e == LogEvent::LocalNetworkOffline)
            .count(),
        1,
        "{events:?}"
    );
}

/// Codex P3-N15: the host came back while the only tunnel that had found it
/// offline was paused. Resuming it brings back no outage seen before: no
/// host line until a probe finds the host offline again.
#[test]
fn a_resume_after_the_network_came_back_during_a_pause_writes_no_old_outage() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    assert_eq!(host_events(&rig.events()), [LogEvent::LocalNetworkOffline]);
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("a"),
        enabled: false,
    });
    rig.run(60);
    rig.lab().fail_probes("a", None);
    rig.run(60);
    assert!(host_events(&rig.events()).is_empty());
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("a"),
        enabled: true,
    });
    rig.run(300);
    let events = rig.events();
    assert!(host_events(&events).is_empty(), "{events:?}");
    // The tunnel's own pair still ends: it had noted the outage itself.
    assert!(
        of(&events, "a").contains(&LogEvent::LocalNetworkOnline),
        "{events:?}"
    );
    assert_eq!(rig.status("a").light, Light::Green);
}

/// Codex P3-N15: a tunnel whose recovery target was removed while it had
/// found the host offline can no longer say, so it does not hold the
/// host-wide outage open: the host line ends when the other tunnel finds the
/// host back.
#[test]
fn a_tunnel_without_a_recovery_target_does_not_hold_the_host_outage() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.lab()
        .fail_probes("b", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(300);
    assert_eq!(host_events(&rig.events()), [LogEvent::LocalNetworkOffline]);
    rig.set_targets("b", None, &[]);
    rig.run(30);
    assert!(host_events(&rig.events()).is_empty());
    rig.lab().fail_probes("a", None);
    rig.lab().fail_probes("b", None);
    rig.run(15);
    let events = rig.events();
    assert_eq!(of(&events, "a")[0], LogEvent::LocalNetworkOnline);
    assert_eq!(host_events(&events), [LogEvent::LocalNetworkOnline]);
}

/// Review R982-P3-b: a wake or a network change drops the local-offline
/// mark as evidence from before. The grace period restarts and the next
/// round, due at once, judges again; while the host is still offline it
/// marks it again, and the log still has one outage.
#[test]
fn a_wake_or_network_change_drops_the_offline_mark_until_the_next_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(120);
    rig.events();
    assert_eq!(
        rig.status("a").light_reason,
        LightReason::LocalNetworkOffline
    );
    for change in [Rig::network_change, Rig::wake] {
        change(&mut rig);
        let status = rig.status("a");
        assert_eq!(status.light, Light::Yellow);
        assert_eq!(status.light_reason, LightReason::GracePeriod);
        rig.run(1);
        assert_eq!(
            rig.status("a").light_reason,
            LightReason::LocalNetworkOffline,
            "the next round found the host still offline"
        );
    }
    assert!(rig.events_of("a").is_empty(), "still one outage");
    rig.lab().fail_probes("a", None);
    rig.run(15);
    assert_eq!(
        rig.events_of("a"),
        [LogEvent::LocalNetworkOnline, LogEvent::TrafficVerified]
    );
    assert_eq!(rig.stops("a"), 0);
}

/// Only probes of the recovery target clear the mark, so a tunnel left
/// without one would otherwise show the outage for good.
#[test]
fn removing_the_recovery_target_drops_the_offline_mark() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(30);
    assert_eq!(
        rig.status("a").light_reason,
        LightReason::LocalNetworkOffline
    );
    rig.set_targets("a", None, &[]);
    rig.run(60);
    assert_eq!(rig.status("a").light_reason, LightReason::NotConfigured);
}

/// Something outside WGAL stops a tunnel the user wants up, and starts one
/// the user disconnected.
#[test]
fn external_stops_and_starts_are_noted() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.ok(Request::Disconnect { tunnel_id: id("b") });
    rig.run_until(50);
    rig.events();
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.lab()
        .fake
        .set_os_state(&binding("b"), OsState::Connected);
    rig.run(20);
    let events = rig.events();
    assert_eq!(of(&events, "a"), [LogEvent::StoppedExternally]);
    assert_eq!(of(&events, "b"), [LogEvent::StartedExternally]);
    assert_eq!(rig.status("a").light_reason, LightReason::NotConnected);
    assert_eq!(rig.status("b").light_reason, LightReason::ExternallyStarted);
    assert_eq!(rig.starts("a") + rig.stops("b"), 1, "only b's own stop");
}

/// With "start when stopped externally" on, the restart is noted after the
/// stop.
#[test]
fn a_restart_after_an_external_stop_is_noted() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::SetStartWhenStoppedExternally {
        tunnel_id: id("a"),
        enabled: true,
    });
    rig.run_until(44);
    rig.events();
    rig.lab()
        .fake
        .set_os_state(&binding("a"), OsState::Disconnected);
    rig.run(20);
    assert_eq!(
        rig.events_of("a"),
        [
            LogEvent::StoppedExternally,
            LogEvent::RestartedAfterExternalStop,
            LogEvent::TrafficVerified,
        ]
    );
    assert_eq!(rig.starts("a"), 1);
}

/// At most `MAX_EVENTS` are kept until the runtime takes them; the oldest
/// beyond that are dropped, and the list starts by saying how many.
#[test]
fn the_newest_events_are_kept_and_the_dropped_ones_counted() {
    let mut rig = Rig::new(&["a"]);
    rig.events();
    let toggles = MAX_EVENTS + 76;
    for i in 0..toggles {
        rig.ok(Request::SetTunnelMonitoring {
            tunnel_id: id("a"),
            enabled: i % 2 == 1,
        });
    }
    let events = rig.events();
    assert_eq!(events.len(), MAX_EVENTS + 1);
    assert_eq!(events[0], (None, LogEvent::LinesLost { count: 76 }));
    assert_eq!(
        events[1],
        (Some(id("a")), LogEvent::MonitoringPaused),
        "the 77th toggle, a pause"
    );
    assert_eq!(
        events.last(),
        Some(&(Some(id("a")), LogEvent::MonitoringResumed))
    );
    assert!(rig.events().is_empty(), "taken");
}
