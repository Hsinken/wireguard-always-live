//! Windows adapter: `WireGuardTunnel$<name>` services driven through the
//! Service Control Manager, with source and route evidence from the IP
//! Helper API (plan §4.2). Owned by work package WGW-001-D.
//!
//! A tunnel's [`TunnelBinding`] is its WireGuard for Windows tunnel name,
//! validated as a [`TunnelName`] before it becomes the one service name
//! `WireGuardTunnel$<name>` or is compared with an adapter name. The adapter
//! talks to Windows only through two narrow traits, [`ServiceControl`] and
//! [`IpHelper`], so all of its logic is unit-tested with fakes on every OS;
//! the real implementations live in `sys.rs`, the one file allowed to use
//! `unsafe`. It never runs PowerShell or `sc.exe`, never touches
//! `WireGuardManager`, and never creates, deletes or reconfigures a service.
//!
//! Every call into Windows runs on a worker thread under a deadline: SCM
//! requests can block while the SCM waits for a service. A call that misses
//! its deadline is reported as `Timeout`, and its worker is left to finish.
//!
//! # Helper threads
//!
//! Those workers, and the threads that ask the default gateways (see "This
//! host offline"), are counted from before each starts until it is done,
//! also after its caller stopped waiting for it: a slot is given back by the
//! thread itself, on its way out, once its calls into Windows have returned
//! and before it hands over the answer, which never blocks (FIX-08; AR-09,
//! N-04). So a caller that has its answer always finds its slot free. They
//! come from two shares that never borrow from each other:
//!
//! - reads ([`Limits::read_threads`]): listing adapters and services, route
//!   lookups, echoes and the check of the host's own network with its
//!   gateways;
//! - tunnel services' state queries ([`Limits::control_threads`]), at most
//!   [`Limits::service_threads`] for any one service;
//! - starts and stops ([`Limits::command_threads`]), at most
//!   [`Limits::service_commands`] for any one service;
//! - the state queries of a service while it is being controlled, from its
//!   first start or stop until twice [`Limits::control`] after its latest
//!   ([`Limits::poll_threads`], at most [`Limits::service_polls`] for any
//!   one service), once its ordinary queries are used up.
//!
//! So reads stuck in Windows, however many, and a service's ordinary state
//! queries stuck in the SCM never keep a stop, the state queries after it
//! and the start from starting (Codex review of FIX-08, P2-02), which is
//! what the worker the runtime keeps for the control slot is for; the lanes
//! and gates below order requests and do not do this. A service whose own
//! calls are stuck uses up only its own slots. A call whose share is full starts no thread
//! and returns `Timeout` at once, which the core takes for a broken probe,
//! never a failed target; a start or stop refused this way takes no place
//! in its service's order and says that it was not sent. A gateway that
//! cannot be asked is unknown, and a check of the host's own network that
//! could not ask what would have decided it reaches no verdict: the probe
//! it was for reports that `Timeout` instead of its lost echo.
//!
//! # Control calls after a timeout
//!
//! A start or stop is opened first and sent second (see [`ServiceControl`]).
//! Per service, the adapter sends at most one request at a time, in the order
//! the requests were made, and sends a request only before its deadline,
//! while its caller has not withdrawn it, and while no newer request for that
//! service exists. The deadline closes a request for good: from then on
//! nothing is sent on its behalf, whatever the worker was doing when it
//! passed, so once `start` or `stop` has returned `Timeout` nothing more can
//! be sent for it. A request still being opened is withdrawn, and a later
//! request is never overtaken by an earlier one. The `Timeout` detail says
//! which happened: "withdrawn" means it was never sent; "sent" means
//! `StartServiceW` or the stop control was already under way when the
//! deadline passed. That is the one remaining window: a sent request cannot
//! be recalled, so it may take effect when the SCM finishes it (within about
//! 30 s), and the next request for the service waits for it. If that next
//! request's own deadline passes while it waits, it is withdrawn and reported
//! as such, and the caller has to ask again once `query_state` shows where the
//! service ended up.
//!
//! A stop that reaches the service while it is still between states (most
//! often a start that timed out after it was sent, so the service is still
//! `START_PENDING`) is refused by the SCM with `SERVICE_CANNOT_ACCEPT_CTRL`
//! and changes nothing. A start is refused with
//! `ERROR_SERVICE_ALREADY_RUNNING` whenever the service is not `STOPPED`,
//! including while a stop is still under way (`STOP_PENDING`), and changes
//! nothing either. Such a refusal is read against the service's state right
//! after it. A start refused while the service is `RUNNING` or
//! `START_PENDING` is already where it would take the tunnel and succeeds; a
//! stop refused while the service is not between states, or a start refused
//! while it is paused, is the service's answer and is returned as it is.
//! Otherwise the refusal is not the request's answer: the request waits,
//! still inside its own deadline and under the same rules as above, until
//! the service has settled and sends itself again. If the deadline passes
//! first, nothing more is sent and the call returns `Timeout` saying the
//! request was not sent and should be asked again; it never ends in a
//! refusal or a success the caller could read as final. A request whose
//! deadline passes while the SCM still holds it is reported as "sent", and if
//! the SCM refuses it only afterwards, it is not sent again either.
//!
//! # Path evidence
//!
//! WireGuard for Windows names the tunnel's adapter after the tunnel; an
//! adapter is taken as the tunnel's only if it has that name, is the only one
//! with it, and is a virtual adapter (`IF_TYPE_PROP_VIRTUAL`, to be confirmed
//! on a device), so a renamed physical adapter is never probed as a tunnel.
//! Before every probe the adapter re-reads that adapter, asks `GetBestRoute2` which
//! interface and source Windows would use for the target with no constraint,
//! and refuses unless both are the tunnel's own and the source address is on
//! no other adapter. A target that is one of the host's own addresses, on
//! any adapter, is refused before that lookup: Windows routes it to the
//! interface holding it, from itself, and answers it locally. The echo is
//! then sent with `IcmpSendEcho2Ex` from that source, and the adapter is
//! read again afterwards so an echo that straddled a reconnect is
//! discarded. A source address is not by itself proof that
//! the echo left through the tunnel (plan §4.2), so `bound_icmp_probe` was
//! `Unknown` until the on-device W3 (`docs/testing.md`). It is
//! `Supported` since the
//! owner's decision of 2026-11-03, after W3 with one tunnel under test: the
//! baseline through the tunnel, a target routed through the physical adapter
//! refused as RouteMismatch, and a silent address inside the tunnel's routes
//! never answered. Two tunnels whose routes overlap were not part of it; that
//! case is still unproven. TCP probes are `Unsupported` until a source-bound connect is
//! available, and the official manager offers no metrics to an unprivileged
//! reader, so `read_metrics` is `Unsupported` too.
//!
//! # This host offline (open finding D-3)
//!
//! An echo that got no reply, or could not be sent for lack of a route, may
//! mean the tunnel is broken or that this computer has no network at all
//! (a Wi-Fi router rebooting, say). Before such a result is reported, the
//! adapter looks for positive evidence that the host has no way out, as
//! described in [`local`]: no adapter that is not a tunnel is up with a
//! usable IPv4 or IPv6 address, or the uplinks have default gateways and
//! none of them answers ARP or IPv6 neighbor discovery. Only then does the
//! probe report `LocalNetworkOffline`, which the core neither counts as a
//! failure nor recovers from, and after which it waits out a grace period
//! once the network is back (plan §6.3); a missing default route is not
//! such evidence (Codex review of wave three, P1-4). The check runs only
//! after a failed probe, never after a reply, so a healthy host pays nothing
//! for it. Its verdict is kept for [`Limits::local_verdict`] behind a lock,
//! so tunnels failing at the same moment share one reading of the adapters
//! and the routing table and one exchange per gateway, and there is at most
//! one check that often however many tunnels fail. Reading adapters, routes
//! and neighbor entries changes nothing; asking a gateway for its address
//! (`ResolveIpNetEntry2`) makes Windows replace its dynamic neighbor entry
//! for that gateway with a fresh one, as Windows' own neighbor
//! unreachability detection does, and is never done for a static entry.
//! Nothing here adds, changes or deletes a route, neighbor entry, interface
//! or firewall rule.
//!
//! # For the watchdog service
//!
//! `sys.rs` also gives `wgal-runtime`'s Windows service the safe wrappers it
//! needs and cannot write itself (it has no `unsafe`): the named pipe server
//! and its clients' identity, SID validation, the ProgramData folder,
//! opening the installation's own files without following reparse points
//! and reading who may change them, and notices of changes to the host's
//! IPv4 interfaces (`InterfaceWatch`). They act only on what the service
//! names from its own constants; nothing here takes a name, path or SID from
//! IPC.
//!
//! The desktop GUI uses exactly two of them: `connect_watchdog_pipe` (with
//! `is_not_the_watchdog` for its error), and `final_path_of_file`, which
//! tells where a file the GUI has just created and still holds open really
//! is, from its handle.

