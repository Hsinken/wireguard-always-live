//! The event log (plan §9.1, "JSONL 輪替事件日誌"; owner's requests 1, 9 and
//! 10 of 2026-11-03): one small JSON object per line, for the GUI's log and so
//! a long run (plan T28) can be judged afterwards.
//!
//! Every line has `seq`, which increases by one per line and continues after
//! a restart from the highest one in the kept files, and `time`, RFC 3339 in
//! the configured time zone with milliseconds and that zone's offset
//! (`2026-11-03T17:16:05.123+08:00`). A line about a tunnel has its
//! [`TunnelLabel`]. Then the line is one of two kinds:
//!
//! - `event`: exactly a contract [`LogEvent`], for the user. These are what
//!   `Request::ReadEventLog` returns ([`EventLog::read_page`]).
//! - `diag`: the detail the soak relies on, a [`Diag`]: the light with its
//!   fault kind, the OS state, the recovery phase, the takeover state,
//!   incidents opening and closing with their first recovery outcome and
//!   notification state, the host-wide pause, storage health, the
//!   notification service's on/off and token state, test notifications, the
//!   last delivery failure, the worker count, and the loop starting and
//!   stopping. A diag object keeps the day-1 log's names (`"event":"light"`
//!   and its fields), one level down, so tools written for it still find
//!   them. The first snapshot gives a `tunnel` diag with each tunnel's whole
//!   state. The reader skips these lines.
//!
//! What is written: the label is the random one the watchdog drew for the
//! tunnel when it was registered and keeps in its stored state. The tunnel
//! ID is often the manager's own name for the tunnel (a Windows tunnel name,
//! a macOS configuration UUID, an Ubuntu interface), so it is never written,
//! and the label is not derived from it or from anything else in the log.
//! Nor are probe targets, display names, chat IDs, tokens or any text that
//! came from the OS: every field is an enum, a count, a flag, a time or an
//! incident ID the watchdog made up.
//!
//! Files: `events.jsonl` in the directory the platform passes in is the
//! current file. Before a line would take it past [`MAX_FILE_BYTES`], or
//! when its first line is older than [`MAX_FILE_AGE`], it becomes
//! `events.1.jsonl` and the older ones move up one, at most [`KEPT_FILES`]
//! of them. Retention (owner's request 10) is done when lines are written,
//! never on a timer: after a rotation, and otherwise at most once an hour,
//! rotated files whose newest line is older than [`RETENTION`] (72 hours,
//! judged by their modification time) are deleted, as are the oldest ones
//! beyond the safety caps of [`KEPT_FILES`] files and `KEPT_FILES` times the
//! file size. So the log keeps the last 72 hours and at most about 24 more
//! (a rotated file spans at most a day), never more than eight files; a log
//! that grows by more than the caps in three days keeps less. Lines are
//! written straight to the file without a sync: a power loss can lose the
//! last few.
//!
//! A line that cannot be written is counted and dropped (its `seq` stays
//! used, as does that of a `lines_lost` line tried with it); the next write
//! starts on a new line and first writes a `lines_lost` event with the
//! count. Nothing that fails here ever stops
//! monitoring or recovery.
//!
//! The directory and the files must not be symbolic links (or junctions);
//! files are opened without following one and must be plain files, for
//! reading as for writing. Keeping others from writing to the directory is
//! the installer's job.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use wgal_protocol::{
    AdapterErrorKind, CredentialState, EventEntry, FirstRecovery, IncidentId, Light, LightReason,
    LogEvent, MAX_EVENT_PAGE, NoticeKind, NotificationFailure, NotificationState, OsState,
    ProbeTarget, ProbeTiming, RecoveryPhase, RecoveryRetry, RequestId, Snapshot, StorageHealth,
    Takeover, TestNotificationState, TunnelId, TunnelStatus,
};

/// The current file.
pub const EVENTS_FILE: &str = "events.jsonl";
/// Size at which the current file is rotated.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Age of its first line at which the current file is rotated.
pub const MAX_FILE_AGE: Duration = Duration::from_secs(24 * 3600);
/// Rotated files whose newest line is older than this are deleted.
pub const RETENTION: Duration = Duration::from_secs(72 * 3600);
/// Most rotated files kept besides the current one: `events.1.jsonl`
/// (newest) to `events.7.jsonl`. A safety cap; retention normally keeps
/// three or four.
pub const KEPT_FILES: usize = 7;
/// Without a rotation, retention is checked at most this often.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// Most of a line read to learn a file's first time or `seq`.
const FIRST_LINE_BYTES: usize = 4096;

/// Opens a symlink or junction itself instead of following it.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// How a tunnel is named in the event log: the watchdog's stored, random
/// label for it (`Watchdog::log_label`, `t-` and 8 hex digits). Stable across
/// restarts, so one tunnel's lines can be followed, and unrelated to the
/// tunnel's IDs and names: nothing in an exported log leads back to them
/// (plan §9.3; review of wave two, P3-1). Only the host's own state file
/// says which tunnel a label stands for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TunnelLabel(String);

