//! The host name messages give when no device name is set, and the time
//! zone read again right before every message (owner decisions of
//! 2026-11-19).
//! What is checked uses the system's zone and host name the test gives;
//! building the loop still reads this machine's once or more before the
//! test's sources take over (only read, never kept or checked). Simulation
//! only.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use jiff::tz::TimeZone;
use wgal_core::MemoryStore;
use wgal_protocol::{ChatId, DeviceName, HostSettings, RecoveryRetry, Request, TimeZoneName};
use wgal_runtime::driver::{Options, UNNAMED_ZONE, ZONE_REFRESH};
use wgal_runtime::ipc::AccessList;
use wgal_runtime::notify::Delivery;
use wgal_testkit::FakeClock;

use support::*;

fn zone(name: &str) -> TimeZone {
    jiff::tz::db().get(name).unwrap()
}

/// Each test has its own switches, as the tests run in parallel.
macro_rules! switches {
    ($zone:ident, $zone_fn:ident, $host:ident, $host_fn:ident) => {
        static $zone: AtomicU8 = AtomicU8::new(0);
        static $host: AtomicU8 = AtomicU8::new(0);
        fn $zone_fn() -> Option<TimeZone> {
            match $zone.load(Ordering::SeqCst) {
                0 => Some(zone("Asia/Taipei")),
                1 => Some(zone("America/New_York")),
                _ => None,
            }
        }
        fn $host_fn() -> Option<String> {
            match $host.load(Ordering::SeqCst) {
                0 => Some("office-pc".to_owned()),
                1 => Some("lab-box".to_owned()),
                2 => Some(" ".repeat(70)),
                _ => None,
            }
        }
    };
}

/// A loop over one managed tunnel, notifications on with a device name
/// (`Lab laptop`), following the system's zone.
fn rig(zone: fn() -> Option<TimeZone>, host: fn() -> Option<String>) -> Sim {
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &["alpha"]);
    manage(&mut wd, "alpha");
    let (notifier, _, _) = notifier(&mut wd);
    let mut sim = Sim::new(
        wd,
        notifier,
        &lab,
        &clock,
        &store,
        AccessList::default(),
        None,
        &Options::default(),
    )
    .with_sources(zone, host);
    // The rig stores UTC; this follows the system's zone.
    sim.ok(Request::UpdateHostSettings {
        settings: HostSettings::default(),
    });
    sim
}

fn device(sim: &mut Sim, name: &str) {
    sim.ok(Request::UpdateNotificationSettings {
        enabled: true,
        device_name: DeviceName::new(name).unwrap(),
        chat_id: Some(ChatId::new(CHAT).unwrap()),
    });
}

/// Sends a test message and returns its text.
fn test_message(sim: &mut Sim) -> String {
    let before = sim.transport.sends();
    sim.ok(Request::SendTestNotification);
    sim.settle();
    assert_eq!(sim.transport.sends(), before + 1);
    sim.transport.texts().pop().unwrap()
}

/// A test message accepted, then the system's zone changes before it is
/// written: it is written in the new zone, not the one of ten minutes ago.
#[test]
fn a_queued_test_message_is_written_in_the_zone_of_now() {
    switches!(ZONE, zone_now, HOST, host_now);
    let mut sim = rig(zone_now, host_now);
    assert!(test_message(&mut sim).contains("Asia/Taipei, UTC+08:00"));
    // Accepted, not yet written: no turn ran since.
    let call = sim.next_call_id();
    let answer = sim
        .engine
        .request(
            call,
            &user(),
            envelope("test-queued", Request::SendTestNotification),
        )
        .unwrap();
    assert!(accepted(&answer));
    ZONE.store(1, Ordering::SeqCst);
    sim.settle();
    let text = sim.transport.texts().pop().unwrap();
    assert!(text.contains("America/New_York, UTC-05:00"), "{text}");
    // And the snapshot gives it too, read when it is asked for.
    ZONE.store(0, Ordering::SeqCst);
    let host = sim.host();
    assert_eq!(host.time_zone.effective.unwrap().as_str(), "Asia/Taipei");
    assert_eq!(host.time_zone.utc_offset_secs, 8 * 3600);
}

