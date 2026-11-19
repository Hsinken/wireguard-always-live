use std::collections::{BTreeMap, VecDeque};
use std::net::Ipv4Addr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use wgal_core::{
    AdapterError, DiscoveredBinding, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, TunnelBinding,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, DisplayName, OsState, TargetAddress,
};

/// One call made against a [`FakePlatform`], recorded in order. Interface
/// calls record the interface they were given, which may be stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Discover,
    CheckRegistration(TunnelBinding),
    QueryState(TunnelBinding),
    ResolveInterface(TunnelBinding),
    CheckRoute(ResolvedInterface, TargetAddress),
    Probe(ResolvedInterface, ProbeRequest),
    Start(TunnelBinding),
    Stop(TunnelBinding),
}

/// Tunnel operations whose next calls a test can make fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    QueryState,
    ResolveInterface,
    Start,
    Stop,
}

/// In-memory [`PlatformAdapter`] with scriptable results and a call log.
///
/// - A tunnel has an interface while connected or transitioning. Every new
///   interface gets a fresh index and incarnation, so an interface resolved
///   before a stop, start or [`FakePlatform::recreate_interface`] is stale,
///   and probes and route checks through it fail with `NotFound`.
///   [`FakePlatform::rebuild_interface_in_place`] keeps the name, index and
///   address and changes only the incarnation, which makes the old one stale
///   in the same way.
/// - `check_registration` succeeds unless [`FakePlatform::set_registration`]
///   says otherwise.
/// - Without a script, probes answer from the tunnel's OS state: a reply after
///   [`FakePlatform::DEFAULT_RTT`] while connected, no reply otherwise.
/// - A target routes through whichever interface asks, unless
///   [`FakePlatform::route_via`] says otherwise.
/// - `start` and `stop` switch the OS state unless a failure was queued or the
///   tunnel's control is not [`Capability::Supported`].
/// - Capabilities start as [`AdapterCapabilities::UNKNOWN`].
/// - Calls for tunnels that were never added fail with `NotFound`.
///
/// Setup methods panic when given a tunnel that was never added, since that is
/// a mistake in the test itself.
#[derive(Debug)]
pub struct FakePlatform {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    capabilities: AdapterCapabilities,
    tunnels: BTreeMap<TunnelBinding, FakeTunnel>,
    /// The tunnel each interface index ever handed out belonged to. Indexes
    /// are never reused.
    owners: BTreeMap<u32, TunnelBinding>,
    next_index: u32,
    /// Every interface instance gets a new incarnation, including one rebuilt
    /// in place with the same name, index and address.
    next_incarnation: u64,
    /// `None` routes a target outside every tunnel.
    routes: BTreeMap<TargetAddress, Option<TunnelBinding>>,
    calls: Vec<Call>,
}

#[derive(Debug)]
struct FakeTunnel {
    display_name: DisplayName,
    control: Capability,
    os_state: OsState,
    interface: Option<ResolvedInterface>,
    registration: Result<(), AdapterError>,
    probe_script: VecDeque<Result<ProbeOutcome, AdapterError>>,
    failures: BTreeMap<Operation, VecDeque<AdapterError>>,
}

impl FakeTunnel {
    fn take_failure(&mut self, operation: Operation) -> Result<(), AdapterError> {
        match self
            .failures
            .get_mut(&operation)
            .and_then(VecDeque::pop_front)
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn not_found(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::NotFound, detail)
}

fn out_of_indexes() -> AdapterError {
    AdapterError::new(
        AdapterErrorKind::ManagerConflict,
        "fake platform has no interface indexes left",
    )
}

impl State {
    fn tunnel_mut(&mut self, id: &TunnelBinding) -> Result<&mut FakeTunnel, AdapterError> {
        self.tunnels
            .get_mut(id)
            .ok_or_else(|| not_found("tunnel is not known to the fake platform"))
    }

