//! The watchdog: every registered tunnel, the host-wide control slot,
//! incidents, persistence, user requests and snapshots.
//!
//! It is driven from outside: [`Watchdog::tick`] plans the platform work that
//! is due, runs it and applies the results, all at the clock's current time.
//! Nothing here sleeps, so tests drive it step by step with a fake clock and a
//! fake platform, and the runtime calls `tick` on a timer.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, BoundedVec, Capability, DesiredState, DiscoveredTunnel,
    DisplayName, ErrorCode, ErrorReason, FirstRecovery, HostSettings, HostStatus, IncidentId,
    IncidentStatus, Light, LogEvent, MAX_DISCOVERED, MAX_TUNNELS, MIN_WORKERS, MonitorSettings,
    NoticeKind, NotificationState, NotificationStatus, OsState, Outcome, ProbeTiming,
    ProductVersion, ProtocolError, RecoveredNotification, RecoveryRetry, Request, Response,
    Snapshot, StorageHealth, Takeover, TimeZoneStatus, TunnelId, TunnelStatus, UserIntent,
    WorkerStatus,
};

use crate::clock::{Clock, MonoInstant};
use crate::platform::{
    AdapterError, DiscoveredBinding, PlatformAdapter, ProbeOutcome, TunnelBinding,
};
use crate::record::{
    External, IncidentRecord, MAX_RECOVERED_NOTICES, NoticeRecord, PendingAutoRecovery,
    PersistedState, RecoveredNotice, ResumeOp, SCHEMA_VERSION, StateStore, StoreError,
    TunnelRecord,
};
use crate::registry::candidate_id;
use crate::timing::Timing;
use crate::tunnel::{Activity, Block, Op, Path, Tunnel, can_bind, summary};
use crate::work::{
    Executor, ProbeSet, TargetRole, WorkItem, WorkKind, WorkOutcome, WorkResult, execute,
};

/// Upper bound on plan/execute/apply passes in one [`Watchdog::tick`]. Work
/// that becomes due at the same instant (a poll right after a stop, a
/// verification right after a start) runs in the same tick; this only guards
/// against a planning bug turning into a busy loop.
const MAX_PASSES_PER_TICK: usize = 64;

/// How far incident numbering jumps after restoring from a backup, so an ID
/// the lost newer state may already have used is not handed out again.
const BACKUP_INCIDENT_GAP: u64 = 1_000;

/// Most events kept for [`Watchdog::take_events`]; older ones are dropped
/// and counted.
pub const MAX_EVENTS: usize = 1024;

/// What the watchdog starts from.
#[derive(Debug)]
pub enum Startup {
    /// No state was stored yet.
    Empty,
    /// State loaded from the store (already migrated to [`SCHEMA_VERSION`]).
    Restored(PersistedState),
    /// The newest stored state was damaged and an older copy was loaded. What
    /// the lost state recorded after it is unknown: a notification may already
    /// have been reserved or sent, and the user may have disconnected since.
    /// The watchdog restores it conservatively (see [`Watchdog::new`]).
    RestoredFromBackup(PersistedState),
    /// Stored state exists but could not be loaded. The watchdog never
    /// overwrites it, reports storage as failed, and takes no automatic action.
    StorageFailed,
}

/// A notification the notification package may send now (plan §7.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueNotification {
    /// Which of the incident's messages is due.
    pub kind: NoticeKind,
    pub tunnel_id: TunnelId,
    pub incident_id: IncidentId,
    pub display_name: DisplayName,
    pub opened_at_unix_ms: i64,
    pub first_recovery: FirstRecovery,
    /// The tunnel's retry and how far the incident got, for the alert's
    /// `{retry}` and the `retries_exhausted` message's `{retries}`.
    pub retry: RecoveryRetry,
    pub retries_used: u32,
    pub retries_exhausted: bool,
    /// When traffic came back, for the `recovered` message.
    pub recovered_at_unix_ms: Option<i64>,
}

/// Proof that a notification was reserved, to be handed back with the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationTicket {
    pub tunnel_id: TunnelId,
    pub incident_id: IncidentId,
    pub kind: NoticeKind,
}

/// What became of a reserved notification (plan §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationOutcome {
    /// Telegram accepted it.
    Sent { message_id: i64 },
    /// Provably never left the host (failed before the connection was made),
    /// or the API refused it in a way that allows a later retry (such as 429).
    RetryLater,
    /// Token, chat or permission rejected; do not retry.
    Rejected,
    /// It may have been delivered; never send it again automatically.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationError {
    NoSuchIncident,
    /// The incident's notification is not in the state this call needs.
    WrongState(NotificationState),
    /// The reservation could not be persisted, so nothing may be sent.
    Storage(StoreError),
}

/// A refused request: its code, the reason the GUI shows a sentence for, and
/// English text for the details. Every rejection of the core has a reason.
type Rejection = (ErrorCode, ErrorReason, &'static str);

/// Everything a transition needs besides the tunnel.
struct Ctx<'a> {
    now: MonoInstant,
    wall: i64,
    timing: &'a Timing,
    caps: AdapterCapabilities,
    /// The tunnel's own probe timing.
    probe: ProbeTiming,
    /// The tunnel's freshness (`Tunnel::freshness`), which also counts its
    /// observe targets.
    freshness: Duration,
}

impl Ctx<'_> {
    fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.probe.interval_secs()))
    }

    fn freshness(&self) -> Duration {
        self.freshness
    }
}

/// The event-log label of a tunnel that was unregistered, kept until the
/// runtime has looked it up (see [`Watchdog::log_label`]).
#[derive(Debug)]
struct Retired {
    tunnel_id: TunnelId,
    label: String,
    /// Its `tunnel_removed` event was handed over by `take_events`.
    handed_over: bool,
}

pub struct Watchdog<A, C> {
    /// Shared with [`Executor`]s, which run planned work on other threads.
    adapter: Arc<A>,
    clock: Arc<C>,
    timing: Timing,
    store: Box<dyn StateStore>,
    storage_ok: bool,
    /// Loading failed: never write, so the stored state survives for repair.
    read_only: bool,
    installation_id: String,
    next_incident: u64,
    tunnels: BTreeMap<TunnelId, Tunnel>,
    next_slot: u32,
    /// Tunnels waiting for, or holding (the front), the host control slot.
    control_queue: VecDeque<TunnelId>,
    discovered: BTreeMap<TunnelId, DiscoveredBinding>,
    /// When to try saving again after a save failed.
    storage_retry_at: Option<MonoInstant>,
    monitoring_paused: bool,
    /// Agents may use WGAL through `wgal-mcp`.
    mcp_enabled: bool,
    /// They may also read the chat ID and change the device name, chat ID
    /// and bot token; the runtime enforces it.
    mcp_telegram_enabled: bool,
    /// Stored for the runtime, never interpreted here (see
    /// [`Watchdog::set_host_settings`]).
    host: HostSettings,
    /// The runtime is shutting down: no new automatic action (see
    /// [`Watchdog::hold_automatic_actions`]).
    automatic_held: bool,
    /// What the core decided, for the event log, oldest first, until the
    /// runtime takes them (see [`Watchdog::take_events`]); at most
    /// [`MAX_EVENTS`].
    events: VecDeque<(Option<TunnelId>, LogEvent)>,
    /// Events dropped from `events` since the runtime last took them.
    events_lost: u32,
    /// The host-wide line for an outage of the host's own network has been
    /// written and its end has not (see [`Self::note_host_network`]).
    host_offline_noted: bool,
    retired: Vec<Retired>,
    /// Sequence numbers of planned items, unique across every tunnel this
    /// watchdog has had, so a result planned for a tunnel that was
    /// unregistered can never be taken for one of a tunnel registered again
    /// under the same ID.
    next_seq: u64,
    /// Saves are held back while [`Watchdog::all_or_nothing`] runs; `held`
    /// says one was asked for meanwhile.
    saves_held: bool,
    held: bool,
    /// The clock pair snapshots write wall times with, kept from one
    /// snapshot to the next (see [`Watchdog::shown_clock`]).
    shown_clock: Option<(MonoInstant, i64)>,
}

impl<A, C> fmt::Debug for Watchdog<A, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watchdog")
            .field("storage_ok", &self.storage_ok)
            .field("read_only", &self.read_only)
            .field("tunnels", &self.tunnels.len())
            .field("control_queue", &self.control_queue)
            .finish_non_exhaustive()
    }
}

fn later(at: MonoInstant, by: Duration) -> MonoInstant {
    at.checked_add(by).unwrap_or(at)
}

/// A stop or start may take effect until at least `until`.
fn may_land(slot: &mut Option<MonoInstant>, until: MonoInstant) {
    *slot = Some(slot.map_or(until, |at| at.max(until)));
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// How far the wall clock may move from the pair a snapshot keeps before a
/// new pair is taken (see [`Watchdog::shown_clock`]).
const SHOWN_CLOCK_TOLERANCE_MS: u64 = 100;

/// Wall time of a monotonic instant, given the current pair.
fn wall_at(now: MonoInstant, wall: i64, at: MonoInstant) -> i64 {
    if at >= now {
        wall.saturating_add(millis(at.saturating_duration_since(now)))
    } else {
        wall.saturating_sub(millis(now.saturating_duration_since(at)))
    }
}

fn timeout(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::Timeout, detail)
}

impl<A: PlatformAdapter, C: Clock> Watchdog<A, C> {
    /// Starts the watchdog from what the store loaded.
    ///
    /// From [`Startup::RestoredFromBackup`] nothing automatic resumes on state
    /// that may be out of date: auto recovery and "start when stopped
    /// externally" are turned off, unfinished operations are dropped, and every
    /// notification that is not settled becomes `unknown`, so it is never sent
    /// (it may have been). The user's later choices re-establish intent.
    pub fn new(
        adapter: A,
        clock: C,
        timing: Timing,
        store: Box<dyn StateStore>,
        startup: Startup,
    ) -> Self {
        let now = clock.now();
        let wall = clock.wall_unix_ms();
        let mut watchdog = Self {
            adapter: Arc::new(adapter),
            clock: Arc::new(clock),
            timing,
            store,
            storage_ok: true,
            read_only: false,
            installation_id: format!("wgal-{:x}", wall.unsigned_abs()),
            next_incident: 1,
            tunnels: BTreeMap::new(),
            next_slot: 0,
            control_queue: VecDeque::new(),
            discovered: BTreeMap::new(),
            storage_retry_at: None,
            monitoring_paused: false,
            mcp_enabled: false,
            mcp_telegram_enabled: false,
            host: HostSettings::default(),
            automatic_held: false,
            events: VecDeque::new(),
            events_lost: 0,
            host_offline_noted: false,
            retired: Vec::new(),
            next_seq: 0,
            saves_held: false,
            held: false,
            shown_clock: None,
        };
        let mut changed = false;
        match startup {
            Startup::Empty => {}
            Startup::StorageFailed => {
                watchdog.storage_ok = false;
                watchdog.read_only = true;
            }
            Startup::Restored(state) | Startup::RestoredFromBackup(state)
                if state.schema_version != SCHEMA_VERSION =>
            {
                watchdog.storage_ok = false;
                watchdog.read_only = true;
                return watchdog;
            }
            Startup::RestoredFromBackup(mut state) => {
                for rec in &mut state.tunnels {
                    distrust(rec);
                }
                state.next_incident = state.next_incident.saturating_add(BACKUP_INCIDENT_GAP);
                changed = true;
                watchdog.restore(state, now, wall);
            }
            Startup::Restored(state) => {
                changed = watchdog.restore(state, now, wall);
            }
        }
        if changed {
            // Failure is recorded in `storage_ok`.
            let _ = watchdog.persist();
        }
        watchdog.refresh_lights();
        watchdog
    }

    /// Loads stored tunnels. Returns whether the state needs saving again.
    fn restore(&mut self, state: PersistedState, now: MonoInstant, wall: i64) -> bool {
        let mut changed = false;
        self.installation_id = state.installation_id;
        self.next_incident = state.next_incident;
        self.monitoring_paused = state.monitoring_paused;
        self.mcp_enabled = state.mcp_enabled;
        self.mcp_telegram_enabled = state.mcp_telegram_enabled;
        self.host = state.host;
        for mut rec in state.tunnels {
            let Ok(binding) = TunnelBinding::new(rec.binding.clone()) else {
                // A record that does not validate means the file was edited or
                // damaged; keep it on disk untouched.
                self.storage_ok = false;
                self.read_only = true;
                continue;
            };
            if let Some(incident) = &mut rec.incident
                && incident.notification == NotificationState::Reserved
            {
                // It may have been sent before the restart (plan §7.3).
                incident.notification = NotificationState::Unknown;
                incident.cancel_requested = false;
                changed = true;
            }
            // So may the messages added in 1.2.0.
            let later_notices = rec
                .incident
                .as_mut()
                .map(|incident| &mut incident.exhausted_notice)
                .into_iter()
                .chain(rec.recovered_notices.iter_mut().map(|n| &mut n.notice));
            for notice in later_notices {
                if notice.state == NotificationState::Reserved {
                    notice.state = NotificationState::Unknown;
                    notice.cancel_requested = false;
                    changed = true;
                }
            }
            // When the incident's current wait started, by the wall clock:
            // the restored backoff and every later wait are measured from
            // there (see `Tunnel::restored_end`).
            let interval = retry_wait(rec.retry);
            let restored_end = rec.incident.as_ref().and_then(|incident| {
                let since = |from: i64| {
                    let ago = wall.saturating_sub(from).max(0);
                    (now, Duration::from_millis(ago.unsigned_abs()))
                };
                if let Some(from) = incident.wait_from_unix_ms {
                    return Some(since(from));
                }
                // A state from before the start of the wait was stored (1.1.0):
                // a stored retry time says what is left, never more than one
                // interval, as it did then (Codex P2-N61); without one, the end
                // of the last round stands in.
                if let Some(due) = incident.next_retry_unix_ms {
                    let left = due.saturating_sub(wall).clamp(0, millis(interval));
                    let left = Duration::from_millis(left.unsigned_abs());
                    return Some((now, interval.saturating_sub(left)));
                }
                incident.last_round_end_unix_ms.map(since)
            });
            let restored_retry = rec.incident.as_ref().and_then(|incident| {
                let due = incident.next_retry_unix_ms?;
                if let Some((anchor, ago)) = restored_end {
                    return Some(later(anchor, interval.saturating_sub(ago)));
                }
                // A state with no start of its wait: what the retry time
                // says is left, never more than one interval.
                let remaining = due.saturating_sub(wall).clamp(0, millis(interval));
                let remaining = Duration::from_millis(remaining.unsigned_abs());
                Some(later(now, remaining))
            });
            // A record kept until a deadline keeps that deadline; a restart
            // never grants a new one. A clock set back cannot stretch it past
            // one window (review of `2694ab3`).
            let kept = rec
                .resume_until_unix_ms
                .take()
                .filter(|_| rec.resume.is_some())
                .map(|due| {
                    due.saturating_sub(wall)
                        .min(millis(self.timing.late_effect()))
                });
            let resume_until = kept
                .filter(|remaining| *remaining > 0)
                .map(|remaining| later(now, Duration::from_millis(remaining.unsigned_abs())));
            if kept.is_some() && resume_until.is_none() {
                rec.resume = None;
                changed = true;
            }
            let id = rec.tunnel_id.clone();
            let mut tunnel = self.new_tunnel(rec, binding, now);
            tunnel.restored_end = restored_end;
            if let Some(retry_at) = restored_retry {
                tunnel.activity = Activity::Backoff { retry_at };
            }
            tunnel.resume_until = resume_until;
            // The previous process may have left a start that has not landed
            // yet; until it can no longer land, a tunnel that looks down is
            // not known to be down (follow-up review 2, finding 1). A confirmed
            // disconnect's record says exactly until when that is.
            tunnel.late_start_until = Some(match (tunnel.rec.resume, resume_until) {
                (Some(ResumeOp::Disconnect), Some(until)) => until,
                _ => later(now, self.timing.late_effect()),
            });
            self.tunnels.insert(id, tunnel);
        }
        changed | self.label_tunnels()
    }

    /// Gives every tunnel without one its event-log label. Returns whether
    /// any was added, which then has to be saved.
    fn label_tunnels(&mut self) -> bool {
        let mut taken: BTreeSet<String> = self
            .tunnels
            .values()
            .filter_map(|t| t.rec.log_label.clone())
            .chain(self.retired.iter().map(|r| r.label.clone()))
            .collect();
        let mut changed = false;
        for t in self.tunnels.values_mut() {
            if t.rec.log_label.is_none() {
                let label = fresh_label(&taken);
                taken.insert(label.clone());
                t.rec.log_label = Some(label);
                changed = true;
            }
        }
        changed
    }

    fn new_tunnel(
        &mut self,
        rec: TunnelRecord,
        binding: TunnelBinding,
        now: MonoInstant,
    ) -> Tunnel {
        let slot = self.next_slot;
        self.next_slot = slot.wrapping_add(1);
        // Successive tunnels start their rounds `stagger` apart, wrapped
        // within each one's own interval, so tunnels with different
        // intervals still do not all probe at once at startup.
        let interval = u128::from(rec.probe.interval_secs()) * 1_000_000_000;
        let offset = self.timing.stagger.saturating_mul(slot);
        let offset =
            Duration::from_nanos(u64::try_from(offset.as_nanos() % interval.max(1)).unwrap_or(0));
        let mut tunnel = Tunnel::new(
            rec,
            binding,
            now,
            later(now, self.timing.grace),
            later(now, offset),
        );
        tunnel.paused = self.monitoring_paused;
        tunnel
    }

