//! Worker threads (owner's requests 6 and 7 of 2026-11-03): the effective
//! count within 2 and one more than the monitored tunnels, the suggestion,
//! threads started only when work needs one, retired when the count drops,
//! and the worker kept for the control slot. The engine is driven step by
//! step with a fake clock (its workers are counted, not real), and the
//! threads are checked through `spawn_with` with a fake platform.
//! Simulation only.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{MemoryStore, WorkItem, WorkKind};
use wgal_protocol::{ProbeTiming, Request, Snapshot};
use wgal_runtime::driver::{MAX_WORKERS, MIN_WORKERS, Options, spawn_with, suggested_workers};
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

const LONG: Duration = Duration::from_secs(10);

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("w{i:02}")).collect()
}

fn timing(interval: u16, timeout: u8) -> ProbeTiming {
    ProbeTiming::new(interval, timeout, 2, 60).unwrap()
}

/// `n` tunnels past the start-up grace period, the engine not turned yet.
fn sim_of(n: usize, paused: bool) -> Sim {
    let all = names(n);
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let lab = Lab::new(&refs);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &refs);
    if paused {
        assert!(matches!(
            wd.handle(&Request::SetMonitoringPaused { paused: true }),
            wgal_protocol::Outcome::Ok(_)
        ));
    }
    Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    )
}

fn rounds(work: &[WorkItem]) -> usize {
    work.iter()
        .filter(|i| matches!(i.kind, WorkKind::Round { .. }))
        .count()
}

/// Makes every tunnel's round due now.
fn all_due(sim: &mut Sim, n: usize) {
    for name in names(n) {
        sim.ok(Request::ProbeNow {
            tunnel_id: id(&name),
        });
    }
}

#[test]
fn the_suggestion_for_several_loads() {
    let default = |n: usize, targets: usize| vec![(ProbeTiming::DEFAULT, targets); n];
    // Nothing monitored, or one tunnel: the fewest.
    assert_eq!(suggested_workers(&[], 2), MIN_WORKERS);
    assert_eq!(suggested_workers(&default(1, 1), 2), 2);
    // The owner's two tunnels at the defaults: one round worker is enough,
    // since one round of the other waits at most 3 s of its 20 s slack.
    assert_eq!(suggested_workers(&default(2, 1), 3), 2);
    // Sixteen at the defaults: three round workers (wait at most
    // ceil(13 / 3) x 3 s = 15 s).
    assert_eq!(suggested_workers(&default(16, 1), 17), 4);
    assert_eq!(suggested_workers(&default(5, 1), 6), 2);
    assert_eq!(suggested_workers(&default(10, 1), 11), 3);
    // Five targets: a round of another tunnel (15 s) is longer than the 8 s
    // the four observe targets leave of the slack, so each gets its own.
    assert_eq!(suggested_workers(&default(2, 5), 3), 3);
    assert_eq!(suggested_workers(&default(16, 5), 17), 17);
    // Never above the limit, never below two.
    assert_eq!(suggested_workers(&default(16, 5), 5), 5);
    assert_eq!(suggested_workers(&default(16, 5), 0), MIN_WORKERS);
    // Rounds that probe nothing need no round worker of their own.
    let mut idle = default(15, 0);
    idle.push((ProbeTiming::DEFAULT, 1));
    assert_eq!(suggested_workers(&idle, 17), 2);
    // A longer timeout leaves more slack (28 s) but makes rounds longer
    // (11 s) by more: ceil((16 - k) / k) x 11 <= 28 first at k = 6.
    assert_eq!(suggested_workers(&vec![(timing(15, 10), 1); 16], 17), 7);
    // A long interval does not shorten anyone's slack, but the rounds of a
    // tunnel probed once an hour can still coincide with the others'.
    assert_eq!(suggested_workers(&vec![(timing(3600, 2), 1); 16], 17), 4);
    // The tightest tunnel decides: one with three targets among plain ones.
    let mut mixed = default(8, 1);
    mixed.push((ProbeTiming::DEFAULT, 3));
    // Slack 20 - 2 x 3 = 14 s; the longest round 9 s: ceil((9 - k) / k)
    // x 9 <= 14 first at k = 5.
    assert_eq!(suggested_workers(&mixed, 10), 6);
}

#[test]
fn no_worker_is_started_before_there_is_work() {
    // Host-wide pause: nothing is monitored, nothing is due.
    let mut sim = sim_of(3, true);
    assert_eq!(sim.engine.stats().workers, 0);
    sim.run(120);
    let host = sim.host();
    assert_eq!(host.workers.running, 0);
    assert_eq!(host.workers.limit, 2, "paused tunnels do not count");
    assert_eq!(host.workers.effective, 2);
    assert_eq!(sim.engine.stats().workers, 0);

    // Monitoring again: rounds need workers, as few as the suggestion.
    sim.ok(Request::SetMonitoringPaused { paused: false });
    sim.run(60);
    let host = sim.host();
    assert_eq!(host.workers.limit, 4);
    assert_eq!(host.workers.suggested, 2);
    assert_eq!(host.workers.effective, 2);
    assert!(
        host.workers.running >= 1 && host.workers.running <= 2,
        "{host:?}"
    );
}

