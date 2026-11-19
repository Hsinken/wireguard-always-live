//! Capacity for many tunnels (owner's request before the day-2 build, and
//! the adjustable worker count of wave three): up to `MAX_TUNNELS` (16)
//! tunnels, each with a recovery target and up to four observe targets,
//! every probe running into its 2 s timeout.
//!
//! The engine is driven by a timed simulation on the fake clock: an item
//! handed out takes the time it would take on a real adapter and its result
//! comes back then, so rounds that have to wait for a worker show up as
//! lateness. A round probes its targets one after the other, the recovery
//! target first: an answer takes 10 ms, a probe without one its timeout (2 s
//! by default), and each probe is dated by when it finished, not by when the
//! round came back (Codex review of wave three, P2-5). A stop, start or poll
//! takes 0.1 s. Simulation only: no device, no real probe.

#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use wgal_core::work::WorkOutcome;
use wgal_core::{Clock, MemoryStore, ProbeOutcome, WorkItem, WorkKind, WorkResult};
use wgal_protocol::{
    BoundedVec, DataHealth, FirstRecovery, HostSettings, MonitorSettings, ProbeTiming,
    RecoveryPhase, Request, TunnelId, WorkerCount,
};
use wgal_runtime::driver::{MAX_WORKERS, MIN_WORKERS, Options, suggested_workers};
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

/// What a probe that gets an answer takes.
const ANSWER: Duration = Duration::from_millis(10);
/// What a round takes besides its probes: the state read, resolving the
/// interface before and after, the route checks.
const ROUND_REST: Duration = Duration::from_millis(50);
/// What a stop, start or poll takes.
const CONTROL: Duration = Duration::from_millis(100);

/// The probe timing every simulated tunnel has, and what a probe that gets
/// no answer takes beyond its timeout: nothing for the table (as measured
/// before), a second for the worst case the suggestion and the freshness
/// allow for (Ubuntu's and macOS's ping exit up to a second late).
#[derive(Clone, Copy, Debug)]
struct Probing {
    timing: ProbeTiming,
    overhead: Duration,
}

const DEFAULT_PROBING: Probing = Probing {
    timing: ProbeTiming::DEFAULT,
    overhead: Duration::ZERO,
};

impl Probing {
    fn interval(self) -> Duration {
        Duration::from_secs(u64::from(self.timing.interval_secs()))
    }
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap()
}

/// Runs `item` at the fake clock's time, as a worker would, and returns how
/// long it takes in all and its result as the platform would give it then:
/// each probe of a round finishes in turn, dated by its own end.
fn run_item(sim: &Sim, item: &WorkItem, probing: Probing) -> (Duration, WorkResult) {
    let mut result = sim.engine.executor().run(item);
    let start = result.finished;
    let took = match (&item.kind, &mut result.outcome) {
        (
            WorkKind::Round { probe_timeout, .. },
            WorkOutcome::Round {
                probes: Some(set), ..
            },
        ) => {
            let wall = sim.clock.wall_unix_ms();
            let mut took = Duration::ZERO;
            for probe in &mut set.probes {
                probe.started_at_unix_ms = wall + millis(took);
                let answered = matches!(
                    probe.result,
                    Ok((_, ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. }))
                );
                took += if answered {
                    ANSWER
                } else {
                    *probe_timeout + probing.overhead
                };
                probe.finished = start + took;
                probe.finished_at_unix_ms = wall + millis(took);
            }
            took + ROUND_REST
        }
        (WorkKind::Round { .. }, _) => ROUND_REST,
        _ => CONTROL,
    };
    result.finished = start + took;
    (took, result)
}

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("t{i:02}")).collect()
}

/// `n` registered tunnels with `targets` targets each (the recovery target
/// and `targets - 1` observe targets, 192.0.2.200 and up) and `timing`, past
/// the start-up grace period.
fn rig_of(n: usize, targets: usize, timing: ProbeTiming) -> (Lab, Arc<FakeClock>, MemoryStore, Wd) {
    let names = names(n);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let lab = Lab::new(&refs);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &refs);
    for (i, name) in names.iter().enumerate() {
        let observe: Vec<_> = (0..targets - 1)
            .map(|j| icmp(u8::try_from(200 + j).unwrap()))
            .collect();
        let request = Request::UpdateMonitorSettings {
            tunnel_id: id(name),
            settings: MonitorSettings {
                recovery_target: Some(icmp(u8::try_from(i + 1).unwrap())),
                observe_targets: BoundedVec::try_from(observe).unwrap(),
                probe: timing,
            },
        };
        assert!(matches!(wd.handle(&request), wgal_protocol::Outcome::Ok(_)));
    }
    (lab, clock, store, wd)
}

