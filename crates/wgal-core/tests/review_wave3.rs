//! Regression tests for the Codex review of wave three (`ba855aa..5ff68a8`,
//! 2026-11-03), core part: a pause cancels every automatic operation that
//! has sent nothing yet (P1-1), and a wake or network change voids the
//! evidence from before it, results still out included (P1-3), and a round
//! that takes long with legal timing does not make its own recovery-target
//! result stale (P2-5). Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{Rig, binding, icmp, id, timing};
use wgal_core::work::WorkOutcome;
use wgal_core::{AdapterError, Clock, ProbeOutcome, WorkItem, WorkKind, WorkResult, execute};
use wgal_protocol::{
    AdapterErrorKind, DataHealth, Light, LightReason, OsState, RecoveryPhase, Request,
};
use wgal_testkit::Operation;

/// Runs whatever is due at this instant, as `tick` does, but stops at the
/// first planned item `pick` matches and returns it without running it (it
/// has not passed `still_due`). Advances one second whenever nothing is due.
fn run_until_planned(
    rig: &mut Rig,
    limit_secs: u64,
    mut pick: impl FnMut(&WorkItem) -> bool,
) -> WorkItem {
    let end = rig.now_secs() + limit_secs;
    loop {
        for _ in 0..64 {
            let items = rig.wd.plan();
            if items.is_empty() {
                break;
            }
            let mut found = None;
            for item in items {
                if found.is_none() && pick(&item) {
                    found = Some(item);
                } else if rig.wd.still_due(&item) {
                    let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
                    rig.wd.apply(result);
                }
            }
            if let Some(item) = found {
                return item;
            }
        }
        assert!(rig.now_secs() < end, "no such item was planned");
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    }
}

fn is_start_of(name: &str) -> impl FnMut(&WorkItem) -> bool + '_ {
    move |item| item.tunnel_id == id(name) && item.kind == WorkKind::Start
}

fn is_round_of(name: &str) -> impl FnMut(&WorkItem) -> bool + '_ {
    move |item| item.tunnel_id == id(name) && matches!(item.kind, WorkKind::Round { .. })
}

fn light(rig: &mut Rig, name: &str) -> (Light, LightReason) {
    let status = rig.status(name);
    (status.light, status.light_reason)
}

/// A monitoring round of `name` planned now and passed by `still_due`, not
/// run yet.
fn round_planned_now(rig: &mut Rig, name: &str) -> WorkItem {
    rig.ok(Request::ProbeNow {
        tunnel_id: id(name),
    });
    let mut items = rig.wd.plan();
    assert_eq!(items.len(), 1, "{items:?}");
    let round = items.remove(0);
    assert!(is_round_of(name)(&round), "{round:?}");
    assert!(rig.wd.still_due(&round));
    round
}

/// A's reconnect holds the host's control slot: its stop is handed out and
/// its result is not back. Meanwhile B, managed with "start when stopped
/// externally" on, is stopped outside WGAL; its round notices that and
/// queues the restart behind A. Returns A's stop result, still to be
/// applied.
fn restart_queued_behind_a(rig: &mut Rig) -> WorkResult {
    rig.manage("b");
    rig.ok(Request::SetStartWhenStoppedExternally {
        tunnel_id: id("b"),
        enabled: true,
    });
    rig.run_until(44);
    rig.lab()
        .fake
        .set_os_state(&binding("b"), OsState::Disconnected);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.ok(Request::ProbeNow { tunnel_id: id("b") });
    let wd = &mut rig.wd;
    let mut stop = None;
    for item in wd.plan() {
        assert!(wd.still_due(&item), "{item:?}");
        let result = execute(wd.adapter(), wd.clock(), &item);
        if item.tunnel_id == id("a") && item.kind == WorkKind::Stop {
            stop = Some(result);
        } else {
            wd.apply(result);
        }
    }
    assert_eq!(rig.status("b").recovery, RecoveryPhase::Queued);
    stop.expect("A's stop was handed out")
}

