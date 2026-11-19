//! A file of WGAL's settings (owner decision of 2026-11-04): exported by
//! `Request::ExportSettings`, read back by `Request::ImportSettings`, so a
//! user or an agent can keep a copy before changing something and put it
//! back, here or on another computer.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::bounded::BoundedVec;
use crate::ids::TunnelId;
use crate::ipc::ErrorCode;
use crate::ipc::ErrorReason;
use crate::probe::{MAX_OBSERVE_TARGETS, ProbeTarget};
use crate::settings::{ProbeTiming, ProductVersion, RecoveryRetry, TimeZoneName, WorkerCount};
use crate::status::MAX_TUNNELS;
use crate::text::DisplayName;

/// The only version of [`SettingsDocument`] this contract reads and writes.
pub const SETTINGS_DOCUMENT_VERSION: u32 = 1;

/// A settings file was rejected, saying why in words a person or an agent
/// can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidDocument(String);

impl fmt::Display for InvalidDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidDocument {}

/// What a settings file says it is: always `wgal-settings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub enum SettingsFormat {
    #[serde(rename = "wgal-settings")]
    WgalSettings,
}

/// WGAL's settings, as a file to keep and read back (owner decision of
/// 2026-11-04).
///
/// `format` is always `wgal-settings` and `version`
/// [`SETTINGS_DOCUMENT_VERSION`]; a file of another version is refused, not
/// guessed at. `watchdog_version` and `exported_at_unix_ms` say where it came
/// from and are not applied. `host` has the host-wide settings, `tunnels` one
/// entry per registered tunnel.
///
/// It never holds the Bot Token, the Chat ID or the device's display name:
/// the token and the chat are secrets, and the name is the computer's own,
/// so reading the file elsewhere must not make that computer's messages look
/// like this one's. The GUI puts the device name in the file's name only.
/// Every field is checked as in the requests that set it, and a field the
/// contract does not have is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "SettingsDocumentFields")]
pub struct SettingsDocument {
    pub format: SettingsFormat,
    pub version: u32,
    pub watchdog_version: ProductVersion,
    #[cfg_attr(test, ts(type = "number"))]
    pub exported_at_unix_ms: i64,
    pub host: HostSettingsEntry,
    #[cfg_attr(test, ts(as = "Vec<TunnelSettingsEntry>"))]
    pub tunnels: BoundedVec<TunnelSettingsEntry, MAX_TUNNELS>,
}

// The wire form of `SettingsDocument`, before its version is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsDocumentFields {
    format: SettingsFormat,
    version: u32,
    watchdog_version: ProductVersion,
    exported_at_unix_ms: i64,
    host: HostSettingsEntry,
    tunnels: BoundedVec<TunnelSettingsEntry, MAX_TUNNELS>,
}

impl TryFrom<SettingsDocumentFields> for SettingsDocument {
    type Error = InvalidDocument;

    fn try_from(f: SettingsDocumentFields) -> Result<Self, InvalidDocument> {
        if f.version != SETTINGS_DOCUMENT_VERSION {
            return Err(InvalidDocument(format!(
                "this settings file is version {}; this WGAL reads version {SETTINGS_DOCUMENT_VERSION} only",
                f.version
            )));
        }
        Ok(Self {
            format: f.format,
            version: f.version,
            watchdog_version: f.watchdog_version,
            exported_at_unix_ms: f.exported_at_unix_ms,
            host: f.host,
            tunnels: f.tunnels,
        })
    }
}

/// The host-wide part of a settings file: the worker threads and time zone
/// (`null` for automatic, as in `HostSettings`), and whether Telegram
/// notifications are on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct HostSettingsEntry {
    pub workers: Option<WorkerCount>,
    pub time_zone: Option<TimeZoneName>,
    pub notifications_enabled: bool,
}

