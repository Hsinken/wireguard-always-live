//! The JSONL event log, version 2 (plan §9.1; owner's requests 1, 9 and 10
//! of 2026-11-03): what the loop writes, `seq` across restarts and
//! rotation, times in a fixed test zone, rotation by size and age, 72-hour
//! retention and the safety caps, reading pages back with labels mapped to
//! tunnels, what is never written, write failures and symbolic links. Real
//! files in fresh temporary directories; the watchdog runs on a fake clock
//! and a fake platform. Simulation only.

#![allow(clippy::unwrap_used)]

mod support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use jiff::tz::TimeZone;
use serde_json::Value;
use wgal_core::MemoryStore;
use wgal_protocol::{
    ChatId, DeviceName, ErrorCode, ErrorReason, EventFilter, Light, LogEvent, MAX_EVENT_PAGE,
    Outcome, Request, Response, StorageHealth,
};
use wgal_runtime::driver::Options;
use wgal_runtime::events::{Diag, EVENTS_FILE, EventLog, KEPT_FILES, Lines, TunnelLabel};
use wgal_runtime::ipc::AccessList;
use wgal_testkit::FakeClock;

use support::*;

const HOUR: i64 = 3_600_000;

fn label(sim: &Sim, name: &str) -> String {
    let stored = sim.engine.watchdog().log_label(&id(name));
    assert!(stored.is_some(), "every registered tunnel has a label");
    TunnelLabel::stored(stored).as_str().to_owned()
}

fn taipei() -> TimeZone {
    jiff::tz::db().get("Asia/Taipei").unwrap()
}

fn older(dir: &Path, n: usize) -> std::path::PathBuf {
    dir.join(format!("events.{n}.jsonl"))
}

/// Sets a file's modification time to `unix_ms`.
fn touch(path: &Path, unix_ms: i64) {
    let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(unix_ms).unwrap());
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

fn seqs(lines: &[Value]) -> Vec<u64> {
    lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect()
}

fn page(
    sim: &mut Sim,
    tunnel: Option<&str>,
    before_seq: Option<u64>,
) -> (Vec<wgal_protocol::EventEntry>, bool) {
    let filter = match tunnel {
        Some(name) => EventFilter::Tunnel {
            tunnel_id: id(name),
        },
        None => EventFilter::All,
    };
    let answer = sim.ok(Request::ReadEventLog { filter, before_seq });
    match answer.outcome {
        Outcome::Ok(Response::EventLog { entries, more }) => (entries.into_vec(), more),
        other => panic!("not a page: {other:?}"),
    }
}

