use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use wgal_core::{Clock, MonoInstant};

/// A clock that moves only when the test says so.
///
/// Wall time is derived from monotonic time plus an offset, so advancing moves
/// both together, while [`FakeClock::shift_wall`] moves only wall time, as a
/// wall-clock correction must not affect interval logic.
///
/// Neither operation panics. One that would take monotonic time past
/// [`Duration::MAX`] or wall time outside `i64` milliseconds returns
/// [`ClockOverflow`] and leaves the clock unchanged.
#[derive(Debug)]
pub struct FakeClock {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    mono: Duration,
    /// Wall time at the monotonic origin, plus every wall shift. `i128` so an
    /// intermediate value can never overflow; [`wall_ms`] keeps the sum valid.
    wall_offset_ms: i128,
}

/// A [`FakeClock`] change was refused because the result would not be
/// representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockOverflow;

impl fmt::Display for ClockOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("fake clock would overflow; it was left unchanged")
    }
}

impl std::error::Error for ClockOverflow {}

/// Wall time for the given offset and monotonic time, if it fits in `i64`.
/// Wall time moves in whole elapsed milliseconds of monotonic time.
fn wall_ms(offset_ms: i128, mono: Duration) -> Option<i64> {
    let elapsed_ms = i128::try_from(mono.as_millis()).ok()?;
    i64::try_from(offset_ms.checked_add(elapsed_ms)?).ok()
}

impl FakeClock {
    /// Starts at [`MonoInstant::ORIGIN`] with the given wall time.
    pub fn new(wall_unix_ms: i64) -> Self {
        Self {
            state: Mutex::new(State {
                mono: Duration::ZERO,
                wall_offset_ms: i128::from(wall_unix_ms),
            }),
        }
    }

    /// Moves monotonic and wall time forward together, as real time passing would.
    pub fn advance(&self, by: Duration) -> Result<(), ClockOverflow> {
        let mut state = self.lock();
        let mono = state.mono.checked_add(by).ok_or(ClockOverflow)?;
        wall_ms(state.wall_offset_ms, mono).ok_or(ClockOverflow)?;
        state.mono = mono;
        Ok(())
    }

    /// Shifts wall time only, as an NTP correction or manual clock change would.
    pub fn shift_wall(&self, delta_ms: i64) -> Result<(), ClockOverflow> {
        let mut state = self.lock();
        let offset = state
            .wall_offset_ms
            .checked_add(i128::from(delta_ms))
            .ok_or(ClockOverflow)?;
        wall_ms(offset, state.mono).ok_or(ClockOverflow)?;
        state.wall_offset_ms = offset;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> MonoInstant {
        MonoInstant::from_origin(self.lock().mono)
    }

    fn wall_unix_ms(&self) -> i64 {
        let state = self.lock();
        // `new`, `advance` and `shift_wall` only ever store states for which
        // this is `Some`.
        wall_ms(state.wall_offset_ms, state.mono).unwrap_or(i64::MAX)
    }
}