/// Written for a tunnel that has no stored label, which the watchdog never
/// leaves; never the tunnel's ID.
const UNLABELED: &str = "t-unlabeled";

impl TunnelLabel {
    /// The label stored for a tunnel, as `Watchdog::log_label` gives it.
    pub fn stored(label: Option<&str>) -> Self {
        Self(label.unwrap_or(UNLABELED).to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The detail of a `diag` line (see the module documentation), serialized
/// with its name in `event` as in the day-1 log.
///
/// `tunnel` is written once per tunnel, when the loop first sees it (at
/// start or after it is registered), with its whole state; the other tunnel
/// entries report one change each. `fault` is the kind of the error behind
/// the light, if any, never its message. `notification_failure` with no
/// `failure` means the last formal notification went through again.
/// `workers` is the most worker threads the loop may run now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Diag {
    Started {
        storage: StorageHealth,
        monitoring_paused: bool,
        tunnels: usize,
    },
    Stopped {
        in_time: bool,
    },
    Tunnel {
        light: Light,
        reason: LightReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        fault: Option<AdapterErrorKind>,
        os_state: OsState,
        recovery: RecoveryPhase,
        takeover: Takeover,
        #[serde(skip_serializing_if = "Option::is_none")]
        incident: Option<IncidentId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_recovery: Option<FirstRecovery>,
        #[serde(skip_serializing_if = "Option::is_none")]
        notification: Option<NotificationState>,
    },
    TunnelGone,
    Light {
        light: Light,
        reason: LightReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        fault: Option<AdapterErrorKind>,
    },
    OsState {
        os_state: OsState,
    },
    Recovery {
        phase: RecoveryPhase,
    },
    Takeover {
        takeover: Takeover,
    },
    IncidentOpened {
        incident: IncidentId,
        first_recovery: FirstRecovery,
        notification: NotificationState,
    },
    FirstRecovery {
        incident: IncidentId,
        first_recovery: FirstRecovery,
    },
    Notification {
        incident: IncidentId,
        notification: NotificationState,
    },
    IncidentClosed {
        incident: IncidentId,
    },
    MonitoringPaused {
        paused: bool,
    },
    Storage {
        storage: StorageHealth,
    },
    Notifier {
        enabled: bool,
        token: CredentialState,
    },
    TestNotification {
        state: TestNotificationState,
    },
    NotificationFailure {
        failure: Option<NotificationFailure>,
    },
    Workers {
        effective: usize,
    },
}

/// One line as written. `seq` comes first, so a reader can skip lines by
/// their number without parsing them.
#[derive(Serialize)]
struct Line<'a> {
    seq: u64,
    time: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tunnel: Option<&'a TunnelLabel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<&'a LogEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diag: Option<&'a Diag>,
}

/// One line as read back. Diag lines have no `event` and are skipped; a
/// line whose `event` is not a [`LogEvent`] does not decode at all.
#[derive(Deserialize)]
struct Stored {
    seq: u64,
    time: String,
    #[serde(default)]
    tunnel: Option<String>,
    #[serde(default)]
    event: Option<LogEvent>,
}

/// Just the time of a line, of either version.
#[derive(Deserialize)]
struct Timed {
    time: String,
}

#[derive(Clone, Copy)]
enum Body<'a> {
    Event(&'a LogEvent),
    Diag(&'a Diag),
}

/// See the module documentation.
#[derive(Debug)]
pub struct EventLog {
    dir: PathBuf,
    max_bytes: u64,
    zone: TimeZone,
    next_seq: u64,
    /// Time of the current file's first line, once known.
    first_ms: Option<i64>,
    /// When retention was last checked.
    pruned_ms: Option<i64>,
    /// Lines not written since the last one that was.
    lost: u64,
    /// Lines not written since the log was opened.
    failures: u64,
}

impl EventLog {
    /// Uses `dir`, which must exist and be a plain directory. Reads the kept
    /// files for the highest `seq` so far; nothing is written until the
    /// first line. Times are written in UTC until [`Self::set_time_zone`].
    pub fn open(dir: &Path) -> io::Result<Self> {
        check_dir(dir)?;
        let mut log = Self {
            dir: dir.to_path_buf(),
            max_bytes: MAX_FILE_BYTES,
            zone: TimeZone::UTC,
            next_seq: 1,
            first_ms: None,
            pruned_ms: None,
            lost: 0,
            failures: 0,
        };
        let highest = log
            .files()
            .iter()
            .filter_map(|path| read_capped(path, log.read_cap()))
            .filter_map(|text| text.lines().filter_map(line_seq).max())
            .max();
        log.next_seq = highest.map_or(1, |seq| seq.saturating_add(1));
        Ok(log)
    }

    /// Rotates at `max_bytes` instead of [`MAX_FILE_BYTES`], for tests. The
    /// cap on the bytes of all rotated files scales with it.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// The zone line times are written in from now on.
    pub fn set_time_zone(&mut self, zone: TimeZone) {
        self.zone = zone;
    }

    pub fn time_zone(&self) -> &TimeZone {
        &self.zone
    }

    /// The `seq` the next line gets.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Lines that could not be written since the log was opened.
    pub fn failures(&self) -> u64 {
        self.failures
    }

    /// Writes a line for the user. A failure is counted and reported with
    /// the next line that is written; it is never returned, so it cannot
    /// stop the caller.
    pub fn event(&mut self, unix_ms: i64, tunnel: Option<&TunnelLabel>, event: &LogEvent) {
        self.write(unix_ms, tunnel, Body::Event(event));
    }

    /// Writes a diagnostic line, which the reader skips. Failures as for
    /// [`Self::event`].
    pub fn diag(&mut self, unix_ms: i64, tunnel: Option<&TunnelLabel>, diag: &Diag) {
        self.write(unix_ms, tunnel, Body::Diag(diag));
    }

    fn write(&mut self, unix_ms: i64, tunnel: Option<&TunnelLabel>, body: Body<'_>) {
        let time = local_time(unix_ms, &self.zone);
        let mut bytes = Vec::new();
        if self.lost > 0 {
            // A write that failed midway may have left part of a line.
            bytes.push(b'\n');
            let lost = LogEvent::LinesLost {
                count: u32::try_from(self.lost).unwrap_or(u32::MAX),
            };
            self.encode(&mut bytes, &time, None, Body::Event(&lost));
        }
        if !self.encode(&mut bytes, &time, tunnel, body) {
            self.lose();
            return;
        }
        match self.append(unix_ms, &bytes) {
            Ok(rotated) => {
                self.lost = 0;
                self.retain(unix_ms, rotated);
            }
            Err(_) => self.lose(),
        }
    }

    /// Adds one line to `bytes` under the next `seq`, which is used up even
    /// if the line is never written. Returns whether it could be encoded.
    fn encode(
        &mut self,
        bytes: &mut Vec<u8>,
        time: &str,
        tunnel: Option<&TunnelLabel>,
        body: Body<'_>,
    ) -> bool {
        let (event, diag) = match body {
            Body::Event(event) => (Some(event), None),
            Body::Diag(diag) => (None, Some(diag)),
        };
        let line = Line {
            seq: self.next_seq,
            time,
            tunnel,
            event,
            diag,
        };
        self.next_seq = self.next_seq.saturating_add(1);
        match serde_json::to_vec(&line) {
            Ok(encoded) => {
                bytes.extend_from_slice(&encoded);
                bytes.push(b'\n');
                true
            }
            Err(_) => false,
        }
    }

    fn lose(&mut self) {
        self.lost = self.lost.saturating_add(1);
        self.failures = self.failures.saturating_add(1);
    }

    /// Appends `bytes`, rotating first if the current file is full or its
    /// first line is too old. Returns whether it rotated.
    fn append(&mut self, unix_ms: i64, bytes: &[u8]) -> io::Result<bool> {
        check_dir(&self.dir)?;
        let current = self.dir.join(EVENTS_FILE);
        let mut file = open_append(&current)?;
        let len = file.metadata()?.len();
        let mut rotated = false;
        if len > 0 {
            let adding = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let first = self.first_ms.or_else(|| first_time(&current));
            // A first line that cannot be read has no age: start afresh.
            let too_old =
                first.is_none_or(|first| unix_ms.saturating_sub(first) >= millis(MAX_FILE_AGE));
            if len.saturating_add(adding) > self.max_bytes || too_old {
                drop(file);
                self.rotate()?;
                file = open_append(&current)?;
                rotated = true;
            } else {
                self.first_ms = first;
            }
        }
        if rotated || len == 0 {
            self.first_ms = None;
        }
        file.write_all(bytes)?;
        if self.first_ms.is_none() {
            self.first_ms = Some(unix_ms);
        }
        Ok(rotated)
    }

    fn older(&self, n: usize) -> PathBuf {
        self.dir.join(format!("events.{n}.jsonl"))
    }

    /// The current file and the rotated ones, newest first.
    fn files(&self) -> Vec<PathBuf> {
        std::iter::once(self.dir.join(EVENTS_FILE))
            .chain((1..=KEPT_FILES).map(|n| self.older(n)))
            .collect()
    }

    /// Most bytes of one file read: a file is rotated before a line takes
    /// it past `max_bytes`, so this is never reached unless the file was
    /// changed by something else; then its newest lines are read.
    fn read_cap(&self) -> u64 {
        self.max_bytes.max(MAX_FILE_BYTES).saturating_mul(2)
    }

    /// Drops the oldest kept file and moves each other one up. Renaming or
    /// removing a symbolic link touches only the link.
    fn rotate(&self) -> io::Result<()> {
        match fs::remove_file(self.older(KEPT_FILES)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        for n in (1..KEPT_FILES).rev() {
            match fs::rename(self.older(n), self.older(n + 1)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        fs::rename(self.dir.join(EVENTS_FILE), self.older(1))
    }

    /// After a line was written: deletes rotated files whose newest line is
    /// older than [`RETENTION`], and the oldest beyond the bytes cap, when
    /// the file was just rotated or an hour has passed since the last check
    /// (either way: a clock set back counts too). Failures are ignored; the
    /// next check tries again.
    fn retain(&mut self, unix_ms: i64, rotated: bool) {
        let due = rotated
            || self
                .pruned_ms
                .is_none_or(|at| unix_ms.abs_diff(at) >= PRUNE_EVERY.as_secs() * 1000);
        if !due {
            return;
        }
        self.pruned_ms = Some(unix_ms);
        let cutoff = unix_ms.saturating_sub(millis(RETENTION));
        let cutoff = u64::try_from(cutoff)
            .ok()
            .and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms)));
        let budget = self
            .max_bytes
            .saturating_mul(u64::try_from(KEPT_FILES).unwrap_or(u64::MAX));
        let mut total = 0_u64;
        let mut over = false;
        for n in 1..=KEPT_FILES {
            let path = self.older(n);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            total = total.saturating_add(meta.len());
            // Older files come after: once over the cap, all of them go.
            over |= total > budget;
            let expired = cutoff.is_some_and(|cutoff| {
                meta.modified()
                    .is_ok_and(|modified: SystemTime| modified < cutoff)
            });
            if over || expired {
                // Retried at the next check if it fails.
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// One page of the user's events (`Request::ReadEventLog`): newest
    /// first, at most [`MAX_EVENT_PAGE`] entries with `seq` below
    /// `before_seq` that `lines` selects, and whether older matching
    /// entries exist. `tunnel_id` maps a label to
    /// the tunnel it stands for now, if any.
    ///
    /// Reads at most the kept files, each up to its cap, newest first, and
    /// skips a file whose first line is already at or above `before_seq`.
    /// A line that cannot be read, has no `event`, or whose event is not a
    /// [`LogEvent`] is skipped; nothing fails the whole page. Files are
    /// opened as the writer opens them: no links, plain files only.
    pub fn read_page(
        &self,
        before_seq: Option<u64>,
        lines: Lines<'_>,
        tunnel_id: impl Fn(&str) -> Option<TunnelId>,
    ) -> (Vec<EventEntry>, bool) {
        let mut entries = Vec::new();
        if check_dir(&self.dir).is_err() {
            return (entries, false);
        }
        let below = |seq: u64| before_seq.is_none_or(|before| seq < before);
        for path in self.files() {
            if first_seq(&path).is_some_and(|seq| !below(seq)) {
                // Every line of this file is newer than the page.
                continue;
            }
            let Some(text) = read_capped(&path, self.read_cap()) else {
                continue;
            };
            for line in text.lines().rev() {
                if !line_seq(line).is_some_and(below) {
                    continue;
                }
                let Ok(stored) = serde_json::from_str::<Stored>(line) else {
                    continue;
                };
                let Some(event) = stored.event else {
                    continue;
                };
                if !lines.selects(stored.tunnel.as_deref(), &event) {
                    continue;
                }
                let Ok(at) = stored.time.parse::<Timestamp>() else {
                    continue;
                };
                if entries.len() == MAX_EVENT_PAGE {
                    return (entries, true);
                }
                let id = stored.tunnel.as_deref().and_then(&tunnel_id);
                entries.push(EventEntry {
                    seq: stored.seq,
                    at_unix_ms: at.as_millisecond(),
                    // A labelled line whose label no registered tunnel has.
                    removed_tunnel: stored.tunnel.is_some() && id.is_none(),
                    tunnel_id: id,
                    event,
                });
            }
        }
        (entries, false)
    }
}

/// Which lines [`EventLog::read_page`] returns (`EventFilter`, with a
/// tunnel named by its label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lines<'a> {
    All,
    /// The lines labelled with this tunnel label.
    Tunnel(&'a str),
    /// The watchdog's own start and stop lines.
    Watchdog,
    /// Every line about no tunnel: those written without a tunnel label.
    System,
}

impl Lines<'_> {
    fn selects(self, label: Option<&str>, event: &LogEvent) -> bool {
        match self {
            Self::All => true,
            Self::Tunnel(wanted) => label == Some(wanted),
            Self::Watchdog => {
                label.is_none()
                    && matches!(event, LogEvent::WatchdogStarted | LogEvent::WatchdogStopped)
            }
            Self::System => label.is_none(),
        }
    }
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The `seq` a line starts with, without parsing the rest.
fn line_seq(line: &str) -> Option<u64> {
    let rest = line.strip_prefix(r#"{"seq":"#)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The first non-empty line of a file, if one is found in its first bytes.
fn first_line(path: &Path) -> Option<String> {
    let mut file = open_read(path).ok()?;
    let mut buf = vec![0_u8; FIRST_LINE_BYTES];
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let text = String::from_utf8_lossy(&buf[..filled]);
    let at_end = filled < buf.len();
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        // The last piece is a whole line only if the file ended there.
        if lines.peek().is_none() && !at_end {
            return None;
        }
        if !line.trim().is_empty() {
            return Some(line.to_owned());
        }
    }
    None
}

/// When the file's first line was written, if it can be read.
fn first_time(path: &Path) -> Option<i64> {
    let line = first_line(path)?;
    let timed: Timed = serde_json::from_str(&line).ok()?;
    let at: Timestamp = timed.time.parse().ok()?;
    Some(at.as_millisecond())
}

fn first_seq(path: &Path) -> Option<u64> {
    line_seq(&first_line(path)?)
}

/// The file's text, at most its last `cap` bytes (from the first whole line
/// in them). `None` if it is missing, a link, not a plain file or cannot be
/// read. Bytes that are not UTF-8 spoil only their own line.
fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let mut file = open_read(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(cap);
    if start > 0 {
        file.seek(SeekFrom::Start(start)).ok()?;
    }
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    Some(if start > 0 {
        // Cut into the middle of a line: drop it.
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_owned())
    } else {
        text.into_owned()
    })
}

/// A Unix time in milliseconds as RFC 3339 in `zone`, with milliseconds and
/// the zone's offset then: `2026-11-03T17:16:05.123+08:00`. An offset with
/// seconds (some zones' local mean time, long before 1970), which RFC 3339
/// cannot write, and a time jiff cannot represent are written as UTC,
/// `+00:00`.
pub fn local_time(unix_ms: i64, zone: &TimeZone) -> String {
    let offset = Timestamp::from_millisecond(unix_ms)
        .map(|at| zone.to_offset(at).seconds())
        .unwrap_or(0);
    let offset = if offset % 60 == 0 { offset } else { 0 };
    let local = unix_ms.saturating_add(i64::from(offset) * 1000);
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.unsigned_abs() / 60;
    format!(
        "{}{sign}{:02}:{:02}",
        civil(local),
        minutes / 60,
        minutes % 60
    )
}

/// A Unix time in milliseconds as `YYYY-MM-DDTHH:MM:SS.mmm`, no zone.
fn civil(unix_ms: i64) -> String {
    let millis = unix_ms.rem_euclid(1000);
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn check_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the event log directory is not a plain directory",
        ));
    }
    Ok(())
}

fn refuse_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the event log file is a symbolic link",
        )),
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whatever was there between the check and the open: a link opened as
/// itself, a directory, a FIFO or a device is not used.
fn plain(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the event log file is not a plain file",
        ))
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    refuse_link(path)?;
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    plain(open_no_follow(&mut options, path)?)
}

