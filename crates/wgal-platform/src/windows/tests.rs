//! Adapter tests against the fake SCM and IP Helper loaded from
//! `tests/fixtures/windows/`. Nothing here calls Windows.

use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use wgal_core::{AdapterError, PlatformAdapter, ProbeOutcome, ProbeRequest, TunnelBinding};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, OsState, ProbeKind, ProbeTarget,
    TargetAddress,
};

use super::fake::{FakeNet, FakeScm, NetCall, ScmCall, host};
use super::status::{GENERAL_FAILURE, REQ_TIMED_OUT, SUCCESS};
use super::{
    AdapterInfo, BestRoute, Control, EchoReply, Gate, IF_TYPE_PROP_VIRTUAL, Lane, Limits, NetError,
    ScmError, ServiceState, Settle, Unsent, WindowsAdapter, Withdrawal, stop_refused,
};

type Adapter = WindowsAdapter<FakeScm, FakeNet>;

const OFFICE_SERVICE: &str = "WireGuardTunnel$office";
const OFFICE_TARGET: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
const OFFICE_SOURCE: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 2);

fn setup_with(limits: Limits) -> (Arc<FakeScm>, Arc<FakeNet>, Adapter) {
    let host = host();
    let scm = Arc::new(FakeScm::from_host(&host));
    let net = Arc::new(FakeNet::from_host(&host));
    let adapter = WindowsAdapter::with_parts(Arc::clone(&scm), Arc::clone(&net), limits);
    (scm, net, adapter)
}

fn setup() -> (Arc<FakeScm>, Arc<FakeNet>, Adapter) {
    setup_with(Limits::DEFAULT)
}

fn binding(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

fn target(address: Ipv4Addr) -> TargetAddress {
    TargetAddress::new(address).unwrap()
}

fn icmp(address: Ipv4Addr, timeout: Duration) -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address: target(address),
            probe: ProbeKind::Icmp,
        },
        timeout,
    }
}

fn echo(status: u32, rtt_ms: u32) -> EchoReply {
    EchoReply {
        status,
        from: OFFICE_TARGET,
        rtt_ms,
        payload_matches: true,
    }
}

fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
    result.unwrap_err().kind
}

#[test]
fn fixture_uses_only_documentation_and_loopback_addresses() {
    let host = host();
    let documentation = |a: &Ipv4Addr| {
        let [x, y, z, _] = a.octets();
        matches!((x, y, z), (192, 0, 2) | (198, 51, 100) | (203, 0, 113)) || a.is_loopback()
    };
    for adapter in &host.adapters {
        assert!(
            adapter.ipv4.iter().all(documentation),
            "{}",
            adapter.friendly_name
        );
    }
    for route in &host.routes {
        assert!(documentation(&route.destination) && documentation(&route.source));
    }
}

/// Only what W3 showed on a device is claimed; control, background operation
/// and concurrent tunnels stay unproven.
#[test]
fn capabilities_claim_nothing_unverified() {
    let (_, _, adapter) = setup();
    assert_eq!(
        adapter.capabilities(),
        AdapterCapabilities {
            bound_icmp_probe: Capability::Supported,
            bound_tcp_probe: Capability::Unsupported,
            read_metrics: Capability::Unsupported,
            ..AdapterCapabilities::UNKNOWN
        }
    );
    assert_eq!(adapter.capabilities().verified_concurrent_tunnels, 0);
    assert_eq!(
        kind(adapter.read_metrics(&binding("office"))),
        AdapterErrorKind::Unsupported
    );
}

#[test]
fn discover_lists_only_valid_tunnel_services_without_opening_any() {
    let (scm, net, adapter) = setup();
    let found = adapter.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|t| (t.binding.as_str(), t.display_name.as_str(), t.control))
        .collect();
    assert_eq!(
        summary,
        [
            ("home-3", "home-3", Capability::Supported),
            ("lab", "lab", Capability::Supported),
            ("office", "office", Capability::Supported),
        ]
    );
    assert_eq!(scm.calls(), [ScmCall::List]);
    assert!(net.calls().is_empty());
}

#[test]
fn discover_reports_scm_refusal() {
    let (scm, _, adapter) = setup();
    scm.fail_next(ScmError::ACCESS_DENIED);
    assert_eq!(kind(adapter.discover()), AdapterErrorKind::PermissionDenied);
}

#[test]
fn query_state_opens_only_the_named_service() {
    let (scm, _, adapter) = setup();
    assert_eq!(
        adapter.query_state(&binding("office")).unwrap(),
        OsState::Connected
    );
    assert_eq!(
        adapter.query_state(&binding("lab")).unwrap(),
        OsState::Disconnected
    );
    assert_eq!(
        adapter.query_state(&binding("home-3")).unwrap(),
        OsState::Transitioning
    );
    assert_eq!(
        scm.calls(),
        [
            ScmCall::Query(OFFICE_SERVICE.to_owned()),
            ScmCall::Query("WireGuardTunnel$lab".to_owned()),
            ScmCall::Query("WireGuardTunnel$home-3".to_owned()),
        ]
    );
}

#[test]
fn a_removed_service_is_a_lost_binding_never_disconnected() {
    let (scm, _, adapter) = setup();
    scm.remove(OFFICE_SERVICE);
    let error = adapter.query_state(&binding("office")).unwrap_err();
    assert_eq!(error.kind, AdapterErrorKind::NotFound);
    assert_eq!(
        kind(adapter.start(&binding("office"))),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        kind(adapter.stop(&binding("office"))),
        AdapterErrorKind::NotFound
    );
}

#[test]
fn query_state_classifies_scm_failures() {
    let (scm, _, adapter) = setup();
    scm.fail_next(ScmError::ACCESS_DENIED);
    scm.fail_next(ScmError::SERVICE_MARKED_FOR_DELETE);
    assert_eq!(
        kind(adapter.query_state(&binding("office"))),
        AdapterErrorKind::PermissionDenied
    );
    assert_eq!(
        kind(adapter.query_state(&binding("office"))),
        AdapterErrorKind::NotFound
    );
    scm.set_state(OFFICE_SERVICE, 9);
    assert_eq!(
        kind(adapter.query_state(&binding("office"))),
        AdapterErrorKind::ParseError
    );
}

