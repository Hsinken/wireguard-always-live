use std::fmt;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// A setting value was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidSettingValue {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for InvalidSettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for InvalidSettingValue {}

fn invalid(kind: &'static str, reason: &'static str) -> InvalidSettingValue {
    InvalidSettingValue { kind, reason }
}

// Slack on top of one interval and one timeout before a probe result goes
// stale, for a round that starts late or runs long. With the default timing
// it gives the 35 s of plan §6.2.
const FRESHNESS_MARGIN_SECS: u32 = 18;

/// How one tunnel is probed and when its failures count as a fault (owner
/// decision of 2026-11-03, which replaces the fixed "3 rounds and 60 s" of
/// plan §6.2).
///
/// Every `interval_secs` the watchdog runs one round of probes, waiting at
/// most `timeout_secs` for each. A fault is confirmed at the first failed
/// round where the current streak has at least `retries + 1` consecutive
/// failed rounds and at least `min_fault_secs` have passed since the
/// streak's first failure. Any successful round resets the streak.
/// `retries = 0` with `min_fault_secs = 0` confirms on the first failed
/// round. A probe result counts as fresh for interval + timeout + 18
/// seconds, plus timeout + 1 for each observe target the watchdog probes
/// after the recovery target (see [`Self::freshness_secs`]).
///
/// Time to judge a fault: with k = max(retries, ceil(min_fault_secs /
/// interval_secs)), the fault is confirmed k rounds after the first failed
/// round, that is between k × interval + timeout and (k + 1) × interval +
/// timeout seconds after traffic stopped. The defaults (15 s, 2 s, 2
/// retries, 60 s) give k = 4, so 62 to 77 s, and 35 s of freshness: the
/// plan's behaviour.
///
/// Each field is checked against its range (`INTERVAL_SECS`,
/// `TIMEOUT_SECS`, `RETRIES`, `MIN_FAULT_SECS`) on construction and when
/// decoding, so an out-of-range value is a malformed message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "ProbeTimingFields")]
pub struct ProbeTiming {
    interval_secs: u16,
    timeout_secs: u8,
    retries: u8,
    min_fault_secs: u16,
}

// The wire form of `ProbeTiming`, before its ranges are checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeTimingFields {
    interval_secs: u16,
    timeout_secs: u8,
    retries: u8,
    min_fault_secs: u16,
}

impl ProbeTiming {
    pub const INTERVAL_SECS: RangeInclusive<u16> = 10..=3600;
    pub const TIMEOUT_SECS: RangeInclusive<u8> = 1..=10;
    pub const RETRIES: RangeInclusive<u8> = 0..=10;
    pub const MIN_FAULT_SECS: RangeInclusive<u16> = 0..=3600;

    /// Probe every 15 s with a 2 s timeout; confirm after 3 failed rounds
    /// over at least 60 s.
    pub const DEFAULT: Self = Self {
        interval_secs: 15,
        timeout_secs: 2,
        retries: 2,
        min_fault_secs: 60,
    };

    pub fn new(
        interval_secs: u16,
        timeout_secs: u8,
        retries: u8,
        min_fault_secs: u16,
    ) -> Result<Self, InvalidSettingValue> {
        let reason = if !Self::INTERVAL_SECS.contains(&interval_secs) {
            "the interval must be 10 to 3600 seconds"
        } else if !Self::TIMEOUT_SECS.contains(&timeout_secs) {
            "the timeout must be 1 to 10 seconds"
        } else if !Self::RETRIES.contains(&retries) {
            "the retries must be 0 to 10"
        } else if !Self::MIN_FAULT_SECS.contains(&min_fault_secs) {
            "the minimum fault time must be 0 to 3600 seconds"
        } else {
            return Ok(Self {
                interval_secs,
                timeout_secs,
                retries,
                min_fault_secs,
            });
        };
        Err(invalid("probe timing", reason))
    }

    pub const fn interval_secs(self) -> u16 {
        self.interval_secs
    }

    pub const fn timeout_secs(self) -> u8 {
        self.timeout_secs
    }

    pub const fn retries(self) -> u8 {
        self.retries
    }

    pub const fn min_fault_secs(self) -> u16 {
        self.min_fault_secs
    }

