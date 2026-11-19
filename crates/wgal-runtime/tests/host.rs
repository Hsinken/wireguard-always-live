//! Host settings (owner's requests 6 and 9 of 2026-11-03): the loop checks
//! the time zone against its database, stores the settings through the core
//! before applying them, reports them in every snapshot, follows the
//! system's zone when none is set and reads it again now and then. The
//! system zone comes from the test, never from this machine. Simulation
//! only.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use jiff::tz::TimeZone;
use wgal_core::{Clock, MemoryStore, Startup};
use wgal_protocol::{ErrorCode, ErrorReason, HostSettings, Request, TimeZoneName, WorkerCount};
use wgal_runtime::driver::{Engine, Options, ZONE_REFRESH};
use wgal_runtime::events::EventLog;
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

fn zone(name: &str) -> TimeZone {
    jiff::tz::db().get(name).unwrap()
}

#[test]
fn host_settings_are_stored_applied_and_reported() {
    let mut sim = Sim::of(&["alpha", "beta"], None);
    sim.settle();
    let host = sim.host();
    assert_eq!(host.workers.configured, None);
    assert_eq!(host.workers.limit, 3);
    assert_eq!(host.workers.suggested, 2);
    assert_eq!(host.workers.effective, 2);
    // The rig stores UTC so times do not depend on this machine.
    assert_eq!(host.time_zone.configured.as_ref().unwrap().as_str(), "UTC");
    assert_eq!(host.time_zone.effective.as_ref().unwrap().as_str(), "UTC");
    assert_eq!(host.time_zone.utc_offset_secs, 0);

    // A plain user may change them.
    assert!(accepted(&sim.host_settings(Some(3), Some("Asia/Taipei"))));
    let host = sim.host();
    assert_eq!(host.workers.configured, Some(WorkerCount::new(3).unwrap()));
    assert_eq!(host.workers.effective, 3);
    assert_eq!(
        host.time_zone.configured,
        Some(TimeZoneName::new("Asia/Taipei").unwrap())
    );
    assert_eq!(
        host.time_zone.effective,
        Some(TimeZoneName::new("Asia/Taipei").unwrap())
    );
    assert_eq!(host.time_zone.utc_offset_secs, 8 * 3600);
    assert_eq!(sim.engine.time_zone().iana_name(), Some("Asia/Taipei"));
    // Stored before it was applied: a restart keeps it.
    let saved = sim.store.saved().unwrap();
    assert_eq!(
        saved.host,
        HostSettings {
            workers: Some(WorkerCount::new(3).unwrap()),
            time_zone: Some(TimeZoneName::new("Asia/Taipei").unwrap()),
        }
    );
    let wd = watchdog(&sim.lab, &sim.clock, &sim.store, Startup::Restored(saved));
    let restarted = Engine::new(
        wd,
        quiet_notifier(),
        AccessList::default(),
        None,
        &Options::default(),
    )
    .with_system_zone(|| Some(zone("America/New_York")));
    assert_eq!(restarted.time_zone().iana_name(), Some("Asia/Taipei"));
    let wall = restarted.watchdog().clock().wall_unix_ms();
    assert_eq!(restarted.host_status(wall).workers.effective, 3);
}

#[test]
fn an_unknown_zone_is_refused_and_changes_nothing() {
    let mut sim = Sim::of(&["alpha"], None);
    assert!(accepted(&sim.host_settings(Some(2), Some("Europe/Berlin"))));
    let before = sim.host();
    for name in ["Mars/Olympus_Mons", "Etc/Unknown", "Asia/Taipei_Old"] {
        let answer = sim.host_settings(Some(2), Some(name));
        assert_eq!(
            refusal_of(&answer),
            Some((
                ErrorCode::InvalidSetting,
                Some(ErrorReason::UnknownTimeZone)
            )),
            "{name}"
        );
        assert_eq!(sim.host(), before, "{name}");
    }
    assert_eq!(sim.engine.time_zone().iana_name(), Some("Europe/Berlin"));
    assert_eq!(
        sim.store.saved().unwrap().host.time_zone.unwrap().as_str(),
        "Europe/Berlin"
    );
}

