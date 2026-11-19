//! Test rig for the watchdog loop: a [`FakePlatform`] shared between the
//! watchdog and the test, with data-path faults and gates that hold a probe,
//! stop or start until the test opens them; builders for the parts; a
//! scripted transport; and [`Sim`], which drives the loop's [`Engine`] step
//! by step with a fake clock. Everything is simulation: no device, no
//! network, no real tunnel.

#![allow(clippy::unwrap_used, dead_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use wgal_core::{
    AdapterError, DiscoveredBinding, MemoryStore, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, Startup, Timing, TunnelBinding, Watchdog, WorkItem,
};
use wgal_protocol::{
    AdapterCapabilities, BotToken, BoundedVec, Capability, ChatId, DeviceName, EventFilter,
    HostSettings, HostSettingsEntry, HostStatus, ImportMatch, ImportOptions, LanguageTag,
    MonitorSettings, NotificationTemplates, OsState, Outcome, PROTOCOL_VERSION, ProbeKind,
    ProbeTarget, ProbeTiming, ProductVersion, RecoveryRetry, Request, RequestEnvelope, RequestId,
    Response, ResponseEnvelope, SETTINGS_DOCUMENT_VERSION, SettingsDocument, SettingsFormat,
    Snapshot, TargetAddress, TimeZoneName, TunnelId, TunnelStatus, WorkerCount, codec,
};
use wgal_runtime::driver::{CallId, Engine, Options};
use wgal_runtime::events::EventLog;
use wgal_runtime::ipc::{AccessList, Caller};
use wgal_runtime::notify::{
    Delivery, MemorySecretStore, MemorySettingsStore, Notifier, SecretStore, Transport,
};
use wgal_testkit::{Call, FakeClock, FakePlatform};

/// 2026-01-01T00:00:00Z.
pub const T0: i64 = 1_767_225_600_000;
pub const TOKEN: &str = "123456789:EXAMPLE-not-a-real-token_0000000000";
pub const CHAT: &str = "-1001234567890";

pub const ALL_SUPPORTED: AdapterCapabilities = AdapterCapabilities {
    query_state: Capability::Supported,
    start_stop: Capability::Supported,
    bound_icmp_probe: Capability::Supported,
    bound_tcp_probe: Capability::Supported,
    read_metrics: Capability::Unsupported,
    verified_concurrent_tunnels: 3,
    background_operation: Capability::Unknown,
};

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn binding(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

pub fn id(name: &str) -> TunnelId {
    TunnelId::new(name).unwrap()
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

/// A caller on the access list without administrator rights.
pub fn user() -> Caller {
    Caller {
        account: "account-1001".to_owned(),
        allowed: true,
        elevated: false,
    }
}

pub fn admin() -> Caller {
    Caller {
        account: "account-500".to_owned(),
        allowed: true,
        elevated: true,
    }
}

pub fn stranger(elevated: bool) -> Caller {
    Caller {
        account: "account-2002".to_owned(),
        allowed: false,
        elevated,
    }
}

pub fn envelope(request_id: &str, request: Request) -> RequestEnvelope {
    RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(request_id).unwrap(),
        request,
    }
}

pub fn accepted(response: &ResponseEnvelope) -> bool {
    matches!(response.outcome, Outcome::Ok(Response::Accepted))
}

pub fn error_code(response: &ResponseEnvelope) -> Option<wgal_protocol::ErrorCode> {
    match &response.outcome {
        Outcome::Err(e) => Some(e.code),
        Outcome::Ok(_) => None,
    }
}

/// The code and reason of a refusal, `None` for an answer that is not one.
pub fn refusal_of(
    response: &ResponseEnvelope,
) -> Option<(wgal_protocol::ErrorCode, Option<wgal_protocol::ErrorReason>)> {
    match &response.outcome {
        Outcome::Err(e) => Some((e.code, e.reason)),
        Outcome::Ok(_) => None,
    }
}

/// The contract's fixture of the built-in templates (English since 3.0.0).
pub const TEMPLATES_FIXTURE: &str =
    include_str!("../../../wgal-protocol/fixtures/request_set_notification_templates.json");

/// The templates of [`TEMPLATES_FIXTURE`].
pub fn fixture_templates() -> NotificationTemplates {
    match codec::decode_request(TEMPLATES_FIXTURE.as_bytes())
        .unwrap()
        .request
    {
        Request::SetNotificationTemplates { templates } => templates,
        other => panic!("not a template request: {other:?}"),
    }
}

pub fn snapshot_of(response: &ResponseEnvelope) -> Snapshot {
    match &response.outcome {
        Outcome::Ok(Response::Snapshot { snapshot }) => snapshot.clone(),
        other => panic!("not a snapshot: {other:?}"),
    }
}

/// An operation a test can hold at a gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gated {
    Probe,
    Stop,
    Start,
}