#[test]
fn the_loop_logs_its_start_each_tunnel_and_a_fault() {
    let dir = TempDir::new("fault");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    sim.run(30);
    sim.lab.kill_for_good("alpha");
    sim.run(120);
    let lines = log_lines(dir.path());
    assert_eq!(lines[0]["event"]["kind"], "watchdog_started");
    assert!(lines[0].get("tunnel").is_none());
    let started = diags_named(&lines, "started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["diag"]["tunnels"], 1);
    assert_eq!(started[0]["diag"]["storage"], "ok");
    let tunnel = label(&sim, "alpha");
    let first = diags_named(&lines, "tunnel");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["tunnel"], tunnel.as_str());
    assert_eq!(first[0]["diag"]["light"], "green");
    assert_eq!(first[0]["diag"]["os_state"], "connected");

    // Green, then failures being confirmed, then a confirmed fault: in the
    // diag lines and as the user's events.
    let lights: Vec<(&str, &str)> = diags_named(&lines, "light")
        .iter()
        .map(|l| {
            (
                l["diag"]["light"].as_str().unwrap(),
                l["diag"]["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lights,
        [("yellow", "confirming"), ("red", "fault_confirmed")],
        "{lines:#?}"
    );
    let changed: Vec<(&str, &str)> = events_named(&lines, "light_changed")
        .iter()
        .map(|l| {
            assert_eq!(l["tunnel"], tunnel.as_str());
            (
                l["event"]["light"].as_str().unwrap(),
                l["event"]["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(changed, lights);
    let opened = diags_named(&lines, "incident_opened");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["tunnel"], tunnel.as_str());
    assert_eq!(opened[0]["diag"]["incident"], "inc-000001");
    assert_eq!(opened[0]["diag"]["first_recovery"], "not_attempted");
    assert_eq!(opened[0]["diag"]["notification"], "not_eligible");
    // Every line has the next seq and a time in the configured zone (UTC
    // in the rig).
    let numbers = seqs(&lines);
    assert_eq!(
        numbers,
        (1..=u64::try_from(lines.len()).unwrap()).collect::<Vec<_>>()
    );
    for line in &lines {
        let time = line["time"].as_str().unwrap();
        assert!(time.starts_with("2026-01-01T00:0"), "{time}");
        assert!(time.ends_with("+00:00") && time.len() == 29, "{time}");
    }
}

#[test]
fn host_changes_are_logged() {
    let dir = TempDir::new("host");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    sim.ok(Request::SetMonitoringPaused { paused: true });
    sim.settle();
    sim.ok(Request::SetMonitoringPaused { paused: false });
    sim.settle();
    sim.store.set_failing(true);
    // A change that must be stored fails, and so does storage.
    let refused = sim
        .call(
            &admin(),
            Request::SetStartWhenStoppedExternally {
                tunnel_id: id("alpha"),
                enabled: false,
            },
        )
        .unwrap();
    assert_eq!(
        refusal_of(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    sim.run(20);
    sim.store.set_failing(false);
    sim.run(20);
    let lines = log_lines(dir.path());
    let paused: Vec<bool> = diags_named(&lines, "monitoring_paused")
        .iter()
        .map(|l| l["diag"]["paused"].as_bool().unwrap())
        .collect();
    assert_eq!(paused, [true, false]);
    let storage: Vec<&str> = diags_named(&lines, "storage")
        .iter()
        .map(|l| l["diag"]["storage"].as_str().unwrap())
        .collect();
    assert_eq!(storage, ["failed", "ok"], "{lines:#?}");
    let kinds = event_kinds(&lines);
    let host: Vec<&str> = kinds
        .iter()
        .map(String::as_str)
        .filter(|k| k.starts_with("monitoring_") || k.starts_with("storage_"))
        .collect();
    assert_eq!(
        host,
        [
            "monitoring_paused",
            "monitoring_resumed",
            "storage_failed",
            "storage_recovered"
        ]
    );
    // Host-wide: no tunnel on them.
    for line in events_named(&lines, "monitoring_paused") {
        assert!(line.get("tunnel").is_none());
    }
    assert!(
        events_named(&lines, "light_changed")
            .iter()
            .any(|l| l["event"]["reason"] == "monitoring_off")
    );
}

/// User operations are logged when accepted, and only then; the switches
/// and settings are logged when they change.
#[test]
fn user_operations_and_switches_are_logged() {
    let dir = TempDir::new("user");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    sim.settle();
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    sim.run(5);
    sim.ok(Request::Connect {
        tunnel_id: id("alpha"),
    });
    sim.run(60);
    sim.ok(Request::Reconnect {
        tunnel_id: id("alpha"),
    });
    sim.run(60);
    // Refused (unknown tunnel): nothing is logged for it.
    let refused = sim
        .call(
            &user(),
            Request::Connect {
                tunnel_id: id("nobody"),
            },
        )
        .unwrap();
    assert_eq!(
        refusal_of(&refused),
        Some((
            ErrorCode::UnknownTunnel,
            Some(ErrorReason::TunnelNotRegistered)
        ))
    );
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    // The same again changes nothing and is not logged again.
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.ok(Request::SetTunnelNotifications {
        tunnel_id: id("alpha"),
        enabled: false,
    });
    sim.ok(settings("alpha", Some(icmp(9))));
    assert!(accepted(&sim.host_settings(Some(3), Some("UTC"))));
    // Unchanged host settings: no line.
    assert!(accepted(&sim.host_settings(Some(3), Some("UTC"))));
    sim.settle();
    let lines = log_lines(dir.path());
    let tunnel = label(&sim, "alpha");
    let user: Vec<&Value> = lines
        .iter()
        .filter(|l| {
            l["event"]["kind"].as_str().is_some_and(|k| {
                k.starts_with("user_") || k.ends_with("_changed") && k != "light_changed"
            })
        })
        .collect();
    let kinds: Vec<&str> = user
        .iter()
        .map(|l| l["event"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "user_disconnect",
            "user_connect",
            "user_reconnect",
            "auto_recovery_changed",
            "tunnel_notifications_changed",
            // A new recovery target turns auto recovery off.
            "auto_recovery_changed",
            "settings_changed",
            "settings_changed",
        ],
        "{lines:#?}"
    );
    assert_eq!(user[3]["event"]["enabled"], true);
    assert_eq!(user[4]["event"]["enabled"], false);
    assert_eq!(user[5]["event"]["enabled"], false);
    for line in &user[..7] {
        assert_eq!(line["tunnel"], tunnel.as_str(), "{line}");
    }
    // The host settings line names no tunnel.
    assert!(user[7].get("tunnel").is_none());
    // Each user operation comes before what it caused.
    let disconnect = lines
        .iter()
        .position(|l| l["event"]["kind"] == "user_disconnect")
        .unwrap();
    let gray = lines
        .iter()
        .position(|l| l["event"]["reason"] == "disconnected_by_user")
        .unwrap();
    assert!(disconnect < gray);
}

#[test]
fn a_new_tunnel_is_logged_as_registered() {
    let dir = TempDir::new("registered");
    let lab = Lab::new(&["alpha", "beta"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = prepared(&lab, &clock, &store, &["alpha"]);
    let mut sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        Some(EventLog::open(dir.path()).unwrap()),
        &Options::default(),
    );
    sim.ok(Request::DiscoverTunnels);
    sim.ok(Request::RegisterTunnel {
        tunnel_id: id("beta"),
    });
    sim.settle();
    let lines = log_lines(dir.path());
    let registered = events_named(&lines, "tunnel_registered");
    assert_eq!(registered.len(), 1, "alpha was there before: {lines:#?}");
    assert_eq!(registered[0]["tunnel"], label(&sim, "beta").as_str());
    assert_eq!(diags_named(&lines, "tunnel").len(), 2);
}

#[test]
fn times_follow_the_configured_zone() {
    let dir = TempDir::new("zone");
    let mut log = EventLog::open(dir.path()).unwrap();
    log.event(T0, None, &LogEvent::WatchdogStarted);
    log.set_time_zone(taipei());
    log.event(T0 + 1, None, &LogEvent::WatchdogStopped);
    let lines = log_lines(dir.path());
    assert_eq!(lines[0]["time"], "2026-01-01T00:00:00.000+00:00");
    assert_eq!(lines[1]["time"], "2026-01-01T08:00:00.001+08:00");

    // Through the loop: the setting changes the zone of the next lines and
    // the snapshot reports it.
    let dir = TempDir::new("zone-loop");
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(dir.path()).unwrap()));
    assert!(accepted(&sim.host_settings(None, Some("Asia/Taipei"))));
    assert_eq!(sim.engine.time_zone().iana_name(), Some("Asia/Taipei"));
    sim.ok(Request::SetMonitoringPaused { paused: true });
    sim.settle();
    let lines = log_lines(dir.path());
    let before = &lines[0];
    assert!(before["time"].as_str().unwrap().ends_with("+00:00"));
    let changed = events_named(&lines, "settings_changed");
    assert_eq!(changed.len(), 1);
    assert!(changed[0]["time"].as_str().unwrap().ends_with("+08:00"));
    let paused = events_named(&lines, "monitoring_paused");
    assert!(
        paused[0]["time"]
            .as_str()
            .unwrap()
            .starts_with("2026-01-01T08:0")
    );
}

#[test]
fn seq_continues_after_a_restart_and_across_rotation() {
    let dir = TempDir::new("seq");
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(400);
    assert_eq!(log.next_seq(), 1);
    for i in 0..20 {
        log.diag(T0 + i, None, &Diag::Stopped { in_time: true });
    }
    assert!(older(dir.path(), 2).exists(), "it rotated");
    drop(log);
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(400);
    assert_eq!(log.next_seq(), 21);
    log.event(T0 + 100, None, &LogEvent::WatchdogStarted);
    let lines = log_lines(dir.path());
    assert_eq!(lines.last().unwrap()["seq"], 21);
    let numbers = seqs(&lines);
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(numbers, sorted, "increasing by one, no repeats");
    assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1));

    // Right after a rotation the current file is gone: the rotated ones
    // still give the highest seq.
    drop(log);
    fs::rename(dir.path().join(EVENTS_FILE), older(dir.path(), KEPT_FILES)).unwrap();
    assert_eq!(EventLog::open(dir.path()).unwrap().next_seq(), 22);

    // A torn last line does not hide the ones before it.
    let mut torn = OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.path().join(EVENTS_FILE))
        .unwrap();
    torn.write_all(br#"{"seq":21,"time":"2026-01-"#).unwrap();
    drop(torn);
    assert_eq!(EventLog::open(dir.path()).unwrap().next_seq(), 22);
}

#[test]
fn files_rotate_at_the_limit_and_seven_older_ones_are_kept() {
    let dir = TempDir::new("rotate");
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(1_000);
    let n = 600_i64;
    for i in 0..n {
        log.diag(T0 + i, None, &Diag::MonitoringPaused { paused: i % 2 == 0 });
    }
    assert_eq!(log.failures(), 0);
    let current = dir.path().join(EVENTS_FILE);
    assert!(current.exists());
    for k in 1..=KEPT_FILES {
        let len = fs::metadata(older(dir.path(), k)).unwrap().len();
        assert!(len <= 1_000 && len > 850, "{k}: {len} bytes");
    }
    assert!(!older(dir.path(), KEPT_FILES + 1).exists());
    assert!(fs::metadata(&current).unwrap().len() <= 1_000);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), KEPT_FILES + 1);

    // The kept lines are the newest ones, in order, the last one last.
    let lines = log_lines(dir.path());
    let times: Vec<&str> = lines.iter().map(|l| l["time"].as_str().unwrap()).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted);
    assert_eq!(*times.last().unwrap(), "2026-01-01T00:00:00.599+00:00");
    assert_eq!(*seqs(&lines).last().unwrap(), 600);
    assert!(lines.len() < 600 && lines.len() > 50);
}

#[test]
fn the_current_file_rotates_when_its_first_line_is_a_day_old() {
    let dir = TempDir::new("age");
    let mut log = EventLog::open(dir.path()).unwrap();
    log.event(T0, None, &LogEvent::WatchdogStarted);
    log.event(T0 + 23 * HOUR, None, &LogEvent::MonitoringPaused);
    assert!(!older(dir.path(), 1).exists());
    log.event(T0 + 24 * HOUR, None, &LogEvent::MonitoringResumed);
    let rotated = fs::read_to_string(older(dir.path(), 1)).unwrap();
    assert_eq!(rotated.lines().count(), 2);
    let current = fs::read_to_string(dir.path().join(EVENTS_FILE)).unwrap();
    assert!(current.contains("monitoring_resumed"));
    assert_eq!(current.lines().count(), 1);

    // After a restart the age is read from the file's first line.
    drop(log);
    let mut log = EventLog::open(dir.path()).unwrap();
    log.event(T0 + 47 * HOUR, None, &LogEvent::WatchdogStarted);
    assert!(!older(dir.path(), 2).exists());
    log.event(T0 + 48 * HOUR, None, &LogEvent::WatchdogStopped);
    assert!(older(dir.path(), 2).exists());

    // A first line that cannot be read has no age: the file is rotated.
    drop(log);
    fs::write(dir.path().join(EVENTS_FILE), b"not json\n").unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    log.event(T0 + 48 * HOUR + 1, None, &LogEvent::WatchdogStarted);
    assert_eq!(
        fs::read_to_string(older(dir.path(), 1)).unwrap(),
        "not json\n"
    );
}

/// Owner's request 10: about 72 hours are kept, and old files are deleted
/// when a line is written, never on a timer.
#[test]
fn rotated_files_older_than_72_hours_are_deleted_when_a_line_is_written() {
    let dir = TempDir::new("retention");
    let now = T0 + 100 * HOUR;
    for n in 1..=4 {
        fs::write(older(dir.path(), n), b"{\"seq\":1}\n").unwrap();
    }
    touch(&older(dir.path(), 1), now - 10 * HOUR);
    touch(&older(dir.path(), 2), now - 50 * HOUR);
    touch(&older(dir.path(), 3), now - 73 * HOUR);
    touch(&older(dir.path(), 4), now - 90 * HOUR);
    let mut log = EventLog::open(dir.path()).unwrap();
    // Opening deletes nothing.
    assert!(older(dir.path(), 4).exists());
    log.event(now, None, &LogEvent::WatchdogStarted);
    assert!(older(dir.path(), 1).exists());
    assert!(older(dir.path(), 2).exists());
    assert!(!older(dir.path(), 3).exists());
    assert!(!older(dir.path(), 4).exists());

    // Within the hour nothing is checked again, even if a file turned old.
    touch(&older(dir.path(), 2), now - 80 * HOUR);
    log.event(now + HOUR / 2, None, &LogEvent::MonitoringPaused);
    assert!(older(dir.path(), 2).exists());
    // An hour later it is.
    log.event(now + HOUR, None, &LogEvent::MonitoringResumed);
    assert!(!older(dir.path(), 2).exists());
    assert!(older(dir.path(), 1).exists());
    assert!(dir.path().join(EVENTS_FILE).exists());
}

#[test]
fn a_rotation_also_deletes_what_is_too_old() {
    let dir = TempDir::new("retention-rotate");
    let now = T0 + 100 * HOUR;
    fs::write(older(dir.path(), 1), b"{\"seq\":1}\n").unwrap();
    touch(&older(dir.path(), 1), now - 80 * HOUR);
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(150);
    log.event(now - 2 * HOUR, None, &LogEvent::WatchdogStarted);
    assert!(!older(dir.path(), 1).exists(), "the first line checks");
    fs::write(older(dir.path(), 1), b"{\"seq\":1}\n").unwrap();
    touch(&older(dir.path(), 1), now - 80 * HOUR);
    // Within the hour, but the line rotates the current file.
    log.event(now - 2 * HOUR + 1, None, &LogEvent::MonitoringPaused);
    assert!(older(dir.path(), 1).exists());
    assert!(
        !older(dir.path(), 2).exists(),
        "the old file, moved up, is gone"
    );
}

#[test]
fn the_safety_cap_on_bytes_drops_the_oldest_files() {
    let dir = TempDir::new("cap");
    for n in 1..=3 {
        fs::write(older(dir.path(), n), vec![b'x'; 3_000]).unwrap();
    }
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(1_000);
    // Seven files of 1000 bytes: 7000 bytes in all.
    log.event(T0, None, &LogEvent::WatchdogStarted);
    assert!(older(dir.path(), 1).exists());
    assert!(older(dir.path(), 2).exists());
    assert!(!older(dir.path(), 3).exists());
}

#[test]
fn a_rotated_line_never_splits_across_files() {
    let dir = TempDir::new("whole-lines");
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(300);
    for i in 0..50 {
        log.diag(T0 + i, None, &Diag::Stopped { in_time: true });
    }
    for entry in fs::read_dir(dir.path()).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(text.ends_with('\n'));
        for line in text.lines() {
            let value: Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["diag"]["event"], "stopped");
        }
    }
}