#[test]
fn workers_grow_only_to_the_effective_count() {
    let mut sim = sim_of(6, false);
    sim.settle();
    assert!(accepted(&sim.host_settings(Some(4), None)));
    assert_eq!(sim.host().workers.effective, 4);
    all_due(&mut sim, 6);
    let (work, _) = sim.turn_only();
    // Rounds take at most effective - 1 workers, and only those are started.
    assert_eq!(rounds(&work), 3);
    assert_eq!(sim.engine.stats().workers, 3);
    // A stop takes the fourth.
    sim.ok(Request::Disconnect {
        tunnel_id: id("w05"),
    });
    let (more, _) = sim.turn_only();
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].kind, WorkKind::Stop);
    assert_eq!(sim.engine.stats().workers, 4);
    // Nothing beyond: the other rounds wait.
    let (none, _) = sim.turn_only();
    assert!(none.is_empty());
    assert_eq!(sim.engine.stats().workers, 4);
    assert!(sim.engine.stats().ready >= 2);
    for item in work.iter().chain(&more) {
        sim.finish(item);
    }
    sim.settle();
    assert!(sim.engine.stats().workers <= 4);

    // A configured count above the limit is held to it.
    assert!(accepted(&sim.host_settings(Some(17), None)));
    let host = sim.host();
    assert_eq!((host.workers.limit, host.workers.effective), (7, 7));
    // And never above `Options::workers` either (a cap for tests).
}

#[test]
fn lowering_the_count_retires_workers_once_they_are_idle() {
    let mut sim = sim_of(6, false);
    sim.settle();
    assert!(accepted(&sim.host_settings(Some(7), None)));
    all_due(&mut sim, 6);
    let (work, _) = sim.turn_only();
    assert_eq!(rounds(&work), 6);
    assert_eq!(sim.engine.stats().workers, 6);

    // Lowered while all six are busy: none can exit yet, and no round is
    // handed out.
    assert!(accepted(&sim.host_settings(Some(2), None)));
    sim.turn_only();
    assert_eq!(sim.engine.stats().workers, 6);
    // A stop still gets a worker at once, a seventh: slot work never waits
    // behind rounds, even ones out from before the count was lowered.
    sim.ok(Request::Disconnect {
        tunnel_id: id("w00"),
    });
    let (stop, _) = sim.turn_only();
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0].kind, WorkKind::Stop);
    assert_eq!(sim.engine.stats().workers, 7);

    // Each finished item lets its worker go, down to the count.
    let mut items: Vec<WorkItem> = work.into_iter().chain(stop).collect();
    let mut seen = Vec::new();
    while let Some(item) = items.pop() {
        sim.finish(&item);
        let (next, _) = sim.turn_only();
        seen.push(sim.engine.stats().workers);
        items.extend(next);
    }
    sim.settle();
    assert_eq!(sim.engine.stats().workers, 2, "{seen:?}");
    assert!(seen.windows(2).all(|w| w[1] <= w[0] + 1), "{seen:?}");
    assert_eq!(sim.host().workers.running, 2);
    assert_eq!(sim.host().workers.effective, 2);
}

#[test]
fn pausing_tunnels_lowers_the_limit_and_retires_workers() {
    let mut sim = sim_of(4, false);
    sim.settle();
    assert!(accepted(&sim.host_settings(Some(5), None)));
    all_due(&mut sim, 4);
    sim.settle();
    assert_eq!(sim.engine.stats().workers, 4);
    sim.ok(Request::SetMonitoringPaused { paused: true });
    sim.settle();
    let host = sim.host();
    assert_eq!((host.workers.limit, host.workers.effective), (2, 2));
    assert_eq!(host.workers.running, 2);
    assert_eq!(host.workers.configured.map(|c| c.get()), Some(5));
}

