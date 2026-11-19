//! Network change notices for the service (plan §6.2, T15): the IP Helper's
//! notices of changes to the host's IPv4 interfaces become
//! `Client::on_network_change` calls, which start every tunnel's 30 s grace
//! period again, so the moments while Wi-Fi reconnects or a cable is
//! plugged in are not counted as failed probes.
//!
//! - Only changes on interfaces that can carry the host's own way out count
//!   ([`InterfaceChange::is_network_change`]): never loopback, tunnel or
//!   virtual adapters, so a tunnel WGAL (or anyone) starts or stops does not
//!   restart every tunnel's grace period.
//! - Bursts are coalesced ([`Coalescer`]): a Wi-Fi reconnect fires many
//!   notices within a second or two. The first passes at once, so the grace
//!   period starts while the network is still changing; the rest of a burst
//!   becomes one more call once the notices have stopped for
//!   [`Coalescing::settle`], so the grace period counts from when the
//!   network settled. Calls are at least [`Coalescing::gap`] apart, and a
//!   burst that never settles still passes one call every
//!   [`Coalescing::longest`], inside the grace period, so it never lapses
//!   while the network keeps changing. Each call makes every tunnel probe
//!   again at once, so passing every notice would be a burst of work of its
//!   own.
//!
//! The registration is dropped when the service stops, which cancels it and
//! ends the forwarding thread.

use std::fmt;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_platform::windows::{InterfaceChange, InterfaceWatch, Win32Error};

use super::host::Control;

/// How notices are coalesced into calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coalescing {
    /// Least time between two calls.
    pub gap: Duration,
    /// Quiet time after the last notice before a burst's last call.
    pub settle: Duration,
    /// Longest a notice waits for its call while the notices go on.
    pub longest: Duration,
}

impl Coalescing {
    pub const DEFAULT: Self = Self {
        gap: Duration::from_secs(2),
        settle: Duration::from_secs(2),
        // Well inside the core's 30 s grace period (plan §6.2).
        longest: Duration::from_secs(20),
    };
}

/// When notices become calls, judged at instants the caller gives, so it is
/// tested without waiting.
#[derive(Debug)]
pub(crate) struct Coalescer {
    timing: Coalescing,
    last_call: Option<Instant>,
    /// The first notice not yet passed on.
    pending_since: Option<Instant>,
    last_notice: Option<Instant>,
}

impl Coalescer {
    pub(crate) fn new(timing: Coalescing) -> Self {
        Self {
            timing,
            last_call: None,
            pending_since: None,
            last_notice: None,
        }
    }

    /// A notice arrived at `now`. True if it is to be passed on now; false
    /// if it waits for [`Self::due_at`].
    pub(crate) fn notice(&mut self, now: Instant) -> bool {
        self.last_notice = Some(now);
        let quiet_before = self.pending_since.is_none()
            && self
                .last_call
                .is_none_or(|at| now.saturating_duration_since(at) >= self.timing.gap);
        if quiet_before {
            self.last_call = Some(now);
            return true;
        }
        self.pending_since.get_or_insert(now);
        false
    }

    /// When the waiting notices are to be passed on, if any wait.
    pub(crate) fn due_at(&self) -> Option<Instant> {
        let since = self.pending_since?;
        let settled = self.last_notice.unwrap_or(since) + self.timing.settle;
        let due = settled.min(since + self.timing.longest);
        Some(match self.last_call {
            Some(at) => due.max(at + self.timing.gap),
            None => due,
        })
    }

    /// True if the waiting notices are due at `now`; they then count as
    /// passed on.
    pub(crate) fn take_due(&mut self, now: Instant) -> bool {
        if self.due_at().is_some_and(|at| now >= at) {
            self.pending_since = None;
            self.last_call = Some(now);
            true
        } else {
            false
        }
    }
}

/// Passes `notices` on through `pass_on` under `timing` until the channel
/// closes. Notices that are not network changes are dropped.
pub(crate) fn forward(
    notices: &Receiver<InterfaceChange>,
    timing: Coalescing,
    mut pass_on: impl FnMut(),
) {
    let mut coalescer = Coalescer::new(timing);
    loop {
        let received = match coalescer.due_at() {
            Some(at) => notices.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => notices.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(change) => {
                if change.is_network_change() && coalescer.notice(Instant::now()) {
                    pass_on();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The registration was cancelled: the service is stopping, and
            // a change still waiting no longer matters.
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if coalescer.take_due(Instant::now()) {
            pass_on();
        }
    }
}

/// The registration and the thread that forwards its notices to the
/// service as [`Control::NetworkChange`]. Dropping it cancels the
/// registration and waits for the thread, which then ends at once.
pub(crate) struct NetworkWatch {
    registration: Option<InterfaceWatch>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for NetworkWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkWatch")
            .field("registered", &self.registration.is_some())
            .finish_non_exhaustive()
    }
}

/// Why network changes are not watched.
#[derive(Debug)]
pub(crate) enum WatchError {
    Register(Win32Error),
    Thread(std::io::Error),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(e) => write!(f, "the registration failed (Windows error {})", e.code),
            Self::Thread(e) => write!(f, "its thread could not start: {e}"),
        }
    }
}

impl NetworkWatch {
    /// Registers for interface notices and forwards network changes to
    /// `controls` under `timing`.
    pub(crate) fn start(controls: Sender<Control>, timing: Coalescing) -> Result<Self, WatchError> {
        let (registration, notices) = InterfaceWatch::start().map_err(WatchError::Register)?;
        let thread = thread::Builder::new()
            .name("wgal-network".to_owned())
            .spawn(move || {
                forward(&notices, timing, || {
                    // The service may already be gone.
                    let _ = controls.send(Control::NetworkChange);
                });
            })
            // Dropping the registration on the way out cancels it.
            .map_err(WatchError::Thread)?;
        Ok(Self {
            registration: Some(registration),
            thread: Some(thread),
        })
    }
}