pub mod local;
mod name;
mod net;
mod scm;
#[cfg(windows)]
mod sys;

use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{
    AdapterError, DiscoveredBinding, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, TunnelBinding,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, DisplayName, OsState, ProbeKind,
    TargetAddress,
};

pub use local::{
    DefaultRoute, Gateway, InterfaceChange, LocalNetwork, Offline, Resolution, WayOut,
};
pub use name::{MAX_TUNNEL_NAME_LEN, SERVICE_PREFIX, ServiceName, TunnelName};
pub use net::{
    AdapterInfo, BestRoute, EchoReply, IF_TYPE_PROP_VIRTUAL, IpHelper, MAX_ADAPTERS, MAX_ADDRESSES,
    NetError, classify_echo, status,
};
pub use scm::{Control, MAX_SERVICES, ScmError, ServiceControl, ServiceState};
#[cfg(windows)]
pub use sys::{
    CHANGE_RIGHTS, InterfaceWatch, LOCAL_SYSTEM_SID, MAX_PIPE_INSTANCES, MAX_PIPE_NAME_LEN,
    MAX_SID_TEXT_LEN, NotTheWatchdog, ObjectSecurity, PIPE_CLIENT_ACCESS, PIPE_CLIENT_QOS,
    PIPE_NAME_PREFIX, PipeError, PipeInstance, PipeListener, PipeStopper, SecretProtectionError,
    ServiceHandle, SystemIpHelper, SystemServiceControl, TokenIdentity, Win32Error,
    connect_watchdog_pipe, current_identity, final_path_of_file, is_canonical_sid,
    is_not_the_watchdog, is_reparse_point, link_count, object_security, open_append_no_follow,
    open_file_no_follow, open_folder_no_follow, program_data_dir, protect_secret,
    restore_stdin_echo, unprotect_secret,
};
#[cfg(windows)]
pub use sys::{console_mode_without_echo, hide_stdin_echo};

/// Deadlines for calls into Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Status queries, enumerations and route lookups.
    pub query: Duration,
    /// Start and stop requests, including any wait for a service that is
    /// still between states. It must stay well below the core's
    /// `Timing::control_phase_timeout`, which counts from when the core hands
    /// the request over: the core confirms the result by polling the state
    /// after the call answers, and a call that answered at the end of the
    /// phase would fail a recovery that worked.
    pub control: Duration,
    /// Longest echo wait accepted; longer probe timeouts are cut to this.
    pub max_echo: Duration,
    /// Time an echo gets beyond its own timeout before it is abandoned.
    pub echo_grace: Duration,
    /// How often a start or stop refused by a service still between states
    /// checks whether the service has settled, within its own deadline.
    pub settle_poll: Duration,
    /// How long the default gateways get to answer ARP or IPv6 neighbor
    /// discovery when the host's own network is checked. A gateway on the
    /// local network answers within milliseconds; Windows itself keeps
    /// asking for about three seconds.
    pub neighbor: Duration,
    /// How long a verdict on the host's own network is reused. Zero checks
    /// again every time.
    pub local_verdict: Duration,
    /// Most helper threads alive at once for reads: listing adapters and
    /// services, route lookups, echoes, the host's own network check and
    /// its gateways (see the module documentation).
    pub read_threads: usize,
    /// Most helper threads alive at once for the tunnel services' state
    /// queries. Reads never take these.
    pub control_threads: usize,
    /// Of [`Limits::control_threads`], most for any one service.
    pub service_threads: usize,
    /// Most helper threads alive at once for starts and stops.
    pub command_threads: usize,
    /// Of [`Limits::command_threads`], most for any one service.
    pub service_commands: usize,
    /// Most helper threads alive at once for the state queries of services
    /// being controlled, once their ordinary queries are used up.
    pub poll_threads: usize,
    /// Of [`Limits::poll_threads`], most for any one service.
    pub service_polls: usize,
}

