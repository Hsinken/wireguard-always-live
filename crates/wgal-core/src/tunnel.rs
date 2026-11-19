//! Runtime state of one registered tunnel and the rules that read it: what
//! is due next, how a probe result counts, and which light it shows.

use std::time::Duration;

use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, DataHealth, DesiredState, Detail, Fault,
    Light, LightReason, LogEvent, OsState, ProbeKind, ProbeResult, ProbeSummary, ProbeTarget,
    RecoveryPhase, TargetStatus,
};

use crate::clock::MonoInstant;
use crate::platform::{AdapterError, ProbeOutcome, TunnelBinding};
use crate::record::{External, TunnelRecord};
use crate::timing::Timing;
use crate::work::{TargetRole, WorkKind};

/// A change of the tunnel's state that WGAL carries out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// Automatic recovery after a confirmed fault.
    Recovery,
    Connect,
    Disconnect,
    Reconnect,
    /// Start after an external stop, with `start_when_stopped_externally` on.
    ExternalRestart,
}

impl Op {
    pub(crate) fn stops_first(self) -> bool {
        matches!(self, Self::Recovery | Self::Disconnect | Self::Reconnect)
    }

    pub(crate) fn starts(self) -> bool {
        self != Self::Disconnect
    }

    /// Started by WGAL on its own rather than by a user request.
    pub(crate) fn is_automatic(self) -> bool {
        matches!(self, Self::Recovery | Self::ExternalRestart)
    }
}

/// What the tunnel is doing. `Stopping` and `Starting` hold the host-wide
/// control slot; nothing else does, so a long verification never blocks
/// another tunnel's recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    Idle,
    /// Waiting for the control slot.
    Queued {
        op: Op,
        since: MonoInstant,
    },
    Stopping {
        op: Op,
        deadline: MonoInstant,
        issued: bool,
        next_poll: MonoInstant,
    },
    Starting {
        op: Op,
        deadline: MonoInstant,
        issued: bool,
        next_poll: MonoInstant,
    },
    Verifying {
        op: Op,
        deadline: MonoInstant,
        next_round: MonoInstant,
    },
    /// The last automatic round failed; the next is due at `retry_at`.
    Backoff {
        retry_at: MonoInstant,
    },
}

impl Activity {
    pub(crate) fn op(self) -> Option<Op> {
        match self {
            Self::Queued { op, .. }
            | Self::Stopping { op, .. }
            | Self::Starting { op, .. }
            | Self::Verifying { op, .. } => Some(op),
            Self::Idle | Self::Backoff { .. } => None,
        }
    }

    pub(crate) fn holds_control_slot(self) -> bool {
        matches!(self, Self::Stopping { .. } | Self::Starting { .. })
    }

    pub(crate) fn wants_control_slot(self) -> bool {
        matches!(self, Self::Queued { .. }) || self.holds_control_slot()
    }
}

/// What is known about the path from this tunnel's interface to the
/// recovery target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Path {
    Unknown,
    /// A probe bound to the interface ran with matching route evidence.
    Verified,
    /// The platform cannot bind this kind of probe to the interface.
    Unverified,
    /// The target routes through another interface.
    Conflict,
}

/// Why automatic recovery cannot act, other than settings and storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    BindingLost,
    External,
    Baseline,
    LocalOffline,
    ProbeUnavailable,
    Path,
    NotConnected,
    Grace,
}