#[test]
fn bindings_that_are_not_tunnel_names_never_reach_the_scm() {
    let (scm, net, adapter) = setup();
    for bad in [
        "WireGuardManager-x".repeat(2),
        "NUL".to_owned(),
        "x".repeat(33),
    ] {
        let tunnel = binding(&bad);
        assert_eq!(
            kind(adapter.query_state(&tunnel)),
            AdapterErrorKind::NotFound
        );
        assert_eq!(kind(adapter.start(&tunnel)), AdapterErrorKind::NotFound);
        assert_eq!(kind(adapter.stop(&tunnel)), AdapterErrorKind::NotFound);
        assert_eq!(
            kind(adapter.resolve_interface(&tunnel)),
            AdapterErrorKind::NotFound
        );
    }
    assert!(scm.calls().is_empty());
    assert!(net.calls().is_empty());
}

#[test]
fn start_and_stop_touch_only_the_named_service() {
    let (scm, _, adapter) = setup();
    adapter.start(&binding("lab")).unwrap();
    adapter.stop(&binding("office")).unwrap();
    assert_eq!(
        scm.calls(),
        [
            ScmCall::Open("WireGuardTunnel$lab".to_owned(), Control::Start),
            ScmCall::Start("WireGuardTunnel$lab".to_owned()),
            ScmCall::Open(OFFICE_SERVICE.to_owned(), Control::Stop),
            ScmCall::Stop(OFFICE_SERVICE.to_owned()),
        ]
    );
    assert_eq!(scm.state_of("WireGuardTunnel$lab"), Some(2));
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(3));
    // Nothing else changed, including the manager.
    assert_eq!(scm.state_of("WireGuardTunnel$home-3"), Some(2));
    assert_eq!(scm.state_of("WireGuardManager"), Some(4));
}

#[test]
fn starting_a_running_or_stopping_a_stopped_tunnel_is_accepted() {
    let (scm, _, adapter) = setup();
    adapter.start(&binding("office")).unwrap();
    adapter.stop(&binding("lab")).unwrap();
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(4));
    assert_eq!(scm.state_of("WireGuardTunnel$lab"), Some(1));
}

#[test]
fn control_refusals_keep_their_cause() {
    let (scm, _, adapter) = setup();
    // A stop refused while the service is not between states is the
    // service's answer and is returned as it is.
    scm.refuse_next(
        ScmCall::Stop(OFFICE_SERVICE.to_owned()),
        ScmError::SERVICE_CANNOT_ACCEPT_CTRL,
    );
    assert_eq!(
        kind(adapter.stop(&binding("office"))),
        AdapterErrorKind::ManagerConflict
    );
    assert_eq!(sends(&scm), [ScmCall::Stop(OFFICE_SERVICE.to_owned())]);
    scm.fail_next(ScmError::SERVICE_DISABLED);
    assert_eq!(
        kind(adapter.start(&binding("lab"))),
        AdapterErrorKind::ManagerConflict
    );
    scm.fail_next(ScmError::ACCESS_DENIED);
    assert_eq!(
        kind(adapter.start(&binding("lab"))),
        AdapterErrorKind::PermissionDenied
    );
    scm.fail_next(ScmError::SERVICE_REQUEST_TIMEOUT);
    assert_eq!(
        kind(adapter.stop(&binding("office"))),
        AdapterErrorKind::Timeout
    );
}

#[test]
fn ir3_a_control_call_answers_before_the_cores_phase_deadline() {
    // The core gives one stop or start phase `control_phase_timeout` from the
    // moment it hands the request over, and polls the state once per
    // `control_poll_interval` after the call answers. A call that may use
    // its whole deadline (a settle loop does) must still leave the core
    // several polls, or a working recovery is recorded as failed.
    let timing = wgal_core::Timing::default();
    let left = timing
        .control_phase_timeout
        .checked_sub(Limits::DEFAULT.control)
        .unwrap_or_default();
    assert!(
        left >= timing.control_poll_interval * 5,
        "adapter control deadline {:?} leaves the core {left:?} of its {:?} phase",
        Limits::DEFAULT.control,
        timing.control_phase_timeout
    );
}

#[test]
fn a_hung_scm_call_times_out_at_the_deadline() {
    let limits = Limits {
        query: Duration::from_millis(50),
        control: Duration::from_millis(50),
        ..Limits::DEFAULT
    };
    let (scm, _, adapter) = setup_with(limits);
    scm.delay(Duration::from_millis(1_000));
    let started = Instant::now();
    assert_eq!(
        kind(adapter.query_state(&binding("office"))),
        AdapterErrorKind::Timeout
    );
    assert_eq!(
        kind(adapter.start(&binding("lab"))),
        AdapterErrorKind::Timeout
    );
    assert!(started.elapsed() < Duration::from_millis(900));
}

#[test]
fn resolves_the_adapter_named_after_the_tunnel() {
    let (_, _, adapter) = setup();
    let interface = adapter.resolve_interface(&binding("office")).unwrap();
    assert_eq!(interface.name, "office");
    assert_eq!(interface.index, Some(21));
    assert_eq!(interface.source, Some(OFFICE_SOURCE));
}

#[test]
fn resolve_fails_without_exactly_one_usable_adapter() {
    let (_, net, adapter) = setup();
    // A stopped tunnel has no adapter.
    assert_eq!(
        kind(adapter.resolve_interface(&binding("lab"))),
        AdapterErrorKind::NotFound
    );

    net.adapter_mut("home-3", |a| a.up = false);
    assert_eq!(
        kind(adapter.resolve_interface(&binding("home-3"))),
        AdapterErrorKind::NotFound
    );

    // A physical adapter renamed after a stopped tunnel is not its adapter.
    net.add_adapter(AdapterInfo {
        friendly_name: "lab".to_owned(),
        if_type: 6,
        if_index: 41,
        luid: 2,
        up: true,
        ipv4: vec![Ipv4Addr::new(192, 0, 2, 41)],
        ipv6: Vec::new(),
    });
    assert_eq!(
        kind(adapter.resolve_interface(&binding("lab"))),
        AdapterErrorKind::ManagerConflict
    );

    net.add_adapter(AdapterInfo {
        friendly_name: "office".to_owned(),
        if_type: IF_TYPE_PROP_VIRTUAL,
        if_index: 40,
        luid: 1,
        up: true,
        ipv4: vec![Ipv4Addr::new(198, 51, 100, 40)],
        ipv6: Vec::new(),
    });
    assert_eq!(
        kind(adapter.resolve_interface(&binding("office"))),
        AdapterErrorKind::ManagerConflict
    );
}