    pub fn adapter(&self) -> &A {
        &self.adapter
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Runs this watchdog's planned items on other threads (see
    /// [`Self::plan`]).
    pub fn executor(&self) -> Executor<A, C> {
        Executor::new(Arc::clone(&self.adapter), Arc::clone(&self.clock))
    }

    /// The installation's ID, stored with the state.
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    /// The manager's name for a tunnel the last discovery listed under this
    /// ID, which [`Request::RegisterTunnel`] would register. For the
    /// runtime's authorization (plan §9.2).
    pub fn discovered_binding(&self, id: &TunnelId) -> Option<&TunnelBinding> {
        self.discovered.get(id).map(|found| &found.binding)
    }

    /// How many tunnels are registered. Each has at most one round out at a
    /// time, so a runtime that runs rounds side by side needs no more round
    /// workers than this.
    pub fn tunnel_count(&self) -> usize {
        self.tunnels.len()
    }

    /// How many tunnels are monitored: registered with their own monitoring
    /// on. None while monitoring is paused host-wide. Only these have
    /// rounds planned; a paused tunnel costs no worker.
    pub fn monitored_count(&self) -> usize {
        self.tunnels.values().filter(|t| t.monitoring()).count()
    }

    /// For each tunnel [`Self::monitored_count`] counts, its probe timing and
    /// how many targets one of its monitoring rounds probes now: none while
    /// the user wants it down, something else controls it, or the manager no
    /// longer has it (such a round only reads the OS state, or is not
    /// planned at all). The runtime computes its worker suggestion from
    /// this; the core does not.
    pub fn monitoring_load(&self) -> Vec<(ProbeTiming, usize)> {
        self.tunnels
            .values()
            .filter(|t| t.monitoring())
            .map(|t| (t.rec.probe, t.round_targets()))
            .collect()
    }

    /// The stored host settings: worker threads and time zone.
    pub fn host_settings(&self) -> &HostSettings {
        &self.host
    }

    /// Replaces the host settings and saves them before it returns. If the
    /// save fails, the old settings stay and the error is returned. The
    /// runtime checks and applies them (`Request::UpdateHostSettings`); the
    /// core only keeps them with the rest of the state.
    pub fn set_host_settings(&mut self, settings: HostSettings) -> Result<(), StoreError> {
        if settings == self.host {
            return Ok(());
        }
        let before = std::mem::replace(&mut self.host, settings);
        if let Err(error) = self.persist() {
            self.host = before;
            return Err(error);
        }
        Ok(())
    }

    /// Hands over what the core decided since the last call, for the event
    /// log, oldest first: each event with the tunnel it is about (`None` for
    /// one about no single tunnel). User operations, host-wide changes,
    /// notifications and storage are the runtime's to log.
    ///
    /// At most [`MAX_EVENTS`] are kept between calls; when older ones had to
    /// be dropped, the list starts with `lines_lost` and how many.
    pub fn take_events(&mut self) -> Vec<(Option<TunnelId>, LogEvent)> {
        self.gather_events();
        // Labels of tunnels whose removal an earlier call handed over have
        // been looked up by now; the ones handed over now stay until the
        // next call.
        self.retired.retain(|r| !r.handed_over);
        for retired in &mut self.retired {
            retired.handed_over = true;
        }
        let mut events = Vec::with_capacity(self.events.len() + 1);
        if self.events_lost > 0 {
            events.push((
                None,
                LogEvent::LinesLost {
                    count: self.events_lost,
                },
            ));
            self.events_lost = 0;
        }
        events.extend(self.events.drain(..));
        events
    }

    /// Moves what each tunnel noted into the watchdog's event queue. Called
    /// at the end of every step that may note something, so events keep the
    /// order in which they were decided.
    fn gather_events(&mut self) {
        let mut back_online = false;
        for (id, t) in &mut self.tunnels {
            for event in t.events.drain(..) {
                back_online |= event == LogEvent::LocalNetworkOnline;
                queue_event(
                    &mut self.events,
                    &mut self.events_lost,
                    Some(id.clone()),
                    event,
                );
            }
        }
        self.note_host_network(back_online);
    }

    /// The host-wide lines for an outage of the host's own network, beside
    /// each tunnel's own: `local_network_offline` about no tunnel when the
    /// first monitored tunnel finds the host offline while none had, and
    /// `local_network_online` when the last one that had finds it back
    /// (`back_online`: a tunnel noted its end in this step). A tunnel that
    /// can no longer say while it had found the host offline (paused,
    /// monitoring off, its recovery target removed, unregistered) ends
    /// nothing it can vouch for: it leaves the outage (`Tunnel::host_vouch`),
    /// the host-wide outage is forgotten without a line once no tunnel is
    /// left, and the next one is written again; a resume does not bring the
    /// old one back (Codex P3-N15).
    fn note_host_network(&mut self, back_online: bool) {
        for t in self.tunnels.values_mut() {
            if t.host_vouch && !(t.monitoring() && t.rec.recovery_target.is_some()) {
                t.host_vouch = false;
            }
        }
        let offline = self.tunnels.values().any(|t| t.host_vouch);
        if offline && !self.host_offline_noted {
            self.host_offline_noted = true;
            queue_event(
                &mut self.events,
                &mut self.events_lost,
                None,
                LogEvent::LocalNetworkOffline,
            );
        } else if !offline && self.host_offline_noted {
            self.host_offline_noted = false;
            if back_online {
                queue_event(
                    &mut self.events,
                    &mut self.events_lost,
                    None,
                    LogEvent::LocalNetworkOnline,
                );
            }
        }
    }

    /// The manager's name for a registered tunnel.
    pub fn registered_binding(&self, id: &TunnelId) -> Option<&TunnelBinding> {
        self.tunnels.get(id).map(|t| &t.binding)
    }

    /// How the event log names a registered tunnel (see
    /// `TunnelRecord::log_label`): random, stored, never derived from its
    /// IDs or names.
    ///
    /// A tunnel that was unregistered keeps answering until the
    /// [`Self::take_events`] call after the one that handed over its
    /// `tunnel_removed` event, so the runtime can name it in the lines it
    /// writes right after taking the events. A tunnel registered again
    /// under the same ID meanwhile answers with its new label.
    pub fn log_label(&self, id: &TunnelId) -> Option<&str> {
        match self.tunnels.get(id) {
            Some(t) => t.rec.log_label.as_deref(),
            None => self
                .retired
                .iter()
                .rev()
                .find(|r| &r.tunnel_id == id)
                .map(|r| r.label.as_str()),
        }
    }

    /// Whether the tunnel is between a stop and the start of the same
    /// operation: stopping for an operation that ends by starting it (a
    /// recovery round, a reconnect), or about to send that start. A runtime
    /// that shuts down keeps polling and sends the start, so WGAL never leaves
    /// down a tunnel it stopped (review of wave two, P1-1).
    pub fn owes_start(&self, id: &TunnelId) -> bool {
        self.tunnels.get(id).is_some_and(|t| match t.activity {
            Activity::Stopping { op, .. } => op.starts(),
            Activity::Starting { issued, .. } => !issued,
            _ => false,
        })
    }

    /// Whether the tunnel is stopping for a user's disconnect. A runtime
    /// that shuts down still sends that stop: it carries out what the user
    /// asked for and takes nothing back (plan §6.4).
    pub fn stopping_for_disconnect(&self, id: &TunnelId) -> bool {
        self.tunnels.get(id).is_some_and(|t| {
            matches!(
                t.activity,
                Activity::Stopping {
                    op: Op::Disconnect,
                    ..
                }
            )
        })
    }

    /// Whether some user's disconnect still has its stop to send: waiting for
    /// the control slot, or holding it with the stop not handed to the
    /// platform yet. A disconnect waiting to retry a failed stop does not
    /// count; it is stored and the next watchdog retries it.
    pub fn disconnect_stop_unsent(&self) -> bool {
        self.tunnels.values().any(|t| {
            !t.rec.binding_lost
                && t.command_out.is_none()
                && matches!(
                    t.activity,
                    Activity::Queued {
                        op: Op::Disconnect,
                        ..
                    } | Activity::Stopping {
                        op: Op::Disconnect,
                        issued: false,
                        ..
                    }
                )
        })
    }

    /// Whether a stop of WGAL's that was not followed by its start may have
    /// left the tunnel down against what the user wants: the user wants it
    /// up, its binding is not lost, and it is not known to be up (seen
    /// connected, readable, with no stop of WGAL's that may still land). False
    /// when nothing is owed any more: the user disconnected it, the manager no
    /// longer has it, or it is up after all.
    pub fn may_be_left_down(&self, id: &TunnelId) -> bool {
        let now = self.clock.now();
        self.tunnels.get(id).is_some_and(|t| {
            t.rec.intent.desired == DesiredState::Connected
                && !t.rec.binding_lost
                && !t.known_up(now)
        })
    }

    /// For a runtime shutting down: from now on no automatic recovery round
    /// or start after an external stop is queued, and queued ones that have
    /// sent nothing are dropped, as while storage fails. One whose stop was
    /// already sent still finishes with its start, so WGAL never leaves down
    /// a tunnel it stopped. User requests work as before. Kept in memory
    /// only and never undone; a new watchdog starts without it.
    pub fn hold_automatic_actions(&mut self) {
        self.automatic_held = true;
    }

    /// Saves the state now, for a runtime shutting down. Every change is
    /// already saved as it is made, so this only matters after a save
    /// failed; it stores nothing new.
    pub fn save(&mut self) -> Result<(), StoreError> {
        self.persist()
    }

    /// The earliest time after now at which [`Self::plan`] may find work
    /// that is not due yet: a round, a poll during a stop or start, a retry
    /// of a user's disconnect, the end of a record kept for a late stop or
    /// start, or another try at saving. A runtime sleeps until then instead
    /// of planning on a fixed tick. It may be early, never late, and is
    /// never now or earlier: work that is due now is already returned by
    /// `plan`. `None` when nothing is scheduled.
    ///
    /// A runtime still plans after every request and every applied result,
    /// since those make work due at once. Planning more often is harmless.
    pub fn next_due(&self) -> Option<MonoInstant> {
        let now = self.clock.now();
        let mut next: Option<MonoInstant> = None;
        let mut consider = |at: Option<MonoInstant>| {
            if let Some(at) = at
                && at > now
            {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        };
        if !self.storage_ok && self.writable() {
            consider(self.storage_retry_at);
        }
        for t in self.tunnels.values() {
            consider(t.next_scheduled());
            // Each is acted on only in some states; waking for one that is
            // not is merely early.
            consider(t.retry_check_at());
            consider(t.stop_retry_at);
            consider(t.resume_until);
            consider(t.late_start_until);
        }
        next
    }

    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    pub fn storage_health(&self) -> StorageHealth {
        if self.storage_ok && !self.read_only {
            StorageHealth::Ok
        } else {
            StorageHealth::Failed
        }
    }

    fn writable(&self) -> bool {
        !self.read_only
    }

    /// The reason to give for a change refused because it could not be
    /// stored: `storage_load_failed` while the stored state could not be
    /// loaded, so nothing is written over it (every save is then refused),
    /// `storage_save_failed` otherwise. For the runtime's refusals of its own
    /// saves through the core, such as [`Self::set_host_settings`].
    pub fn storage_reason(&self) -> ErrorReason {
        if self.writable() {
            ErrorReason::StorageSaveFailed
        } else {
            ErrorReason::StorageLoadFailed
        }
    }

    /// Runs all platform work that is due now. Returns how many items ran.
    pub fn tick(&mut self) -> usize {
        let mut ran = 0;
        for _ in 0..MAX_PASSES_PER_TICK {
            let items = self.plan();
            if items.is_empty() {
                break;
            }
            for item in items {
                if !self.still_due(&item) {
                    continue;
                }
                let result = execute(&*self.adapter, &*self.clock, &item);
                self.apply(result);
                ran += 1;
            }
        }
        ran
    }

    /// Plans the work due now and marks it outstanding. A runtime that runs
    /// probes concurrently executes the items itself and hands each result to
    /// [`Watchdog::apply`]; control items ([`WorkItem::is_control`]) must be
    /// executed and applied before any other request for that tunnel, and
    /// every item must pass [`Watchdog::still_due`] right before it runs.
    pub fn plan(&mut self) -> Vec<WorkItem> {
        let now = self.clock.now();
        let mut expired = false;
        let mut settled_connects = Vec::new();
        for (id, t) in &mut self.tunnels {
            // A round still out is judged by when its reading was taken.
            if t.pending.is_none() {
                let connect = t.rec.resume == Some(ResumeOp::Connect);
                if expire_retained(t, now) {
                    expired = true;
                    if connect && t.rec.resume.is_none() {
                        settled_connects.push(id.clone());
                    }
                }
            }
        }
        if expired {
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        if self.storage_ok {
            self.storage_retry_at = None;
        } else if self.writable() && self.storage_retry_at.is_none_or(|at| now >= at) {
            // A save failed. Try again every probe interval even if nothing
            // changed since, so storage that works again ends the fail-closed
            // state on its own (plan §9.1; review of `121b42a`, P2-6).
            self.storage_retry_at = Some(later(now, self.timing.storage_retry));
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        if !self.storage_ok || !self.writable() || self.automatic_held {
            self.cancel_unstarted_automatic_ops(now);
        }
        let mut given_back = false;
        for t in self.tunnels.values_mut() {
            // Checked again before the slot is handed out: nothing automatic
            // starts on a tunnel that is not monitored (Codex P1-1).
            if !t.monitoring() {
                given_back |= withdraw_unmonitored(t, now);
            }
        }
        if given_back {
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        self.redrive_disconnects(now);
        let timing = &self.timing;
        for t in self.tunnels.values_mut() {
            if t.pending.is_none() && stalled(t, now, timing) {
                // The process or host stalled in the middle of an operation;
                // the time it was away is no evidence (plan §6.2).
                rebase_deadlines(t, now, timing);
            }
            let monitoring = matches!(t.activity, Activity::Idle | Activity::Backoff { .. });
            if monitoring
                && t.pending.is_none()
                && t.monitoring()
                && now > later(t.next_round, t.freshness())
            {
                // Rounds stopped for longer than a result stays fresh: the host
                // slept or the process stalled. Those missing samples are not
                // failures (plan §6.2), and the fault confirmed before is no
                // evidence for a retry now: it is confirmed afresh (Codex
                // P2-N27).
                t.reset_streak();
                t.grace_until = later(now, timing.grace);
                t.next_round = now;
            }
        }
        // A retry runs on its own interval, not when the next monitoring
        // round happens to come back (Codex P2-N17), and only on evidence
        // that is still fresh (Codex P2-N27).
        let retries: Vec<TunnelId> = self
            .tunnels
            .iter()
            .filter(|(_, t)| t.retry_check_at().is_some_and(|at| now >= at))
            .map(|(id, _)| id.clone())
            .collect();
        // A recovery held back while the user's connect was unsettled is
        // weighed as soon as the connect's record ends, not at the next
        // round, which may be an hour away (Codex P2-N55).
        for id in retries.iter().chain(&settled_connects) {
            self.consider_recovery(id);
        }
        self.grant_control_slot(now);
        let next_seq = &mut self.next_seq;
        let mut items = Vec::new();
        for (id, t) in &mut self.tunnels {
            if t.monitoring() {
                // Monitored again before the check asked for went out: its
                // rounds answer it, and it must not run once monitoring is
                // off again (Codex P2).
                t.one_off_at = None;
            }
            if let Some(kind) = t.due(now) {
                *next_seq = next_seq.wrapping_add(1);
                t.seq = *next_seq;
                t.pending = Some(t.seq);
                // The one round of a check asked for while not monitored; a
                // verification round after a start is not one.
                if !t.monitoring()
                    && matches!(kind, WorkKind::Round { .. })
                    && matches!(t.activity, Activity::Idle | Activity::Backoff { .. })
                {
                    t.one_off_at = None;
                    t.one_off_seq = Some(t.seq);
                }
                items.push(WorkItem {
                    tunnel_id: id.clone(),
                    binding: t.binding.clone(),
                    generation: t.rec.generation,
                    probe_generation: t.probe_generation,
                    seq: t.seq,
                    kind,
                });
            }
        }
        self.gather_events();
        items
    }

    /// Whether a planned item may still run. Applying an earlier item of the
    /// same batch can make a later one obsolete: when that apply could not be
    /// stored, automatic operations that have sent nothing yet are cancelled
    /// (plan §9.1, follow-up review F4). So is one of a tunnel whose
    /// monitoring was turned off, by its own switch or the host-wide pause,
    /// after the item was planned (Codex review of wave three, P1-1). An
    /// item that may not run is withdrawn, and the caller drops it without
    /// executing it.
    pub fn still_due(&mut self, item: &WorkItem) -> bool {
        let now = self.clock.now();
        // Withdrawing a stale round may save, and a save that fails stops
        // every automatic action not sent yet, this one included: so
        // storage is judged after it (Codex P2-N41).
        if item.is_control() {
            self.withdraw_stale_recovery(&item.tunnel_id, now);
        }
        if !self.storage_ok || !self.writable() || self.automatic_held {
            self.cancel_unstarted_automatic_ops(now);
        }
        let until = later(now, self.timing.late_effect());
        let Some(t) = self.tunnels.get_mut(&item.tunnel_id) else {
            return false;
        };
        if !t.monitoring() && withdraw_unmonitored(t, now) {
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        let Some(t) = self.tunnels.get_mut(&item.tunnel_id) else {
            return false;
        };
        if t.pending != Some(item.seq) {
            return false;
        }
        let due = item.generation == t.rec.generation
            && match item.kind {
                WorkKind::Stop => matches!(t.activity, Activity::Stopping { issued: false, .. }),
                WorkKind::Start => matches!(t.activity, Activity::Starting { issued: false, .. }),
                WorkKind::Poll | WorkKind::Round { .. } => true,
            };
        if !due {
            t.pending = None;
            return false;
        }
        // From here on the request may reach the platform, even if its result
        // is later discarded (follow-up review 2).
        match item.kind {
            WorkKind::Stop => may_land(&mut t.late_stop_until, until),
            WorkKind::Start => may_land(&mut t.late_start_until, until),
            WorkKind::Poll | WorkKind::Round { .. } => {}
        }
        if item.is_control() {
            t.command_out = Some(item.seq);
            t.sent = true;
        }
        // An item that waited for a worker leaves now. The time it waited is
        // the runtime's own queueing, not time the host was away: a stall is
        // judged from when it left (capacity review).
        match &mut t.activity {
            Activity::Verifying { next_round, .. } => *next_round = (*next_round).max(now),
            Activity::Stopping { next_poll, .. } | Activity::Starting { next_poll, .. } => {
                *next_poll = (*next_poll).max(now);
            }
            _ => {}
        }
        true
    }

    /// Right before the first stop or start of an automatic recovery round
    /// of a connected tunnel goes out: if the fault it was queued on is no
    /// longer fresh evidence, or a grace period started since (a stall, a
    /// wake or a network change), the round is withdrawn, neither run nor
    /// counted, the tunnel is probed at once and the fault confirmed afresh
    /// (Codex P2-N46). A tunnel WGAL left down is started on its backoff as
    /// before, and a round whose stop or start went out finishes.
    fn withdraw_stale_recovery(&mut self, id: &TunnelId, now: MonoInstant) {
        let Some(t) = self.tunnels.get_mut(id) else {
            return;
        };
        let seen_up = t.os == OsState::Connected && t.os_error.is_none();
        if t.unsent_automatic() != Some(Op::Recovery)
            || !seen_up
            || (t.fault_fresh(now) && !t.in_grace(now))
        {
            return;
        }
        give_back_round(t);
        t.activity = Activity::Idle;
        forget_unsent_recovery(t);
        t.reset_streak();
        round_due(t, now);
        // Failure is recorded in `storage_ok`.
        let _ = self.persist();
    }

    /// A user's disconnect is never given up (follow-up review F2, F9):
    /// until the tunnel is seen down, its stop is queued again once the
    /// retry wait has passed, including right after a restart.
    fn redrive_disconnects(&mut self, now: MonoInstant) {
        let due: Vec<TunnelId> = self
            .tunnels
            .iter()
            .filter(|(_, t)| {
                t.rec.resume == Some(ResumeOp::Disconnect)
                    && t.rec.intent.desired == DesiredState::Disconnected
                    && !t.rec.binding_lost
                    && t.pending.is_none()
                    && matches!(t.activity, Activity::Idle | Activity::Backoff { .. })
                    && !(t.os == OsState::Disconnected && t.os_error.is_none())
                    && t.stop_retry_at.is_none_or(|at| now >= at)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut kept_deadline = false;
        for id in &due {
            kept_deadline |= self
                .tunnels
                .get(id)
                .is_some_and(|t| t.resume_until.is_some());
            self.enqueue(id, Op::Disconnect, now);
        }
        if kept_deadline {
            // This disconnect is unfinished again, with no deadline: stored
            // before its stop goes out, so a crash cannot drop it at the old
            // deadline (plan §6.4, review of `b5e4828`). A failed save holds
            // nothing back: the intent is stored, and the failure is recorded
            // in `storage_ok`.
            let _ = self.persist();
        }
    }

    /// Plan §9.1: while intent and incidents cannot be stored, no new
    /// automatic stop or start is sent. Automatic operations that have not
    /// sent anything yet are dropped (`Tunnel::unsent_automatic`); one whose
    /// stop was already sent still finishes its start, so WGAL never leaves a
    /// tunnel down that it stopped. A stop or start that passed `still_due`
    /// counts as sent even if a runtime running items on other threads has
    /// not brought its result back yet (review of wave two, N-P3-4). A
    /// recovery of a tunnel left down keeps its stored `resume` record, so it
    /// is started once storage works again.
    fn cancel_unstarted_automatic_ops(&mut self, now: MonoInstant) {
        let mut given_back = false;
        for t in self.tunnels.values_mut() {
            if let Some(op) = t.unsent_automatic() {
                given_back |= give_back_round(t);
                t.activity = if op == Op::Recovery {
                    // Kept over a pause too, so the round runs once storage
                    // works again (Codex P2-N38).
                    withdrawn_recovery(t, now)
                } else {
                    Activity::Idle
                };
                if op == Op::Recovery && matches!(t.activity, Activity::Idle) {
                    given_back |= forget_unsent_recovery(t);
                }
                t.next_round = now;
            }
        }
        if given_back {
            // Usually refused as well while storage fails; then the count
            // the store keeps is the one before, which a restart honours
            // without counting the round twice.
            let _ = self.persist();
        }
    }

    fn grant_control_slot(&mut self, now: MonoInstant) {
        // Nothing can start or stop a tunnel the official manager no longer
        // has, so it must never wait for or hold the slot: it would keep every
        // other tunnel's recovery and disconnect waiting (review of `121b42a`,
        // P1-1).
        for t in self.tunnels.values_mut() {
            if t.rec.binding_lost && t.activity.op().is_some() {
                t.activity = Activity::Idle;
            }
        }
        let tunnels = &self.tunnels;
        self.control_queue.retain(|id| {
            tunnels
                .get(id)
                .is_some_and(|t| !t.rec.binding_lost && t.activity.wants_control_slot())
        });
        let Some(front) = self.control_queue.front() else {
            return;
        };
        let phase = self.timing.control_phase_timeout;
        if let Some(t) = self.tunnels.get_mut(front)
            && let Activity::Queued { op, .. } = t.activity
        {
            match op {
                Op::Recovery => {
                    // 1 for the incident's first round, then 2, 3, ...
                    let rounds = t.rec.incident.as_ref().map_or(0, |i| i.backoff_step);
                    t.note(LogEvent::RecoveryStarted {
                        attempt: rounds.saturating_add(1),
                    });
                }
                Op::ExternalRestart => t.note(LogEvent::RestartedAfterExternalStop),
                Op::Connect | Op::Disconnect | Op::Reconnect => {}
            }
            // A tunnel that is already down needs no stop before its start.
            let down = t.os == OsState::Disconnected && t.os_error.is_none();
            t.activity = if op.stops_first() && !(op.starts() && down) {
                Activity::Stopping {
                    op,
                    deadline: later(now, phase),
                    issued: false,
                    next_poll: now,
                }
            } else {
                Activity::Starting {
                    op,
                    deadline: later(now, phase),
                    issued: false,
                    next_poll: now,
                }
            };
        }
    }

    fn enqueue(&mut self, id: &TunnelId, op: Op, now: MonoInstant) {
        if let Some(t) = self.tunnels.get_mut(id) {
            t.activity = Activity::Queued { op, since: now };
            t.sent = false;
            t.round_counted = false;
            // What WGAL does next is not an external change.
            t.settled_os = None;
            t.resume_until = None;
            if !self.control_queue.contains(id) {
                self.control_queue.push_back(id.clone());
            }
        }
    }

    /// Applies the result of an item from [`Watchdog::plan`]. Results that
    /// are not the tunnel's outstanding item, or were planned under an older
    /// operation generation, are discarded.
    pub fn apply(&mut self, result: WorkResult) {
        let now = self.clock.now();
        let wall = self.clock.wall_unix_ms();
        let caps = self.adapter.capabilities();
        let Some(t) = self.tunnels.get_mut(&result.tunnel_id) else {
            return;
        };
        if t.command_out == Some(result.seq) {
            t.command_out = None;
        }
        if t.pending != Some(result.seq) {
            return;
        }
        t.pending = None;
        let one_off = t.one_off_seq.take() == Some(result.seq);
        if result.generation != t.rec.generation {
            return;
        }
        if one_off {
            if let WorkOutcome::Round { probes, .. } = result.outcome {
                let probes = probes.filter(|_| result.probe_generation == t.probe_generation);
                apply_one_off(t, probes);
            }
            self.gather_events();
            return;
        }
        let ctx = Ctx {
            now,
            wall,
            timing: &self.timing,
            caps,
            probe: t.rec.probe,
            freshness: t.freshness(),
        };
        // The light as it is now, before this result, to notice it turning
        // green.
        let was_green = t.light(now, ctx.timing).light == Light::Green;
        let was_confirmed = t.confirmed;
        if stalled(t, ctx.now, ctx.timing) {
            // The host slept or the process stalled while this work was out.
            // Its result is judged on its own freshness, and the time away is
            // no evidence against the operation's deadlines (plan §6.2,
            // follow-up review F13).
            rebase_deadlines(t, ctx.now, ctx.timing);
        }
        let before = t.rec.clone();
        let before_until = t.resume_until;
        let mut start = None;
        let finished = result.finished;
        match result.outcome {
            WorkOutcome::Command(r) => apply_command(t, r, &ctx),
            WorkOutcome::Poll(r) => apply_poll(t, r, &ctx),
            WorkOutcome::Round { os, probes } => {
                // Probes of a round planned before the host woke or its
                // network changed prove nothing now; its state reading still
                // counts (Codex review of wave three, P1-3).
                let planned_before = result.probe_generation != t.probe_generation;
                let probes = probes.filter(|_| !planned_before);
                match t.activity {
                    Activity::Verifying { op, deadline, .. } => {
                        apply_verify(t, os, probes, finished, op, deadline, &ctx);
                    }
                    _ => start = apply_round(t, os, probes, finished, &ctx),
                }
                if planned_before {
                    // The round that counts is the next one, due at once.
                    round_due(t, ctx.now);
                }
            }
        }
        let transition_deadline = later(t.os_since, ctx.timing.control_phase_timeout);
        t.stuck = (t.os == OsState::Transitioning
            && t.os_error.is_none()
            && ctx.now >= transition_deadline)
            .then(|| timeout("the tunnel has been changing state for too long"));
        let mut opened = false;
        if t.confirmed && t.rec.incident.is_none() {
            let id = IncidentId::new(format!("inc-{:06}", self.next_incident));
            if let Ok(incident_id) = id {
                t.rec.incident = Some(IncidentRecord {
                    incident_id,
                    opened_at_unix_ms: ctx.wall,
                    first_recovery: FirstRecovery::NotAttempted,
                    backoff_step: 0,
                    counted_from_step: 0,
                    retries_exhausted: false,
                    exhausted_notice: NoticeRecord::default(),
                    manual_last: false,
                    resume_uncounted: false,
                    wait_from_unix_ms: None,
                    last_round_end_unix_ms: None,
                    next_retry_unix_ms: None,
                    notification: NotificationState::NotEligible,
                    notification_message_id: None,
                    cancel_requested: false,
                });
                opened = true;
            }
        }
        if t.confirmed
            && !was_confirmed
            && let Some(incident) = &t.rec.incident
        {
            let incident_id = incident.incident_id.clone();
            t.note(LogEvent::FaultConfirmed { incident_id });
        }
        let changed = t.rec != before || t.resume_until != before_until;
        let id = result.tunnel_id;
        if opened {
            self.next_incident = self.next_incident.saturating_add(1);
        }
        if changed {
            // A failure marks storage as failed, which stops new automatic
            // actions below; monitoring carries on.
            let _ = self.persist();
        }
        let now = self.clock.now();
        let monitoring = self.tunnels.get(&id).is_some_and(Tunnel::monitoring);
        if let Some(op) = start
            && self.storage_ok
            && self.writable()
            && (monitoring || !op.is_automatic())
            && !(self.automatic_held && op.is_automatic())
        {
            let recount = op == Op::Recovery
                && self.tunnels.get(&id).is_some_and(|t| {
                    t.rec
                        .incident
                        .as_ref()
                        .is_some_and(|incident| incident.resume_uncounted)
                });
            if !recount || self.count_again(&id) {
                self.enqueue(&id, op, now);
                if op == Op::Recovery
                    && let Some(t) = self.tunnels.get_mut(&id)
                {
                    // Counted again just now, or a retry the process that
                    // queued it counted before a restart: either way it is
                    // given back if it never sends anything or meets the host
                    // offline (Codex P2-N49). A first round is never counted.
                    t.round_counted = recount
                        || t.rec.incident.as_ref().is_some_and(|incident| {
                            incident.last_round_end_unix_ms.is_some()
                                && incident.backoff_step > incident.counted_from_step
                        });
                }
            }
        }
        self.consider_recovery(&id);
        self.refresh_light(&id);
        if let Some(t) = self.tunnels.get_mut(&id)
            && !was_green
            && t.light.is_some_and(|(light, _)| light == Light::Green)
        {
            t.note(LogEvent::TrafficVerified);
        }
        self.gather_events();
    }

    /// Queues automatic recovery if the tunnel needs it and every
    /// precondition of plan §6.3 holds. The incident, and the fact that a round
    /// is under way, are persisted before the round is queued; if that fails,
    /// nothing is queued.
    fn consider_recovery(&mut self, id: &TunnelId) {
        if !self.storage_ok || !self.writable() || self.automatic_held {
            return;
        }
        let now = self.clock.now();
        let caps = self.adapter.capabilities();
        let Some(t) = self.tunnels.get_mut(id) else {
            return;
        };
        let intent = t.rec.intent;
        if !intent.auto_recovery_enabled
            || !t.monitoring()
            || intent.desired != DesiredState::Connected
            || caps.start_stop == Capability::Unsupported
            || t.pending.is_some()
        {
            return;
        }
        if t.rec.resume == Some(ResumeOp::Connect) {
            // The user's connect may still be finished by a stop of WGAL's
            // that lands late (plan §6.4): a recovery queued now would
            // replace its record, and if that recovery were withdrawn before
            // it sent anything the connect would be lost. The recovery waits
            // until the record is settled, at most the late-effect window
            // (Codex P2-N51).
            return;
        }
        if matches!(t.activity, Activity::Idle | Activity::Backoff { .. })
            && t.confirmed
            && !t.fault_fresh(now)
        {
            // The fault was confirmed on evidence that is no longer fresh:
            // the process stalled, or the results came back late. It is no
            // evidence for an automatic round, or against a round that ended
            // inconclusive, and is confirmed afresh (Codex P2-N35, P2-N39,
            // P2-N40), with a round due at once for that.
            t.reset_streak();
            round_due(t, now);
        }
        if inconclusive_round_failed(t, now) && self.persist().is_err() {
            // Not stored: no new automatic action (plan §9.1). The change
            // is saved with the next save that succeeds.
            return;
        }
        let Some(t) = self.tunnels.get_mut(id) else {
            return;
        };
        let before = t.rec.clone();
        let retry = t.rec.retry;
        let wait = retry_wait(retry);
        let block = t.recovery_block(now);
        let confirmed = t.confirmed;
        // Our own failed start may have left the tunnel down; that is still
        // this fault, not an external stop.
        let left_down = t.os == OsState::Disconnected && t.os_error.is_none();
        let last_round_end = t.last_round_end;
        let after_last = t.round_due_after(wait);
        let offline_fresh = t.offline_fresh(now);
        let Some(incident) = t.rec.incident.as_mut() else {
            return;
        };
        let step_before = incident.backoff_step;
        // Whether a round of the incident has ended at all, as stored (Codex
        // P2-N47).
        let ended = last_round_end.is_some() || incident.last_round_end_unix_ms.is_some();
        match t.activity {
            Activity::Idle => {
                if !confirmed || block.is_some() {
                    return;
                }
                match incident.first_recovery {
                    FirstRecovery::NotAttempted | FirstRecovery::InProgress => {
                        // A first round that ended inconclusive is tried
                        // again only after its backoff, so errors that only
                        // show during verification cannot restart the tunnel
                        // every couple of minutes (re-review NF-3). That next
                        // round is one after the first, and counted as such.
                        if incident.first_recovery == FirstRecovery::InProgress && ended {
                            if incident.retries_exhausted {
                                return;
                            }
                            if let Some(retry_at) = after_last
                                && now < retry_at
                            {
                                // Due on its own interval (Codex P2-N17).
                                t.activity = Activity::Backoff { retry_at };
                                return;
                            }
                            if !retry_left(retry, incident) {
                                exhaust_retries(t);
                                // Failure is recorded in `storage_ok`.
                                let _ = self.persist();
                                return;
                            }
                            incident.backoff_step = incident.backoff_step.saturating_add(1);
                        }
                        incident.first_recovery = FirstRecovery::InProgress;
                    }
                    _ => {
                        // A later round of the same incident counts against
                        // the tunnel's retry limit (owner decision of
                        // 2026-11-04), and still waits the retry interval
                        // from the end of the previous round.
                        if incident.retries_exhausted {
                            return;
                        }
                        if !retry_left(retry, incident) {
                            exhaust_retries(t);
                            // Failure is recorded in `storage_ok`.
                            let _ = self.persist();
                            return;
                        }
                        if let Some(retry_at) = after_last
                            && now < retry_at
                        {
                            t.activity = Activity::Backoff { retry_at };
                            return;
                        }
                        incident.backoff_step = incident.backoff_step.saturating_add(1);
                    }
                }
            }
            Activity::Backoff { retry_at } => {
                if now < retry_at {
                    return;
                }
                if t.os_error.is_some()
                    || !matches!(t.os, OsState::Connected | OsState::Disconnected)
                {
                    // Whether the tunnel is down cannot be told right now (a
                    // read error, or still transitioning or unknown): wait for
                    // a settled reading instead of giving the retry up for
                    // good (plan §6.3; re-reviews of 4b843b6 and 50eb014).
                    return;
                }
                // A tunnel WGAL left down cannot show a verified path or a
                // fresh failure, and nothing probes it, so a probe error from
                // before cannot clear; only what would make starting it wrong
                // blocks. Retries stay on the backoff schedule (re-review of
                // 64a3030, N2). The host found offline does block, neither
                // run nor counted, while that finding is evidence: until the
                // network changes or the host wakes, or the mark is older
                // than the tunnel's freshness (Codex P2-N19).
                let resumable = match block {
                    None
                    | Some(
                        Block::NotConnected | Block::Path | Block::Grace | Block::ProbeUnavailable,
                    ) => true,
                    Some(Block::LocalOffline) => !offline_fresh,
                    Some(_) => false,
                };
                let allowed = block.is_none() || (left_down && resumable);
                // A first round withdrawn before it sent anything, and
                // waiting here, is still the incident's first round: it is
                // not counted against the retry limit (Codex P2-N42).
                let first = matches!(
                    incident.first_recovery,
                    FirstRecovery::NotAttempted | FirstRecovery::InProgress
                ) && !ended;
                if !first && !retry_left(retry, incident) {
                    // A limit lowered while the retry waited.
                    exhaust_retries(t);
                    // Failure is recorded in `storage_ok`.
                    let _ = self.persist();
                    return;
                }
                if !(confirmed || left_down) || !allowed {
                    if !left_down {
                        // The retry is due but the fault is not confirmed now
                        // (for example after a restart): nothing waits any
                        // more, and a new confirmation starts the next round.
                        t.activity = Activity::Idle;
                        if incident.next_retry_unix_ms.take().is_some() {
                            // Failure is recorded in `storage_ok`.
                            let _ = self.persist();
                        }
                    }
                    return;
                }
                if first {
                    incident.first_recovery = FirstRecovery::InProgress;
                } else {
                    incident.backoff_step = incident.backoff_step.saturating_add(1);
                }
            }
            _ => return,
        }
        let counted = incident.backoff_step != step_before;
        incident.next_retry_unix_ms = None;
        incident.manual_last = false;
        incident.resume_uncounted = false;
        t.rec.resume = Some(ResumeOp::Recovery);
        let kept_until = t.resume_until.take();
        if self.persist().is_err() {
            if let Some(t) = self.tunnels.get_mut(id) {
                t.rec = before;
                t.resume_until = kept_until;
            }
            return;
        }
        self.enqueue(id, Op::Recovery, now);
        if let Some(t) = self.tunnels.get_mut(id) {
            t.round_counted = counted;
        }
    }

    /// A counted round given back before it sent anything is started again
    /// by its `resume` record: it counts again, within the tunnel's limit,
    /// and is stored before it is queued. Returns whether it may be queued.
    fn count_again(&mut self, id: &TunnelId) -> bool {
        let Some(t) = self.tunnels.get_mut(id) else {
            return false;
        };
        let before = t.rec.clone();
        let retry = t.rec.retry;
        let Some(incident) = t.rec.incident.as_mut() else {
            return true;
        };
        incident.resume_uncounted = false;
        if !retry_left(retry, incident) {
            // A limit lowered meanwhile: the round is not run, and nothing
            // is left to resume.
            t.rec.resume = None;
            exhaust_retries(t);
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
            return false;
        }
        incident.backoff_step = incident.backoff_step.saturating_add(1);
        if self.persist().is_err() {
            if let Some(t) = self.tunnels.get_mut(id) {
                t.rec = before;
            }
            return false;
        }
        true
    }

    fn persist(&mut self) -> Result<(), StoreError> {
        if self.read_only {
            self.storage_ok = false;
            return Err(StoreError::Refused(
                "stored state could not be loaded; it is not overwritten",
            ));
        }
        if self.saves_held {
            // Stored together when `all_or_nothing` ends.
            self.held = true;
            return Ok(());
        }
        let now = self.clock.now();
        let wall = self.clock.wall_unix_ms();
        let state = PersistedState {
            schema_version: SCHEMA_VERSION,
            installation_id: self.installation_id.clone(),
            next_incident: self.next_incident,
            monitoring_paused: self.monitoring_paused,
            mcp_enabled: self.mcp_enabled,
            mcp_telegram_enabled: self.mcp_telegram_enabled,
            host: self.host.clone(),
            tunnels: self
                .tunnels
                .values()
                .map(|t| {
                    let mut rec = t.rec.clone();
                    rec.resume_until_unix_ms = rec
                        .resume
                        .and(t.resume_until)
                        .map(|until| wall_at(now, wall, until));
                    // When the incident's wait started, in the wall clock of
                    // this save: the time waited by this process's clock
                    // before now, so a wall clock set since does not move it
                    // apart from the retry time saved with it (Codex
                    // P2-N60).
                    if let (Some(incident), Some(waited)) = (rec.incident.as_mut(), t.waited(now)) {
                        incident.wait_from_unix_ms = Some(wall.saturating_sub(millis(waited)));
                    }
                    rec
                })
                .collect(),
        };
        let result = self.store.save(&state);
        self.storage_ok = result.is_ok();
        result
    }

    fn refresh_light(&mut self, id: &TunnelId) {
        let now = self.clock.now();
        let wall = self.clock.wall_unix_ms();
        if let Some(t) = self.tunnels.get_mut(id) {
            let view = t.light(now, &self.timing);
            let current = Some((view.light, view.reason));
            if t.light != current {
                t.light = current;
                t.light_since_unix_ms = wall;
            }
        }
    }

    fn refresh_lights(&mut self) {
        let ids: Vec<TunnelId> = self.tunnels.keys().cloned().collect();
        for id in &ids {
            self.refresh_light(id);
        }
    }

    /// The host woke from sleep: earlier samples are not evidence now, and
    /// failures are not counted during the grace period (plan §6.2, T15).
    pub fn on_wake(&mut self) {
        self.restart_grace();
    }

    /// The host's network changed (another Wi-Fi, a cable): as after a wake.
    pub fn on_network_change(&mut self) {
        self.restart_grace();
    }

    /// Every tunnel starts a grace period and is probed again at once.
    /// Nothing from before counts: the failure streak, the offline mark, the
    /// answers that kept the light green and the path they proved, and the
    /// probes of a round still out (Codex review of wave three, P1-3). A
    /// stop or start under way still finishes, with its deadlines restarted.
    fn restart_grace(&mut self) {
        let now = self.clock.now();
        let timing = &self.timing;
        for t in self.tunnels.values_mut() {
            t.reset_streak();
            // Whether the host is offline is judged afresh by the next round,
            // which is due now; recovery waits for the grace period anyway
            // (review R982-P3-b).
            t.forget_offline();
            // Until a round planned from now answers, the light shows the
            // grace period, not green, however long the tunnel's freshness.
            t.forget_path_evidence();
            // A pending notification waits until the fault is seen again
            // (plan §7.3: still failing now).
            t.fault_seen = false;
            t.grace_until = later(now, timing.grace);
            t.next_round = now;
            rebase_deadlines(t, now, timing);
        }
        self.refresh_lights();
    }

    /// Answers one GUI request (plan §3.2). The caller has already decoded it
    /// with the codec and verified the peer's OS identity.
    pub fn handle(&mut self, request: &Request) -> Outcome {
        let result = self.dispatch(request);
        self.refresh_lights();
        self.gather_events();
        match result {
            Ok(response) => Outcome::Ok(response),
            Err((code, reason, message)) => {
                Outcome::Err(ProtocolError::new(code, Some(reason), message))
            }
        }
    }

    fn dispatch(&mut self, request: &Request) -> Result<Response, Rejection> {
        match request {
            Request::GetSnapshot => Ok(Response::Snapshot {
                snapshot: self.snapshot(),
            }),
            Request::DiscoverTunnels => self.discover(),
            Request::RegisterTunnel { tunnel_id } => self.register(tunnel_id),
            Request::UpdateMonitorSettings {
                tunnel_id,
                settings,
            } => self.update_settings(tunnel_id, settings),
            Request::ProbeNow { tunnel_id } => {
                let now = self.clock.now();
                let t = self.tunnel_mut(tunnel_id)?;
                if matches!(t.activity, Activity::Idle | Activity::Backoff { .. }) {
                    if t.monitoring() {
                        t.next_round = now;
                    } else if t.one_off_seq.is_none() || t.pending != t.one_off_seq {
                        // Not monitored: one round, asked for (owner
                        // decision of 2026-11-06), whose result only shows
                        // what the targets answer (see `apply_one_off`).
                        t.one_off_at = Some(now);
                    }
                }
                Ok(Response::Accepted)
            }
            Request::Connect { tunnel_id } => self.connect(tunnel_id),
            Request::Disconnect { tunnel_id } => self.disconnect(tunnel_id),
            Request::Reconnect { tunnel_id } => self.reconnect(tunnel_id),
            Request::SetAutoRecovery { tunnel_id, enabled } => {
                self.set_auto_recovery(tunnel_id, *enabled)
            }
            Request::SetStartWhenStoppedExternally { tunnel_id, enabled } => {
                let enabled = *enabled;
                self.change(tunnel_id, |rec| {
                    rec.intent.start_when_stopped_externally = enabled;
                    Ok(())
                })?;
                Ok(Response::Accepted)
            }
            Request::SetMonitoringPaused { paused } => self.set_monitoring_paused(*paused),
            Request::SetMcpEnabled { enabled } => self.set_mcp_enabled(*enabled),
            Request::SetMcpTelegramEnabled { enabled } => self.set_mcp_telegram_enabled(*enabled),
            Request::SetTunnelMonitoring { tunnel_id, enabled } => {
                self.set_tunnel_monitoring(tunnel_id, *enabled)
            }
            Request::SetTunnelNotifications { tunnel_id, enabled } => {
                self.set_tunnel_notifications(tunnel_id, *enabled)
            }
            Request::SetRecoveryRetry { tunnel_id, retry } => {
                self.set_recovery_retry(tunnel_id, *retry)
            }
            Request::UnregisterTunnel { tunnel_id } => self.unregister(tunnel_id),
            // Notification settings, the bot token and test messages belong to
            // the runtime's notification service, which answers them before
            // they reach the core.
            Request::UpdateNotificationSettings { .. }
            | Request::SetBotToken { .. }
            | Request::ClearBotToken
            | Request::SendTestNotification
            | Request::SetNotificationTemplates { .. }
            // `wgal`'s shared language is kept with the notification
            // settings (owner decision of 2026-11-19).
            | Request::SetCliLanguage { .. } => Err((
                ErrorCode::Internal,
                ErrorReason::NotAvailableHere,
                "notifications are not available in this watchdog",
            )),
            // The runtime checks and applies the host settings and keeps the
            // event log, and answers these before they reach the core.
            // So are the settings file's export and import, which also touch
            // the host settings and notifications.
            Request::UpdateHostSettings { .. }
            | Request::ReadEventLog { .. }
            | Request::ExportSettings
            | Request::ImportSettings { .. } => Err((
                ErrorCode::Internal,
                ErrorReason::NotAvailableHere,
                "this request is not available in this watchdog",
            )),
        }
    }

    /// A tunnel the official manager no longer has cannot be started or
    /// stopped until it is registered again (plan §4.2).
    fn refuse_lost(&mut self, id: &TunnelId) -> Result<(), Rejection> {
        if self.tunnel_mut(id)?.rec.binding_lost {
            return Err((
                ErrorCode::InvalidState,
                ErrorReason::BindingLost,
                "the official manager no longer has this tunnel; register it again",
            ));
        }
        Ok(())
    }

    fn tunnel_mut(&mut self, id: &TunnelId) -> Result<&mut Tunnel, Rejection> {
        self.tunnels.get_mut(id).ok_or((
            ErrorCode::UnknownTunnel,
            ErrorReason::TunnelNotRegistered,
            "tunnel is not registered",
        ))
    }

    /// Changes the tunnel's record under a new generation and persists it
    /// before anything acts on it. If persisting fails the change is undone
    /// and the request rejected, so an intent is never acted on unrecorded.
    fn change(
        &mut self,
        id: &TunnelId,
        f: impl FnOnce(&mut TunnelRecord) -> Result<(), Rejection>,
    ) -> Result<(), Rejection> {
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        let t = self.tunnel_mut(id)?;
        let before = t.rec.clone();
        f(&mut t.rec)?;
        t.rec.generation = t.rec.generation.wrapping_add(1);
        // Anything planned before this change is now stale.
        t.pending = None;
        if self.persist().is_err() {
            if let Some(t) = self.tunnels.get_mut(id) {
                t.rec = before;
            }
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the change could not be stored",
            ));
        }
        Ok(())
    }

    /// Changes the tunnel's record and persists it, for a setting that
    /// changes nothing WGAL is doing to the tunnel: no new generation, so
    /// work already planned stays valid. If persisting fails the change is
    /// undone and the request rejected. A change that leaves the record as
    /// it was is accepted without a save.
    fn change_setting(
        &mut self,
        id: &TunnelId,
        f: impl FnOnce(&mut TunnelRecord),
    ) -> Result<(), Rejection> {
        let t = self.tunnel_mut(id)?;
        let before = t.rec.clone();
        let mut after = before.clone();
        f(&mut after);
        if after == before {
            return Ok(());
        }
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        self.tunnel_mut(id)?.rec = after;
        if self.persist().is_err() {
            if let Some(t) = self.tunnels.get_mut(id) {
                t.rec = before;
            }
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the change could not be stored",
            ));
        }
        Ok(())
    }

    /// [`Self::change`] for a request that replaces what was to be resumed: a
    /// deadline kept for the old record does not carry over to the new one.
    fn change_resume(
        &mut self,
        id: &TunnelId,
        f: impl FnOnce(&mut TunnelRecord) -> Result<(), Rejection>,
    ) -> Result<(), Rejection> {
        let kept = self.tunnel_mut(id)?.resume_until.take();
        let result = self.change(id, f);
        if result.is_err()
            && let Some(t) = self.tunnels.get_mut(id)
        {
            t.resume_until = kept;
        }
        result
    }

    fn discover(&mut self) -> Result<Response, Rejection> {
        let found = self.adapter.discover().map_err(|_| {
            (
                ErrorCode::Internal,
                ErrorReason::ManagerQueryFailed,
                "the official manager could not be queried",
            )
        })?;
        self.discovered.clear();
        let mut tunnels = Vec::new();
        for item in found {
            let Some(id) = candidate_id(&item.binding) else {
                continue;
            };
            if tunnels.len() < MAX_DISCOVERED {
                tunnels.push(DiscoveredTunnel {
                    tunnel_id: id.clone(),
                    display_name: item.display_name.clone(),
                    control: item.control,
                });
            }
            self.discovered.insert(id, item);
        }
        let tunnels = BoundedVec::try_from(tunnels).map_err(|_| {
            (
                ErrorCode::Internal,
                ErrorReason::TooManyDiscovered,
                "too many tunnels discovered",
            )
        })?;
        Ok(Response::Discovered { tunnels })
    }

    fn register(&mut self, id: &TunnelId) -> Result<Response, Rejection> {
        let found = self.discovered.get(id).cloned().ok_or((
            ErrorCode::UnknownTunnel,
            ErrorReason::TunnelNotDiscovered,
            "tunnel was not among the discovered ones",
        ))?;
        // A tunnel another manager owns (for example a NetworkManager
        // connection on Ubuntu) is never taken over (plan §1.2, §4.4).
        if found.control == Capability::Unsupported {
            return Err((
                ErrorCode::InvalidState,
                ErrorReason::ManagedByAnotherTool,
                "another tool manages this tunnel; WGAL does not take it over",
            ));
        }
        // Plan §4.4: never take on a tunnel whose configuration or hooks an
        // unprivileged user could change.
        if self.adapter.check_registration(&found.binding).is_err() {
            return Err((
                ErrorCode::InvalidState,
                ErrorReason::ConfigNotProtected,
                "the tunnel's configuration is not protected well enough to control it",
            ));
        }
        if let Some(t) = self.tunnels.get(id) {
            if t.binding != found.binding {
                return Err((
                    ErrorCode::InvalidState,
                    ErrorReason::IdConflict,
                    "the ID belongs to another tunnel",
                ));
            }
            if t.rec.binding_lost {
                self.change(id, |rec| {
                    rec.binding_lost = false;
                    Ok(())
                })?;
            }
            return Ok(Response::Accepted);
        }
        if self.tunnels.len() >= MAX_TUNNELS {
            return Err((
                ErrorCode::InvalidState,
                ErrorReason::TooManyTunnels,
                "no more tunnels can be registered",
            ));
        }
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        // Registering never changes the tunnel: the wanted state starts as
        // whatever the manager shows now.
        let desired = match self.adapter.query_state(&found.binding) {
            Ok(OsState::Connected | OsState::Transitioning) => DesiredState::Connected,
            _ => DesiredState::Disconnected,
        };
        let rec = TunnelRecord {
            tunnel_id: id.clone(),
            binding: found.binding.as_str().to_owned(),
            display_name: found.display_name.clone(),
            intent: UserIntent {
                desired,
                monitoring_enabled: true,
                auto_recovery_enabled: false,
                start_when_stopped_externally: false,
            },
            recovery_target: None,
            observe_targets: BoundedVec::new(),
            probe: ProbeTiming::DEFAULT,
            retry: RecoveryRetry::DEFAULT,
            notifications_enabled: true,
            baseline_passed: false,
            external: None,
            binding_lost: false,
            generation: 0,
            incident: None,
            resume: None,
            resume_until_unix_ms: None,
            log_label: None,
            recovered_notices: Vec::new(),
            auto_recovery_after_baseline: None,
        };
        let now = self.clock.now();
        let tunnel = self.new_tunnel(rec, found.binding, now);
        self.tunnels.insert(id.clone(), tunnel);
        self.label_tunnels();
        if self.persist().is_err() {
            self.tunnels.remove(id);
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the tunnel could not be stored",
            ));
        }
        Ok(Response::Accepted)
    }

    fn update_settings(
        &mut self,
        id: &TunnelId,
        settings: &MonitorSettings,
    ) -> Result<Response, Rejection> {
        let settings = settings.clone();
        let target_changed = self.tunnel_mut(id)?.rec.recovery_target != settings.recovery_target;
        let observe_changed = self.tunnel_mut(id)?.rec.observe_targets != settings.observe_targets;
        let timing_changed = self.tunnel_mut(id)?.rec.probe != settings.probe;
        self.change(id, |rec| {
            if target_changed {
                // A new recovery target must pass its own baseline before
                // it can drive recovery (plan §5.1).
                rec.recovery_target = settings.recovery_target;
                rec.baseline_passed = false;
                rec.intent.auto_recovery_enabled = false;
                rec.auto_recovery_after_baseline = None;
            }
            rec.observe_targets = settings.observe_targets;
            rec.probe = settings.probe;
            Ok(())
        })?;
        let now = self.clock.now();
        let t = self.tunnel_mut(id)?;
        if target_changed {
            t.path = Path::Unknown;
            t.probe_error = None;
            t.route_error = None;
            t.last_success = None;
            // Only probes of the recovery target clear it, so without one it
            // would never clear.
            t.forget_offline();
        }
        if target_changed || timing_changed {
            // Failures counted under the old target or timing do not count
            // towards the new one. A new timing takes effect at the next
            // round, which is due now; it keeps the baseline and auto
            // recovery (owner decision of 2026-11-03).
            t.reset_streak();
        }
        if target_changed || observe_changed {
            t.reset_views();
        }
        t.next_round = now;
        Ok(Response::Accepted)
    }

    /// Pauses or resumes monitoring of one tunnel, stored first like any
    /// other intent. Off cancels automatic actions that have not started and
    /// plans nothing for the tunnel on its own: no round, so no probe and no
    /// OS state read, and no automatic action. An operation already under
    /// way finishes (a stop already sent is followed by its start and its
    /// verification), and the user's connect, disconnect and reconnect still
    /// work. On again counts nothing from before and starts with a grace
    /// period, as the host-wide resume does (see `monitoring_changed`). The
    /// tunnel is never started or stopped because of it.
    fn set_tunnel_monitoring(
        &mut self,
        id: &TunnelId,
        enabled: bool,
    ) -> Result<Response, Rejection> {
        let t = self.tunnel_mut(id)?;
        if t.rec.intent.monitoring_enabled == enabled {
            return Ok(Response::Accepted);
        }
        let was_monitoring = t.monitoring();
        self.change(id, |rec| {
            rec.intent.monitoring_enabled = enabled;
            if !enabled && rec.resume == Some(ResumeOp::Recovery) {
                rec.resume = None;
            }
            Ok(())
        })?;
        let now = self.clock.now();
        let grace = self.timing.grace;
        let t = self.tunnel_mut(id)?;
        let given_back = monitoring_changed(t, was_monitoring, now, grace);
        t.note(if enabled {
            LogEvent::MonitoringResumed
        } else {
            LogEvent::MonitoringPaused
        });
        if given_back {
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        Ok(Response::Accepted)
    }

    /// Forgets WGAL's registration of the tunnel and everything kept for it:
    /// its record, open incident (and with it any notification not sent
    /// yet), targets and settings. Nothing is sent to the platform: the
    /// tunnel stays as it is, and its official configuration is never
    /// touched. Refused with `busy` while WGAL has anything unfinished on it
    /// (see `Tunnel::unfinished`), and with `storage_unavailable`, keeping
    /// everything, if the change cannot be stored. A result still out for
    /// it is discarded when it comes back. Registering it again later is a
    /// fresh registration.
    fn unregister(&mut self, id: &TunnelId) -> Result<Response, Rejection> {
        let now = self.clock.now();
        let queued = self.control_queue.contains(id);
        let t = self.tunnel_mut(id)?;
        if queued || t.unfinished(now) {
            return Err((
                ErrorCode::Busy,
                ErrorReason::OperationInProgress,
                "WGAL has an operation on this tunnel under way; try again once it has finished",
            ));
        }
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        // What was noted before keeps its place ahead of the removal.
        self.gather_events();
        let Some(removed) = self.tunnels.remove(id) else {
            return Err((
                ErrorCode::UnknownTunnel,
                ErrorReason::TunnelNotRegistered,
                "tunnel is not registered",
            ));
        };
        if self.persist().is_err() {
            self.tunnels.insert(id.clone(), removed);
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the change could not be stored",
            ));
        }
        if let Some(label) = removed.rec.log_label {
            self.retired.push(Retired {
                tunnel_id: id.clone(),
                label,
                handed_over: false,
            });
        }
        queue_event(
            &mut self.events,
            &mut self.events_lost,
            Some(id.clone()),
            LogEvent::TunnelRemoved,
        );
        Ok(Response::Accepted)
    }

    /// The tunnel's own notification switch, under the global one. Off
    /// cancels the incident's notification as turning notifications off
    /// does: a pending one at once, one in flight once its result is back.
    /// On again never re-arms a cancelled one.
    fn set_tunnel_notifications(
        &mut self,
        id: &TunnelId,
        enabled: bool,
    ) -> Result<Response, Rejection> {
        self.change_setting(id, |rec| {
            rec.notifications_enabled = enabled;
            if !enabled {
                cancel_notices(rec);
            }
        })?;
        Ok(Response::Accepted)
    }

    /// Asks for automatic recovery to turn on, with `start_when_stopped_externally`,
    /// once the recovery target passes its baseline check, if that happens
    /// before `until_unix_ms` (owner decision of 2026-11-04: an import that
    /// sets up a tunnel whose baseline has not passed yet). The caller
    /// authorizes it as it would `SetAutoRecovery`. Stored at once.
    pub fn arm_auto_recovery(
        &mut self,
        id: &TunnelId,
        start_when_stopped_externally: bool,
        until_unix_ms: i64,
    ) -> Result<(), ProtocolError> {
        self.change(id, |rec| {
            rec.auto_recovery_after_baseline = Some(PendingAutoRecovery {
                start_when_stopped_externally,
                until_unix_ms,
            });
            Ok(())
        })
        .map_err(|(code, reason, message)| ProtocolError::new(code, Some(reason), message))
    }

    /// What an import asked for the tunnel's automatic recovery while its
    /// recovery target's baseline has not passed: `None` when nothing waits
    /// for it, or the wait is over.
    pub fn pending_auto_recovery(&self, id: &TunnelId) -> Option<PendingAutoRecovery> {
        let wall = self.clock.wall_unix_ms();
        self.tunnels
            .get(id)?
            .rec
            .auto_recovery_after_baseline
            .filter(|p| wall <= p.until_unix_ms)
    }

    /// Runs `f`, which hands the watchdog requests and changes, with every
    /// save held back, and then stores all it changed in one save: the
    /// changes take effect together or not at all (one tunnel's part of a
    /// settings import; Codex P2-N21). If `f` returns an error, or that save
    /// fails, everything that is stored is put back as it was before `f`
    /// ran: each tunnel's record, the host-wide settings, and a tunnel
    /// registered meanwhile is forgotten again; the events noted meanwhile
    /// are dropped. What is not stored, such as a grace period restarted or
    /// probe evidence forgotten, stays as `f` left it and is judged afresh
    /// as after any change. `f` must not unregister a tunnel.
    pub fn all_or_nothing<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        // Everything `f` can change, put back whole when it is undone: each
        // tunnel with what it is doing and what is out for it, so work in
        // flight is applied as if `f` never ran (Codex P2-N28), and the
        // event queue as it was (Codex P3-N33).
        let saved = (
            self.tunnels.clone(),
            self.control_queue.clone(),
            self.events.clone(),
            self.events_lost,
            self.host_offline_noted,
            self.next_slot,
        );
        let host = (
            self.monitoring_paused,
            self.mcp_enabled,
            self.mcp_telegram_enabled,
            self.host.clone(),
            self.next_incident,
        );
        let outer = std::mem::replace(&mut self.saves_held, true);
        let outer_held = std::mem::take(&mut self.held);
        let result = f(self);
        self.saves_held = outer;
        let asked = std::mem::take(&mut self.held);
        // An outer `all_or_nothing` stores this part with its own.
        self.held = outer_held || (outer && asked);
        let error = match result {
            Ok(value) if outer || !asked => return Ok(value),
            Ok(value) => match self.persist() {
                Ok(()) => return Ok(value),
                Err(_) => ProtocolError::new(
                    ErrorCode::StorageUnavailable,
                    Some(self.storage_reason()),
                    "the change could not be stored",
                ),
            },
            Err(error) => error,
        };
        let (tunnels, control_queue, events, events_lost, host_offline_noted, next_slot) = saved;
        self.tunnels = tunnels;
        self.control_queue = control_queue;
        self.events = events;
        self.events_lost = events_lost;
        self.host_offline_noted = host_offline_noted;
        self.next_slot = next_slot;
        let (paused, mcp, mcp_telegram, host, next_incident) = host;
        self.monitoring_paused = paused;
        self.mcp_enabled = mcp;
        self.mcp_telegram_enabled = mcp_telegram;
        self.host = host;
        self.next_incident = next_incident;
        Err(error)
    }