#[derive(Clone, Debug)]
pub(crate) struct Tunnel {
    pub(crate) rec: TunnelRecord,
    pub(crate) binding: TunnelBinding,
    pub(crate) os: OsState,
    pub(crate) os_since: MonoInstant,
    pub(crate) os_error: Option<AdapterError>,
    /// The last settled state (connected or disconnected) a monitoring round
    /// saw, or the one WGAL's own operation left the tunnel in. Cleared while
    /// WGAL changes the tunnel, so only changes made outside WGAL are compared
    /// against it.
    pub(crate) settled_os: Option<OsState>,
    /// Failed tries of the current disconnect's stop, and when the next one
    /// is due.
    pub(crate) stop_attempts: u32,
    pub(crate) stop_retry_at: Option<MonoInstant>,
    /// When `rec.resume` stops counting, if it is kept only because a late
    /// stop or start may still land: until then the tunnel changing finishes
    /// that operation instead of counting as an external change. Saved as
    /// wall time, so a restart keeps the deadline instead of granting a new
    /// one.
    pub(crate) resume_until: Option<MonoInstant>,
    /// Until when a stop WGAL handed to the platform may still take the
    /// tunnel down although nothing showed it doing so yet (it timed out or
    /// failed after it may have been sent). Cleared once the tunnel is seen
    /// down. Meanwhile the tunnel going down is WGAL's doing, never an
    /// external stop (follow-up review 2, finding 2).
    pub(crate) late_stop_until: Option<MonoInstant>,
    /// The same for a start, which may still bring the tunnel up: cleared
    /// once the tunnel is seen up. Meanwhile a tunnel that looks down is not
    /// known to be down (follow-up review 2, finding 1).
    pub(crate) late_start_until: Option<MonoInstant>,
    /// The OS state has been `Transitioning` for longer than a control phase.
    pub(crate) stuck: Option<AdapterError>,
    pub(crate) path: Path,
    pub(crate) route_error: Option<AdapterError>,
    pub(crate) probe_error: Option<AdapterError>,
    /// A user operation failed to carry out its stop or start.
    pub(crate) control_error: Option<AdapterError>,
    /// Why the last automatic recovery round failed.
    pub(crate) recovery_error: Option<AdapterError>,
    /// The last probe of the recovery target found this host without a
    /// network of its own. Cleared when a probe gets through, and dropped as
    /// evidence from before when the host wakes, the network changes,
    /// monitoring resumes or the recovery target changes: the next round
    /// judges afresh.
    pub(crate) local_offline: bool,
    /// When a probe last found this host offline. A tunnel that is down is
    /// not probed, so its mark cannot be cleared by a probe getting through;
    /// it stops blocking a retry once it is older than the tunnel's
    /// freshness, as any other probe result (plan §5.2; Codex P2-N19).
    pub(crate) offline_seen_at: Option<MonoInstant>,
    /// The log was told this host went offline and not yet that it is back:
    /// the pair is written once per outage, however often the mark is
    /// dropped and set again in between.
    pub(crate) offline_noted: bool,
    /// This tunnel is one of those the host-wide offline line rests on: a
    /// probe of its recovery target found the host offline and none got
    /// through since. Dropped without evidence once the tunnel can no longer
    /// say (monitoring off or paused, no recovery target), so a later resume
    /// does not bring back an outage seen before (Codex P3-N15). Kept over a
    /// wake or a network change: the next probe settles it either way.
    pub(crate) host_vouch: bool,
    /// Counted failures in the current streak: failed monitoring rounds of
    /// the recovery target outside the grace period, since the last success
    /// or the last time the streak was reset.
    pub(crate) failures: u32,
    pub(crate) first_failure: Option<MonoInstant>,
    /// When the evidence of the last counted failure was taken: the probe's
    /// finish, or the end of a recovery round whose verification saw no
    /// reply. A confirmed fault is evidence for an automatic round only
    /// while this is no older than the tunnel's freshness (plan §5.2): after
    /// a stall, or results applied late, it is confirmed afresh (Codex
    /// P2-N35, P2-N39, P2-N40).
    pub(crate) last_failure: Option<MonoInstant>,
    pub(crate) confirmed: bool,
    /// This process has seen the fault go on: a counted failure or a failed
    /// recovery round since the last success. A pending notification is due
    /// only then, never on state restored from before a restart.
    pub(crate) fault_seen: bool,
    /// When the recovery target last answered a bound, route-checked probe.
    /// Kept as the time of the last answer even when it stops being evidence
    /// (see `current_success`).
    pub(crate) last_success: Option<MonoInstant>,
    /// Advanced when the host wakes or its network changes: an answer from
    /// before proves nothing about the path now, and the probes of a round
    /// planned before are ignored when it comes back (Codex review of wave
    /// three, P1-3). Separate from the operation generation, which would also
    /// void a stop or start under way.
    pub(crate) probe_generation: u64,
    /// The probe generation `last_success` was given under.
    pub(crate) success_generation: u64,
    /// Start of the current run of fresh successes, for closing an incident.
    pub(crate) stable_since: Option<MonoInstant>,
    pub(crate) grace_until: MonoInstant,
    pub(crate) next_round: MonoInstant,
    pub(crate) last_round_end: Option<MonoInstant>,
    /// When the incident's current wait for its next automatic round
    /// started: the end of the last automatic round, or a manual connect or
    /// reconnect that failed and put the tunnel back on the incident's
    /// backoff. Every such wait is measured from it with the tunnel's
    /// interval at the time. Kept apart from `last_round_end`, which is
    /// evidence about the round itself (Codex P2-N56).
    pub(crate) wait_from: Option<MonoInstant>,
    /// After a restart, when that wait started, as stored
    /// (`IncidentRecord::wait_from_unix_ms`, or the end of the last round in
    /// a state from before it): a moment of this process (the restart) and
    /// how long before it, never negative and never cut to an interval, so
    /// a time before this process is never truncated and time already
    /// waited counts with any later interval. A restored backoff is due from
    /// it too, so every wait agrees (Codex P2-N47, P2-N50, P2-N53, P2-N54,
    /// P2-N56, P2-N57, P2-N58). Used until this process starts a wait; it
    /// is no evidence about the round itself (P2-N52).
    pub(crate) restored_end: Option<(MonoInstant, Duration)>,
    /// The current verification has seen the recovery target fail to answer
    /// a fresh, bound, route-checked probe. Without that, a verification that
    /// runs out of time only met errors and proves nothing either way.
    pub(crate) verify_saw_no_reply: bool,
    /// The current verification found the host offline. A recovery round
    /// that then ends inconclusive could not check anything for that
    /// reason and does not count against the retry limit (Codex P2-N19).
    pub(crate) verify_saw_offline: bool,
    /// The automatic round queued or under way was counted against the
    /// tunnel's retry limit in this process (the incident's `backoff_step`
    /// was raised for it). A round that never sends anything, or whose
    /// verification finds the host offline, gives the count back (Codex
    /// P2-N18, P2-N19).
    pub(crate) round_counted: bool,
    /// Monitoring of every tunnel is paused; mirrors the watchdog's flag.
    pub(crate) paused: bool,
    pub(crate) activity: Activity,
    pub(crate) recovery_view: Option<TargetStatus>,
    pub(crate) observe_views: Vec<TargetStatus>,
    pub(crate) light: Option<(Light, LightReason)>,
    pub(crate) light_since_unix_ms: i64,
    pub(crate) seq: u64,
    pub(crate) pending: Option<u64>,
    /// A check asked for (`Request::ProbeNow`) while this tunnel is not
    /// monitored, by its own switch or the host-wide pause (owner decision
    /// of 2026-11-06): when it was asked for. One round is planned for it;
    /// see [`Self::one_off_seq`].
    pub(crate) one_off_at: Option<MonoInstant>,
    /// The sequence number of that one round once planned. Its result only
    /// updates what each target last answered; it is no evidence for the
    /// light, a streak, a fault or any operation, and the state reading in
    /// it is not applied (see `apply_one_off`).
    pub(crate) one_off_seq: Option<u64>,
    /// The sequence number of a stop or start that passed `still_due` and
    /// whose result has not been applied: it may reach the platform, so its
    /// operation is under way and no longer "nothing sent yet", although
    /// `issued` stays false until the result is in.
    pub(crate) command_out: Option<u64>,
    /// The operation under way has handed a stop or start to the platform
    /// (one passed `still_due`, or its result was applied). Reset when an
    /// operation is queued. An automatic operation without it has sent
    /// nothing yet and may still be withdrawn.
    pub(crate) sent: bool,
    /// What was decided about this tunnel for the event log, oldest first,
    /// until the watchdog collects it (see `Watchdog::take_events`).
    pub(crate) events: Vec<LogEvent>,
}

