//! The loop's threads ([`wgal_runtime::driver::spawn_with`]): requests
//! answered while platform work or a notification blocks, the control rule
//! across threads, deadlines, shutdown, wake-ups and idling. The fake
//! platform holds calls at gates the test opens; time is a fake clock the
//! test advances, so the owner thread is woken every 10 ms to look at it.
//! Simulation only, with real threads and short real timeouts.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::{Clock, MemoryStore, ResumeOp};
use wgal_protocol::{
    DesiredState, ErrorCode, ErrorReason, Light, LightReason, NotificationState, OsState, Request,
    Snapshot,
};
use wgal_runtime::driver::{Client, Options, Running, spawn_with};
use wgal_runtime::events::EventLog;
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

const LONG: Duration = Duration::from_secs(10);

fn fast() -> Options {
    Options {
        max_wait: Duration::from_millis(10),
        ..Options::default()
    }
}

fn start(
    wd: Wd,
    notifier: Service,
    transport: Script,
    events: Option<EventLog>,
    options: &Options,
) -> (Client, Running) {
    spawn_with(
        wd,
        notifier,
        Box::new(transport),
        AccessList::default(),
        events,
        options,
    )
    .unwrap()
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

fn snapshot(client: &Client) -> Snapshot {
    snapshot_of(&client.call(&user(), envelope("snap", Request::GetSnapshot), LONG))
}

fn tunnel(client: &Client, name: &str) -> wgal_protocol::TunnelStatus {
    snapshot(client)
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == id(name))
        .cloned()
        .unwrap()
}

struct Rig {
    lab: Lab,
    clock: Arc<FakeClock>,
    store: MemoryStore,
    wd: Wd,
}

fn rig(tunnels: &[&str]) -> Rig {
    let lab = Lab::new(tunnels);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, tunnels);
    Rig {
        lab,
        clock,
        store,
        wd,
    }
}

#[test]
fn requests_are_answered_while_a_probe_runs() {
    let Rig { lab, clock, wd, .. } = rig(&["alpha"]);
    let gate = lab.gate("alpha", Gated::Probe);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    clock.advance(Duration::from_secs(15)).unwrap();
    assert!(gate.wait_entered(LONG), "a round reached the probe");

    let started = Instant::now();
    let answer = client.call(&user(), envelope("s1", Request::GetSnapshot), LONG);
    assert_eq!(answer.request_id.as_ref().unwrap().as_str(), "s1");
    assert!(error_code(&answer).is_none(), "{answer:?}");
    // Probes and polls do not lock the tunnel.
    let probe = client.call(
        &user(),
        envelope(
            "p1",
            Request::ProbeNow {
                tunnel_id: id("alpha"),
            },
        ),
        LONG,
    );
    assert!(accepted(&probe), "{probe:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(gate.entered(), 1, "still the same probe");

    gate.open();
    assert!(eventually(|| tunnel(&client, "alpha").light == Light::Green));
    assert!(running.stop(LONG));
}

#[test]
fn a_request_for_a_tunnel_waits_for_its_stop_and_is_answered_after() {
    let Rig { lab, wd, .. } = rig(&["alpha", "beta"]);
    let gate = lab.gate("alpha", Gated::Stop);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    let disconnect = client.call(
        &user(),
        envelope(
            "d1",
            Request::Disconnect {
                tunnel_id: id("alpha"),
            },
        ),
        LONG,
    );
    assert!(accepted(&disconnect));
    assert!(gate.wait_entered(LONG), "the stop reached the platform");

    let waiting = client.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let answer = waiting.call(
            &user(),
            envelope(
                "c1",
                Request::Connect {
                    tunnel_id: id("alpha"),
                },
            ),
            LONG,
        );
        let _ = tx.send(answer);
    });
    // The host and the other tunnel are answered meanwhile.
    assert!(
        error_code(&client.call(&user(), envelope("s1", Request::GetSnapshot), LONG)).is_none()
    );
    let other = client.call(
        &user(),
        envelope(
            "b1",
            Request::SetAutoRecovery {
                tunnel_id: id("beta"),
                enabled: false,
            },
        ),
        LONG,
    );
    assert!(accepted(&other));
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "the connect waits for the stop"
    );
    assert_eq!(running.stats().held, 1);

    gate.open();
    let answer = rx.recv_timeout(LONG).unwrap();
    assert_eq!(answer.request_id.as_ref().unwrap().as_str(), "c1");
    assert!(accepted(&answer), "{answer:?}");
    // Stopped, then started again for the connect.
    assert!(eventually(|| {
        let t = tunnel(&client, "alpha");
        t.intent.desired == DesiredState::Connected && t.light == Light::Green
    }));
    assert_eq!(lab.stops("alpha"), 1);
    assert_eq!(lab.starts("alpha"), 1);
    assert!(running.stop(LONG));
}