/// Holds every call that reaches it until [`Gate::open`].
#[derive(Debug, Default)]
pub struct Gate {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Gate {
    fn pass(&self) {
        let mut state = lock(&self.state);
        state.0 += 1;
        self.changed.notify_all();
        while !state.1 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Waits until a call reached the gate.
    pub fn wait_entered(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.0 == 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    pub fn entered(&self) -> usize {
        lock(&self.state).0
    }

    pub fn open(&self) {
        lock(&self.state).1 = true;
        self.changed.notify_all();
    }
}

#[derive(Debug, Default)]
struct Knobs {
    bindings: Vec<TunnelBinding>,
    gates: BTreeMap<(TunnelBinding, Gated), Arc<Gate>>,
    /// Probes get no reply until the tunnel is started again.
    dead_until_start: BTreeSet<TunnelBinding>,
    dead_for_good: BTreeSet<TunnelBinding>,
    /// Targets that never answer, whatever tunnel probes them.
    dead_targets: BTreeSet<TargetAddress>,
    /// Probes panic, as a broken adapter might.
    panicking: BTreeSet<TunnelBinding>,
    /// A stop leaves the tunnel transitioning (a service in STOP_PENDING)
    /// for this many state reads before it is down.
    slow_stop: BTreeMap<TunnelBinding, u32>,
    /// A start leaves the tunnel transitioning (START_PENDING) for this many
    /// state reads before it is up.
    slow_start: BTreeMap<TunnelBinding, u32>,
    /// State reads left before a pending stop or start settles, and where.
    pending_reads: BTreeMap<TunnelBinding, (u32, OsState)>,
    /// Holds discovery, which runs on the loop's owner thread.
    discover_gate: Option<Arc<Gate>>,
}

#[derive(Debug, Default)]
pub struct Platform {
    pub fake: FakePlatform,
    knobs: Mutex<Knobs>,
}

/// The platform as both the watchdog and the test hold it.
#[derive(Clone, Debug, Default)]
pub struct Lab(pub Arc<Platform>);

impl Lab {
    pub fn new(tunnels: &[&str]) -> Self {
        let lab = Self::default();
        lab.0.fake.set_capabilities(ALL_SUPPORTED);
        for name in tunnels {
            lab.0
                .fake
                .add_tunnel(binding(name), &format!("Tunnel {name}"), OsState::Connected);
            lab.knobs().bindings.push(binding(name));
        }
        lab
    }

    pub fn fake(&self) -> &FakePlatform {
        &self.0.fake
    }

    fn knobs(&self) -> MutexGuard<'_, Knobs> {
        lock(&self.0.knobs)
    }

    /// Holds the tunnel's next calls of `what` until the returned gate opens.
    pub fn gate(&self, name: &str, what: Gated) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.knobs()
            .gates
            .insert((binding(name), what), Arc::clone(&gate));
        gate
    }

