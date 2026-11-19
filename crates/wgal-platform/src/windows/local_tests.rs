//! Open finding D-3: the adapter reports `LocalNetworkOffline` instead of a
//! lost echo when this host itself has no network, against the fake SCM and
//! IP Helper. The last tests drive the core watchdog over the adapter
//! through a router reboot. Nothing here calls Windows.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{
    AdapterError, MemoryStore, PlatformAdapter, ProbeOutcome, ProbeRequest, ResolvedInterface,
    Startup, Timing, TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterErrorKind, BoundedVec, LightReason, MonitorSettings, Outcome, ProbeKind, ProbeTarget,
    ProbeTiming, Request, TargetAddress, TunnelId,
};
use wgal_testkit::FakeClock;

use super::fake::{FakeNet, FakeScm, NetCall, ScmCall, host};
use super::status::{BAD_ROUTE, GENERAL_FAILURE, REQ_TIMED_OUT, SUCCESS};
use super::{
    BestRoute, DefaultRoute, EchoReply, IF_TYPE_PROP_VIRTUAL, Limits, NetError, WindowsAdapter,
};

type Adapter = WindowsAdapter<FakeScm, FakeNet>;

const OFFICE_TARGET: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
/// The fixture's `Ethernet` adapter.
const ETHERNET_LUID: u64 = 1_688_849_877_467_136;
const ETHERNET_INDEX: u32 = 7;
/// The fixture's `office` tunnel adapter.
const OFFICE_LUID: u64 = 14_918_173_849_550_848;
/// The `Ethernet` adapter's IPv6 router, as router advertisements name it.
const GATEWAY_V6: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);

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

fn icmp(address: Ipv4Addr) -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address: TargetAddress::new(address).unwrap(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    }
}

fn echo(status: u32) -> EchoReply {
    EchoReply {
        status,
        from: OFFICE_TARGET,
        rtt_ms: 4,
        payload_matches: true,
    }
}

fn office(adapter: &Adapter) -> ResolvedInterface {
    adapter.resolve_interface(&binding("office")).unwrap()
}

fn probe(adapter: &Adapter) -> Result<ProbeOutcome, AdapterError> {
    adapter.probe(&office(adapter), &icmp(OFFICE_TARGET))
}

fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
    result.unwrap_err().kind
}

fn route_reads(net: &FakeNet) -> usize {
    net.count(|c| *c == NetCall::DefaultRoutes)
}

fn resolutions(net: &FakeNet) -> usize {
    net.count(|c| matches!(c, NetCall::Resolve(..)))
}

/// The router stops answering ARP, as while it reboots.
fn router_down(net: &FakeNet) {
    net.neighbor_answers(ETHERNET_LUID, GATEWAY, Ok(false));
}

fn router_up(net: &FakeNet) {
    net.neighbor_answers(ETHERNET_LUID, GATEWAY, Ok(true));
}

/// The `Ethernet` adapter with only IPv6 for a way out: no IPv4 address but
/// a link-local one, a global and a link-local IPv6 address, and no IPv4
/// default route.
fn ipv6_only(net: &FakeNet) {
    net.adapter_mut("Ethernet", |a| {
        a.ipv4 = vec![Ipv4Addr::new(169, 254, 7, 10)];
        a.ipv6 = vec![
            Ipv6Addr::new(0x2001, 0xdb8, 0, 7, 0, 0, 0, 10),
            Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 10),
        ];
    });
    net.set_default_routes(Vec::new());
}

fn default_route_v6() -> DefaultRoute {
    DefaultRoute {
        if_index: ETHERNET_INDEX,
        luid: ETHERNET_LUID,
        next_hop: GATEWAY_V6.into(),
    }
}