/// Reading pages back: newest first, at most `MAX_EVENT_PAGE`, across
/// rotated files, diag and broken lines skipped, labels mapped or not.
#[test]
fn pages_are_read_newest_first_across_files() {
    let dir = TempDir::new("pages");
    let mut log = EventLog::open(dir.path()).unwrap().with_max_bytes(4_000);
    let alpha = TunnelLabel::stored(Some("t-aaaaaaaa"));
    let gone = TunnelLabel::stored(Some("t-99999999"));
    for i in 0..130_i64 {
        let tunnel = if i % 2 == 0 { &alpha } else { &gone };
        log.event(
            T0 + i * 1000,
            Some(tunnel),
            &LogEvent::ProbeFailed {
                streak: u32::try_from(i).unwrap(),
                needed: 3,
            },
        );
        log.diag(
            T0 + i * 1000,
            Some(tunnel),
            &Diag::Stopped { in_time: true },
        );
        log.event(T0 + i * 1000, None, &LogEvent::WatchdogStarted);
    }
    // Lines the reader must skip: not JSON, an unknown kind, no seq.
    let mut current = OpenOptions::new()
        .append(true)
        .open(dir.path().join(EVENTS_FILE))
        .unwrap();
    current
        .write_all(
            b"garbage\n{\"seq\":999,\"time\":\"2026-01-01T00:00:00.000+00:00\",\"event\":{\"kind\":\"execute\"}}\n{\"time\":\"x\",\"event\":{\"kind\":\"watchdog_started\"}}\n",
        )
        .unwrap();
    drop(current);
    assert!(older(dir.path(), 3).exists(), "spread over files");
    let map = |label: &str| (label == "t-aaaaaaaa").then(|| id("alpha"));

    // Everything: 260 events (diag lines skipped).
    let (first, more) = log.read_page(None, Lines::All, map);
    assert_eq!(first.len(), MAX_EVENT_PAGE);
    assert!(more);
    assert_eq!(first[0].seq, 390);
    assert!(first.windows(2).all(|w| w[0].seq > w[1].seq));
    assert!(first[0].event == LogEvent::WatchdogStarted && first[0].tunnel_id.is_none());
    assert!(
        !first[0].removed_tunnel,
        "a host line is not a removed tunnel's"
    );
    // Newest first: 390 started, 388 the gone tunnel's (129 is odd), 387
    // started, 385 alpha's.
    assert_eq!(first[1].seq, 388);
    // A label no tunnel has now: no tunnel ID, marked as a removed tunnel's.
    assert!(first[1].tunnel_id.is_none());
    assert!(first[1].removed_tunnel);
    let alpha_line = &first[3];
    assert_eq!(alpha_line.seq, 385);
    assert_eq!(alpha_line.tunnel_id, Some(id("alpha")));
    assert!(!alpha_line.removed_tunnel);
    assert_eq!(
        alpha_line.event,
        LogEvent::ProbeFailed {
            streak: 128,
            needed: 3
        }
    );
    assert_eq!(alpha_line.at_unix_ms, T0 + 128_000);

    let mut all = first;
    let mut before = all.last().map(|e| e.seq);
    loop {
        let (next, more) = log.read_page(before, Lines::All, map);
        assert!(next.iter().all(|e| e.seq < before.unwrap()));
        all.extend(next);
        before = all.last().map(|e| e.seq);
        if !more {
            break;
        }
    }
    // What was rotated away is gone; the rest is all there, once.
    let kept = all.len();
    assert!(kept > 150 && kept <= 260, "{kept}");
    assert!(all.windows(2).all(|w| w[0].seq > w[1].seq));

    // Only alpha's: the same ones.
    let (alpha, more) = log.read_page(None, Lines::Tunnel("t-aaaaaaaa"), map);
    let alphas: Vec<u64> = all
        .iter()
        .filter(|e| e.tunnel_id == Some(id("alpha")))
        .map(|e| e.seq)
        .collect();
    assert!(alphas.len() < MAX_EVENT_PAGE);
    assert_eq!(alpha.iter().map(|e| e.seq).collect::<Vec<_>>(), alphas);
    assert!(!more);

    // Only the watchdog's own starts and stops, none of any tunnel's.
    let (own, _) = log.read_page(None, Lines::Watchdog, map);
    let starts: Vec<u64> = all
        .iter()
        .filter(|e| e.event == LogEvent::WatchdogStarted && e.tunnel_id.is_none())
        .map(|e| e.seq)
        .collect();
    assert!(!starts.is_empty());
    assert_eq!(
        own.iter().map(|e| e.seq).collect::<Vec<_>>(),
        starts
            .iter()
            .copied()
            .take(MAX_EVENT_PAGE)
            .collect::<Vec<_>>()
    );
    assert!(own.iter().all(|e| !e.removed_tunnel));

    // System lines: every line about no tunnel, the watchdog's starts
    // included; none of a registered tunnel's, nor of one no longer
    // registered (labelled, so not about the system).
    let (system, _) = log.read_page(None, Lines::System, map);
    let unlabelled: Vec<u64> = all
        .iter()
        .filter(|e| e.tunnel_id.is_none() && !e.removed_tunnel)
        .map(|e| e.seq)
        .take(MAX_EVENT_PAGE)
        .collect();
    assert!(!unlabelled.is_empty());
    assert_eq!(system.iter().map(|e| e.seq).collect::<Vec<_>>(), unlabelled);
    assert!(all.iter().any(|e| e.removed_tunnel), "the case is there");

    let (none, more) = log.read_page(Some(1), Lines::All, map);
    assert!(none.is_empty() && !more);
}