impl Drop for NetworkWatch {
    fn drop(&mut self) {
        // Cancelling closes the channel, which ends the thread.
        drop(self.registration.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const ETHERNET: u64 = 6 << 48;
    const WIFI: u64 = 71 << 48;
    const WIREGUARD: u64 = 53 << 48;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    /// The default timing, in milliseconds instead of seconds.
    fn fast() -> Coalescing {
        Coalescing {
            gap: Duration::from_millis(2_000),
            settle: Duration::from_millis(2_000),
            longest: Duration::from_millis(20_000),
        }
    }

    #[test]
    fn a_single_notice_passes_at_once_and_nothing_follows() {
        let t = Instant::now();
        let mut c = Coalescer::new(fast());
        assert!(c.notice(t));
        assert_eq!(c.due_at(), None);
        assert!(!c.take_due(at(t, 60_000)));
    }

    /// A Wi-Fi reconnect: twenty notices over three seconds become one call
    /// at once and one after they have stopped for the settle time.
    #[test]
    fn a_burst_becomes_one_call_at_once_and_one_once_it_settles() {
        let t = Instant::now();
        let mut c = Coalescer::new(fast());
        let mut calls = Vec::new();
        for n in 0..20 {
            let now = at(t, n * 150);
            if c.notice(now) {
                calls.push(n * 150);
            }
            assert!(!c.take_due(now));
        }
        // The last notice came at 2850 ms: due 2 s later.
        assert_eq!(c.due_at(), Some(at(t, 4_850)));
        assert!(!c.take_due(at(t, 4_849)));
        assert!(c.take_due(at(t, 4_850)));
        calls.push(4_850);
        assert_eq!(calls, [0, 4_850]);
        assert_eq!(c.due_at(), None);
    }

    #[test]
    fn calls_are_never_closer_than_the_gap() {
        let t = Instant::now();
        let mut c = Coalescer::new(Coalescing {
            settle: Duration::from_millis(100),
            ..fast()
        });
        assert!(c.notice(t));
        assert!(!c.notice(at(t, 50)));
        // Settled at 150 ms, but the gap holds it until 2 s.
        assert_eq!(c.due_at(), Some(at(t, 2_000)));
        assert!(c.take_due(at(t, 2_000)));
        // A notice right after a call waits for the gap too.
        assert!(!c.notice(at(t, 2_100)));
        assert_eq!(c.due_at(), Some(at(t, 4_000)));
    }

    #[test]
    fn a_notice_after_a_quiet_gap_passes_at_once_again() {
        let t = Instant::now();
        let mut c = Coalescer::new(fast());
        assert!(c.notice(t));
        assert!(c.notice(at(t, 2_000)));
        assert!(c.notice(at(t, 10_000)));
    }

    /// A network that keeps changing still gets a call every `longest`, so
    /// the grace period never lapses while it does.
    #[test]
    fn a_burst_that_never_settles_still_passes_one_call_every_longest() {
        let t = Instant::now();
        let mut c = Coalescer::new(fast());
        let mut calls = Vec::new();
        for n in 0..=600 {
            let now = at(t, n * 100);
            if c.notice(now) || c.take_due(now) {
                calls.push(n * 100);
            }
        }
        assert_eq!(calls, [0, 20_100, 40_200]);
    }

    #[test]
    fn forwarding_drops_tunnel_changes_and_ends_with_the_channel() {
        let (to, notices) = mpsc::sync_channel(64);
        for luid in [WIREGUARD, WIREGUARD | (5 << 24), 24 << 48, 131 << 48] {
            to.send(InterfaceChange { luid }).unwrap();
        }
        drop(to);
        let mut calls = 0;
        forward(&notices, fast(), || calls += 1);
        assert_eq!(calls, 0);
    }

    /// Real threads and short real times: a burst on Wi-Fi becomes two
    /// calls; a tunnel's notices in between add none; the thread ends once
    /// the channel closes.
    #[test]
    fn forwarding_coalesces_a_burst_on_a_real_channel() {
        let timing = Coalescing {
            gap: Duration::from_millis(100),
            settle: Duration::from_millis(100),
            longest: Duration::from_secs(5),
        };
        let (to, notices) = mpsc::sync_channel(64);
        let (calls_to, calls) = mpsc::channel();
        let thread = thread::spawn(move || {
            forward(&notices, timing, || calls_to.send(Instant::now()).unwrap());
        });
        let started = Instant::now();
        for n in 0..10_u64 {
            let luid = if n % 2 == 0 { WIFI } else { WIREGUARD };
            to.send(InterfaceChange { luid: luid | n }).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        to.send(InterfaceChange { luid: ETHERNET }).unwrap();
        thread::sleep(Duration::from_millis(400));
        drop(to);
        thread.join().unwrap();
        let calls: Vec<_> = calls.try_iter().collect();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0] - started < Duration::from_millis(100));
        assert!(calls[1] - calls[0] >= Duration::from_millis(100));
    }

    /// Registers with the real IP Helper and cancels; no network change is
    /// caused. Ignored only because it registers for real.
    #[test]
    #[ignore = "registers a real IP Helper notification; run on its own"]
    fn a_real_registration_is_cancelled_on_drop() {
        let (controls, _inbox) = mpsc::channel();
        let watch = NetworkWatch::start(controls, Coalescing::DEFAULT).unwrap();
        let started = Instant::now();
        drop(watch);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