#[test]
fn an_expired_deadline_is_a_timeout_and_the_request_still_applies() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let gate = lab.gate("alpha", Gated::Stop);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    let disconnect = Request::Disconnect {
        tunnel_id: id("alpha"),
    };
    assert!(accepted(&client.call(
        &user(),
        envelope("d1", disconnect),
        LONG
    )));
    assert!(gate.wait_entered(LONG));

    let started = Instant::now();
    let connect = Request::Connect {
        tunnel_id: id("alpha"),
    };
    let answer = client.call(&user(), envelope("c1", connect), Duration::from_millis(100));
    assert_eq!(
        refusal_of(&answer),
        Some((ErrorCode::Timeout, Some(ErrorReason::NoAnswerInTime)))
    );
    assert_eq!(answer.request_id.as_ref().unwrap().as_str(), "c1");
    assert!(started.elapsed() < Duration::from_secs(2));

    gate.open();
    assert!(eventually(|| {
        tunnel(&client, "alpha").intent.desired == DesiredState::Connected
    }));
    assert!(running.stop(LONG));
}

#[test]
fn stop_gives_up_on_a_stop_in_flight_at_its_deadline() {
    let Rig { lab, store, wd, .. } = rig(&["alpha"]);
    let dir = TempDir::new("stop-late");
    let gate = lab.gate("alpha", Gated::Stop);
    let events = EventLog::open(dir.path()).unwrap();
    let (client, running) = start(
        wd,
        quiet_notifier(),
        Script::default(),
        Some(events),
        &fast(),
    );
    let disconnect = Request::Disconnect {
        tunnel_id: id("alpha"),
    };
    assert!(accepted(&client.call(
        &user(),
        envelope("d1", disconnect),
        LONG
    )));
    assert!(gate.wait_entered(LONG));

    let started = Instant::now();
    assert!(!running.stop(Duration::from_millis(200)), "not in time");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(150) && took < Duration::from_secs(3),
        "{took:?}"
    );
    // New calls are refused.
    let refused = client.call(&user(), envelope("s1", Request::GetSnapshot), LONG);
    assert_eq!(
        refusal_of(&refused),
        Some((ErrorCode::Busy, Some(ErrorReason::ShuttingDown)))
    );
    // The disconnect is stored as unfinished: a restart stops the tunnel.
    let saved = store.saved().unwrap();
    assert_eq!(saved.tunnels[0].intent.desired, DesiredState::Disconnected);
    assert_eq!(saved.tunnels[0].resume, Some(ResumeOp::Disconnect));
    let lines = log_lines(dir.path());
    let stopped = diags_named(&lines, "stopped");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["diag"]["in_time"], false);
    assert_eq!(lines.last().unwrap()["event"]["kind"], "watchdog_stopped");
    // The stuck call ends on its own once the platform answers.
    gate.open();
    assert!(eventually(
        || lab.fake().os_state(&binding("alpha")) == OsState::Disconnected
    ));
}

#[test]
fn stop_waits_for_a_stop_in_flight_that_finishes_in_time() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let dir = TempDir::new("stop-in-time");
    let gate = lab.gate("alpha", Gated::Stop);
    let events = EventLog::open(dir.path()).unwrap();
    let (client, running) = start(
        wd,
        quiet_notifier(),
        Script::default(),
        Some(events),
        &fast(),
    );
    let disconnect = Request::Disconnect {
        tunnel_id: id("alpha"),
    };
    assert!(accepted(&client.call(
        &user(),
        envelope("d1", disconnect),
        LONG
    )));
    assert!(gate.wait_entered(LONG));
    let opener = Arc::clone(&gate);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        opener.open();
    });
    assert!(running.stop(LONG), "the stop came back in time");
    assert_eq!(lab.stops("alpha"), 1);
    let lines = log_lines(dir.path());
    assert_eq!(lines.last().unwrap()["event"]["kind"], "watchdog_stopped");
    let stopped = diags_named(&lines, "stopped");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["diag"]["in_time"], true);
}