fn open_read(path: &Path) -> io::Result<File> {
    refuse_link(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    plain(open_no_follow(&mut options, path)?)
}

#[cfg(unix)]
fn open_no_follow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK keeps a FIFO put in the file's place from blocking the open.
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_no_follow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    options
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_no_follow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    // Checked by `refuse_link` just before.
    options.open(path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HostSeen {
    storage: StorageHealth,
    paused: bool,
    enabled: bool,
    token: CredentialState,
    /// The request ID only tells one test from the next; it is not written.
    test: Option<(RequestId, TestNotificationState)>,
    failure: Option<NotificationFailure>,
    workers: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IncidentSeen {
    id: IncidentId,
    first_recovery: FirstRecovery,
    notification: NotificationState,
    exhausted_notification: NotificationState,
}

/// What the user set for the tunnel's monitoring.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingsSeen {
    recovery_target: Option<ProbeTarget>,
    observe_targets: Vec<ProbeTarget>,
    probe: ProbeTiming,
    retry: RecoveryRetry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TunnelSeen {
    light: (Light, LightReason, Option<AdapterErrorKind>),
    os_state: OsState,
    recovery: RecoveryPhase,
    takeover: Takeover,
    incident: Option<IncidentSeen>,
    auto_recovery: bool,
    notifications: bool,
    settings: SettingsSeen,
    /// The closed incident's `recovered` message and how far it got.
    recovered: Option<(IncidentId, NotificationState)>,
}

/// One line the [`Tracker`] wants written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Event(LogEvent),
    Diag(Diag),
}

impl TunnelSeen {
    fn of(status: &TunnelStatus) -> Self {
        Self {
            light: (
                status.light,
                status.light_reason,
                status.fault.as_ref().map(|f| f.kind),
            ),
            os_state: status.os_state,
            recovery: status.recovery,
            takeover: status.takeover,
            incident: status.incident.as_ref().map(|i| IncidentSeen {
                id: i.incident_id.clone(),
                first_recovery: i.first_recovery,
                notification: i.notification,
                exhausted_notification: i.exhausted_notification,
            }),
            auto_recovery: status.intent.auto_recovery_enabled,
            notifications: status.notifications_enabled,
            settings: SettingsSeen {
                recovery_target: status.recovery_target.as_ref().map(|t| t.target),
                observe_targets: status.observe_targets.iter().map(|t| t.target).collect(),
                probe: status.probe,
                retry: status.retry,
            },
            recovered: status
                .recovered_notification
                .as_ref()
                .map(|n| (n.incident_id.clone(), n.notification)),
        }
    }

    fn whole(&self) -> Diag {
        let (light, reason, fault) = self.light;
        Diag::Tunnel {
            light,
            reason,
            fault,
            os_state: self.os_state,
            recovery: self.recovery,
            takeover: self.takeover,
            incident: self.incident.as_ref().map(|i| i.id.clone()),
            first_recovery: self.incident.as_ref().map(|i| i.first_recovery),
            notification: self.incident.as_ref().map(|i| i.notification),
        }
    }

    /// What changed from `before` to `self`, in the order it is written.
    /// `failure` is why the last formal notification was not accepted.
    fn changes(&self, before: &Self, failure: Option<NotificationFailure>) -> Vec<Entry> {
        let mut entries = Vec::new();
        if self.os_state != before.os_state {
            entries.push(Entry::Diag(Diag::OsState {
                os_state: self.os_state,
            }));
        }
        if self.takeover != before.takeover {
            entries.push(Entry::Diag(Diag::Takeover {
                takeover: self.takeover,
            }));
            // Only `RegisterTunnel` gives a lost binding back.
            if before.takeover == Takeover::BindingLost {
                entries.push(Entry::Event(LogEvent::TunnelRegistered));
            }
        }
        if self.recovery != before.recovery {
            entries.push(Entry::Diag(Diag::Recovery {
                phase: self.recovery,
            }));
        }
        match (&before.incident, &self.incident) {
            (Some(old), Some(new)) if old.id == new.id => {
                if new.first_recovery != old.first_recovery {
                    entries.push(Entry::Diag(Diag::FirstRecovery {
                        incident: new.id.clone(),
                        first_recovery: new.first_recovery,
                    }));
                }
                if new.notification != old.notification {
                    entries.push(Entry::Diag(Diag::Notification {
                        incident: new.id.clone(),
                        notification: new.notification,
                    }));
                    entries.extend(notification_event(
                        NoticeKind::Alert,
                        &new.id,
                        old.notification,
                        new.notification,
                        failure,
                    ));
                }
                if new.exhausted_notification != old.exhausted_notification {
                    entries.extend(notification_event(
                        NoticeKind::RetriesExhausted,
                        &new.id,
                        old.exhausted_notification,
                        new.exhausted_notification,
                        failure,
                    ));
                }
            }
            (old, new) => {
                if let Some(old) = old {
                    entries.push(Entry::Diag(Diag::IncidentClosed {
                        incident: old.id.clone(),
                    }));
                }
                if let Some(new) = new {
                    entries.push(Entry::Diag(Diag::IncidentOpened {
                        incident: new.id.clone(),
                        first_recovery: new.first_recovery,
                        notification: new.notification,
                    }));
                }
            }
        }
        if self.light != before.light {
            let (light, reason, fault) = self.light;
            entries.push(Entry::Diag(Diag::Light {
                light,
                reason,
                fault,
            }));
            if (light, reason) != (before.light.0, before.light.1) {
                entries.push(Entry::Event(LogEvent::LightChanged { light, reason }));
            }
        }
        if self.auto_recovery != before.auto_recovery {
            entries.push(Entry::Event(LogEvent::AutoRecoveryChanged {
                enabled: self.auto_recovery,
            }));
        }
        if self.notifications != before.notifications {
            entries.push(Entry::Event(LogEvent::TunnelNotificationsChanged {
                enabled: self.notifications,
            }));
        }
        if self.settings != before.settings {
            entries.push(Entry::Event(LogEvent::SettingsChanged));
        }
        if let Some((id, new)) = &self.recovered {
            // A message for another incident than before starts from not
            // eligible.
            let old = match &before.recovered {
                Some((old_id, old)) if old_id == id => *old,
                _ => NotificationState::NotEligible,
            };
            if *new != old {
                entries.extend(notification_event(
                    NoticeKind::Recovered,
                    id,
                    old,
                    *new,
                    failure,
                ));
            }
        }
        entries
    }
}

/// The user's line for one of an incident's messages (`notice`) moving from
/// `old` to `new`: accepted, cancelled, or a send that did not go through
/// (back to pending to retry, refused, or of unknown outcome).
fn notification_event(
    notice: NoticeKind,
    incident: &IncidentId,
    old: NotificationState,
    new: NotificationState,
    failure: Option<NotificationFailure>,
) -> Option<Entry> {
    let incident_id = incident.clone();
    let event = match new {
        NotificationState::Sent => LogEvent::NotificationSent {
            incident_id,
            notice,
        },
        NotificationState::Cancelled => LogEvent::NotificationCancelled {
            incident_id,
            notice,
        },
        NotificationState::Pending | NotificationState::Rejected | NotificationState::Unknown
            if old == NotificationState::Reserved =>
        {
            // The notifier records why with the result; this is only for a
            // result it did not explain.
            let fallback = match new {
                NotificationState::Unknown => NotificationFailure::OutcomeUnknown,
                NotificationState::Rejected => NotificationFailure::ChatRejected,
                _ => NotificationFailure::NetworkUnreachable,
            };
            LogEvent::NotificationFailed {
                incident_id,
                failure: failure.unwrap_or(fallback),
                notice,
            }
        }
        _ => return None,
    };
    Some(Entry::Event(event))
}

/// Turns consecutive snapshots into lines.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    host: Option<HostSeen>,
    /// Each tunnel's label, kept so a tunnel that goes away is still named.
    tunnels: BTreeMap<TunnelId, (TunnelLabel, TunnelSeen)>,
}

impl Tracker {
    /// The label this tracker last saw for the tunnel, if any.
    pub(crate) fn label(&self, id: &TunnelId) -> Option<&TunnelLabel> {
        self.tunnels.get(id).map(|(label, _)| label)
    }

    /// What changed since the last snapshot; for the first one, the whole
    /// state as diag lines (and `storage_failed` if storage is failing).
    /// `workers` is the most worker threads the loop may run now, `label`
    /// gives each tunnel's stored label.
    pub(crate) fn observe(
        &mut self,
        snapshot: &Snapshot,
        workers: usize,
        label: impl Fn(&TunnelId) -> TunnelLabel,
    ) -> Vec<(Option<TunnelLabel>, Entry)> {
        let mut entries = Vec::new();
        let n = &snapshot.notification;
        let host = HostSeen {
            storage: snapshot.storage,
            paused: snapshot.monitoring_paused,
            enabled: n.enabled,
            token: n.token,
            test: n
                .last_test
                .as_ref()
                .map(|t| (t.request_id.clone(), t.state)),
            failure: n.last_failure,
            workers,
        };
        let first = self.host.is_none();
        match &self.host {
            None => {
                entries.push(Entry::Diag(Diag::Started {
                    storage: host.storage,
                    monitoring_paused: host.paused,
                    tunnels: snapshot.tunnels.len(),
                }));
                if host.storage == StorageHealth::Failed {
                    entries.push(Entry::Event(LogEvent::StorageFailed));
                }
                entries.push(Entry::Diag(Diag::Notifier {
                    enabled: host.enabled,
                    token: host.token,
                }));
                entries.push(Entry::Diag(Diag::Workers {
                    effective: host.workers,
                }));
            }
            Some(before) => entries.extend(host_changes(before, &host)),
        }
        let mut lines: Vec<_> = entries.into_iter().map(|e| (None, e)).collect();
        self.host = Some(host);

        let mut seen = BTreeMap::new();
        for status in snapshot.tunnels.iter() {
            let now = TunnelSeen::of(status);
            let label = label(&status.tunnel_id);
            match self.tunnels.get(&status.tunnel_id) {
                None => {
                    if !first {
                        lines.push((
                            Some(label.clone()),
                            Entry::Event(LogEvent::TunnelRegistered),
                        ));
                    }
                    lines.push((Some(label.clone()), Entry::Diag(now.whole())));
                }
                Some((_, before)) => lines.extend(
                    now.changes(before, n.last_failure)
                        .into_iter()
                        .map(|e| (Some(label.clone()), e)),
                ),
            }
            seen.insert(status.tunnel_id.clone(), (label, now));
        }
        for (id, (label, _)) in &self.tunnels {
            if !seen.contains_key(id) {
                lines.push((Some(label.clone()), Entry::Diag(Diag::TunnelGone)));
            }
        }
        self.tunnels = seen;
        lines
    }
}

fn host_changes(before: &HostSeen, now: &HostSeen) -> Vec<Entry> {
    let mut entries = Vec::new();
    if now.storage != before.storage {
        entries.push(Entry::Diag(Diag::Storage {
            storage: now.storage,
        }));
        entries.push(Entry::Event(match now.storage {
            StorageHealth::Failed => LogEvent::StorageFailed,
            StorageHealth::Ok => LogEvent::StorageRecovered,
        }));
    }
    if now.paused != before.paused {
        entries.push(Entry::Diag(Diag::MonitoringPaused { paused: now.paused }));
        entries.push(Entry::Event(if now.paused {
            LogEvent::MonitoringPaused
        } else {
            LogEvent::MonitoringResumed
        }));
    }
    if (now.enabled, now.token) != (before.enabled, before.token) {
        entries.push(Entry::Diag(Diag::Notifier {
            enabled: now.enabled,
            token: now.token,
        }));
    }
    if now.test != before.test
        && let Some((_, state)) = &now.test
    {
        entries.push(Entry::Diag(Diag::TestNotification { state: *state }));
        entries.push(Entry::Event(LogEvent::TestNotification { state: *state }));
    }
    if now.failure != before.failure {
        entries.push(Entry::Diag(Diag::NotificationFailure {
            failure: now.failure,
        }));
    }
    if now.workers != before.workers {
        entries.push(Entry::Diag(Diag::Workers {
            effective: now.workers,
        }));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_767_225_600_000;

    fn zone(name: &str) -> TimeZone {
        jiff::tz::db().get(name).unwrap()
    }

    #[test]
    fn times_in_utc() {
        assert_eq!(
            local_time(0, &TimeZone::UTC),
            "1970-01-01T00:00:00.000+00:00"
        );
        assert_eq!(
            local_time(T, &TimeZone::UTC),
            "2026-01-01T00:00:00.000+00:00"
        );
        assert_eq!(
            local_time(1_709_210_096_789, &TimeZone::UTC),
            "2024-02-29T12:34:56.789+00:00"
        );
        assert_eq!(
            local_time(-1, &TimeZone::UTC),
            "1969-12-31T23:59:59.999+00:00"
        );
    }

    #[test]
    fn times_in_a_zone_carry_its_offset_then() {
        // 2026-11-03T09:16:05.123Z.
        let at = 1_793_697_365_123;
        assert_eq!(
            local_time(at, &zone("Asia/Taipei")),
            "2026-11-03T17:16:05.123+08:00"
        );
        assert_eq!(
            local_time(at, &zone("America/New_York")),
            "2026-11-03T04:16:05.123-05:00"
        );
        // Summer time: four hours behind.
        assert_eq!(
            local_time(at - 60 * 86_400_000, &zone("America/New_York")),
            "2026-09-04T05:16:05.123-04:00"
        );
        assert_eq!(
            local_time(at, &zone("Asia/Kolkata")),
            "2026-11-03T14:46:05.123+05:30"
        );
        assert_eq!(
            local_time(at, &zone("Etc/GMT+12")),
            "2026-11-02T21:16:05.123-12:00"
        );
        // Each reads back as the same instant.
        for name in ["UTC", "Asia/Taipei", "America/New_York", "Asia/Kolkata"] {
            let text = local_time(at, &zone(name));
            let back: Timestamp = text.parse().unwrap();
            assert_eq!(back.as_millisecond(), at, "{name}: {text}");
        }
    }

    #[test]
    fn an_offset_with_seconds_is_written_in_utc() {
        // Liberia kept -00:44:30 until 1972.
        let monrovia = zone("Africa/Monrovia");
        assert_eq!(
            monrovia
                .to_offset(Timestamp::from_millisecond(0).unwrap())
                .seconds(),
            -2670
        );
        assert_eq!(local_time(0, &monrovia), "1970-01-01T00:00:00.000+00:00");
        assert_eq!(
            local_time(1_793_697_365_123, &monrovia),
            "2026-11-03T09:16:05.123+00:00"
        );
    }

    #[test]
    fn the_seq_is_read_from_the_start_of_a_line() {
        assert_eq!(line_seq(r#"{"seq":42,"time":"x"}"#), Some(42));
        assert_eq!(line_seq(r#"{"seq":7}"#), Some(7));
        assert_eq!(line_seq(r#"{"time":"x","seq":42}"#), None);
        assert_eq!(line_seq(r#"{"seq":"42"}"#), None);
        assert_eq!(line_seq(""), None);
        assert_eq!(line_seq(r#"{"seq":99999999999999999999999}"#), None);
    }

    #[test]
    fn a_missing_label_is_never_the_tunnel_id() {
        assert_eq!(
            TunnelLabel::stored(Some("t-0a1b2c3d")).as_str(),
            "t-0a1b2c3d"
        );
        assert_eq!(TunnelLabel::stored(None).as_str(), UNLABELED);
    }

    #[test]
    fn lines_put_seq_first_and_keep_event_and_diag_apart() {
        let label = TunnelLabel::stored(Some("t-0a1b2c3d"));
        let line = Line {
            seq: 3,
            time: "2026-01-01T00:00:00.000+00:00",
            tunnel: Some(&label),
            event: Some(&LogEvent::ProbeFailed {
                streak: 1,
                needed: 3,
            }),
            diag: None,
        };
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            r#"{"seq":3,"time":"2026-01-01T00:00:00.000+00:00","tunnel":"t-0a1b2c3d","event":{"kind":"probe_failed","streak":1,"needed":3}}"#
        );
        let line = Line {
            seq: 4,
            time: "2026-01-01T00:00:00.000+00:00",
            tunnel: None,
            event: None,
            diag: Some(&Diag::Stopped { in_time: true }),
        };
        let text = serde_json::to_string(&line).unwrap();
        assert_eq!(
            text,
            r#"{"seq":4,"time":"2026-01-01T00:00:00.000+00:00","diag":{"event":"stopped","in_time":true}}"#
        );
        let stored: Stored = serde_json::from_str(&text).unwrap();
        assert!(stored.event.is_none());
    }
}