    pub fn kill_until_restart(&self, name: &str) {
        self.knobs().dead_until_start.insert(binding(name));
    }

    pub fn kill_for_good(&self, name: &str) {
        self.knobs().dead_for_good.insert(binding(name));
    }

    /// The remote end of a tunnel killed for good comes back by itself.
    pub fn revive(&self, name: &str) {
        self.knobs().dead_for_good.remove(&binding(name));
    }

    /// 192.0.2.`last` never answers again, through any tunnel.
    pub fn kill_target(&self, last: u8) {
        self.knobs().dead_targets.insert(address(last));
    }

    /// From now on, a stop of the tunnel is accepted but leaves it
    /// transitioning for `reads` state reads, as a Windows service in
    /// STOP_PENDING; the read after those finds it down.
    pub fn slow_stop(&self, name: &str, reads: u32) {
        self.knobs().slow_stop.insert(binding(name), reads);
    }

    /// From now on, a start of the tunnel is accepted but leaves it
    /// transitioning for `reads` state reads, as a Windows service in
    /// START_PENDING; the read after those finds it up.
    pub fn slow_start(&self, name: &str, reads: u32) {
        self.knobs().slow_start.insert(binding(name), reads);
    }

    /// Holds the next discoveries until the returned gate opens.
    pub fn gate_discovery(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.knobs().discover_gate = Some(Arc::clone(&gate));
        gate
    }

    pub fn panic_on_probes(&self, name: &str, panicking: bool) {
        let mut knobs = self.knobs();
        if panicking {
            knobs.panicking.insert(binding(name));
        } else {
            knobs.panicking.remove(&binding(name));
        }
    }

    fn wait_at(&self, tunnel: &TunnelBinding, what: Gated) {
        let gate = self.knobs().gates.get(&(tunnel.clone(), what)).cloned();
        if let Some(gate) = gate {
            gate.pass();
        }
    }

    pub fn count(&self, name: &str, what: fn(&Call) -> bool) -> usize {
        self.fake()
            .calls_for(&binding(name))
            .iter()
            .filter(|c| what(c))
            .count()
    }

    pub fn stops(&self, name: &str) -> usize {
        self.count(name, |c| matches!(c, Call::Stop(_)))
    }

    pub fn starts(&self, name: &str) -> usize {
        self.count(name, |c| matches!(c, Call::Start(_)))
    }

    pub fn probes(&self, name: &str) -> usize {
        self.count(name, |c| matches!(c, Call::Probe(..)))
    }

    fn owner(&self, interface: &ResolvedInterface) -> Option<TunnelBinding> {
        let bindings = self.knobs().bindings.clone();
        bindings
            .into_iter()
            .find(|b| self.fake().interface(b).as_ref() == Some(interface))
    }
}

impl PlatformAdapter for Lab {
    fn capabilities(&self) -> AdapterCapabilities {
        self.fake().capabilities()
    }

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        let gate = self.knobs().discover_gate.clone();
        if let Some(gate) = gate {
            gate.pass();
        }
        self.fake().discover()
    }

    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.fake().check_registration(tunnel)
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        let settles = match self.knobs().pending_reads.get_mut(tunnel) {
            Some((n, to)) if *n > 0 => {
                *n -= 1;
                (*n == 0).then_some(*to)
            }
            _ => None,
        };
        let state = self.fake().query_state(tunnel);
        if let Some(to) = settles {
            self.fake().set_os_state(tunnel, to);
        }
        state
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        self.fake().resolve_interface(tunnel)
    }

    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        self.fake().check_route(interface, target)
    }

    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        let owner = self.owner(interface);
        if let Some(owner) = &owner {
            self.wait_at(owner, Gated::Probe);
            let panicking = self.knobs().panicking.contains(owner);
            assert!(!panicking, "injected adapter panic");
        }
        let result = self.fake().probe(interface, request)?;
        let knobs = self.knobs();
        let dead = owner.is_some_and(|o| {
            knobs.dead_until_start.contains(&o) || knobs.dead_for_good.contains(&o)
        }) || knobs.dead_targets.contains(&request.target.address);
        Ok(if dead { ProbeOutcome::NoReply } else { result })
    }

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.wait_at(tunnel, Gated::Start);
        let result = self.fake().start(tunnel);
        if result.is_ok() {
            self.knobs().dead_until_start.remove(tunnel);
            let slow = self.knobs().slow_start.get(tunnel).copied();
            if let Some(reads) = slow {
                self.fake().set_os_state(tunnel, OsState::Transitioning);
                self.knobs()
                    .pending_reads
                    .insert(tunnel.clone(), (reads, OsState::Connected));
            }
        }
        result
    }

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.wait_at(tunnel, Gated::Stop);
        let result = self.fake().stop(tunnel);
        let slow = self.knobs().slow_stop.get(tunnel).copied();
        if result.is_ok()
            && let Some(reads) = slow
        {
            self.fake().set_os_state(tunnel, OsState::Transitioning);
            self.knobs()
                .pending_reads
                .insert(tunnel.clone(), (reads, OsState::Disconnected));
        }
        result
    }
}