#[test]
fn several_tunnel_addresses_leave_the_source_to_the_route() {
    let (_, net, adapter) = setup();
    net.adapter_mut("office", |a| a.ipv4.push(Ipv4Addr::new(198, 51, 100, 22)));
    let interface = adapter.resolve_interface(&binding("office")).unwrap();
    assert_eq!(interface.source, None);
    let evidence = adapter
        .check_route(&interface, target(OFFICE_TARGET))
        .unwrap();
    assert_eq!(evidence.source, Some(OFFICE_SOURCE));
}

#[test]
fn adapter_listing_failures_are_not_a_missing_interface() {
    let (_, net, adapter) = setup();
    net.fail_adapters(NetError::ACCESS_DENIED);
    assert_eq!(
        kind(adapter.resolve_interface(&binding("office"))),
        AdapterErrorKind::PermissionDenied
    );
    net.fail_adapters(NetError::NOT_ENOUGH_MEMORY);
    assert_eq!(
        kind(adapter.resolve_interface(&binding("office"))),
        AdapterErrorKind::Unsupported
    );
}

#[test]
fn route_evidence_names_the_tunnel_interface_and_source() {
    let (_, _, adapter) = setup();
    let interface = adapter.resolve_interface(&binding("office")).unwrap();
    let evidence = adapter
        .check_route(&interface, target(OFFICE_TARGET))
        .unwrap();
    assert_eq!(evidence.target, target(OFFICE_TARGET));
    assert_eq!(evidence.interface_name, "office");
    assert_eq!(evidence.interface_index, Some(21));
    assert_eq!(evidence.source, Some(OFFICE_SOURCE));
}

#[test]
fn a_target_routed_elsewhere_is_a_route_mismatch() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    // Through the physical adapter.
    let lan = Ipv4Addr::new(192, 0, 2, 50);
    assert_eq!(
        kind(adapter.check_route(&office, target(lan))),
        AdapterErrorKind::RouteMismatch
    );
    // Through another tunnel: plan T08, B can reach it while A is broken.
    let home_target = Ipv4Addr::new(203, 0, 113, 30);
    assert_eq!(
        kind(adapter.check_route(&office, target(home_target))),
        AdapterErrorKind::RouteMismatch
    );
    // Same index but a different LUID is a different interface.
    net.set_route(
        OFFICE_TARGET,
        BestRoute {
            if_index: 21,
            luid: 99,
            source: OFFICE_SOURCE,
        },
    );
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::RouteMismatch
    );
    // No route at all.
    net.remove_route(OFFICE_TARGET);
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::RouteMismatch
    );
}

#[test]
fn a_source_that_is_not_the_tunnels_own_is_a_route_mismatch() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    let route = |source| BestRoute {
        if_index: 21,
        luid: 14_918_173_849_550_848,
        source,
    };

    net.set_route(OFFICE_TARGET, route(Ipv4Addr::new(192, 0, 2, 10)));
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::RouteMismatch
    );

    // The tunnel gained a second address and the route picked the new one,
    // not the one resolved.
    net.adapter_mut("office", |a| a.ipv4.push(Ipv4Addr::new(198, 51, 100, 22)));
    net.set_route(OFFICE_TARGET, route(Ipv4Addr::new(198, 51, 100, 22)));
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::RouteMismatch
    );
}

#[test]
fn a_source_address_shared_with_another_interface_is_ambiguous() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.adapter_mut("home-3", |a| a.ipv4.push(OFFICE_SOURCE));
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::RouteMismatch
    );
}

const OFFICE_LUID: u64 = 14_918_173_849_550_848;

/// The best route as Windows reports it through the office tunnel.
fn via_office(source: Ipv4Addr) -> BestRoute {
    BestRoute {
        if_index: 21,
        luid: OFFICE_LUID,
        source,
    }
}

/// Asserts that `address` is refused as a target of the office tunnel by
/// both `check_route` and `probe`, and that no echo is sent to it.
fn assert_local_target_refused(adapter: &Adapter, net: &FakeNet, address: Ipv4Addr) {
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    let (kind, text) = detail(adapter.check_route(&office, target(address)));
    assert_eq!(kind, AdapterErrorKind::RouteMismatch, "{address}: {text}");
    assert!(text.contains("own address"), "{address}: {text}");
    // The local stack would answer at once, from the target itself.
    net.push_echo(Ok(EchoReply {
        status: SUCCESS,
        from: address,
        rtt_ms: 0,
        payload_matches: true,
    }));
    let (kind, text) = detail(adapter.probe(&office, &icmp(address, Duration::from_secs(2))));
    assert_eq!(kind, AdapterErrorKind::RouteMismatch, "{address}: {text}");
    assert!(net.echoes_sent().is_empty(), "{address}: {:?}", net.calls());
}

#[test]
fn ir1_the_tunnels_own_address_is_never_route_evidence() {
    let (_, net, adapter) = setup();
    // Windows keeps a /32 host route for every local address on the
    // interface that holds it, so the best route to the tunnel's own address
    // is the tunnel itself, with the target as its source.
    net.set_route(OFFICE_SOURCE, via_office(OFFICE_SOURCE));
    assert_local_target_refused(&adapter, &net, OFFICE_SOURCE);
}