impl Limits {
    pub const DEFAULT: Self = Self {
        query: Duration::from_secs(5),
        // 5 s short of the core's 30 s phase (`Timing::control_phase_timeout`,
        // plan §6.2), so the core always has at least five of its one-second
        // polls left. The SCM itself may hold a start for up to about 30 s; a
        // request still held at this deadline is reported as a Timeout that
        // was "sent" and may still take effect, which the core allows for.
        control: Duration::from_secs(25),
        max_echo: Duration::from_secs(30),
        echo_grace: Duration::from_secs(2),
        settle_poll: Duration::from_millis(250),
        neighbor: Duration::from_secs(2),
        // At most one check this often, however many tunnels fail. Rounds of
        // different tunnels are a couple of seconds apart, so a few tunnels
        // share each check; after a network change their rounds fall due
        // together and all of them share one.
        local_verdict: Duration::from_secs(5),
        // At most `MAX_WORKERS` (17) runtime workers wait on one read each,
        // and one network check at a time on up to `local::MAX_GATEWAYS` (8)
        // gateways: 25 in all. The rest is room for calls left behind after
        // their deadline, which Windows normally ends within seconds.
        read_threads: 64,
        // Four for each of the most tunnels the watchdog monitors.
        control_threads: wgal_protocol::MAX_TUNNELS * 4,
        // A tunnel has one item out at a time, so one query waited for,
        // and room for three left behind.
        service_threads: 4,
        // A start or stop waited for, and room for one the SCM still holds
        // (up to about 30 s), which the next one waits behind anyway.
        command_threads: wgal_protocol::MAX_TUNNELS * 2,
        service_commands: 2,
        // A poll waited for and one left behind, for a service whose
        // ordinary queries are stuck while it is stopped and started.
        poll_threads: wgal_protocol::MAX_TUNNELS * 2,
        service_polls: 2,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// [`PlatformAdapter`] for WireGuard for Windows tunnel services.
///
/// Nothing here has been verified on a Windows host with tunnels yet, so
/// [`capabilities`](PlatformAdapter::capabilities) reports query, control,
/// bound ICMP and background operation as `Unknown` until on-device
/// acceptance proves them.
#[derive(Debug)]
pub struct WindowsAdapter<S, N> {
    scm: Arc<S>,
    net: Arc<N>,
    limits: Limits,
    lanes: Lanes,
    /// Helper threads alive for reads, the tunnel services' state queries,
    /// their starts and stops, and their queries while being controlled.
    reads: Arc<Count>,
    controls: Arc<Count>,
    commands: Arc<Count>,
    polls: Arc<Count>,
    /// The last verdict on the host's own network and when it was reached.
    /// Held while a check runs, so concurrent failures wait for one check.
    local: Mutex<Option<(Instant, LocalNetwork)>>,
}

#[cfg(windows)]
impl WindowsAdapter<SystemServiceControl, SystemIpHelper> {
    pub fn new() -> Self {
        Self::with_parts(
            Arc::new(SystemServiceControl),
            Arc::new(SystemIpHelper),
            Limits::DEFAULT,
        )
    }
}

#[cfg(windows)]
impl Default for WindowsAdapter<SystemServiceControl, SystemIpHelper> {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper threads alive in one share, each counted from before it starts
/// until it ends.
#[derive(Debug, Default)]
struct Count(AtomicUsize);

impl Count {
    /// A slot, if fewer than `cap` are taken.
    fn claim(self: &Arc<Self>, cap: usize) -> Option<Slot> {
        self.0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
                (live < cap).then_some(live + 1)
            })
            .ok()?;
        Some(Slot(Arc::clone(self)))
    }

    #[cfg(test)]
    fn live(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// One taken place in a [`Count`], given back when dropped.
#[derive(Debug)]
struct Slot(Arc<Count>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A slot in a share and in one service's part of it, or none when either
/// is full (a share slot taken first is given back).
fn claim_both(
    share: &Arc<Count>,
    share_cap: usize,
    service: &Arc<Count>,
    service_cap: usize,
) -> Option<Claim> {
    let all = share.claim(share_cap)?;
    let own = service.claim(service_cap)?;
    Some(Claim {
        _share: all,
        _service: Some(own),
    })
}

/// The slots one helper thread holds. It is moved into the thread and
/// dropped there on its way out, once its calls into Windows have returned,
/// or with the thread's closure if the thread could not start.
#[derive(Debug)]
struct Claim {
    _share: Slot,
    _service: Option<Slot>,
}

/// The `Timeout` for a call that was not started because its share of
/// helper threads is full.
fn busy(what: &str) -> AdapterError {
    AdapterError::new(
        AdapterErrorKind::Timeout,
        &format!("{what} was not started: too many earlier calls into Windows are still running"),
    )
}

/// A call that was not started because its share of helper threads is full.
#[derive(Debug)]
struct Busy(AdapterError);

/// Runs `work` on its own thread, which holds `claim` until `work` has
/// returned, and waits at most `limit` for it.
fn bounded<T, F>(claim: Claim, limit: Duration, what: &str, work: F) -> Result<T, AdapterError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("wgal-windows-call".to_owned())
        .spawn(move || {
            let answer = work();
            // Given back only now that the call into Windows has returned,
            // whether or not anyone still waits for it, and before the
            // answer is handed over, so a caller that has its answer always
            // finds the slot free again. A panic in `work` gives it back
            // as it unwinds.
            drop(claim);
            // The receiver is gone only if the caller already gave up; the
            // channel has room, so this never blocks.
            let _ = tx.send(answer);
        })
        .map_err(|_| {
            AdapterError::new(
                AdapterErrorKind::Unsupported,
                &format!("{what}: could not start a worker thread"),
            )
        })?;
    rx.recv_timeout(limit).map_err(|e| match e {
        RecvTimeoutError::Timeout => AdapterError::new(
            AdapterErrorKind::Timeout,
            &format!("{what} did not finish in time"),
        ),
        RecvTimeoutError::Disconnected => AdapterError::new(
            AdapterErrorKind::Unsupported,
            &format!("{what} failed unexpectedly"),
        ),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where one start or stop request stands, shared by its caller and worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Request {
    /// The request has not been sent.
    #[default]
    Waiting,
    /// The service refused the request because it was between states, which
    /// changed nothing; it may be sent again once the service settles.
    Deferred,
    /// The request was handed to the SCM.
    Sent,
}

/// What a caller giving up at its deadline leaves behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Withdrawal {
    /// The request was under way in the SCM and may still take effect.
    Sent,
    /// It was never sent.
    NotSent,
    /// It was refused while the service was between states, and has now been
    /// withdrawn before it could be sent again: nothing took effect.
    Deferred,
}

#[derive(Debug, Default)]
struct GateState {
    request: Request,
    withdrawn: bool,
}

/// Whether one request may still be sent. It closes for good at the caller's
/// deadline or when the caller withdraws, whichever comes first, and stays
/// closed whatever the SCM later answers to a send already under way: a stop
/// refused after its caller has its answer is never sent again.
#[derive(Debug)]
struct Gate {
    /// `None` only for a deadline too far away to represent.
    deadline: Option<Instant>,
    state: Mutex<GateState>,
}

impl Gate {
    fn new(deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            state: Mutex::default(),
        }
    }

    fn open(&self, state: &GateState) -> bool {
        !state.withdrawn
            && state.request != Request::Sent
            && self.deadline.is_none_or(|d| Instant::now() < d)
    }

    fn waiting(&self) -> bool {
        self.open(&lock(&self.state))
    }

    /// Marks the request sent, if it may still be sent.
    fn send(&self) -> bool {
        let mut state = lock(&self.state);
        let send = self.open(&state);
        if send {
            state.request = Request::Sent;
        }
        send
    }

    /// The SCM refused the request without acting on it; it may be sent
    /// again if the gate is still open.
    fn defer(&self) {
        let mut state = lock(&self.state);
        if state.request == Request::Sent {
            state.request = Request::Deferred;
        }
    }

    /// The caller gives up. Nothing is sent after this, even if a send under
    /// way is refused afterwards.
    fn withdraw(&self) -> Withdrawal {
        let mut state = lock(&self.state);
        state.withdrawn = true;
        match state.request {
            Request::Sent => Withdrawal::Sent,
            Request::Deferred => Withdrawal::Deferred,
            Request::Waiting => Withdrawal::NotSent,
        }
    }
}

/// What a request the SCM refused because of the service's state does next,
/// judged by the state the service is in right after the refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    /// The service is already where the request would take it.
    Done,
    /// The refusal is the service's answer; waiting would not change it.
    Final,
    /// The service is between states, or has just left the state the
    /// refusal was about: wait until it has settled, then send again.
    Wait,
}

/// The SCM refuses a stop with `SERVICE_CANNOT_ACCEPT_CTRL` while the
/// service is between states. Refused although it is not: that is the
/// service's answer, not something waiting would change.
fn stop_refused(state: ServiceState) -> Refused {
    if state.is_pending() {
        Refused::Wait
    } else {
        Refused::Final
    }
}

/// `StartServiceW` refuses a start with `ERROR_SERVICE_ALREADY_RUNNING`
/// whenever the service is not `STOPPED`, including while a stop is still
/// under way. Only a running or starting service is already where the start
/// would take it.
fn start_refused(state: ServiceState) -> Refused {
    match state {
        ServiceState::Running | ServiceState::StartPending => Refused::Done,
        // The stop that caused the refusal finished in between: the start
        // changed nothing and can be sent again.
        ServiceState::Stopped => Refused::Wait,
        pending if pending.is_pending() => Refused::Wait,
        // Paused (tunnel services never pause) or undocumented.
        _ => Refused::Final,
    }
}

/// How a request refused because of the service's state waits for the
/// service to settle, within its own deadline.
struct Settle<'a> {
    /// The error the SCM refuses the request with when the service's state
    /// does not let it through. Such a refusal changes nothing.
    refusal: ScmError,
    /// Reads the service's state.
    state: &'a dyn Fn() -> Result<ServiceState, ScmError>,
    /// What the refusal means in the state read right after it.
    judge: fn(ServiceState) -> Refused,
    poll: Duration,
}

/// Start and stop requests for one service: numbered in the order they are
/// made, and sent one at a time. It also counts the service's helper
/// threads: its state queries, its starts and stops, and its queries while
/// it is being controlled, apart.
#[derive(Debug, Default)]
struct Lane {
    newest: AtomicU64,
    sending: Mutex<()>,
    threads: Arc<Count>,
    commands: Arc<Count>,
    polls: Arc<Count>,
    /// When its latest start or stop was asked for.
    controlled_at: Mutex<Option<Instant>>,
}

/// Why a control request was not answered by the SCM.
#[derive(Debug, PartialEq, Eq)]
enum Unsent {
    Scm(ScmError),
    /// Its gate closed: the deadline passed or its caller withdrew it. Not
    /// sent, or not sent again after a refusal.
    Withdrawn,
    /// A newer request for the service replaced it before it was sent.
    Replaced,
}

impl Lane {
    fn ticket(&self) -> u64 {
        self.newest.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_newest(&self, ticket: u64) -> bool {
        self.newest.load(Ordering::SeqCst) == ticket
    }

    fn wanted(&self, ticket: u64, gate: &Gate) -> Result<(), Unsent> {
        if !gate.waiting() {
            Err(Unsent::Withdrawn)
        } else if !self.is_newest(ticket) {
            Err(Unsent::Replaced)
        } else {
            Ok(())
        }
    }

    /// Opens, then sends only if the request is still wanted and newest. The
    /// send happens while holding `sending`, so a later request waits for an
    /// earlier one the SCM is still working on instead of overtaking it.
    ///
    /// A send refused with `settle.refusal` is judged by the service's state
    /// right after it: done, final, or waited out until the service has
    /// settled and sent again, for as long as it is still wanted and newest.
    fn run<H>(
        &self,
        ticket: u64,
        gate: &Gate,
        open: impl FnOnce() -> Result<H, ScmError>,
        send: impl Fn(&H) -> Result<(), ScmError>,
        settle: &Settle<'_>,
    ) -> Result<(), Unsent> {
        self.wanted(ticket, gate)?;
        let handle = open().map_err(Unsent::Scm)?;
        let _one_at_a_time = lock(&self.sending);
        loop {
            self.wanted(ticket, gate)?;
            // Checked again under the gate's lock: the deadline or the caller
            // may have closed it since.
            if !gate.send() {
                return Err(Unsent::Withdrawn);
            }
            let refused = match send(&handle) {
                Err(e) if e == settle.refusal => e,
                answer => return answer.map_err(Unsent::Scm),
            };
            gate.defer();
            match (settle.judge)((settle.state)().map_err(Unsent::Scm)?) {
                Refused::Done => return Ok(()),
                Refused::Final => return Err(Unsent::Scm(refused)),
                Refused::Wait => {}
            }
            // At least one poll before any resend, so a service that keeps
            // refusing is never hammered.
            loop {
                thread::sleep(settle.poll);
                self.wanted(ticket, gate)?;
                if !(settle.state)().map_err(Unsent::Scm)?.is_pending() {
                    break;
                }
            }
        }
    }
}

/// One [`Lane`] per service ever queried or controlled, keyed
/// case-insensitively as the SCM compares names. Only validated tunnel
/// services get one.
#[derive(Debug, Default)]
struct Lanes(Mutex<HashMap<String, Arc<Lane>>>);

impl Lanes {
    fn get(&self, service: &ServiceName) -> Arc<Lane> {
        let key = service.as_str().to_ascii_lowercase();
        Arc::clone(lock(&self.0).entry(key).or_default())
    }
}

fn stale_interface() -> AdapterError {
    AdapterError::new(
        AdapterErrorKind::NotFound,
        "the tunnel's network adapter has changed or gone; resolve it again",
    )
}

fn route_mismatch(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::RouteMismatch, detail)
}

/// The one up adapter named after the tunnel. An adapter of another type
/// that merely carries the name (a renamed physical adapter, say) is never
/// taken for the tunnel.
fn find_adapter<'a>(
    adapters: &'a [AdapterInfo],
    name: &TunnelName,
) -> Result<&'a AdapterInfo, AdapterError> {
    let mut named = adapters.iter().filter(|a| a.friendly_name == name.as_str());
    match (named.next(), named.next()) {
        (None, _) => Err(AdapterError::new(
            AdapterErrorKind::NotFound,
            "the tunnel has no network adapter",
        )),
        (Some(_), Some(_)) => Err(AdapterError::new(
            AdapterErrorKind::ManagerConflict,
            "several network adapters carry the tunnel's name",
        )),
        (Some(adapter), None) if adapter.if_type != IF_TYPE_PROP_VIRTUAL => Err(AdapterError::new(
            AdapterErrorKind::ManagerConflict,
            "the adapter carrying the tunnel's name is not a WireGuard adapter",
        )),
        (Some(adapter), None) if !adapter.up => Err(AdapterError::new(
            AdapterErrorKind::NotFound,
            "the tunnel's network adapter is not up",
        )),
        (Some(adapter), None) => Ok(adapter),
    }
}

