//! Parsers for the `systemctl` output the adapter reads.
//!
//! Every parser here fails on anything it does not recognise: an unknown key
//! or value, a duplicate, a missing key, or truncated output. A failure is a
//! `ParseError`, never a guess at the tunnel's state.

use std::collections::BTreeMap;

use wgal_core::AdapterError;
use wgal_protocol::{AdapterErrorKind, OsState};

use super::name::InterfaceName;

/// The properties `systemctl show` is asked for, comma-separated as passed in
/// `--property=`. The parser expects exactly these keys, each once.
pub const SHOW_PROPERTIES: &str = "LoadState,ActiveState,SubState,FragmentPath,DropInPaths";

const KEYS: [&str; 5] = [
    "LoadState",
    "ActiveState",
    "SubState",
    "FragmentPath",
    "DropInPaths",
];

/// Most bytes a `systemctl show` answer for these properties can need.
pub const MAX_SHOW_BYTES: usize = 16 * 1024;

fn parse_error(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::ParseError, detail)
}

/// The output as text. systemctl ends every line with a newline, so output
/// that does not end with one was cut short and is rejected; empty output is
/// allowed only where the caller says so.
fn complete_text<'a>(
    stdout: &'a [u8],
    tool: &str,
    empty_ok: bool,
) -> Result<&'a str, AdapterError> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| parse_error(&format!("{tool} output is not UTF-8")))?;
    if text.is_empty() && empty_ok {
        return Ok(text);
    }
    if !text.ends_with('\n') {
        return Err(parse_error(&format!("{tool} output is incomplete")));
    }
    Ok(text)
}

/// `LoadState` values systemd documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    Loaded,
    NotFound,
    BadSetting,
    Error,
    Merged,
    Stub,
    Masked,
}

impl LoadState {
    /// Why a unit in this load state cannot be used, or `None` for `loaded`
    /// and `masked`, whose active state still tells what the tunnel is doing.
    pub fn error(self) -> Option<AdapterError> {
        match self {
            Self::Loaded | Self::Masked => None,
            Self::NotFound => Some(AdapterError::new(
                AdapterErrorKind::NotFound,
                "systemd has no such wg-quick unit",
            )),
            Self::BadSetting | Self::Error => Some(AdapterError::new(
                AdapterErrorKind::ManagerConflict,
                "systemd could not load the wg-quick unit",
            )),
            Self::Merged | Self::Stub => {
                Some(parse_error("wg-quick unit is in an unexpected load state"))
            }
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "loaded" => Self::Loaded,
            "not-found" => Self::NotFound,
            "bad-setting" => Self::BadSetting,
            "error" => Self::Error,
            "merged" => Self::Merged,
            "stub" => Self::Stub,
            "masked" => Self::Masked,
            _ => return None,
        })
    }
}

/// `ActiveState` values systemd documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Maintenance,
    Refreshing,
}

impl ActiveState {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "active" => Self::Active,
            "reloading" => Self::Reloading,
            "inactive" => Self::Inactive,
            "failed" => Self::Failed,
            "activating" => Self::Activating,
            "deactivating" => Self::Deactivating,
            "maintenance" => Self::Maintenance,
            "refreshing" => Self::Refreshing,
            _ => return None,
        })
    }
}

/// Service `SubState` values systemd documents.
const SERVICE_SUB_STATES: [&str; 26] = [
    "dead",
    "condition",
    "start-pre",
    "start",
    "start-post",
    "running",
    "exited",
    "reload",
    "reload-signal",
    "reload-notify",
    "mounting",
    "stop",
    "stop-watchdog",
    "stop-sigterm",
    "stop-sigkill",
    "stop-post",
    "final-watchdog",
    "final-sigterm",
    "final-sigkill",
    "failed",
    "dead-before-auto-restart",
    "failed-before-auto-restart",
    "dead-resources-pinned",
    "auto-restart",
    "auto-restart-queued",
    "cleaning",
];

/// The parsed answer of `systemctl show <unit> --property=SHOW_PROPERTIES`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitStatus {
    pub load: LoadState,
    pub active: ActiveState,
    pub sub: String,
    /// The unit file systemd loaded, if any.
    pub fragment_path: Option<String>,
    /// Drop-in files that change the unit, in systemd's order.
    pub drop_in_paths: Vec<String>,
}