#[test]
fn ir1_any_local_address_is_never_route_evidence() {
    let (_, net, adapter) = setup();
    // An address held by a down adapter is still one of this host's own.
    net.add_adapter(AdapterInfo {
        friendly_name: "Wi-Fi".to_owned(),
        if_type: 71,
        if_index: 9,
        luid: 1_970_324_836_974_592,
        up: false,
        ipv4: vec![Ipv4Addr::new(192, 0, 2, 20)],
        ipv6: Vec::new(),
    });
    let local = [
        // The physical adapter's address.
        Ipv4Addr::new(192, 0, 2, 10),
        // Another tunnel's address.
        Ipv4Addr::new(198, 51, 100, 3),
        Ipv4Addr::new(192, 0, 2, 20),
    ];
    for address in local {
        // Even if Windows answered with the tunnel and its own source.
        net.set_route(address, via_office(OFFICE_SOURCE));
        assert_local_target_refused(&adapter, &net, address);
    }
    // A target that is not local is still checked as before.
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    adapter.check_route(&office, target(OFFICE_TARGET)).unwrap();
}

#[test]
fn an_interface_from_before_a_reconnect_is_stale() {
    let (_, net, adapter) = setup();
    let before = adapter.resolve_interface(&binding("office")).unwrap();
    let rebuilt = net.recreate("office");
    assert_ne!(Some(rebuilt.if_index), before.index);

    assert_eq!(
        kind(adapter.check_route(&before, target(OFFICE_TARGET))),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        kind(adapter.probe(&before, &icmp(OFFICE_TARGET, Duration::from_secs(2)))),
        AdapterErrorKind::NotFound
    );
    assert!(net.echoes_sent().is_empty());

    // Resolving again works with the new interface.
    let after = adapter.resolve_interface(&binding("office")).unwrap();
    assert_eq!(after.index, Some(rebuilt.if_index));
    adapter.check_route(&after, target(OFFICE_TARGET)).unwrap();
}

#[test]
fn a_vanished_or_foreign_interface_is_never_probed() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.remove_adapter("office");
    assert_eq!(
        kind(adapter.probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))),
        AdapterErrorKind::NotFound
    );

    // Interfaces naming the physical or loopback adapter are refused before
    // any route lookup or echo, even though "Ethernet" is a valid tunnel name.
    let lan = Ipv4Addr::new(192, 0, 2, 50);
    let foreign = [
        ("Ethernet", 7, Ipv4Addr::new(192, 0, 2, 10)),
        ("Loopback Pseudo-Interface 1", 1, Ipv4Addr::LOCALHOST),
    ];
    for (name, index, source) in foreign {
        let luid = host()
            .adapters
            .iter()
            .find(|a| a.friendly_name == name)
            .map(|a| a.luid);
        let interface = wgal_core::ResolvedInterface {
            name: name.to_owned(),
            index: Some(index),
            source: Some(source),
            incarnation: luid,
        };
        assert!(
            adapter
                .probe(&interface, &icmp(lan, Duration::from_secs(2)))
                .is_err()
        );
        assert!(adapter.check_route(&interface, target(lan)).is_err());
    }
    assert!(net.echoes_sent().is_empty());
    assert!(!net.calls().contains(&NetCall::BestRoute(lan)));
}

#[test]
fn probe_sends_one_echo_from_the_tunnel_source_and_keeps_the_rtt() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.push_echo(Ok(echo(SUCCESS, 23)));
    let outcome = adapter
        .probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))
        .unwrap();
    assert_eq!(
        outcome,
        ProbeOutcome::Reply {
            rtt: Duration::from_millis(23)
        }
    );
    assert_eq!(
        net.echoes_sent(),
        [NetCall::Echo {
            source: OFFICE_SOURCE,
            destination: OFFICE_TARGET,
            timeout: Duration::from_secs(2),
        }]
    );
    // The route was checked before the echo, and the adapter again after it.
    let calls = net.calls();
    let echo_at = calls
        .iter()
        .position(|c| matches!(c, NetCall::Echo { .. }))
        .unwrap();
    assert!(calls[..echo_at].contains(&NetCall::BestRoute(OFFICE_TARGET)));
    assert_eq!(calls[echo_at + 1..], [NetCall::Adapters]);
}

#[test]
fn probe_timeouts_are_clamped() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.push_echo(Ok(echo(SUCCESS, 1)));
    net.push_echo(Ok(echo(SUCCESS, 1)));
    adapter
        .probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(600)))
        .unwrap();
    adapter
        .probe(&office, &icmp(OFFICE_TARGET, Duration::ZERO))
        .unwrap();
    let timeouts: Vec<_> = net
        .echoes_sent()
        .into_iter()
        .map(|c| match c {
            NetCall::Echo { timeout, .. } => timeout,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        timeouts,
        [Limits::DEFAULT.max_echo, Duration::from_millis(1)]
    );
}

#[test]
fn a_lost_echo_is_no_reply_but_a_failed_send_is_an_error() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    let request = icmp(OFFICE_TARGET, Duration::from_secs(2));

    net.push_echo(Ok(echo(REQ_TIMED_OUT, 0)));
    assert_eq!(
        adapter.probe(&office, &request).unwrap(),
        ProbeOutcome::NoReply
    );

    net.push_echo(Ok(echo(GENERAL_FAILURE, 0)));
    assert_eq!(
        kind(adapter.probe(&office, &request)),
        AdapterErrorKind::Unsupported
    );

    net.push_echo(Err(NetError::ACCESS_DENIED));
    assert_eq!(
        kind(adapter.probe(&office, &request)),
        AdapterErrorKind::PermissionDenied
    );

    net.push_echo(Ok(EchoReply {
        from: Ipv4Addr::new(203, 0, 113, 99),
        ..echo(SUCCESS, 5)
    }));
    assert_eq!(
        kind(adapter.probe(&office, &request)),
        AdapterErrorKind::ParseError
    );
}

#[test]
fn an_echo_that_straddles_a_reconnect_is_discarded() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.recreate_during_next_echo("office");
    net.push_echo(Ok(echo(SUCCESS, 9)));
    assert_eq!(
        kind(adapter.probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))),
        AdapterErrorKind::NotFound
    );
}

