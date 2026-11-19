//! In-memory [`ServiceControl`] and [`IpHelper`] loaded from an anonymized
//! host fixture. They record every call so tests can assert that only the
//! named service and the expected lookups were touched.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{AdapterInfo, BestRoute, EchoReply, IpHelper, NetError, ScmError, ServiceControl};
use super::{Control, DefaultRoute, ServiceName, ServiceState};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub services: Vec<FixtureService>,
    pub adapters: Vec<AdapterInfo>,
    pub routes: Vec<FixtureRoute>,
    pub default_routes: Vec<DefaultRoute>,
    pub neighbors: Vec<FixtureNeighbor>,
}

/// A neighbor (ARP or IPv6 neighbor discovery) entry: whether it is static,
/// and whether the neighbor answers when it is resolved.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureNeighbor {
    pub luid: u64,
    pub address: IpAddr,
    pub is_static: bool,
    pub answers: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureService {
    pub name: String,
    pub state: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRoute {
    pub destination: Ipv4Addr,
    pub if_index: u32,
    pub luid: u64,
    pub source: Ipv4Addr,
}

pub fn host() -> Host {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/windows/host-three-tunnels.json"
    ))
    .unwrap()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `Open` changes nothing; `Start` and `Stop` are the requests actually sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScmCall {
    List,
    Query(String),
    Open(String, Control),
    Start(String),
    Stop(String),
}

/// Service names are compared ignoring ASCII case, as the SCM does.
#[derive(Debug, Default)]
pub struct FakeScm {
    state: Mutex<ScmState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct ScmState {
    services: Vec<(String, u32)>,
    calls: Vec<ScmCall>,
    failures: VecDeque<ScmError>,
    /// The next call equal to the first fails with the second.
    refusal: Option<(ScmCall, ScmError)>,
    delay: Duration,
    /// The next call equal to this one blocks until released.
    hold: Option<ScmCall>,
    held: bool,
    /// Every call equal to this one blocks until [`FakeScm::unstall`], up
    /// to `stall_left` of them.
    stall: Option<ScmCall>,
    stall_left: usize,
    /// Requests being sent right now, and how often one began while another
    /// was still being sent.
    sending: usize,
    overlaps: usize,
    finished: usize,
}

impl ScmState {
    fn find(&mut self, name: &str) -> Result<&mut u32, ScmError> {
        self.services
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, state)| state)
            .ok_or(ScmError::SERVICE_DOES_NOT_EXIST)
    }
}

impl FakeScm {
    pub fn from_host(host: &Host) -> Self {
        let services = host
            .services
            .iter()
            .map(|s| (s.name.clone(), s.state))
            .collect();
        Self {
            state: Mutex::new(ScmState {
                services,
                ..ScmState::default()
            }),
            changed: Condvar::new(),
        }
    }

    pub fn state_of(&self, name: &str) -> Option<u32> {
        lock(&self.state).find(name).ok().copied()
    }

    pub fn set_state(&self, name: &str, state: u32) {
        *lock(&self.state).find(name).unwrap() = state;
    }

