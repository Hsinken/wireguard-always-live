//! Refusals of the runtime carry a reason beside their code (wave three,
//! stage 8) where the other tests do not already assert it: a full queue
//! of waiting requests, and host settings that cannot be stored because
//! the stored state was not loaded. Fakes only: simulation.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use wgal_core::{MemoryStore, Startup};
use wgal_protocol::{ErrorCode, ErrorReason, Request, StorageHealth};
use wgal_runtime::driver::Options;
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

#[test]
fn a_full_queue_of_waiting_requests_says_too_many_waiting() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let options = Options {
        max_calls: 1,
        ..Options::default()
    };
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &options,
    );
    sim.settle();
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    let (work, _) = sim.turn_only();
    assert_eq!(work.len(), 1, "{work:?}");
    let connect = || Request::Connect {
        tunnel_id: id("alpha"),
    };
    // The only place to wait is taken...
    assert!(sim.call(&user(), connect()).is_none());
    // ...so the next request for the tunnel is refused at once, untouched.
    let refused = sim.call(&user(), connect()).unwrap();
    assert_eq!(
        refusal_of(&refused),
        Some((ErrorCode::Busy, Some(ErrorReason::TooManyWaiting)))
    );
    assert_eq!(sim.engine.stats().held, 1);
    sim.finish(&work[0]);
    assert_eq!(sim.engine.stats().held, 0);
}

#[test]
fn host_settings_over_state_that_was_not_loaded_say_load_failed() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = watchdog(&lab, &clock, &store, Startup::StorageFailed);
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    );
    assert_eq!(sim.snapshot().storage, StorageHealth::Failed);
    let answer = sim.host_settings(Some(2), Some("Asia/Taipei"));
    assert_eq!(
        refusal_of(&answer),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        ))
    );
    assert_eq!(store.saved(), None, "nothing was written over it");
}