pub type Wd = Watchdog<Lab, Arc<FakeClock>>;
pub type Service = Notifier<MemorySecretStore, MemorySettingsStore>;
pub type Eng = Engine<Lab, Arc<FakeClock>, MemorySecretStore, MemorySettingsStore>;

pub fn watchdog(lab: &Lab, clock: &Arc<FakeClock>, store: &MemoryStore, startup: Startup) -> Wd {
    Watchdog::new(
        lab.clone(),
        Arc::clone(clock),
        Timing::default(),
        Box::new(store.clone()),
        startup,
    )
}

fn handled(wd: &mut Wd, request: Request) {
    match wd.handle(&request) {
        Outcome::Ok(_) => {}
        Outcome::Err(e) => panic!("{request:?} was rejected: {e:?}"),
    }
}

/// Advances the clock second by second, ticking the core directly.
pub fn tick_for(wd: &mut Wd, secs: u64) {
    for _ in 0..secs {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
}

/// A watchdog with each tunnel registered (recovery target 192.0.2.(i + 1)),
/// driven with `tick` past the start-up grace period so every baseline has
/// passed: the clock stands at 35 s. Auto recovery is off.
pub fn prepared(lab: &Lab, clock: &Arc<FakeClock>, store: &MemoryStore, tunnels: &[&str]) -> Wd {
    let mut wd = watchdog(lab, clock, store, Startup::Empty);
    handled(&mut wd, Request::DiscoverTunnels);
    for (i, name) in tunnels.iter().enumerate() {
        handled(
            &mut wd,
            Request::RegisterTunnel {
                tunnel_id: id(name),
            },
        );
        let last = u8::try_from(i + 1).unwrap();
        handled(&mut wd, settings(name, Some(icmp(last))));
    }
    wd.tick();
    tick_for(&mut wd, 35);
    wd
}

pub fn settings(name: &str, recovery: Option<ProbeTarget>) -> Request {
    Request::UpdateMonitorSettings {
        tunnel_id: id(name),
        settings: MonitorSettings {
            recovery_target: recovery,
            observe_targets: BoundedVec::new(),
            probe: ProbeTiming::DEFAULT,
        },
    }
}

pub fn manage(wd: &mut Wd, name: &str) {
    handled(
        wd,
        Request::SetAutoRecovery {
            tunnel_id: id(name),
            enabled: true,
        },
    );
}

/// A notifier with notifications on, a chat and a stored token.
pub fn notifier(wd: &mut Wd) -> (Service, MemorySecretStore, MemorySettingsStore) {
    let settings = MemorySettingsStore::new();
    let mut secrets = MemorySecretStore::new();
    secrets.save(&BotToken::new(TOKEN).unwrap()).unwrap();
    let mut notifier = Notifier::new(settings.clone(), secrets.clone());
    let outcome = notifier
        .handle(
            wd,
            &RequestId::new("settings-1").unwrap(),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("Lab laptop").unwrap(),
                chat_id: Some(ChatId::new(CHAT).unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(Response::Accepted)));
    (notifier, secrets, settings)
}

/// A notifier with nothing configured.
pub fn quiet_notifier() -> Service {
    Notifier::new(MemorySettingsStore::new(), MemorySecretStore::new())
}

/// Answers with scripted deliveries (then `Accepted`), records every send,
/// and can hold sends at a gate.
#[derive(Clone, Debug, Default)]
pub struct Script {
    answers: Arc<Mutex<VecDeque<Delivery>>>,
    sent: Arc<Mutex<Vec<String>>>,
    gate: Arc<Mutex<Option<Arc<Gate>>>>,
    next_id: Arc<Mutex<i64>>,
}

impl Script {
    pub fn answer(&self, delivery: Delivery) {
        lock(&self.answers).push_back(delivery);
    }

    pub fn sends(&self) -> usize {
        lock(&self.sent).len()
    }

    pub fn texts(&self) -> Vec<String> {
        lock(&self.sent).clone()
    }

    pub fn hold(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *lock(&self.gate) = Some(Arc::clone(&gate));
        gate
    }
}

impl Transport for Script {
    fn send_message(&self, token: &BotToken, chat: &ChatId, text: &str) -> Delivery {
        assert_eq!(token.expose_secret(), TOKEN);
        assert_eq!(chat.as_str(), CHAT);
        let gate = lock(&self.gate).clone();
        if let Some(gate) = gate {
            gate.pass();
        }
        lock(&self.sent).push(text.to_owned());
        lock(&self.answers).pop_front().unwrap_or_else(|| {
            let mut id = lock(&self.next_id);
            *id += 1;
            Delivery::Accepted { message_id: *id }
        })
    }
}

/// A fresh directory under the system temp directory.
#[derive(Debug)]
pub struct TempDir(PathBuf);

static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("wgal-loop-{}-{name}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every line of the event log, oldest first, across rotated files.
pub fn log_lines(dir: &Path) -> Vec<serde_json::Value> {
    let mut files: Vec<PathBuf> = (1..=wgal_runtime::events::KEPT_FILES)
        .rev()
        .map(|n| dir.join(format!("events.{n}.jsonl")))
        .collect();
    files.push(dir.join(wgal_runtime::events::EVENTS_FILE));
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|text| {
            text.lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn raw_log(dir: &Path) -> String {
    let mut text = String::new();
    for n in (1..=wgal_runtime::events::KEPT_FILES).rev() {
        text.push_str(
            &std::fs::read_to_string(dir.join(format!("events.{n}.jsonl"))).unwrap_or_default(),
        );
    }
    text.push_str(
        &std::fs::read_to_string(dir.join(wgal_runtime::events::EVENTS_FILE)).unwrap_or_default(),
    );
    text
}

/// The lines whose user event (`event`, a `LogEvent`) is of `kind`.
pub fn events_named<'a>(lines: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    lines
        .iter()
        .filter(|l| l["event"]["kind"] == kind)
        .collect()
}

/// The diag lines named `name` (the day-1 log's `event`, one level down).
pub fn diags_named<'a>(lines: &'a [serde_json::Value], name: &str) -> Vec<&'a serde_json::Value> {
    lines
        .iter()
        .filter(|l| l["diag"]["event"] == name)
        .collect()
}

/// The kinds of the user events, in order.
pub fn event_kinds(lines: &[serde_json::Value]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l["event"]["kind"].as_str().map(str::to_owned))
        .collect()
}