/// Runs the engine for `secs` of fake time with items taking as long as
/// [`run_item`] says. `dispatched` and `finished` see each item when it is
/// handed out and when its result comes back (with the time it came back).
struct Timed {
    sim: Sim,
    probing: Probing,
    out: Vec<(Duration, WorkItem, WorkResult)>,
}

impl Timed {
    fn new(sim: Sim, probing: Probing) -> Self {
        Self {
            sim,
            probing,
            out: Vec::new(),
        }
    }

    fn now(&self) -> Duration {
        self.sim.clock.now().since_origin()
    }

    fn run(
        &mut self,
        secs: u64,
        mut dispatched: impl FnMut(&mut Sim, &WorkItem, Duration),
        mut finished: impl FnMut(&mut Sim, &WorkItem, Duration),
    ) {
        let end = self.now() + Duration::from_secs(secs);
        while self.now() < end {
            let now = self.now();
            let (work, _) = self.sim.turn_only();
            for item in work {
                dispatched(&mut self.sim, &item, now);
                let (took, result) = run_item(&self.sim, &item, self.probing);
                self.out.push((now + took, item, result));
            }
            let wake = now + self.sim.engine.idle_wait();
            let next = self
                .out
                .iter()
                .map(|(done, ..)| *done)
                .min()
                .map_or(wake, |d| d.min(wake))
                .min(end);
            assert!(next > now || now >= end, "the loop would spin");
            self.sim.clock.advance(next.saturating_sub(now)).unwrap();
            let now = self.now();
            self.out.sort_by_key(|(done, ..)| *done);
            while self.out.first().is_some_and(|(done, ..)| *done <= now) {
                let (done, item, result) = self.out.remove(0);
                let answers = self.sim.engine.finished(result);
                self.sim.released.extend(answers);
                finished(&mut self.sim, &item, done);
            }
        }
    }
}

#[derive(Debug)]
struct Report {
    tunnels: usize,
    targets: usize,
    /// Longest a round started after it was due (one interval after the
    /// previous round of the tunnel came back).
    max_late: Duration,
    /// Longest time between two rounds of one tunnel starting.
    max_period: Duration,
    /// When each tunnel's fault was confirmed, after the data paths died.
    confirmed: Vec<Option<Duration>>,
    most_rounds_at_once: usize,
    /// Most worker threads started at once.
    workers: usize,
    /// The effective worker count.
    effective: usize,
    plan_passes: u64,
}

/// `n` tunnels with `targets` targets each and the default timing, all dead
/// from the start, for `secs`, with `workers` configured (`None`: the
/// suggestion).
fn simulate(n: usize, targets: usize, options: &Options, workers: Option<u8>, secs: u64) -> Report {
    simulate_with(n, targets, DEFAULT_PROBING, options, workers, secs)
}

/// [`simulate`] with another probe timing.
fn simulate_with(
    n: usize,
    targets: usize,
    probing: Probing,
    options: &Options,
    workers: Option<u8>,
    secs: u64,
) -> Report {
    let (lab, clock, store, mut wd) = rig_of(n, targets, probing.timing);
    wd.set_host_settings(HostSettings {
        workers: workers.map(|w| WorkerCount::new(w).unwrap()),
        time_zone: None,
    })
    .unwrap();
    let sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        options,
    );
    for name in names(n) {
        lab.kill_for_good(&name);
    }
    let mut timed = Timed::new(sim, probing);
    let killed = timed.now();
    let plan_before = timed.sim.engine.stats().plan_passes;
    let mut last_done: BTreeMap<TunnelId, Duration> = BTreeMap::new();
    let mut last_start: BTreeMap<TunnelId, Duration> = BTreeMap::new();
    let mut confirmed: BTreeMap<TunnelId, Duration> = BTreeMap::new();
    let mut max_late = Duration::ZERO;
    let mut max_period = Duration::ZERO;
    let mut most = 0;
    let mut workers = 0;
    // Both callbacks keep their own state; the finished one hands back what
    // the dispatched one needs through these cells.
    let done_at = std::cell::RefCell::new(&mut last_done);
    let running = std::cell::Cell::new(0_usize);
    timed.run(
        secs,
        |sim, item, now| {
            workers = workers.max(sim.engine.stats().workers);
            if matches!(item.kind, WorkKind::Round { .. }) {
                if let Some(done) = done_at.borrow().get(&item.tunnel_id) {
                    max_late = max_late.max(now.saturating_sub(*done + probing.interval()));
                }
                if let Some(start) = last_start.insert(item.tunnel_id.clone(), now) {
                    max_period = max_period.max(now - start);
                }
                running.set(running.get() + 1);
                most = most.max(running.get());
            }
        },
        |sim, item, done| {
            if matches!(item.kind, WorkKind::Round { .. }) {
                running.set(running.get() - 1);
                done_at.borrow_mut().insert(item.tunnel_id.clone(), done);
                if !confirmed.contains_key(&item.tunnel_id)
                    && sim.status(item.tunnel_id.as_str()).data_health == DataHealth::Failed
                {
                    confirmed.insert(item.tunnel_id.clone(), done - killed);
                }
            }
        },
    );
    Report {
        tunnels: n,
        targets,
        max_late,
        max_period,
        confirmed: names(n)
            .iter()
            .map(|n| confirmed.get(&id(n)).copied())
            .collect(),
        most_rounds_at_once: most,
        workers,
        effective: timed.sim.engine.stats().effective,
        plan_passes: timed.sim.engine.stats().plan_passes - plan_before,
    }
}