/// P1-1, the reviewer's case with one tunnel paused: B's restart after an
/// external stop waits behind A's control work, then B's monitoring is
/// paused. When A's stop and start are done and the slot is free, B is not
/// started. A's own operation finishes.
#[test]
fn p1_1_pausing_a_tunnel_cancels_its_queued_restart() {
    let mut rig = Rig::new(&["a", "b"]);
    let stop = restart_queued_behind_a(&mut rig);
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("b"),
        enabled: false,
    });
    rig.wd.apply(stop);
    rig.run(120);
    assert_eq!(rig.starts("b"), 0, "the paused tunnel was started");
    assert_eq!(
        rig.lab().fake.os_state(&binding("b")),
        OsState::Disconnected
    );
    let status = rig.status("b");
    assert_eq!(
        (status.light, status.light_reason),
        (Light::Gray, LightReason::MonitoringOff)
    );
    assert_eq!(status.recovery, RecoveryPhase::Idle);
    // A's reconnect went on regardless.
    assert_eq!((rig.stops("a"), rig.starts("a")), (1, 1));
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);

    // With monitoring on again, the external stop is judged like one made
    // while monitoring was paused: with the option on, B is started.
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("b"),
        enabled: true,
    });
    rig.run(60);
    assert_eq!(rig.starts("b"), 1);
    assert_eq!(rig.lab().fake.os_state(&binding("b")), OsState::Connected);
}

/// P1-1 with the host-wide pause: the same queued restart is cancelled and
/// B stays down; A's reconnect, a user's operation, still finishes.
#[test]
fn p1_1_the_host_wide_pause_cancels_a_queued_restart() {
    let mut rig = Rig::new(&["a", "b"]);
    let stop = restart_queued_behind_a(&mut rig);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    rig.wd.apply(stop);
    rig.run(120);
    assert_eq!(rig.starts("b"), 0, "a paused tunnel was started");
    assert_eq!(
        rig.lab().fake.os_state(&binding("b")),
        OsState::Disconnected
    );
    assert_eq!((rig.stops("a"), rig.starts("a")), (1, 1));
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);

    rig.ok(Request::SetMonitoringPaused { paused: false });
    rig.run(60);
    assert_eq!(rig.starts("b"), 1);
}

/// P1-1, checked again when the item is handed out: B's restart got the
/// slot and its start was planned, but the host-wide pause came before the
/// start passed `still_due` (a runtime answers that request while the item
/// waits for a worker). The start is withdrawn and never sent.
#[test]
fn p1_1_a_restart_planned_before_the_pause_is_withdrawn_when_handed_out() {
    let mut rig = Rig::new(&["a", "b"]);
    let stop = restart_queued_behind_a(&mut rig);
    rig.wd.apply(stop);
    let start = run_until_planned(&mut rig, 60, is_start_of("b"));
    assert_eq!(rig.status("b").recovery, RecoveryPhase::Starting);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    assert!(!rig.wd.still_due(&start), "the start still went out");
    rig.run(120);
    assert_eq!(rig.starts("b"), 0);
    assert_eq!(
        rig.lab().fake.os_state(&binding("b")),
        OsState::Disconnected
    );
}

/// P1-1, an automatic recovery that has sent nothing although it holds the
/// slot: WGAL's own start failed and left A down, so the retry goes straight
/// to a start. The pause comes after that start is planned and before it is
/// handed out: it is withdrawn, the retry stays due, and it starts A once
/// monitoring is on again.
#[test]
fn p1_1_a_retry_that_only_starts_is_withdrawn_by_a_pause_and_kept() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Start,
        AdapterError::new(AdapterErrorKind::ManagerConflict, "injected start failure"),
    );
    // The first round: the stop goes out, the start fails, A is left down.
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 400, "the first round never ended");
    }
    assert_eq!((rig.stops("a"), rig.starts("a")), (1, 1));
    assert_eq!(
        rig.lab().fake.os_state(&binding("a")),
        OsState::Disconnected
    );
    rig.lab().heal("a");
    let start = run_until_planned(&mut rig, 400, is_start_of("a"));
    assert_eq!(
        rig.stops("a"),
        1,
        "the retry of a tunnel left down stops nothing"
    );
    rig.ok(Request::SetMonitoringPaused { paused: true });
    assert!(!rig.wd.still_due(&start), "the start still went out");
    rig.run(300);
    assert_eq!(rig.starts("a"), 1, "started while paused");

    rig.ok(Request::SetMonitoringPaused { paused: false });
    rig.run(60);
    assert_eq!(rig.starts("a"), 2, "the retry was dropped");
    assert_eq!(rig.lab().fake.os_state(&binding("a")), OsState::Connected);
}

