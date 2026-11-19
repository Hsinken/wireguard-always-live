//! Deterministic test doubles for the watchdog.
//!
//! Depend on this crate from `[dev-dependencies]` only. Nothing here touches
//! the OS, the network or a real tunnel: [`FakeClock`] moves only when a test
//! advances it, and [`FakePlatform`] keeps all tunnel state in memory.

mod fake_clock;
mod fake_platform;

pub use fake_clock::{ClockOverflow, FakeClock};
pub use fake_platform::{Call, FakePlatform, Operation};