    /// How automatic recovery tries again after its first round (owner
    /// decision of 2026-11-04). It takes effect at once: a retry already
    /// waiting is rescheduled from the end of the last round with the new
    /// interval; a used-up count resumes if the new limit allows more; one
    /// that the new limit has used up stops at the next retry. Nothing is
    /// started or stopped here.
    fn set_recovery_retry(
        &mut self,
        id: &TunnelId,
        retry: RecoveryRetry,
    ) -> Result<Response, Rejection> {
        self.change_setting(id, |rec| {
            rec.retry = retry;
            if let Some(incident) = &mut rec.incident
                && incident.retries_exhausted
                && retry.allows(retries_used(incident))
            {
                incident.retries_exhausted = false;
            }
        })?;
        let now = self.clock.now();
        let wall = self.clock.wall_unix_ms();
        let t = self.tunnel_mut(id)?;
        if let Activity::Backoff { retry_at } = t.activity {
            let wait = retry_wait(retry);
            // From the start of the incident's wait, as this process saw it or
            // as stored before a restart (Codex P2-N53). A backoff with no
            // such start (a first round withdrawn before it ran) keeps its
            // time: it is due at once (Codex P2-N59).
            let at = t.round_due_after(wait).unwrap_or(retry_at);
            if at != retry_at {
                t.activity = Activity::Backoff { retry_at: at };
                let ahead = at.saturating_duration_since(now);
                if let Some(incident) = &mut t.rec.incident {
                    incident.next_retry_unix_ms = Some(wall.saturating_add(millis(ahead)));
                }
                // Failure is recorded in `storage_ok`; the retry time is
                // saved with the next save that succeeds.
                let _ = self.persist();
            }
        }
        Ok(Response::Accepted)
    }

