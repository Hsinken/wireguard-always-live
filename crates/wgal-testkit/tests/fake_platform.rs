#![allow(clippy::unwrap_used)]

use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::time::Duration;

use wgal_core::{AdapterError, Clock, PlatformAdapter, ProbeOutcome, ProbeRequest, TunnelBinding};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, OsState, ProbeKind, ProbeTarget,
    TargetAddress,
};
use wgal_testkit::{Call, FakeClock, FakePlatform, Operation};

fn tunnel(id: &str) -> TunnelBinding {
    TunnelBinding::new(id).unwrap()
}

fn target() -> TargetAddress {
    TargetAddress::new(Ipv4Addr::new(192, 0, 2, 1)).unwrap()
}

fn icmp() -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    }
}

fn two_tunnels() -> (FakePlatform, TunnelBinding, TunnelBinding) {
    let platform = FakePlatform::new();
    let (a, b) = (tunnel("a"), tunnel("b"));
    platform.add_tunnel(a.clone(), "Tunnel A", OsState::Connected);
    platform.add_tunnel(b.clone(), "Tunnel B", OsState::Connected);
    (platform, a, b)
}

#[test]
fn discovers_added_tunnels_in_id_order_with_their_control() {
    let platform = FakePlatform::new();
    platform.add_tunnel(tunnel("b"), "B", OsState::Disconnected);
    platform.add_tunnel(tunnel("a"), "A", OsState::Connected);
    platform.set_control(&tunnel("b"), Capability::Unsupported);

    let found = platform.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|t| (t.binding.as_str(), t.display_name.as_str(), t.control))
        .collect();
    assert_eq!(
        summary,
        [
            ("a", "A", Capability::Supported),
            ("b", "B", Capability::Unsupported)
        ]
    );
    assert_eq!(platform.calls(), [Call::Discover]);
}

#[test]
fn capabilities_start_unknown_and_can_be_set() {
    let platform = FakePlatform::new();
    assert_eq!(platform.capabilities(), AdapterCapabilities::UNKNOWN);

    let proven = AdapterCapabilities {
        query_state: Capability::Supported,
        verified_concurrent_tunnels: 2,
        ..AdapterCapabilities::UNKNOWN
    };
    platform.set_capabilities(proven);
    assert_eq!(platform.capabilities(), proven);
}

#[test]
fn unknown_tunnel_is_not_found_and_still_logged() {
    let platform = FakePlatform::new();
    let ghost = tunnel("ghost");

    let err = platform.start(&ghost).unwrap_err();
    assert_eq!(err.kind, AdapterErrorKind::NotFound);
    assert_eq!(platform.calls(), [Call::Start(ghost)]);
}

#[test]
fn only_connected_tunnels_have_an_interface() {
    let platform = FakePlatform::new();
    let (up, down) = (tunnel("up"), tunnel("down"));
    platform.add_tunnel(up.clone(), "Up", OsState::Connected);
    platform.add_tunnel(down.clone(), "Down", OsState::Disconnected);

    let interface = platform.resolve_interface(&up).unwrap();
    assert_eq!(Some(interface), platform.interface(&up));
    assert_eq!(
        platform.resolve_interface(&down).unwrap_err().kind,
        AdapterErrorKind::NotFound
    );
}

#[test]
fn reconnect_gives_a_new_interface_and_the_old_one_goes_stale() {
    let (platform, a, _) = two_tunnels();
    let before = platform.resolve_interface(&a).unwrap();

    platform.stop(&a).unwrap();
    platform.start(&a).unwrap();
    let after = platform.resolve_interface(&a).unwrap();
    assert_ne!(after.index, before.index);

    // A result through the old interface must not count for the new one.
    let stale_probe = platform.probe(&before, &icmp()).unwrap_err();
    let stale_route = platform.check_route(&before, target()).unwrap_err();
    assert_eq!(stale_probe.kind, AdapterErrorKind::NotFound);
    assert_eq!(stale_route.kind, AdapterErrorKind::NotFound);
    assert!(matches!(
        platform.probe(&after, &icmp()).unwrap(),
        ProbeOutcome::Reply { .. }
    ));
}

#[test]
fn interface_rebuilt_behind_the_managers_back_goes_stale_too() {
    let (platform, a, _) = two_tunnels();
    let before = platform.resolve_interface(&a).unwrap();

    let rebuilt = platform.recreate_interface(&a);
    assert_eq!(platform.os_state(&a), OsState::Connected);
    assert_ne!(rebuilt.index, before.index);
    assert!(platform.probe(&before, &icmp()).is_err());
    assert!(platform.probe(&rebuilt, &icmp()).is_ok());
}

