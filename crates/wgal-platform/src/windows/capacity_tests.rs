//! FIX-08 (AR-09 P2, N-04): the adapter's helper threads, those left
//! running after their caller stopped waiting included, stay within their
//! shares, a slot is given back only when its thread's call has returned,
//! and probe reads never take the share kept for the tunnel services'
//! stops, starts and state queries. Against the fake SCM and IP Helper;
//! nothing here calls Windows. Every test releases what it held and waits
//! until no fake call is under way and every slot is back, so no helper
//! thread outlives it.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{AdapterError, PlatformAdapter, ProbeRequest, TunnelBinding};
use wgal_protocol::{AdapterErrorKind, OsState, ProbeKind, ProbeTarget, TargetAddress};

use super::fake::{FakeNet, FakeScm, NetCall, ScmCall, host};
use super::status::REQ_TIMED_OUT;
use super::{DefaultRoute, EchoReply, Limits, WindowsAdapter, local};

type Adapter = WindowsAdapter<FakeScm, FakeNet>;

const OFFICE: &str = "office";
const OFFICE_SERVICE: &str = "WireGuardTunnel$office";
const LAB_SERVICE: &str = "WireGuardTunnel$lab";
const OFFICE_TARGET: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
/// The fixture's `Ethernet` adapter, the host's uplink.
const ETHERNET_LUID: u64 = 1_688_849_877_467_136;
const ETHERNET_INDEX: u32 = 7;
/// Invented default gateways on the uplink, none of which answers in these
/// tests: their resolutions block until the test releases them. Three, so
/// the read share does not fill in whole checks.
const GATEWAYS: [Ipv4Addr; 3] = [
    Ipv4Addr::new(192, 0, 2, 1),
    Ipv4Addr::new(198, 51, 100, 1),
    Ipv4Addr::new(203, 0, 113, 254),
];
/// Long enough for every thread a test leaves behind to finish once
/// released.
const DRAIN: Duration = Duration::from_secs(10);

fn setup_with(limits: Limits) -> (Arc<FakeScm>, Arc<FakeNet>, Adapter) {
    let host = host();
    let scm = Arc::new(FakeScm::from_host(&host));
    let net = Arc::new(FakeNet::from_host(&host));
    let adapter = WindowsAdapter::with_parts(Arc::clone(&scm), Arc::clone(&net), limits);
    (scm, net, adapter)
}

fn binding(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

fn icmp() -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address: TargetAddress::new(OFFICE_TARGET).unwrap(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_millis(20),
    }
}

fn detail<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> (AdapterErrorKind, String) {
    let error = result.unwrap_err();
    (error.kind, error.detail.as_str().to_owned())
}

/// Every echo is lost and every gateway stays silent, as when the host's
/// router is gone and resolving it never returns: each failed probe checks
/// the host's own network again (no reuse) and asks every gateway.
fn stuck_gateways(net: &FakeNet) {
    net.answer_echoes(Some(EchoReply {
        status: REQ_TIMED_OUT,
        from: Ipv4Addr::UNSPECIFIED,
        rtt_ms: 0,
        payload_matches: false,
    }));
    net.set_default_routes(
        GATEWAYS
            .iter()
            .map(|&g| DefaultRoute {
                if_index: ETHERNET_INDEX,
                luid: ETHERNET_LUID,
                next_hop: IpAddr::V4(g),
            })
            .collect(),
    );
    net.hold_resolutions();
}

fn quick_checks() -> Limits {
    Limits {
        query: Duration::from_millis(200),
        control: Duration::from_millis(500),
        settle_poll: Duration::from_millis(10),
        neighbor: Duration::from_millis(20),
        echo_grace: Duration::from_millis(20),
        local_verdict: Duration::ZERO,
        ..Limits::DEFAULT
    }
}