impl UnitStatus {
    pub fn parse(stdout: &[u8]) -> Result<Self, AdapterError> {
        let text = complete_text(stdout, "systemctl show", false)?;
        let mut values = BTreeMap::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| parse_error("systemctl show line is not Key=Value"))?;
            if !KEYS.contains(&key) {
                return Err(parse_error("systemctl show returned an unexpected key"));
            }
            if values.insert(key, value).is_some() {
                return Err(parse_error("systemctl show repeated a key"));
            }
        }
        let get = |key| {
            values
                .get(key)
                .copied()
                .ok_or_else(|| parse_error("systemctl show is missing a key"))
        };

        let load = LoadState::parse(get("LoadState")?)
            .ok_or_else(|| parse_error("unknown systemd LoadState"))?;
        let active = ActiveState::parse(get("ActiveState")?)
            .ok_or_else(|| parse_error("unknown systemd ActiveState"))?;
        let sub = get("SubState")?;
        if !SERVICE_SUB_STATES.contains(&sub) {
            return Err(parse_error("unknown systemd SubState"));
        }
        let fragment = get("FragmentPath")?;
        let drop_ins = get("DropInPaths")?;
        Ok(Self {
            load,
            active,
            sub: sub.to_owned(),
            fragment_path: (!fragment.is_empty()).then(|| fragment.to_owned()),
            drop_in_paths: drop_ins
                .split(' ')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect(),
        })
    }

    /// The manager's view of the tunnel.
    ///
    /// wg-quick units are `Type=oneshot` with `RemainAfterExit=yes`, so an up
    /// tunnel is `active (exited)`; that is `Connected`, not a stopped
    /// service. Whether its interface still exists is a data-path question
    /// answered by `resolve_interface`, not by this mapping. `failed` is a
    /// unit that did not come up or went down with an error: `Disconnected`
    /// as far as the manager is concerned. A unit systemd cannot find is
    /// `NotFound` (the binding is gone), never `Disconnected`.
    pub fn os_state(&self) -> Result<OsState, AdapterError> {
        if let Some(error) = self.load.error() {
            return Err(error);
        }
        Ok(match self.active {
            ActiveState::Active if matches!(self.sub.as_str(), "exited" | "running") => {
                OsState::Connected
            }
            ActiveState::Active
            | ActiveState::Reloading
            | ActiveState::Refreshing
            | ActiveState::Activating
            | ActiveState::Deactivating
            | ActiveState::Maintenance => OsState::Transitioning,
            ActiveState::Inactive | ActiveState::Failed => OsState::Disconnected,
        })
    }
}