/// P1-3, the reviewer's case: a tunnel probed once an hour has a fresh
/// success (fresh for 3620 s). The host moves to another network, where the
/// data path is dead. Nothing proven on the old network keeps the light
/// green: it shows the grace period until a round answers, and the round
/// right after the change, which gets no reply and is not counted in the
/// grace period, does not make it green either. The same after a wake.
#[test]
fn p1_3_a_success_from_before_a_network_change_or_a_wake_is_not_green_after_it() {
    for wake in [false, true] {
        let mut rig = Rig::new(&["a"]);
        rig.set_probe("a", timing(3600, 2, 2, 60));
        rig.run(1);
        assert_eq!(light(&mut rig, "a"), (Light::Green, LightReason::Verified));
        rig.lab().break_for_good("a");
        if wake {
            rig.wake();
        } else {
            rig.network_change();
        }
        let status = rig.status("a");
        assert_eq!(
            (status.light, status.light_reason),
            (Light::Yellow, LightReason::GracePeriod),
            "wake: {wake}"
        );
        assert_eq!(status.data_health, DataHealth::Unknown);
        // The time of the last answer is still reported as such.
        assert!(status.last_verified_unix_ms.is_some());
        for _ in 0..600 {
            rig.run(1);
            assert_ne!(rig.status("a").light, Light::Green, "wake: {wake}");
        }
        rig.lab().heal("a");
        rig.ok(Request::ProbeNow { tunnel_id: id("a") });
        rig.run(1);
        assert_eq!(light(&mut rig, "a"), (Light::Green, LightReason::Verified));
    }
}

/// P1-3: a round planned before the network changed comes back after it.
/// Its answer is no evidence on the new network, whether the probe finished
/// before the change or after it: the light stays in the grace period, and
/// a round that counts is due at once.
#[test]
fn p1_3_a_round_planned_before_the_change_is_ignored_when_applied_after_it() {
    for finished_before in [true, false] {
        let mut rig = Rig::new(&["a"]);
        let round = round_planned_now(&mut rig, "a");
        let result = if finished_before {
            let result = execute(rig.wd.adapter(), rig.wd.clock(), &round);
            rig.network_change();
            result
        } else {
            rig.network_change();
            execute(rig.wd.adapter(), rig.wd.clock(), &round)
        };
        rig.wd.apply(result);
        assert_eq!(
            light(&mut rig, "a"),
            (Light::Yellow, LightReason::GracePeriod),
            "finished before the change: {finished_before}"
        );
        let next = rig.wd.plan();
        assert!(
            next.len() == 1 && is_round_of("a")(&next[0]),
            "no new round was due at once: {next:?}"
        );
    }
}

/// P1-3: what has to finish still finishes. A reconnect's stop out during a
/// network change is applied and followed by its start; a verification
/// round planned before a second change is ignored, and the verification
/// goes on with a new round instead of ending on the old answer.
#[test]
fn p1_3_a_stop_and_start_under_way_still_complete() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    let mut items = rig.wd.plan();
    assert_eq!(items.len(), 1, "{items:?}");
    let stop = items.remove(0);
    assert_eq!(stop.kind, WorkKind::Stop);
    assert!(rig.wd.still_due(&stop));
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &stop);
    rig.network_change();
    rig.wd.apply(result);

    let verify = run_until_planned(&mut rig, 60, is_round_of("a"));
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Verifying);
    assert_eq!((rig.stops("a"), rig.starts("a")), (1, 1));
    assert!(rig.wd.still_due(&verify));
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &verify);
    rig.network_change();
    rig.wd.apply(result);
    assert_eq!(
        rig.status("a").recovery,
        RecoveryPhase::Verifying,
        "the verification ended on an answer from before the change"
    );

    rig.run(10);
    let status = rig.status("a");
    assert_eq!(status.recovery, RecoveryPhase::Idle);
    assert_eq!(
        (status.light, status.light_reason),
        (Light::Green, LightReason::Verified)
    );
    assert!(status.fault.is_none(), "{:?}", status.fault);
    assert_eq!((rig.stops("a"), rig.starts("a")), (1, 1));
}