/// Waits up to [`DRAIN`] until `adapter` holds exactly `count` slots.
fn wait_for_threads(adapter: &Adapter, count: usize) -> bool {
    let deadline = Instant::now() + DRAIN;
    while adapter.helper_threads() != count {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    true
}

/// Releases everything held, then waits until every fake call has returned
/// and every helper thread has given its slot back.
fn drain(scm: &FakeScm, net: &FakeNet, adapter: &Adapter) {
    net.release();
    scm.unstall();
    assert!(net.wait_until_idle(DRAIN), "fake calls still under way");
    assert!(
        wait_for_threads(adapter, 0),
        "{} helper threads still hold a slot",
        adapter.helper_threads()
    );
}

/// N-04: gateways that never answer used to leave a thread each behind at
/// every check, without limit. Now the gateway threads count in the read
/// share, and once it is full a probe is broken (`Timeout`), never a lost
/// echo the core would count against the target.
#[test]
fn fix08_gateway_threads_that_never_end_stay_within_the_read_share() {
    let (scm, net, adapter) = setup_with(quick_checks());
    stuck_gateways(&net);
    let interface = adapter.resolve_interface(&binding(OFFICE)).unwrap();
    let cap = Limits::DEFAULT.read_threads;
    let mut results = Vec::new();
    for _ in 0..(cap / GATEWAYS.len() + 8) {
        results.push(adapter.probe(&interface, &icmp()));
        assert!(
            net.busy_calls() <= cap && adapter.helper_threads() <= cap,
            "{} fake calls under way, {} slots taken, more than {cap}",
            net.busy_calls(),
            adapter.helper_threads()
        );
    }
    let most = net.most_busy_calls();
    let stuck = net.count(|c| matches!(c, NetCall::Resolve(..)));
    drain(&scm, &net, &adapter);
    assert!(most <= cap, "{most} helper threads were alive at once");
    // The share filled with gateway threads and then asked no more.
    assert_eq!(stuck, cap, "gateway resolutions started");
    for result in &results {
        assert!(
            matches!(
                result,
                Err(e) if matches!(
                    e.kind,
                    AdapterErrorKind::LocalNetworkOffline | AdapterErrorKind::Timeout
                )
            ),
            "{result:?}"
        );
    }
    let (kind, text) = detail(results.pop().unwrap());
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
}

/// The capacity requirement of AR-09: reads stuck in Windows must not keep
/// a stop, the state query after it and the start from starting.
#[test]
fn fix08_a_full_read_share_leaves_stop_poll_and_start_working() {
    let (scm, net, adapter) = setup_with(quick_checks());
    stuck_gateways(&net);
    let interface = adapter.resolve_interface(&binding(OFFICE)).unwrap();
    // Fill the read share with gateway threads that never end.
    for _ in 0..(Limits::DEFAULT.read_threads / GATEWAYS.len() + 2) {
        let _ = adapter.probe(&interface, &icmp());
    }
    let (kind, text) = detail(adapter.probe(&interface, &icmp()));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
    let office = binding(OFFICE);
    let control = (|| {
        adapter.stop(&office)?;
        // The service stops, as the SCM reports once it has.
        scm.set_state(OFFICE_SERVICE, 1);
        let stopped = adapter.query_state(&office)?;
        adapter.start(&office)?;
        let starting = adapter.query_state(&office)?;
        Ok::<_, AdapterError>((stopped, starting))
    })();
    // Reads are still refused.
    assert_eq!(
        detail(adapter.resolve_interface(&office)).0,
        AdapterErrorKind::Timeout
    );
    drain(&scm, &net, &adapter);
    assert_eq!(
        control.unwrap(),
        (OsState::Disconnected, OsState::Transitioning)
    );
    assert_eq!(scm.state_of(OFFICE_SERVICE), Some(2));
    // Once the stuck threads have ended, reads start again.
    assert!(adapter.resolve_interface(&office).is_ok());
}

/// A slot is held by its thread until the thread's call returns, not until
/// its caller stops waiting.
#[test]
fn fix08_a_slot_comes_back_when_its_call_returns_not_when_its_caller_gives_up() {
    let (scm, net, adapter) = setup_with(Limits {
        read_threads: 1,
        ..quick_checks()
    });
    let interface = adapter.resolve_interface(&binding(OFFICE)).unwrap();
    assert_eq!(adapter.helper_threads(), 0);
    net.hold_echoes();
    net.answer_echoes(Some(EchoReply {
        status: super::status::SUCCESS,
        from: OFFICE_TARGET,
        rtt_ms: 4,
        payload_matches: true,
    }));
    // The caller gives up on the echo; its thread is still in Windows.
    let (kind, text) = detail(adapter.probe(&interface, &icmp()));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("did not finish in time"), "{text}");
    assert_eq!(adapter.helper_threads(), 1);
    assert_eq!(net.busy_calls(), 1);
    let listings = net.count(|c| *c == NetCall::Adapters);
    let (kind, text) = detail(adapter.resolve_interface(&binding(OFFICE)));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
    assert_eq!(
        net.count(|c| *c == NetCall::Adapters),
        listings,
        "a refused read reached Windows"
    );
    drain(&scm, &net, &adapter);
    assert!(adapter.resolve_interface(&binding(OFFICE)).is_ok());
}