#[test]
fn the_fixture_network_uses_only_documentation_addresses() {
    let host = host();
    let documentation = |a: &IpAddr| match a {
        IpAddr::V4(a) => {
            let [x, y, z, _] = a.octets();
            matches!((x, y, z), (192, 0, 2) | (198, 51, 100) | (203, 0, 113))
        }
        // 2001:db8::/32, or link-local.
        IpAddr::V6(a) => {
            let [x, y, ..] = a.segments();
            (x, y) == (0x2001, 0x0db8) || a.is_unicast_link_local()
        }
    };
    assert!(!host.default_routes.is_empty() && !host.neighbors.is_empty());
    assert!(
        host.default_routes
            .iter()
            .all(|r| documentation(&r.next_hop))
    );
    assert!(host.neighbors.iter().all(|n| documentation(&n.address)));
}

#[test]
fn a_lost_echo_on_a_host_that_is_online_stays_no_reply() {
    let (_, net, adapter) = setup();
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    // One look at the routing table, one question to the gateway.
    assert_eq!(route_reads(&net), 1);
    assert_eq!(
        net.count(|c| *c == NetCall::Resolve(ETHERNET_LUID, GATEWAY.into())),
        1
    );
}

#[test]
fn a_reply_or_a_broken_probe_never_checks_the_network() {
    let (_, net, adapter) = setup();
    net.push_echo(Ok(echo(SUCCESS)));
    assert!(matches!(probe(&adapter), Ok(ProbeOutcome::Reply { .. })));
    net.push_echo(Ok(echo(GENERAL_FAILURE)));
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::Unsupported);
    net.push_echo(Err(NetError::ACCESS_DENIED));
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::PermissionDenied);
    assert_eq!(route_reads(&net), 0);
    assert_eq!(resolutions(&net), 0);
}

/// Codex review of wave three, P1-4: a WireGuard endpoint on the local
/// network needs no default route. An uplink that is up with an address
/// and has no default gateway proves nothing about the way out, so the lost
/// echo is reported as it is.
#[test]
fn p1_4_a_lan_only_uplink_is_not_offline() {
    let (scm, net, adapter) = setup();
    net.set_default_routes(Vec::new());
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    assert_eq!(resolutions(&net), 0);
    assert!(scm.calls().is_empty());
}

/// P1-4: the endpoint reached by a specific route through the uplink (here
/// 198.51.100.0/24 via the LAN), with no default route anywhere.
#[test]
fn p1_4_an_uplink_with_only_a_specific_route_is_not_offline() {
    let (_, net, adapter) = setup();
    net.set_default_routes(Vec::new());
    let endpoint = Ipv4Addr::new(198, 51, 100, 77);
    net.set_route(
        endpoint,
        BestRoute {
            if_index: ETHERNET_INDEX,
            luid: ETHERNET_LUID,
            source: Ipv4Addr::new(192, 0, 2, 10),
        },
    );
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    // The check reads no route but the default ones.
    assert_eq!(net.count(|c| *c == NetCall::BestRoute(endpoint)), 0);
}

/// P1-4: an IPv6-only uplink has no IPv4 default route. Its IPv6 router
/// answers, or there is no IPv6 default route at all: either way nothing
/// proves the host offline.
#[test]
fn p1_4_an_ipv6_only_uplink_is_not_offline() {
    let (_, net, adapter) = setup_with(Limits {
        local_verdict: Duration::ZERO,
        ..Limits::DEFAULT
    });
    ipv6_only(&net);
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    assert_eq!(resolutions(&net), 0);

    net.set_default_routes(vec![default_route_v6()]);
    net.neighbor_answers(ETHERNET_LUID, GATEWAY_V6, Ok(true));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    assert_eq!(
        net.count(|c| *c == NetCall::Resolve(ETHERNET_LUID, GATEWAY_V6.into())),
        1
    );
}

