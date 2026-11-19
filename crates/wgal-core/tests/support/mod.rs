//! Test rig for the watchdog: a [`FakePlatform`] with data-path faults that
//! the platform itself does not report, a [`FakeClock`], and helpers that
//! drive the watchdog second by second.

#![allow(clippy::unwrap_used, dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use wgal_core::{
    AdapterError, Clock, DiscoveredBinding, MemoryStore, PlatformAdapter, ProbeOutcome,
    ProbeRequest, ResolvedInterface, RouteEvidence, Startup, Timing, TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, BoundedVec, Capability, LogEvent, MonitorSettings,
    OsState, Outcome, ProbeKind, ProbeTarget, ProbeTiming, Request, Response, TargetAddress,
    TunnelId, TunnelStatus,
};
use wgal_testkit::{Call, FakeClock, FakePlatform};

/// 2026-01-01T00:00:00Z.
pub const T0: i64 = 1_767_225_600_000;

pub const ALL_SUPPORTED: AdapterCapabilities = AdapterCapabilities {
    query_state: Capability::Supported,
    start_stop: Capability::Supported,
    bound_icmp_probe: Capability::Supported,
    bound_tcp_probe: Capability::Supported,
    read_metrics: Capability::Unsupported,
    verified_concurrent_tunnels: 3,
    background_operation: Capability::Unknown,
};