    fn allocate_interface(&mut self, id: &TunnelBinding) -> Option<ResolvedInterface> {
        let index = self.next_index;
        self.next_index = index.checked_add(1)?;
        let incarnation = self.take_incarnation()?;
        self.owners.insert(index, id.clone());
        // TEST-NET-2 (RFC 5737): fakes never use real addresses.
        let host = u8::try_from(index % 254).map_or(1, |h| h + 1);
        Some(ResolvedInterface {
            name: format!("fake{index}"),
            index: Some(index),
            source: Some(Ipv4Addr::new(198, 51, 100, host)),
            incarnation: Some(incarnation),
        })
    }

    fn take_incarnation(&mut self) -> Option<u64> {
        let incarnation = self.next_incarnation;
        self.next_incarnation = incarnation.checked_add(1)?;
        Some(incarnation)
    }

    /// Sets the OS state and keeps the interface consistent with it. Fails only
    /// when a new interface is needed and no index is left.
    fn set_os_state(&mut self, id: &TunnelBinding, os_state: OsState) -> Result<(), AdapterError> {
        let has_interface = self.tunnel_mut(id)?.interface.is_some();
        let new_interface = match os_state {
            OsState::Connected | OsState::Transitioning if !has_interface => {
                Some(self.allocate_interface(id).ok_or_else(out_of_indexes)?)
            }
            _ => None,
        };
        let tunnel = self.tunnel_mut(id)?;
        tunnel.os_state = os_state;
        match os_state {
            OsState::Disconnected => tunnel.interface = None,
            _ if new_interface.is_some() => tunnel.interface = new_interface,
            _ => {}
        }
        Ok(())
    }

    /// The tunnel whose current interface is `interface`, if it still exists.
    fn live_owner(&self, interface: &ResolvedInterface) -> Result<TunnelBinding, AdapterError> {
        interface
            .index
            .and_then(|index| self.owners.get(&index))
            .filter(|id| {
                self.tunnels
                    .get(*id)
                    .is_some_and(|t| t.interface.as_ref() == Some(interface))
            })
            .cloned()
            .ok_or_else(|| not_found("interface no longer exists"))
    }