#[test]
fn the_notification_runs_on_its_own_thread_and_its_result_is_applied_once() {
    let Rig {
        lab,
        clock: _,
        mut wd,
        ..
    } = rig(&["alpha"]);
    manage(&mut wd, "alpha");
    let (notifier, _, _) = notifier(&mut wd);
    lab.kill_for_good("alpha");
    for _ in 0..600 {
        tick_for(&mut wd, 1);
        if !wd.notifications_due().is_empty() {
            break;
        }
    }
    assert!(!wd.notifications_due().is_empty());
    let transport = Script::default();
    let hold = transport.hold();
    let (client, running) = start(wd, notifier, transport.clone(), None, &fast());
    assert!(hold.wait_entered(LONG), "the alert is being sent");

    // HTTP blocks its own thread only.
    let started = Instant::now();
    let incident = tunnel(&client, "alpha").incident.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(incident.notification, NotificationState::Reserved);

    hold.open();
    assert!(eventually(|| {
        tunnel(&client, "alpha").incident.unwrap().notification == NotificationState::Sent
    }));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(transport.sends(), 1);
    assert!(running.stop(LONG));
    assert_eq!(transport.sends(), 1);
}

#[test]
fn an_idle_loop_does_not_spin() {
    let Rig { wd, .. } = rig(&["alpha"]);
    let (client, running) = start(
        wd,
        quiet_notifier(),
        Script::default(),
        None,
        &Options::default(),
    );
    thread::sleep(Duration::from_millis(500));
    let stats = running.stats();
    // The clock does not move: one or two wake-ups at most, never a loop.
    assert!(stats.turns <= 3, "{stats:?}");
    assert!(stats.plan_passes <= 3, "{stats:?}");
    // A request still wakes it at once.
    let started = Instant::now();
    assert!(
        error_code(&client.call(&user(), envelope("s1", Request::GetSnapshot), LONG)).is_none()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let started = Instant::now();
    assert!(running.stop(LONG));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_wake_or_network_change_probes_again_at_once() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    assert!(eventually(
        || running.stats().in_flight == 0 && running.stats().turns > 1
    ));
    let probes = lab.probes("alpha");
    client.on_wake();
    assert!(eventually(|| lab.probes("alpha") > probes));
    let probes = lab.probes("alpha");
    client.on_network_change();
    assert!(eventually(|| lab.probes("alpha") > probes));
    assert!(running.stop(LONG));
}

#[test]
fn an_adapter_that_panics_does_not_stall_the_loop() {
    let Rig { lab, clock, wd, .. } = rig(&["alpha"]);
    lab.panic_on_probes("alpha", true);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    clock.advance(Duration::from_secs(15)).unwrap();
    assert!(eventually(|| tunnel(&client, "alpha").light == Light::Red));
    lab.panic_on_probes("alpha", false);
    clock.advance(Duration::from_secs(15)).unwrap();
    assert!(eventually(|| tunnel(&client, "alpha").light == Light::Green));
    assert_eq!(running.stats().in_flight, 0);
    assert!(running.stop(LONG));
}

#[test]
fn too_many_waiting_requests_are_refused() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let gate = lab.gate("alpha", Gated::Stop);
    let options = Options {
        max_calls: 2,
        ..fast()
    };
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &options);
    let disconnect = Request::Disconnect {
        tunnel_id: id("alpha"),
    };
    assert!(accepted(&client.call(
        &user(),
        envelope("d1", disconnect),
        LONG
    )));
    assert!(gate.wait_entered(LONG));
    let (tx, rx) = mpsc::channel();
    for n in 0..2 {
        let waiting = client.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let connect = Request::Connect {
                tunnel_id: id("alpha"),
            };
            let _ = tx.send(waiting.call(&user(), envelope(&format!("c{n}"), connect), LONG));
        });
    }
    assert!(eventually(|| running.stats().held == 2));
    let refused = client.call(&user(), envelope("s1", Request::GetSnapshot), LONG);
    assert_eq!(
        refusal_of(&refused),
        Some((ErrorCode::Busy, Some(ErrorReason::TooManyWaiting)))
    );
    gate.open();
    for _ in 0..2 {
        assert!(accepted(&rx.recv_timeout(LONG).unwrap()));
    }
    assert!(
        error_code(&client.call(&user(), envelope("s2", Request::GetSnapshot), LONG)).is_none()
    );
    assert!(running.stop(LONG));
}