/// How long one probe of a round takes in [`run_timed`]: an answer after
/// 10 ms, anything else (no reply, an error) its timeout plus the second the
/// platform may take beyond it (see `driver::suggested_workers`).
fn probe_time(answered: bool, timeout: Duration) -> Duration {
    if answered {
        Duration::from_millis(10)
    } else {
        timeout + Duration::from_secs(1)
    }
}

/// Runs `secs` of fake time with rounds that take as long as real ones:
/// each target's probe finishes in turn (the recovery target first), and
/// the round is applied when its last probe is done, as a runtime does.
/// Other work is applied at once.
fn run_timed(rig: &mut Rig, secs: u64) {
    let end = rig.now_secs() + secs;
    while rig.now_secs() < end {
        let items = rig.wd.plan();
        if items.is_empty() {
            rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
            continue;
        }
        for item in items {
            if !rig.wd.still_due(&item) {
                continue;
            }
            let mut result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
            if let WorkKind::Round { probe_timeout, .. } = &item.kind
                && let WorkOutcome::Round {
                    probes: Some(set), ..
                } = &mut result.outcome
            {
                let start = result.finished;
                let wall = rig.wd.clock().wall_unix_ms();
                let mut took = Duration::ZERO;
                for probe in &mut set.probes {
                    probe.started_at_unix_ms = wall + i64::try_from(took.as_millis()).unwrap();
                    let answered = matches!(
                        probe.result,
                        Ok((_, ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. }))
                    );
                    took += probe_time(answered, *probe_timeout);
                    probe.finished = start + took;
                    probe.finished_at_unix_ms = wall + i64::try_from(took.as_millis()).unwrap();
                }
                rig.wd.clock().advance(took).unwrap();
                result.finished = rig.wd.clock().now();
            }
            rig.wd.apply(result);
        }
    }
}

/// P2-5, the reviewer's settings: interval 10 s, timeout 10 s and four
/// observe targets that never answer. The recovery target is probed first;
/// the four observe probes then take 4 × 11 s before the round is applied,
/// longer than the old freshness of 10 + 10 + 18 = 38 s, so every result of
/// the recovery target was thrown away: a new target never passed its
/// baseline, a dead data path was never confirmed and an incident never
/// closed. The freshness now covers the round's own span (4 × 11 s more).
#[test]
fn p2_5_a_long_round_does_not_make_its_own_recovery_result_stale() {
    let mut rig = Rig::new(&["a"]);
    let observe: Vec<_> = (200..204).map(icmp).collect();
    for last in 200..204 {
        rig.lab().kill_target(last);
    }
    rig.set_probe("a", timing(10, 10, 2, 60));
    // A new recovery target, whose baseline is still to pass.
    rig.set_targets("a", Some(icmp(9)), &observe);
    assert!(!rig.status("a").auto_recovery_allowed);

    for _ in 0..6 {
        run_timed(&mut rig, 30);
        let status = rig.status("a");
        assert!(status.auto_recovery_allowed, "the baseline never passed");
        assert_eq!(
            (status.light, status.light_reason),
            (Light::Green, LightReason::Verified),
            "at {} s",
            rig.now_secs()
        );
    }

    rig.lab().break_for_good("a");
    run_timed(&mut rig, 400);
    let status = rig.status("a");
    assert_eq!(status.data_health, DataHealth::Failed, "never confirmed");
    assert!(status.incident.is_some());

    // Back again: successes about 54 s apart, within the freshness, so the
    // incident closes after 120 s of them.
    rig.lab().heal("a");
    run_timed(&mut rig, 400);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green);
    assert!(status.incident.is_none(), "the incident never closed");
    // What the snapshot reports is the freshness the core uses.
    assert_eq!(status.freshness_secs, 10 + 10 + 18 + 4 * 11);
}