#[test]
fn a_hung_echo_is_abandoned_after_its_timeout_and_grace() {
    let limits = Limits {
        echo_grace: Duration::from_millis(50),
        ..Limits::DEFAULT
    };
    let (_, net, adapter) = setup_with(limits);
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.delay_echo(Duration::from_millis(1_000));
    net.push_echo(Ok(echo(SUCCESS, 1)));
    let started = Instant::now();
    assert_eq!(
        kind(adapter.probe(&office, &icmp(OFFICE_TARGET, Duration::from_millis(50)))),
        AdapterErrorKind::Timeout
    );
    assert!(started.elapsed() < Duration::from_millis(900));
}

#[test]
fn tcp_probes_are_unsupported_and_send_nothing() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    let before = net.calls().len();
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(OFFICE_TARGET),
            probe: ProbeKind::TcpConnect {
                port: NonZeroU16::new(443).unwrap(),
            },
        },
        timeout: Duration::from_secs(2),
    };
    assert_eq!(
        kind(adapter.probe(&office, &request)),
        AdapterErrorKind::Unsupported
    );
    assert_eq!(net.calls().len(), before);
}

#[test]
fn probing_one_tunnel_never_controls_any_service() {
    let (scm, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.push_echo(Ok(echo(REQ_TIMED_OUT, 0)));
    adapter
        .probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))
        .unwrap();
    assert!(scm.calls().is_empty());
}

const LAB_SERVICE: &str = "WireGuardTunnel$lab";

fn short_control() -> Limits {
    Limits {
        control: Duration::from_millis(150),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    }
}

fn detail<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> (AdapterErrorKind, String) {
    let error = result.unwrap_err();
    (error.kind, error.detail.as_str().to_owned())
}

fn sends(scm: &FakeScm) -> Vec<ScmCall> {
    scm.calls()
        .into_iter()
        .filter(|c| matches!(c, ScmCall::Start(_) | ScmCall::Stop(_)))
        .collect()
}

/// Time for a released worker to do anything it still might.
fn settle() {
    std::thread::sleep(Duration::from_millis(100));
}

#[test]
fn f02_a_start_withdrawn_at_its_deadline_never_runs_after_a_later_stop() {
    let (scm, _, adapter) = setup_with(short_control());
    let lab = binding("lab");
    scm.hold(ScmCall::Open(LAB_SERVICE.to_owned(), Control::Start));

    let (kind, text) = detail(adapter.start(&lab));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("withdrawn"), "{text}");
    assert!(scm.wait_until_held(Duration::from_secs(1)));

    // The disconnect completes while the timed-out start is still opening.
    adapter.stop(&lab).unwrap();
    scm.release();
    assert!(scm.wait_for_finished(3, Duration::from_secs(2)));
    settle();

    assert_eq!(scm.state_of(LAB_SERVICE), Some(1), "{:?}", scm.calls());
    assert_eq!(sends(&scm), [ScmCall::Stop(LAB_SERVICE.to_owned())]);
}

#[test]
fn f02_a_stop_withdrawn_at_its_deadline_is_never_sent() {
    let (scm, _, adapter) = setup_with(short_control());
    scm.hold(ScmCall::Open(OFFICE_SERVICE.to_owned(), Control::Stop));

    let (kind, text) = detail(adapter.stop(&binding("office")));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("withdrawn"), "{text}");
    scm.release();
    assert!(scm.wait_for_finished(1, Duration::from_secs(2)));
    settle();

    assert!(sends(&scm).is_empty(), "{:?}", scm.calls());
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(4));
}

#[test]
fn f02_a_start_already_sent_is_never_overtaken_by_a_later_stop() {
    // The stop must still be inside its deadline when the start is released
    // below: a stop whose deadline passes while it waits is withdrawn unsent,
    // which is right, but not what this test is about. A deadline of a second
    // leaves a slow runner a wide margin (with 150 ms one failed in CI).
    let (scm, _, adapter) = setup_with(Limits {
        control: Duration::from_millis(1000),
        ..short_control()
    });
    let lab = binding("lab");
    // The SCM holds the start request itself past the deadline.
    scm.hold(ScmCall::Start(LAB_SERVICE.to_owned()));

    let (kind, text) = detail(adapter.start(&lab));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was sent"), "{text}");
    assert!(scm.wait_until_held(Duration::from_secs(1)));

    let stop = std::thread::scope(|scope| {
        let stop = scope.spawn(|| adapter.stop(&lab));
        // The start is released only once the stop has opened the service,
        // so the stop is known to be waiting behind the start, not yet to
        // come.
        let opened = ScmCall::Open(LAB_SERVICE.to_owned(), Control::Stop);
        let until = Instant::now() + Duration::from_secs(1);
        while !scm.calls().contains(&opened) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(scm.calls().contains(&opened), "{:?}", scm.calls());
        // The opened stop now has time to send if it would not wait for the
        // start: then it would show in `sends` here, and as an overlap below.
        // That a stop is held back can only be seen as a send that does not
        // come in such a window; seeing the wait itself would need a hook in
        // the adapter.
        std::thread::sleep(Duration::from_millis(100));
        // The stop waits for the start the SCM is still working on.
        assert_eq!(sends(&scm), [ScmCall::Start(LAB_SERVICE.to_owned())]);
        scm.release();
        stop.join().unwrap()
    });

    assert_eq!(scm.overlapping_sends(), 0);
    assert_eq!(
        sends(&scm),
        [
            ScmCall::Start(LAB_SERVICE.to_owned()),
            ScmCall::Stop(LAB_SERVICE.to_owned()),
        ]
    );
    // The start was already under way, so the service is still starting and
    // refused the stop. The stop waited for it to settle until its deadline,
    // and reports a retryable timeout, never a final refusal (F2).
    let (kind, text) = detail(stop);
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(
        text.contains("not sent") && text.contains("ask again"),
        "{text}"
    );
}