    /// Only a connect that will send a start is recorded as unfinished
    /// (`ResumeOp::Connect`); one that finds the tunnel up sends nothing and
    /// leaves nothing to resume, so a later external stop is judged by the
    /// user's settings alone (follow-up review 2, finding 3).
    fn connect(&mut self, id: &TunnelId) -> Result<Response, Rejection> {
        self.refuse_lost(id)?;
        let now = self.clock.now();
        let t = self.tunnel_mut(id)?;
        // Every operation under way ends with a start (a disconnect's becomes
        // a reconnect below); with none, only a tunnel not known to be up is
        // started.
        let starts = t.activity.op().is_some() || !t.known_up(now);
        let intent = |rec: &mut TunnelRecord| {
            rec.intent.desired = DesiredState::Connected;
            rec.external = None;
            restart_retry_count(rec);
            rec.resume = if starts {
                Some(ResumeOp::Connect)
            } else {
                // Nothing to start; an unfinished disconnect is superseded,
                // and a restored operation's record stays for its own phase.
                rec.resume.filter(|op| *op != ResumeOp::Disconnect)
            };
            Ok(())
        };
        if starts {
            self.change_resume(id, intent)?;
        } else {
            self.change(id, intent)?;
        }
        let t = self.tunnel_mut(id)?;
        t.control_error = None;
        t.stop_attempts = 0;
        t.stop_retry_at = None;
        let next = match t.activity {
            Activity::Queued {
                op: Op::Disconnect, ..
            } => Some(Op::Connect),
            Activity::Stopping {
                op: Op::Disconnect,
                deadline,
                issued,
                next_poll,
            } => {
                // The stop is under way; finish it, then start again.
                t.activity = Activity::Stopping {
                    op: Op::Reconnect,
                    deadline,
                    issued,
                    next_poll,
                };
                None
            }
            Activity::Idle | Activity::Backoff { .. } if starts => Some(Op::Connect),
            Activity::Idle | Activity::Backoff { .. } => {
                t.next_round = now;
                None
            }
            _ => None,
        };
        if let Some(op) = next {
            self.enqueue(id, op, now);
        }
        Ok(Response::Accepted)
    }