/// The lines about no tunnel, newest first.
fn system_page(sim: &mut Sim) -> Vec<wgal_protocol::EventEntry> {
    let answer = sim.ok(Request::ReadEventLog {
        filter: EventFilter::System,
        before_seq: None,
    });
    match answer.outcome {
        Outcome::Ok(Response::EventLog { entries, .. }) => entries.into_vec(),
        other => panic!("not a page: {other:?}"),
    }
}

/// Codex P2-N9: a change of the notification settings is a line about no
/// tunnel, so 系統事件 lists it; saving the same settings again is none.
/// The line says nothing of what they are.
#[test]
fn a_notification_settings_change_is_one_system_line() {
    let dir = TempDir::new("notify-line");
    let log = EventLog::open(dir.path()).unwrap();
    let mut sim = Sim::of(&["alpha"], Some(log));
    sim.settle();
    let changes = |sim: &mut Sim| {
        system_page(sim)
            .iter()
            .filter(|e| e.event == LogEvent::SettingsChanged)
            .count()
    };
    let before = changes(&mut sim);
    let settings = Request::UpdateNotificationSettings {
        enabled: true,
        device_name: DeviceName::new("Desk").unwrap(),
        chat_id: Some(ChatId::new("-100123").unwrap()),
    };
    sim.ok(settings.clone());
    sim.run(1);
    assert_eq!(changes(&mut sim), before + 1);
    sim.ok(settings);
    sim.run(1);
    assert_eq!(changes(&mut sim), before + 1, "the same settings again");
    let text = fs::read_to_string(dir.path().join(EVENTS_FILE)).unwrap();
    assert!(!text.contains("Desk") && !text.contains("-100123"));
}