#[test]
fn f02_a_newer_request_replaces_an_older_one_not_yet_sent() {
    // Stop, then start: the stop is still opening when the start is made.
    let (scm, _, adapter) = setup();
    let lab = binding("lab");
    scm.hold(ScmCall::Open(LAB_SERVICE.to_owned(), Control::Stop));
    let older = std::thread::scope(|scope| {
        let older = scope.spawn(|| adapter.stop(&lab));
        assert!(scm.wait_until_held(Duration::from_secs(1)));
        adapter.start(&lab).unwrap();
        scm.release();
        older.join().unwrap()
    });
    let (kind, text) = detail(older);
    assert_eq!(kind, AdapterErrorKind::ManagerConflict);
    assert!(text.contains("replaced"), "{text}");
    assert_eq!(sends(&scm), [ScmCall::Start(LAB_SERVICE.to_owned())]);
    assert_eq!(scm.state_of(LAB_SERVICE), Some(2));

    // Start, then start: only the newer one is sent.
    let (scm, _, adapter) = setup();
    scm.hold(ScmCall::Open(LAB_SERVICE.to_owned(), Control::Start));
    let older = std::thread::scope(|scope| {
        let older = scope.spawn(|| adapter.start(&lab));
        assert!(scm.wait_until_held(Duration::from_secs(1)));
        adapter.start(&lab).unwrap();
        scm.release();
        older.join().unwrap()
    });
    assert_eq!(kind_of(older), AdapterErrorKind::ManagerConflict);
    assert_eq!(sends(&scm), [ScmCall::Start(LAB_SERVICE.to_owned())]);
}

#[test]
fn f02_requests_for_different_tunnels_do_not_wait_for_each_other() {
    let (scm, _, adapter) = setup_with(short_control());
    scm.hold(ScmCall::Start(LAB_SERVICE.to_owned()));
    std::thread::scope(|scope| {
        let lab = scope.spawn(|| adapter.start(&binding("lab")));
        assert!(scm.wait_until_held(Duration::from_secs(1)));
        adapter.stop(&binding("office")).unwrap();
        scm.release();
        let _ = lab.join().unwrap();
    });
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(3));
}

fn kind_of(result: Result<(), AdapterError>) -> AdapterErrorKind {
    result.unwrap_err().kind
}

#[test]
fn f01_check_registration_validates_the_name_and_touches_nothing() {
    let (scm, net, adapter) = setup();
    adapter.check_registration(&binding("office")).unwrap();
    for bad in ["CON", "nul", &"x".repeat(33)] {
        assert_eq!(
            kind(adapter.check_registration(&binding(bad))),
            AdapterErrorKind::NotFound,
            "{bad}"
        );
    }
    assert!(scm.calls().is_empty());
    assert!(net.calls().is_empty());
}

#[test]
fn f05_resolution_carries_the_adapter_luid_as_its_incarnation() {
    let (_, net, adapter) = setup();
    let before = adapter.resolve_interface(&binding("office")).unwrap();
    let luid = host()
        .adapters
        .iter()
        .find(|a| a.friendly_name == "office")
        .map(|a| a.luid);
    assert_eq!(before.incarnation, luid);

    net.recreate_in_place("office");
    let after = adapter.resolve_interface(&binding("office")).unwrap();
    assert_eq!(
        (&after.name, after.index, after.source),
        (&before.name, before.index, before.source)
    );
    assert_ne!(after.incarnation, before.incarnation);
    assert_ne!(after, before);
}

#[test]
fn f05_an_adapter_rebuilt_in_place_is_stale_before_any_echo() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    net.recreate_in_place("office");
    assert_eq!(
        kind(adapter.check_route(&office, target(OFFICE_TARGET))),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        kind(adapter.probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))),
        AdapterErrorKind::NotFound
    );
    assert!(net.echoes_sent().is_empty());
}

#[test]
fn f05_an_echo_across_an_in_place_rebuild_is_discarded() {
    let (_, net, adapter) = setup();
    let office = adapter.resolve_interface(&binding("office")).unwrap();
    // Same name, index and address afterwards; only the LUID differs.
    net.recreate_in_place_during_next_echo("office");
    net.push_echo(Ok(echo(SUCCESS, 9)));
    assert_eq!(
        kind(adapter.probe(&office, &icmp(OFFICE_TARGET, Duration::from_secs(2)))),
        AdapterErrorKind::NotFound
    );
    assert_eq!(net.echoes_sent().len(), 1);
}

/// A start that timed out after it was sent leaves the service
/// `START_PENDING`; the SCM refuses a stop until it settles.
fn after_timed_out_start(limits: Limits) -> (Arc<FakeScm>, Adapter) {
    let (scm, _, adapter) = setup_with(limits);
    scm.set_state(LAB_SERVICE, 2);
    (scm, adapter)
}

fn stops_sent(scm: &FakeScm) -> usize {
    sends(scm)
        .iter()
        .filter(|c| matches!(c, ScmCall::Stop(_)))
        .count()
}

#[test]
fn f2_a_stop_waits_out_a_pending_start_and_then_stops() {
    let limits = Limits {
        control: Duration::from_secs(5),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    };
    let (scm, adapter) = after_timed_out_start(limits);
    let stop = std::thread::scope(|scope| {
        let stop = scope.spawn(|| adapter.stop(&binding("lab")));
        std::thread::sleep(Duration::from_millis(80));
        // Refused once, then waiting: nothing else was sent meanwhile.
        assert_eq!(stops_sent(&scm), 1);
        // The pending start finishes.
        scm.set_state(LAB_SERVICE, 4);
        stop.join().unwrap()
    });
    stop.unwrap();
    assert_eq!(stops_sent(&scm), 2);
    assert_eq!(scm.overlapping_sends(), 0);
    // Stopping, not left running.
    assert_eq!(scm.state_of(LAB_SERVICE), Some(3));
}

#[test]
fn f2_a_pending_start_that_fails_leaves_the_stop_done() {
    let limits = Limits {
        control: Duration::from_secs(5),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    };
    let (scm, adapter) = after_timed_out_start(limits);
    let stop = std::thread::scope(|scope| {
        let stop = scope.spawn(|| adapter.stop(&binding("lab")));
        std::thread::sleep(Duration::from_millis(50));
        // The start gives up on its own; the service is stopped already.
        scm.set_state(LAB_SERVICE, 1);
        stop.join().unwrap()
    });
    stop.unwrap();
    assert_eq!(scm.state_of(LAB_SERVICE), Some(1));
}