    /// Plan §6.4: the intent and a new generation are persisted first, pending
    /// retries and notifications are cancelled, and only then is the tunnel
    /// stopped. A start already sent cannot be recalled, so it is followed by
    /// a stop; nothing planned before this request can start it again. Until
    /// the tunnel is seen down, the disconnect stays recorded as unfinished,
    /// so a failed stop is tried again and a restart finishes it (follow-up
    /// review F2, F9).
    fn disconnect(&mut self, id: &TunnelId) -> Result<Response, Rejection> {
        self.refuse_lost(id)?;
        let now = self.clock.now();
        let t = self.tunnel_mut(id)?;
        // A tunnel that only looks down while a start of WGAL's may still
        // land is stopped all the same (follow-up review 2, finding 1).
        let down =
            t.known_down(now) && matches!(t.activity, Activity::Idle | Activity::Backoff { .. });
        self.change_resume(id, |rec| {
            rec.intent.desired = DesiredState::Disconnected;
            rec.external = None;
            rec.resume = (!down).then_some(ResumeOp::Disconnect);
            if let Some(incident) = &mut rec.incident {
                incident.next_retry_unix_ms = None;
                cancel_notification(incident);
                cancel_notice(&mut incident.exhausted_notice);
            }
            Ok(())
        })?;
        let phase = self.timing.control_phase_timeout;
        let t = self.tunnel_mut(id)?;
        t.control_error = None;
        t.stop_attempts = 0;
        t.stop_retry_at = None;
        t.reset_streak();
        let queue = match t.activity {
            Activity::Queued { .. } => true,
            Activity::Stopping {
                deadline,
                issued,
                next_poll,
                ..
            } => {
                t.activity = Activity::Stopping {
                    op: Op::Disconnect,
                    deadline,
                    issued,
                    next_poll,
                };
                false
            }
            Activity::Starting { .. } => {
                // This tunnel holds the control slot; stop next.
                t.activity = Activity::Stopping {
                    op: Op::Disconnect,
                    deadline: later(now, phase),
                    issued: false,
                    next_poll: now,
                };
                false
            }
            Activity::Verifying { .. } => true,
            Activity::Idle | Activity::Backoff { .. } => {
                t.activity = Activity::Idle;
                !down
            }
        };
        if queue {
            self.enqueue(id, Op::Disconnect, now);
        }
        Ok(Response::Accepted)
    }

    fn reconnect(&mut self, id: &TunnelId) -> Result<Response, Rejection> {
        self.refuse_lost(id)?;
        if self.tunnel_mut(id)?.activity.op().is_some() {
            return Err((
                ErrorCode::Busy,
                ErrorReason::OperationInProgress,
                "another operation on this tunnel is under way",
            ));
        }
        self.change_resume(id, |rec| {
            rec.intent.desired = DesiredState::Connected;
            rec.external = None;
            restart_retry_count(rec);
            rec.resume = Some(ResumeOp::Connect);
            Ok(())
        })?;
        let now = self.clock.now();
        let t = self.tunnel_mut(id)?;
        t.control_error = None;
        t.stop_attempts = 0;
        t.stop_retry_at = None;
        self.enqueue(id, Op::Reconnect, now);
        Ok(Response::Accepted)
    }

    /// The host-wide pause (plan §8). It is stored like any other intent; a
    /// tunnel is never started or stopped because of it, and a stop already
    /// sent is still followed by its start.
    /// Whether agents may use WGAL through `wgal-mcp` (owner decision of
    /// 2026-11-04). The runtime refuses every request from it while off.
    pub fn mcp_enabled(&self) -> bool {
        self.mcp_enabled
    }

    /// Whether agents may also read the chat ID and change the device name,
    /// chat ID and bot token (owner decision of 2026-11-06). Kept as it is
    /// while MCP is off, when it has no effect; the runtime refuses an
    /// agent's change to them while it is off.
    pub fn mcp_telegram_enabled(&self) -> bool {
        self.mcp_telegram_enabled
    }

    /// `SetMcpEnabled`: stored before it takes effect.
    fn set_mcp_enabled(&mut self, enabled: bool) -> Result<Response, Rejection> {
        self.set_switch(enabled, |w| &mut w.mcp_enabled)
    }

    /// `SetMcpTelegramEnabled`: stored before it takes effect.
    fn set_mcp_telegram_enabled(&mut self, enabled: bool) -> Result<Response, Rejection> {
        self.set_switch(enabled, |w| &mut w.mcp_telegram_enabled)
    }

    /// Turns a stored host-wide switch, storing it before it takes effect.
    fn set_switch(
        &mut self,
        enabled: bool,
        switch: fn(&mut Self) -> &mut bool,
    ) -> Result<Response, Rejection> {
        if enabled == *switch(self) {
            return Ok(Response::Accepted);
        }
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        *switch(self) = enabled;
        if self.persist().is_err() {
            *switch(self) = !enabled;
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the change could not be stored",
            ));
        }
        Ok(Response::Accepted)
    }

    fn set_monitoring_paused(&mut self, paused: bool) -> Result<Response, Rejection> {
        if paused == self.monitoring_paused {
            return Ok(Response::Accepted);
        }
        if !self.writable() {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "stored state could not be loaded",
            ));
        }
        let before: Vec<(TunnelId, TunnelRecord)> = self
            .tunnels
            .iter()
            .map(|(id, t)| (id.clone(), t.rec.clone()))
            .collect();
        self.monitoring_paused = paused;
        // No generation change and no pending item dropped: a stop or start
        // already handed to the platform must still have its result applied
        // (F/G review P3-2). A monitoring round still out is applied too, but
        // starts nothing automatic while paused (see `apply`).
        for t in self.tunnels.values_mut() {
            if paused && t.rec.resume == Some(ResumeOp::Recovery) {
                t.rec.resume = None;
            }
        }
        if self.persist().is_err() {
            self.monitoring_paused = !paused;
            for (id, rec) in before {
                if let Some(t) = self.tunnels.get_mut(&id) {
                    t.rec = rec;
                }
            }
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the change could not be stored",
            ));
        }
        let now = self.clock.now();
        let grace = self.timing.grace;
        let mut given_back = false;
        for t in self.tunnels.values_mut() {
            let was_monitoring = t.monitoring();
            t.paused = paused;
            given_back |= monitoring_changed(t, was_monitoring, now, grace);
        }
        if given_back {
            // Failure is recorded in `storage_ok`.
            let _ = self.persist();
        }
        Ok(Response::Accepted)
    }

    fn set_auto_recovery(&mut self, id: &TunnelId, enabled: bool) -> Result<Response, Rejection> {
        let caps = self.adapter.capabilities();
        self.change(id, |rec| {
            // The user's own choice replaces what an import asked for.
            rec.auto_recovery_after_baseline = None;
            if enabled {
                if rec.recovery_target.is_none() || !rec.baseline_passed {
                    return Err((
                        ErrorCode::InvalidState,
                        ErrorReason::BaselineNotPassed,
                        "the recovery target has not passed its baseline check",
                    ));
                }
                if caps.start_stop == Capability::Unsupported {
                    return Err((
                        ErrorCode::InvalidState,
                        ErrorReason::CannotControl,
                        "this tunnel cannot be controlled",
                    ));
                }
            } else if rec.external == Some(External::StoppedExternally) {
                rec.external = None;
            }
            rec.intent.auto_recovery_enabled = enabled;
            if !enabled {
                if let Some(incident) = &mut rec.incident {
                    incident.next_retry_unix_ms = None;
                }
                if rec.resume == Some(ResumeOp::Recovery) {
                    rec.resume = None;
                }
            }
            Ok(())
        })?;
        let t = self.tunnel_mut(id)?;
        if !enabled
            && matches!(
                t.activity,
                Activity::Backoff { .. }
                    | Activity::Queued {
                        op: Op::Recovery,
                        ..
                    }
            )
        {
            // Only automatic actions that have not started are cancelled; a
            // stop already sent is still followed by its start.
            t.activity = Activity::Idle;
        }
        Ok(Response::Accepted)
    }

    /// The snapshot the GUI renders (plan §6.1).
    pub fn snapshot(&mut self) -> Snapshot {
        self.refresh_lights();
        let now = self.clock.now();
        let wall = self.clock.wall_unix_ms();
        let shown = self.shown_clock(now, wall);
        let caps = self.adapter.capabilities();
        let storage_ok = self.storage_health() == StorageHealth::Ok;
        let tunnels: Vec<TunnelStatus> = self
            .tunnels
            .values()
            .take(MAX_TUNNELS)
            .map(|t| status(t, now, wall, shown, &self.timing, storage_ok, &caps))
            .collect();
        Snapshot {
            generated_at_unix_ms: wall,
            storage: self.storage_health(),
            capabilities: caps,
            monitoring_paused: self.monitoring_paused,
            // Filled in by the runtime, which applies the host settings.
            host: unapplied_host_status(&self.host),
            // Filled in by the runtime's notification service.
            notification: NotificationStatus::default(),
            tunnels: BoundedVec::try_from(tunnels).unwrap_or_default(),
        }
    }

    /// The clock pair a snapshot writes wall times with: the one the last
    /// snapshot used, while the wall clock still agrees with it within
    /// [`SHOWN_CLOCK_TOLERANCE_MS`], and otherwise, the wall clock having
    /// been set or drifted further, the current one. The two clocks are read
    /// one after the other and each rounded down to a millisecond, so a pair
    /// taken afresh for every snapshot made a time it reports
    /// (`stable_since`, `last_verified`, a deadline) move by a millisecond
    /// from one snapshot to the next (day 2, 2026-11-05, P3).
    fn shown_clock(&mut self, now: MonoInstant, wall: i64) -> (MonoInstant, i64) {
        if let Some((at, at_wall)) = self.shown_clock
            && wall_at(at, at_wall, now).abs_diff(wall) <= SHOWN_CLOCK_TOLERANCE_MS
        {
            return (at, at_wall);
        }
        self.shown_clock = Some((now, wall));
        (now, wall)
    }

    /// Messages that may be sent now, each tunnel's in the order they should
    /// go: its alert, the message that a limited retry count is used up,
    /// and the one that the last incident automatic recovery worked on is
    /// over (owner decision of 2026-11-04). An alert or a used-up count is
    /// due while pending, the tunnel still wanted connected with its own
    /// notifications on, the fault seen to go on in this process (never just
    /// state restored from before a restart; the used-up count is itself
    /// such a sign) and not in a stable-recovery observation (a short success
    /// only defers it). A `recovered` message is due while pending and the
    /// tunnel's notifications are on. Nothing is due while storage fails.
    pub fn notifications_due(&self) -> Vec<DueNotification> {
        if self.storage_health() != StorageHealth::Ok {
            return Vec::new();
        }
        let mut due = Vec::new();
        for t in self.tunnels.values() {
            if !t.rec.notifications_enabled {
                continue;
            }
            let base = |kind, incident: &IncidentRecord| DueNotification {
                kind,
                tunnel_id: t.rec.tunnel_id.clone(),
                incident_id: incident.incident_id.clone(),
                display_name: t.rec.display_name.clone(),
                opened_at_unix_ms: incident.opened_at_unix_ms,
                first_recovery: incident.first_recovery,
                retry: t.rec.retry,
                retries_used: retries_used(incident),
                retries_exhausted: incident.retries_exhausted,
                recovered_at_unix_ms: None,
            };
            let ongoing = t.rec.intent.desired == DesiredState::Connected
                && t.monitoring()
                && t.stable_since.is_none();
            if ongoing && let Some(incident) = &t.rec.incident {
                if t.fault_seen && incident.notification == NotificationState::Pending {
                    due.push(base(NoticeKind::Alert, incident));
                }
                if incident.exhausted_notice.state == NotificationState::Pending {
                    due.push(base(NoticeKind::RetriesExhausted, incident));
                }
            }
            for n in t
                .rec
                .recovered_notices
                .iter()
                .filter(|n| n.notice.state == NotificationState::Pending)
            {
                due.push(DueNotification {
                    kind: NoticeKind::Recovered,
                    tunnel_id: t.rec.tunnel_id.clone(),
                    incident_id: n.incident_id.clone(),
                    display_name: t.rec.display_name.clone(),
                    opened_at_unix_ms: n.opened_at_unix_ms,
                    first_recovery: FirstRecovery::NotAttempted,
                    retry: t.rec.retry,
                    retries_used: 0,
                    retries_exhausted: false,
                    recovered_at_unix_ms: Some(n.recovered_at_unix_ms),
                });
            }
        }
        due
    }

    /// Marks the message `reserved` and persists that before returning, so it
    /// is never sent twice across a crash (plan §7.3). Send only after this
    /// succeeds.
    pub fn reserve_notification(
        &mut self,
        tunnel_id: &TunnelId,
        incident_id: &IncidentId,
        kind: NoticeKind,
    ) -> Result<NotificationTicket, NotificationError> {
        if !self.notifications_due().iter().any(|due| {
            &due.tunnel_id == tunnel_id && &due.incident_id == incident_id && due.kind == kind
        }) {
            let (state, _) = self.notice(tunnel_id, incident_id, kind)?;
            return Err(NotificationError::WrongState(state));
        }
        let ticket = NotificationTicket {
            tunnel_id: tunnel_id.clone(),
            incident_id: incident_id.clone(),
            kind,
        };
        self.set_notification(&ticket, NotificationState::Reserved, None)?;
        Ok(ticket)
    }

    /// Records what happened to a reserved message.
    pub fn record_notification(
        &mut self,
        ticket: &NotificationTicket,
        outcome: NotificationOutcome,
    ) -> Result<(), NotificationError> {
        let (state, cancelled) =
            self.notice(&ticket.tunnel_id, &ticket.incident_id, ticket.kind)?;
        if state != NotificationState::Reserved {
            return Err(NotificationError::WrongState(state));
        }
        let (next, message_id) = match outcome {
            NotificationOutcome::Sent { message_id } => (NotificationState::Sent, Some(message_id)),
            // Cancelled while it was in flight: never eligible again.
            NotificationOutcome::RetryLater if cancelled => (NotificationState::Cancelled, None),
            NotificationOutcome::RetryLater => (NotificationState::Pending, None),
            NotificationOutcome::Rejected => (NotificationState::Rejected, None),
            NotificationOutcome::Unknown => (NotificationState::Unknown, None),
        };
        self.set_notification(ticket, next, message_id)
    }

    /// Cancels every pending message, for when notifications are turned
    /// off. One already in flight keeps its result, but is never eligible
    /// again.
    pub fn cancel_pending_notifications(&mut self) -> Result<(), StoreError> {
        let mut changed = false;
        for t in self.tunnels.values_mut() {
            changed |= cancel_notices(&mut t.rec);
        }
        if changed { self.persist() } else { Ok(()) }
    }

    /// The state of one of a tunnel's messages and whether it was cancelled
    /// in flight.
    fn notice(
        &self,
        tunnel_id: &TunnelId,
        incident_id: &IncidentId,
        kind: NoticeKind,
    ) -> Result<(NotificationState, bool), NotificationError> {
        let rec = &self
            .tunnels
            .get(tunnel_id)
            .ok_or(NotificationError::NoSuchIncident)?
            .rec;
        let incident = rec
            .incident
            .as_ref()
            .filter(|incident| &incident.incident_id == incident_id);
        match kind {
            NoticeKind::Alert => incident.map(|i| (i.notification, i.cancel_requested)),
            NoticeKind::RetriesExhausted => incident.map(|i| {
                (
                    i.exhausted_notice.state,
                    i.exhausted_notice.cancel_requested,
                )
            }),
            NoticeKind::Recovered => rec
                .recovered_notices
                .iter()
                .find(|n| &n.incident_id == incident_id)
                .map(|n| (n.notice.state, n.notice.cancel_requested)),
        }
        .ok_or(NotificationError::NoSuchIncident)
    }

    fn set_notification(
        &mut self,
        ticket: &NotificationTicket,
        state: NotificationState,
        message_id: Option<i64>,
    ) -> Result<(), NotificationError> {
        self.notice(&ticket.tunnel_id, &ticket.incident_id, ticket.kind)?;
        let t = self
            .tunnels
            .get_mut(&ticket.tunnel_id)
            .ok_or(NotificationError::NoSuchIncident)?;
        let before = t.rec.clone();
        let set = |notice: &mut NoticeRecord| {
            notice.state = state;
            if state != NotificationState::Reserved {
                notice.cancel_requested = false;
            }
            if message_id.is_some() {
                notice.message_id = message_id;
            }
        };
        match ticket.kind {
            NoticeKind::Alert => {
                if let Some(incident) = &mut t.rec.incident {
                    incident.notification = state;
                    if state != NotificationState::Reserved {
                        incident.cancel_requested = false;
                    }
                    if message_id.is_some() {
                        incident.notification_message_id = message_id;
                    }
                }
            }
            NoticeKind::RetriesExhausted => {
                if let Some(incident) = &mut t.rec.incident {
                    set(&mut incident.exhausted_notice);
                }
            }
            NoticeKind::Recovered => {
                if let Some(n) = t
                    .rec
                    .recovered_notices
                    .iter_mut()
                    .find(|n| n.incident_id == ticket.incident_id)
                {
                    set(&mut n.notice);
                }
            }
        }
        if let Err(e) = self.persist() {
            // A reservation that is not durable must not be used. Other
            // results stay in memory: after a restart the stored `reserved`
            // becomes `unknown`, which never resends.
            if state == NotificationState::Reserved
                && let Some(t) = self.tunnels.get_mut(&ticket.tunnel_id)
            {
                t.rec = before;
            }
            return Err(NotificationError::Storage(e));
        }
        Ok(())
    }
}