pub fn binding(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

pub fn id(name: &str) -> TunnelId {
    TunnelId::new(name).unwrap()
}

/// A probe timing that must be valid.
pub fn timing(interval: u16, timeout: u8, retries: u8, min_fault: u16) -> ProbeTiming {
    ProbeTiming::new(interval, timeout, retries, min_fault).unwrap()
}

pub fn address(last: u8) -> TargetAddress {
    TargetAddress::new(Ipv4Addr::new(192, 0, 2, last)).unwrap()
}

pub fn icmp(last: u8) -> ProbeTarget {
    ProbeTarget {
        address: address(last),
        probe: ProbeKind::Icmp,
    }
}

pub fn tcp(last: u8, port: u16) -> ProbeTarget {
    ProbeTarget {
        address: address(last),
        probe: ProbeKind::TcpConnect {
            port: std::num::NonZeroU16::new(port).unwrap(),
        },
    }
}

/// Round-trip time the lab reports for a refused connection.
pub const REFUSED_RTT: Duration = Duration::from_millis(7);

/// [`FakePlatform`] plus faults in the data path: the manager still reports
/// the tunnel connected, but probes get no reply.
#[derive(Debug, Default)]
pub struct Lab {
    pub fake: FakePlatform,
    knobs: Mutex<Knobs>,
}

#[derive(Debug, Default)]
struct Knobs {
    bindings: Vec<TunnelBinding>,
    /// Interfaces whose data path is dead. A restart creates a new interface,
    /// which works again.
    dead_interfaces: BTreeSet<String>,
    /// Tunnels whose data path stays dead whatever interface they get.
    dead_tunnels: BTreeSet<TunnelBinding>,
    dead_targets: BTreeSet<TargetAddress>,
    /// Targets that answer through the tunnel but refuse the connection.
    refusing_targets: BTreeSet<TargetAddress>,
    probe_errors: BTreeMap<TunnelBinding, AdapterErrorKind>,
    /// Rebuild the tunnel's interface right after its next probe.
    rebuild_after_probe: BTreeSet<TunnelBinding>,
    /// Rebuild it in place (same name, index and address, new incarnation)
    /// right after its next probe.
    rebuild_in_place_after_probe: BTreeSet<TunnelBinding>,
    /// While the tunnel's next probe runs, move this target's route to
    /// another tunnel.
    reroute_during_probe: BTreeMap<TunnelBinding, (TargetAddress, TunnelBinding)>,
}

impl Lab {
    pub fn new(tunnels: &[&str]) -> Self {
        let lab = Self::default();
        lab.fake.set_capabilities(ALL_SUPPORTED);
        for name in tunnels {
            lab.fake
                .add_tunnel(binding(name), &format!("Tunnel {name}"), OsState::Connected);
            lab.knobs().bindings.push(binding(name));
        }
        lab
    }

    fn knobs(&self) -> MutexGuard<'_, Knobs> {
        self.knobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Kills the data path of the tunnel's current interface.
    pub fn break_until_restart(&self, name: &str) {
        let interface = self.fake.interface(&binding(name)).unwrap();
        self.knobs().dead_interfaces.insert(interface.name);
    }

    pub fn break_for_good(&self, name: &str) {
        self.knobs().dead_tunnels.insert(binding(name));
    }

    pub fn heal(&self, name: &str) {
        let mut knobs = self.knobs();
        knobs.dead_tunnels.remove(&binding(name));
        knobs.dead_interfaces.clear();
    }

    pub fn kill_target(&self, last: u8) {
        self.knobs().dead_targets.insert(address(last));
    }

    pub fn refuse_target(&self, last: u8, refusing: bool) {
        let mut knobs = self.knobs();
        if refusing {
            knobs.refusing_targets.insert(address(last));
        } else {
            knobs.refusing_targets.remove(&address(last));
        }
    }

    pub fn fail_probes(&self, name: &str, kind: Option<AdapterErrorKind>) {
        let mut knobs = self.knobs();
        match kind {
            Some(kind) => knobs.probe_errors.insert(binding(name), kind),
            None => knobs.probe_errors.remove(&binding(name)),
        };
    }

    pub fn rebuild_after_next_probe(&self, name: &str) {
        self.knobs().rebuild_after_probe.insert(binding(name));
    }

    pub fn rebuild_in_place_after_next_probe(&self, name: &str) {
        self.knobs()
            .rebuild_in_place_after_probe
            .insert(binding(name));
    }

    /// During `name`'s next probe, the route to 192.0.2.`last` moves to `to`.
    pub fn reroute_during_next_probe(&self, name: &str, last: u8, to: &str) {
        self.knobs()
            .reroute_during_probe
            .insert(binding(name), (address(last), binding(to)));
    }

    fn owner(&self, interface: &ResolvedInterface) -> Option<TunnelBinding> {
        let bindings = self.knobs().bindings.clone();
        bindings
            .into_iter()
            .find(|b| self.fake.interface(b).as_ref() == Some(interface))
    }
}

impl PlatformAdapter for Lab {
    fn capabilities(&self) -> AdapterCapabilities {
        self.fake.capabilities()
    }

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        self.fake.discover()
    }

    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.fake.check_registration(tunnel)
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        self.fake.query_state(tunnel)
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        self.fake.resolve_interface(tunnel)
    }

    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        self.fake.check_route(interface, target)
    }

    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        let owner = self.owner(interface);
        let result = self.fake.probe(interface, request)?;
        let mut knobs = self.knobs();
        if let Some(owner) = &owner {
            if knobs.rebuild_after_probe.remove(owner) {
                drop(knobs);
                self.fake.recreate_interface(owner);
                knobs = self.knobs();
            }
            if knobs.rebuild_in_place_after_probe.remove(owner) {
                drop(knobs);
                self.fake.rebuild_interface_in_place(owner);
                knobs = self.knobs();
            }
            if let Some((target, to)) = knobs.reroute_during_probe.remove(owner) {
                drop(knobs);
                self.fake.route_via(target, Some(&to));
                knobs = self.knobs();
            }
            if let Some(kind) = knobs.probe_errors.get(owner) {
                return Err(AdapterError::new(*kind, "injected probe error"));
            }
        }
        let dead = knobs.dead_interfaces.contains(&interface.name)
            || owner.is_some_and(|o| knobs.dead_tunnels.contains(&o))
            || knobs.dead_targets.contains(&request.target.address);
        Ok(if dead {
            ProbeOutcome::NoReply
        } else if knobs.refusing_targets.contains(&request.target.address) {
            ProbeOutcome::Refused { rtt: REFUSED_RTT }
        } else {
            result
        })
    }

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.fake.start(tunnel)
    }

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.fake.stop(tunnel)
    }
}

pub type Wd = Watchdog<Lab, FakeClock>;

/// A watchdog over a [`Lab`], advanced one second per tick.
#[derive(Debug)]
pub struct Rig {
    pub wd: Wd,
    pub store: MemoryStore,
}

impl Rig {
    /// Registers each tunnel with recovery target 192.0.2.(i + 1), runs past
    /// the startup grace period so every baseline passes, and returns at
    /// t = 35 s. Auto recovery is still off.
    pub fn new(tunnels: &[&str]) -> Self {
        Self::with_lab(Lab::new(tunnels), tunnels)
    }

    pub fn with_lab(lab: Lab, tunnels: &[&str]) -> Self {
        let store = MemoryStore::new();
        let wd = Watchdog::new(
            lab,
            FakeClock::new(T0),
            Timing::default(),
            Box::new(store.clone()),
            Startup::Empty,
        );
        let mut rig = Self { wd, store };
        rig.ok(Request::DiscoverTunnels);
        for (i, name) in tunnels.iter().enumerate() {
            rig.ok(Request::RegisterTunnel {
                tunnel_id: id(name),
            });
            let last = u8::try_from(i + 1).unwrap();
            rig.set_targets(name, Some(icmp(last)), &[]);
        }
        rig.wd.tick();
        rig.run(35);
        rig
    }