/// Stores UTC as the host's time zone unless one is stored, so log times do
/// not depend on the zone of the machine the tests run on.
pub fn fix_zone(wd: &mut Wd) {
    if wd.host_settings().time_zone.is_none() {
        let mut settings = wd.host_settings().clone();
        settings.time_zone = Some(TimeZoneName::new("UTC").unwrap());
        // A store set to fail keeps the system zone; such tests do not
        // look at times.
        let _ = wd.set_host_settings(settings);
    }
}

/// The loop's owner-thread part, driven step by step: work runs on the test
/// thread, right away or when the test says.
pub struct Sim {
    pub engine: Eng,
    pub clock: Arc<FakeClock>,
    pub lab: Lab,
    pub store: MemoryStore,
    pub transport: Script,
    next_call: u64,
    /// Answers to requests that had to wait.
    pub released: BTreeMap<CallId, ResponseEnvelope>,
}

impl Sim {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wd: Wd,
        notifier: Service,
        lab: &Lab,
        clock: &Arc<FakeClock>,
        store: &MemoryStore,
        access: AccessList,
        events: Option<EventLog>,
        options: &Options,
    ) -> Self {
        let mut wd = wd;
        fix_zone(&mut wd);
        Self {
            engine: Engine::new(wd, notifier, access, events, options),
            clock: Arc::clone(clock),
            lab: lab.clone(),
            store: store.clone(),
            transport: Script::default(),
            next_call: 0,
            released: BTreeMap::new(),
        }
    }

    /// The same loop with the system's zone and the host name taken from
    /// the test from now on. `Engine::new` has already read this machine's,
    /// and replacing the zone's source reads the host name once more before
    /// its own source is replaced: only read, never what a test checks.
    #[allow(dead_code)]
    pub fn with_sources(
        self,
        zone: fn() -> Option<jiff::tz::TimeZone>,
        host: fn() -> Option<String>,
    ) -> Self {
        Self {
            engine: self.engine.with_system_zone(zone).with_host_name(host),
            ..self
        }
    }

    /// A rig of `tunnels` (see [`prepared`]) with nothing configured for
    /// notifications.
    pub fn of(tunnels: &[&str], events: Option<EventLog>) -> Self {
        let lab = Lab::new(tunnels);
        let clock = Arc::new(FakeClock::new(T0));
        let store = MemoryStore::new();
        let wd = prepared(&lab, &clock, &store, tunnels);
        Self::new(
            wd,
            quiet_notifier(),
            &lab,
            &clock,
            &store,
            AccessList::default(),
            events,
            &Options::default(),
        )
    }

    pub fn now_secs(&self) -> u64 {
        use wgal_core::Clock;
        self.clock.now().since_origin().as_secs()
    }

    /// One turn; returns the work it handed out without running it.
    pub fn turn_only(&mut self) -> (Vec<WorkItem>, bool) {
        let turn = self.engine.turn();
        self.released.extend(turn.answers);
        let job = turn.job.is_some();
        if let Some(job) = turn.job {
            let delivery = job.run(&self.transport);
            self.engine.delivered(job, delivery);
        }
        (turn.work, job)
    }

    pub fn finish(&mut self, item: &WorkItem) {
        let result = self.engine.executor().run(item);
        let answers = self.engine.finished(result);
        self.released.extend(answers);
    }

    /// Turns and runs everything handed out until a turn hands out nothing.
    pub fn settle(&mut self) {
        for _ in 0..1000 {
            let (work, job) = self.turn_only();
            if work.is_empty() && !job {
                return;
            }
            for item in &work {
                self.finish(item);
            }
        }
        panic!("the loop kept handing out work at one instant");
    }

    /// Sleeps as the owner thread would, `secs` of fake time, settling at
    /// every wake-up. Returns how many times it woke up.
    pub fn run(&mut self, secs: u64) -> u64 {
        use wgal_core::Clock;
        let end = self.clock.now().since_origin() + Duration::from_secs(secs);
        let mut wakes = 0;
        self.settle();
        loop {
            let now = self.clock.now().since_origin();
            if now >= end {
                return wakes;
            }
            let wait = self.engine.idle_wait();
            assert!(!wait.is_zero(), "the loop would spin");
            self.clock.advance(wait.min(end - now)).unwrap();
            wakes += 1;
            self.settle();
        }
    }

    /// Hands a request to the engine; `None` if it has to wait.
    pub fn call(&mut self, caller: &Caller, request: Request) -> Option<ResponseEnvelope> {
        self.next_call += 1;
        let call = CallId(self.next_call);
        self.engine.request(
            call,
            caller,
            envelope(&format!("req-{}", self.next_call), request),
        )
    }

    /// The id the next [`Self::call`] uses.
    pub fn next_call_id(&self) -> CallId {
        CallId(self.next_call + 1)
    }

    pub fn ok(&mut self, request: Request) -> ResponseEnvelope {
        let answer = self
            .call(&admin(), request.clone())
            .unwrap_or_else(|| panic!("{request:?} had to wait"));
        assert!(error_code(&answer).is_none(), "{request:?}: {answer:?}");
        answer
    }

    pub fn snapshot(&mut self) -> Snapshot {
        snapshot_of(&self.ok(Request::GetSnapshot))
    }

    pub fn host(&mut self) -> HostStatus {
        self.snapshot().host
    }

    /// Sets the host settings through the loop, as the GUI does.
    pub fn host_settings(&mut self, workers: Option<u8>, zone: Option<&str>) -> ResponseEnvelope {
        self.call(
            &user(),
            Request::UpdateHostSettings {
                settings: HostSettings {
                    workers: workers.map(|n| WorkerCount::new(n).unwrap()),
                    time_zone: zone.map(|z| TimeZoneName::new(z).unwrap()),
                },
            },
        )
        .expect("host settings never wait")
    }

    pub fn status(&mut self, name: &str) -> TunnelStatus {
        self.snapshot()
            .tunnels
            .iter()
            .find(|t| t.tunnel_id == id(name))
            .cloned()
            .unwrap()
    }
}