#[test]
fn the_loop_reads_the_log_with_labels_mapped_to_registered_tunnels() {
    let dir = TempDir::new("read-loop");
    // A line from a tunnel registered before and no longer.
    let mut log = EventLog::open(dir.path()).unwrap();
    log.event(
        T0 - HOUR,
        Some(&TunnelLabel::stored(Some("t-0badc0de"))),
        &LogEvent::UserConnect,
    );
    let mut sim = Sim::of(&["alpha", "beta"], Some(log));
    sim.settle();
    sim.ok(Request::Disconnect {
        tunnel_id: id("alpha"),
    });
    sim.ok(Request::Disconnect {
        tunnel_id: id("beta"),
    });
    sim.run(10);

    let (all, more) = page(&mut sim, None, None);
    assert!(!more);
    let disconnects: Vec<_> = all
        .iter()
        .filter(|e| e.event == LogEvent::UserDisconnect)
        .map(|e| e.tunnel_id.clone())
        .collect();
    assert_eq!(disconnects, [Some(id("beta")), Some(id("alpha"))]);
    let old = all.last().unwrap();
    assert_eq!(old.event, LogEvent::UserConnect);
    assert_eq!(old.tunnel_id, None, "an unknown label maps to no tunnel");
    assert!(old.removed_tunnel);
    assert!(
        all.iter()
            .any(|e| e.event == LogEvent::WatchdogStarted && e.tunnel_id.is_none())
    );

    let (alpha, _) = page(&mut sim, Some("alpha"), None);
    assert!(!alpha.is_empty());
    assert!(alpha.iter().all(|e| e.tunnel_id == Some(id("alpha"))));
    assert!(alpha.iter().any(|e| e.event == LogEvent::UserDisconnect));
    // A tunnel that is not registered has no lines.
    let (nobody, more) = page(&mut sim, Some("nobody"), None);
    assert!(nobody.is_empty() && !more);
    // The watchdog's own lines: its start, and nothing about a tunnel.
    let answer = sim.ok(Request::ReadEventLog {
        filter: EventFilter::Watchdog,
        before_seq: None,
    });
    let Outcome::Ok(Response::EventLog { entries: own, .. }) = answer.outcome else {
        panic!("not a page");
    };
    assert!(own.iter().any(|e| e.event == LogEvent::WatchdogStarted));
    assert!(own.iter().all(|e| {
        e.tunnel_id.is_none()
            && matches!(
                e.event,
                LogEvent::WatchdogStarted | LogEvent::WatchdogStopped
            )
    }));
    // System lines: the watchdog's start and the rest about no tunnel; not
    // the old line of the tunnel no longer registered.
    let answer = sim.ok(Request::ReadEventLog {
        filter: EventFilter::System,
        before_seq: None,
    });
    let Outcome::Ok(Response::EventLog {
        entries: system, ..
    }) = answer.outcome
    else {
        panic!("not a page");
    };
    assert!(system.iter().any(|e| e.event == LogEvent::WatchdogStarted));
    assert!(
        system
            .iter()
            .all(|e| e.tunnel_id.is_none() && !e.removed_tunnel)
    );
    assert_eq!(
        system.iter().map(|e| e.seq).collect::<Vec<_>>(),
        all.iter()
            .filter(|e| e.tunnel_id.is_none() && !e.removed_tunnel)
            .map(|e| e.seq)
            .collect::<Vec<_>>()
    );
    // Paging with before_seq.
    let newest = all[0].seq;
    let (older_page, _) = page(&mut sim, None, Some(newest));
    assert_eq!(older_page[0].seq, all[1].seq);
    // A plain user may read it.
    let answer = sim
        .call(
            &user(),
            Request::ReadEventLog {
                filter: EventFilter::All,
                before_seq: None,
            },
        )
        .unwrap();
    assert!(error_code(&answer).is_none());
}