/// A new event-log label, not in `taken`: `t-` and 8 hex digits from std's
/// per-process random hash keys, which the OS seeds. Nothing in it comes from
/// the tunnel, so it reveals nothing about it.
fn fresh_label(taken: &BTreeSet<String>) -> String {
    let mut label = String::new();
    for attempt in 0_u32..64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u32(attempt);
        hasher.write_usize(taken.len());
        let bytes = hasher.finish().to_le_bytes();
        let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        label = format!("t-{value:08x}");
        if !taken.contains(&label) {
            break;
        }
    }
    label
}

/// Finding-3 rule for state restored from a backup: nothing automatic may
/// act on it, and no notification that might already have gone out is sent.
fn distrust(rec: &mut TunnelRecord) {
    rec.intent.auto_recovery_enabled = false;
    rec.intent.start_when_stopped_externally = false;
    rec.resume = None;
    let unsettled = |state| {
        matches!(
            state,
            NotificationState::NotEligible
                | NotificationState::Pending
                | NotificationState::Reserved
        )
    };
    if let Some(incident) = &mut rec.incident {
        incident.next_retry_unix_ms = None;
        incident.cancel_requested = false;
        incident.resume_uncounted = false;
        if unsettled(incident.notification) {
            incident.notification = NotificationState::Unknown;
        }
        // So may the messages added in 1.2.0.
        let exhausted = &mut incident.exhausted_notice;
        exhausted.cancel_requested = false;
        if unsettled(exhausted.state) {
            exhausted.state = NotificationState::Unknown;
        }
    }
    for recovered in &mut rec.recovered_notices {
        recovered.notice.cancel_requested = false;
        if unsettled(recovered.notice.state) {
            recovered.notice.state = NotificationState::Unknown;
        }
    }
}

/// Adds an event to the queue for [`Watchdog::take_events`], dropping the
/// oldest one when it is full.
fn queue_event(
    events: &mut VecDeque<(Option<TunnelId>, LogEvent)>,
    lost: &mut u32,
    tunnel: Option<TunnelId>,
    event: LogEvent,
) {
    if events.len() >= MAX_EVENTS {
        events.pop_front();
        *lost = lost.saturating_add(1);
    }
    events.push_back((tunnel, event));
}

/// The incident's first automatic round failed, so its notification becomes
/// due (plan §7.2). With the tunnel's own notifications off it is cancelled
/// at once instead, as an alert that becomes due while notifications are off
/// globally is: it is never sent later.
fn make_eligible(incident: &mut IncidentRecord, notifications_enabled: bool) {
    if incident.notification == NotificationState::NotEligible {
        incident.notification = if notifications_enabled {
            NotificationState::Pending
        } else {
            NotificationState::Cancelled
        };
    }
}

/// The wait before the next automatic round (owner decision of 2026-11-04:
/// the tunnel's own interval, which replaces plan §6.3's backoff).
fn retry_wait(retry: RecoveryRetry) -> Duration {
    Duration::from_secs(u64::from(retry.interval_secs()))
}

/// Rounds after the first that count against the tunnel's retry limit.
fn retries_used(incident: &IncidentRecord) -> u32 {
    incident
        .backoff_step
        .saturating_sub(incident.counted_from_step)
}

/// Whether one more automatic round may follow in the incident.
fn retry_left(retry: RecoveryRetry, incident: &IncidentRecord) -> bool {
    !incident.retries_exhausted && retry.allows(retries_used(incident))
}

/// The retry limit is used up: no automatic round follows in this incident,
/// nothing waits for one, and the log hears of it once.
fn exhaust_retries(t: &mut Tunnel) {
    let Some(incident) = &mut t.rec.incident else {
        return;
    };
    if incident.retries_exhausted {
        return;
    }
    incident.retries_exhausted = true;
    incident.next_retry_unix_ms = None;
    let retries = retries_used(incident);
    // A count of 0 is said by the alert itself (owner decision of
    // 2026-11-04); a tunnel whose notifications are off never sends it.
    if retries > 0 && incident.exhausted_notice.state == NotificationState::NotEligible {
        incident.exhausted_notice.state = if t.rec.notifications_enabled {
            NotificationState::Pending
        } else {
            NotificationState::Cancelled
        };
    }
    if matches!(t.activity, Activity::Backoff { .. }) {
        t.activity = Activity::Idle;
    }
    t.note(LogEvent::RetriesExhausted { retries });
}

/// The automatic round queued for the tunnel is withdrawn before it sent
/// anything: if it was counted against the retry limit, the count is given
/// back, so a retry that never ran uses nothing up (Codex P2-N18). When its
/// `resume` record stays, starting the same round from it counts it again
/// (`IncidentRecord::resume_uncounted`). Returns whether the record
/// changed.
fn give_back_round(t: &mut Tunnel) -> bool {
    if !std::mem::take(&mut t.round_counted) {
        return false;
    }
    let resumes = t.rec.resume == Some(ResumeOp::Recovery);
    let Some(incident) = &mut t.rec.incident else {
        return false;
    };
    if incident.backoff_step <= incident.counted_from_step {
        return false;
    }
    incident.backoff_step -= 1;
    // Stored with the count, so a restart counts the resumed round too
    // (Codex P2-N29).
    incident.resume_uncounted = resumes;
    true
}

/// A manual connect or reconnect starts the incident's retry count again,
/// and if it brings the tunnel back, that recovery is the user's.
fn restart_retry_count(rec: &mut TunnelRecord) {
    if let Some(incident) = &mut rec.incident {
        incident.counted_from_step = incident.backoff_step;
        incident.retries_exhausted = false;
        incident.manual_last = true;
    }
}

/// A probe of the recovery target found this host without a network of its
/// own. The log hears of it once per outage.
fn mark_offline(t: &mut Tunnel, now: MonoInstant) {
    t.local_offline = true;
    t.offline_seen_at = Some(now);
    t.host_vouch = true;
    if !t.offline_noted {
        t.offline_noted = true;
        t.note(LogEvent::LocalNetworkOffline);
    }
}

/// A probe of the recovery target got through: the host has its network.
/// The log hears of it if it had heard of an outage.
fn back_online(t: &mut Tunnel) {
    t.local_offline = false;
    t.host_vouch = false;
    if t.offline_noted {
        t.offline_noted = false;
        t.note(LogEvent::LocalNetworkOnline);
    }
}

/// Cancels one of the messages added in 1.2.0 as [`cancel_notification`]
/// does the alert. Returns whether anything changed.
fn cancel_notice(notice: &mut NoticeRecord) -> bool {
    match notice.state {
        NotificationState::Pending => {
            notice.state = NotificationState::Cancelled;
            true
        }
        NotificationState::Reserved if !notice.cancel_requested => {
            notice.cancel_requested = true;
            true
        }
        _ => false,
    }
}

/// Cancels every message the tunnel still has to send. Returns whether
/// anything changed.
fn cancel_notices(rec: &mut TunnelRecord) -> bool {
    let mut changed = false;
    if let Some(incident) = &mut rec.incident {
        changed |= cancel_notification(incident);
        changed |= cancel_notice(&mut incident.exhausted_notice);
    }
    for recovered in &mut rec.recovered_notices {
        changed |= cancel_notice(&mut recovered.notice);
    }
    changed
}

/// Adds the `recovered` message of an incident that just closed. Messages
/// that are settled (sent, rejected, unknown, cancelled) are dropped first;
/// those still to be sent or in flight stay (Codex P2-N20). When
/// [`MAX_RECOVERED_NOTICES`] are still waiting, Telegram has been out of
/// reach for that many recoveries: the oldest one not in flight is
/// cancelled to make room, as an unsent alert is when its incident closes
/// (plan §7.2), and the log says so (`notification_cancelled`, notice
/// `recovered`; Codex P2-N31). The list never holds more.
fn queue_recovered(t: &mut Tunnel, notice: RecoveredNotice) {
    let list = &mut t.rec.recovered_notices;
    list.retain(|n| {
        matches!(
            n.notice.state,
            NotificationState::Pending | NotificationState::Reserved
        )
    });
    let mut cancelled = Vec::new();
    while list.len() >= MAX_RECOVERED_NOTICES {
        let Some(oldest) = list
            .iter()
            .position(|n| n.notice.state != NotificationState::Reserved)
        else {
            break;
        };
        cancelled.push(list.remove(oldest).incident_id);
    }
    if list.len() < MAX_RECOVERED_NOTICES {
        list.push(notice);
    } else {
        // Every one is in flight: the new one makes no room.
        cancelled.push(notice.incident_id);
    }
    for incident_id in cancelled {
        t.note(LogEvent::NotificationCancelled {
            incident_id,
            notice: NoticeKind::Recovered,
        });
    }
}

/// Cancels the incident's notification: a pending one at once, one in flight
/// once its result comes back. Returns whether anything changed.
fn cancel_notification(incident: &mut IncidentRecord) -> bool {
    match incident.notification {
        NotificationState::Pending => {
            incident.notification = NotificationState::Cancelled;
            true
        }
        NotificationState::Reserved if !incident.cancel_requested => {
            incident.cancel_requested = true;
            true
        }
        _ => false,
    }
}

/// The tunnel's operation has had no work planned for longer than
/// `Timing::stall_after`: the host slept or the process stalled.
fn stalled(t: &Tunnel, now: MonoInstant, timing: &Timing) -> bool {
    let next = match t.activity {
        Activity::Verifying { next_round, .. } => next_round,
        Activity::Stopping { next_poll, .. } | Activity::Starting { next_poll, .. } => next_poll,
        _ => return false,
    };
    now > later(next, timing.stall_after)
}

/// The tunnel's next round, monitoring or verification, is due at `now` at
/// the latest.
fn round_due(t: &mut Tunnel, now: MonoInstant) {
    match &mut t.activity {
        Activity::Verifying { next_round, .. } => *next_round = (*next_round).min(now),
        Activity::Idle | Activity::Backoff { .. } => t.next_round = t.next_round.min(now),
        Activity::Queued { .. } | Activity::Stopping { .. } | Activity::Starting { .. } => {}
    }
}

/// After a wake or a stall, the deadlines of an operation under way restart
/// from now, so time spent asleep never fails it (plan §6.2, T15).
fn rebase_deadlines(t: &mut Tunnel, now: MonoInstant, timing: &Timing) {
    let phase = later(now, timing.control_phase_timeout);
    t.activity = match t.activity {
        Activity::Verifying { op, deadline, .. } => Activity::Verifying {
            op,
            deadline: deadline.max(later(now, timing.verify_window)),
            next_round: now,
        },
        Activity::Stopping {
            op,
            deadline,
            issued,
            ..
        } => Activity::Stopping {
            op,
            deadline: deadline.max(phase),
            issued,
            next_poll: now,
        },
        Activity::Starting {
            op,
            deadline,
            issued,
            ..
        } => Activity::Starting {
            op,
            deadline: deadline.max(phase),
            issued,
            next_poll: now,
        },
        other => other,
    };
}

/// The snapshot's host part as far as the core knows it: the stored choices
/// and nothing worked out from them. The runtime, which applies the host
/// settings, replaces it before the snapshot leaves the watchdog.
fn unapplied_host_status(host: &HostSettings) -> HostStatus {
    HostStatus {
        watchdog_version: ProductVersion::current(),
        min_gui_version: ProductVersion::min_gui(),
        min_usable_gui_version: Some(ProductVersion::min_usable_gui()),
        mcp_enabled: false,
        mcp_telegram_enabled: false,
        workers: WorkerStatus {
            configured: host.workers,
            suggested: MIN_WORKERS,
            limit: MIN_WORKERS,
            effective: MIN_WORKERS,
            running: 0,
        },
        time_zone: TimeZoneStatus {
            configured: host.time_zone.clone(),
            effective: None,
            utc_offset_secs: 0,
        },
    }
}

fn takeover(rec: &TunnelRecord) -> Takeover {
    if rec.binding_lost {
        Takeover::BindingLost
    } else {
        match rec.external {
            Some(External::StoppedExternally) => Takeover::StoppedExternally,
            Some(External::ExternallyControlled) => Takeover::ExternallyControlled,
            None if rec.intent.auto_recovery_enabled => Takeover::Managed,
            None => Takeover::ObserveOnly,
        }
    }
}

/// A tunnel's status at `now` and `wall`; its instants are written as wall
/// times with the pair `shown` ([`Watchdog::shown_clock`]).
fn status(
    t: &Tunnel,
    now: MonoInstant,
    wall: i64,
    shown: (MonoInstant, i64),
    timing: &Timing,
    storage_ok: bool,
    caps: &AdapterCapabilities,
) -> TunnelStatus {
    let view = t.light(now, timing);
    let to_wall = |at| wall_at(shown.0, shown.1, at);
    TunnelStatus {
        tunnel_id: t.rec.tunnel_id.clone(),
        display_name: t.rec.display_name.clone(),
        light: view.light,
        light_reason: view.reason,
        light_since_unix_ms: t.light_since_unix_ms,
        deadline_unix_ms: view.deadline.map(to_wall),
        // Only with a red light, as the contract says.
        next_retry_unix_ms: view.next_retry.map(to_wall),
        last_verified_unix_ms: t.last_success.map(to_wall),
        intent: t.rec.intent,
        // The same checks as `set_auto_recovery`.
        auto_recovery_allowed: t.rec.recovery_target.is_some()
            && t.rec.baseline_passed
            && !t.rec.binding_lost
            && caps.start_stop != Capability::Unsupported,
        notifications_enabled: t.rec.notifications_enabled,
        takeover: takeover(&t.rec),
        os_state: if t.os_error.is_some() {
            OsState::Unknown
        } else {
            t.os
        },
        data_health: t.data_health(now),
        recovery: t.recovery_phase(now, storage_ok),
        recovery_target: t.recovery_view.clone(),
        observe_targets: BoundedVec::try_from(t.observe_views.clone()).unwrap_or_default(),
        probe: t.rec.probe,
        freshness_secs: t.freshness_secs(),
        consecutive_failures: t.failures,
        fault: t.fault(),
        incident: t.rec.incident.as_ref().map(|incident| IncidentStatus {
            incident_id: incident.incident_id.clone(),
            opened_at_unix_ms: incident.opened_at_unix_ms,
            first_recovery: incident.first_recovery,
            backoff_step: incident.backoff_step,
            stable_since_unix_ms: t.stable_since.map(to_wall),
            notification: incident.notification,
            retries_used: retries_used(incident),
            retries_exhausted: incident.retries_exhausted,
            exhausted_notification: incident.exhausted_notice.state,
        }),
        retry: t.rec.retry,
        // The newest one: the message the last closed incident is due.
        recovered_notification: t
            .rec
            .recovered_notices
            .last()
            .map(|n| RecoveredNotification {
                incident_id: n.incident_id.clone(),
                notification: n.notice.state,
            }),
        auto_recovery_pending: t
            .rec
            .auto_recovery_after_baseline
            .is_some_and(|p| wall <= p.until_unix_ms),
    }
}