pub fn kind(request: &Request) -> &'static str {
    match request {
        Request::GetSnapshot => "get_snapshot",
        Request::DiscoverTunnels => "discover_tunnels",
        Request::RegisterTunnel { .. } => "register_tunnel",
        Request::UnregisterTunnel { .. } => "unregister_tunnel",
        Request::UpdateMonitorSettings { .. } => "update_monitor_settings",
        Request::SetTunnelMonitoring { .. } => "set_tunnel_monitoring",
        Request::ProbeNow { .. } => "probe_now",
        Request::Connect { .. } => "connect",
        Request::Disconnect { .. } => "disconnect",
        Request::Reconnect { .. } => "reconnect",
        Request::SetAutoRecovery { .. } => "set_auto_recovery",
        Request::SetStartWhenStoppedExternally { .. } => "set_start_when_stopped_externally",
        Request::SetTunnelNotifications { .. } => "set_tunnel_notifications",
        Request::SetRecoveryRetry { .. } => "set_recovery_retry",
        Request::UpdateNotificationSettings { .. } => "update_notification_settings",
        Request::SetBotToken { .. } => "set_bot_token",
        Request::ClearBotToken => "clear_bot_token",
        Request::SetMonitoringPaused { .. } => "set_monitoring_paused",
        Request::SetMcpEnabled { .. } => "set_mcp_enabled",
        Request::SetMcpTelegramEnabled { .. } => "set_mcp_telegram_enabled",
        Request::SendTestNotification => "send_test_notification",
        Request::SetNotificationTemplates { .. } => "set_notification_templates",
        Request::SetCliLanguage { .. } => "set_cli_language",
        Request::UpdateHostSettings { .. } => "update_host_settings",
        Request::ReadEventLog { .. } => "read_event_log",
        Request::ExportSettings => "export_settings",
        Request::ImportSettings { .. } => "import_settings",
    }
}