/// The control slot with the fewest workers and many tunnels: rounds fall
/// behind, one at a time, but a stop never waits behind them.
#[test]
fn the_control_slot_stays_free_with_two_workers_and_many_tunnels() {
    let mut sim = sim_of(10, false);
    sim.settle();
    assert!(accepted(&sim.host_settings(Some(2), None)));
    all_due(&mut sim, 10);
    let (work, _) = sim.turn_only();
    assert_eq!(rounds(&work), 1, "one round worker");
    assert_eq!(sim.engine.stats().ready, 9);
    for (i, name) in ["w03", "w07"].into_iter().enumerate() {
        sim.ok(Request::Disconnect {
            tunnel_id: id(name),
        });
        let (stop, _) = sim.turn_only();
        assert_eq!(stop.len(), 1, "{i}: {stop:?}");
        assert_eq!(stop[0].kind, WorkKind::Stop);
        assert_eq!(sim.engine.stats().workers, 2);
        sim.finish(&stop[0]);
        // Its polls and the rest go on through the same worker.
        sim.settle();
        let (out, _) = sim.turn_only();
        assert!(out.is_empty());
    }
    sim.finish(&work[0]);
    sim.run(120);
    assert!(sim.engine.stats().workers <= 2);
    assert_eq!(
        sim.status("w03").os_state,
        wgal_protocol::OsState::Disconnected
    );
    assert_eq!(
        sim.status("w07").os_state,
        wgal_protocol::OsState::Disconnected
    );
}

fn snapshot(client: &wgal_runtime::Client) -> Snapshot {
    snapshot_of(&client.call(&user(), envelope("snap", Request::GetSnapshot), LONG))
}

fn eventually(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + LONG;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The real threads: none before work, started as rounds need them, one
/// more for a stop, and down again when the count is lowered.
#[test]
fn threads_are_started_for_work_and_exit_when_the_count_drops() {
    let all = names(3);
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let lab = Lab::new(&refs);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &refs);
    let options = Options {
        max_wait: Duration::from_millis(10),
        ..Options::default()
    };
    let (client, running) = spawn_with(
        wd,
        quiet_notifier(),
        Box::new(Script::default()),
        AccessList::default(),
        None,
        &options,
    )
    .unwrap();
    let call = |request| client.call(&user(), envelope("r", request), LONG);
    // The fake clock stands still: nothing is due, no thread is started.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(snapshot(&client).host.workers.running, 0);

    let gates: Vec<_> = refs.iter().map(|n| lab.gate(n, Gated::Probe)).collect();
    let set = |workers: u8| {
        call(Request::UpdateHostSettings {
            settings: wgal_protocol::HostSettings {
                workers: Some(wgal_protocol::WorkerCount::new(workers).unwrap()),
                time_zone: None,
            },
        })
    };
    assert!(accepted(&set(4)));
    for name in &refs {
        assert!(accepted(&call(Request::ProbeNow {
            tunnel_id: id(name)
        })));
    }
    for gate in &gates {
        assert!(gate.wait_entered(LONG));
    }
    assert!(eventually(|| snapshot(&client).host.workers.running == 3));
    // A stop of another tunnel takes a fourth thread while the rounds hang.
    let stop_gate = lab.gate("w00", Gated::Stop);
    assert!(accepted(&call(Request::Disconnect {
        tunnel_id: id("w00")
    })));
    assert!(stop_gate.wait_entered(LONG));
    let host = snapshot(&client).host.workers;
    assert_eq!((host.running, host.effective), (4, 4));

    // Lowered: the threads above the count exit once their items are done.
    assert!(accepted(&set(2)));
    assert_eq!(snapshot(&client).host.workers.running, 4, "all still busy");
    stop_gate.open();
    for gate in &gates {
        gate.open();
    }
    assert!(
        eventually(|| snapshot(&client).host.workers.running == 2),
        "{:?}",
        snapshot(&client).host
    );
    assert!(running.stop(LONG));
}

/// Sixteen tunnels with every probe held: one thread per round, one for a
/// stop, never more than `MAX_WORKERS`.
#[test]
fn the_pool_never_passes_its_bound() {
    let all = names(16);
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let lab = Lab::new(&refs);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &refs);
    let options = Options {
        max_wait: Duration::from_millis(10),
        ..Options::default()
    };
    let (client, running) = spawn_with(
        wd,
        quiet_notifier(),
        Box::new(Script::default()),
        AccessList::default(),
        None,
        &options,
    )
    .unwrap();
    let call = |request| client.call(&admin(), envelope("r", request), LONG);
    let gates: Vec<_> = refs.iter().map(|n| lab.gate(n, Gated::Probe)).collect();
    assert!(accepted(&call(Request::UpdateHostSettings {
        settings: wgal_protocol::HostSettings {
            workers: Some(wgal_protocol::WorkerCount::new(17).unwrap()),
            time_zone: None,
        },
    })));
    for name in &refs {
        assert!(accepted(&call(Request::ProbeNow {
            tunnel_id: id(name)
        })));
    }
    for gate in &gates {
        assert!(gate.wait_entered(LONG));
    }
    assert!(eventually(|| snapshot(&client).host.workers.running == 16));
    thread::sleep(Duration::from_millis(100));
    let host = snapshot(&client).host.workers;
    assert_eq!(usize::from(host.running), 16);
    assert_eq!(usize::from(host.effective), MAX_WORKERS);
    for gate in &gates {
        gate.open();
    }
    assert!(running.stop(LONG));
}