/// One tunnel in a settings file: its ID and display name on the computer
/// that wrote it (what an import matches it by), whether it is monitored,
/// may notify, recovers automatically and is started when stopped
/// externally, and its targets, probe timing and recovery retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct TunnelSettingsEntry {
    pub tunnel_id: TunnelId,
    pub name: DisplayName,
    pub monitoring: bool,
    pub notifications: bool,
    pub auto_recovery: bool,
    pub start_when_stopped_externally: bool,
    pub recovery_target: Option<ProbeTarget>,
    #[cfg_attr(test, ts(as = "Vec<ProbeTarget>"))]
    pub observe_targets: BoundedVec<ProbeTarget, MAX_OBSERVE_TARGETS>,
    pub probe: ProbeTiming,
    pub retry: RecoveryRetry,
}

/// How an import finds a file's tunnel on this computer: `name`, by the
/// official manager's display name (for another computer; a name that more
/// than one tunnel here has is not guessed at), or `id`, by the tunnel ID
/// (for this computer, where names may repeat).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum ImportMatch {
    Name,
    Id,
}

/// How to import (owner decision of 2026-11-04): `match_by` (see
/// [`ImportMatch`]); `register_missing`, also register a tunnel the
/// official manager has but WGAL does not monitor yet (within the
/// installation's access list); `monitor_new`, turn monitoring on for a
/// tunnel registered by this import as the file says, instead of leaving it
/// off; `dry_run`, change nothing and say what would happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct ImportOptions {
    pub match_by: ImportMatch,
    pub register_missing: bool,
    pub monitor_new: bool,
    pub dry_run: bool,
}

/// What became of one part of an import (or would, in a dry run):
/// `applied`, its settings were set; `registered`, the tunnel was
/// registered and its settings set; `not_found`, no tunnel here has that
/// name or ID; `ambiguous`, more than one does; `duplicate_in_file`, the
/// file has another entry for the same name or ID, so neither is used;
/// `not_registered`, the tunnel is here but WGAL does not monitor it and
/// `register_missing` was off; `unsupported`, the official manager does not
/// let WGAL control it; `refused`, the watchdog refused a change, with the
/// code and reason it gave. Nothing is set for any but the first two.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportOutcome {
    Applied,
    Registered,
    NotFound,
    Ambiguous,
    DuplicateInFile,
    NotRegistered,
    Unsupported,
    Refused {
        code: ErrorCode,
        reason: Option<ErrorReason>,
    },
}

/// What became of a tunnel's automatic recovery in an import: `off`, the
/// file has it off; `on`, it is on; `waiting_for_baseline`, it turns on once
/// the recovery target passes its baseline check, if that happens within
/// ten minutes (owner decision of 2026-11-04), otherwise it stays off;
/// `refused`, the watchdog refused it (for example the tunnel is not on the
/// installation's access list).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoRecoveryImport {
    Off,
    On,
    WaitingForBaseline,
    Refused {
        code: ErrorCode,
        reason: Option<ErrorReason>,
    },
}

/// One file entry's result: `index` its position in the file (from 0),
/// `name` its name there, `tunnel_id` the tunnel here it was matched to,
/// `outcome` and `auto_recovery` as above.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ImportItem {
    pub index: u16,
    pub name: DisplayName,
    pub tunnel_id: Option<TunnelId>,
    pub outcome: ImportOutcome,
    pub auto_recovery: AutoRecoveryImport,
}

/// What an import did, or would do when `dry_run`: `host`, the host
/// settings (worker threads and time zone); `notifications`, the
/// notification switch; and each tunnel entry in the file's order. The
/// switch is stored apart from the host settings, so each has its own
/// outcome and is set whole or not at all (Codex P2-N32); `notifications`
/// is `null` from a watchdog that did not report it apart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ImportResult {
    pub dry_run: bool,
    pub host: ImportOutcome,
    #[serde(default)]
    pub notifications: Option<ImportOutcome>,
    #[cfg_attr(test, ts(as = "Vec<ImportItem>"))]
    pub tunnels: BoundedVec<ImportItem, MAX_TUNNELS>,
}