fn line(r: &Report) -> String {
    let confirmed: Vec<u64> = r
        .confirmed
        .iter()
        .flatten()
        .map(Duration::as_secs)
        .collect();
    format!(
        "{:>2} tunnels x {} targets: effective {:>2}, started {:>2}, rounds at once {:>2}, max late {:>5.1} s, \
         max period {:>5.1} s, confirmed {}/{} (slowest {} s), plan passes {}",
        r.tunnels,
        r.targets,
        r.effective,
        r.workers,
        r.most_rounds_at_once,
        r.max_late.as_secs_f64(),
        r.max_period.as_secs_f64(),
        confirmed.len(),
        r.tunnels,
        confirmed
            .iter()
            .max()
            .map_or("-".to_owned(), u64::to_string),
        r.plan_passes,
    )
}

/// The numbers for the report: 2, 5, 10 and 16 tunnels, 1 and 5 targets,
/// all timing out, for ten simulated minutes: with the suggested count (the
/// default), with one worker per tunnel plus one (the limit, configured),
/// and capped at four workers as the old fixed pool was, for comparison.
#[test]
fn capacity_table() {
    let old = Options {
        workers: 4,
        ..Options::default()
    };
    for (label, options, workers) in [
        ("suggested", Options::default(), None),
        ("limit", Options::default(), Some(17)),
        ("4 workers", old, Some(17)),
    ] {
        println!("{label}:");
        for n in [2, 5, 10, 16] {
            for targets in [1, 5] {
                println!("  {}", line(&simulate(n, targets, &options, workers, 600)));
            }
        }
    }
}

/// The owner's case: 16 tunnels, five targets each, every probe timing
/// out. The suggestion is a worker for each tunnel plus one; every tunnel's
/// rounds start when they are due (15 s after its last one came back),
/// every fault is confirmed as soon as 3 rounds over 60 s allow, and the
/// loop does not spin.
#[test]
fn sixteen_tunnels_with_five_dead_targets_keep_their_schedule() {
    let report = simulate(16, 5, &Options::default(), None, 900);
    println!("{}", line(&report));
    assert_eq!(report.effective, MAX_WORKERS);
    assert!(
        report.max_late <= Duration::from_secs(1),
        "rounds fell behind: {report:?}"
    );
    // 10 s of timeouts per round, then the 15 s interval.
    assert!(report.max_period <= Duration::from_millis(25_100) + Duration::from_secs(1));
    // The first failure comes at the latest one period in, the third two
    // periods later, and at least 60 s after the first: about 75 s, plus
    // the first round's offset.
    for (i, confirmed) in report.confirmed.iter().enumerate() {
        let at = confirmed.unwrap_or_else(|| panic!("t{i:02} was never confirmed"));
        assert!(
            at <= Duration::from_secs(110),
            "t{i:02} confirmed after {at:?}"
        );
    }
    // Rounds only, no slot work: one worker per tunnel was started, not
    // the one kept for the control slot.
    assert_eq!(report.workers, 16);
    assert_eq!(report.most_rounds_at_once, 16);
    // About one turn per round result and per wake-up: 16 tunnels, one round
    // every 25 s each, plus 5 s ticks, over 900 s.
    assert!(
        report.plan_passes < 900,
        "{} plan passes",
        report.plan_passes
    );
}

/// The suggestion keeps its promise in the simulation: with every probe
/// timing out, no round waits for a worker longer than the slack the
/// formula allows for (20 s with one target at the defaults, 8 s with
/// five), whatever the number of tunnels, and every fault is confirmed.
#[test]
fn the_suggestion_keeps_rounds_on_time() {
    for (n, targets, slack) in [
        (2, 1, 20),
        (5, 1, 20),
        (10, 1, 20),
        (16, 1, 20),
        (5, 5, 8),
        (10, 5, 8),
    ] {
        let report = simulate(n, targets, &Options::default(), None, 900);
        println!("{}", line(&report));
        let load = vec![(ProbeTiming::DEFAULT, targets); n];
        assert_eq!(report.effective, suggested_workers(&load, n + 1));
        assert!(
            report.max_late <= Duration::from_secs(slack),
            "{n} x {targets}: {report:?}"
        );
        assert!(report.confirmed.iter().all(Option::is_some), "{report:?}");
    }
}

