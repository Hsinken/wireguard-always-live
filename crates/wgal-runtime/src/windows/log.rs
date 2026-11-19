//! The service's own log, `<WGAL>\service.log`: how it started, why it did
//! not, and why it stopped, for an operator without a debugger. The event
//! log (`crate::events`) records what the watchdog saw; this one records
//! what the service did around it.
//!
//! Each line is `<UTC time> <message>`. Messages are fixed texts with
//! counts and the service's own error descriptions, which never quote a
//! file, a request, a SID, a path or anything from a secret. Control
//! characters become spaces and a line is cut at [`MAX_LINE_CHARS`], so one
//! message is always one line.
//!
//! Every line opens the file again without following a link
//! (`open_append_no_follow`) and refuses one that is a reparse point. Before
//! a line would take the file past [`MAX_LOG_BYTES`], it becomes
//! [`SERVICE_LOG_OLD`], replacing the older copy. A line that cannot be
//! written is dropped and counted, the next one that is written says how
//! many were lost, and nothing here ever stops the service. The folder is
//! the installation's protected one; the service never writes a log into a
//! folder that failed its checks ([`ServiceLog::none`]).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use wgal_platform::windows::open_append_no_follow;

use crate::notify::utc;

pub const SERVICE_LOG: &str = "service.log";
pub const SERVICE_LOG_OLD: &str = "service.1.log";
/// Largest the current log grows before it is rotated.
pub const MAX_LOG_BYTES: u64 = 256 * 1024;
/// Longest message kept, in characters.
pub const MAX_LINE_CHARS: usize = 400;

#[derive(Debug)]
pub struct ServiceLog {
    dir: Option<PathBuf>,
    max_bytes: u64,
    lost: u64,
}

impl ServiceLog {
    /// Writes into `dir`, a folder the caller has checked.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: Some(dir.to_path_buf()),
            max_bytes: MAX_LOG_BYTES,
            lost: 0,
        }
    }

    /// Writes nothing.
    pub fn none() -> Self {
        Self {
            dir: None,
            max_bytes: MAX_LOG_BYTES,
            lost: 0,
        }
    }

    /// Rotates at `max_bytes` instead of [`MAX_LOG_BYTES`], for tests.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Lines dropped since the last one that was written.
    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn write(&mut self, message: &str) {
        let Some(dir) = &self.dir else {
            return;
        };
        let mut line = format!("{} {}", utc(now_unix_ms()), one_line(message));
        if self.lost > 0 {
            line.push_str(&format!(" ({} earlier lines lost)", self.lost));
        }
        line.push('\n');
        match append(dir, &line, self.max_bytes) {
            Ok(()) => self.lost = 0,
            Err(_) => self.lost += 1,
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

fn one_line(message: &str) -> String {
    message
        .chars()
        .take(MAX_LINE_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn append(dir: &Path, line: &str, max_bytes: u64) -> io::Result<()> {
    let path = dir.join(SERVICE_LOG);
    let mut file = open_append_no_follow(&path)?;
    let len = file.metadata()?.len();
    if len > 0 && len + line.len() as u64 > max_bytes {
        drop(file);
        // Replaces the older copy, or a link in its place, never a target.
        fs::rename(&path, dir.join(SERVICE_LOG_OLD))?;
        file = open_append_no_follow(&path)?;
    }
    file.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wgal-log-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_one_timestamped_line_per_message() {
        let dir = scratch("lines");
        let mut log = ServiceLog::new(&dir);
        log.write("started");
        log.write("two\nlines\r\tand a bell \u{7}");
        let text = fs::read_to_string(dir.join(SERVICE_LOG)).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" UTC started"), "{}", lines[0]);
        assert!(
            lines[1].ends_with("two lines  and a bell  "),
            "{}",
            lines[1]
        );
        log.write(&"x".repeat(5000));
        let text = fs::read_to_string(dir.join(SERVICE_LOG)).unwrap();
        assert!(text.lines().last().unwrap().len() < MAX_LINE_CHARS + 40);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_before_the_cap_and_keeps_one_older_copy() {
        let dir = scratch("rotate");
        let mut log = ServiceLog::new(&dir).with_max_bytes(200);
        for n in 0..20 {
            log.write(&format!("line {n:02} {}", "y".repeat(40)));
        }
        let current = fs::metadata(dir.join(SERVICE_LOG)).unwrap().len();
        let older = fs::read_to_string(dir.join(SERVICE_LOG_OLD)).unwrap();
        assert!(current <= 200, "{current}");
        assert!(older.len() <= 200);
        assert!(
            fs::read_to_string(dir.join(SERVICE_LOG))
                .unwrap()
                .contains("line 19")
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_that_cannot_be_written_is_counted_and_reported_later() {
        let dir = scratch("lost");
        fs::create_dir(dir.join(SERVICE_LOG)).unwrap();
        let mut log = ServiceLog::new(&dir);
        log.write("one");
        log.write("two");
        assert_eq!(log.lost(), 2);
        fs::remove_dir(dir.join(SERVICE_LOG)).unwrap();
        log.write("three");
        assert_eq!(log.lost(), 0);
        let text = fs::read_to_string(dir.join(SERVICE_LOG)).unwrap();
        assert!(
            text.trim_end().ends_with("three (2 earlier lines lost)"),
            "{text}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_none_log_writes_nothing() {
        let mut log = ServiceLog::none();
        log.write("anything");
        assert_eq!(log.lost(), 0);
    }
}