#[test]
fn f2_a_stop_past_its_deadline_is_retryable_and_sends_nothing_more() {
    let (scm, adapter) = after_timed_out_start(short_control());
    let lab = binding("lab");

    let (kind, text) = detail(adapter.stop(&lab));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(
        text.contains("not sent") && text.contains("ask again"),
        "{text}"
    );
    assert_eq!(stops_sent(&scm), 1);

    // The start settles after the stop's deadline: the withdrawn stop must
    // not be sent on its own.
    scm.set_state(LAB_SERVICE, 4);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stops_sent(&scm), 1, "{:?}", scm.calls());
    assert_eq!(scm.state_of(LAB_SERVICE), Some(4));

    // The caller asks again, as the detail says, and the service stops.
    adapter.stop(&lab).unwrap();
    assert_eq!(stops_sent(&scm), 2);
    assert_eq!(scm.state_of(LAB_SERVICE), Some(3));
}

#[test]
fn f2_retried_stops_never_leave_the_service_running() {
    let (scm, adapter) = after_timed_out_start(short_control());
    let lab = binding("lab");
    let mut outcomes = Vec::new();
    for round in 0..4 {
        if round == 2 {
            // The start completes between two attempts.
            scm.set_state(LAB_SERVICE, 4);
        }
        let outcome = adapter.stop(&lab);
        outcomes.push(outcome.as_ref().map_err(|e| e.kind).copied());
        if outcome.is_ok() {
            break;
        }
    }
    assert_eq!(
        outcomes,
        [
            Err(AdapterErrorKind::Timeout),
            Err(AdapterErrorKind::Timeout),
            Ok(()),
        ]
    );
    assert_ne!(scm.state_of(LAB_SERVICE), Some(4));
    assert_eq!(scm.overlapping_sends(), 0);
}

#[test]
fn f2_a_newer_start_replaces_a_stop_that_is_waiting() {
    let limits = Limits {
        control: Duration::from_secs(5),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    };
    let (scm, adapter) = after_timed_out_start(limits);
    let lab = binding("lab");
    let (stop, start) = std::thread::scope(|scope| {
        let stop = scope.spawn(|| adapter.stop(&lab));
        std::thread::sleep(Duration::from_millis(50));
        let start = adapter.start(&lab);
        (stop.join().unwrap(), start)
    });
    let (kind, text) = detail(stop);
    assert_eq!(kind, AdapterErrorKind::ManagerConflict);
    assert!(text.contains("replaced"), "{text}");
    // Still starting; accepted as already where the start would take it.
    start.unwrap();
    assert_eq!(stops_sent(&scm), 1);
    assert_eq!(scm.overlapping_sends(), 0);
}

/// Only `ERROR_SERVICE_ALREADY_RUNNING` while the service is not running or
/// starting makes a start wait and send itself again (integrated review 2);
/// any other refusal is the service's answer.
#[test]
fn f2_a_start_refused_for_another_reason_is_never_resent() {
    let (scm, _, adapter) = setup_with(short_control());
    scm.refuse_next(
        ScmCall::Start(LAB_SERVICE.to_owned()),
        ScmError::SERVICE_CANNOT_ACCEPT_CTRL,
    );
    assert_eq!(
        kind(adapter.start(&binding("lab"))),
        AdapterErrorKind::ManagerConflict
    );
    assert_eq!(sends(&scm), [ScmCall::Start(LAB_SERVICE.to_owned())]);
    assert_eq!(scm.state_of(LAB_SERVICE), Some(1));
}

fn queries(scm: &FakeScm) -> usize {
    scm.calls()
        .iter()
        .filter(|c| matches!(c, ScmCall::Query(_)))
        .count()
}

#[test]
fn r2_a_stop_sent_in_time_and_refused_after_its_deadline_is_never_resent() {
    let (scm, adapter) = after_timed_out_start(short_control());
    // The SCM holds the stop itself past the deadline.
    scm.hold(ScmCall::Stop(LAB_SERVICE.to_owned()));

    let (kind, text) = detail(adapter.stop(&binding("lab")));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(
        text.contains("was sent") && text.contains("not sent again"),
        "{text}"
    );
    assert!(scm.wait_until_held(Duration::from_secs(1)));

    // Only now does the SCM answer, refusing the stop: the service is still
    // starting. Then the start finishes.
    scm.release();
    assert!(scm.wait_for_finished(3, Duration::from_secs(2)));
    scm.set_state(LAB_SERVICE, 4);
    settle();

    // The caller already has its answer, so nothing more is sent for it, and
    // the worker has stopped watching the service.
    assert_eq!(stops_sent(&scm), 1, "{:?}", scm.calls());
    assert_eq!(scm.state_of(LAB_SERVICE), Some(4));
    let polled = queries(&scm);
    settle();
    assert_eq!(queries(&scm), polled);
}

fn in_a_minute() -> Option<Instant> {
    Instant::now().checked_add(Duration::from_secs(60))
}

#[test]
fn r2_withdrawing_a_sent_request_closes_it_for_good() {
    let gate = Gate::new(in_a_minute());
    assert!(gate.send());
    assert_eq!(gate.withdraw(), Withdrawal::Sent);
    // The SCM refuses the send only after the caller has its answer.
    gate.defer();
    assert!(!gate.waiting());
    assert!(!gate.send());

    let gate = Gate::new(in_a_minute());
    assert!(gate.send());
    gate.defer();
    assert_eq!(gate.withdraw(), Withdrawal::Deferred);
    assert!(!gate.send());

    let gate = Gate::new(in_a_minute());
    assert_eq!(gate.withdraw(), Withdrawal::NotSent);
    assert!(!gate.send());
}

#[test]
fn r2_a_passed_deadline_closes_the_gate_without_a_withdrawal() {
    let gate = Gate::new(Some(Instant::now()));
    assert!(!gate.waiting());
    assert!(!gate.send());
    assert_eq!(gate.withdraw(), Withdrawal::NotSent);
}