#[test]
fn after_stop_every_call_is_busy() {
    let Rig { wd, .. } = rig(&["alpha"]);
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    assert!(running.stop(LONG));
    let answer = client.call(&user(), envelope("s1", Request::GetSnapshot), LONG);
    assert_eq!(
        refusal_of(&answer),
        Some((ErrorCode::Busy, Some(ErrorReason::ShuttingDown)))
    );
    assert_eq!(answer.request_id.unwrap().as_str(), "s1");
    client.on_wake();
}

#[test]
fn strangers_are_refused_without_the_loop() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let gate = lab.gate("alpha", Gated::Stop);
    let options = Options {
        max_calls: 1,
        ..fast()
    };
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &options);
    let disconnect = Request::Disconnect {
        tunnel_id: id("alpha"),
    };
    assert!(accepted(&client.call(
        &user(),
        envelope("d1", disconnect),
        LONG
    )));
    assert!(gate.wait_entered(LONG));
    // A request for the stopping tunnel fills the only place.
    let waiting = client.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let connect = Request::Connect {
            tunnel_id: id("alpha"),
        };
        let _ = tx.send(waiting.call(&user(), envelope("c1", connect), LONG));
    });
    assert!(eventually(|| running.stats().held == 1));
    // Unauthorized, not busy: they never took a place.
    for request in every_request("alpha") {
        let answer = client.call(&stranger(true), envelope("x1", request), LONG);
        assert_eq!(
            refusal_of(&answer),
            Some((ErrorCode::Unauthorized, Some(ErrorReason::CallerNotAllowed)))
        );
    }
    assert_eq!(running.stats().held, 1);
    gate.open();
    assert!(accepted(&rx.recv_timeout(LONG).unwrap()));
    assert!(running.stop(LONG));
}

