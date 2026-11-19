//! Per-tunnel probe timing and the owner's confirmation rule of 2026-11-03
//! (which replaces plan §6.2's fixed 3 rounds and 60 s): interval, timeout,
//! `retries + 1` failed rounds and `min_fault_secs`, and a freshness of
//! interval + timeout + 18 s. Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Lab, Rig, T0, id, timing};
use wgal_core::{MemoryStore, Startup, Timing, Watchdog};
use wgal_protocol::{DataHealth, Light, LightReason, LogEvent, ProbeTiming, Request, Takeover};
use wgal_testkit::{Call, FakeClock};

/// With no retries and no minimum fault time, the first failed round
/// confirms the fault and recovery starts at once.
#[test]
fn retries_0_and_min_0_confirm_on_the_first_failed_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", timing(10, 1, 0, 0));
    rig.run(1); // the round at 36 s succeeds; the next is at 46 s
    rig.lab().break_until_restart("a");
    rig.events();
    rig.run_until(45);
    assert_eq!(rig.stops("a"), 0);
    assert_eq!(rig.status("a").light, Light::Green);
    rig.run_until(46);
    assert_eq!(rig.stops("a"), 1, "the first failed round confirms");
    let events = rig.events_of("a");
    assert_eq!(
        events[0],
        LogEvent::ProbeFailed {
            streak: 1,
            needed: 1
        }
    );
    assert!(matches!(events[1], LogEvent::FaultConfirmed { .. }));
    assert_eq!(events[2], LogEvent::RecoveryStarted { attempt: 1 });
}

/// Ten retries need eleven failed rounds in a row, however short the
/// minimum fault time.
#[test]
fn ten_retries_need_eleven_failed_rounds() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", timing(10, 1, 10, 0));
    rig.run(1); // success at 36 s
    rig.lab().break_until_restart("a");
    rig.run_until(145); // failed rounds at 46, 56, ..., 136 s
    let status = rig.status("a");
    assert_eq!(rig.stops("a"), 0, "ten failed rounds are not enough");
    assert_eq!(status.consecutive_failures, 10);
    assert_eq!(status.light, Light::Yellow);
    assert_eq!(status.light_reason, LightReason::Confirming);
    assert_eq!(
        status.deadline_unix_ms,
        Some(T0 + 146_000),
        "the first failure (46 s) plus ten rounds of 10 s"
    );
    rig.run_until(146);
    assert_eq!(rig.stops("a"), 1, "the eleventh failed round confirms");
}

/// A successful round ends the streak; the failures before it never count
/// towards the next one.
#[test]
fn a_success_in_the_middle_starts_the_streak_again() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", timing(10, 1, 2, 0));
    rig.run(1); // success at 36 s
    rig.lab().break_for_good("a");
    rig.events();
    rig.run_until(60); // failed rounds at 46 and 56 s
    assert_eq!(rig.status("a").consecutive_failures, 2);
    rig.lab().heal("a");
    rig.run_until(66); // success
    assert_eq!(rig.status("a").consecutive_failures, 0);
    rig.lab().break_until_restart("a");
    rig.run_until(95); // failed rounds at 76 and 86 s
    assert_eq!(rig.status("a").consecutive_failures, 2);
    assert_eq!(rig.stops("a"), 0, "2 + 2 failures are not 3 in a row");
    rig.run_until(96);
    assert_eq!(rig.stops("a"), 1);
    let failed = |streak| LogEvent::ProbeFailed { streak, needed: 3 };
    let events = rig.events_of("a");
    assert_eq!(
        events[..7],
        [
            failed(1),
            failed(2),
            LogEvent::ProbeRecovered { after: 2 },
            // Yellow (confirming) back to green.
            LogEvent::TrafficVerified,
            failed(1),
            failed(2),
            failed(3),
        ]
    );
    assert!(matches!(events[7], LogEvent::FaultConfirmed { .. }));
}

/// An hour between rounds: the last success stays fresh for interval +
/// timeout + 18 s, so the light stays green between rounds and nothing is
/// taken for a stall; the fault is confirmed by rounds an hour apart, and
/// after a recovery the next round, an hour later, closes the incident.
#[test]
fn an_hour_between_rounds_keeps_results_fresh_and_closes_incidents() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", timing(3600, 2, 1, 0));
    rig.run(1); // success at 36 s; the next round is at 3636 s
    let probes = rig.probes("a");
    rig.run_until(3635);
    let status = rig.status("a");
    assert_eq!(rig.probes("a"), probes, "no round in between");
    assert_eq!(status.freshness_secs, 3620);
    assert_eq!(status.light, Light::Green);
    assert_eq!(status.data_health, DataHealth::Healthy);

    rig.lab().break_until_restart("a");
    rig.run_until(3636);
    let status = rig.status("a");
    assert_eq!(status.consecutive_failures, 1);
    assert_eq!(status.light_reason, LightReason::Confirming);
    assert_eq!(status.deadline_unix_ms, Some(T0 + 7_236_000));
    rig.run_until(7235);
    assert_eq!(rig.stops("a"), 0);
    assert_eq!(rig.status("a").light_reason, LightReason::Confirming);

    rig.run_until(7236); // second failed round: confirmed, restarted, verified
    assert_eq!(rig.stops("a"), 1);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green);
    assert!(status.incident.is_some(), "open until stable long enough");
    rig.run_until(10_835);
    assert!(rig.status("a").incident.is_some());
    assert_eq!(rig.status("a").light, Light::Green);
    rig.run_until(10_836); // the next round, an hour after the verification
    assert_eq!(rig.status("a").incident, None, "closed");
    assert_eq!(rig.stops("a"), 1);
}