    /// Consecutive failed rounds a fault needs: `retries + 1`.
    pub fn failures_needed(self) -> u32 {
        u32::from(self.retries) + 1
    }

    /// How many rounds after the streak's first failed round the fault is
    /// confirmed: max(retries, ceil(min_fault_secs / interval_secs)).
    pub fn rounds_to_confirm(self) -> u32 {
        let by_time = u32::from(self.min_fault_secs).div_ceil(u32::from(self.interval_secs));
        u32::from(self.retries).max(by_time)
    }

    /// How old a probe result may be, in seconds, and still count, for a
    /// tunnel whose rounds probe only the recovery target:
    /// `interval_secs + timeout_secs + 18`. A round probes the recovery
    /// target first and its observe targets after it, so the watchdog adds
    /// `timeout_secs + 1` for each observe target, and a round's own span
    /// never makes its recovery target's result stale (Codex review of wave
    /// three, P2-5). The value it uses for a tunnel is
    /// `TunnelStatus::freshness_secs`.
    pub fn freshness_secs(self) -> u32 {
        u32::from(self.interval_secs) + u32::from(self.timeout_secs) + FRESHNESS_MARGIN_SECS
    }
}

impl Default for ProbeTiming {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<ProbeTimingFields> for ProbeTiming {
    type Error = InvalidSettingValue;

    fn try_from(fields: ProbeTimingFields) -> Result<Self, InvalidSettingValue> {
        Self::new(
            fields.interval_secs,
            fields.timeout_secs,
            fields.retries,
            fields.min_fault_secs,
        )
    }
}

/// How often automatic recovery of one tunnel tries again after its first
/// round (owner decision of 2026-11-04, which replaces the fixed "1, 2, 5,
/// then every 15 minutes, never given up" of plan §6.3).
///
/// The first round runs as soon as the fault is confirmed, as before. Each
/// later round waits `interval_secs` (10 to 10800) from the end of the round
/// before it. `limit` is how many rounds may follow the first one in an
/// incident, 0 to 3600, or `null` to keep trying for as long as the fault
/// lasts. When a limited count is used up the watchdog stops trying for that
/// incident; a manual connect or reconnect, or the incident closing, starts
/// the count again. The default is at most 100 more rounds, one every 60
/// seconds. Each field is checked against its range on construction and when
/// decoding, so an out-of-range value is a malformed message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "RecoveryRetryFields")]
pub struct RecoveryRetry {
    limit: Option<u16>,
    interval_secs: u16,
}

// The wire form of `RecoveryRetry`, before its ranges are checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryRetryFields {
    limit: Option<u16>,
    interval_secs: u16,
}

impl RecoveryRetry {
    pub const LIMIT: RangeInclusive<u16> = 0..=3600;
    pub const INTERVAL_SECS: RangeInclusive<u16> = 10..=10800;

    /// At most 100 more rounds, one every 60 seconds.
    pub const DEFAULT: Self = Self {
        limit: Some(100),
        interval_secs: 60,
    };

    /// `limit`: `None` keeps trying for as long as the fault lasts.
    pub fn new(limit: Option<u16>, interval_secs: u16) -> Result<Self, InvalidSettingValue> {
        let reason = if limit.is_some_and(|n| !Self::LIMIT.contains(&n)) {
            "the retry limit must be 0 to 3600, or null to keep trying"
        } else if !Self::INTERVAL_SECS.contains(&interval_secs) {
            "the retry interval must be 10 to 10800 seconds"
        } else {
            return Ok(Self {
                limit,
                interval_secs,
            });
        };
        Err(invalid("recovery retry", reason))
    }

    /// How many rounds may follow the first; `None` for no limit.
    pub const fn limit(self) -> Option<u16> {
        self.limit
    }

    pub const fn interval_secs(self) -> u16 {
        self.interval_secs
    }

    /// Whether another round may start once `used` rounds have followed
    /// the first one.
    pub fn allows(self, used: u32) -> bool {
        self.limit.is_none_or(|limit| used < u32::from(limit))
    }
}

impl Default for RecoveryRetry {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<RecoveryRetryFields> for RecoveryRetry {
    type Error = InvalidSettingValue;