#[test]
fn interface_rebuilt_in_place_differs_only_in_incarnation_and_goes_stale() {
    let (platform, a, _) = two_tunnels();
    let before = platform.resolve_interface(&a).unwrap();

    let rebuilt = platform.rebuild_interface_in_place(&a);
    assert_eq!(
        (&rebuilt.name, rebuilt.index, rebuilt.source),
        (&before.name, before.index, before.source)
    );
    assert_ne!(rebuilt.incarnation, before.incarnation);
    assert_ne!(rebuilt, before);
    assert_eq!(
        platform.probe(&before, &icmp()).unwrap_err().kind,
        AdapterErrorKind::NotFound
    );
    assert!(platform.check_route(&before, target()).is_err());
    assert!(platform.probe(&rebuilt, &icmp()).is_ok());
    assert_eq!(platform.resolve_interface(&a).unwrap(), rebuilt);
}

#[test]
fn every_new_interface_has_a_new_incarnation() {
    let (platform, a, b) = two_tunnels();
    let first_a = platform.resolve_interface(&a).unwrap();
    let first_b = platform.resolve_interface(&b).unwrap();
    platform.stop(&a).unwrap();
    platform.start(&a).unwrap();
    let second_a = platform.resolve_interface(&a).unwrap();

    let incarnations = [
        first_a.incarnation,
        first_b.incarnation,
        second_a.incarnation,
    ];
    assert!(incarnations.iter().all(Option::is_some));
    assert_ne!(incarnations[0], incarnations[1]);
    assert_ne!(incarnations[0], incarnations[2]);
    assert_ne!(incarnations[1], incarnations[2]);
}

#[test]
fn registration_check_is_scriptable_and_logged() {
    let (platform, a, b) = two_tunnels();
    assert_eq!(platform.check_registration(&a), Ok(()));

    let refusal = AdapterError::new(
        AdapterErrorKind::PermissionDenied,
        "config is user-writable",
    );
    platform.set_registration(&a, Err(refusal.clone()));
    assert_eq!(platform.check_registration(&a), Err(refusal));
    assert_eq!(platform.check_registration(&b), Ok(()));
    assert_eq!(
        platform
            .check_registration(&tunnel("missing"))
            .unwrap_err()
            .kind,
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        platform.calls_for(&a),
        [
            Call::CheckRegistration(a.clone()),
            Call::CheckRegistration(a.clone())
        ]
    );
}

#[test]
fn default_probe_follows_os_state() {
    let (platform, a, _) = two_tunnels();
    let interface = platform.resolve_interface(&a).unwrap();
    assert_eq!(
        platform.probe(&interface, &icmp()).unwrap(),
        ProbeOutcome::Reply {
            rtt: FakePlatform::DEFAULT_RTT
        }
    );

    platform.set_os_state(&a, OsState::Transitioning);
    assert_eq!(
        platform.probe(&interface, &icmp()).unwrap(),
        ProbeOutcome::NoReply
    );
}

#[test]
fn scripted_probes_run_in_order_then_fall_back() {
    let (platform, a, _) = two_tunnels();
    let interface = platform.resolve_interface(&a).unwrap();
    platform.script_probes(
        &a,
        [
            Ok(ProbeOutcome::NoReply),
            Err(AdapterError::new(
                AdapterErrorKind::PermissionDenied,
                "raw socket not permitted",
            )),
        ],
    );

    assert_eq!(
        platform.probe(&interface, &icmp()).unwrap(),
        ProbeOutcome::NoReply
    );
    // A probe that cannot run surfaces as an error, distinct from NoReply.
    let err = platform.probe(&interface, &icmp()).unwrap_err();
    assert_eq!(err.kind, AdapterErrorKind::PermissionDenied);
    assert!(matches!(
        platform.probe(&interface, &icmp()).unwrap(),
        ProbeOutcome::Reply { .. }
    ));
}

#[test]
fn route_through_another_tunnel_is_a_mismatch() {
    let (platform, a, b) = two_tunnels();
    let (via_a, via_b) = (
        platform.resolve_interface(&a).unwrap(),
        platform.resolve_interface(&b).unwrap(),
    );

    platform.route_via(target(), Some(&b));
    assert_eq!(
        platform.check_route(&via_a, target()).unwrap_err().kind,
        AdapterErrorKind::RouteMismatch
    );
    let evidence = platform.check_route(&via_b, target()).unwrap();
    assert_eq!(evidence.interface_index, via_b.index);
    assert_eq!(evidence.source, via_b.source);

    platform.route_via(target(), None);
    assert!(platform.check_route(&via_b, target()).is_err());
}

