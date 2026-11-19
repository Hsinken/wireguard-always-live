use std::ops::Add;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A point on a monotonic timeline, measured from the owning clock's origin.
///
/// Interval logic (timeouts, thresholds, backoff) must use this rather than
/// wall time, so a wall-clock adjustment can neither shorten nor stretch it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoInstant(Duration);

impl MonoInstant {
    pub const ORIGIN: Self = Self(Duration::ZERO);

    pub const fn from_origin(offset: Duration) -> Self {
        Self(offset)
    }

    pub const fn since_origin(self) -> Duration {
        self.0
    }

    /// Time from `earlier` to `self`, or zero if `earlier` is actually later.
    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn checked_add(self, offset: Duration) -> Option<Self> {
        self.0.checked_add(offset).map(Self)
    }
}

impl Add<Duration> for MonoInstant {
    type Output = Self;

    /// Panics on overflow, like [`Instant`].
    fn add(self, offset: Duration) -> Self {
        Self(self.0 + offset)
    }
}

/// Source of time for the watchdog.
pub trait Clock: Send + Sync {
    /// Monotonic time, for measuring intervals. Never goes backwards.
    fn now(&self) -> MonoInstant;

    /// Wall-clock milliseconds since the Unix epoch, for records and display
    /// only. It can jump in either direction.
    fn wall_unix_ms(&self) -> i64;
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> MonoInstant {
        (**self).now()
    }

    fn wall_unix_ms(&self) -> i64 {
        (**self).wall_unix_ms()
    }
}

/// The real clock: [`Instant`] for monotonic time, [`SystemTime`] for wall time.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> MonoInstant {
        MonoInstant(self.origin.elapsed())
    }

    fn wall_unix_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |ms| -ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_instant_arithmetic() {
        let a = MonoInstant::from_origin(Duration::from_secs(10));
        let b = a + Duration::from_secs(5);
        assert_eq!(b.saturating_duration_since(a), Duration::from_secs(5));
        assert_eq!(a.saturating_duration_since(b), Duration::ZERO);
        assert_eq!(
            MonoInstant::from_origin(Duration::MAX).checked_add(Duration::from_nanos(1)),
            None
        );
    }

    #[test]
    fn system_clock_is_monotonic() {
        let clock = SystemClock::new();
        let first = clock.now();
        let second = clock.now();
        assert!(second >= first);
        assert!(clock.wall_unix_ms() > 0);
    }
}