/// The first alert of an incident is not sent (`NotSent`); the zone and
/// the host name change while it waits; the next try is written with the
/// new ones. What was handed out keeps its text.
#[test]
fn an_alert_sent_again_is_written_anew_with_the_zone_and_host_of_now() {
    switches!(ZONE, zone_now, HOST, host_now);
    let mut sim = rig(zone_now, host_now);
    device(&mut sim, "");
    sim.run(30);
    sim.transport.answer(Delivery::NotSent);
    sim.lab.kill_for_good("alpha");
    let mut waited = 0;
    while sim.transport.sends() == 0 && waited < 3600 {
        sim.run(10);
        waited += 10;
    }
    let first = sim.transport.texts();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("Asia/Taipei"), "{}", first[0]);
    assert!(first[0].contains("office-pc"), "{}", first[0]);
    ZONE.store(1, Ordering::SeqCst);
    HOST.store(1, Ordering::SeqCst);
    let mut waited = 0;
    while sim.transport.sends() < 2 && waited < 3600 {
        sim.run(10);
        waited += 10;
    }
    let texts = sim.transport.texts();
    assert!(texts.len() >= 2, "{texts:?}");
    assert_eq!(texts[0], first[0], "what was handed out keeps its text");
    assert!(texts[1].contains("America/New_York"), "{}", texts[1]);
    assert!(texts[1].contains("lab-box"), "{}", texts[1]);
}

/// Runs until the transport has been handed `count` messages, at most an
/// hour of simulated time.
fn until_sent(sim: &mut Sim, count: usize) -> Vec<String> {
    let mut waited = 0;
    while sim.transport.sends() < count && waited < 3600 {
        sim.run(10);
        waited += 10;
    }
    let texts = sim.transport.texts();
    assert_eq!(texts.len(), count, "{texts:?}");
    texts
}

/// The other two notices go through the same step: the message saying the
/// limited retries are used up, and the one saying traffic is back, are
/// each written with the zone and host name of the moment they are written.
#[test]
fn the_used_up_and_recovered_messages_are_written_with_the_zone_and_host_of_now() {
    switches!(ZONE, zone_now, HOST, host_now);
    let mut sim = rig(zone_now, host_now);
    device(&mut sim, "");
    sim.ok(Request::SetRecoveryRetry {
        tunnel_id: id("alpha"),
        retry: RecoveryRetry::new(Some(1), 60).unwrap(),
    });
    sim.run(30);
    sim.lab.kill_for_good("alpha");
    let alert = until_sent(&mut sim, 1).remove(0);
    assert!(
        alert.contains("Asia/Taipei") && alert.contains("office-pc"),
        "{alert}"
    );

    ZONE.store(1, Ordering::SeqCst);
    HOST.store(1, Ordering::SeqCst);
    let used_up = until_sent(&mut sim, 2).remove(1);
    assert!(used_up.contains("Retries stopped"), "{used_up}");
    assert!(used_up.contains("America/New_York"), "{used_up}");
    assert!(used_up.contains("Device: lab-box"), "{used_up}");

    ZONE.store(0, Ordering::SeqCst);
    HOST.store(0, Ordering::SeqCst);
    sim.lab.revive("alpha");
    let recovered = until_sent(&mut sim, 3).remove(2);
    assert!(recovered.contains("Recovered"), "{recovered}");
    assert!(recovered.contains("Asia/Taipei"), "{recovered}");
    assert!(recovered.contains("Device: office-pc"), "{recovered}");
}