/// The platform gets the tunnel's own probe timeout.
#[test]
fn the_probe_timeout_is_the_tunnels_own() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.set_probe("a", timing(30, 7, 2, 60));
    rig.run(1);
    let timeouts = |rig: &Rig, name: &str| -> Vec<Duration> {
        rig.calls(name)
            .iter()
            .filter_map(|c| match c {
                Call::Probe(_, request) => Some(request.timeout),
                _ => None,
            })
            .collect()
    };
    assert_eq!(timeouts(&rig, "a").last(), Some(&Duration::from_secs(7)));
    rig.run(15);
    assert_eq!(timeouts(&rig, "b").last(), Some(&Duration::from_secs(2)));
}

/// A new timing takes effect at the next round, which is due at once, and
/// starts a new streak; it keeps the baseline and auto recovery.
#[test]
fn changing_the_timing_mid_streak_starts_a_new_streak_and_keeps_auto_recovery() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(61); // failed rounds at 45 and 60 s
    assert_eq!(rig.status("a").consecutive_failures, 2);
    rig.events();

    rig.set_probe("a", timing(10, 1, 2, 0));
    let status = rig.status("a");
    assert_eq!(status.consecutive_failures, 0, "a new streak");
    assert_eq!(status.probe, timing(10, 1, 2, 0));
    assert_eq!(status.freshness_secs, 29);
    assert!(status.intent.auto_recovery_enabled);
    assert!(status.auto_recovery_allowed, "the baseline is kept");
    assert_eq!(status.takeover, Takeover::Managed);

    rig.run_until(81); // failed rounds at 62 and 72 s
    assert_eq!(rig.stops("a"), 0);
    rig.run_until(82); // the third in a row
    assert_eq!(rig.stops("a"), 1);
    assert_eq!(
        rig.events_of("a")[0],
        LogEvent::ProbeFailed {
            streak: 1,
            needed: 3
        }
    );
}

/// Plan §6.2's example under the defaults, from the timing itself: three
/// failed rounds by 30 s are not a fault; the round at 60 s is the first to
/// meet both conditions.
#[test]
fn the_default_timing_keeps_the_plans_example() {
    let probe = ProbeTiming::DEFAULT;
    assert_eq!(probe, timing(15, 2, 2, 60));
    let mut rig = Rig::new(&["a"]);
    assert_eq!(rig.status("a").probe, probe);
    assert_eq!(rig.status("a").freshness_secs, 35);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(75); // 45, 60 and 75 s failed
    assert_eq!(rig.status("a").consecutive_failures, 3);
    assert_eq!(rig.stops("a"), 0);
    rig.run_until(105);
    assert_eq!(rig.stops("a"), 1);
}

/// Tunnels restored with different intervals start their first rounds
/// `stagger` apart, each wrapped within its own interval, and then keep
/// their own pace.
#[test]
fn first_rounds_are_staggered_whatever_the_intervals() {
    let rig = Rig::new(&["a", "b", "c"]);
    let mut saved = rig.store.saved().unwrap();
    for (rec, interval) in saved.tunnels.iter_mut().zip([10, 15, 3600]) {
        rec.probe = timing(interval, 2, 2, 60);
    }
    let mut wd = Watchdog::new(
        Lab::new(&["a", "b", "c"]),
        FakeClock::new(T0 + 600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    let mut rounds: Vec<(&str, u64)> = Vec::new();
    for second in 0..=20 {
        if second > 0 {
            wd.clock().advance(Duration::from_secs(1)).unwrap();
        }
        let before: Vec<usize> = ["a", "b", "c"].iter().map(|n| probes_of(&wd, n)).collect();
        wd.tick();
        for (name, count) in ["a", "b", "c"].iter().zip(before) {
            if probes_of(&wd, name) > count {
                rounds.push((name, second));
            }
        }
    }
    assert_eq!(
        rounds,
        [
            ("a", 0),
            ("b", 2),
            ("c", 4),
            ("a", 10),
            ("b", 17),
            ("a", 20)
        ]
    );
}

fn probes_of(wd: &support::Wd, name: &str) -> usize {
    wd.adapter()
        .fake
        .calls_for(&support::binding(name))
        .iter()
        .filter(|c| matches!(c, Call::Probe(..)))
        .count()
}

/// The request carries the whole timing, checked on decode; an unknown
/// tunnel is refused as before.
#[test]
fn timing_for_an_unknown_tunnel_is_refused() {
    let mut rig = Rig::new(&["a"]);
    let outcome = rig.request(Request::UpdateMonitorSettings {
        tunnel_id: id("zz"),
        settings: wgal_protocol::MonitorSettings {
            recovery_target: None,
            observe_targets: wgal_protocol::BoundedVec::new(),
            probe: timing(10, 1, 0, 0),
        },
    });
    assert!(
        matches!(&outcome, wgal_protocol::Outcome::Err(e) if e.code == wgal_protocol::ErrorCode::UnknownTunnel),
        "{outcome:?}"
    );
}