    fn call_tunnel<'a>(&'a self, call: &'a Call) -> Option<&'a TunnelBinding> {
        match call {
            Call::Discover => None,
            Call::CheckRegistration(id)
            | Call::QueryState(id)
            | Call::ResolveInterface(id)
            | Call::Start(id)
            | Call::Stop(id) => Some(id),
            Call::CheckRoute(interface, _) | Call::Probe(interface, _) => {
                interface.index.and_then(|index| self.owners.get(&index))
            }
        }
    }
}

impl Default for FakePlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl FakePlatform {
    pub const DEFAULT_RTT: Duration = Duration::from_millis(10);

    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                capabilities: AdapterCapabilities::UNKNOWN,
                tunnels: BTreeMap::new(),
                owners: BTreeMap::new(),
                next_index: 1,
                next_incarnation: 1,
                routes: BTreeMap::new(),
                calls: Vec::new(),
            }),
        }
    }

    pub fn set_capabilities(&self, capabilities: AdapterCapabilities) {
        self.lock().capabilities = capabilities;
    }

    /// Adds a tunnel the official manager knows about. It is controllable
    /// until [`FakePlatform::set_control`] says otherwise.
    pub fn add_tunnel(&self, id: TunnelBinding, display_name: &str, os_state: OsState) {
        let display_name =
            DisplayName::new(display_name).unwrap_or_else(|e| panic!("FakePlatform: {e}"));
        let mut state = self.lock();
        let tunnel = FakeTunnel {
            display_name,
            control: Capability::Supported,
            os_state: OsState::Disconnected,
            interface: None,
            registration: Ok(()),
            probe_script: VecDeque::new(),
            failures: BTreeMap::new(),
        };
        if state.tunnels.insert(id.clone(), tunnel).is_some() {
            panic!("FakePlatform: tunnel {id} was added twice");
        }
        Self::expect_setup(state.set_os_state(&id, os_state));
    }

    /// Whether this platform can start and stop the tunnel, as for a tunnel
    /// owned by a manager WGAL recognizes but does not drive.
    pub fn set_control(&self, id: &TunnelBinding, control: Capability) {
        self.with_tunnel(id, |t| t.control = control);
    }

    /// Changes the OS state behind the watchdog's back, as an external tool would.
    pub fn set_os_state(&self, id: &TunnelBinding, os_state: OsState) {
        self.with_tunnel(id, |_| ());
        Self::expect_setup(self.lock().set_os_state(id, os_state));
    }

    pub fn os_state(&self, id: &TunnelBinding) -> OsState {
        self.with_tunnel(id, |t| t.os_state)
    }

    /// The tunnel's current interface, without recording a call.
    pub fn interface(&self, id: &TunnelBinding) -> Option<ResolvedInterface> {
        self.with_tunnel(id, |t| t.interface.clone())
    }

    /// Replaces the tunnel's interface with a new one without changing its OS
    /// state, as when the OS rebuilds the interface behind the manager's back.
    pub fn recreate_interface(&self, id: &TunnelBinding) -> ResolvedInterface {
        self.with_tunnel(id, |t| {
            assert!(t.interface.is_some(), "FakePlatform: {id} has no interface");
        });
        let mut state = self.lock();
        let interface = state
            .allocate_interface(id)
            .unwrap_or_else(|| panic!("FakePlatform: no interface indexes left"));
        Self::expect_setup(state.tunnel_mut(id)).interface = Some(interface.clone());
        interface
    }

    /// Replaces the tunnel's interface with one that has the same name, index
    /// and address but a new incarnation, as when Windows recreates an adapter
    /// with the same index or macOS reconnects on the same `utun`. Anything
    /// resolved before is stale.
    pub fn rebuild_interface_in_place(&self, id: &TunnelBinding) -> ResolvedInterface {
        let mut state = self.lock();
        let incarnation = state
            .take_incarnation()
            .unwrap_or_else(|| panic!("FakePlatform: no incarnations left"));
        let tunnel = Self::expect_setup(state.tunnel_mut(id));
        let interface = tunnel
            .interface
            .as_mut()
            .unwrap_or_else(|| panic!("FakePlatform: {id} has no interface"));
        interface.incarnation = Some(incarnation);
        interface.clone()
    }

    /// What `check_registration` answers for the tunnel from now on. `Ok` by
    /// default.
    pub fn set_registration(&self, id: &TunnelBinding, result: Result<(), AdapterError>) {
        self.with_tunnel(id, |t| t.registration = result);
    }

    /// Routes `target` through `via`'s interface only, or outside every tunnel
    /// when `via` is `None`.
    pub fn route_via(&self, target: TargetAddress, via: Option<&TunnelBinding>) {
        if let Some(id) = via {
            self.with_tunnel(id, |_| ());
        }
        self.lock().routes.insert(target, via.cloned());
    }

    /// Queues probe results, consumed in order before the default behaviour applies.
    pub fn script_probes(
        &self,
        id: &TunnelBinding,
        results: impl IntoIterator<Item = Result<ProbeOutcome, AdapterError>>,
    ) {
        self.with_tunnel(id, |t| t.probe_script.extend(results));
    }

    /// Drops the probe results still queued, so the default behaviour
    /// applies from the next probe on: the tunnel's path comes back.
    pub fn clear_probe_script(&self, id: &TunnelBinding) {
        self.with_tunnel(id, |t| t.probe_script.clear());
    }

    /// Makes the next `operation` on the tunnel fail with `error`. Queued
    /// failures are used in order.
    pub fn fail_next(&self, id: &TunnelBinding, operation: Operation, error: AdapterError) {
        self.with_tunnel(id, |t| {
            t.failures.entry(operation).or_default().push_back(error);
        });
    }

    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Calls that targeted `id` or one of its interfaces, current or stale, in order.
    pub fn calls_for(&self, id: &TunnelBinding) -> Vec<Call> {
        let state = self.lock();
        state
            .calls
            .iter()
            .filter(|call| state.call_tunnel(call) == Some(id))
            .cloned()
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_tunnel<R>(&self, id: &TunnelBinding, f: impl FnOnce(&mut FakeTunnel) -> R) -> R {
        let mut state = self.lock();
        let tunnel = state
            .tunnels
            .get_mut(id)
            .unwrap_or_else(|| panic!("FakePlatform: tunnel {id} was never added"));
        f(tunnel)
    }

    fn expect_setup<T>(result: Result<T, AdapterError>) -> T {
        result.unwrap_or_else(|e| panic!("FakePlatform setup failed: {e}"))
    }
}

