//! A check asked for while a tunnel is not monitored (owner decision of
//! 2026-11-06): `ProbeNow` probes its targets once, and the answer only
//! shows what each target last said; it is no evidence for the light, a
//! streak, a fault or any operation. Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use support::{Rig, id};
use wgal_core::execute;
use wgal_protocol::{Light, LightReason, ProbeResult, RecoveryPhase, Request, TunnelStatus};

fn monitor(rig: &mut Rig, name: &str, enabled: bool) {
    rig.ok(Request::SetTunnelMonitoring {
        tunnel_id: id(name),
        enabled,
    });
}

fn last_probe(status: &TunnelStatus) -> (i64, ProbeResult) {
    let probe = status
        .recovery_target
        .as_ref()
        .and_then(|t| t.last_probe.as_ref())
        .expect("the recovery target has a last probe");
    (probe.finished_at_unix_ms, probe.result)
}

fn check(rig: &mut Rig, name: &str) {
    rig.ok(Request::ProbeNow {
        tunnel_id: id(name),
    });
}

/// The check probes once and shows the answer, a failing one included,
/// while the light stays the unmonitored one and nothing else follows.
#[test]
fn a_check_while_monitoring_is_off_probes_once_and_only_shows_the_answer() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    rig.events();
    let (before, _) = last_probe(&rig.status("a"));
    let calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(rig.adapter_calls("a"), calls, "nothing on its own");

    rig.lab().break_for_good("a");
    let probes = rig.probes("a");
    check(&mut rig, "a");
    rig.run(10);
    assert_eq!(rig.probes("a"), probes + 1, "one probe of the one target");
    let status = rig.status("a");
    let (at, result) = last_probe(&status);
    assert!(at > before, "a new answer is shown");
    assert_eq!(result, ProbeResult::NoReply);
    assert_eq!(status.light, Light::Gray);
    assert_eq!(status.light_reason, LightReason::MonitoringOff);
    assert_eq!(status.consecutive_failures, 0, "no streak");
    assert_eq!(status.incident, None, "no fault");
    assert_eq!(rig.stops("a"), 0, "no recovery");
    assert!(rig.events_of("a").is_empty(), "nothing to log");

    let calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(rig.adapter_calls("a"), calls, "one round only");

    rig.lab().heal("a");
    check(&mut rig, "a");
    rig.run(10);
    let (_, result) = last_probe(&rig.status("a"));
    assert!(matches!(result, ProbeResult::Reply { .. }), "{result:?}");
    assert_eq!(rig.status("a").light, Light::Gray, "still not judged");
}

/// The host-wide pause is the same: a check asked for runs once.
#[test]
fn a_check_while_every_tunnel_is_paused_probes_once_too() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    let (before, _) = last_probe(&rig.status("a"));
    let (probes_a, probes_b) = (rig.probes("a"), rig.probes("b"));
    rig.lab().break_for_good("a");
    check(&mut rig, "a");
    rig.run(600);
    assert_eq!(rig.probes("a"), probes_a + 1, "once");
    assert_eq!(rig.probes("b"), probes_b, "the other tunnel is not probed");
    let status = rig.status("a");
    assert!(last_probe(&status).0 > before);
    assert_eq!(status.light, Light::Gray);
    assert_eq!(status.incident, None);
    assert_eq!(rig.stops("a"), 0);
}

/// Asking again while the one round is out adds no second round; asking
/// after it adds one more.
#[test]
fn checks_asked_twice_run_one_round_each_time_it_is_free() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    let probes = rig.probes("a");
    check(&mut rig, "a");
    check(&mut rig, "a");
    rig.run(10);
    assert_eq!(rig.probes("a"), probes + 1);
    check(&mut rig, "a");
    rig.run(10);
    assert_eq!(rig.probes("a"), probes + 2);
}

/// Turning monitoring on again works as before: rounds resume with a grace
/// period, and the answers of the check were no evidence for them.
#[test]
fn monitoring_on_again_after_a_check_starts_as_before() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    rig.lab().break_for_good("a");
    check(&mut rig, "a");
    rig.run(10);
    rig.lab().heal("a");
    monitor(&mut rig, "a", true);
    rig.run(120);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green, "{status:?}");
    assert_eq!(status.incident, None);
    assert_eq!(rig.stops("a"), 0);
}

