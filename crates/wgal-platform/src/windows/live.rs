//! On-device checks ("On-device tests" in `docs/testing.md`). Every
//! test here is ignored by default and also refuses to run unless the owner
//! set `WGAL_LIVE_AUTHORIZED=yes-for-this-run`; the control test needs
//! `WGAL_LIVE_CONTROL=yes-for-this-run` as well. They talk to the real SCM and
//! network, so they never run in CI or in an ordinary `cargo test`.
//!
//! Output goes to the owner's own console (`--nocapture`) and stays on that
//! machine: it contains real tunnel names and indexes, and must not be
//! committed (AGENTS.md rule 4).

use std::net::Ipv4Addr;
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{PlatformAdapter, ProbeOutcome, ProbeRequest, TunnelBinding};
use wgal_protocol::{AdapterErrorKind, OsState, ProbeKind, ProbeTarget, TargetAddress};

use super::{SystemIpHelper, SystemServiceControl, WindowsAdapter};

type Live = WindowsAdapter<SystemServiceControl, SystemIpHelper>;

const SETTLE: Duration = Duration::from_secs(30);

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name} for this run"))
}

fn require(name: &str) {
    assert_eq!(
        std::env::var(name).as_deref(),
        Ok("yes-for-this-run"),
        "refusing to run without {name}=yes-for-this-run"
    );
}

fn tunnel() -> TunnelBinding {
    TunnelBinding::new(var("WGAL_LIVE_TUNNEL")).expect("WGAL_LIVE_TUNNEL is a tunnel name")
}

fn target() -> TargetAddress {
    target_from("WGAL_LIVE_TARGET")
}

fn target_from(name: &str) -> TargetAddress {
    let address: Ipv4Addr = var(name)
        .parse()
        .unwrap_or_else(|_| panic!("{name} is an IPv4 address"));
    TargetAddress::new(address).unwrap_or_else(|_| panic!("{name} is a unicast host address"))
}

fn icmp_request(address: TargetAddress) -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address,
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    }
}

fn wait_for(adapter: &Live, tunnel: &TunnelBinding, want: OsState) -> Duration {
    let started = Instant::now();
    loop {
        let state = adapter.query_state(tunnel);
        if state.as_ref().ok() == Some(&want) {
            return started.elapsed();
        }
        assert!(
            started.elapsed() < SETTLE,
            "still {state:?} after {SETTLE:?}"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

/// W1: read-only. Lists tunnel services, queries one, resolves its adapter.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w1_discover_query_resolve() {
    require("WGAL_LIVE_AUTHORIZED");
    let adapter = WindowsAdapter::new();
    let tunnel = tunnel();
    let found = adapter.discover().unwrap();
    println!("tunnel services found: {}", found.len());
    println!(
        "selected tunnel listed: {}",
        found.iter().any(|t| t.binding == tunnel)
    );
    println!("query_state: {:?}", adapter.query_state(&tunnel));
    println!(
        "resolve_interface: {:?}",
        adapter.resolve_interface(&tunnel)
    );
}

/// W3: read-only apart from the echoes. Route evidence and five probes.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w3_route_and_bound_probe() {
    require("WGAL_LIVE_AUTHORIZED");
    let adapter = WindowsAdapter::new();
    let interface = adapter.resolve_interface(&tunnel()).unwrap();
    println!(
        "check_route: {:?}",
        adapter.check_route(&interface, target())
    );
    let request = icmp_request(target());
    for round in 1..=5 {
        println!("probe {round}: {:?}", adapter.probe(&interface, &request));
        thread::sleep(Duration::from_secs(1));
    }
}

/// W3 item 3 on a machine with one tunnel: `WGAL_LIVE_OFF_TUNNEL_TARGET` is
/// an address Windows routes through another interface, such as the
/// physical adapter's gateway. Read-only: the route lookup must refuse it as
/// `RouteMismatch`, so no echo is sent at all and no probe is ever a reply.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w3_off_tunnel_target_is_refused() {
    require("WGAL_LIVE_AUTHORIZED");
    let adapter = WindowsAdapter::new();
    let interface = adapter.resolve_interface(&tunnel()).unwrap();
    let target = target_from("WGAL_LIVE_OFF_TUNNEL_TARGET");
    let route = adapter.check_route(&interface, target);
    println!("check_route: {route:?}");
    assert_eq!(
        route.map(|_| ()).map_err(|e| e.kind),
        Err(AdapterErrorKind::RouteMismatch)
    );
    let request = icmp_request(target);
    for round in 1..=5 {
        let probe = adapter.probe(&interface, &request);
        println!("probe {round}: {probe:?}");
        assert_eq!(
            probe.map(|_| ()).map_err(|e| e.kind),
            Err(AdapterErrorKind::RouteMismatch)
        );
    }
}