/// Monitoring of the tunnel was turned on or off, by its own setting or the
/// host-wide pause. Off cancels automatic actions that have sent nothing yet
/// (see [`withdraw_unmonitored`]; a stop already sent is still followed by
/// its start); on again counts nothing from before and starts with a grace
/// period.
fn monitoring_changed(
    t: &mut Tunnel,
    was_monitoring: bool,
    now: MonoInstant,
    grace: Duration,
) -> bool {
    let mut given_back = false;
    if t.monitoring() {
        // Monitored again: a check asked for while it was not is answered by
        // its rounds, at once, even if no plan ran in between (requests are
        // handled in batches; Codex P2).
        t.one_off_at = None;
    }
    if t.monitoring() != was_monitoring {
        // An alert waits until the fault is seen again with monitoring on
        // (plan §7.3: still failing now; F/G review P3-1).
        t.fault_seen = false;
    }
    if !t.monitoring() {
        // A tunnel WGAL's own recovery left down keeps its backoff: nothing
        // runs while monitoring is off, but once it is on again the retry is
        // still due and starts it (plan §6.3). A connected tunnel's backoff is
        // dropped: its fault has to be confirmed again (re-review of d5ced49,
        // N1).
        // Only a tunnel seen connected, without a read error, has a fault
        // that can be confirmed again; any other one might be down and keeps
        // its retry (re-review of 4b843b6, N1b).
        let seen_up = t.os == OsState::Connected && t.os_error.is_none();
        if seen_up && matches!(t.activity, Activity::Backoff { .. }) {
            t.activity = Activity::Idle;
        }
        given_back = withdraw_unmonitored(t, now);
    } else if !was_monitoring {
        t.reset_streak();
        t.forget_offline();
        t.grace_until = later(now, grace);
        t.next_round = now;
    }
    given_back
}

/// Monitoring of the tunnel is off: an automatic operation that has sent
/// nothing to the platform yet (`Tunnel::unsent_automatic`) is withdrawn, so
/// it can neither take the control slot nor send its stop or start once the
/// slot is free (Codex review of wave three, P1-1). One whose stop or start
/// went out finishes, so WGAL never leaves down a tunnel it stopped.
///
/// A recovery round for a tunnel that may be down goes back to a backoff
/// that is due at once, so it runs when monitoring is on again (re-review of
/// d5ced49, N1a); a connected tunnel's fault has to be confirmed again. A
/// start after an external stop leaves that stop unanswered: the tunnel's
/// last settled state is connected again, so the first round with
/// monitoring on judges the stop as one made while monitoring was off, and
/// starts the tunnel only if the user's settings still say so.
fn withdraw_unmonitored(t: &mut Tunnel, now: MonoInstant) -> bool {
    let Some(op) = t.unsent_automatic() else {
        return false;
    };
    let given_back = give_back_round(t);
    t.activity = if op == Op::Recovery {
        withdrawn_recovery(t, now)
    } else {
        Activity::Idle
    };
    if op == Op::ExternalRestart {
        t.settled_os = Some(OsState::Connected);
    }
    given_back
}

/// A recovery round of a connected tunnel was withdrawn before it sent
/// anything: there is nothing to finish, so its resume record goes, and the
/// tunnel's last settled state is connected again, so a stop made outside
/// WGAL from now on is judged as one (plan §6.4; Codex P2-N48). Returns
/// whether the record changed.
fn forget_unsent_recovery(t: &mut Tunnel) -> bool {
    let changed = t.rec.resume == Some(ResumeOp::Recovery);
    if changed {
        t.rec.resume = None;
        t.resume_until = None;
    }
    if let Some(incident) = &mut t.rec.incident {
        incident.resume_uncounted = false;
    }
    if t.os == OsState::Connected && t.os_error.is_none() {
        t.settled_os = Some(OsState::Connected);
    }
    changed
}

/// What a recovery round withdrawn before it sent anything leaves the
/// tunnel doing: one that may be down keeps a backoff due at once, so the
/// round runs as soon as nothing holds it back and a pause does not drop
/// it; a connected one's fault has to be confirmed again.
fn withdrawn_recovery(t: &Tunnel, now: MonoInstant) -> Activity {
    let seen_up = t.os == OsState::Connected && t.os_error.is_none();
    if seen_up {
        Activity::Idle
    } else {
        Activity::Backoff { retry_at: now }
    }
}

fn lose_binding(t: &mut Tunnel, error: AdapterError) {
    // Never reported as disconnected, never recreated (plan §4.2).
    t.rec.binding_lost = true;
    t.rec.resume = None;
    t.activity = Activity::Idle;
    t.os = OsState::Unknown;
    t.os_error = Some(error);
    t.reset_streak();
}

fn apply_command(t: &mut Tunnel, result: Result<(), AdapterError>, ctx: &Ctx<'_>) {
    // Whatever the platform answered, the request may still take effect for
    // a while after it answered (follow-up review 2).
    let until = later(ctx.now, ctx.timing.late_effect());
    match t.activity {
        Activity::Stopping { issued: false, .. } => may_land(&mut t.late_stop_until, until),
        Activity::Starting { issued: false, .. } => may_land(&mut t.late_start_until, until),
        _ => {}
    }
    t.sent = true;
    match (t.activity, result) {
        (
            Activity::Stopping {
                op,
                deadline,
                issued: false,
                ..
            },
            Ok(()),
        ) => {
            t.activity = Activity::Stopping {
                op,
                deadline,
                issued: true,
                next_poll: ctx.now,
            };
        }
        (
            Activity::Starting {
                op,
                deadline,
                issued: false,
                ..
            },
            Ok(()),
        ) => {
            t.activity = Activity::Starting {
                op,
                deadline,
                issued: true,
                next_poll: ctx.now,
            };
        }
        (Activity::Stopping { .. } | Activity::Starting { .. }, Err(error))
            if error.kind == AdapterErrorKind::NotFound =>
        {
            lose_binding(t, error);
        }
        (Activity::Stopping { op, .. } | Activity::Starting { op, .. }, Err(error)) => {
            t.note(LogEvent::ControlFailed { error: error.kind });
            op_failed(t, op, error, false, ctx);
        }
        _ => {}
    }
}

/// Records an OS state read. Returns `false` if the binding is gone.
fn read_os(t: &mut Tunnel, result: Result<OsState, AdapterError>, ctx: &Ctx<'_>) -> bool {
    match result {
        Ok(state) => {
            t.set_os(state, ctx.now);
            true
        }
        Err(error) if error.kind == AdapterErrorKind::NotFound => {
            lose_binding(t, error);
            false
        }
        Err(error) => {
            // A failed read is never taken as "disconnected" (plan §4.1).
            t.os_error = Some(error);
            t.stable_since = None;
            true
        }
    }
}

fn apply_poll(t: &mut Tunnel, result: Result<OsState, AdapterError>, ctx: &Ctx<'_>) {
    if !read_os(t, result, ctx) {
        return;
    }
    let settled = |want| t.os == want && t.os_error.is_none();
    match t.activity {
        Activity::Stopping {
            op,
            deadline,
            issued: true,
            ..
        } => {
            if settled(OsState::Disconnected) {
                if op.starts() {
                    t.activity = Activity::Starting {
                        op,
                        deadline: later(ctx.now, ctx.timing.control_phase_timeout),
                        issued: false,
                        next_poll: ctx.now,
                    };
                } else {
                    t.activity = Activity::Idle;
                    t.next_round = later(ctx.now, ctx.interval());
                    t.settled_os = Some(OsState::Disconnected);
                    disconnect_done(t, ctx.now);
                }
            } else if ctx.now >= deadline {
                t.note(LogEvent::ControlFailed {
                    error: AdapterErrorKind::Timeout,
                });
                op_failed(t, op, timeout("the tunnel did not stop in time"), true, ctx);
            } else {
                t.activity = Activity::Stopping {
                    op,
                    deadline,
                    issued: true,
                    next_poll: later(ctx.now, ctx.timing.control_poll_interval),
                };
            }
        }
        Activity::Starting {
            op,
            deadline,
            issued: true,
            ..
        } => {
            if settled(OsState::Connected) {
                // The control slot is released here: verification can take
                // 45 s and must not hold up other tunnels (plan §5.3).
                t.verify_saw_no_reply = false;
                t.verify_saw_offline = false;
                t.activity = Activity::Verifying {
                    op,
                    deadline: later(ctx.now, ctx.timing.verify_window),
                    next_round: ctx.now,
                };
            } else if ctx.now >= deadline {
                t.note(LogEvent::ControlFailed {
                    error: AdapterErrorKind::Timeout,
                });
                op_failed(
                    t,
                    op,
                    timeout("the tunnel did not start in time"),
                    true,
                    ctx,
                );
            } else {
                t.activity = Activity::Starting {
                    op,
                    deadline,
                    issued: true,
                    next_poll: later(ctx.now, ctx.timing.control_poll_interval),
                };
            }
        }
        _ => {}
    }
}

/// The result of the one round a check asked for while the tunnel is not
/// monitored (owner decision of 2026-11-06): it only updates what each
/// target last answered. The state reading and every answer are no
/// evidence here: no streak, fault, light, path or operation follows from
/// them, nothing is stored, and the light stays the unmonitored one.
fn apply_one_off(t: &mut Tunnel, probes: Option<ProbeSet>) {
    if let Some(set) = probes
        && !set.stale
        && set.interface.is_ok()
    {
        update_views(t, &set);
    }
}

fn update_views(t: &mut Tunnel, set: &ProbeSet) {
    for probe in &set.probes {
        let result = probe
            .result
            .as_ref()
            .map(|(_, outcome)| *outcome)
            .map_err(|e| e.kind);
        let last = Some(summary(
            probe.started_at_unix_ms,
            probe.finished_at_unix_ms,
            &result,
        ));
        let view = match probe.role {
            TargetRole::Recovery => t.recovery_view.as_mut(),
            TargetRole::Observe(i) => t.observe_views.get_mut(i),
        };
        if let Some(view) = view
            && view.target == probe.target
        {
            view.last_probe = last;
        }
    }
}

/// A verification round after a start (plan §6.3): success needs the
/// interface resolved anew and a bound, route-checked answer from the recovery
/// target; neither the start command nor `Connected` is enough. A refused TCP
/// connection is an answer through the tunnel and verifies the path too.
fn apply_verify(
    t: &mut Tunnel,
    os: Result<OsState, AdapterError>,
    probes: Option<ProbeSet>,
    finished: MonoInstant,
    op: Op,
    deadline: MonoInstant,
    ctx: &Ctx<'_>,
) {
    if !read_os(t, os, ctx) {
        return;
    }
    let connected = t.os == OsState::Connected && t.os_error.is_none();
    if t.os == OsState::Disconnected && t.os_error.is_none() && fresh(finished, ctx) {
        // The restarted tunnel went down again during WGAL's own recovery:
        // the round failed (plan §6.4: not an external stop while WGAL is
        // recovering it; re-review NF-1). A manual stop in the official app
        // within the 45 s window is counted the same way.
        t.verify_saw_no_reply = true;
    }
    let mut verified = connected && t.rec.recovery_target.is_none();
    let mut answer = None;
    if let Some(set) = &probes
        && !set.stale
        && connected
        && fresh(finished, ctx)
        && matches!(&set.interface, Err(e) if e.kind == AdapterErrorKind::NotFound)
    {
        // Connected according to the manager, yet no interface: counted as a
        // failure in monitoring, so it is evidence here too (re-review NF-2).
        t.verify_saw_no_reply = true;
    }
    if let Some(set) = probes.filter(|set| !set.stale) {
        update_views(t, &set);
        answer = set.probes.iter().find_map(|p| match p.result {
            Ok((_, outcome @ (ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. })))
                if p.role == TargetRole::Recovery
                    && can_bind(&ctx.caps, &p.target)
                    && fresh(p.finished, ctx) =>
            {
                Some((outcome, p.finished))
            }
            _ => None,
        });
        verified |= answer.is_some();
        for p in set
            .probes
            .iter()
            .filter(|p| p.role == TargetRole::Recovery && fresh(p.finished, ctx))
        {
            match &p.result {
                Ok((_, ProbeOutcome::NoReply)) if can_bind(&ctx.caps, &p.target) => {
                    t.verify_saw_no_reply = true;
                }
                // What the verification could not check is recorded as for a
                // monitoring round, so it also blocks the next automatic round
                // until a normal round clears it (plan §4.1, §6.3).
                Err(error) if error.kind == AdapterErrorKind::LocalNetworkOffline => {
                    mark_offline(t, p.finished);
                    t.verify_saw_offline = true;
                }
                Err(error)
                    if !matches!(
                        error.kind,
                        AdapterErrorKind::NotFound | AdapterErrorKind::RouteMismatch
                    ) =>
                {
                    t.probe_error = Some(error.clone());
                }
                _ => {}
            }
        }
    }
    if verified {
        if let Some((outcome, at)) = answer {
            t.path = Path::Verified;
            t.probe_error = None;
            t.route_error = None;
            record_success(t, outcome, at, ctx);
        }
        op_succeeded(t, op, ctx);
    } else if ctx.now >= deadline && op == Op::Recovery && !t.verify_saw_no_reply {
        // Only errors, offline or stale answers: the restart was carried out
        // but nothing showed whether traffic flows. That is no failed
        // recovery and makes no alert due (plan §4.1, §7.2; review of
        // `121b42a`, P2-1).
        let error = timeout("traffic through the tunnel could not be checked after the restart");
        op_inconclusive(t, op, error, ctx);
    } else if ctx.now >= deadline {
        let error = timeout("traffic through the tunnel was not verified in time");
        op_failed(t, op, error, true, ctx);
    } else {
        t.activity = Activity::Verifying {
            op,
            deadline,
            next_round: later(ctx.now, ctx.timing.verify_interval),
        };
    }
}

/// The operation is over; nothing is left to resume after a restart, unless
/// an earlier stop of WGAL's may still undo a user's connect: that connect
/// stays unfinished until the stop lands, which starts the tunnel again, or
/// the stop's window ends. A recovery is not kept: its backoff decides when
/// it runs next. The state the operation left the tunnel in is the baseline
/// for noticing what others do next, so an external stop seen first as
/// transitioning is still noticed (follow-up review F12).
fn op_ended(t: &mut Tunnel, op: Op, now: MonoInstant) {
    t.activity = Activity::Idle;
    t.round_counted = false;
    if let Some(incident) = &mut t.rec.incident {
        incident.resume_uncounted = false;
    }
    t.settled_os = (t.os_error.is_none()
        && matches!(t.os, OsState::Connected | OsState::Disconnected))
    .then_some(t.os);
    t.resume_until = None;
    if !op.starts() {
        return;
    }
    if t.rec.resume == Some(ResumeOp::Connect) && t.late_stop(now) {
        t.resume_until = t.late_stop_until;
    } else {
        t.rec.resume = None;
    }
}

/// Ends a record kept only for a late stop or start once its window has
/// passed at `at`, without waiting for another reading: from then on the
/// tunnel changing is someone else's doing, judged as such (plan §6.4).
/// Returns whether the record changed.
fn expire_retained(t: &mut Tunnel, at: MonoInstant) -> bool {
    if t.activity.op().is_some() {
        return false;
    }
    let before = t.rec.resume;
    match before {
        Some(ResumeOp::Disconnect) if t.known_down(at) => disconnect_done(t, at),
        Some(ResumeOp::Connect | ResumeOp::Recovery)
            if t.resume_until.is_some_and(|until| at >= until) =>
        {
            t.rec.resume = None;
            t.resume_until = None;
        }
        _ => {}
    }
    t.rec.resume != before
}

/// The user's disconnect has reached its goal: the tunnel is down. While a
/// start of WGAL's may still land, the disconnect stays unfinished, so the
/// tunnel coming up then is stopped again rather than taken for an external
/// start (follow-up review 2, finding 1).
fn disconnect_done(t: &mut Tunnel, now: MonoInstant) {
    if t.rec.resume == Some(ResumeOp::Disconnect) {
        if t.late_start(now) {
            t.resume_until = t.late_start_until;
        } else {
            t.rec.resume = None;
            t.resume_until = None;
        }
    }
    t.stop_attempts = 0;
    t.stop_retry_at = None;
    t.control_error = None;
}

fn op_succeeded(t: &mut Tunnel, op: Op, ctx: &Ctx<'_>) {
    op_ended(t, op, ctx.now);
    t.next_round = later(ctx.now, ctx.interval());
    t.control_error = None;
    t.recovery_error = None;
    if op == Op::Recovery {
        t.note(LogEvent::RecoverySucceeded);
        t.last_round_end = Some(ctx.now);
        t.wait_from = Some(ctx.now);
        if let Some(incident) = &mut t.rec.incident {
            if incident.first_recovery == FirstRecovery::InProgress {
                incident.first_recovery = FirstRecovery::Succeeded;
            }
            incident.last_round_end_unix_ms = Some(ctx.wall);
            incident.wait_from_unix_ms = Some(ctx.wall);
            incident.next_retry_unix_ms = None;
        }
    }
}