    pub fn remove(&self, name: &str) {
        lock(&self.state)
            .services
            .retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    /// The next call fails with `error`. Queued failures are used in order.
    pub fn fail_next(&self, error: ScmError) {
        lock(&self.state).failures.push_back(error);
    }

    /// The next call equal to `call` fails with `error` and changes nothing;
    /// other calls are unaffected.
    pub fn refuse_next(&self, call: ScmCall, error: ScmError) {
        lock(&self.state).refusal = Some((call, error));
    }

    /// Every later call blocks for `delay` before answering.
    pub fn delay(&self, delay: Duration) {
        lock(&self.state).delay = delay;
    }

    pub fn calls(&self) -> Vec<ScmCall> {
        lock(&self.state).calls.clone()
    }

    /// The next call equal to `call` blocks, after it is recorded and before
    /// it does anything, until [`FakeScm::release`].
    pub fn hold(&self, call: ScmCall) {
        lock(&self.state).hold = Some(call);
    }

    /// Waits up to `limit` for a call to block on [`FakeScm::hold`].
    pub fn wait_until_held(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut state = lock(&self.state);
        while !state.held {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    pub fn release(&self) {
        lock(&self.state).held = false;
        self.changed.notify_all();
    }

    /// Every later call equal to `call` blocks, after it is recorded and
    /// before it does anything, until [`FakeScm::unstall`].
    pub fn stall(&self, call: ScmCall) {
        self.stall_next(call, usize::MAX);
    }

    /// Like [`FakeScm::stall`], for the next `count` calls equal to `call`
    /// only; later ones go on at once.
    pub fn stall_next(&self, call: ScmCall, count: usize) {
        let mut state = lock(&self.state);
        state.stall = Some(call);
        state.stall_left = count;
    }

    /// Lets every stalled call go on, and stalls nothing more.
    pub fn unstall(&self) {
        lock(&self.state).stall = None;
        self.changed.notify_all();
    }

    /// Waits up to `limit` until `count` calls have finished.
    pub fn wait_for_finished(&self, count: usize, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut state = lock(&self.state);
        while state.finished < count {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// How often a request was sent while another was still being sent.
    pub fn overlapping_sends(&self) -> usize {
        lock(&self.state).overlaps
    }

    /// Records the call, waits out any hold and delay, then applies `op`.
    fn call<T>(
        &self,
        record: ScmCall,
        op: impl FnOnce(&mut ScmState) -> Result<T, ScmError>,
    ) -> Result<T, ScmError> {
        let sends = matches!(record, ScmCall::Start(_) | ScmCall::Stop(_));
        let (delay, refused) = {
            let mut state = lock(&self.state);
            if sends {
                if state.sending > 0 {
                    state.overlaps += 1;
                }
                state.sending += 1;
            }
            let held = state.hold.as_ref() == Some(&record);
            let refused = match &state.refusal {
                Some((call, error)) if *call == record => Some(*error),
                _ => None,
            };
            if refused.is_some() {
                state.refusal = None;
            }
            let stalled = state.stall.as_ref() == Some(&record) && state.stall_left > 0;
            if stalled {
                state.stall_left -= 1;
            }
            state.calls.push(record);
            if stalled {
                while state.stall.is_some() {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            if held {
                state.hold = None;
                state.held = true;
                self.changed.notify_all();
                while state.held {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            (state.delay, refused)
        };
        thread::sleep(delay);
        let mut state = lock(&self.state);
        let result = match refused.or_else(|| state.failures.pop_front()) {
            Some(error) => Err(error),
            None => op(&mut state),
        };
        if sends {
            state.sending -= 1;
        }
        state.finished += 1;
        self.changed.notify_all();
        result
    }
}

impl ServiceControl for FakeScm {
    fn service_names(&self) -> Result<Vec<String>, ScmError> {
        self.call(ScmCall::List, |s| {
            Ok(s.services.iter().map(|(n, _)| n.clone()).collect())
        })
    }

    fn query(&self, service: &ServiceName) -> Result<ServiceState, ScmError> {
        let name = service.as_str();
        self.call(ScmCall::Query(name.to_owned()), |s| {
            s.find(name).map(|state| ServiceState::from_raw(*state))
        })
    }

    /// The service's name and what it was opened for.
    type Handle = (String, Control);

    fn open(&self, service: &ServiceName, control: Control) -> Result<Self::Handle, ScmError> {
        let name = service.as_str();
        self.call(ScmCall::Open(name.to_owned(), control), |s| {
            s.find(name).map(|_| (name.to_owned(), control))
        })
    }

    fn send(&self, handle: &Self::Handle, control: Control) -> Result<(), ScmError> {
        let (name, opened_for) = handle;
        if *opened_for != control {
            return Err(ScmError::ACCESS_DENIED);
        }
        match control {
            Control::Start => self.call(ScmCall::Start(name.clone()), |s| {
                let state = s.find(name)?;
                if *state != 1 {
                    return Err(ScmError::SERVICE_ALREADY_RUNNING);
                }
                *state = 2;
                Ok(())
            }),
            Control::Stop => self.call(ScmCall::Stop(name.clone()), |s| {
                let state = s.find(name)?;
                match *state {
                    1 => Err(ScmError::SERVICE_NOT_ACTIVE),
                    2 => Err(ScmError::SERVICE_CANNOT_ACCEPT_CTRL),
                    _ => {
                        *state = 3;
                        Ok(())
                    }
                }
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetCall {
    Adapters,
    BestRoute(Ipv4Addr),
    Echo {
        source: Ipv4Addr,
        destination: Ipv4Addr,
        timeout: Duration,
    },
    /// The adapters were listed with their IPv6 addresses too.
    AdaptersAnyFamily,
    DefaultRoutes,
    /// The neighbor entry was read.
    Neighbor(u64, IpAddr),
    /// The neighbor was resolved afresh (ARP or neighbor solicitation sent).
    Resolve(u64, IpAddr),
}

/// A neighbor as the fake resolves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Neighbor {
    is_static: bool,
    answer: Result<bool, NetError>,
}

#[derive(Debug, Default)]
pub struct FakeNet {
    state: Mutex<NetState>,
    changed: Condvar,
}

/// One call into the fake, from when it starts until it returns.
struct Busy<'a>(&'a FakeNet);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).busy -= 1;
        self.0.changed.notify_all();
    }
}

#[derive(Debug, Default)]
struct NetState {
    adapters: Vec<AdapterInfo>,
    routes: BTreeMap<Ipv4Addr, BestRoute>,
    echoes: VecDeque<Result<EchoReply, NetError>>,
    calls: Vec<NetCall>,
    adapter_failure: Option<NetError>,
    /// Adapter to rebuild during the next echo, and whether it keeps its index.
    recreate_during_echo: Option<(String, bool)>,
    echo_delay: Duration,
    /// The answer to every echo once the queued ones are used up.
    echo_default: Option<EchoReply>,
    default_routes: Vec<DefaultRoute>,
    default_routes_failure: Option<NetError>,
    /// By interface LUID and address. An address not listed has no entry
    /// and never answers.
    neighbors: BTreeMap<(u64, IpAddr), Neighbor>,
    resolve_delay: Duration,
    /// Resolutions and echoes block, once recorded, while these are set.
    hold_resolutions: bool,
    hold_echoes: bool,
    /// Calls started and not yet returned, now and at most so far.
    busy: usize,
    most_busy: usize,
}

impl NetState {
    fn recreate(&mut self, name: &str, keep_index: bool) -> AdapterInfo {
        let adapter = self
            .adapters
            .iter_mut()
            .find(|a| a.friendly_name == name)
            .unwrap();
        let (old_index, old_luid) = (adapter.if_index, adapter.luid);
        if !keep_index {
            adapter.if_index += 100;
        }
        adapter.luid += 1 << 24;
        let fresh = adapter.clone();
        for route in self.routes.values_mut() {
            if route.if_index == old_index && route.luid == old_luid {
                route.if_index = fresh.if_index;
                route.luid = fresh.luid;
            }
        }
        fresh
    }
}

impl FakeNet {
    pub fn from_host(host: &Host) -> Self {
        let routes = host
            .routes
            .iter()
            .map(|r| {
                let route = BestRoute {
                    if_index: r.if_index,
                    luid: r.luid,
                    source: r.source,
                };
                (r.destination, route)
            })
            .collect();
        let neighbors = host
            .neighbors
            .iter()
            .map(|n| {
                let neighbor = Neighbor {
                    is_static: n.is_static,
                    answer: Ok(n.answers),
                };
                ((n.luid, n.address), neighbor)
            })
            .collect();
        Self {
            state: Mutex::new(NetState {
                adapters: host.adapters.clone(),
                routes,
                default_routes: host.default_routes.clone(),
                neighbors,
                ..NetState::default()
            }),
            changed: Condvar::new(),
        }
    }

    fn busy(&self) -> Busy<'_> {
        let mut state = lock(&self.state);
        state.busy += 1;
        state.most_busy = state.most_busy.max(state.busy);
        Busy(self)
    }

    /// Blocks while `held` says so.
    fn wait_while(&self, held: impl Fn(&NetState) -> bool) {
        let mut state = lock(&self.state);
        while held(&state) {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Every later resolution blocks, once recorded, until
    /// [`FakeNet::release`].
    pub fn hold_resolutions(&self) {
        lock(&self.state).hold_resolutions = true;
    }

    /// Every later echo blocks, once recorded, until [`FakeNet::release`].
    pub fn hold_echoes(&self) {
        lock(&self.state).hold_echoes = true;
    }

    /// Lets every held call go on, and holds nothing more.
    pub fn release(&self) {
        let mut state = lock(&self.state);
        state.hold_resolutions = false;
        state.hold_echoes = false;
        drop(state);
        self.changed.notify_all();
    }

    /// Calls into the fake that have started and not returned yet: each is
    /// one of the adapter's helper threads still alive.
    pub fn busy_calls(&self) -> usize {
        lock(&self.state).busy
    }

    /// The most calls that were ever under way at once.
    pub fn most_busy_calls(&self) -> usize {
        lock(&self.state).most_busy
    }

    /// Waits up to `limit` until no call is under way.
    pub fn wait_until_idle(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        let mut state = lock(&self.state);
        while state.busy > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    pub fn adapter_mut(&self, name: &str, change: impl FnOnce(&mut AdapterInfo)) {
        let mut state = lock(&self.state);
        change(
            state
                .adapters
                .iter_mut()
                .find(|a| a.friendly_name == name)
                .unwrap(),
        );
    }

    pub fn add_adapter(&self, adapter: AdapterInfo) {
        lock(&self.state).adapters.push(adapter);
    }

    pub fn remove_adapter(&self, name: &str) {
        lock(&self.state)
            .adapters
            .retain(|a| a.friendly_name != name);
    }

    /// Gives the adapter a new index and LUID, as when the tunnel service
    /// restarts, and moves its routes along.
    pub fn recreate(&self, name: &str) -> AdapterInfo {
        lock(&self.state).recreate(name, false)
    }

    /// Replaces the adapter with one that has the same name, index and
    /// addresses and only a new LUID, as when Windows reuses the index.
    pub fn recreate_in_place(&self, name: &str) -> AdapterInfo {
        lock(&self.state).recreate(name, true)
    }

    pub fn recreate_during_next_echo(&self, name: &str) {
        lock(&self.state).recreate_during_echo = Some((name.to_owned(), false));
    }

    /// Like [`FakeNet::recreate_in_place`], during the next echo.
    pub fn recreate_in_place_during_next_echo(&self, name: &str) {
        lock(&self.state).recreate_during_echo = Some((name.to_owned(), true));
    }

    pub fn set_route(&self, destination: Ipv4Addr, route: BestRoute) {
        lock(&self.state).routes.insert(destination, route);
    }

    pub fn remove_route(&self, destination: Ipv4Addr) {
        lock(&self.state).routes.remove(&destination);
    }

    pub fn fail_adapters(&self, error: NetError) {
        lock(&self.state).adapter_failure = Some(error);
    }

    pub fn push_echo(&self, reply: Result<EchoReply, NetError>) {
        lock(&self.state).echoes.push_back(reply);
    }

    pub fn delay_echo(&self, delay: Duration) {
        lock(&self.state).echo_delay = delay;
    }

    /// Every echo without a queued reply gets `reply`; `None` makes such an
    /// echo a test failure again.
    pub fn answer_echoes(&self, reply: Option<EchoReply>) {
        lock(&self.state).echo_default = reply;
    }

    pub fn set_default_routes(&self, routes: Vec<DefaultRoute>) {
        lock(&self.state).default_routes = routes;
    }

    pub fn fail_default_routes(&self, error: Option<NetError>) {
        lock(&self.state).default_routes_failure = error;
    }

    /// What resolving the neighbor `address` on `luid` answers from now on;
    /// the entry is created dynamic if it did not exist.
    pub fn neighbor_answers(
        &self,
        luid: u64,
        address: impl Into<IpAddr>,
        answer: Result<bool, NetError>,
    ) {
        lock(&self.state)
            .neighbors
            .entry((luid, address.into()))
            .or_insert(Neighbor {
                is_static: false,
                answer: Ok(false),
            })
            .answer = answer;
    }

    /// Every resolution blocks for `delay` before answering.
    pub fn delay_resolve(&self, delay: Duration) {
        lock(&self.state).resolve_delay = delay;
    }

    /// How many calls matched `which`.
    pub fn count(&self, which: impl Fn(&NetCall) -> bool) -> usize {
        lock(&self.state).calls.iter().filter(|c| which(c)).count()
    }

    pub fn calls(&self) -> Vec<NetCall> {
        lock(&self.state).calls.clone()
    }

    pub fn echoes_sent(&self) -> Vec<NetCall> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, NetCall::Echo { .. }))
            .collect()
    }
}

impl IpHelper for FakeNet {
    /// The IPv4 listing: every adapter, without its IPv6 addresses.
    fn adapters(&self) -> Result<Vec<AdapterInfo>, NetError> {
        let _busy = self.busy();
        let mut state = lock(&self.state);
        state.calls.push(NetCall::Adapters);
        match state.adapter_failure {
            Some(error) => Err(error),
            None => Ok(state
                .adapters
                .iter()
                .map(|a| AdapterInfo {
                    ipv6: Vec::new(),
                    ..a.clone()
                })
                .collect()),
        }
    }

    fn adapters_any_family(&self) -> Result<Vec<AdapterInfo>, NetError> {
        let _busy = self.busy();
        let mut state = lock(&self.state);
        state.calls.push(NetCall::AdaptersAnyFamily);
        match state.adapter_failure {
            Some(error) => Err(error),
            None => Ok(state.adapters.clone()),
        }
    }

    fn best_route(&self, destination: Ipv4Addr) -> Result<BestRoute, NetError> {
        let _busy = self.busy();
        let mut state = lock(&self.state);
        state.calls.push(NetCall::BestRoute(destination));
        state
            .routes
            .get(&destination)
            .copied()
            .ok_or(NetError::NOT_FOUND)
    }

    fn echo(
        &self,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        timeout: Duration,
    ) -> Result<EchoReply, NetError> {
        let _busy = self.busy();
        let delay = {
            let mut state = lock(&self.state);
            state.calls.push(NetCall::Echo {
                source,
                destination,
                timeout,
            });
            if let Some((name, keep_index)) = state.recreate_during_echo.take() {
                state.recreate(&name, keep_index);
            }
            state.echo_delay
        };
        self.wait_while(|s| s.hold_echoes);
        thread::sleep(delay);
        let mut state = lock(&self.state);
        match (state.echoes.pop_front(), state.echo_default) {
            (Some(queued), _) => queued,
            (None, Some(reply)) => Ok(reply),
            (None, None) => panic!("FakeNet: no echo reply queued"),
        }
    }

    fn default_routes(&self) -> Result<Vec<DefaultRoute>, NetError> {
        let _busy = self.busy();
        let mut state = lock(&self.state);
        state.calls.push(NetCall::DefaultRoutes);
        match state.default_routes_failure {
            Some(error) => Err(error),
            None => Ok(state.default_routes.clone()),
        }
    }

    fn neighbor_is_static(&self, luid: u64, address: IpAddr) -> Result<bool, NetError> {
        let _busy = self.busy();
        let mut state = lock(&self.state);
        state.calls.push(NetCall::Neighbor(luid, address));
        Ok(state
            .neighbors
            .get(&(luid, address))
            .is_some_and(|n| n.is_static))
    }

    fn resolve_neighbor(&self, luid: u64, address: IpAddr) -> Result<bool, NetError> {
        let _busy = self.busy();
        let delay = {
            let mut state = lock(&self.state);
            state.calls.push(NetCall::Resolve(luid, address));
            state.resolve_delay
        };
        self.wait_while(|s| s.hold_resolutions);
        thread::sleep(delay);
        lock(&self.state)
            .neighbors
            .get(&(luid, address))
            .map_or(Ok(false), |n| n.answer)
    }
}