/// P1-4: an IPv6 router that stays silent, as while it reboots, is the
/// same evidence as a silent IPv4 gateway.
#[test]
fn p1_4_a_silent_ipv6_router_is_this_host_offline() {
    let (_, net, adapter) = setup();
    ipv6_only(&net);
    net.set_default_routes(vec![default_route_v6()]);
    net.neighbor_answers(ETHERNET_LUID, GATEWAY_V6, Ok(false));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    let error = probe(&adapter).unwrap_err();
    assert_eq!(error.kind, AdapterErrorKind::LocalNetworkOffline);
    assert!(error.detail.as_str().contains("gateway"), "{error}");
}

/// P1-4: on a dual-stack uplink the host is offline only when the IPv4
/// gateway and the IPv6 router are both silent; a router reboot silences
/// both.
#[test]
fn p1_4_a_dual_stack_host_is_offline_only_when_every_gateway_is_silent() {
    let (_, net, adapter) = setup_with(Limits {
        local_verdict: Duration::ZERO,
        ..Limits::DEFAULT
    });
    net.adapter_mut("Ethernet", |a| {
        a.ipv6 = vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 7, 0, 0, 0, 10)];
    });
    let v4 = host().default_routes[0];
    net.set_default_routes(vec![v4, default_route_v6()]);
    router_down(&net);
    net.neighbor_answers(ETHERNET_LUID, GATEWAY_V6, Ok(true));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);

    net.neighbor_answers(ETHERNET_LUID, GATEWAY_V6, Ok(false));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
}

/// An adapter that is up but has no address to leave the link with (DHCP
/// failed: only a link-local IPv4 address, only a link-local IPv6 one) is
/// no way out, whatever its routes.
#[test]
fn an_uplink_without_a_usable_address_is_this_host_offline() {
    let (_, net, adapter) = setup();
    net.adapter_mut("Ethernet", |a| {
        a.ipv4 = vec![Ipv4Addr::new(169, 254, 7, 10)];
        a.ipv6 = vec![Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 10)];
    });
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    let error = probe(&adapter).unwrap_err();
    assert_eq!(error.kind, AdapterErrorKind::LocalNetworkOffline);
    assert!(error.detail.as_str().contains("usable address"), "{error}");
    assert_eq!(resolutions(&net), 0);
    assert_eq!(route_reads(&net), 0);
}

/// Wi-Fi that lost its access point: the adapter is no longer up, so its
/// default route (if Windows still lists it) is no way out.
#[test]
fn an_uplink_that_went_down_is_this_host_offline() {
    let (_, net, adapter) = setup();
    net.adapter_mut("Ethernet", |a| a.up = false);
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    let error = probe(&adapter).unwrap_err();
    assert_eq!(error.kind, AdapterErrorKind::LocalNetworkOffline);
    assert!(error.detail.as_str().contains("usable address"), "{error}");
    assert_eq!(resolutions(&net), 0);
    assert_eq!(route_reads(&net), 0);
}

/// A full-tunnel configuration routes 0.0.0.0/0 through the tunnel: with
/// the physical uplink gone that is no way out.
#[test]
fn a_default_route_through_the_tunnel_is_no_way_out() {
    let (_, net, adapter) = setup();
    net.adapter_mut("Ethernet", |a| a.up = false);
    net.set_default_routes(vec![
        DefaultRoute {
            if_index: ETHERNET_INDEX,
            luid: ETHERNET_LUID,
            next_hop: GATEWAY.into(),
        },
        DefaultRoute {
            if_index: 21,
            luid: OFFICE_LUID,
            next_hop: Ipv4Addr::UNSPECIFIED.into(),
        },
    ]);
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
    assert_eq!(
        host()
            .adapters
            .iter()
            .find(|a| a.luid == OFFICE_LUID)
            .map(|a| a.if_type),
        Some(IF_TYPE_PROP_VIRTUAL)
    );
}

/// A wired link to a router that is rebooting: the route stays, the router
/// does not answer ARP.
#[test]
fn a_silent_gateway_is_this_host_offline() {
    let (_, net, adapter) = setup();
    router_down(&net);
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    let error = probe(&adapter).unwrap_err();
    assert_eq!(error.kind, AdapterErrorKind::LocalNetworkOffline);
    assert!(error.detail.as_str().contains("gateway"), "{error}");
    // The detail names no address.
    assert!(!error.detail.as_str().contains("192.0.2"), "{error}");
    assert_eq!(resolutions(&net), 1);
}