/// The planned round of a check, and its result, by hand: what a runtime
/// that runs probes on threads of its own does between `plan` and `apply`.
fn planned_round(rig: &mut Rig) -> wgal_core::WorkItem {
    rig.wd
        .clock()
        .advance(std::time::Duration::from_secs(1))
        .unwrap();
    let mut items = rig.wd.plan();
    assert_eq!(items.len(), 1, "one round is planned");
    items.remove(0)
}

/// Asking again while the round is out adds no second round.
#[test]
fn a_check_asked_while_its_round_is_out_adds_none() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    let probes = rig.probes("a");
    check(&mut rig, "a");
    let item = planned_round(&mut rig);
    check(&mut rig, "a");
    assert!(rig.wd.still_due(&item));
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
    rig.wd.apply(result);
    rig.run(600);
    assert_eq!(rig.probes("a"), probes + 1, "one round for both requests");
}

/// A round withdrawn because monitoring came back on frees the next
/// request, and monitored rounds go on as before.
#[test]
fn a_withdrawn_check_frees_the_next_one() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    check(&mut rig, "a");
    let item = planned_round(&mut rig);
    monitor(&mut rig, "a", true);
    assert!(!rig.wd.still_due(&item), "a new generation withdraws it");
    rig.run(120);
    assert_eq!(rig.status("a").light, Light::Green);
    monitor(&mut rig, "a", false);
    let probes = rig.probes("a");
    check(&mut rig, "a");
    rig.run(10);
    assert_eq!(rig.probes("a"), probes + 1, "the next request still runs");
}

/// A result that comes back after monitoring came on again is dropped, and
/// is no evidence for the monitored rounds.
#[test]
fn a_check_result_after_monitoring_came_back_is_dropped() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    rig.lab().break_for_good("a");
    check(&mut rig, "a");
    let item = planned_round(&mut rig);
    assert!(rig.wd.still_due(&item));
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
    rig.lab().heal("a");
    let (shown, _) = last_probe(&rig.status("a"));
    monitor(&mut rig, "a", true);
    rig.wd.apply(result);
    assert_eq!(
        last_probe(&rig.status("a")).0,
        shown,
        "the late answer is not shown"
    );
    rig.run(120);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green, "{status:?}");
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(rig.stops("a"), 0);
}

/// A check asked for and then answered by monitoring coming back does not
/// run once monitoring is off again (Codex P2).
#[test]
fn a_check_answered_by_monitoring_does_not_run_later() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    check(&mut rig, "a");
    monitor(&mut rig, "a", true);
    rig.run(60);
    monitor(&mut rig, "a", false);
    let calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(rig.adapter_calls("a"), calls, "nothing left over");
}

/// A reconnect while monitoring is off verifies as before: its rounds are
/// verification rounds, not checks, so the verification succeeds (only a
/// verification round moves last_verified; a check never does).
#[test]
fn a_reconnect_while_monitoring_is_off_still_verifies() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    let verified = rig.status("a").last_verified_unix_ms.unwrap();
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(120);
    let status = rig.status("a");
    assert_eq!(rig.starts("a"), 1);
    assert_eq!(status.recovery, RecoveryPhase::Idle, "{status:?}");
    assert!(
        status.last_verified_unix_ms.unwrap() > verified,
        "verified: {status:?}"
    );
    assert_eq!(status.light, Light::Gray);
    assert_eq!(status.light_reason, LightReason::MonitoringOff);
}

/// A check asked for, then monitoring on and off again within one batch of
/// requests, with no plan in between, does not run later (Codex P2).
#[test]
fn a_check_answered_within_one_batch_does_not_run_later() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    monitor(&mut rig, "a", false);
    rig.run(5);
    check(&mut rig, "a");
    monitor(&mut rig, "a", true);
    monitor(&mut rig, "a", false);
    let calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(rig.adapter_calls("a"), calls, "nothing left over");

    // The same with the host-wide pause, for a tunnel whose own switch is
    // on (with it off, the tunnel stays unmonitored and the check runs).
    monitor(&mut rig, "a", true);
    rig.run(60);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    rig.run(5);
    check(&mut rig, "a");
    rig.ok(Request::SetMonitoringPaused { paused: false });
    rig.ok(Request::SetMonitoringPaused { paused: true });
    let calls = rig.adapter_calls("a");
    rig.run(600);
    assert_eq!(
        rig.adapter_calls("a"),
        calls,
        "nothing left over after the pause"
    );
}