    fn try_from(fields: RecoveryRetryFields) -> Result<Self, InvalidSettingValue> {
        Self::new(fields.limit, fields.interval_secs)
    }
}

/// Fewest worker threads the watchdog can be set to use.
pub const MIN_WORKERS: u8 = 2;

/// Most worker threads the watchdog can be set to use: one for each of
/// [`crate::MAX_TUNNELS`] tunnels, plus one.
pub const MAX_WORKERS: u8 = 17;

/// A worker thread count chosen by the user, [`MIN_WORKERS`] to
/// [`MAX_WORKERS`]. Whatever is configured, the watchdog uses at most one
/// more than the tunnels it is monitoring; see [`WorkerStatus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "u8", into = "u8")]
pub struct WorkerCount(u8);

impl WorkerCount {
    pub fn new(count: u8) -> Result<Self, InvalidSettingValue> {
        if (MIN_WORKERS..=MAX_WORKERS).contains(&count) {
            Ok(Self(count))
        } else {
            Err(invalid("worker count", "must be 2 to 17"))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for WorkerCount {
    type Error = InvalidSettingValue;

    fn try_from(count: u8) -> Result<Self, InvalidSettingValue> {
        Self::new(count)
    }
}

impl From<WorkerCount> for u8 {
    fn from(count: WorkerCount) -> u8 {
        count.0
    }
}

/// An IANA time zone name such as `Asia/Taipei` or `UTC`: 1 to 64 bytes of
/// ASCII letters, digits and `/ _ + -`, starting with a letter, with no
/// empty segment between slashes and no trailing slash. The form keeps it
/// from ever being a file path. Whether the zone exists is the watchdog's
/// check, against its own time zone database.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct TimeZoneName(String);

impl TimeZoneName {
    /// Maximum length in bytes.
    pub const MAX_BYTES: usize = 64;

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidSettingValue> {
        let value = value.into();
        let reason = if value.is_empty() || value.len() > Self::MAX_BYTES {
            "must be 1 to 64 bytes"
        } else if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'+' | b'-'))
        {
            "only ASCII letters, digits, '/', '_', '+' and '-' are allowed"
        } else if !value.as_bytes()[0].is_ascii_alphabetic() {
            "must start with a letter"
        } else if value.ends_with('/') || value.contains("//") {
            "must not have an empty segment"
        } else {
            return Ok(Self(value));
        };
        Err(invalid("time zone name", reason))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TimeZoneName {
    type Error = InvalidSettingValue;

    fn try_from(value: String) -> Result<Self, InvalidSettingValue> {
        Self::new(value)
    }
}

impl From<TimeZoneName> for String {
    fn from(name: TimeZoneName) -> String {
        name.0
    }
}

impl fmt::Display for TimeZoneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The oldest GUI version that has every function of this watchdog, reported
/// in every snapshot (`HostStatus::min_gui_version`). A minimum is what makes
/// every function work, not what can merely connect (owner decision of
/// 2026-11-04): a GUI below it says it is too old for the full function.
/// Raise it with any watchdog change an older GUI cannot show or operate;
/// wording, a new language or a fix on either side never needs it. 2.0.0:
/// WGAL started again at 2.0.0 before it had users (owner decision of
/// 2026-11-06), so no 1.x GUI is supported. 2.1.0: the device name holds
/// 64 characters (`DeviceName`), which a 2.0.0 GUI cannot decode beyond 96
/// bytes; 2.0.0 was never installed. 3.0.1: the event log's
/// `agent_request` lines may name `RequestOp::SetCliLanguage` (watchdog
/// 4.0.0), a value of an answer's enum that older GUIs do not have (owner
/// decision of 2026-11-19). 3.1.0: the snapshot carries the host name
/// messages give when no device name is set (`NotificationStatus::host_name`,
/// watchdog 4.1.0), which the GUI shows (owner decision of 2026-11-19).
/// 4.2.1: the window, the watchdog, `wgal-mcp` and `wgal` share one version
/// from 4.2.1, and every minimum starts there (owner decision of
/// 2026-11-19, a one-time sync).
pub const MIN_GUI_VERSION: &str = "4.2.1";

/// The oldest GUI version that can still use this watchdog's tunnel
/// functions at all (`HostStatus::min_usable_gui_version`): below it the GUI
/// says it is not supported. At most [`MIN_GUI_VERSION`]. 2.1.0, as above:
/// a 2.0.0 GUI may not read the snapshot at all.
/// 4.2.1: the window, the watchdog, `wgal-mcp` and `wgal` share one version
/// from 4.2.1, and every minimum starts there (owner decision of
/// 2026-11-19, a one-time sync).
pub const MIN_USABLE_GUI_VERSION: &str = "4.2.1";

/// A product version such as `1.0.0`: 1 to 32 bytes of ASCII letters,
/// digits and `. + -`, starting with a digit. The GUI and the watchdog each
/// have one and raise it on their own; they only need each other to be at
/// least a minimum ([`MIN_GUI_VERSION`] here, `MIN_WATCHDOG_VERSION` in the
/// GUI), compared with [`ProductVersion::is_at_least`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct ProductVersion(String);

impl ProductVersion {
    /// Maximum length in bytes.
    pub const MAX_BYTES: usize = 32;

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidSettingValue> {
        let value = value.into();
        let reason = if value.is_empty() || value.len() > Self::MAX_BYTES {
            "must be 1 to 32 bytes"
        } else if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        {
            "only ASCII letters, digits, '.', '+' and '-' are allowed"
        } else if !value.as_bytes()[0].is_ascii_digit() {
            "must start with a digit"
        } else {
            return Ok(Self(value));
        };
        Err(invalid("product version", reason))
    }

    /// The version this crate, and so the watchdog's workspace, was built
    /// as.
    pub fn current() -> Self {
        Self(env!("CARGO_PKG_VERSION").to_owned())
    }

    /// [`MIN_GUI_VERSION`].
    pub fn min_gui() -> Self {
        Self(MIN_GUI_VERSION.to_owned())
    }

    /// [`MIN_USABLE_GUI_VERSION`].
    pub fn min_usable_gui() -> Self {
        Self(MIN_USABLE_GUI_VERSION.to_owned())
    }

    /// Whether this version is `minimum` or newer, comparing the
    /// dot-separated numbers at the start (`1.10.0` is newer than `1.9.3`; a
    /// missing number counts as 0) and ignoring a pre-release or build
    /// suffix after them.
    pub fn is_at_least(&self, minimum: &Self) -> bool {
        let (mut have, mut need) = (numbers(&self.0), numbers(&minimum.0));
        let len = have.len().max(need.len());
        have.resize(len, 0);
        need.resize(len, 0);
        have >= need
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The dot-separated numbers a version starts with, before any `-` or `+`
/// suffix.
fn numbers(version: &str) -> Vec<u64> {
    version
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

impl TryFrom<String> for ProductVersion {
    type Error = InvalidSettingValue;

    fn try_from(value: String) -> Result<Self, InvalidSettingValue> {
        Self::new(value)
    }
}

impl From<ProductVersion> for String {
    fn from(version: ProductVersion) -> String {
        version.0
    }
}

impl fmt::Display for ProductVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host-wide settings the GUI may change, sent in
/// `Request::UpdateHostSettings`.
///
/// `workers` is how many worker threads probe and recover tunnels; `null`
/// is automatic, the watchdog's suggestion. `time_zone` is the zone that
/// Telegram messages and log times are written in; `null` follows the
/// system's zone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct HostSettings {
    pub workers: Option<WorkerCount>,
    pub time_zone: Option<TimeZoneName>,
}

/// The host settings and what the watchdog makes of them, reported in every
/// snapshot, with `watchdog_version`, the version of the watchdog that made
/// the snapshot (what `wgal-watchdog --version` prints),
/// `min_gui_version`, the oldest GUI with every function
/// ([`MIN_GUI_VERSION`]), and `min_usable_gui_version`, the oldest that can
/// still use the tunnel functions ([`MIN_USABLE_GUI_VERSION`]; `null` from a
/// watchdog older than 1.2.0, which had only the one minimum), and
/// `mcp_enabled`, whether agents may use WGAL through `wgal-mcp`, and
/// `mcp_telegram_enabled`, whether they may also read the chat ID and
/// change the device name, chat ID and bot token
/// (`Request::SetMcpTelegramEnabled`; `false` from a watchdog older than
/// 1.7.0, which never lets them).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct HostStatus {
    pub watchdog_version: ProductVersion,
    pub min_gui_version: ProductVersion,
    #[serde(default)]
    pub min_usable_gui_version: Option<ProductVersion>,
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default)]
    pub mcp_telegram_enabled: bool,
    pub workers: WorkerStatus,
    pub time_zone: TimeZoneStatus,
}

/// Worker threads as the GUI shows them.
///
/// `configured` is the user's choice, `null` for automatic. `suggested` is
/// the count the watchdog computes for the tunnels it monitors now, used
/// when `configured` is `null`. `limit` is the number of tunnels being
/// monitored plus one, at least [`MIN_WORKERS`]; paused tunnels do not
/// count. `effective` is `configured`, or else `suggested`, clamped to
/// `MIN_WORKERS..=limit`: the most worker threads the watchdog runs now.
/// `running` is how many worker threads exist now, which may be fewer:
/// threads are started only when work needs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct WorkerStatus {
    pub configured: Option<WorkerCount>,
    pub suggested: u8,
    pub limit: u8,
    pub effective: u8,
    pub running: u8,
}

/// The time zone that Telegram messages and log times are written in.
///
/// `configured` is the user's choice, `null` to follow the system.
/// `effective` is the IANA name times are written in: the configured one,
/// or else the system's. It is `null` when the system zone has no IANA
/// name; the GUI then uses `utc_offset_secs`, the zone's offset from UTC
/// now, which is within ±14 hours.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TimeZoneStatus {
    pub configured: Option<TimeZoneName>,
    pub effective: Option<TimeZoneName>,
    pub utc_offset_secs: i32,
}

/// What the `wgal-mcp` executable carries right before its version, which
/// a NUL byte ends, so the window can tell which MCP server is installed
/// beside it without running it (owner request of 2026-11-04): the window
/// holds no power to start programs (AGENTS.md rule 9).
pub const MCP_VERSION_MARK: &str = "WGAL-MCP-VERSION=";

/// The version a `wgal-mcp` executable's bytes carry after
/// [`MCP_VERSION_MARK`]: the first mark followed by a valid
/// [`ProductVersion`] and a NUL byte, so a stray copy of the mark itself is
/// passed over. `None` when there is none.
pub fn mcp_version_in(bytes: &[u8]) -> Option<ProductVersion> {
    let mark = MCP_VERSION_MARK.as_bytes();
    let mut rest = bytes;
    while let Some(at) = rest.windows(mark.len()).position(|w| w == mark) {
        rest = &rest[at + mark.len()..];
        let tail = &rest[..rest.len().min(ProductVersion::MAX_BYTES + 1)];
        if let Some(end) = tail.iter().position(|&b| b == 0)
            && let Ok(text) = std::str::from_utf8(&tail[..end])
            && let Ok(version) = ProductVersion::new(text)
        {
            return Some(version);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mcp_version_is_found_after_its_mark() {
        let found = |bytes: &[u8]| mcp_version_in(bytes).map(|v| v.as_str().to_owned());
        assert_eq!(
            found(b"MZ\x90junk\0WGAL-MCP-VERSION=1.2.1\0more"),
            Some("1.2.1".into())
        );
        // A stray copy of the mark, then the real one.
        assert_eq!(
            found(b"WGAL-MCP-VERSION=\x01\x02WGAL-MCP-VERSION=1.2.1\0"),
            Some("1.2.1".into())
        );
        for bad in [
            &b"no mark here"[..],
            b"WGAL-MCP-VERSION=",
            b"WGAL-MCP-VERSION=1.2.1",
            b"WGAL-MCP-VERSION=v1.2\0",
            b"WGAL-MCP-VERSION=1.2.1 \0",
            b"WGAL-MCP-VERSION=123456789012345678901234567890123\0",
            b"",
        ] {
            assert_eq!(found(bad), None, "{bad:?}");
        }
    }
    use crate::status::MAX_TUNNELS;

    fn timing(interval: u16, timeout: u8, retries: u8, min_fault: u16) -> ProbeTiming {
        ProbeTiming::new(interval, timeout, retries, min_fault).unwrap()
    }

    #[test]
    fn default_timing_is_the_plans() {
        let d = ProbeTiming::default();
        assert_eq!(d, ProbeTiming::DEFAULT);
        assert_eq!(d, timing(15, 2, 2, 60));
        assert_eq!(d.failures_needed(), 3);
        assert_eq!(d.rounds_to_confirm(), 4);
        assert_eq!(d.freshness_secs(), 35);
    }

    #[test]
    fn plan_example_three_failures_in_30_s_do_not_confirm() {
        // Plan §6.2: first failure at t = 0, a round every 15 s. Rounds 0, 1
        // and 2 (t = 30) are three failures but not 60 s; round 4 (t = 60)
        // is the first that meets both conditions.
        let d = ProbeTiming::DEFAULT;
        let interval = u32::from(d.interval_secs());
        let enough_failures = d.failures_needed() - 1;
        assert_eq!(enough_failures * interval, 30);
        assert!(enough_failures < d.rounds_to_confirm());
        assert_eq!(d.rounds_to_confirm() * interval, 60);
    }

    #[test]
    fn rounds_to_confirm_takes_the_slower_condition() {
        // Zero and zero: the first failed round confirms.
        let t = timing(10, 1, 0, 0);
        assert_eq!((t.failures_needed(), t.rounds_to_confirm()), (1, 0));
        // The retries are the slower condition.
        let t = timing(10, 2, 10, 60);
        assert_eq!((t.failures_needed(), t.rounds_to_confirm()), (11, 10));
        // The minimum fault time is, and it is rounded up to whole rounds.
        assert_eq!(timing(15, 2, 2, 61).rounds_to_confirm(), 5);
        assert_eq!(timing(15, 2, 0, 1).rounds_to_confirm(), 1);
        assert_eq!(timing(3600, 2, 0, 3600).rounds_to_confirm(), 1);
    }

    #[test]
    fn freshness_follows_interval_and_timeout() {
        assert_eq!(timing(10, 1, 0, 0).freshness_secs(), 29);
        assert_eq!(timing(3600, 10, 10, 3600).freshness_secs(), 3628);
    }

    #[test]
    fn probe_timing_accepts_its_ranges_and_nothing_else() {
        assert!(ProbeTiming::new(10, 1, 0, 0).is_ok());
        assert!(ProbeTiming::new(3600, 10, 10, 3600).is_ok());
        for (interval, timeout, retries, min_fault) in [
            (9, 2, 2, 60),
            (3601, 2, 2, 60),
            (0, 2, 2, 60),
            (15, 0, 2, 60),
            (15, 11, 2, 60),
            (15, 2, 11, 60),
            (15, 2, 2, 3601),
            (u16::MAX, u8::MAX, u8::MAX, u16::MAX),
        ] {
            assert!(
                ProbeTiming::new(interval, timeout, retries, min_fault).is_err(),
                "{interval} {timeout} {retries} {min_fault} should be rejected"
            );
        }
    }

    #[test]
    fn probe_timing_deserialization_applies_validation() {
        let json = |i: u32, t: u32, r: u32, m: u32| {
            format!(
                r#"{{"interval_secs":{i},"timeout_secs":{t},"retries":{r},"min_fault_secs":{m}}}"#
            )
        };
        let ok: ProbeTiming = serde_json::from_str(&json(15, 2, 2, 60)).unwrap();
        assert_eq!(ok, ProbeTiming::DEFAULT);
        assert_eq!(serde_json::to_string(&ok).unwrap(), json(15, 2, 2, 60));
        for bad in [
            json(9, 2, 2, 60),
            json(15, 0, 2, 60),
            json(15, 2, 11, 60),
            json(15, 2, 2, 3601),
            json(70_000, 2, 2, 60),
            r#"{"interval_secs":15,"timeout_secs":2,"retries":2}"#.to_owned(),
            r#"{"interval_secs":15,"timeout_secs":2,"retries":2,"min_fault_secs":60,"x":1}"#
                .to_owned(),
        ] {
            assert!(
                serde_json::from_str::<ProbeTiming>(&bad).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn max_workers_is_one_per_tunnel_plus_one() {
        assert_eq!(usize::from(MAX_WORKERS), MAX_TUNNELS + 1);
    }

    #[test]
    fn worker_count_accepts_2_to_17() {
        for ok in MIN_WORKERS..=MAX_WORKERS {
            assert_eq!(WorkerCount::new(ok).unwrap().get(), ok);
        }
        for bad in [0, 1, 18, u8::MAX] {
            assert!(WorkerCount::new(bad).is_err(), "{bad} should be rejected");
        }
        assert_eq!(serde_json::from_str::<WorkerCount>("2").unwrap().get(), 2);
        assert!(serde_json::from_str::<WorkerCount>("1").is_err());
        assert!(serde_json::from_str::<WorkerCount>("18").is_err());
        assert!(serde_json::from_str::<WorkerCount>("256").is_err());
    }

    #[test]
    fn time_zone_name_accepts_iana_names() {
        let longest = format!("A{}", "b".repeat(TimeZoneName::MAX_BYTES - 1));
        for ok in [
            "UTC",
            "Asia/Taipei",
            "Etc/GMT+8",
            "Etc/GMT-14",
            "America/Argentina/Buenos_Aires",
            "America/Port-au-Prince",
            longest.as_str(),
        ] {
            assert!(TimeZoneName::new(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn time_zone_name_rejects_anything_path_like() {
        let too_long = "a".repeat(TimeZoneName::MAX_BYTES + 1);
        for bad in [
            "",
            "../x",
            "/etc",
            "/etc/localtime",
            "C:\\x",
            "C:/x",
            "a b",
            "Asia/",
            "Asia//Taipei",
            "Asia/./Taipei",
            "1Asia",
            "_UTC",
            "Asia/Taipei\n",
            "Asia/Taipéi",
            too_long.as_str(),
        ] {
            assert!(
                TimeZoneName::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn time_zone_name_deserialization_applies_validation() {
        assert!(serde_json::from_str::<TimeZoneName>(r#""Asia/Taipei""#).is_ok());
        assert!(serde_json::from_str::<TimeZoneName>(r#""../etc/passwd""#).is_err());
    }

    #[test]
    fn host_settings_default_to_automatic() {
        let json = r#"{"workers":null,"time_zone":null}"#;
        let settings: HostSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings, HostSettings::default());
        let bad = r#"{"workers":1,"time_zone":null}"#;
        assert!(serde_json::from_str::<HostSettings>(bad).is_err());
    }

    #[test]
    fn product_version_accepts_release_and_pre_release_forms() {
        for ok in ["1.0.0", "1.2.3-rc.1+build.5", "10.20.30"] {
            assert_eq!(ProductVersion::new(ok).unwrap().as_str(), ok);
        }
        assert_eq!(
            ProductVersion::current().as_str(),
            env!("CARGO_PKG_VERSION")
        );
        let longest = "9".repeat(ProductVersion::MAX_BYTES);
        assert!(ProductVersion::new(longest.clone()).is_ok());
        for bad in [
            "", "v1.0.0", "1.0 .0", "../1", "1/0", "1.0.0
",
        ] {
            assert!(ProductVersion::new(bad).is_err(), "{bad:?}");
        }
        assert!(ProductVersion::new(longest + "9").is_err());
        assert!(serde_json::from_str::<ProductVersion>(r#""x1""#).is_err());
    }

    #[test]
    fn versions_compare_by_their_numbers() {
        let v = |s: &str| ProductVersion::new(s).unwrap();
        for (have, need) in [
            ("1.0.0", "1.0.0"),
            ("1.0.1", "1.0.0"),
            ("1.10.0", "1.9.3"),
            ("2.0.0", "1.99.99"),
            ("1.0", "1.0.0"),
            ("1.0.0-rc.1", "1.0.0"),
            ("1.2.0+build.7", "1.2"),
        ] {
            assert!(v(have).is_at_least(&v(need)), "{have} >= {need}");
        }
        for (have, need) in [("0.9.9", "1.0.0"), ("1.9.3", "1.10.0"), ("1.0.0", "1.0.1")] {
            assert!(!v(have).is_at_least(&v(need)), "{have} < {need}");
        }
        assert!(ProductVersion::new(MIN_GUI_VERSION).is_ok());
        assert!(ProductVersion::current().is_at_least(&ProductVersion::min_gui()));
    }
}
