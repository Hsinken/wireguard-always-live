//! Units of platform work the watchdog plans, and their execution.
//!
//! The watchdog never calls the adapter while deciding: it plans a
//! [`WorkItem`], [`execute`] runs it against the adapter, and the watchdog
//! applies the [`WorkResult`]. Each item carries the tunnel's operation
//! generation and a sequence number, and a result that no longer matches is
//! discarded, so a probe planned before a disconnect or reconnect can never be
//! credited to the new state. It also carries the tunnel's probe generation,
//! which a wake or network change advances: the probes of a round planned
//! before that are ignored, while a stop, start or state reading still
//! counts, so an operation under way finishes.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use wgal_protocol::{AdapterErrorKind, OsState, ProbeTarget, TunnelId};

use crate::clock::{Clock, MonoInstant};
use crate::platform::{
    AdapterError, PlatformAdapter, ProbeOutcome, ProbeRequest, ResolvedInterface, RouteEvidence,
    TunnelBinding,
};

/// Which of a tunnel's targets a probe was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetRole {
    Recovery,
    /// Index into the tunnel's observe targets.
    Observe(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkKind {
    /// Read the OS state, while waiting for a stop or start to settle.
    Poll,
    Stop,
    Start,
    /// A monitoring or verification round: read the OS state and, if the
    /// tunnel is connected, resolve its interface, check the route to and
    /// probe each target, then resolve the interface again.
    Round {
        targets: Vec<(TargetRole, ProbeTarget)>,
        probe_timeout: Duration,
    },
}

/// One unit of planned work. `generation` is the tunnel's operation
/// generation and `probe_generation` its probe generation when the item was
/// planned (see the module documentation); `seq` is unique within the
/// watchdog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkItem {
    pub tunnel_id: TunnelId,
    pub binding: TunnelBinding,
    pub generation: u64,
    pub probe_generation: u64,
    pub seq: u64,
    pub kind: WorkKind,
}

impl WorkItem {
    /// Whether running this item changes the tunnel. Such items must be
    /// executed and applied without another request for the same tunnel in
    /// between; probes and polls may run concurrently with anything.
    pub fn is_control(&self) -> bool {
        matches!(self.kind, WorkKind::Stop | WorkKind::Start)
    }
}

/// One probe of one target. `finished` is the monotonic time the probe
/// completed: a result is only as fresh as that, however late it is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetProbe {
    pub role: TargetRole,
    pub target: ProbeTarget,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub finished: MonoInstant,
    pub result: Result<(RouteEvidence, ProbeOutcome), AdapterError>,
}

/// The probing part of a round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeSet {
    pub interface: Result<ResolvedInterface, AdapterError>,
    pub probes: Vec<TargetProbe>,
    /// The interface resolved after the probes differs from the one before
    /// in any field, its incarnation included, so the probes may have gone
    /// through something else and must be ignored.
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkOutcome {
    Poll(Result<OsState, AdapterError>),
    Command(Result<(), AdapterError>),
    Round {
        os: Result<OsState, AdapterError>,
        /// `None` when the tunnel was not connected or there was nothing to
        /// probe.
        probes: Option<ProbeSet>,
    },
}

/// What came of a [`WorkItem`], with the item's generations and `seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkResult {
    pub tunnel_id: TunnelId,
    pub generation: u64,
    pub probe_generation: u64,
    pub seq: u64,
    /// Monotonic time the work finished.
    pub finished: MonoInstant,
    pub outcome: WorkOutcome,
}

/// Runs one item against the adapter. Every adapter call is bounded by the
/// adapter itself, so this returns within the sum of those deadlines.
pub fn execute<A, C>(adapter: &A, clock: &C, item: &WorkItem) -> WorkResult
where
    A: PlatformAdapter + ?Sized,
    C: Clock + ?Sized,
{
    let binding = &item.binding;
    let outcome = match &item.kind {
        WorkKind::Poll => WorkOutcome::Poll(adapter.query_state(binding)),
        // Stopping or starting runs the manager's own hooks with its
        // privileges, so the registration check is repeated right before
        // every command (plan §4.4, §9.2).
        WorkKind::Stop => WorkOutcome::Command(
            adapter
                .check_registration(binding)
                .and_then(|()| adapter.stop(binding)),
        ),
        WorkKind::Start => WorkOutcome::Command(
            adapter
                .check_registration(binding)
                .and_then(|()| adapter.start(binding)),
        ),
        WorkKind::Round {
            targets,
            probe_timeout,
        } => {
            let os = adapter.query_state(binding);
            let probes = (matches!(os, Ok(OsState::Connected)) && !targets.is_empty())
                .then(|| probe_round(adapter, clock, binding, targets, *probe_timeout));
            WorkOutcome::Round { os, probes }
        }
    };
    WorkResult {
        tunnel_id: item.tunnel_id.clone(),
        generation: item.generation,
        probe_generation: item.probe_generation,
        seq: item.seq,
        finished: clock.now(),
        outcome,
    }
}

