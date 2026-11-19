//! The wall times a snapshot reports hold still from one snapshot to the
//! next (day 2, 2026-11-05, P3): the watchdog keeps the clock pair it
//! writes them with while the wall clock agrees with it, and takes a new
//! one when the clock is set.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::Rig;

/// Moves the monotonic clock by `micros` without a tick, as time passes
/// between two snapshots the GUI asks for.
fn pass(rig: &Rig, micros: u64) {
    rig.wd
        .clock()
        .advance(Duration::from_micros(micros))
        .unwrap();
}

/// What `read` gives in 20 snapshots taken 0.3 ms apart.
fn over_snapshots<T: std::fmt::Debug + PartialEq>(
    rig: &mut Rig,
    read: impl Fn(&mut Rig) -> T,
) -> Vec<T> {
    (0..20)
        .map(|_| {
            pass(rig, 300);
            read(rig)
        })
        .collect()
}

fn all_equal<T: std::fmt::Debug + PartialEq>(values: &[T], what: &str) {
    assert!(
        values.windows(2).all(|w| w[0] == w[1]),
        "{what} moved between snapshots: {values:?}"
    );
}

/// A rig whose ticks fall 0.6 ms after each second, so the instants it
/// records are not whole milliseconds of the wall clock.
fn off_the_millisecond() -> Rig {
    let rig = Rig::new(&["a"]);
    pass(&rig, 600);
    rig
}

#[test]
fn the_last_verification_holds_still() {
    let mut rig = off_the_millisecond();
    rig.run(70);
    let seen = over_snapshots(&mut rig, |rig| rig.status("a").last_verified_unix_ms);
    assert!(seen[0].is_some());
    all_equal(&seen, "last_verified_unix_ms");
}

#[test]
fn a_deadline_holds_still() {
    let mut rig = off_the_millisecond();
    rig.manage("a");
    rig.run(10);
    rig.lab().break_for_good("a");
    let mut deadline = None;
    for _ in 0..200 {
        rig.run(1);
        deadline = rig.status("a").deadline_unix_ms;
        if deadline.is_some() {
            break;
        }
    }
    assert!(deadline.is_some(), "no deadline while confirming");
    let seen = over_snapshots(&mut rig, |rig| rig.status("a").deadline_unix_ms);
    all_equal(&seen, "deadline_unix_ms");
}

#[test]
fn the_start_of_a_stable_period_holds_still() {
    let mut rig = off_the_millisecond();
    rig.manage("a");
    rig.run(10);
    rig.lab().break_for_good("a");
    rig.run(160);
    assert!(rig.status("a").incident.is_some());
    rig.lab().heal("a");
    rig.run(15);
    let seen = over_snapshots(&mut rig, |rig| {
        rig.status("a")
            .incident
            .and_then(|incident| incident.stable_since_unix_ms)
    });
    assert!(seen[0].is_some(), "no stable period");
    all_equal(&seen, "stable_since_unix_ms");
}

#[test]
fn a_wall_clock_set_is_followed_and_a_tiny_step_is_not() {
    let mut rig = off_the_millisecond();
    rig.run(70);
    let before = rig.status("a").last_verified_unix_ms.unwrap();
    // A step within the tolerance keeps the pair: nothing moves.
    rig.wd.clock().shift_wall(50).unwrap();
    assert_eq!(rig.status("a").last_verified_unix_ms, Some(before));
    // The clock set an hour ahead: the times follow it.
    rig.wd.clock().shift_wall(3_600_000).unwrap();
    let after = rig.status("a").last_verified_unix_ms.unwrap();
    let moved = after - before;
    assert!(
        (3_600_049..=3_600_051).contains(&moved),
        "moved by {moved} ms"
    );
    // And hold still again at the new pair.
    let seen = over_snapshots(&mut rig, |rig| rig.status("a").last_verified_unix_ms);
    all_equal(&seen, "last_verified_unix_ms after the clock was set");
    assert_eq!(seen[0], Some(after));
}