#[test]
fn failures_are_errors_not_disconnects() {
    let (platform, a, _) = two_tunnels();
    platform.fail_next(
        &a,
        Operation::QueryState,
        AdapterError::new(AdapterErrorKind::Timeout, "no answer"),
    );
    platform.fail_next(
        &a,
        Operation::ResolveInterface,
        AdapterError::new(AdapterErrorKind::ParseError, "unexpected output"),
    );

    assert_eq!(
        platform.query_state(&a).unwrap_err().kind,
        AdapterErrorKind::Timeout
    );
    assert_eq!(platform.query_state(&a).unwrap(), OsState::Connected);
    assert_eq!(
        platform.resolve_interface(&a).unwrap_err().kind,
        AdapterErrorKind::ParseError
    );
    assert!(platform.resolve_interface(&a).is_ok());
}

#[test]
fn start_and_stop_switch_state_unless_failure_queued() {
    let (platform, a, _) = two_tunnels();

    platform.stop(&a).unwrap();
    assert_eq!(platform.os_state(&a), OsState::Disconnected);
    assert_eq!(platform.interface(&a), None);

    platform.fail_next(
        &a,
        Operation::Start,
        AdapterError::new(AdapterErrorKind::ManagerConflict, "manager busy"),
    );
    assert!(platform.start(&a).is_err());
    assert_eq!(platform.os_state(&a), OsState::Disconnected);

    platform.start(&a).unwrap();
    assert_eq!(platform.os_state(&a), OsState::Connected);
}

#[test]
fn uncontrolled_tunnel_refuses_start_and_stop() {
    let (platform, a, _) = two_tunnels();
    platform.set_control(&a, Capability::Unsupported);

    assert_eq!(
        platform.stop(&a).unwrap_err().kind,
        AdapterErrorKind::Unsupported
    );
    assert_eq!(platform.os_state(&a), OsState::Connected);
}

#[test]
fn metrics_default_to_unsupported() {
    let (platform, a, _) = two_tunnels();
    assert_eq!(
        platform.read_metrics(&a).unwrap_err().kind,
        AdapterErrorKind::Unsupported
    );
}

#[test]
fn acting_on_one_tunnel_leaves_the_other_untouched() {
    let (platform, a, b) = two_tunnels();
    let b_interface = platform.interface(&b).unwrap();

    platform.stop(&a).unwrap();
    platform.start(&a).unwrap();
    let a_interface = platform.resolve_interface(&a).unwrap();
    platform.probe(&a_interface, &icmp()).unwrap();

    assert_eq!(platform.os_state(&b), OsState::Connected);
    assert_eq!(platform.interface(&b), Some(b_interface));
    assert!(platform.calls_for(&b).is_empty());
    assert_eq!(
        platform.calls_for(&a),
        [
            Call::Stop(a.clone()),
            Call::Start(a.clone()),
            Call::ResolveInterface(a.clone()),
            Call::Probe(a_interface, icmp()),
        ]
    );
}

#[test]
fn tcp_probes_are_recorded_with_their_port() {
    let (platform, a, _) = two_tunnels();
    let interface = platform.resolve_interface(&a).unwrap();
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::TcpConnect {
                port: NonZeroU16::new(443).unwrap(),
            },
        },
        timeout: Duration::from_secs(2),
    };

    platform.probe(&interface, &request).unwrap();
    assert_eq!(
        platform.calls_for(&a),
        [
            Call::ResolveInterface(a.clone()),
            Call::Probe(interface, request)
        ]
    );
}

/// Plan §6.2's example, run on the fakes: the recovery target fails every
/// probe, 15 s apart, starting at t=0. By t=30 three probes have failed but
/// only 30 s have passed, short of the 60 s confirmation window, which is
/// reached at t=60 on the fifth failure. Deciding to recover is the core state
/// machine's job (test T03); this pins the timeline the fakes reproduce.
#[test]
fn plan_example_three_failures_by_thirty_seconds_is_short_of_sixty() {
    let clock = FakeClock::new(1_767_225_600_000);
    let (platform, a, b) = two_tunnels();
    let (via_a, via_b) = (
        platform.resolve_interface(&a).unwrap(),
        platform.resolve_interface(&b).unwrap(),
    );
    platform.script_probes(&a, std::iter::repeat_n(Ok(ProbeOutcome::NoReply), 5));

    let start = clock.now();
    let mut failures = Vec::new();
    for round in 0..5 {
        if round > 0 {
            clock.advance(Duration::from_secs(15)).unwrap();
        }
        if platform.probe(&via_a, &icmp()).unwrap() == ProbeOutcome::NoReply {
            failures.push(clock.now().saturating_duration_since(start));
        }
        // The healthy tunnel keeps answering throughout.
        assert!(matches!(
            platform.probe(&via_b, &icmp()).unwrap(),
            ProbeOutcome::Reply { .. }
        ));
    }

    let secs: Vec<_> = failures.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [0, 15, 30, 45, 60]);
    assert!(failures[2] < Duration::from_secs(60));
}