impl<S, N> WindowsAdapter<S, N>
where
    S: ServiceControl + 'static,
    N: IpHelper + 'static,
{
    pub fn with_parts(scm: Arc<S>, net: Arc<N>, limits: Limits) -> Self {
        Self {
            scm,
            net,
            limits,
            lanes: Lanes::default(),
            reads: Arc::default(),
            controls: Arc::default(),
            commands: Arc::default(),
            polls: Arc::default(),
            local: Mutex::new(None),
        }
    }

    /// A read's slot, if the read share is not full.
    fn read_claim(&self, what: &str) -> Result<Claim, Busy> {
        self.reads
            .claim(self.limits.read_threads)
            .map(|slot| Claim {
                _share: slot,
                _service: None,
            })
            .ok_or_else(|| Busy(busy(what)))
    }

    /// Slots for a state query of the service `lane` belongs to: an
    /// ordinary one, or, once those are used up while the service is being
    /// controlled, one of its polls.
    fn query_claim(&self, lane: &Lane) -> Option<Claim> {
        claim_both(
            &self.controls,
            self.limits.control_threads,
            &lane.threads,
            self.limits.service_threads,
        )
        .or_else(|| {
            let since = (*lock(&lane.controlled_at))?;
            let window = self.limits.control.saturating_mul(2);
            if since.elapsed() >= window {
                return None;
            }
            claim_both(
                &self.polls,
                self.limits.poll_threads,
                &lane.polls,
                self.limits.service_polls,
            )
        })
    }

    /// Slots for a start or stop of the service `lane` belongs to.
    fn command_claim(&self, lane: &Lane) -> Option<Claim> {
        let claim = claim_both(
            &self.commands,
            self.limits.command_threads,
            &lane.commands,
            self.limits.service_commands,
        )?;
        *lock(&lane.controlled_at) = Some(Instant::now());
        Some(claim)
    }

    /// Helper threads alive now, in every share.
    #[cfg(test)]
    fn helper_threads(&self) -> usize {
        self.reads.live() + self.controls.live() + self.commands.live() + self.polls.live()
    }

    /// The SCM's answer, or an error if the call missed its deadline.
    fn scm_call<T, F>(
        &self,
        claim: Claim,
        limit: Duration,
        what: &str,
        call: F,
    ) -> Result<Result<T, ScmError>, AdapterError>
    where
        T: Send + 'static,
        F: FnOnce(&S) -> Result<T, ScmError> + Send + 'static,
    {
        let scm = Arc::clone(&self.scm);
        bounded(claim, limit, what, move || call(&scm))
    }

    fn net_call<T, F>(&self, limit: Duration, what: &str, call: F) -> Result<T, AdapterError>
    where
        T: Send + 'static,
        F: FnOnce(&N) -> Result<T, NetError> + Send + 'static,
    {
        self.try_net_call(limit, what, call)
            .map_err(|Busy(error)| error)?
    }

    /// A read from the read share: [`Busy`] if it was not started, else its
    /// answer or error.
    fn try_net_call<T, F>(
        &self,
        limit: Duration,
        what: &str,
        call: F,
    ) -> Result<Result<T, AdapterError>, Busy>
    where
        T: Send + 'static,
        F: FnOnce(&N) -> Result<T, NetError> + Send + 'static,
    {
        let claim = self.read_claim(what)?;
        let net = Arc::clone(&self.net);
        Ok(bounded(claim, limit, what, move || call(&net))
            .and_then(|answer| answer.map_err(|e| e.to_adapter_error(what))))
    }

    fn adapters(&self) -> Result<Vec<AdapterInfo>, AdapterError> {
        let adapters = self.net_call(self.limits.query, "listing network adapters", |n| {
            n.adapters()
        })?;
        if adapters.len() > MAX_ADAPTERS {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "Windows reported more network adapters than WGAL handles",
            ));
        }
        Ok(adapters)
    }

    /// The adapter `interface` was resolved to, if it is still the same one:
    /// same tunnel name, same interface index and LUID (the incarnation),
    /// still holding the source. Windows can hand a rebuilt adapter the old
    /// index; the LUID tells them apart.
    fn current<'a>(
        adapters: &'a [AdapterInfo],
        interface: &ResolvedInterface,
    ) -> Result<&'a AdapterInfo, AdapterError> {
        // Only tunnel adapters are ever probed, whatever the caller passes.
        let name = TunnelName::new(&interface.name).ok_or_else(stale_interface)?;
        let adapter = find_adapter(adapters, &name).map_err(|e| match e.kind {
            AdapterErrorKind::ManagerConflict => e,
            _ => stale_interface(),
        })?;
        let same_source = interface.source.is_none_or(|s| adapter.ipv4.contains(&s));
        let same_adapter = interface.index == Some(adapter.if_index)
            && interface.incarnation == Some(adapter.luid);
        if same_adapter && same_source {
            Ok(adapter)
        } else {
            Err(stale_interface())
        }
    }

    /// Checks the unconstrained best route to `target` against the tunnel's
    /// current adapter and returns the evidence, with the source to send from.
    ///
    /// A target that is one of this host's own addresses is never evidence:
    /// Windows keeps a host route for every local address on the interface
    /// that holds it, so the best route to the tunnel's own address is the
    /// tunnel, from that same address, and the local stack answers the echo
    /// without anything crossing the tunnel. Such a target is refused before
    /// any route lookup. That also covers a route whose source is the target
    /// itself: the source must be one of the tunnel's addresses in the same
    /// listing, so it is refused here or by the source check below.
    fn verify_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        let adapters = self.adapters()?;
        let adapter = Self::current(&adapters, interface)?;
        let destination = target.get();
        // Every adapter counts, whatever its type or whether it is up.
        if adapters.iter().any(|a| a.ipv4.contains(&destination)) {
            return Err(route_mismatch(
                "the target is one of this host's own addresses; Windows answers it locally, \
                 not through the tunnel",
            ));
        }
        let route = self.net_call(self.limits.query, "the route lookup", move |n| {
            n.best_route(destination)
        })?;
        if route.if_index != adapter.if_index || route.luid != adapter.luid {
            return Err(route_mismatch(
                "the target is routed through another interface",
            ));
        }
        if !adapter.ipv4.contains(&route.source) {
            return Err(route_mismatch(
                "the route's source address is not the tunnel's",
            ));
        }
        if interface.source.is_some_and(|s| s != route.source) {
            return Err(route_mismatch(
                "the route uses a different tunnel address than resolved",
            ));
        }
        let shared = adapters
            .iter()
            .any(|a| a.if_index != adapter.if_index && a.ipv4.contains(&route.source));
        if shared {
            return Err(route_mismatch(
                "the tunnel's source address is also on another interface",
            ));
        }
        Ok(RouteEvidence {
            target,
            interface_name: adapter.friendly_name.clone(),
            interface_index: Some(adapter.if_index),
            source: Some(route.source),
        })
    }

    /// Starts or stops one tunnel service under the rules in the module docs.
    fn control(
        &self,
        tunnel: &TunnelBinding,
        what: &str,
        control: Control,
    ) -> Result<(), AdapterError> {
        let service = TunnelName::from_binding(tunnel)?.service_name();
        let lane = self.lanes.get(&service);
        // Before the ticket: a request that cannot start takes no place in
        // the service's order, so it replaces no earlier one.
        let claim = self.command_claim(&lane).ok_or_else(|| {
            AdapterError::new(
                AdapterErrorKind::Timeout,
                &format!(
                    "{what} was not sent: too many earlier calls into Windows for tunnel \
                     services are still running; ask again"
                ),
            )
        })?;
        let ticket = lane.ticket();
        // Taken before the worker starts, so the gate closes no later than
        // the caller stops waiting.
        let gate = Arc::new(Gate::new(Instant::now().checked_add(self.limits.control)));
        let worker_gate = Arc::clone(&gate);
        let scm = Arc::clone(&self.scm);
        let poll = self.limits.settle_poll;
        let answer = bounded(claim, self.limits.control, what, move || {
            let state = || scm.query(&service);
            // A refused request changed nothing, so sending it again is safe;
            // the lane sends it only while it is still wanted and newest.
            let (refusal, judge): (_, fn(ServiceState) -> Refused) = match control {
                Control::Start => (ScmError::SERVICE_ALREADY_RUNNING, start_refused),
                Control::Stop => (ScmError::SERVICE_CANNOT_ACCEPT_CTRL, stop_refused),
            };
            let settle = Settle {
                refusal,
                state: &state,
                judge,
                poll,
            };
            lane.run(
                ticket,
                &worker_gate,
                || scm.open(&service, control),
                |handle| scm.send(handle, control),
                &settle,
            )
        });
        match answer {
            Ok(Ok(())) => Ok(()),
            // Stopping a tunnel that is already stopped; the core confirms the
            // state through query_state either way. A start that found the
            // tunnel already running or starting returned Ok above.
            Ok(Err(Unsent::Scm(error)))
                if control == Control::Stop && error == ScmError::SERVICE_NOT_ACTIVE =>
            {
                Ok(())
            }
            Ok(Err(Unsent::Scm(error))) => Err(error.to_adapter_error()),
            Ok(Err(Unsent::Replaced)) => Err(AdapterError::new(
                AdapterErrorKind::ManagerConflict,
                &format!("{what}: a newer request for this tunnel replaced it; it was not sent"),
            )),
            // The worker saw the deadline pass just before the caller did.
            Ok(Err(Unsent::Withdrawn)) => Err(Self::timed_out(&gate, what)),
            Err(error) if error.kind == AdapterErrorKind::Timeout => {
                Err(Self::timed_out(&gate, what))
            }
            Err(error) => Err(error),
        }
    }

    /// Withdraws a request whose deadline has passed and says what it left.
    fn timed_out(gate: &Gate, what: &str) -> AdapterError {
        let detail = match gate.withdraw() {
            Withdrawal::Sent => format!(
                "{what} was sent but did not finish in time; it may still take effect and is \
                 not sent again"
            ),
            Withdrawal::NotSent => {
                format!("{what} did not get through in time and was withdrawn; it was not sent")
            }
            Withdrawal::Deferred => format!(
                "{what} was not sent: the service was still changing state when the deadline \
                 passed; ask again"
            ),
        };
        AdapterError::new(AdapterErrorKind::Timeout, &detail)
    }

    /// `result`, a probe that got no reply or found no route, unless this
    /// host itself is offline: then `LocalNetworkOffline` instead.
    fn unless_offline(
        &self,
        result: Result<ProbeOutcome, AdapterError>,
    ) -> Result<ProbeOutcome, AdapterError> {
        match self.local_network() {
            Ok(LocalNetwork::Offline(why)) => Err(AdapterError::new(
                AdapterErrorKind::LocalNetworkOffline,
                why.detail(),
            )),
            Ok(LocalNetwork::Online) => result,
            // The check could not start what would have decided it: the
            // probe is broken, not the target.
            Err(error) => Err(error),
        }
    }

    /// The verdict on the host's own network: the last one if it is younger
    /// than [`Limits::local_verdict`], otherwise a new check. The lock is
    /// held during the check, so callers that fail together share it. A
    /// check that could not start what would have decided it reaches no
    /// verdict, and none is kept.
    fn local_network(&self) -> Result<LocalNetwork, AdapterError> {
        let mut last = lock(&self.local);
        if let Some((at, verdict)) = *last
            && at.elapsed() < self.limits.local_verdict
        {
            return Ok(verdict);
        }
        let verdict = self.check_local_network()?;
        *last = Some((Instant::now(), verdict));
        Ok(verdict)
    }

    /// Reads the adapters with both families' addresses and, if one of
    /// them is an uplink, the default routes, and asks the gateways those
    /// name. Whatever cannot be read counts as online. A read, or gateways,
    /// that could not be started for lack of helper threads where they
    /// would have decided is no verdict: `Timeout`.
    fn check_local_network(&self) -> Result<LocalNetwork, AdapterError> {
        let what = "listing network adapters";
        let adapters = match self.try_net_call(self.limits.query, what, |n| n.adapters_any_family())
        {
            Ok(Ok(adapters)) => adapters,
            Ok(Err(_)) => return Ok(LocalNetwork::Online),
            Err(Busy(error)) => return Err(error),
        };
        if adapters.len() > MAX_ADAPTERS {
            return Ok(LocalNetwork::Online);
        }
        let routes = if adapters.iter().any(local::is_uplink) {
            let what = "reading the routing table";
            let routes = match self.try_net_call(self.limits.query, what, |n| n.default_routes()) {
                Ok(Ok(routes)) => routes,
                Ok(Err(_)) => return Ok(LocalNetwork::Online),
                Err(Busy(error)) => return Err(error),
            };
            if routes.len() > local::MAX_DEFAULT_ROUTES {
                return Ok(LocalNetwork::Online);
            }
            routes
        } else {
            Vec::new()
        };
        match local::way_out(&adapters, &routes) {
            WayOut::None => Ok(LocalNetwork::Offline(Offline::NoUplink)),
            WayOut::Unchecked => Ok(LocalNetwork::Online),
            WayOut::Gateways(gateways) => {
                let asked = self.ask_gateways(&gateways);
                // Every gateway asked was silent, so those not asked would
                // have decided.
                if asked.unasked > 0 && asked.answers.iter().all(|r| *r == Resolution::Silent) {
                    return Err(busy("checking the default gateways"));
                }
                Ok(local::judge_gateways(&asked.answers))
            }
        }
    }

    /// Asks every gateway at once, each on its own thread from the read
    /// share, and waits at most [`Limits::neighbor`] for all of them; stops
    /// waiting as soon as one answers. A gateway still asking at the
    /// deadline is silent; one whose thread could not start or ended without
    /// an answer is unknown, and so is one not asked because the read share
    /// is full, which is counted apart.
    fn ask_gateways(&self, gateways: &[Gateway]) -> Asked {
        let deadline = Instant::now() + self.limits.neighbor;
        let (tx, rx) = mpsc::channel();
        let mut answers = Vec::with_capacity(gateways.len());
        let mut unasked = 0;
        let mut asking = 0;
        for &gateway in gateways {
            let Ok(claim) = self.read_claim("asking a default gateway") else {
                unasked += 1;
                continue;
            };
            let net = Arc::clone(&self.net);
            let tx = tx.clone();
            let spawned = thread::Builder::new()
                .name("wgal-windows-gateway".to_owned())
                .spawn(move || {
                    let answer = ask_gateway(&*net, gateway);
                    // As in `bounded`: given back once the calls into
                    // Windows have returned, before the answer goes.
                    drop(claim);
                    // The receiver is gone only if the caller stopped
                    // waiting; the channel is unbounded, so this never
                    // blocks.
                    let _ = tx.send(answer);
                });
            match spawned {
                Ok(_) => asking += 1,
                Err(_) => answers.push(Resolution::Unknown),
            }
        }
        drop(tx);
        for _ in 0..asking {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Resolution::Answered) => {
                    return Asked {
                        answers: vec![Resolution::Answered],
                        unasked,
                    };
                }
                Ok(other) => answers.push(other),
                Err(RecvTimeoutError::Timeout) => answers.push(Resolution::Silent),
                Err(RecvTimeoutError::Disconnected) => answers.push(Resolution::Unknown),
            }
        }
        Asked { answers, unasked }
    }
}