#[test]
fn without_an_event_log_reading_it_is_refused() {
    let mut sim = Sim::of(&["alpha"], None);
    let answer = sim
        .call(
            &user(),
            Request::ReadEventLog {
                filter: EventFilter::All,
                before_seq: None,
            },
        )
        .unwrap();
    assert_eq!(
        refusal_of(&answer),
        Some((ErrorCode::Internal, Some(ErrorReason::EventLogUnavailable)))
    );
}

/// Everything that must never reach the log, in a run that has all of it:
/// a binding (which is also the tunnel ID), its display name, a recovery
/// target and an observe target, a chat, a token, a device name, request
/// IDs, adapter error messages and a sent alert.
#[test]
fn no_address_binding_name_chat_token_or_os_text_is_written() {
    let dir = TempDir::new("secrets");
    let name = "lab-tunnel-7";
    let lab = Lab::new(&[name]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let mut wd = prepared(&lab, &clock, &store, &[name]);
    manage(&mut wd, name);
    let (notifier, _, _) = notifier(&mut wd);
    let mut sim = Sim::new(
        wd,
        notifier,
        &lab,
        &clock,
        &store,
        AccessList::default(),
        Some(EventLog::open(dir.path()).unwrap()),
        &Options::default(),
    );
    sim.ok(Request::UpdateMonitorSettings {
        tunnel_id: id(name),
        settings: wgal_protocol::MonitorSettings {
            recovery_target: Some(icmp(1)),
            observe_targets: wgal_protocol::BoundedVec::try_from(vec![icmp(77)]).unwrap(),
            probe: wgal_protocol::ProbeTiming::DEFAULT,
        },
    });
    sim.run(40);
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id(name),
        enabled: true,
    });
    sim.ok(Request::SendTestNotification);
    lab.kill_for_good(name);
    // A failed start leaves an adapter error message behind.
    lab.fake().fail_next(
        &binding(name),
        wgal_testkit::Operation::Start,
        wgal_core::AdapterError::new(
            wgal_protocol::AdapterErrorKind::PermissionDenied,
            "service lab-tunnel-7 at 192.0.2.1 refused: access denied",
        ),
    );
    sim.run(900);
    assert!(sim.transport.sends() >= 2, "a test and an alert were sent");

    let text = raw_log(dir.path());
    for forbidden in [
        name,
        "Tunnel ",
        "192.0.2",
        "198.51.100",
        CHAT,
        TOKEN,
        "123456789",
        "Lab laptop",
        "req-",
        "settings-1",
        "access denied",
        "did not",
        "traffic through",
    ] {
        assert!(!text.contains(forbidden), "{forbidden:?} is in the log");
    }
    let lines = log_lines(dir.path());
    assert!(
        diags_named(&lines, "notification")
            .iter()
            .any(|l| l["diag"]["notification"] == "sent")
    );
    assert_eq!(events_named(&lines, "notification_sent").len(), 1);
    assert!(!events_named(&lines, "test_notification").is_empty());
    assert!(
        diags_named(&lines, "light")
            .iter()
            .any(|l| l["diag"]["fault"] == "permission_denied")
    );
    // Only these keys ever appear.
    let top = ["seq", "time", "tunnel", "event", "diag"];
    let in_event = [
        "kind",
        "streak",
        "needed",
        "after",
        "incident_id",
        "attempt",
        "next_retry_unix_ms",
        "error",
        "light",
        "reason",
        "enabled",
        "failure",
        "state",
        "count",
        "notice",
        "retries",
    ];
    let in_diag = [
        "event",
        "storage",
        "monitoring_paused",
        "tunnels",
        "in_time",
        "light",
        "reason",
        "fault",
        "os_state",
        "recovery",
        "takeover",
        "incident",
        "first_recovery",
        "notification",
        "phase",
        "paused",
        "enabled",
        "token",
        "state",
        "failure",
        "effective",
    ];
    for line in &lines {
        for key in line.as_object().unwrap().keys() {
            assert!(top.contains(&key.as_str()), "{key} in {line}");
        }
        for (part, allowed) in [("event", &in_event[..]), ("diag", &in_diag[..])] {
            if let Some(object) = line.get(part) {
                for key in object.as_object().unwrap().keys() {
                    assert!(allowed.contains(&key.as_str()), "{key} in {line}");
                }
            }
        }
        if let Some(tunnel) = line.get("tunnel") {
            let tunnel = tunnel.as_str().unwrap();
            assert!(tunnel.starts_with("t-") && tunnel.len() == 10, "{tunnel}");
        }
    }
}