/// The light with its reason and the times the GUI shows next to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LightView {
    pub(crate) light: Light,
    pub(crate) reason: LightReason,
    pub(crate) deadline: Option<MonoInstant>,
    pub(crate) next_retry: Option<MonoInstant>,
}

fn view(light: Light, reason: LightReason) -> LightView {
    LightView {
        light,
        reason,
        deadline: None,
        next_retry: None,
    }
}

fn with_deadline(light: Light, reason: LightReason, deadline: MonoInstant) -> LightView {
    LightView {
        deadline: Some(deadline),
        ..view(light, reason)
    }
}

/// What one probe that runs into its timeout may take beyond it: the
/// platform's own deadline (Ubuntu's and macOS's ping exit up to a second
/// after the wait) and the route checks around the probe. The runtime's
/// worker suggestion counts the same second (`driver::suggested_workers`).
const PROBE_OVERHEAD_SECS: u32 = 1;

fn later(at: MonoInstant, by: Duration) -> MonoInstant {
    at.checked_add(by).unwrap_or(at)
}

/// Whether the platform can bind this kind of probe to an interface.
pub(crate) fn can_bind(caps: &AdapterCapabilities, target: &ProbeTarget) -> bool {
    let capability = match target.probe {
        ProbeKind::Icmp => caps.bound_icmp_probe,
        ProbeKind::TcpConnect { .. } => caps.bound_tcp_probe,
    };
    capability == Capability::Supported
}

pub(crate) fn summary(
    started_at_unix_ms: i64,
    finished_at_unix_ms: i64,
    result: &Result<ProbeOutcome, AdapterErrorKind>,
) -> ProbeSummary {
    let result = match result {
        Ok(ProbeOutcome::Reply { rtt }) => ProbeResult::Reply {
            rtt_ms: u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX),
        },
        Ok(ProbeOutcome::Refused { rtt }) => ProbeResult::Refused {
            rtt_ms: u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX),
        },
        Ok(ProbeOutcome::NoReply) => ProbeResult::NoReply,
        Err(kind) => ProbeResult::Error { error: *kind },
    };
    ProbeSummary {
        started_at_unix_ms,
        finished_at_unix_ms,
        result,
    }
}