/// What asking the default gateways found: the answers of those asked, and
/// how many were not asked because the read share was full.
#[derive(Debug)]
struct Asked {
    answers: Vec<Resolution>,
    unasked: usize,
}

/// Asks one gateway for its link-layer address, unless its neighbor entry
/// is static: resolving would replace that entry, so the gateway is then
/// left unchecked.
fn ask_gateway<N: IpHelper + ?Sized>(net: &N, gateway: Gateway) -> Resolution {
    match net.neighbor_is_static(gateway.luid, gateway.address) {
        Ok(false) => {}
        Ok(true) | Err(_) => return Resolution::Unknown,
    }
    match net.resolve_neighbor(gateway.luid, gateway.address) {
        Ok(true) => Resolution::Answered,
        Ok(false) => Resolution::Silent,
        Err(_) => Resolution::Unknown,
    }
}

impl<S, N> PlatformAdapter for WindowsAdapter<S, N>
where
    S: ServiceControl + 'static,
    N: IpHelper + 'static,
{
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            // W3 on device, owner's decision (see the module documentation).
            bound_icmp_probe: Capability::Supported,
            bound_tcp_probe: Capability::Unsupported,
            read_metrics: Capability::Unsupported,
            ..AdapterCapabilities::UNKNOWN
        }
    }

    /// Every `WireGuardTunnel$<name>` service with a valid tunnel name,
    /// sorted. No service is opened; services with other names, including
    /// `WireGuardManager`, are never returned.
    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        let what = "listing services";
        let claim = self.read_claim(what).map_err(|Busy(error)| error)?;
        let names = self
            .scm_call(claim, self.limits.query, what, |scm| scm.service_names())?
            .map_err(ScmError::to_adapter_error)?;
        if names.len() > MAX_SERVICES {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "Windows reported more services than WGAL handles",
            ));
        }
        let tunnels: BTreeSet<TunnelName> = names
            .iter()
            .filter_map(|n| TunnelName::from_service_name(n))
            .collect();
        Ok(tunnels
            .into_iter()
            .map(|name| DiscoveredBinding {
                binding: name.binding(),
                display_name: DisplayName::lossy(name.as_str()),
                control: Capability::Supported,
            })
            .collect())
    }

    /// Validates the tunnel name and touches nothing else. WireGuard for
    /// Windows keeps each tunnel's configuration encrypted in its own data
    /// directory, readable only by SYSTEM and Administrators, runs the tunnel
    /// service as LocalSystem from the installed manager, and runs no
    /// configuration scripts unless an administrator enables them in HKLM.
    /// Under the default service ACL an unprivileged user can neither edit
    /// the service nor point it elsewhere, and WGAL never edits either, so
    /// there is no user-writable code or configuration for the watchdog to
    /// refuse here. On-device check W2 records the service ACL and binary
    /// path that this relies on.
    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        TunnelName::from_binding(tunnel).map(|_| ())
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        let service = TunnelName::from_binding(tunnel)?.service_name();
        let what = "querying the tunnel service";
        let claim = self
            .query_claim(&self.lanes.get(&service))
            .ok_or_else(|| busy(what))?;
        self.scm_call(claim, self.limits.query, what, move |scm| {
            scm.query(&service)
        })?
        .map_err(ScmError::to_adapter_error)?
        .os_state()
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        let name = TunnelName::from_binding(tunnel)?;
        let adapters = self.adapters()?;
        let adapter = find_adapter(&adapters, &name)?;
        if adapter.ipv4.len() > MAX_ADDRESSES {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "the tunnel's adapter has more addresses than WGAL handles",
            ));
        }
        let source = match adapter.ipv4.as_slice() {
            [only] => Some(*only),
            _ => None,
        };
        Ok(ResolvedInterface {
            name: adapter.friendly_name.clone(),
            index: Some(adapter.if_index),
            source,
            incarnation: Some(adapter.luid),
        })
    }

    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        self.verify_route(interface, target)
    }

    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        if request.target.probe != ProbeKind::Icmp {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "interface-bound TCP probes are not available on Windows yet",
            ));
        }
        let target = request.target.address;
        let evidence = self.verify_route(interface, target)?;
        let source = evidence
            .source
            .ok_or_else(|| route_mismatch("the route lookup did not report a source address"))?;
        let destination: Ipv4Addr = target.get();
        let timeout = request
            .timeout
            .clamp(Duration::from_millis(1), self.limits.max_echo);
        let net = Arc::clone(&self.net);
        let what = "the ICMP echo";
        let claim = self.read_claim(what).map_err(|Busy(error)| error)?;
        let sent = bounded(claim, timeout + self.limits.echo_grace, what, move || {
            net.echo(source, destination, timeout)
        })?;
        let result = match sent {
            Ok(reply) => {
                // An echo that straddled a reconnect says nothing about the
                // new interface; discard it.
                Self::current(&self.adapters()?, interface)?;
                classify_echo(&reply, destination)
            }
            Err(error) => Err(error.to_adapter_error(what)),
        };
        match result {
            // No reply, or no route to send the echo: the tunnel may be
            // broken, or this host may have no network at all (D-3).
            Ok(ProbeOutcome::NoReply) => self.unless_offline(Ok(ProbeOutcome::NoReply)),
            Err(error) if error.kind == AdapterErrorKind::RouteMismatch => {
                self.unless_offline(Err(error))
            }
            other => other,
        }
    }

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(tunnel, "starting the tunnel service", Control::Start)
    }

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(tunnel, "stopping the tunnel service", Control::Stop)
    }
}

#[cfg(test)]
mod capacity_tests;
#[cfg(test)]
mod fake;
#[cfg(all(test, windows))]
mod live;
#[cfg(test)]
mod local_tests;
#[cfg(test)]
mod tests;