/// Interfaces named by the `wg-quick@<interface>.service` lines of
/// `systemctl list-units --all --type=service --plain --no-legend --full`
/// (columns UNIT LOAD ACTIVE SUB DESCRIPTION). Units systemd cannot find are
/// left out: they are leftovers, not tunnels. A unit whose instance is not a
/// valid interface name is skipped, since no adapter call could name it.
pub fn parse_list_units(stdout: &[u8]) -> Result<Vec<InterfaceName>, AdapterError> {
    let text = complete_text(stdout, "systemctl list-units", true)?;
    let mut names = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let mut unit = fields.next().unwrap_or_default();
        // Some systemd versions keep the status bullet even with --plain.
        if unit == "\u{25cf}" {
            unit = fields.next().unwrap_or_default();
        }
        let (Some(load), Some(active), Some(sub)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(parse_error("systemctl list-units line has too few columns"));
        };
        if !unit.starts_with("wg-quick@") || !unit.ends_with(".service") {
            return Err(parse_error(
                "systemctl list-units returned an unrequested unit",
            ));
        }
        let load = LoadState::parse(load)
            .ok_or_else(|| parse_error("unknown systemd LoadState in list-units"))?;
        if ActiveState::parse(active).is_none() || !SERVICE_SUB_STATES.contains(&sub) {
            return Err(parse_error("unknown systemd state in list-units"));
        }
        if load == LoadState::NotFound {
            continue;
        }
        if let Some(name) = InterfaceName::from_unit(unit) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Interfaces with an instance unit in `systemctl list-unit-files
/// --type=service --plain --no-legend wg-quick@*` (columns UNIT-FILE STATE
/// and, on newer systemd, PRESET). The `wg-quick@.service` template itself is
/// skipped.
pub fn parse_list_unit_files(stdout: &[u8]) -> Result<Vec<InterfaceName>, AdapterError> {
    const STATES: [&str; 13] = [
        "enabled",
        "enabled-runtime",
        "linked",
        "linked-runtime",
        "alias",
        "masked",
        "masked-runtime",
        "static",
        "disabled",
        "indirect",
        "generated",
        "transient",
        "bad",
    ];
    let text = complete_text(stdout, "systemctl list-unit-files", true)?;
    let mut names = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [unit, state, rest @ ..] = fields.as_slice() else {
            return Err(parse_error(
                "systemctl list-unit-files line has too few columns",
            ));
        };
        if rest.len() > 1 || !STATES.contains(state) {
            return Err(parse_error("unexpected systemctl list-unit-files line"));
        }
        if !unit.starts_with("wg-quick@") || !unit.ends_with(".service") {
            return Err(parse_error(
                "systemctl list-unit-files returned an unrequested unit",
            ));
        }
        if *unit == "wg-quick@.service" {
            continue;
        }
        if let Some(name) = InterfaceName::from_unit(unit) {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    fn status(name: &str) -> Result<UnitStatus, AdapterError> {
        UnitStatus::parse(fixture(name).as_bytes())
    }

    fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
        result.unwrap_err().kind
    }

    #[test]
    fn active_exited_oneshot_is_connected_not_a_fault() {
        let unit = status("systemctl-show-active-exited.txt").unwrap();
        assert_eq!(unit.load, LoadState::Loaded);
        assert_eq!(unit.active, ActiveState::Active);
        assert_eq!(unit.sub, "exited");
        assert_eq!(
            unit.fragment_path.as_deref(),
            Some("/usr/lib/systemd/system/wg-quick@.service")
        );
        assert!(unit.drop_in_paths.is_empty());
        assert_eq!(unit.os_state().unwrap(), OsState::Connected);
    }

    #[test]
    fn maps_each_documented_state_deliberately() {
        let cases = [
            ("systemctl-show-inactive.txt", OsState::Disconnected),
            ("systemctl-show-failed.txt", OsState::Disconnected),
            ("systemctl-show-activating.txt", OsState::Transitioning),
            ("systemctl-show-deactivating.txt", OsState::Transitioning),
        ];
        for (name, expected) in cases {
            assert_eq!(
                status(name).unwrap().os_state().unwrap(),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn a_unit_systemd_cannot_find_is_not_found_not_disconnected() {
        let unit = status("systemctl-show-not-found.txt").unwrap();
        assert_eq!(unit.fragment_path, None);
        assert_eq!(kind(unit.os_state()), AdapterErrorKind::NotFound);
    }

    #[test]
    fn reads_drop_in_paths() {
        let unit = status("systemctl-show-drop-ins.txt").unwrap();
        assert_eq!(
            unit.drop_in_paths,
            [
                "/etc/systemd/system/wg-quick@.service.d/10-order.conf",
                "/etc/systemd/system/wg-quick@wg-lab.service.d/20-local.conf",
            ]
        );
    }

    #[test]
    fn rejects_unknown_duplicate_missing_and_malformed_output() {
        for name in [
            "systemctl-show-unknown-state.txt",
            "systemctl-show-duplicate-key.txt",
            "systemctl-show-missing-key.txt",
            "systemctl-show-unexpected-key.txt",
        ] {
            assert_eq!(kind(status(name)), AdapterErrorKind::ParseError, "{name}");
        }
        for raw in ["", "LoadState loaded\n", "\u{0}", "garbage"] {
            assert_eq!(
                kind(UnitStatus::parse(raw.as_bytes())),
                AdapterErrorKind::ParseError,
                "{raw:?}"
            );
        }
        assert_eq!(
            kind(UnitStatus::parse(&[0xff, 0xfe])),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn every_truncation_of_show_output_is_rejected() {
        for name in [
            "systemctl-show-active-exited.txt",
            "systemctl-show-drop-ins.txt",
        ] {
            let full = fixture(name);
            for cut in 0..full.len() {
                let prefix = &full.as_bytes()[..cut];
                assert_eq!(
                    kind(UnitStatus::parse(prefix)),
                    AdapterErrorKind::ParseError,
                    "{name} cut at {cut}"
                );
            }
        }
        let units = fixture("systemctl-list-units.txt");
        assert_eq!(
            kind(parse_list_units(&units.as_bytes()[..units.len() - 1])),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn unloadable_units_are_a_manager_conflict() {
        let text = fixture("systemctl-show-inactive.txt")
            .replace("LoadState=loaded", "LoadState=bad-setting");
        let unit = UnitStatus::parse(text.as_bytes()).unwrap();
        assert_eq!(kind(unit.os_state()), AdapterErrorKind::ManagerConflict);
    }

    #[test]
    fn lists_wg_quick_units_and_skips_leftovers_and_unnamable_instances() {
        let names = parse_list_units(fixture("systemctl-list-units.txt").as_bytes()).unwrap();
        let names: Vec<_> = names.iter().map(InterfaceName::as_str).collect();
        assert_eq!(names, ["wg-home", "wg-lab", "wg-office"]);
    }

    #[test]
    fn lists_enabled_instances_without_the_template() {
        let names =
            parse_list_unit_files(fixture("systemctl-list-unit-files.txt").as_bytes()).unwrap();
        let names: Vec<_> = names.iter().map(InterfaceName::as_str).collect();
        assert_eq!(names, ["wg-office", "wg-spare"]);
    }

    #[test]
    fn list_parsers_reject_unrequested_units_and_short_lines() {
        let bad_units = [
            "ssh.service loaded active running OpenBSD Secure Shell server\n",
            "wg-quick@wg0.service loaded\n",
            "wg-quick@wg0.service loaded sideways exited x\n",
        ];
        for raw in bad_units {
            assert_eq!(
                kind(parse_list_units(raw.as_bytes())),
                AdapterErrorKind::ParseError,
                "{raw:?}"
            );
        }
        for raw in [
            "ssh.service enabled enabled\n",
            "wg-quick@wg0.service\n",
            "wg-quick@wg0.service sometimes enabled\n",
            "wg-quick@wg0.service enabled enabled extra\n",
        ] {
            assert_eq!(
                kind(parse_list_unit_files(raw.as_bytes())),
                AdapterErrorKind::ParseError,
                "{raw:?}"
            );
        }
        assert!(parse_list_units(b"").unwrap().is_empty());
        assert!(parse_list_unit_files(b"\n").unwrap().is_empty());
    }
}