#[test]
fn a_failing_log_never_stops_monitoring_or_recovery() {
    let parent = TempDir::new("failing");
    let dir = parent.path().join("log");
    fs::create_dir(&dir).unwrap();
    let mut sim = Sim::of(&["alpha"], Some(EventLog::open(&dir).unwrap()));
    sim.ok(Request::SetAutoRecovery {
        tunnel_id: id("alpha"),
        enabled: true,
    });
    sim.run(20);
    let before = log_lines(&dir).len();
    fs::remove_dir_all(&dir).unwrap();

    sim.lab.kill_until_restart("alpha");
    sim.run(200);
    assert_eq!(sim.lab.stops("alpha"), 1);
    assert_eq!(sim.lab.starts("alpha"), 1);
    assert_eq!(sim.status("alpha").light, Light::Green);
    let lost = sim.engine.stats().event_write_failures;
    assert!(lost >= 5, "{lost} lines lost");

    // Back: the first line says how many were lost, no later one does.
    fs::create_dir(&dir).unwrap();
    sim.ok(Request::SetMonitoringPaused { paused: true });
    sim.settle();
    let lines = log_lines(&dir);
    assert!(before > 0);
    assert_eq!(lines[0]["event"]["kind"], "lines_lost", "{lines:#?}");
    assert_eq!(lines[0]["event"]["count"].as_u64(), Some(lost));
    assert_eq!(events_named(&lines, "lines_lost").len(), 1);
    assert!(!events_named(&lines, "monitoring_paused").is_empty());
    // The lost lines used up their seq numbers.
    assert!(lines[0]["seq"].as_u64().unwrap() > u64::try_from(before).unwrap() + lost);
}

