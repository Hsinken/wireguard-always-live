//! Regression tests for the independent review of wave two (2026-11-03),
//! core part: event-log labels that reveal nothing about the tunnel (P3-1)
//! and the backoff schedule after an inconclusive first round (P3-2). Fake
//! clock and fake platform only.

#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use support::{Lab, Rig, id};
use wgal_core::{MemoryStore, Startup, Timing, Watchdog, WorkKind, execute};
use wgal_protocol::{AdapterErrorKind, ErrorCode, FirstRecovery, Outcome, RecoveryPhase, Request};
use wgal_testkit::FakeClock;

fn step(rig: &mut Rig) {
    let wd = &mut rig.wd;
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    for item in wd.plan() {
        let result = execute(wd.adapter(), wd.clock(), &item);
        wd.apply(result);
    }
}

fn labels(rig: &Rig, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|n| rig.wd.log_label(&id(n)).unwrap().to_owned())
        .collect()
}

const NAMES: [&str; 3] = ["office", "home", "lab"];

/// P3-1: every tunnel gets its own label when it is registered: `t-` and 8
/// hex digits, drawn at random, so two installations with the same tunnels at
/// the same moment do not share them and nothing in a label comes from the
/// tunnel. It is stored and kept across restarts.
#[test]
fn p3_1_labels_are_random_stored_and_kept() {
    let first = Rig::new(&NAMES);
    let second = Rig::new(&NAMES);
    let ours = labels(&first, &NAMES);
    let theirs = labels(&second, &NAMES);
    for label in &ours {
        assert_eq!(label.len(), 10, "{label}");
        assert!(label.starts_with("t-"));
        assert!(label[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert_eq!(ours.iter().collect::<BTreeSet<_>>().len(), 3, "{ours:?}");
    for (a, b) in ours.iter().zip(&theirs) {
        assert_ne!(a, b, "the same tunnel got the same label twice");
    }

    let saved = first.store.saved().unwrap();
    let stored: Vec<_> = saved
        .tunnels
        .iter()
        .map(|t| t.log_label.clone().unwrap())
        .collect();
    assert_eq!(
        stored.iter().collect::<BTreeSet<_>>(),
        ours.iter().collect::<BTreeSet<_>>()
    );
    let restarted = Watchdog::new(
        Lab::new(&NAMES),
        FakeClock::new(support::T0 + 3_600_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    for (name, label) in NAMES.iter().zip(&ours) {
        assert_eq!(restarted.log_label(&id(name)), Some(label.as_str()));
    }
}

/// P3-1, migration: a state saved before labels existed loads, and every
/// tunnel gets a label that is saved at once.
#[test]
fn p3_1_a_state_from_before_labels_gets_them_and_saves_them() {
    let rig = Rig::new(&NAMES);
    let mut saved = rig.store.saved().unwrap();
    for rec in &mut saved.tunnels {
        rec.log_label = None;
    }
    let store = MemoryStore::new();
    let wd = Watchdog::new(
        Lab::new(&NAMES),
        FakeClock::new(support::T0 + 3_600_000),
        Timing::default(),
        Box::new(store.clone()),
        Startup::Restored(saved),
    );
    let given = NAMES
        .iter()
        .map(|n| wd.log_label(&id(n)).unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(given.len(), 3);
    let stored = store
        .saved()
        .expect("the labels were saved at start")
        .tunnels
        .iter()
        .map(|t| t.log_label.clone().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(stored, given);
}

/// P3-2: an inconclusive first round counts once, like any other round.
/// After it each round waits the tunnel's retry interval from the end of the
/// one before (owner decision of 2026-11-04, which replaced the plan's 1, 2,
/// 5 minutes), also when the first round later counts as failed; before the
/// fix a wait was skipped.
#[test]
fn p3_2_an_inconclusive_first_round_keeps_the_backoff_schedule() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: wgal_protocol::RecoveryRetry::new(None, 120).unwrap(),
    });
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let mut stops = Vec::new();
    let mut ends = Vec::new();
    let mut steps_at_stop = Vec::new();
    let mut was_verifying = false;
    while rig.now_secs() < 2400 && stops.len() < 4 {
        // Only the first verification meets probe-tool errors: inconclusive.
        rig.lab().fail_probes(
            "a",
            (was_verifying && ends.is_empty()).then_some(AdapterErrorKind::PermissionDenied),
        );
        let before = rig.stops("a");
        step(&mut rig);
        let status = rig.status("a");
        if rig.stops("a") > before {
            stops.push(rig.now_secs());
            steps_at_stop.push(status.incident.as_ref().unwrap().backoff_step);
        }
        let verifying = status.recovery == RecoveryPhase::Verifying;
        if was_verifying && !verifying {
            ends.push(rig.now_secs());
        }
        was_verifying = verifying;
    }
    assert_eq!(
        stops.len(),
        4,
        "stops at {stops:?}, rounds ended at {ends:?}"
    );
    assert_eq!(steps_at_stop, [0, 1, 2, 3], "each round counted once");
    let first = rig.status("a").incident.unwrap().first_recovery;
    assert_eq!(
        first,
        FirstRecovery::Failed,
        "the fault was confirmed again"
    );
    // The fault has to be confirmed again after the inconclusive round, and
    // the next round waits the interval too; the later rounds wait theirs,
    // checked at the next monitoring round (every 15 s).
    assert!(stops[1] - ends[0] >= 120, "{stops:?} {ends:?}");
    for k in [1, 2] {
        let gap = stops[k + 1] - ends[k];
        let wait = 120;
        assert!(
            (wait..=wait + 16).contains(&gap),
            "after round {}: {gap} s, expected {wait} s",
            k + 1
        );
    }
}

/// Review of wave two, N-P3-4, core part: a runtime that runs items on
/// other threads may plan again while an automatic stop is out. If storage
/// fails (or automatic actions are held) meanwhile, that operation has sent
/// something and must not be dropped as if it had not: its start still goes
/// out, so WGAL does not leave down a tunnel it stopped.
#[test]
fn an_automatic_stop_out_when_storage_fails_still_gets_its_start() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    let stop = 'found: loop {
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
        for item in rig.wd.plan() {
            if item.tunnel_id == id("a") && item.kind == WorkKind::Stop {
                break 'found item;
            }
            let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
            rig.wd.apply(result);
        }
        assert!(rig.now_secs() < 600, "recovery never started");
    };
    assert!(rig.wd.still_due(&stop));
    // Another change cannot be stored while the stop is out.
    rig.store.set_failing(true);
    assert!(matches!(
        rig.request(Request::SetStartWhenStoppedExternally {
            tunnel_id: id("b"),
            enabled: false,
        }),
        Outcome::Err(e) if e.code == ErrorCode::StorageUnavailable
    ));
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    assert!(
        rig.wd.plan().iter().all(|i| i.tunnel_id != id("a")),
        "nothing more for a while its stop is out"
    );
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &stop);
    rig.wd.apply(result);
    rig.run(60);
    assert_eq!(rig.starts("a"), 1, "WGAL left the tunnel it stopped down");
}

/// The same with automatic actions held for a shutdown.
#[test]
fn an_automatic_stop_out_when_actions_are_held_still_gets_its_start() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    let stop = 'found: loop {
        rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
        for item in rig.wd.plan() {
            if item.kind == WorkKind::Stop {
                break 'found item;
            }
            let result = execute(rig.wd.adapter(), rig.wd.clock(), &item);
            rig.wd.apply(result);
        }
        assert!(rig.now_secs() < 600, "recovery never started");
    };
    assert!(rig.wd.still_due(&stop));
    rig.wd.hold_automatic_actions();
    rig.wd.clock().advance(Duration::from_secs(1)).unwrap();
    assert!(rig.wd.plan().is_empty());
    let result = execute(rig.wd.adapter(), rig.wd.clock(), &stop);
    rig.wd.apply(result);
    rig.run(60);
    assert_eq!(rig.starts("a"), 1, "WGAL left the tunnel it stopped down");
    // Held: the fault confirmed again later starts nothing new.
    rig.lab().break_for_good("a");
    rig.run(600);
    assert_eq!(rig.stops("a"), 1);
}