/// Every request kind, the two automatic options and the per-tunnel
/// switches both ways, naming `name`.
pub fn every_request(name: &str) -> Vec<Request> {
    let tunnel_id = id(name);
    vec![
        Request::GetSnapshot,
        Request::DiscoverTunnels,
        Request::RegisterTunnel {
            tunnel_id: tunnel_id.clone(),
        },
        Request::UnregisterTunnel {
            tunnel_id: tunnel_id.clone(),
        },
        Request::UpdateMonitorSettings {
            tunnel_id: tunnel_id.clone(),
            settings: MonitorSettings {
                recovery_target: Some(icmp(1)),
                observe_targets: BoundedVec::new(),
                probe: ProbeTiming::DEFAULT,
            },
        },
        Request::SetTunnelMonitoring {
            tunnel_id: tunnel_id.clone(),
            enabled: true,
        },
        Request::SetTunnelMonitoring {
            tunnel_id: tunnel_id.clone(),
            enabled: false,
        },
        Request::ProbeNow {
            tunnel_id: tunnel_id.clone(),
        },
        Request::Connect {
            tunnel_id: tunnel_id.clone(),
        },
        Request::Disconnect {
            tunnel_id: tunnel_id.clone(),
        },
        Request::Reconnect {
            tunnel_id: tunnel_id.clone(),
        },
        Request::SetAutoRecovery {
            tunnel_id: tunnel_id.clone(),
            enabled: true,
        },
        Request::SetAutoRecovery {
            tunnel_id: tunnel_id.clone(),
            enabled: false,
        },
        Request::SetStartWhenStoppedExternally {
            tunnel_id: tunnel_id.clone(),
            enabled: true,
        },
        Request::SetStartWhenStoppedExternally {
            tunnel_id: tunnel_id.clone(),
            enabled: false,
        },
        Request::SetTunnelNotifications {
            tunnel_id: tunnel_id.clone(),
            enabled: true,
        },
        Request::SetTunnelNotifications {
            tunnel_id: tunnel_id.clone(),
            enabled: false,
        },
        Request::SetRecoveryRetry {
            tunnel_id: tunnel_id.clone(),
            retry: RecoveryRetry::new(None, 300).unwrap(),
        },
        Request::SetMcpEnabled { enabled: true },
        Request::SetMcpTelegramEnabled { enabled: true },
        Request::ExportSettings,
        Request::ImportSettings {
            document: SettingsDocument {
                format: SettingsFormat::WgalSettings,
                version: SETTINGS_DOCUMENT_VERSION,
                watchdog_version: ProductVersion::current(),
                exported_at_unix_ms: 0,
                host: HostSettingsEntry {
                    workers: None,
                    time_zone: None,
                    notifications_enabled: false,
                },
                tunnels: BoundedVec::new(),
            },
            options: ImportOptions {
                match_by: ImportMatch::Name,
                register_missing: false,
                monitor_new: false,
                dry_run: true,
            },
        },
        Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("Lab laptop").unwrap(),
            chat_id: Some(ChatId::new(CHAT).unwrap()),
        },
        Request::SetBotToken {
            token: BotToken::new(TOKEN).unwrap(),
        },
        Request::ClearBotToken,
        Request::SetMonitoringPaused { paused: true },
        Request::SendTestNotification,
        Request::SetNotificationTemplates {
            templates: fixture_templates(),
        },
        Request::SetCliLanguage {
            language: Some(LanguageTag::new("ja").unwrap()),
        },
        Request::UpdateHostSettings {
            settings: HostSettings {
                workers: Some(WorkerCount::new(4).unwrap()),
                time_zone: Some(TimeZoneName::new("UTC").unwrap()),
            },
        },
        Request::ReadEventLog {
            filter: EventFilter::Tunnel { tunnel_id },
            before_seq: None,
        },
    ]
}