#[test]
fn a_gateway_that_does_not_answer_in_time_is_silent() {
    let limits = Limits {
        neighbor: Duration::from_millis(100),
        ..Limits::DEFAULT
    };
    let (_, net, adapter) = setup_with(limits);
    net.delay_resolve(Duration::from_millis(1_500));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    let started = Instant::now();
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
    assert!(started.elapsed() < Duration::from_millis(1_000));
}

/// Anything the check cannot read or decide leaves the probe's own result,
/// as before the check existed.
#[test]
fn what_cannot_be_read_or_decided_counts_as_online() {
    let (_, net, adapter) = setup_with(Limits {
        local_verdict: Duration::ZERO,
        ..Limits::DEFAULT
    });
    net.fail_default_routes(Some(NetError::ACCESS_DENIED));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    net.fail_default_routes(None);

    net.neighbor_answers(ETHERNET_LUID, GATEWAY, Err(NetError::ACCESS_DENIED));
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);

    // More default routes than are ever read.
    net.set_default_routes(vec![
        DefaultRoute {
            if_index: ETHERNET_INDEX,
            luid: ETHERNET_LUID,
            next_hop: GATEWAY.into(),
        };
        super::local::MAX_DEFAULT_ROUTES + 1
    ]);
    router_down(&net);
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
}

/// A static neighbor entry for the gateway is never resolved: resolving
/// would replace it. The gateway then counts as unchecked, so online.
#[test]
fn a_static_gateway_entry_is_never_resolved() {
    let mut host = host();
    host.neighbors[0].is_static = true;
    host.neighbors[0].answers = false;
    let net = Arc::new(FakeNet::from_host(&host));
    let adapter = WindowsAdapter::with_parts(
        Arc::new(FakeScm::from_host(&host)),
        Arc::clone(&net),
        Limits::DEFAULT,
    );
    net.push_echo(Ok(echo(REQ_TIMED_OUT)));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    assert_eq!(
        net.count(|c| *c == NetCall::Neighbor(ETHERNET_LUID, GATEWAY.into())),
        1
    );
    assert_eq!(resolutions(&net), 0);
}

/// An echo that could not be sent for lack of a route is a route mismatch
/// on a host that is online, and the host offline otherwise.
#[test]
fn an_echo_without_a_route_is_offline_only_when_the_host_is() {
    let (_, net, adapter) = setup_with(Limits {
        local_verdict: Duration::ZERO,
        ..Limits::DEFAULT
    });
    for (sent, expected) in [
        (
            Err(NetError::NETWORK_UNREACHABLE),
            AdapterErrorKind::RouteMismatch,
        ),
        (Ok(echo(BAD_ROUTE)), AdapterErrorKind::RouteMismatch),
    ] {
        net.push_echo(sent);
        assert_eq!(kind(probe(&adapter)), expected);
    }
    router_down(&net);
    for sent in [Err(NetError::NETWORK_UNREACHABLE), Ok(echo(BAD_ROUTE))] {
        net.push_echo(sent);
        assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
    }
}

/// Sixteen tunnels failing at the same moment share one reading of the
/// routing table and one question to the gateway.
#[test]
fn failures_at_the_same_moment_share_one_check() {
    let (_, net, adapter) = setup();
    net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    // Long enough that the first check is still running when the others
    // fail.
    net.delay_resolve(Duration::from_millis(300));
    let adapter = Arc::new(adapter);
    let interface = office(&adapter);
    let results: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let adapter = Arc::clone(&adapter);
                let interface = interface.clone();
                scope.spawn(move || adapter.probe(&interface, &icmp(OFFICE_TARGET)))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(
        results
            .iter()
            .all(|r| r.as_ref().ok() == Some(&ProbeOutcome::NoReply)),
        "{results:?}"
    );
    assert_eq!(route_reads(&net), 1);
    assert_eq!(resolutions(&net), 1);
}