/// The one-tunnel stand-in for part of W3 item 2: `WGAL_LIVE_SILENT_TARGET`
/// is an address inside the tunnel's routes that nothing answers (unused on
/// the far side). Its route must be the tunnel's and every one of the five
/// echoes must be `NoReply`: a silent far side never shows up as a reply.
/// It does not show that a target another tunnel can reach fails here:
/// that needs two tunnels (W3 item 2, not run).
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w3_silent_target_in_the_tunnel_gets_no_reply() {
    require("WGAL_LIVE_AUTHORIZED");
    let adapter = WindowsAdapter::new();
    let interface = adapter.resolve_interface(&tunnel()).unwrap();
    let target = target_from("WGAL_LIVE_SILENT_TARGET");
    let route = adapter.check_route(&interface, target);
    println!("check_route: {route:?}");
    assert!(
        route.is_ok(),
        "the silent target is not routed through the tunnel"
    );
    let request = icmp_request(target);
    for round in 1..=5 {
        let probe = adapter.probe(&interface, &request);
        println!("probe {round}: {probe:?}");
        assert!(
            matches!(probe, Ok(ProbeOutcome::NoReply)),
            "expected no reply"
        );
        thread::sleep(Duration::from_secs(1));
    }
}

/// W9 step 1 (open finding D-3), with the network up: a silent target in the
/// tunnel (`WGAL_LIVE_SILENT_TARGET`, as in W3) gets `NoReply` five times,
/// never `LocalNetworkOffline`, so the check of this host's own network finds
/// this machine online. Apart from the echoes it reads the routing table and
/// asks the default gateway for its link-layer address (Windows replaces its
/// dynamic neighbor entry for the gateway with a fresh one); it changes
/// nothing else.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w9_silent_target_with_the_network_up_is_not_offline() {
    require("WGAL_LIVE_AUTHORIZED");
    let adapter = WindowsAdapter::new();
    let interface = adapter.resolve_interface(&tunnel()).unwrap();
    let request = icmp_request(target_from("WGAL_LIVE_SILENT_TARGET"));
    for round in 1..=5 {
        let probe = adapter.probe(&interface, &request);
        println!("probe {round}: {probe:?}");
        assert!(
            matches!(probe, Ok(ProbeOutcome::NoReply)),
            "expected no reply, and this host online"
        );
        // Past the 5 s the verdict is kept, so every probe checks afresh.
        thread::sleep(Duration::from_secs(6));
    }
}

/// W9 step 2 (D-3): probes `WGAL_LIVE_TARGET` through the tunnel every 5 s
/// for `WGAL_LIVE_W9_SECONDS` (default 300) and prints each outcome with the
/// seconds since the start. The test changes nothing itself; the owner takes
/// the local network away and brings it back meanwhile, notes the times,
/// and compares the printed outcomes with them. It
/// asserts nothing about the outcomes.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w9_probe_through_an_outage() {
    require("WGAL_LIVE_AUTHORIZED");
    let seconds: u64 = std::env::var("WGAL_LIVE_W9_SECONDS")
        .ok()
        .map(|s| {
            s.parse()
                .expect("WGAL_LIVE_W9_SECONDS is a number of seconds")
        })
        .unwrap_or(300);
    let adapter = WindowsAdapter::new();
    let tunnel = tunnel();
    let request = icmp_request(target());
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let at = started.elapsed().as_secs();
        let probe = adapter
            .resolve_interface(&tunnel)
            .and_then(|interface| adapter.probe(&interface, &request));
        match &probe {
            Ok(outcome) => println!("{at:>4} s: {outcome:?}"),
            Err(error) => println!("{at:>4} s: {:?}: {}", error.kind, error.detail),
        }
        thread::sleep(Duration::from_secs(5));
    }
}

/// W4: stops and starts ONE tunnel through the SCM and checks that no other
/// tunnel service changed state. Needs the automatic revert from the
/// document running in a separate console first.
#[test]
#[ignore = "on-device only; see docs/testing.md"]
fn w4_control_only_the_named_tunnel() {
    require("WGAL_LIVE_AUTHORIZED");
    require("WGAL_LIVE_CONTROL");
    let adapter = WindowsAdapter::new();
    let tunnel = tunnel();
    let others: Vec<_> = adapter
        .discover()
        .unwrap()
        .into_iter()
        .map(|t| t.binding)
        .filter(|b| *b != tunnel)
        .collect();
    let before: Vec<_> = others.iter().map(|b| adapter.query_state(b)).collect();
    let old = adapter.resolve_interface(&tunnel).unwrap();

    adapter.stop(&tunnel).unwrap();
    println!(
        "stopped after {:?}",
        wait_for(&adapter, &tunnel, OsState::Disconnected)
    );
    adapter.start(&tunnel).unwrap();
    println!(
        "started after {:?}",
        wait_for(&adapter, &tunnel, OsState::Connected)
    );

    let new = adapter.resolve_interface(&tunnel).unwrap();
    println!("interface index changed: {}", old.index != new.index);
    println!(
        "interface LUID (incarnation) changed: {}",
        old.incarnation != new.incarnation
    );
    let after: Vec<_> = others.iter().map(|b| adapter.query_state(b)).collect();
    assert_eq!(before, after, "another tunnel service changed state");
}
