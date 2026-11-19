use std::time::Duration;

/// Timers and thresholds of the watchdog that are the same for every tunnel
/// (plan §6.2). How often a tunnel is probed, how long one probe may take,
/// when its failures confirm a fault and how long a result stays fresh are
/// the tunnel's own `ProbeTiming` instead (owner decision of 2026-11-03). All
/// intervals are measured on the monotonic clock; wall time is used only for
/// records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Offset between the first rounds of successive tunnels, so they do not
    /// all probe at the same moment. Taken modulo each tunnel's interval.
    pub stagger: Duration,
    /// After the watchdog starts, the host wakes, the network changes or the
    /// local network comes back, failures are not counted for this long.
    pub grace: Duration,
    /// Longest wait for one stop or start phase to reach its state.
    pub control_phase_timeout: Duration,
    /// How often the OS state is polled while waiting for a phase.
    pub control_poll_interval: Duration,
    /// Longest traffic verification after a start.
    pub verify_window: Duration,
    /// Time between verification rounds.
    pub verify_interval: Duration,
    /// Wait before stopping again after a user's disconnect could not stop
    /// the tunnel; the last entry repeats. A disconnect is never given up.
    pub stop_retry: [Duration; 4],
    /// An incident closes after this long of continuous, fresh, healthy probes.
    pub incident_close_after: Duration,
    /// A stop, start or verification under way that has had no work planned
    /// for longer than this was held up by a sleeping host or a stalled
    /// process: its deadlines restart from now. Its polls and rounds are due
    /// every second or every few seconds, so this is far beyond any normal
    /// gap; it is the 35 s the plan gives a probe result.
    pub stall_after: Duration,
    /// How often a failed save is tried again even if nothing changed since.
    pub storage_retry: Duration,
}

impl Timing {
    /// The wait before the stop of a disconnect is tried again, after
    /// `attempts` failed tries.
    pub fn stop_retry_after(&self, attempts: u32) -> Duration {
        let last = self.stop_retry.len() - 1;
        let index = usize::try_from(attempts.saturating_sub(1)).map_or(last, |i| i.min(last));
        self.stop_retry[index]
    }

    /// How long a stop or start WGAL handed to the platform may still take
    /// effect after it was sent or its result came back without showing it:
    /// the platform's call may last a control phase, and a request it gave up
    /// on may land within another.
    pub fn late_effect(&self) -> Duration {
        self.control_phase_timeout.saturating_mul(2)
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            stagger: Duration::from_secs(2),
            grace: Duration::from_secs(30),
            control_phase_timeout: Duration::from_secs(30),
            control_poll_interval: Duration::from_secs(1),
            verify_window: Duration::from_secs(45),
            verify_interval: Duration::from_secs(5),
            stop_retry: [
                Duration::from_secs(5),
                Duration::from_secs(15),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ],
            incident_close_after: Duration::from_secs(120),
            stall_after: Duration::from_secs(35),
            storage_retry: Duration::from_secs(15),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_retries_grow_then_stay_at_one_minute() {
        let timing = Timing::default();
        let secs: Vec<u64> = (1..7)
            .map(|attempts| timing.stop_retry_after(attempts).as_secs())
            .collect();
        assert_eq!(secs, [5, 15, 30, 60, 60, 60]);
        assert_eq!(timing.stop_retry_after(0).as_secs(), 5);
        assert_eq!(timing.stop_retry_after(u32::MAX).as_secs(), 60);
    }
}