impl PlatformAdapter for FakePlatform {
    fn capabilities(&self) -> AdapterCapabilities {
        self.lock().capabilities
    }

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        let mut state = self.lock();
        state.calls.push(Call::Discover);
        Ok(state
            .tunnels
            .iter()
            .map(|(id, t)| DiscoveredBinding {
                binding: id.clone(),
                display_name: t.display_name.clone(),
                control: t.control,
            })
            .collect())
    }

    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        let mut state = self.lock();
        state.calls.push(Call::CheckRegistration(tunnel.clone()));
        state.tunnel_mut(tunnel)?.registration.clone()
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        let mut state = self.lock();
        state.calls.push(Call::QueryState(tunnel.clone()));
        let t = state.tunnel_mut(tunnel)?;
        t.take_failure(Operation::QueryState)?;
        Ok(t.os_state)
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        let mut state = self.lock();
        state.calls.push(Call::ResolveInterface(tunnel.clone()));
        let t = state.tunnel_mut(tunnel)?;
        t.take_failure(Operation::ResolveInterface)?;
        t.interface
            .clone()
            .ok_or_else(|| not_found("tunnel has no interface"))
    }

    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        let mut state = self.lock();
        state
            .calls
            .push(Call::CheckRoute(interface.clone(), target));
        let owner = state.live_owner(interface)?;
        match state.routes.get(&target) {
            Some(via) if via.as_ref() != Some(&owner) => Err(AdapterError::new(
                AdapterErrorKind::RouteMismatch,
                "target does not route through this interface",
            )),
            _ => Ok(RouteEvidence {
                target,
                interface_name: interface.name.clone(),
                interface_index: interface.index,
                source: interface.source,
            }),
        }
    }

    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        let mut state = self.lock();
        state.calls.push(Call::Probe(interface.clone(), *request));
        let owner = state.live_owner(interface)?;
        let t = state.tunnel_mut(&owner)?;
        t.probe_script.pop_front().unwrap_or(match t.os_state {
            OsState::Connected => Ok(ProbeOutcome::Reply {
                rtt: Self::DEFAULT_RTT,
            }),
            _ => Ok(ProbeOutcome::NoReply),
        })
    }

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(tunnel, Call::Start, Operation::Start, OsState::Connected)
    }

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(tunnel, Call::Stop, Operation::Stop, OsState::Disconnected)
    }
}

impl FakePlatform {
    fn control(
        &self,
        id: &TunnelBinding,
        call: fn(TunnelBinding) -> Call,
        operation: Operation,
        target: OsState,
    ) -> Result<(), AdapterError> {
        let mut state = self.lock();
        state.calls.push(call(id.clone()));
        let t = state.tunnel_mut(id)?;
        if t.control != Capability::Supported {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "this tunnel's manager is not controlled by the platform",
            ));
        }
        t.take_failure(operation)?;
        state.set_os_state(id, target)
    }
}