/// A small pool, so rounds do wait for a worker: the wait is the loop's own
/// queueing, not time the host was away. A tunnel whose recovery is being
/// verified still fails its round at the 45 s deadline (judged when a late
/// verification round comes back) instead of having the deadline pushed out
/// by every late round, and the fault is reported.
#[test]
fn queueing_for_a_worker_is_never_taken_for_a_stall() {
    let (lab, clock, store, mut wd) = rig_of(16, 5, ProbeTiming::DEFAULT);
    manage(&mut wd, "t00");
    let options = Options {
        workers: MIN_WORKERS,
        ..Options::default()
    };
    let sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &options,
    );
    for name in names(16) {
        lab.kill_for_good(&name);
    }
    let mut timed = Timed::new(sim, DEFAULT_PROBING);
    let mut verifying_since: Option<Duration> = None;
    let mut verification_took: Option<Duration> = None;
    let mut longest_wait = Duration::ZERO;
    let due = std::cell::RefCell::new(BTreeMap::<TunnelId, Duration>::new());
    timed.run(
        1800,
        |_, item, now| {
            if matches!(item.kind, WorkKind::Round { .. })
                && let Some(done) = due.borrow().get(&item.tunnel_id)
            {
                longest_wait = longest_wait.max(now.saturating_sub(*done));
            }
        },
        |sim, item, done| {
            if item.tunnel_id != id("t00") {
                return;
            }
            due.borrow_mut().insert(item.tunnel_id.clone(), done);
            let phase = sim.status("t00").recovery;
            match (phase, verifying_since) {
                (RecoveryPhase::Verifying, None) => verifying_since = Some(done),
                (RecoveryPhase::Verifying, Some(_)) => {}
                (_, Some(since)) if verification_took.is_none() => {
                    verification_took = Some(done - since);
                }
                _ => {}
            }
        },
    );
    let took = verification_took.expect("the verification never ended");
    println!(
        "verification took {:.1} s; longest wait for a worker {:.1} s",
        took.as_secs_f64(),
        longest_wait.as_secs_f64()
    );
    // The deadline is 45 s; the round that sees it may have waited for the
    // one worker behind fifteen 10 s rounds.
    assert!(
        took <= Duration::from_secs(45 + 170),
        "verification took {took:?}"
    );
    assert!(longest_wait > Duration::from_secs(35), "rounds did queue");
    let incident = timed.sim.status("t00").incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::Failed);
}

/// Codex review of wave three, P2-5, the reviewer's settings: interval
/// 10 s, timeout 10 s and four observe targets, every probe without an
/// answer taking its timeout plus a second. The recovery target is probed
/// first and the observe targets take 44 s more before the round comes
/// back. That was older than the old freshness (38 s), so the recovery
/// target's results were all thrown away; the simulation used to date every
/// probe by the end of its round and could not show it.
///
/// Recovery targets that answer keep their tunnels healthy at every round,
/// and recovery targets that do not are confirmed as faults.
#[test]
fn long_rounds_keep_their_recovery_results_fresh() {
    let probing = Probing {
        timing: ProbeTiming::new(10, 10, 2, 60).unwrap(),
        overhead: Duration::from_secs(1),
    };
    let (lab, clock, store, wd) = rig_of(2, 5, probing.timing);
    for last in 200..204 {
        lab.kill_target(last);
    }
    let sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    );
    let mut timed = Timed::new(sim, probing);
    let mut rounds = 0;
    let mut unhealthy = Vec::new();
    timed.run(
        600,
        |_, _, _| {},
        |sim, item, done| {
            if matches!(item.kind, WorkKind::Round { .. }) {
                rounds += 1;
                let health = sim.status(item.tunnel_id.as_str()).data_health;
                if health != DataHealth::Healthy {
                    unhealthy.push((item.tunnel_id.clone(), done, health));
                }
            }
        },
    );
    // A round every 44 s plus the interval: about ten each in 600 s.
    assert!(rounds >= 18, "{rounds} rounds");
    assert!(unhealthy.is_empty(), "{unhealthy:?}");

    let report = simulate_with(2, 5, probing, &Options::default(), None, 600);
    println!("{}", line(&report));
    for (i, confirmed) in report.confirmed.iter().enumerate() {
        // Three failed rounds 65 s apart, the first within one round.
        let at = confirmed.unwrap_or_else(|| panic!("t{i:02} was never confirmed"));
        assert!(
            at <= Duration::from_secs(200),
            "t{i:02} confirmed after {at:?}"
        );
    }
    assert_eq!(
        timed.sim.status("t00").freshness_secs,
        10 + 10 + 18 + 4 * 11
    );
}
