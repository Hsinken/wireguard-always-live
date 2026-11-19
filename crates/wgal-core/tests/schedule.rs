//! What a runtime that sleeps between plans needs from the core:
//! `Watchdog::next_due` is never late and never now, the executor runs and
//! stands in for planned work, and the bindings behind tunnel IDs can be
//! looked up for authorization. Fake clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use wgal_core::work::WorkOutcome;
use wgal_core::{AdapterError, Clock, WorkKind};
use wgal_protocol::{AdapterErrorKind, Light, LightReason, OsState, Request};
use wgal_testkit::Operation;

use support::*;

#[test]
fn next_due_is_each_round_never_late_and_never_now() {
    let mut rig = Rig::new(&["a"]);
    for _ in 0..20 {
        let now = rig.wd.clock().now();
        let due = rig.wd.next_due().expect("a round is scheduled");
        assert!(due > now);
        let wait = due.saturating_duration_since(now);
        assert!(wait <= Duration::from_secs(15), "{wait:?}");
        // Nothing is due before then...
        let probes = rig.probes("a");
        if wait > Duration::from_millis(1) {
            rig.wd
                .clock()
                .advance(wait - Duration::from_millis(1))
                .unwrap();
            assert!(rig.wd.plan().is_empty());
            rig.wd.clock().advance(Duration::from_millis(1)).unwrap();
        } else {
            rig.wd.clock().advance(wait).unwrap();
        }
        // ...and at that time the round is.
        rig.wd.tick();
        assert_eq!(rig.probes("a"), probes + 1);
    }
}

#[test]
fn nothing_is_scheduled_without_monitoring() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::SetMonitoringPaused { paused: true });
    rig.wd.tick();
    assert_eq!(rig.wd.next_due(), None);
    rig.ok(Request::SetMonitoringPaused { paused: false });
    assert!(rig.wd.next_due().is_some() || !rig.wd.plan().is_empty());
}

#[test]
fn a_failed_disconnect_wakes_for_its_retry() {
    let mut rig = Rig::new(&["a"]);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::Stop,
        AdapterError::new(AdapterErrorKind::PermissionDenied, "refused"),
    );
    rig.ok(Request::Disconnect { tunnel_id: id("a") });
    rig.wd.tick();
    assert_eq!(rig.stops("a"), 1);
    let now = rig.wd.clock().now();
    let due = rig.wd.next_due().unwrap();
    assert_eq!(due.saturating_duration_since(now), Duration::from_secs(5));
    rig.wd.clock().advance(Duration::from_secs(5)).unwrap();
    rig.wd.tick();
    assert_eq!(rig.stops("a"), 2, "retried at the time next_due gave");
    assert_eq!(rig.status("a").os_state, OsState::Disconnected);
}

#[test]
fn the_executor_runs_planned_work_and_stands_in_for_work_that_broke_off() {
    let mut rig = Rig::new(&["a"]);
    let executor = rig.wd.executor();
    let wait = rig
        .wd
        .next_due()
        .unwrap()
        .saturating_duration_since(rig.wd.clock().now());
    rig.wd.clock().advance(wait).unwrap();
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert!(rig.wd.still_due(item));
    let error = AdapterError::new(AdapterErrorKind::ParseError, "the adapter broke off");
    let result = executor.failed(item, error.clone());
    assert_eq!(result.tunnel_id, item.tunnel_id);
    assert_eq!(result.seq, item.seq);
    assert!(matches!(
        &result.outcome,
        WorkOutcome::Round { os: Err(e), probes: None } if *e == error
    ));
    rig.wd.apply(result);
    let status = rig.status("a");
    assert_eq!(status.os_state, OsState::Unknown);
    assert_eq!(
        (status.light, status.light_reason),
        (Light::Red, LightReason::StatusUnknown)
    );
    assert_eq!(status.fault.unwrap().kind, AdapterErrorKind::ParseError);

    // The tunnel carries on: the next round runs through the executor.
    let wait = rig
        .wd
        .next_due()
        .unwrap()
        .saturating_duration_since(rig.wd.clock().now());
    rig.wd.clock().advance(wait).unwrap();
    let items = rig.wd.plan();
    assert_eq!(items.len(), 1);
    assert!(rig.wd.still_due(&items[0]));
    let result = executor.run(&items[0]);
    rig.wd.apply(result);
    assert_eq!(rig.status("a").light, Light::Green);

    // Each kind of item gets the matching failed outcome.
    for kind in [WorkKind::Poll, WorkKind::Stop, WorkKind::Start] {
        let mut other = items[0].clone();
        other.kind = kind.clone();
        let outcome = executor.failed(&other, error.clone()).outcome;
        match kind {
            WorkKind::Poll => assert!(matches!(outcome, WorkOutcome::Poll(Err(_)))),
            _ => assert!(matches!(outcome, WorkOutcome::Command(Err(_)))),
        }
    }
}

#[test]
fn bindings_behind_ids_can_be_looked_up() {
    let rig = Rig::new(&["a"]);
    assert_eq!(rig.wd.registered_binding(&id("a")), Some(&binding("a")));
    assert_eq!(rig.wd.discovered_binding(&id("a")), Some(&binding("a")));
    assert_eq!(rig.wd.registered_binding(&id("b")), None);
    assert_eq!(rig.wd.discovered_binding(&id("b")), None);
    assert!(rig.wd.installation_id().starts_with("wgal-"));
}