/// A recovery round whose verification ran out of time without one valid
/// answer either way. The incident's first round stays `in_progress`, so it
/// does not count as failed by itself and makes no notification due (see
/// [`inconclusive_round_failed`] for when later evidence does).
///
/// Seen connected at the end: the fault has to be confirmed again from fresh
/// evidence before another round runs, and that round waits out the backoff
/// (re-review NF-3). Not seen connected (still transitioning, unknown or
/// unreadable): the restart may have left the tunnel down, and no external
/// stop can be noticed without a settled state before it, so the tunnel goes
/// into backoff exactly as after a failed round, only without eligibility.
/// The retry then starts a tunnel found down and waits while it is
/// unsettled (review of `3d77031`, P2).
fn op_inconclusive(t: &mut Tunnel, op: Op, error: AdapterError, ctx: &Ctx<'_>) {
    if op == Op::Recovery && t.verify_saw_offline {
        // The host was offline: the round could check nothing, and the
        // offline host uses up no retries (Codex P2-N19). Nothing is left
        // to resume, so nothing counts it again.
        give_back_round(t);
    }
    op_ended(t, op, ctx.now);
    t.next_round = ctx.now;
    t.recovery_error = Some(error);
    t.last_round_end = Some(ctx.now);
    t.wait_from = Some(ctx.now);
    t.reset_streak();
    t.fault_seen = false;
    let seen_up = t.os == OsState::Connected && t.os_error.is_none();
    let retry = t.rec.retry;
    let mut exhausted = false;
    if let Some(incident) = &mut t.rec.incident {
        incident.last_round_end_unix_ms = Some(ctx.wall);
        incident.wait_from_unix_ms = Some(ctx.wall);
        // As after a failed round, the next round waits the retry interval
        // from this one's end and is counted when it is queued (review of
        // wave two, P3-2), within the tunnel's retry limit.
        if !seen_up {
            if retry_left(retry, incident) {
                let wait = retry_wait(retry);
                incident.next_retry_unix_ms = Some(ctx.wall.saturating_add(millis(wait)));
                t.activity = Activity::Backoff {
                    retry_at: later(ctx.now, wait),
                };
            } else {
                exhausted = true;
            }
        }
    }
    if exhausted {
        exhaust_retries(t);
    }
}

/// A first round that ended inconclusive failed after all once fresh
/// evidence shows the restart did not bring the tunnel back: the fault is
/// confirmed again with no successful probe since that round ended, or the
/// tunnel is found settled down when its retry is due. Its incident then
/// becomes eligible for the alert like any failed first round (plan §7.2), so
/// an outage WGAL cannot fix is reported. Without such evidence (a state that
/// stays unreadable or transitioning, probe-tool errors, the host offline)
/// it stays `in_progress` and raises no alert (plan §4.1). Returns whether
/// the record changed.
fn inconclusive_round_failed(t: &mut Tunnel, now: MonoInstant) -> bool {
    let Some(end) = t.last_round_end else {
        return false;
    };
    let evidence = match t.activity {
        Activity::Idle => t.confirmed && t.last_success.is_none_or(|at| at < end),
        Activity::Backoff { retry_at } => {
            now >= retry_at && t.os == OsState::Disconnected && t.os_error.is_none()
        }
        _ => false,
    };
    let notifications = t.rec.notifications_enabled;
    let Some(incident) = t.rec.incident.as_mut() else {
        return false;
    };
    if !evidence || incident.first_recovery != FirstRecovery::InProgress {
        return false;
    }
    incident.first_recovery = FirstRecovery::Failed;
    make_eligible(incident, notifications);
    t.fault_seen = true;
    true
}

/// `executed` is false when the stop or start itself could not be carried
/// out (no permission, manager error), as opposed to a tunnel that was
/// restarted but did not recover.
fn op_failed(t: &mut Tunnel, op: Op, error: AdapterError, executed: bool, ctx: &Ctx<'_>) {
    op_ended(t, op, ctx.now);
    t.next_round = ctx.now;
    if op == Op::Disconnect {
        // Whatever the adapter said, the user still wants the tunnel down:
        // show the error and stop again after a bounded wait, until
        // `query_state` shows it down (follow-up review F2).
        t.control_error = Some(error);
        t.stop_attempts = t.stop_attempts.saturating_add(1);
        t.stop_retry_at = Some(later(ctx.now, ctx.timing.stop_retry_after(t.stop_attempts)));
        return;
    }
    if op != Op::Recovery {
        t.control_error = Some(error);
        // The user's connect or reconnect replaced the incident's automatic
        // retry. If it fails, the automatic retries go on (plan §6.3: never
        // given up): the tunnel goes back on the incident's backoff, and the
        // retry starts it if it is found down (review of `3d77031`, P3-3).
        let retry = t.rec.retry;
        let mut exhausted = false;
        if matches!(op, Op::Connect | Op::Reconnect)
            && t.rec.intent.auto_recovery_enabled
            && let Some(incident) = &mut t.rec.incident
            && incident.first_recovery != FirstRecovery::NotAttempted
        {
            // The manual connect started the count again (`connect`), so the
            // limit allows a retry unless it is 0.
            if retry_left(retry, incident) {
                let wait = retry_wait(retry);
                incident.next_retry_unix_ms = Some(ctx.wall.saturating_add(millis(wait)));
                t.activity = Activity::Backoff {
                    retry_at: later(ctx.now, wait),
                };
                // The incident's wait starts again from this failure.
                incident.wait_from_unix_ms = Some(ctx.wall);
                t.wait_from = Some(ctx.now);
            } else {
                exhausted = true;
            }
        }
        if exhausted {
            exhaust_retries(t);
        }
        return;
    }
    t.recovery_error = Some(error);
    t.fault_seen = true;
    t.last_round_end = Some(ctx.now);
    t.wait_from = Some(ctx.now);
    if executed {
        // The verification saw no reply, or the tunnel down again: evidence
        // taken now.
        t.last_failure = Some(ctx.now);
    }
    let notifications = t.rec.notifications_enabled;
    let retry = t.rec.retry;
    let mut next_retry_unix_ms = None;
    let mut exhausted = false;
    if let Some(incident) = &mut t.rec.incident {
        if incident.first_recovery == FirstRecovery::InProgress {
            incident.first_recovery = if executed {
                FirstRecovery::Failed
            } else {
                FirstRecovery::NotExecuted
            };
            // Only the first round of an incident can make it eligible
            // (plan §7.2).
            make_eligible(incident, notifications);
        }
        incident.last_round_end_unix_ms = Some(ctx.wall);
        incident.wait_from_unix_ms = Some(ctx.wall);
        if retry_left(retry, incident) {
            let wait = retry_wait(retry);
            incident.next_retry_unix_ms = Some(ctx.wall.saturating_add(millis(wait)));
            next_retry_unix_ms = incident.next_retry_unix_ms;
            t.activity = Activity::Backoff {
                retry_at: later(ctx.now, wait),
            };
        } else {
            exhausted = true;
        }
    }
    t.note(LogEvent::RecoveryFailed { next_retry_unix_ms });
    if exhausted {
        exhaust_retries(t);
    }
}

/// Whether a probe that finished at `at` is still evidence now: no older
/// than the tunnel's freshness. A result applied late is only as fresh as
/// the probe itself (plan §5.2).
fn fresh(at: MonoInstant, ctx: &Ctx<'_>) -> bool {
    ctx.now.saturating_duration_since(at) <= ctx.freshness()
}

/// A bound, route-checked probe of the recovery target got an answer when it
/// finished at `at`. Both a reply and a refused TCP connection prove the
/// tunnel carries traffic, so both end a failure streak and count towards
/// closing an incident. Only a reply proves the target itself is the service
/// the user chose, so only a reply passes the baseline (plan §4.3, §5.1).
fn record_success(t: &mut Tunnel, outcome: ProbeOutcome, at: MonoInstant, ctx: &Ctx<'_>) {
    let previous = t.last_success;
    t.failures = 0;
    t.first_failure = None;
    t.last_failure = None;
    t.confirmed = false;
    t.fault_seen = false;
    t.answered(at);
    if matches!(outcome, ProbeOutcome::Reply { .. }) {
        t.rec.baseline_passed = true;
        // What an import asked for, if the baseline passed in time (owner
        // decision of 2026-11-04).
        if let Some(pending) = t.rec.auto_recovery_after_baseline.take()
            && ctx.wall <= pending.until_unix_ms
            && t.rec.recovery_target.is_some()
        {
            t.rec.intent.auto_recovery_enabled = true;
            t.rec.intent.start_when_stopped_externally = pending.start_when_stopped_externally;
        }
    }
    if t.rec.incident.is_none() {
        t.stable_since = None;
        return;
    }
    if matches!(t.activity, Activity::Backoff { .. }) {
        // It recovered on its own; stop retrying but keep the incident open
        // until it has been stable long enough.
        t.activity = Activity::Idle;
        if let Some(incident) = &mut t.rec.incident {
            incident.next_retry_unix_ms = None;
        }
    }
    let gap = previous.is_none_or(|before| at.saturating_duration_since(before) > ctx.freshness());
    let since = match t.stable_since {
        Some(since) if !gap => since,
        _ => at,
    };
    t.stable_since = Some(since);
    if at.saturating_duration_since(since) >= ctx.timing.incident_close_after {
        // Closed (plan §7.2): a pending alert is dropped with it, and the
        // next fault opens a new incident. If automatic recovery worked on it
        // and no manual connect or reconnect ended it, the `recovered`
        // message is due (owner decision of 2026-11-04), whether or not an
        // alert was sent. An earlier incident's message still to be sent or
        // in flight keeps its place beside it (Codex P2-N20).
        if let Some(incident) = t.rec.incident.take() {
            let automatic =
                incident.first_recovery != FirstRecovery::NotAttempted && !incident.manual_last;
            if automatic && t.rec.notifications_enabled {
                let back = ctx.now.saturating_duration_since(since);
                queue_recovered(
                    t,
                    RecoveredNotice {
                        incident_id: incident.incident_id.clone(),
                        opened_at_unix_ms: incident.opened_at_unix_ms,
                        recovered_at_unix_ms: ctx.wall.saturating_sub(millis(back)),
                        notice: NoticeRecord {
                            state: NotificationState::Pending,
                            ..NoticeRecord::default()
                        },
                    },
                );
            }
            t.note(LogEvent::IncidentClosed {
                incident_id: incident.incident_id,
            });
        }
        t.stable_since = None;
        t.last_round_end = None;
        t.wait_from = None;
        t.restored_end = None;
    }
}

/// A failed monitoring round of the recovery target. Outside the grace
/// period it adds to the streak, and the fault is confirmed at the first
/// failed round where the streak has at least `retries + 1` failed rounds
/// and at least `min_fault_secs` have passed since its first failure (owner
/// decision of 2026-11-03, replacing plan §6.2's 3 rounds and 60 s; the
/// defaults give the same). Each round that counts towards confirming is
/// noted for the log; rounds after the fault is confirmed are not.
fn record_failure(t: &mut Tunnel, at: MonoInstant, ctx: &Ctx<'_>) {
    t.stable_since = None;
    if t.in_grace(ctx.now) {
        return;
    }
    t.failures = t.failures.saturating_add(1);
    t.fault_seen = true;
    t.last_failure = Some(t.last_failure.map_or(at, |before| before.max(at)));
    let first = *t.first_failure.get_or_insert(ctx.now);
    if t.confirmed {
        return;
    }
    let needed = ctx.probe.failures_needed();
    t.note(LogEvent::ProbeFailed {
        streak: t.failures,
        needed,
    });
    let min_fault = Duration::from_secs(u64::from(ctx.probe.min_fault_secs()));
    if t.failures >= needed && ctx.now.saturating_duration_since(first) >= min_fault {
        t.confirmed = true;
    }
}

/// Notices changes made outside WGAL (plan §6.4). `previous` is the last
/// settled state a monitoring round saw, so a stop seen as connected,
/// transitioning, disconnected over several rounds still counts. A stop of
/// WGAL's that failed or timed out may land later (`own_stop`): the tunnel
/// going down then is WGAL's doing, so the recovery or backoff under way goes
/// on (follow-up review 2, finding 2). Returns `true` if the tunnel should be
/// started again because the user allowed that.
fn notice_external(t: &mut Tunnel, previous: Option<OsState>, own_stop: bool) -> bool {
    let intent = t.rec.intent;
    match (intent.desired, t.os) {
        (DesiredState::Connected, OsState::Disconnected)
            if previous == Some(OsState::Connected) && t.rec.external.is_none() && !own_stop =>
        {
            t.note(LogEvent::StoppedExternally);
            // Only a tunnel handed over to WGAL is held back from or started
            // again by automatic recovery; an observe-only one is just shown
            // as not connected.
            if intent.auto_recovery_enabled {
                t.activity = Activity::Idle;
                if intent.start_when_stopped_externally {
                    return true;
                }
                t.rec.external = Some(External::StoppedExternally);
            }
        }
        (DesiredState::Connected, OsState::Connected)
            if t.rec.external == Some(External::StoppedExternally) =>
        {
            t.rec.external = None;
        }
        // While the user's own disconnect is unfinished, the tunnel being up is
        // WGAL's business, not something else's (follow-up review F2, F9).
        (DesiredState::Disconnected, OsState::Connected | OsState::Transitioning)
            if t.rec.external != Some(External::ExternallyControlled)
                && t.rec.resume != Some(ResumeOp::Disconnect) =>
        {
            // Started by something else after the user disconnected it:
            // report it, do not fight it, and stop probing through it.
            t.note(LogEvent::StartedExternally);
            t.rec.external = Some(External::ExternallyControlled);
            t.activity = Activity::Idle;
        }
        (DesiredState::Disconnected, OsState::Disconnected)
            if t.rec.external == Some(External::ExternallyControlled) =>
        {
            t.rec.external = None;
        }
        _ => {}
    }
    false
}

/// An operation interrupted by a restart, found in a monitoring round with no
/// operation under way (finding 9). Down: finish it with a start. Up: it got
/// there, never stopped, or its stop was accepted and has not landed yet; so
/// the record is kept for one control phase, and the tunnel going down in that
/// time still finishes the operation instead of counting as an external stop
/// (follow-up review F9). A disconnect is finished by
/// [`Watchdog::plan`] instead.
fn resume(t: &mut Tunnel, ctx: &Ctx<'_>) -> Option<Op> {
    let resume = t.rec.resume?;
    if resume == ResumeOp::Disconnect || t.activity.op().is_some() || t.os_error.is_some() {
        return None;
    }
    match t.os {
        OsState::Connected => {
            // As long as a stop the previous process sent may still land, as
            // in-process (follow-up review 2; review of `121b42a`, P2-5).
            let until = *t
                .resume_until
                .get_or_insert_with(|| later(ctx.now, ctx.timing.late_effect()));
            if ctx.now >= until {
                t.rec.resume = None;
                t.resume_until = None;
            }
            None
        }
        OsState::Disconnected => {
            t.resume_until = None;
            let intent = t.rec.intent;
            let wanted = intent.desired == DesiredState::Connected
                && t.monitoring()
                && !t.rec.binding_lost
                && t.rec.external.is_none();
            match resume {
                ResumeOp::Recovery if wanted && intent.auto_recovery_enabled => Some(Op::Recovery),
                ResumeOp::Connect if wanted => Some(Op::Connect),
                _ => {
                    t.rec.resume = None;
                    None
                }
            }
        }
        OsState::Transitioning | OsState::Unknown => None,
    }
}

/// A monitoring round. Returns an operation to queue: an unfinished one to
/// resume, or a start that undoes an external stop.
fn apply_round(
    t: &mut Tunnel,
    os: Result<OsState, AdapterError>,
    probes: Option<ProbeSet>,
    finished: MonoInstant,
    ctx: &Ctx<'_>,
) -> Option<Op> {
    t.next_round = later(ctx.now, ctx.interval());
    let previous = t.settled_os;
    // Taken before this reading, which clears it if the tunnel is down, and
    // as of the time of the reading, however late it is applied.
    let own_stop = t.late_stop(finished);
    expire_retained(t, finished);
    if !read_os(t, os, ctx) {
        return None;
    }
    if t.rec.resume == Some(ResumeOp::Disconnect)
        && !(t.os == OsState::Disconnected && t.os_error.is_none())
    {
        // A late start landed in time, so the disconnect is unfinished again
        // and no deadline may drop it after a restart (review of `b5e4828`).
        t.resume_until = None;
    }
    if t.os_error.is_none() && matches!(t.os, OsState::Connected | OsState::Disconnected) {
        t.settled_os = Some(t.os);
    }
    if t.rec.intent.desired == DesiredState::Disconnected
        && t.os == OsState::Disconnected
        && t.os_error.is_none()
    {
        disconnect_done(t, ctx.now);
    }
    let mut start = resume(t, ctx);
    if start.is_none() && t.os_error.is_none() && notice_external(t, previous, own_stop) {
        start = Some(Op::ExternalRestart);
    }
    if t.os != OsState::Connected || t.os_error.is_some() {
        // Not a data-path sample either way.
        t.failures = 0;
        t.first_failure = None;
        t.confirmed = false;
        t.stable_since = None;
        return start;
    }
    let Some(set) = probes else {
        return start;
    };
    if set.stale {
        // The interface changed during the round: nothing from it counts, and
        // nothing proven through the old interface is shown any more (plan
        // §5.2). The new interface is checked again shortly.
        t.stable_since = None;
        t.last_success = None;
        t.path = Path::Unknown;
        t.next_round = later(ctx.now, ctx.timing.verify_interval);
        return start;
    }
    if set.interface.is_err() && !fresh(finished, ctx) {
        // Applied too late to be evidence either way, like a stale probe
        // (plan §5.2, follow-up review F7).
        t.stable_since = None;
        return start;
    }
    match &set.interface {
        Err(error) if error.kind == AdapterErrorKind::NotFound => {
            // Connected according to the manager, yet no interface.
            if t.rec.recovery_target.is_some() {
                record_failure(t, finished, ctx);
            }
            return start;
        }
        Err(error) => {
            t.probe_error = Some(error.clone());
            t.stable_since = None;
            return start;
        }
        Ok(_) => {}
    }
    update_views(t, &set);
    for probe in set.probes.iter().filter(|p| p.role == TargetRole::Recovery) {
        if !fresh(probe.finished, ctx) {
            // Applied too late to be evidence either way (plan §5.2).
            t.stable_since = None;
            continue;
        }
        match &probe.result {
            Ok((_, outcome)) => {
                t.route_error = None;
                t.probe_error = None;
                if t.local_offline {
                    // Back online: probe again only after a grace period.
                    t.grace_until = later(ctx.now, ctx.timing.grace);
                }
                back_online(t);
                if !can_bind(&ctx.caps, &probe.target) {
                    // Never let an unbound probe turn the light green (§5.2).
                    t.path = Path::Unverified;
                    t.stable_since = None;
                    continue;
                }
                t.path = Path::Verified;
                match outcome {
                    ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. } => {
                        if t.failures > 0 {
                            t.note(LogEvent::ProbeRecovered { after: t.failures });
                        }
                        record_success(t, *outcome, probe.finished, ctx);
                    }
                    ProbeOutcome::NoReply => record_failure(t, probe.finished, ctx),
                }
            }
            Err(error) => {
                t.stable_since = None;
                match error.kind {
                    AdapterErrorKind::RouteMismatch => {
                        t.path = Path::Conflict;
                        t.route_error = Some(error.clone());
                    }
                    AdapterErrorKind::LocalNetworkOffline => {
                        // Samples from before the host went offline are no
                        // evidence once it is back: the fault is confirmed
                        // afresh after the grace period (plan §6.2, §6.3).
                        mark_offline(t, probe.finished);
                        t.reset_streak();
                        t.fault_seen = false;
                    }
                    // The interface went away mid-round: stale, not a failure.
                    AdapterErrorKind::NotFound => {}
                    // The probe itself is broken: never a target failure (T23).
                    _ => t.probe_error = Some(error.clone()),
                }
            }
        }
    }
    start
}