#[test]
fn lost_lines_are_counted_and_reported_once() {
    let parent = TempDir::new("lost");
    let dir = parent.path().join("log");
    fs::create_dir(&dir).unwrap();
    let mut log = EventLog::open(&dir).unwrap();
    fs::remove_dir(&dir).unwrap();
    for i in 0..3 {
        log.diag(T0 + i, None, &Diag::Stopped { in_time: false });
    }
    assert_eq!(log.failures(), 3);
    fs::create_dir(&dir).unwrap();
    log.diag(T0 + 10, None, &Diag::Stopped { in_time: true });
    log.diag(T0 + 11, None, &Diag::Stopped { in_time: true });
    let lines = log_lines(&dir);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["event"]["kind"], "lines_lost");
    assert_eq!(lines[0]["event"]["count"], 3);
    // Each try used up its seq numbers: the line, and from the second try on
    // the `lines_lost` line written with it.
    assert_eq!(seqs(&lines), [6, 7, 8]);
    assert_eq!(lines[1]["diag"]["event"], "stopped");
    assert_eq!(log.failures(), 3);
    // The partial line it may have left is cut off by a newline first.
    let text = fs::read_to_string(dir.join(EVENTS_FILE)).unwrap();
    assert!(text.starts_with('\n'));
}

#[test]
fn the_directory_must_be_a_plain_one() {
    let parent = TempDir::new("plain");
    assert!(EventLog::open(&parent.path().join("missing")).is_err());
    let file = parent.path().join("file");
    fs::write(&file, b"x").unwrap();
    assert!(EventLog::open(&file).is_err());
}

#[test]
fn symbolic_links_are_refused() {
    let parent = TempDir::new("links");
    let real = parent.path().join("real");
    fs::create_dir(&real).unwrap();
    let linked_dir = parent.path().join("linked");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&real, &linked_dir);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&real, &linked_dir);
    if let Err(e) = made {
        // Creating symlinks needs a privilege on Windows that test machines
        // may not grant; nothing to check without one.
        eprintln!("symlink not created ({e}); skipping");
        return;
    }
    assert!(EventLog::open(&linked_dir).is_err());

    // A log file replaced by a link to another file: nothing is written
    // through it, and nothing is read through it.
    let target = parent.path().join("elsewhere.txt");
    fs::write(
        &target,
        b"{\"seq\":5,\"time\":\"2026-01-01T00:00:00.000+00:00\",\"event\":{\"kind\":\"watchdog_started\"}}\n",
    )
    .unwrap();
    let mut log = EventLog::open(&real).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, real.join(EVENTS_FILE)).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, real.join(EVENTS_FILE)).unwrap();
    log.diag(T0, None, &Diag::Stopped { in_time: true });
    assert_eq!(log.failures(), 1);
    assert!(fs::read(&target).unwrap().starts_with(b"{\"seq\":5"));
    let (entries, _) = log.read_page(None, Lines::All, |_| None);
    assert!(entries.is_empty(), "read through a link: {entries:?}");
    // Nor through a rotated file that is a link.
    fs::remove_file(real.join(EVENTS_FILE)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, real.join("events.1.jsonl")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, real.join("events.1.jsonl")).unwrap();
    let (entries, _) = log.read_page(None, Lines::All, |_| None);
    assert!(entries.is_empty(), "read through a link: {entries:?}");
    assert_eq!(EventLog::open(&real).unwrap().next_seq(), 1);
}

#[test]
fn storage_failing_at_start_is_logged_for_the_user() {
    let dir = TempDir::new("start-failed");
    let lab = Lab::new(&["alpha"]);
    let clock = Arc::new(FakeClock::new(T0));
    let store = MemoryStore::new();
    let wd = watchdog(&lab, &clock, &store, wgal_core::Startup::StorageFailed);
    assert_eq!(wd.storage_health(), StorageHealth::Failed);
    let _sim = Sim::new(
        wd,
        quiet_notifier(),
        &lab,
        &clock,
        &store,
        AccessList::default(),
        Some(EventLog::open(dir.path()).unwrap()),
        &Options::default(),
    );
    let lines = log_lines(dir.path());
    assert_eq!(
        event_kinds(&lines),
        ["watchdog_started", "storage_failed"],
        "{lines:#?}"
    );
}