#[test]
fn a_failed_save_changes_nothing() {
    let dir = TempDir::new("host-fail");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    sim.settle();
    let before = sim.host();
    sim.store.set_failing(true);
    let answer = sim.host_settings(Some(2), Some("Asia/Taipei"));
    assert_eq!(
        refusal_of(&answer),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    sim.store.set_failing(false);
    assert_eq!(sim.host(), before);
    assert_eq!(sim.engine.watchdog().host_settings().workers, None);
    assert_eq!(sim.engine.time_zone(), &TimeZone::UTC);
    let lines = log_lines(dir.path());
    assert!(events_named(&lines, "settings_changed").is_empty());
}

/// Which zone the test's "system" has: 0 Taipei, 1 New York, 2 none.
static SYSTEM: AtomicU8 = AtomicU8::new(0);

fn switchable() -> Option<TimeZone> {
    match SYSTEM.load(Ordering::SeqCst) {
        0 => Some(zone("Asia/Taipei")),
        1 => Some(zone("America/New_York")),
        _ => None,
    }
}

/// No zone set: the system's, read again every ten minutes, and UTC when
/// the system's cannot be read. The test's "system" is a switch.
#[test]
fn without_a_zone_the_system_zone_is_followed() {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    assert_eq!(wd.host_settings().time_zone, None);
    SYSTEM.store(0, Ordering::SeqCst);
    let dir = TempDir::new("system-zone");
    let mut engine = Engine::new(
        wd,
        quiet_notifier(),
        AccessList::default(),
        Some(EventLog::open(dir.path()).unwrap()),
        &Options::default(),
    )
    .with_system_zone(switchable);
    // `Engine::new` wrote its first lines in this machine's zone; only the
    // ones after the switch are looked at.
    let first = log_lines(dir.path()).len();
    assert_eq!(engine.time_zone().iana_name(), Some("Asia/Taipei"));
    let status = engine.host_status(T0).time_zone;
    assert_eq!(status.configured, None);
    assert_eq!(status.effective.unwrap().as_str(), "Asia/Taipei");
    assert_eq!(status.utc_offset_secs, 8 * 3600);

    // The system's zone changes; it is picked up within ten minutes.
    SYSTEM.store(1, Ordering::SeqCst);
    clock
        .advance(ZONE_REFRESH - Duration::from_secs(60))
        .unwrap();
    engine.turn();
    assert_eq!(engine.time_zone().iana_name(), Some("Asia/Taipei"));
    clock.advance(Duration::from_secs(61)).unwrap();
    engine.turn();
    assert_eq!(engine.time_zone().iana_name(), Some("America/New_York"));
    let wall = clock.wall_unix_ms();
    assert_eq!(
        engine.host_status(wall).time_zone.utc_offset_secs,
        -5 * 3600
    );
    // The log follows: a network change gives some lines to look at.
    engine.on_network_change();
    engine.turn();

    // It cannot be read: UTC.
    SYSTEM.store(2, Ordering::SeqCst);
    clock.advance(ZONE_REFRESH).unwrap();
    engine.turn();
    assert_eq!(engine.time_zone(), &TimeZone::UTC);
    engine.on_network_change();
    engine.turn();
    let status = engine.host_status(clock.wall_unix_ms()).time_zone;
    assert_eq!(status.effective.unwrap().as_str(), "UTC");
    assert_eq!(status.utc_offset_secs, 0);

    // A configured zone wins over the system's at once.
    SYSTEM.store(0, Ordering::SeqCst);
    let answer = engine
        .request(
            wgal_runtime::driver::CallId(1),
            &user(),
            envelope(
                "h1",
                Request::UpdateHostSettings {
                    settings: HostSettings {
                        workers: None,
                        time_zone: Some(TimeZoneName::new("Europe/Berlin").unwrap()),
                    },
                },
            ),
        )
        .unwrap();
    assert!(accepted(&answer));
    assert_eq!(engine.time_zone().iana_name(), Some("Europe/Berlin"));
    // Cleared again: back to the system's, at once.
    let answer = engine
        .request(
            wgal_runtime::driver::CallId(2),
            &user(),
            envelope(
                "h2",
                Request::UpdateHostSettings {
                    settings: HostSettings::default(),
                },
            ),
        )
        .unwrap();
    assert!(accepted(&answer));
    assert_eq!(engine.time_zone().iana_name(), Some("Asia/Taipei"));

    let lines = log_lines(dir.path());
    let mut offsets: Vec<String> = lines[first..]
        .iter()
        .map(|l| l["time"].as_str().unwrap()[23..].to_owned())
        .collect();
    offsets.dedup();
    // Lines from the turns before New York, if any, are in Taipei's.
    if offsets.first().is_some_and(|o| o == "+08:00") {
        offsets.remove(0);
    }
    // New York in January, UTC, Berlin in January, then Taipei again.
    assert_eq!(
        offsets,
        ["-05:00", "+00:00", "+01:00", "+08:00"],
        "{lines:#?}"
    );
}

/// The worker count is part of every snapshot, and the request needs no
/// administrator.
#[test]
fn snapshots_carry_the_worker_counts() {
    let mut sim = Sim::of(&["alpha", "beta", "gamma"], None);
    sim.settle();
    let host = sim.host();
    assert_eq!(
        (
            host.workers.limit,
            host.workers.suggested,
            host.workers.effective
        ),
        (4, 2, 2)
    );
    assert!(host.workers.running <= 2);
    assert!(accepted(&sim.host_settings(Some(4), None)));
    assert_eq!(sim.host().workers.effective, 4);
    sim.ok(Request::SetTunnelMonitoring {
        tunnel_id: id("gamma"),
        enabled: false,
    });
    let host = sim.host();
    assert_eq!((host.workers.limit, host.workers.effective), (3, 3));
    assert_eq!(host.workers.configured.map(|c| c.get()), Some(4));
    // Automatic again.
    assert!(accepted(&sim.host_settings(None, None)));
    let host = sim.host();
    assert_eq!(host.workers.configured, None);
    assert_eq!(host.workers.effective, host.workers.suggested);
}