/// Moves the fake clock one second every `every` while it lives, so the
/// owner thread's one-second polls come due during a real-time stop.
struct Ticker {
    running: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Ticker {
    fn start(clock: &Arc<FakeClock>, every: Duration) -> Self {
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let clock = Arc::clone(clock);
        let thread = thread::spawn(move || {
            while flag.load(std::sync::atomic::Ordering::Relaxed) {
                clock.advance(Duration::from_secs(1)).unwrap();
                thread::sleep(every);
            }
        });
        Self {
            running,
            thread: Some(thread),
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A managed tunnel whose recovery stop is held at the gate, so the test
/// decides when the stop is accepted; the stop then leaves it transitioning
/// for `reads` state reads.
fn recovery_stop_at_the_gate(reads: u32) -> (Lab, Arc<FakeClock>, MemoryStore, Arc<Gate>, Running) {
    let Rig {
        lab,
        clock,
        store,
        mut wd,
    } = rig(&["alpha"]);
    manage(&mut wd, "alpha");
    lab.slow_stop("alpha", reads);
    let gate = lab.gate("alpha", Gated::Stop);
    let (_client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    lab.kill_until_restart("alpha");
    assert!(eventually(|| {
        clock.advance(Duration::from_secs(1)).unwrap();
        gate.entered() > 0
    }));
    (lab, clock, store, gate, running)
}

/// Review of wave two, P1-1, through the threads: `Running::stop` while a
/// recovery's stop is still pending polls it to the end and sends the start
/// before it reports success.
#[test]
fn stop_waits_for_a_pending_recovery_stop_and_sends_its_start() {
    let (lab, clock, _store, gate, running) = recovery_stop_at_the_gate(2);
    // The stop goes on 5 s later on the loop's own (fake) clock, well within
    // the core's 30 s control phase. Waiting real time instead made the test
    // depend on how long the ticker's 5 ms sleeps really take: on macOS more
    // than 30 fake seconds passed in 200 ms, the core gave the stop up and
    // sent no start (first native macOS run, 2026-11-13).
    let opener = Arc::clone(&gate);
    let watch = Arc::clone(&clock);
    let opened_at = watch.wall_unix_ms() + 5_000;
    thread::spawn(move || {
        while watch.wall_unix_ms() < opened_at {
            thread::sleep(Duration::from_millis(1));
        }
        opener.open();
    });
    let ticker = Ticker::start(&clock, Duration::from_millis(5));
    let in_time = running.stop(LONG);
    drop(ticker);
    assert_eq!(
        lab.starts("alpha"),
        1,
        "WGAL left the tunnel it stopped down"
    );
    assert!(in_time, "the start was sent in time");
    assert_eq!(lab.fake().os_state(&binding("alpha")), OsState::Connected);
}

/// P1-1: a stop that is still pending, or that the core gives up on, by the
/// deadline makes `stop` report `false`; nothing is started.
#[test]
fn stop_reports_a_stop_that_never_settles() {
    let (lab, clock, store, gate, running) = recovery_stop_at_the_gate(1_000_000);
    gate.open();
    let ticker = Ticker::start(&clock, Duration::from_millis(2));
    let started = Instant::now();
    let in_time = running.stop(Duration::from_millis(500));
    drop(ticker);
    assert!(!in_time);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(lab.starts("alpha"), 0);
    // What the next start of the service needs is stored: the unfinished
    // round, or, once the core gave the stop up, the round's retry.
    let saved = store.saved().unwrap();
    let rec = &saved.tunnels[0];
    assert!(
        rec.resume == Some(ResumeOp::Recovery)
            || rec
                .incident
                .as_ref()
                .is_some_and(|i| i.next_retry_unix_ms.is_some()),
        "{rec:?}"
    );
}

/// Review of wave two (Windows), P2: an owner thread stuck in a call made on
/// it (here a discovery that never returns) does not keep `stop` from
/// returning by its deadline; it reports `false`.
#[test]
fn stop_returns_by_its_deadline_when_the_owner_thread_hangs() {
    let Rig { lab, wd, .. } = rig(&["alpha"]);
    let gate = lab.gate_discovery();
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    let caller = client.clone();
    thread::spawn(move || {
        let _ = caller.call(&admin(), envelope("d1", Request::DiscoverTunnels), LONG);
    });
    assert!(gate.wait_entered(LONG), "the owner thread is in discovery");
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        let _ = tx.send(running.stop(Duration::from_millis(200)));
    });
    let returned = rx.recv_timeout(Duration::from_secs(5));
    let took = started.elapsed();
    gate.open();
    assert_eq!(returned, Ok(false), "stop did not return by its deadline");
    let limit =
        Duration::from_millis(200) + wgal_runtime::driver::FINISH_GRACE + Duration::from_secs(1);
    assert!(took < limit, "{took:?}");
}

/// Review of wave two, N-P3-4, through the threads: after `begin_stop` a
/// fault confirmed later starts no recovery, while requests are answered
/// and monitoring goes on; `stop` then finishes in time.
#[test]
fn begin_stop_holds_new_automatic_actions_while_requests_are_answered() {
    let Rig {
        lab, clock, mut wd, ..
    } = rig(&["alpha"]);
    manage(&mut wd, "alpha");
    let (client, running) = start(wd, quiet_notifier(), Script::default(), None, &fast());
    running.begin_stop();
    running.begin_stop();
    lab.kill_for_good("alpha");
    let confirmed = eventually(|| {
        clock.advance(Duration::from_secs(5)).unwrap();
        thread::sleep(Duration::from_millis(10));
        tunnel(&client, "alpha").light_reason == LightReason::FaultConfirmed
    });
    assert!(confirmed, "the fault was never confirmed");
    for _ in 0..20 {
        clock.advance(Duration::from_secs(5)).unwrap();
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(lab.stops("alpha"), 0, "a recovery was started");
    let answer = client.call(&user(), envelope("s1", Request::GetSnapshot), LONG);
    assert!(error_code(&answer).is_none());
    assert!(running.stop(LONG));
}