#[test]
fn the_verdict_is_reused_only_while_it_is_young() {
    let (_, net, adapter) = setup_with(Limits {
        local_verdict: Duration::from_millis(200),
        ..Limits::DEFAULT
    });
    net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    router_down(&net);
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
    // The router is back, but the verdict is still young.
    router_up(&net);
    assert_eq!(kind(probe(&adapter)), AdapterErrorKind::LocalNetworkOffline);
    assert_eq!(resolutions(&net), 1);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(probe(&adapter).unwrap(), ProbeOutcome::NoReply);
    assert_eq!(resolutions(&net), 2);
}

/// What the core does with the adapter's answers: a watchdog over the fake
/// Windows host, advanced one second per tick.
struct Rig {
    wd: Watchdog<Adapter, FakeClock>,
    scm: Arc<FakeScm>,
    net: Arc<FakeNet>,
    reasons: Vec<LightReason>,
}

/// 2026-01-01T00:00:00Z.
const T0: i64 = 1_767_225_600_000;

impl Rig {
    /// `office` registered with 203.0.113.1 as its recovery target, its
    /// baseline passed, automatic recovery on, at t = 35 s. Verdicts on the
    /// host's network are never reused: the fake clock moves much faster
    /// than real time.
    fn new() -> Self {
        let (scm, net, adapter) = setup_with(Limits {
            local_verdict: Duration::ZERO,
            ..Limits::DEFAULT
        });
        net.answer_echoes(Some(echo(SUCCESS)));
        let wd = Watchdog::new(
            adapter,
            FakeClock::new(T0),
            Timing::default(),
            Box::new(MemoryStore::new()),
            Startup::Empty,
        );
        let mut rig = Self {
            wd,
            scm,
            net,
            reasons: Vec::new(),
        };
        let id = TunnelId::new("office").unwrap();
        rig.ok(Request::DiscoverTunnels);
        rig.ok(Request::RegisterTunnel {
            tunnel_id: id.clone(),
        });
        rig.ok(Request::UpdateMonitorSettings {
            tunnel_id: id.clone(),
            settings: MonitorSettings {
                recovery_target: Some(ProbeTarget {
                    address: TargetAddress::new(OFFICE_TARGET).unwrap(),
                    probe: ProbeKind::Icmp,
                }),
                observe_targets: BoundedVec::default(),
                probe: ProbeTiming::DEFAULT,
            },
        });
        rig.wd.tick();
        rig.run(35);
        rig.ok(Request::SetAutoRecovery {
            tunnel_id: id,
            enabled: true,
        });
        rig.run(20);
        assert_eq!(rig.reason(), LightReason::Verified);
        rig.reasons.clear();
        rig
    }

    fn ok(&mut self, request: Request) {
        let outcome = self.wd.handle(&request);
        assert!(
            matches!(outcome, Outcome::Ok(_)),
            "{request:?}: {outcome:?}"
        );
    }

    fn run(&mut self, secs: u64) {
        for _ in 0..secs {
            self.wd.clock().advance(Duration::from_secs(1)).unwrap();
            self.wd.tick();
            let reason = self.reason();
            if self.reasons.last() != Some(&reason) {
                self.reasons.push(reason);
            }
        }
    }

    fn reason(&mut self) -> LightReason {
        self.wd.snapshot().tunnels[0].light_reason
    }

    /// Starts and stops sent to any service.
    fn controls(&self) -> usize {
        self.scm
            .calls()
            .iter()
            .filter(|c| matches!(c, ScmCall::Start(_) | ScmCall::Stop(_)))
            .count()
    }
}