#[test]
fn r2_the_deadline_stops_a_resend_before_the_caller_withdraws() {
    let lane = Lane::default();
    let ticket = lane.ticket();
    let deadline = Instant::now() + Duration::from_millis(100);
    let gate = Gate::new(Some(deadline));
    let sent = AtomicUsize::new(0);
    let checks = AtomicUsize::new(0);
    // Still starting at the first check; settled at the second, which only
    // answers after the deadline. The caller has not withdrawn yet.
    let state = || {
        if checks.fetch_add(1, Ordering::SeqCst) > 0 {
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
            );
            return Ok(ServiceState::Running);
        }
        Ok(ServiceState::StartPending)
    };
    let result = lane.run(
        ticket,
        &gate,
        || Ok(()),
        |_: &()| {
            sent.fetch_add(1, Ordering::SeqCst);
            Err(ScmError::SERVICE_CANNOT_ACCEPT_CTRL)
        },
        &Settle {
            refusal: ScmError::SERVICE_CANNOT_ACCEPT_CTRL,
            state: &state,
            judge: stop_refused,
            poll: Duration::from_millis(1),
        },
    );
    assert_eq!(result, Err(Unsent::Withdrawn));
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    // The caller, waking late, learns the stop was refused and not resent.
    assert_eq!(gate.withdraw(), Withdrawal::Deferred);
}

fn starts_sent(scm: &FakeScm) -> usize {
    sends(scm)
        .iter()
        .filter(|c| matches!(c, ScmCall::Start(_)))
        .count()
}

/// A stop that is still under way leaves the service `STOP_PENDING`, and
/// `StartServiceW` refuses a start with `ERROR_SERVICE_ALREADY_RUNNING`
/// whenever the service is not `STOPPED`, this state included.
fn during_a_pending_stop(limits: Limits) -> (Arc<FakeScm>, Adapter) {
    let (scm, _, adapter) = setup_with(limits);
    scm.set_state(OFFICE_SERVICE, 3);
    (scm, adapter)
}

#[test]
fn ir2_a_start_waits_out_a_pending_stop_and_then_starts() {
    let limits = Limits {
        control: Duration::from_secs(5),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    };
    let (scm, adapter) = during_a_pending_stop(limits);
    let start = std::thread::scope(|scope| {
        let start = scope.spawn(|| adapter.start(&binding("office")));
        std::thread::sleep(Duration::from_millis(80));
        // Refused once, then waiting: nothing else was sent meanwhile.
        assert_eq!(starts_sent(&scm), 1);
        // The pending stop finishes.
        scm.set_state(OFFICE_SERVICE, 1);
        start.join().unwrap()
    });
    start.unwrap();
    assert_eq!(starts_sent(&scm), 2);
    assert_eq!(scm.overlapping_sends(), 0);
    // Starting, not left stopped.
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(2));
}

#[test]
fn ir2_a_start_past_its_deadline_during_a_pending_stop_is_retryable() {
    let (scm, adapter) = during_a_pending_stop(short_control());
    let office = binding("office");

    let (kind, text) = detail(adapter.start(&office));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(
        text.contains("not sent") && text.contains("ask again"),
        "{text}"
    );
    assert_eq!(starts_sent(&scm), 1);

    // The stop settles after the start's deadline: the withdrawn start must
    // not be sent on its own.
    scm.set_state(OFFICE_SERVICE, 1);
    settle();
    assert_eq!(starts_sent(&scm), 1, "{:?}", scm.calls());
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(1));

    // The caller asks again, as the detail says, and the service starts.
    adapter.start(&office).unwrap();
    assert_eq!(starts_sent(&scm), 2);
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(2));
}

#[test]
fn ir2_a_newer_stop_replaces_a_start_that_is_waiting() {
    let limits = Limits {
        control: Duration::from_secs(5),
        settle_poll: Duration::from_millis(10),
        ..Limits::DEFAULT
    };
    let (scm, adapter) = during_a_pending_stop(limits);
    let office = binding("office");
    let (start, stop) = std::thread::scope(|scope| {
        let start = scope.spawn(|| adapter.start(&office));
        std::thread::sleep(Duration::from_millis(50));
        let stop = adapter.stop(&office);
        (start.join().unwrap(), stop)
    });
    let (kind, text) = detail(start);
    assert_eq!(kind, AdapterErrorKind::ManagerConflict);
    assert!(text.contains("replaced"), "{text}");
    stop.unwrap();
    // The refused start was never sent again, and nothing started.
    assert_eq!(starts_sent(&scm), 1);
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(3));
    assert_eq!(scm.overlapping_sends(), 0);
}

#[test]
fn ir2_already_running_is_accepted_only_when_running_or_starting() {
    // Running, or starting: the start is already where it would take the
    // tunnel, and is not sent again.
    let (scm, _, adapter) = setup_with(short_control());
    adapter.start(&binding("office")).unwrap();
    adapter.start(&binding("home-3")).unwrap();
    assert_eq!(starts_sent(&scm), 2);
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(4));
    assert_eq!(scm.state_of("WireGuardTunnel$home-3"), Some(2));

    // Refused although the service is stopped by the time it is read (the
    // stop finished in between): the start is sent again and takes effect.
    let (scm, _, adapter) = setup_with(short_control());
    scm.refuse_next(
        ScmCall::Start(LAB_SERVICE.to_owned()),
        ScmError::SERVICE_ALREADY_RUNNING,
    );
    adapter.start(&binding("lab")).unwrap();
    assert_eq!(starts_sent(&scm), 2);
    assert_eq!(scm.state_of(LAB_SERVICE), Some(2));

    // Paused (tunnel services never pause): not running as far as the
    // tunnel goes, and waiting would not change it. The refusal is returned.
    let (scm, _, adapter) = setup_with(short_control());
    scm.set_state(OFFICE_SERVICE, 7);
    assert_eq!(
        kind(adapter.start(&binding("office"))),
        AdapterErrorKind::ManagerConflict
    );
    assert_eq!(starts_sent(&scm), 1);
}