/// The host name: given in messages and the snapshot while no device name
/// is set; a set device name always wins; none once it cannot be read, or
/// is nothing but spaces, and then the unnamed device's text.
#[test]
fn the_host_name_stands_in_for_a_device_name_that_is_not_set() {
    switches!(ZONE, zone_now, HOST, host_now);
    let mut sim = rig(zone_now, host_now);
    // Set: the device name, whatever the host name.
    assert_eq!(
        sim.snapshot().notification.host_name.unwrap().as_str(),
        "office-pc"
    );
    let text = test_message(&mut sim);
    assert!(text.contains("Device: Lab laptop"), "{text}");
    // Not set, or nothing but spaces: the host name.
    for blank in ["", "   "] {
        device(&mut sim, blank);
        let text = test_message(&mut sim);
        assert!(text.contains("Device: office-pc"), "{blank:?}: {text}");
    }
    // It changes: the next message and snapshot give the new one.
    HOST.store(1, Ordering::SeqCst);
    assert!(test_message(&mut sim).contains("Device: lab-box"));
    assert_eq!(
        sim.snapshot().notification.host_name.unwrap().as_str(),
        "lab-box"
    );
    // Nothing but spaces, or unreadable: none, and the unnamed text.
    for unreadable in [2, 3] {
        HOST.store(unreadable, Ordering::SeqCst);
        assert_eq!(sim.snapshot().notification.host_name, None, "{unreadable}");
        let text = test_message(&mut sim);
        assert!(
            text.contains("Device: (unnamed device)"),
            "{unreadable}: {text}"
        );
    }
}

/// A zone WGAL was given wins over the system's, which changing then
/// changes nothing; with none given the system's is followed at once.
#[test]
fn a_configured_zone_wins_over_the_system_one() {
    switches!(ZONE, zone_now, HOST, host_now);
    let mut sim = rig(zone_now, host_now);
    sim.ok(Request::UpdateHostSettings {
        settings: HostSettings {
            workers: None,
            time_zone: Some(TimeZoneName::new("Europe/Berlin").unwrap()),
        },
    });
    ZONE.store(1, Ordering::SeqCst);
    let text = test_message(&mut sim);
    assert!(text.contains("Europe/Berlin"), "{text}");
    sim.ok(Request::UpdateHostSettings {
        settings: HostSettings::default(),
    });
    let text = test_message(&mut sim);
    assert!(text.contains("America/New_York"), "{text}");
    // The system's cannot be read: UTC.
    ZONE.store(2, Ordering::SeqCst);
    let text = test_message(&mut sim);
    assert!(text.contains("UTC"), "{text}");
}

/// A TZif file of one zone at `hours` east of UTC, as `/etc/localtime` holds
/// when it is a copy rather than a link.
fn tzif(hours: i32) -> Vec<u8> {
    let mut data = b"TZif".to_vec();
    data.push(0); // version 1
    data.extend_from_slice(&[0; 15]);
    // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
    for count in [0u32, 0, 0, 0, 1, 4] {
        data.extend_from_slice(&count.to_be_bytes());
    }
    data.extend_from_slice(&(hours * 3600).to_be_bytes()); // utoff
    data.push(0); // isdst
    data.push(0); // desigidx
    data.extend_from_slice(b"LMT\0");
    data
}

static COPIED: AtomicU8 = AtomicU8::new(8);

/// The system's zone as the loop gets it from a copied file: TZif data,
/// unnamed, whose offset the test changes.
fn copied_zone() -> Option<TimeZone> {
    let hours = i32::from(COPIED.load(Ordering::SeqCst));
    Some(TimeZone::tzif(UNNAMED_ZONE, &tzif(hours)).unwrap())
}

fn office_pc() -> Option<String> {
    Some("office-pc".to_owned())
}

/// A zone read from a file's TZif data has no name: the message gives its
/// offset alone, never the name it carries inside, and the snapshot no
/// effective zone; when the file's contents change, the next message and
/// snapshot follow.
#[test]
fn an_unnamed_zone_is_given_by_its_offset_and_followed_when_it_changes() {
    let mut sim = rig(copied_zone, office_pc);
    let text = test_message(&mut sim);
    assert!(text.contains("Time zone: UTC+08:00\n"), "{text}");
    assert!(
        !text.contains("unnamed") && !text.contains("Local"),
        "{text}"
    );
    let host = sim.host();
    assert_eq!(host.time_zone.effective, None);
    assert_eq!(host.time_zone.utc_offset_secs, 8 * 3600);

    COPIED.store(9, Ordering::SeqCst);
    let text = test_message(&mut sim);
    assert!(text.contains("Time zone: UTC+09:00\n"), "{text}");
    let host = sim.host();
    assert_eq!(host.time_zone.effective, None);
    assert_eq!(host.time_zone.utc_offset_secs, 9 * 3600);
}