/// The owner's case: the router reboots and the network is gone for more
/// than two minutes. The tunnel is never judged faulty or restarted; once
/// the router is back, a grace period passes before probes count again,
/// and a short wait for the ISP is not a fault either.
#[test]
fn a_router_reboot_is_not_a_tunnel_fault() {
    let mut rig = Rig::new();
    rig.net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    router_down(&rig.net);
    rig.run(180);
    assert_eq!(rig.reason(), LightReason::LocalNetworkOffline);
    for never in [
        LightReason::FaultConfirmed,
        LightReason::Reconnecting,
        LightReason::RecoveryFailed,
    ] {
        assert!(!rig.reasons.contains(&never), "{:?}", rig.reasons);
    }
    assert_eq!(rig.controls(), 0);
    // One question to the gateway per failed round at most, not per second.
    assert!(resolutions(&rig.net) <= 13, "{}", resolutions(&rig.net));

    // The router is back, the ISP 45 s later.
    router_up(&rig.net);
    rig.run(45);
    rig.net.answer_echoes(Some(echo(SUCCESS)));
    rig.run(30);
    assert_eq!(rig.reason(), LightReason::Verified);
    assert_eq!(rig.controls(), 0);
    assert!(
        !rig.reasons.contains(&LightReason::FaultConfirmed),
        "{:?}",
        rig.reasons
    );
}

/// P1-4 through the core: on a host whose only uplink has no default route
/// (a LAN-only or specific-route endpoint), a dead tunnel is a fault and is
/// recovered, never taken for the host being offline.
#[test]
fn p1_4_a_dead_tunnel_on_a_host_without_a_default_route_is_recovered() {
    let mut rig = Rig::new();
    rig.net.set_default_routes(Vec::new());
    rig.net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    rig.run(120);
    assert!(
        rig.reasons.contains(&LightReason::FaultConfirmed)
            || rig.reasons.contains(&LightReason::Reconnecting),
        "{:?}",
        rig.reasons
    );
    assert!(rig.controls() >= 1);
    assert!(
        !rig.reasons.contains(&LightReason::LocalNetworkOffline),
        "{:?}",
        rig.reasons
    );
}

/// P1-4 through the core: the same on an IPv6-only uplink whose router
/// answers.
#[test]
fn p1_4_a_dead_tunnel_on_an_ipv6_only_host_is_recovered() {
    let mut rig = Rig::new();
    ipv6_only(&rig.net);
    rig.net.set_default_routes(vec![default_route_v6()]);
    rig.net
        .neighbor_answers(ETHERNET_LUID, GATEWAY_V6, Ok(true));
    rig.net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    rig.run(120);
    assert!(rig.controls() >= 1, "{:?}", rig.reasons);
    assert!(
        !rig.reasons.contains(&LightReason::LocalNetworkOffline),
        "{:?}",
        rig.reasons
    );
}

/// Wi-Fi without its access point: no adapter is up, so no ARP at all.
#[test]
fn a_lost_access_point_is_not_a_tunnel_fault() {
    let mut rig = Rig::new();
    rig.net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    rig.net.adapter_mut("Ethernet", |a| a.up = false);
    rig.run(300);
    assert_eq!(rig.reason(), LightReason::LocalNetworkOffline);
    assert_eq!(rig.controls(), 0);
    assert_eq!(resolutions(&rig.net), 0);
}

/// The check changes nothing for a real tunnel fault on a host that is
/// online: the fault is confirmed and the tunnel restarted as before.
#[test]
fn a_dead_tunnel_on_a_host_that_is_online_is_still_recovered() {
    let mut rig = Rig::new();
    rig.net.answer_echoes(Some(echo(REQ_TIMED_OUT)));
    rig.run(120);
    assert!(
        rig.reasons.contains(&LightReason::FaultConfirmed)
            || rig.reasons.contains(&LightReason::Reconnecting),
        "{:?}",
        rig.reasons
    );
    assert!(rig.controls() >= 1);
    assert!(
        !rig.reasons.contains(&LightReason::LocalNetworkOffline),
        "{:?}",
        rig.reasons
    );
}
