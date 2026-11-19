#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use wgal_core::{Clock, MonoInstant};
use wgal_testkit::{ClockOverflow, FakeClock};

const START_WALL_MS: i64 = 1_767_225_600_000;

/// Largest `u64` that is also a valid `i64`.
const I64_MAX_U64: u64 = i64::MAX as u64;

fn assert_unchanged(clock: &FakeClock, mono: MonoInstant, wall: i64) {
    assert_eq!(clock.now(), mono);
    assert_eq!(clock.wall_unix_ms(), wall);
}

#[test]
fn starts_at_origin_and_moves_only_when_advanced() {
    let clock = FakeClock::new(START_WALL_MS);
    assert_eq!(clock.now(), MonoInstant::ORIGIN);
    assert_eq!(clock.now(), MonoInstant::ORIGIN);
    assert_eq!(clock.wall_unix_ms(), START_WALL_MS);

    clock.advance(Duration::from_millis(1500)).unwrap();
    assert_eq!(clock.now().since_origin(), Duration::from_millis(1500));
    assert_eq!(clock.wall_unix_ms(), START_WALL_MS + 1500);
}

#[test]
fn sub_millisecond_advances_accumulate_into_wall_time() {
    let clock = FakeClock::new(START_WALL_MS);
    for _ in 0..4 {
        clock.advance(Duration::from_micros(500)).unwrap();
    }
    assert_eq!(clock.now().since_origin(), Duration::from_millis(2));
    assert_eq!(clock.wall_unix_ms(), START_WALL_MS + 2);
}

#[test]
fn wall_clock_jumps_do_not_move_monotonic_time() {
    let clock = FakeClock::new(START_WALL_MS);
    clock.advance(Duration::from_secs(10)).unwrap();
    let before = clock.now();

    clock.shift_wall(-3_600_000).unwrap();
    assert_eq!(clock.now(), before);
    assert_eq!(clock.wall_unix_ms(), START_WALL_MS + 10_000 - 3_600_000);

    clock.shift_wall(86_400_000).unwrap();
    assert_eq!(clock.now(), before);
}

#[test]
fn shared_handle_sees_advances() {
    let clock = Arc::new(FakeClock::new(START_WALL_MS));
    let as_dyn: Arc<dyn Clock> = clock.clone();
    let start = as_dyn.now();

    clock.advance(Duration::from_secs(30)).unwrap();
    assert_eq!(
        as_dyn.now().saturating_duration_since(start),
        Duration::from_secs(30)
    );
}

#[test]
fn advance_up_to_the_last_representable_millisecond_succeeds() {
    let clock = FakeClock::new(0);
    clock.advance(Duration::from_millis(I64_MAX_U64)).unwrap();
    assert_eq!(clock.wall_unix_ms(), i64::MAX);

    // Still inside the last millisecond, so wall time is unchanged.
    clock.advance(Duration::from_nanos(999_999)).unwrap();
    assert_eq!(clock.wall_unix_ms(), i64::MAX);
}

#[test]
fn advance_past_the_wall_limit_fails_and_leaves_the_clock_unchanged() {
    let clock = FakeClock::new(0);
    clock
        .advance(Duration::from_millis(I64_MAX_U64) + Duration::from_nanos(999_999))
        .unwrap();
    let (mono, wall) = (clock.now(), clock.wall_unix_ms());

    assert_eq!(clock.advance(Duration::from_nanos(1)), Err(ClockOverflow));
    assert_unchanged(&clock, mono, wall);
}

#[test]
fn advance_by_duration_max_fails_and_leaves_the_clock_unchanged() {
    // Even from the lowest wall time, Duration::MAX is far past i64 milliseconds.
    let clock = FakeClock::new(i64::MIN);
    clock.advance(Duration::from_secs(5)).unwrap();
    let (mono, wall) = (clock.now(), clock.wall_unix_ms());

    assert_eq!(clock.advance(Duration::MAX), Err(ClockOverflow));
    assert_unchanged(&clock, mono, wall);
}

#[test]
fn monotonic_overflow_fails_and_leaves_the_clock_unchanged() {
    let clock = FakeClock::new(i64::MIN);
    // The full u64 millisecond range fits between i64::MIN and i64::MAX.
    clock.advance(Duration::from_millis(u64::MAX)).unwrap();
    assert_eq!(clock.wall_unix_ms(), i64::MAX);
    let (mono, wall) = (clock.now(), clock.wall_unix_ms());

    assert_eq!(clock.advance(Duration::MAX), Err(ClockOverflow));
    assert_eq!(clock.advance(Duration::from_millis(1)), Err(ClockOverflow));
    assert_unchanged(&clock, mono, wall);
}

#[test]
fn wall_shifts_stop_exactly_at_the_i64_bounds() {
    let clock = FakeClock::new(0);
    clock.shift_wall(i64::MAX).unwrap();
    assert_eq!(clock.wall_unix_ms(), i64::MAX);
    assert_eq!(clock.shift_wall(1), Err(ClockOverflow));
    assert_eq!(clock.advance(Duration::from_millis(1)), Err(ClockOverflow));
    assert_unchanged(&clock, MonoInstant::ORIGIN, i64::MAX);

    let clock = FakeClock::new(-1);
    clock.shift_wall(i64::MIN + 1).unwrap();
    assert_eq!(clock.wall_unix_ms(), i64::MIN);
    assert_eq!(clock.shift_wall(-1), Err(ClockOverflow));
    assert_eq!(clock.shift_wall(i64::MIN), Err(ClockOverflow));
    assert_unchanged(&clock, MonoInstant::ORIGIN, i64::MIN);

    // Back inside the range works again.
    clock.shift_wall(i64::MAX).unwrap();
    assert_eq!(clock.wall_unix_ms(), -1);
}

#[test]
fn wall_shift_is_judged_on_the_result_not_the_internal_offset() {
    // After a long advance the internal offset is far below the wall time; a
    // shift that keeps wall time in range must still succeed.
    let clock = FakeClock::new(0);
    clock.advance(Duration::from_millis(I64_MAX_U64)).unwrap();
    clock.shift_wall(i64::MIN).unwrap();
    assert_eq!(clock.wall_unix_ms(), -1);
}