/// SCM queries stuck for one service use up that service's own query slots
/// only: another tunnel is still stopped, polled and started, and so is the
/// service itself (Codex review of FIX-08, P2-02): its starts and stops have
/// their own slots, and its polls while it is being controlled too.
#[test]
fn fix08_one_services_stuck_queries_leave_every_tunnel_controllable() {
    let (scm, net, adapter) = setup_with(Limits {
        query: Duration::from_millis(30),
        service_threads: 2,
        ..quick_checks()
    });
    scm.stall_next(ScmCall::Query(LAB_SERVICE.to_owned()), 2);
    let lab = binding("lab");
    for _ in 0..2 {
        let (kind, text) = detail(adapter.query_state(&lab));
        assert_eq!(kind, AdapterErrorKind::Timeout);
        assert!(text.contains("did not finish in time"), "{text}");
    }
    // Its ordinary queries are used up, and it is not being controlled.
    let (kind, text) = detail(adapter.query_state(&lab));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
    let queries = |scm: &FakeScm| {
        scm.calls()
            .into_iter()
            .filter(|c| *c == ScmCall::Query(LAB_SERVICE.to_owned()))
            .count()
    };
    assert_eq!(queries(&scm), 2, "{:?}", scm.calls());

    // The stuck service itself: stop, poll, start, poll.
    adapter.stop(&lab).unwrap();
    scm.set_state(LAB_SERVICE, 1);
    assert_eq!(adapter.query_state(&lab).unwrap(), OsState::Disconnected);
    adapter.start(&lab).unwrap();
    assert_eq!(adapter.query_state(&lab).unwrap(), OsState::Transitioning);

    // Another tunnel too.
    let office = binding(OFFICE);
    adapter.stop(&office).unwrap();
    scm.set_state(OFFICE_SERVICE, 1);
    assert_eq!(adapter.query_state(&office).unwrap(), OsState::Disconnected);
    adapter.start(&office).unwrap();
    assert_eq!(
        adapter.query_state(&office).unwrap(),
        OsState::Transitioning
    );
    drain(&scm, &net, &adapter);
    // Its slots back, the service can be asked again as usual.
    assert!(adapter.query_state(&lab).is_ok());
}