impl Tunnel {
    pub(crate) fn new(
        rec: TunnelRecord,
        binding: TunnelBinding,
        now: MonoInstant,
        grace_until: MonoInstant,
        first_round: MonoInstant,
    ) -> Self {
        let mut tunnel = Self {
            rec,
            binding,
            os: OsState::Unknown,
            os_since: now,
            os_error: None,
            settled_os: None,
            stop_attempts: 0,
            stop_retry_at: None,
            resume_until: None,
            late_stop_until: None,
            late_start_until: None,
            stuck: None,
            path: Path::Unknown,
            route_error: None,
            probe_error: None,
            control_error: None,
            recovery_error: None,
            local_offline: false,
            offline_seen_at: None,
            offline_noted: false,
            host_vouch: false,
            failures: 0,
            first_failure: None,
            last_failure: None,
            confirmed: false,
            fault_seen: false,
            last_success: None,
            probe_generation: 0,
            success_generation: 0,
            stable_since: None,
            grace_until,
            next_round: first_round,
            last_round_end: None,
            wait_from: None,
            restored_end: None,
            verify_saw_no_reply: false,
            verify_saw_offline: false,
            round_counted: false,
            paused: false,
            activity: Activity::Idle,
            recovery_view: None,
            observe_views: Vec::new(),
            light: None,
            light_since_unix_ms: 0,
            seq: 0,
            pending: None,
            one_off_at: None,
            one_off_seq: None,
            command_out: None,
            sent: false,
            events: Vec::new(),
        };
        tunnel.reset_views();
        tunnel
    }

    pub(crate) fn reset_views(&mut self) {
        self.recovery_view = self.rec.recovery_target.map(|target| TargetStatus {
            target,
            last_probe: None,
        });
        self.observe_views = self
            .rec
            .observe_targets
            .iter()
            .map(|&target| TargetStatus {
                target,
                last_probe: None,
            })
            .collect();
    }

    /// Forgets the failure streak and any confirmation, as after a wake, a
    /// network change or a target change: old samples are not evidence now.
    pub(crate) fn reset_streak(&mut self) {
        self.failures = 0;
        self.first_failure = None;
        self.last_failure = None;
        self.confirmed = false;
        self.stable_since = None;
    }

    pub(crate) fn in_grace(&self, now: MonoInstant) -> bool {
        now < self.grace_until
    }

    /// Drops the local-offline mark as evidence from before (see
    /// `local_offline`). Nothing is logged: the outage is over for the log
    /// only once a probe gets through.
    pub(crate) fn forget_offline(&mut self) {
        self.local_offline = false;
        self.offline_seen_at = None;
    }

    /// The confirmed fault rests on evidence that is still fresh at `now`
    /// (see `last_failure`).
    pub(crate) fn fault_fresh(&self, now: MonoInstant) -> bool {
        self.confirmed
            && self
                .last_failure
                .is_some_and(|at| now <= later(at, self.freshness()))
    }

    /// The host-offline mark is still evidence at `now`: no older than the
    /// tunnel's freshness.
    pub(crate) fn offline_fresh(&self, now: MonoInstant) -> bool {
        self.local_offline
            && self
                .offline_seen_at
                .is_some_and(|at| now < later(at, self.freshness()))
    }

    /// How long the incident's current wait has gone on at `now`, if one has
    /// started (see `wait_from`, `restored_end`): measured by this
    /// process's clock, with the time before a restart as stored.
    pub(crate) fn waited(&self, now: MonoInstant) -> Option<Duration> {
        if let Some(from) = self.wait_from {
            return Some(now.saturating_duration_since(from));
        }
        let (anchor, ago) = self.restored_end?;
        Some(now.saturating_duration_since(anchor).saturating_add(ago))
    }

    /// When the incident's next automatic round may run, `wait` after its
    /// current wait started, if one has (see `wait_from`, `restored_end`).
    pub(crate) fn round_due_after(&self, wait: Duration) -> Option<MonoInstant> {
        if let Some(from) = self.wait_from {
            return Some(later(from, wait));
        }
        let (anchor, ago) = self.restored_end?;
        Some(later(anchor, wait.saturating_sub(ago)))
    }

    /// When an automatic retry waiting in backoff is next worth checking,
    /// apart from any round: its due time, or for a tunnel left down while
    /// the host was found offline, the later moment that mark stops being
    /// evidence. The retry runs on its own interval, not the probe
    /// interval's (Codex P2-N17).
    pub(crate) fn retry_check_at(&self) -> Option<MonoInstant> {
        if self.rec.binding_lost || self.pending.is_some() {
            return None;
        }
        let Activity::Backoff { retry_at } = self.activity else {
            return None;
        };
        let offline_until = self
            .offline_seen_at
            .filter(|_| self.local_offline)
            .map(|at| later(at, self.freshness()));
        Some(offline_until.map_or(retry_at, |until| until.max(retry_at)))
    }