static ZONE_READS: AtomicU32 = AtomicU32::new(0);
static HOST_READS: AtomicU32 = AtomicU32::new(0);

fn counted_zone() -> Option<TimeZone> {
    ZONE_READS.fetch_add(1, Ordering::SeqCst);
    Some(zone("Asia/Taipei"))
}

fn counted_host() -> Option<String> {
    HOST_READS.fetch_add(1, Ordering::SeqCst);
    Some("office-pc".to_owned())
}

fn reads() -> (u32, u32) {
    (
        ZONE_READS.load(Ordering::SeqCst),
        HOST_READS.load(Ordering::SeqCst),
    )
}

/// When nothing is written and nothing asked for, the system's zone and
/// the host name are read again every ten minutes, not more often; a zone
/// WGAL was given means the system's is never asked for, before a message,
/// a snapshot or on the ten-minute read, while the host name still is.
#[test]
fn without_messages_both_are_read_every_ten_minutes_and_a_configured_zone_is_never_asked() {
    let mut sim = rig(counted_zone, counted_host);
    device(&mut sim, "Lab laptop");
    sim.settle();
    let before = reads();
    let first = ZONE_REFRESH.as_secs() - 60;
    sim.run(first);
    assert_eq!(reads(), before, "nothing read before ten minutes");
    sim.run(61);
    assert_eq!(
        reads(),
        (before.0 + 1, before.1 + 1),
        "both read at ten minutes"
    );

    sim.ok(Request::UpdateHostSettings {
        settings: HostSettings {
            workers: None,
            time_zone: Some(TimeZoneName::new("Europe/Berlin").unwrap()),
        },
    });
    let (zones, hosts) = reads();
    assert!(test_message(&mut sim).contains("Europe/Berlin"));
    let _ = sim.snapshot();
    sim.run(ZONE_REFRESH.as_secs() + 1);
    let (zones_after, hosts_after) = reads();
    assert_eq!(zones_after, zones, "the system's zone is never asked for");
    assert!(
        hosts_after >= hosts + 3,
        "the host name still is: {hosts} -> {hosts_after}"
    );
}

static WAIT_ZONE_READS: AtomicU32 = AtomicU32::new(0);
static WAIT_HOST_READS: AtomicU32 = AtomicU32::new(0);

fn wait_zone() -> Option<TimeZone> {
    WAIT_ZONE_READS.fetch_add(1, Ordering::SeqCst);
    Some(zone("Asia/Taipei"))
}

fn wait_host() -> Option<String> {
    WAIT_HOST_READS.fetch_add(1, Ordering::SeqCst);
    Some("office-pc".to_owned())
}

fn wait_reads() -> (u32, u32) {
    (
        WAIT_ZONE_READS.load(Ordering::SeqCst),
        WAIT_HOST_READS.load(Ordering::SeqCst),
    )
}

/// While an alert waits to be sent again, nothing is read for it; both are
/// read once, right before the message that goes out when the wait is over
/// (no ten-minute read falls in between: each read starts that wait anew).
#[test]
fn nothing_is_read_while_a_message_waits_and_once_when_it_is_written() {
    let mut sim = rig(wait_zone, wait_host);
    device(&mut sim, "");
    sim.run(30);
    sim.transport.answer(Delivery::NotSent);
    sim.lab.kill_for_good("alpha");
    until_sent(&mut sim, 1);
    let after_first = wait_reads();
    sim.run(20);
    assert_eq!(wait_reads(), after_first, "nothing read while it waits");
    until_sent(&mut sim, 2);
    assert_eq!(
        wait_reads(),
        (after_first.0 + 1, after_first.1 + 1),
        "read once, for the message written"
    );
}