    pub fn lab(&self) -> &Lab {
        self.wd.adapter()
    }

    pub fn now_secs(&self) -> u64 {
        self.wd.clock().now().since_origin().as_secs()
    }

    pub fn set_targets(
        &mut self,
        name: &str,
        recovery: Option<ProbeTarget>,
        observe: &[ProbeTarget],
    ) {
        // The tunnel's probe timing stays as it is, as the GUI sends it.
        let probe = self.status(name).probe;
        self.ok(Request::UpdateMonitorSettings {
            tunnel_id: id(name),
            settings: MonitorSettings {
                recovery_target: recovery,
                observe_targets: BoundedVec::try_from(observe.to_vec()).unwrap(),
                probe,
            },
        });
    }

    /// Gives the tunnel a new probe timing and keeps its targets, as the GUI
    /// sends it.
    pub fn set_probe(&mut self, name: &str, probe: ProbeTiming) {
        let status = self.status(name);
        let observe: Vec<ProbeTarget> = status.observe_targets.iter().map(|t| t.target).collect();
        self.ok(Request::UpdateMonitorSettings {
            tunnel_id: id(name),
            settings: MonitorSettings {
                recovery_target: status.recovery_target.map(|t| t.target),
                observe_targets: BoundedVec::try_from(observe).unwrap(),
                probe,
            },
        });
    }

    /// The core's events since the last call, as `take_events` hands them
    /// over.
    pub fn events(&mut self) -> Vec<(Option<TunnelId>, LogEvent)> {
        self.wd.take_events()
    }

    /// The events about one tunnel since the last call (the others are
    /// dropped).
    pub fn events_of(&mut self, name: &str) -> Vec<LogEvent> {
        let tunnel = id(name);
        self.events()
            .into_iter()
            .filter(|(t, _)| t.as_ref() == Some(&tunnel))
            .map(|(_, event)| event)
            .collect()
    }

    /// The host woke from sleep.
    pub fn wake(&mut self) {
        self.wd.on_wake();
    }

    /// The host's network changed.
    pub fn network_change(&mut self) {
        self.wd.on_network_change();
    }

    /// Calls the platform got for the tunnel: every kind, probes included.
    pub fn adapter_calls(&self, name: &str) -> usize {
        self.calls(name).len()
    }

    /// Advances the clock one second at a time, ticking after each.
    pub fn run(&mut self, secs: u64) {
        for _ in 0..secs {
            self.wd.clock().advance(Duration::from_secs(1)).unwrap();
            self.wd.tick();
        }
    }

    /// Runs until `t` seconds since the clock's origin.
    pub fn run_until(&mut self, t: u64) {
        let now = self.now_secs();
        assert!(t >= now, "already at {now} s");
        self.run(t - now);
    }

    pub fn request(&mut self, request: Request) -> Outcome {
        self.wd.handle(&request)
    }

    pub fn ok(&mut self, request: Request) -> Response {
        match self.request(request.clone()) {
            Outcome::Ok(response) => response,
            Outcome::Err(e) => panic!("{request:?} was rejected: {e:?}"),
        }
    }

    pub fn manage(&mut self, name: &str) {
        self.ok(Request::SetAutoRecovery {
            tunnel_id: id(name),
            enabled: true,
        });
    }

    pub fn status(&mut self, name: &str) -> TunnelStatus {
        let snapshot = self.wd.snapshot();
        snapshot
            .tunnels
            .iter()
            .find(|t| t.tunnel_id == id(name))
            .cloned()
            .unwrap()
    }

    pub fn calls(&self, name: &str) -> Vec<Call> {
        self.lab().fake.calls_for(&binding(name))
    }

    pub fn stops(&self, name: &str) -> usize {
        self.calls(name)
            .iter()
            .filter(|c| matches!(c, Call::Stop(_)))
            .count()
    }

    pub fn starts(&self, name: &str) -> usize {
        self.calls(name)
            .iter()
            .filter(|c| matches!(c, Call::Start(_)))
            .count()
    }

    pub fn probes(&self, name: &str) -> usize {
        self.calls(name)
            .iter()
            .filter(|c| matches!(c, Call::Probe(..)))
            .count()
    }
}