    /// The host woke or its network changed: what was proven about the path
    /// before is no evidence now. The last answer no longer counts towards
    /// the light, the path is not known, and the probes of a round still out
    /// are ignored when it comes back (`probe_generation`). Stops, starts and
    /// state readings are not affected.
    pub(crate) fn forget_path_evidence(&mut self) {
        self.probe_generation = self.probe_generation.wrapping_add(1);
        self.path = Path::Unknown;
        self.route_error = None;
    }

    /// The recovery target answered a probe that finished at `at`.
    pub(crate) fn answered(&mut self, at: MonoInstant) {
        self.last_success = Some(at);
        self.success_generation = self.probe_generation;
    }

    /// The last answer of the recovery target, if it is still evidence of
    /// the path: from a round planned since the host last woke or its network
    /// changed.
    pub(crate) fn current_success(&self) -> Option<MonoInstant> {
        self.last_success
            .filter(|_| self.success_generation == self.probe_generation)
    }

    /// Records an event for the log.
    pub(crate) fn note(&mut self, event: LogEvent) {
        self.events.push(event);
    }

    /// Time between this tunnel's monitoring rounds.
    pub(crate) fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.rec.probe.interval_secs()))
    }

    /// Deadline the platform gets for one probe of this tunnel.
    pub(crate) fn probe_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.rec.probe.timeout_secs()))
    }

    /// How old a probe result of this tunnel may be, in seconds, and still
    /// count: the probe timing's freshness (interval + timeout + 18 s) plus,
    /// for each observe target, the timeout and [`PROBE_OVERHEAD_SECS`]. A
    /// round probes the recovery target first and its observe targets after
    /// it, so the round's own span never makes the recovery target's result
    /// stale by the time the round is applied, and two answers of the
    /// recovery target a round apart stay within it however many observe
    /// targets run into their timeout in between (Codex review of wave three,
    /// P2-5). Without observe targets it is the probe timing's own (35 s with
    /// the defaults). The snapshot reports this value.
    pub(crate) fn freshness_secs(&self) -> u32 {
        let probe = self.rec.probe;
        let per_target = u32::from(probe.timeout_secs()) + PROBE_OVERHEAD_SECS;
        let observe = u32::try_from(self.rec.observe_targets.len()).unwrap_or(u32::MAX);
        probe
            .freshness_secs()
            .saturating_add(observe.saturating_mul(per_target))
    }

    /// [`Self::freshness_secs`] as a duration.
    pub(crate) fn freshness(&self) -> Duration {
        Duration::from_secs(u64::from(self.freshness_secs()))
    }

    /// The earliest a streak whose first failure was at `first` can confirm
    /// the fault: after `rounds_to_confirm` more rounds, which also covers
    /// the minimum fault time. Rounds that run late confirm later.
    pub(crate) fn confirm_deadline(&self, first: MonoInstant) -> MonoInstant {
        later(
            first,
            self.interval()
                .saturating_mul(self.rec.probe.rounds_to_confirm()),
        )
    }

    /// WGAL has something under way on the tunnel that forgetting it would
    /// leave half done: an operation (queued, stopping, starting or
    /// verifying), a stop or start whose result is not back, an operation
    /// recorded to resume (a disconnect whose stop is not confirmed among
    /// them), or a stop of WGAL's that may still take the tunnel down. A late
    /// start is not counted: it can only bring the tunnel up, and a user's
    /// disconnect keeps its record while one may land.
    pub(crate) fn unfinished(&self, now: MonoInstant) -> bool {
        self.activity.op().is_some()
            || self.command_out.is_some()
            || self.rec.resume.is_some()
            || self.stop_retry_at.is_some()
            || self.late_stop(now)
    }

    /// The automatic operation under way (a recovery round, or a start after
    /// an external stop) if it has sent nothing to the platform yet: queued
    /// for the control slot, or holding it with neither its stop nor its
    /// start handed out. A recovery of a tunnel that is already down goes
    /// straight to its start, so a start not sent yet is only "nothing sent"
    /// when no stop went out before it.
    pub(crate) fn unsent_automatic(&self) -> Option<Op> {
        let op = match self.activity {
            Activity::Queued { op, .. }
            | Activity::Stopping {
                op, issued: false, ..
            }
            | Activity::Starting {
                op, issued: false, ..
            } => op,
            _ => return None,
        };
        (op.is_automatic() && !self.sent && self.command_out.is_none()).then_some(op)
    }

    /// A stop of WGAL's may still take the tunnel down.
    pub(crate) fn late_stop(&self, now: MonoInstant) -> bool {
        self.late_stop_until.is_some_and(|until| now < until)
    }

    /// A start of WGAL's may still bring the tunnel up.
    pub(crate) fn late_start(&self, now: MonoInstant) -> bool {
        self.late_start_until.is_some_and(|until| now < until)
    }

    /// Seen down, and no start of WGAL's can still bring it up.
    pub(crate) fn known_down(&self, now: MonoInstant) -> bool {
        self.os == OsState::Disconnected && self.os_error.is_none() && !self.late_start(now)
    }

    /// Monitoring is on for this tunnel and not paused for every tunnel.
    pub(crate) fn monitoring(&self) -> bool {
        self.rec.intent.monitoring_enabled && !self.paused
    }

    /// Seen up, and no stop of WGAL's can still take it down.
    pub(crate) fn known_up(&self, now: MonoInstant) -> bool {
        self.os == OsState::Connected && self.os_error.is_none() && !self.late_stop(now)
    }

    pub(crate) fn wants_probes(&self) -> bool {
        self.rec.intent.desired == DesiredState::Connected
            && self.rec.external != Some(External::ExternallyControlled)
    }

    /// Every target of this tunnel, the recovery target first, as a round
    /// probes them.
    pub(crate) fn all_targets(&self) -> Vec<(TargetRole, ProbeTarget)> {
        let recovery = self.rec.recovery_target.map(|t| (TargetRole::Recovery, t));
        let observe = self
            .rec
            .observe_targets
            .iter()
            .enumerate()
            .map(|(i, &t)| (TargetRole::Observe(i), t));
        recovery.into_iter().chain(observe).collect()
    }

    /// How many targets a monitoring round of this tunnel probes now, as
    /// [`Self::due`] would plan it.
    pub(crate) fn round_targets(&self) -> usize {
        if self.rec.binding_lost || !self.wants_probes() {
            return 0;
        }
        usize::from(self.rec.recovery_target.is_some()) + self.rec.observe_targets.len()
    }

    /// The platform work due now, if any.
    pub(crate) fn due(&self, now: MonoInstant) -> Option<WorkKind> {
        if self.rec.binding_lost || self.pending.is_some() {
            return None;
        }
        match self.activity {
            Activity::Queued { .. } => None,
            Activity::Stopping { issued: false, .. } => Some(WorkKind::Stop),
            Activity::Starting { issued: false, .. } => Some(WorkKind::Start),
            Activity::Stopping { next_poll, .. } | Activity::Starting { next_poll, .. } => {
                (now >= next_poll).then_some(WorkKind::Poll)
            }
            Activity::Verifying { next_round, .. } => {
                (now >= next_round).then(|| WorkKind::Round {
                    targets: self
                        .rec
                        .recovery_target
                        .map(|t| (TargetRole::Recovery, t))
                        .into_iter()
                        .collect(),
                    probe_timeout: self.probe_timeout(),
                })
            }
            Activity::Idle | Activity::Backoff { .. } => {
                if !self.monitoring() {
                    // Not monitored: only a check asked for, once.
                    let asked = self.one_off_at.is_some_and(|at| now >= at);
                    return (asked && self.round_targets() > 0).then(|| WorkKind::Round {
                        targets: self.all_targets(),
                        probe_timeout: self.probe_timeout(),
                    });
                }
                if now < self.next_round {
                    return None;
                }
                let targets = if self.wants_probes() {
                    let recovery = self.rec.recovery_target.map(|t| (TargetRole::Recovery, t));
                    let observe = self
                        .rec
                        .observe_targets
                        .iter()
                        .enumerate()
                        .map(|(i, &t)| (TargetRole::Observe(i), t));
                    recovery.into_iter().chain(observe).collect()
                } else {
                    Vec::new()
                };
                Some(WorkKind::Round {
                    targets,
                    probe_timeout: self.probe_timeout(),
                })
            }
        }
    }

    /// When [`Self::due`] next has work, as far as its own schedule says:
    /// the next poll or round. `None` while an item is out, the binding is
    /// lost, the tunnel waits for the control slot, or a stop or start is
    /// ready to be sent (planned at once, not at a time).
    pub(crate) fn next_scheduled(&self) -> Option<MonoInstant> {
        if self.rec.binding_lost || self.pending.is_some() {
            return None;
        }
        match self.activity {
            Activity::Queued { .. }
            | Activity::Stopping { issued: false, .. }
            | Activity::Starting { issued: false, .. } => None,
            Activity::Stopping { next_poll, .. } | Activity::Starting { next_poll, .. } => {
                Some(next_poll)
            }
            Activity::Verifying { next_round, .. } => Some(next_round),
            Activity::Idle | Activity::Backoff { .. } => {
                if self.monitoring() {
                    Some(self.next_round)
                } else {
                    self.one_off_at.filter(|_| self.round_targets() > 0)
                }
            }
        }
    }

    /// Records the OS state. Any change starts a new operation generation:
    /// whatever was planned against the old state is discarded when its
    /// result comes back. Leaving `Connected` also voids what was proven
    /// through the old interface, which the platform may replace with one
    /// that looks the same (plan §5.2). A late stop or start of WGAL's has
    /// landed once the tunnel is seen where it would take it.
    pub(crate) fn set_os(&mut self, state: OsState, now: MonoInstant) {
        match state {
            OsState::Disconnected => self.late_stop_until = None,
            OsState::Connected => self.late_start_until = None,
            OsState::Transitioning | OsState::Unknown => {}
        }
        if state != self.os {
            if self.os == OsState::Connected {
                self.last_success = None;
                self.path = Path::Unknown;
                self.stable_since = None;
            }
            self.os = state;
            self.os_since = now;
            self.rec.generation = self.rec.generation.wrapping_add(1);
        }
        self.os_error = None;
    }

    pub(crate) fn data_health(&self, now: MonoInstant) -> DataHealth {
        if self.confirmed {
            DataHealth::Failed
        } else if self.failures > 0 {
            DataHealth::Suspect
        } else {
            match self.current_success() {
                Some(at) if now.saturating_duration_since(at) <= self.freshness() => {
                    DataHealth::Healthy
                }
                Some(_) => DataHealth::Stale,
                None => DataHealth::Unknown,
            }
        }
    }

    /// What, apart from the user's settings and storage, keeps automatic
    /// recovery from acting now (plan §6.3).
    pub(crate) fn recovery_block(&self, now: MonoInstant) -> Option<Block> {
        if self.rec.binding_lost {
            Some(Block::BindingLost)
        } else if self.rec.external.is_some() {
            Some(Block::External)
        } else if !self.rec.baseline_passed {
            Some(Block::Baseline)
        } else if self.local_offline {
            Some(Block::LocalOffline)
        } else if self.probe_error.is_some() {
            Some(Block::ProbeUnavailable)
        } else if matches!(self.path, Path::Unverified | Path::Conflict) {
            // Only a probe that cannot be bound or a route that leaves through
            // another interface blocks recovery (plan §5.2). A path not seen
            // yet does not: a tunnel that is Connected without an interface
            // never gets a probe that could show one (re-review of 64a3030).
            Some(Block::Path)
        } else if self.os != OsState::Connected || self.os_error.is_some() {
            Some(Block::NotConnected)
        } else if self.in_grace(now) {
            Some(Block::Grace)
        } else {
            None
        }
    }

    pub(crate) fn recovery_phase(&self, now: MonoInstant, storage_ok: bool) -> RecoveryPhase {
        match self.activity {
            Activity::Queued { .. } => RecoveryPhase::Queued,
            Activity::Stopping { .. } => RecoveryPhase::Stopping,
            Activity::Starting { .. } => RecoveryPhase::Starting,
            Activity::Verifying { .. } => RecoveryPhase::Verifying,
            Activity::Backoff { .. } => RecoveryPhase::Backoff,
            Activity::Idle => {
                let wanted = self.rec.intent.auto_recovery_enabled
                    && self.monitoring()
                    && self.rec.intent.desired == DesiredState::Connected;
                let block = self.recovery_block(now);
                if wanted && (!storage_ok || block.is_some_and(|b| b != Block::Grace)) {
                    RecoveryPhase::Blocked
                } else {
                    RecoveryPhase::Idle
                }
            }
        }
    }

    pub(crate) fn fault(&self) -> Option<Fault> {
        let error = self
            .control_error
            .as_ref()
            .or(self.os_error.as_ref())
            .or(self.stuck.as_ref())
            .or(self.recovery_error.as_ref())
            .or(self.probe_error.as_ref())
            .or(self.route_error.as_ref())?;
        Some(Fault {
            kind: error.kind,
            message: Detail::lossy(error.detail.as_str()),
        })
    }

    /// The light, from the same state the snapshot reports (plan §6.1).
    pub(crate) fn light(&self, now: MonoInstant, timing: &Timing) -> LightView {
        use Light::{Gray, Green, Red, Yellow};
        use LightReason as R;

        if self.rec.binding_lost {
            return view(Red, R::BindingLost);
        }
        if !self.monitoring() {
            return view(Gray, R::MonitoringOff);
        }
        if self.activity.op().is_none() && self.control_error.is_some() {
            return view(Red, R::ControlFailed);
        }
        let phase = timing.control_phase_timeout;
        let verify = timing.verify_window;
        match self.activity {
            Activity::Queued {
                op: Op::Disconnect, ..
            }
            | Activity::Stopping {
                op: Op::Disconnect, ..
            } => return view(Gray, R::DisconnectedByUser),
            Activity::Verifying { deadline, .. } => {
                return with_deadline(Yellow, R::Verifying, deadline);
            }
            Activity::Queued { op, since } => {
                let rest = phase.saturating_mul(2).saturating_add(verify);
                return with_deadline(Yellow, op_reason(op), later(since, rest));
            }
            Activity::Stopping { op, deadline, .. } => {
                let rest = if op.starts() {
                    phase.saturating_add(verify)
                } else {
                    Duration::ZERO
                };
                return with_deadline(Yellow, op_reason(op), later(deadline, rest));
            }
            Activity::Starting { op, deadline, .. } => {
                return with_deadline(Yellow, op_reason(op), later(deadline, verify));
            }
            Activity::Idle | Activity::Backoff { .. } => {}
        }
        let next_retry = match self.activity {
            Activity::Backoff { retry_at } => Some(retry_at),
            _ => None,
        };
        let retrying = |reason| LightView {
            next_retry,
            ..view(Red, reason)
        };

        if self.rec.intent.desired == DesiredState::Disconnected {
            return if self.rec.external == Some(External::ExternallyControlled)
                || self.os == OsState::Connected
            {
                view(Red, R::ExternallyStarted)
            } else if self.os_error.is_none() && self.os == OsState::Unknown && self.in_grace(now) {
                with_deadline(Yellow, R::GracePeriod, self.grace_until)
            } else if self.os_error.is_some() || self.os == OsState::Unknown {
                // Unknown is never shown as a normal gray (plan §6.1).
                view(Red, R::StatusUnknown)
            } else {
                view(Gray, R::DisconnectedByUser)
            };
        }
        if self.os_error.is_some() {
            return view(Red, R::StatusUnknown);
        }
        if self.rec.external == Some(External::StoppedExternally) {
            return view(Red, R::StoppedExternally);
        }
        // The retry limit is used up (owner decision of 2026-11-04): WGAL no
        // longer tries for this incident, which still needs the user.
        let exhausted = self
            .rec
            .incident
            .as_ref()
            .is_some_and(|i| i.retries_exhausted);
        match self.os {
            OsState::Disconnected if next_retry.is_some() => return retrying(R::RecoveryFailed),
            OsState::Disconnected if exhausted => return view(Red, R::RetriesExhausted),
            OsState::Disconnected => return view(Gray, R::NotConnected),
            OsState::Transitioning => {
                // Yellow is always bounded (plan §6.1): a change that outlasts
                // a control phase is an error, not a longer wait.
                let deadline = later(self.os_since, phase);
                return if now >= deadline {
                    view(Red, R::StatusUnknown)
                } else {
                    with_deadline(Yellow, R::Starting, deadline)
                };
            }
            OsState::Unknown if self.in_grace(now) => {
                return with_deadline(Yellow, R::GracePeriod, self.grace_until);
            }
            OsState::Unknown => return view(Red, R::StatusUnknown),
            OsState::Connected => {}
        }
        if self.local_offline {
            return retrying(R::LocalNetworkOffline);
        }
        if self.rec.recovery_target.is_none() {
            return view(Gray, R::NotConfigured);
        }
        match self.path {
            Path::Conflict => return retrying(R::RouteConflict),
            Path::Unverified => return retrying(R::PathUnverified),
            Path::Unknown | Path::Verified => {}
        }
        if self.probe_error.is_some() {
            return retrying(R::ProbeUnavailable);
        }
        if !self.rec.baseline_passed && (self.failures > 0 || self.confirmed) {
            return view(Red, R::BaselineFailed);
        }
        if next_retry.is_some() {
            return retrying(R::RecoveryFailed);
        }
        if self.confirmed && exhausted {
            return view(Red, R::RetriesExhausted);
        }
        if self.confirmed {
            return view(Red, R::FaultConfirmed);
        }
        if let Some(first) = self.first_failure {
            return with_deadline(Yellow, R::Confirming, self.confirm_deadline(first));
        }
        if self.data_health(now) == DataHealth::Healthy {
            return view(Green, R::Verified);
        }
        if self.in_grace(now) {
            return with_deadline(Yellow, R::GracePeriod, self.grace_until);
        }
        view(Red, R::StatusUnknown)
    }
}

fn op_reason(op: Op) -> LightReason {
    match op {
        Op::Connect | Op::ExternalRestart => LightReason::Starting,
        Op::Recovery | Op::Reconnect => LightReason::Reconnecting,
        Op::Disconnect => LightReason::DisconnectedByUser,
    }
}