/// The polls of a service being controlled are bounded too, and a service
/// that is not being controlled has none.
#[test]
fn fix08_polls_while_controlled_are_bounded_and_only_while_controlled() {
    let (scm, net, adapter) = setup_with(Limits {
        query: Duration::from_millis(30),
        service_threads: 1,
        service_polls: 1,
        ..quick_checks()
    });
    let lab = binding("lab");
    scm.stall(ScmCall::Query(LAB_SERVICE.to_owned()));
    assert_eq!(
        detail(adapter.query_state(&lab)).0,
        AdapterErrorKind::Timeout
    );
    assert!(
        detail(adapter.query_state(&lab))
            .1
            .contains("was not started")
    );
    adapter.stop(&lab).unwrap();
    // One poll, stuck too; then none.
    assert!(
        detail(adapter.query_state(&lab))
            .1
            .contains("did not finish in time")
    );
    assert!(
        detail(adapter.query_state(&lab))
            .1
            .contains("was not started")
    );
    drain(&scm, &net, &adapter);
}

/// The tunnel services' share has a total too, beyond each service's part.
#[test]
fn fix08_the_services_share_is_bounded_in_total() {
    let (scm, net, adapter) = setup_with(Limits {
        query: Duration::from_millis(30),
        control_threads: 2,
        ..quick_checks()
    });
    scm.stall(ScmCall::Query(LAB_SERVICE.to_owned()));
    for _ in 0..2 {
        assert_eq!(
            detail(adapter.query_state(&binding("lab"))).0,
            AdapterErrorKind::Timeout
        );
    }
    let (kind, text) = detail(adapter.query_state(&binding(OFFICE)));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
    assert!(
        !scm.calls()
            .contains(&ScmCall::Query(OFFICE_SERVICE.to_owned()))
    );
    // Reads are a share of their own.
    assert!(adapter.resolve_interface(&binding(OFFICE)).is_ok());
    drain(&scm, &net, &adapter);
    assert_eq!(
        adapter.query_state(&binding(OFFICE)).unwrap(),
        OsState::Connected
    );
}

/// The defaults leave room for what the runtime can have waiting at once:
/// every worker on one read and one network check on every gateway, and
/// every tunnel the watchdog monitors its own part of the services' share.
#[test]
fn fix08_the_default_shares_cover_the_runtimes_own_concurrency() {
    let limits = Limits::DEFAULT;
    let workers = usize::from(wgal_protocol::MAX_WORKERS);
    assert!(limits.read_threads >= 2 * (workers + local::MAX_GATEWAYS));
    assert!(limits.service_threads >= 2);
    assert!(limits.control_threads >= wgal_protocol::MAX_TUNNELS * limits.service_threads);
    assert!(limits.service_commands >= 2);
    assert!(limits.command_threads >= wgal_protocol::MAX_TUNNELS * limits.service_commands);
    assert!(limits.service_polls >= 1);
    assert!(limits.poll_threads >= wgal_protocol::MAX_TUNNELS * limits.service_polls);
}

/// Codex review of FIX-08, round 2 (P3): the polls of a service being
/// controlled end with the control window. With a poll slot still free, a
/// query after the window is refused like any other once the ordinary
/// queries are used up.
#[test]
fn fix08_polls_end_with_the_control_window() {
    let (scm, net, adapter) = setup_with(Limits {
        query: Duration::from_millis(30),
        service_threads: 1,
        service_polls: 2,
        ..quick_checks()
    });
    let window = quick_checks().control * 2;
    let lab = binding("lab");
    scm.stall(ScmCall::Query(LAB_SERVICE.to_owned()));
    assert!(
        detail(adapter.query_state(&lab))
            .1
            .contains("did not finish in time")
    );
    adapter.stop(&lab).unwrap();
    let stopped = Instant::now();
    assert!(
        detail(adapter.query_state(&lab))
            .1
            .contains("did not finish in time")
    );
    thread::sleep(window.saturating_sub(stopped.elapsed()) + Duration::from_millis(100));
    let (kind, text) = detail(adapter.query_state(&lab));
    assert_eq!(kind, AdapterErrorKind::Timeout);
    assert!(text.contains("was not started"), "{text}");
    drain(&scm, &net, &adapter);
}