/// Runs planned work away from the watchdog, for a runtime that executes
/// items on threads of its own while another thread keeps the watchdog (see
/// [`crate::Watchdog::plan`]). It shares the watchdog's adapter and clock and
/// holds nothing else, so it can be cloned onto any number of threads.
pub struct Executor<A, C> {
    adapter: Arc<A>,
    clock: Arc<C>,
}

impl<A, C> Clone for Executor<A, C> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
            clock: Arc::clone(&self.clock),
        }
    }
}

impl<A, C> fmt::Debug for Executor<A, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

impl<A: PlatformAdapter, C: Clock> Executor<A, C> {
    pub(crate) fn new(adapter: Arc<A>, clock: Arc<C>) -> Self {
        Self { adapter, clock }
    }

    /// [`execute`] with the watchdog's adapter and clock.
    pub fn run(&self, item: &WorkItem) -> WorkResult {
        execute(&*self.adapter, &*self.clock, item)
    }

    /// The result for an item whose execution broke off without one, such
    /// as an adapter that panicked: `error` stands for every call the item
    /// would have made, so the watchdog applies it like any failed call
    /// instead of waiting for the item forever. `error` must not be
    /// `NotFound`, which would mark the binding lost.
    pub fn failed(&self, item: &WorkItem, error: AdapterError) -> WorkResult {
        let outcome = match item.kind {
            WorkKind::Poll => WorkOutcome::Poll(Err(error)),
            WorkKind::Stop | WorkKind::Start => WorkOutcome::Command(Err(error)),
            WorkKind::Round { .. } => WorkOutcome::Round {
                os: Err(error),
                probes: None,
            },
        };
        WorkResult {
            tunnel_id: item.tunnel_id.clone(),
            generation: item.generation,
            probe_generation: item.probe_generation,
            seq: item.seq,
            finished: self.clock.now(),
            outcome,
        }
    }
}

fn route_moved(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::RouteMismatch, detail)
}

fn probe_round<A, C>(
    adapter: &A,
    clock: &C,
    binding: &TunnelBinding,
    targets: &[(TargetRole, ProbeTarget)],
    probe_timeout: Duration,
) -> ProbeSet
where
    A: PlatformAdapter + ?Sized,
    C: Clock + ?Sized,
{
    let before = match adapter.resolve_interface(binding) {
        Ok(interface) => interface,
        Err(error) => {
            return ProbeSet {
                interface: Err(error),
                probes: Vec::new(),
                stale: false,
            };
        }
    };
    let probes = targets
        .iter()
        .map(|&(role, target)| {
            let started_at_unix_ms = clock.wall_unix_ms();
            let result = adapter
                .check_route(&before, target.address)
                .and_then(|evidence| {
                    if evidence.interface_name != before.name {
                        return Err(route_moved("route leaves through another interface"));
                    }
                    let request = ProbeRequest {
                        target,
                        timeout: probe_timeout,
                    };
                    let outcome = adapter.probe(&before, &request)?;
                    // Plan §5.2: route evidence from before the probe proves
                    // nothing if the route moved while it ran.
                    let after = adapter.check_route(&before, target.address)?;
                    if after != evidence {
                        return Err(route_moved("the route changed during the probe"));
                    }
                    Ok((evidence, outcome))
                });
            TargetProbe {
                role,
                target,
                started_at_unix_ms,
                finished_at_unix_ms: clock.wall_unix_ms(),
                finished: clock.now(),
                result,
            }
        })
        .collect();
    let stale = adapter.resolve_interface(binding).as_ref() != Ok(&before);
    ProbeSet {
        interface: Ok(before),
        probes,
        stale,
    }
}
