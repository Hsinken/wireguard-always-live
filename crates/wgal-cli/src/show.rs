//! What `wgal` prints for people, in the words of its language files
//! (`lang/`, English by default): the watchdog's state as short lines,
//! every time in the watchdog's configured zone (as its Telegram messages
//! give it), every state by its contract name (`green`, `fault_confirmed`),
//! the words the window's states are documented with, in every language.

use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use serde::Serialize;
use wgal_protocol::{
    AutoRecoveryImport, CredentialState, DiscoveredTunnel, EventEntry, ImportItem, ImportOutcome,
    ImportResult, Light, ProbeKind, ProbeResult, ProbeSummary, ProbeTarget, RecoveryRetry,
    Snapshot, TunnelStatus,
};

use crate::languages;
use crate::text::tr;

/// Times as the watchdog writes them: in its configured zone.
#[derive(Debug)]
pub struct Clock {
    zone: TimeZone,
}

impl Clock {
    /// The zone the snapshot says the watchdog uses: its name when this
    /// program knows it, otherwise its offset now.
    pub fn of(snapshot: &Snapshot) -> Self {
        let zone = &snapshot.host.time_zone;
        let named = zone
            .effective
            .as_ref()
            .and_then(|name| TimeZone::get(name.as_str()).ok());
        let zone = named.unwrap_or_else(|| {
            Offset::from_seconds(zone.utc_offset_secs).map_or(TimeZone::UTC, TimeZone::fixed)
        });
        Self { zone }
    }

    /// UTC, for tests.
    #[cfg(test)]
    pub fn utc() -> Self {
        Self {
            zone: TimeZone::UTC,
        }
    }

    /// `2026-11-18 20:01:02 +08:00`.
    pub fn at(&self, unix_ms: i64) -> String {
        Timestamp::from_millisecond(unix_ms).map_or_else(
            |_| format!("(unix ms {unix_ms})"),
            |t| {
                t.to_zoned(self.zone.clone())
                    .strftime("%Y-%m-%d %H:%M:%S %:z")
                    .to_string()
            },
        )
    }

    /// [`Self::at`], or `-` for none.
    pub fn maybe(&self, unix_ms: Option<i64>) -> String {
        unix_ms.map_or_else(|| "-".to_owned(), |ms| self.at(ms))
    }
}

/// A contract value by its wire name: an enum's `snake_case` name, a
/// tagged one's `kind`, anything else as compact JSON.
pub fn word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(serde_json::Value::Object(map)) => match map.get("kind") {
            Some(serde_json::Value::String(kind)) => {
                let rest: Vec<String> = map
                    .iter()
                    .filter(|(k, _)| *k != "kind")
                    .map(|(k, v)| format!("{k} {}", plain(v)))
                    .collect();
                if rest.is_empty() {
                    kind.clone()
                } else {
                    format!("{kind} ({})", rest.join(", "))
                }
            }
            _ => serde_json::Value::Object(map).to_string(),
        },
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => "-".to_owned(),
        other => other.to_string(),
    }
}

/// A switch's value, `on` or `off`, the same in every language: what the
/// commands take.
pub fn on(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// `192.0.2.1 (ICMP)` or `192.0.2.1:443 (TCP)`, as `set` takes it back.
pub fn target(t: &ProbeTarget) -> String {
    match t.probe {
        ProbeKind::Icmp => format!("{} (ICMP)", t.address.get()),
        ProbeKind::TcpConnect { port } => format!("{}:{port} (TCP)", t.address.get()),
    }
}

/// A probe's answer.
pub fn result(r: &ProbeResult) -> String {
    match r {
        ProbeResult::Reply { rtt_ms } => tr!("show.result.reply", ms = rtt_ms),
        ProbeResult::Refused { rtt_ms } => tr!("show.result.refused", ms = rtt_ms),
        ProbeResult::NoReply => tr!("show.result.no_reply"),
        ProbeResult::Error { error } => tr!("show.result.error", error = word(error)),
    }
}

/// Whether a probe's answer shows traffic through the tunnel.
pub fn reached(r: &ProbeResult) -> bool {
    matches!(r, ProbeResult::Reply { .. } | ProbeResult::Refused { .. })
}

fn last_probe(p: Option<&ProbeSummary>, clock: &Clock) -> String {
    p.map_or_else(
        || tr!("show.not_probed_yet"),
        |p| format!("{}, {}", result(&p.result), clock.at(p.finished_at_unix_ms)),
    )
}

/// The retry setting in words.
pub fn retry(r: &RecoveryRetry) -> String {
    match r.limit() {
        Some(limit) => tr!(
            "show.retry_limited",
            rounds = limit,
            seconds = r.interval_secs()
        ),
        None => tr!("show.retry_forever", seconds = r.interval_secs()),
    }
}

/// One line for a tunnel.
pub fn tunnel_line(t: &TunnelStatus) -> String {
    format!(
        "  {}",
        tr!(
            "show.tunnel_line",
            tunnel = t.tunnel_id.as_str(),
            name = t.display_name.as_str(),
            light = word(&t.light),
            reason = word(&t.light_reason),
            monitoring = on(t.intent.monitoring_enabled),
            os = word(&t.os_state),
            seconds = t.freshness_secs,
        )
    )
}

/// A tunnel in full.
pub fn tunnel_detail(t: &TunnelStatus, clock: &Clock) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} \"{}\"",
        t.tunnel_id.as_str(),
        t.display_name.as_str()
    );
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.light",
            light = word(&t.light),
            reason = word(&t.light_reason),
            since = clock.at(t.light_since_unix_ms)
        )
    );
    let _ = writeln!(
        out,
        "  {}",
        if t.light == Light::Green {
            tr!("show.traffic_verified")
        } else {
            tr!("show.traffic_not_verified")
        }
    );
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.os_state",
            os = word(&t.os_state),
            wanted = word(&t.intent.desired),
            recovery = word(&t.recovery)
        )
    );
    let auto_recovery = on(t.intent.auto_recovery_enabled);
    let auto_recovery = if t.auto_recovery_pending {
        tr!("show.auto_recovery_pending", state = auto_recovery)
    } else if t.intent.auto_recovery_enabled && !t.auto_recovery_allowed {
        tr!("show.auto_recovery_not_allowed", state = auto_recovery)
    } else {
        auto_recovery.to_owned()
    };
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.switches",
            monitoring = on(t.intent.monitoring_enabled),
            auto_recovery = auto_recovery,
            start_when_stopped = on(t.intent.start_when_stopped_externally),
            notifications = on(t.notifications_enabled)
        )
    );
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.probe",
            interval = t.probe.interval_secs(),
            timeout = t.probe.timeout_secs(),
            retries = t.probe.retries(),
            min_fault = t.probe.min_fault_secs()
        )
    );
    let _ = writeln!(out, "  {}", tr!("show.retry", retry = retry(&t.retry)));
    let _ = writeln!(
        out,
        "  {}",
        match &t.recovery_target {
            Some(r) => tr!(
                "show.recovery_target",
                target = target(&r.target),
                last_probe = last_probe(r.last_probe.as_ref(), clock)
            ),
            None => tr!("show.recovery_target_none"),
        }
    );
    for o in t.observe_targets.iter() {
        let _ = writeln!(
            out,
            "  {}",
            tr!(
                "show.observe_target",
                target = target(&o.target),
                last_probe = last_probe(o.last_probe.as_ref(), clock)
            )
        );
    }
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.failed_rounds",
            rounds = t.consecutive_failures,
            seconds = t.freshness_secs
        )
    );
    let _ = writeln!(
        out,
        "  {}",
        tr!(
            "show.times",
            verified = clock.maybe(t.last_verified_unix_ms),
            deadline = clock.maybe(t.deadline_unix_ms),
            next_retry = clock.maybe(t.next_retry_unix_ms)
        )
    );
    if let Some(f) = &t.fault {
        let _ = writeln!(
            out,
            "  {}",
            tr!(
                "show.fault",
                kind = word(&f.kind),
                detail = f.message.as_str()
            )
        );
    }
    if let Some(i) = &t.incident {
        let incident = i.incident_id.as_str();
        let opened = clock.at(i.opened_at_unix_ms);
        let first_recovery = word(&i.first_recovery);
        let _ = writeln!(
            out,
            "  {}",
            if i.retries_exhausted {
                tr!(
                    "show.incident_retries_used_up",
                    incident = incident,
                    opened = opened,
                    first_recovery = first_recovery,
                    retries = i.retries_used
                )
            } else {
                tr!(
                    "show.incident",
                    incident = incident,
                    opened = opened,
                    first_recovery = first_recovery,
                    retries = i.retries_used
                )
            }
        );
    }
    out
}

/// The watchdog and every tunnel, briefly.
pub fn status(s: &Snapshot) -> String {
    let mut out = String::new();
    let zone = &s.host.time_zone;
    let version = s.host.watchdog_version.as_str();
    let utc_offset = offset(zone.utc_offset_secs);
    let _ = writeln!(
        out,
        "{}",
        match zone.effective.as_ref() {
            Some(name) => tr!(
                "show.watchdog",
                version = version,
                zone = name.as_str(),
                offset = utc_offset
            ),
            None => tr!(
                "show.watchdog_system_zone",
                version = version,
                offset = utc_offset
            ),
        }
    );
    let storage = word(&s.storage);
    let (running, workers) = (s.host.workers.running, s.host.workers.effective);
    let _ = writeln!(
        out,
        "{}",
        if s.monitoring_paused {
            tr!(
                "show.monitoring_paused",
                storage = storage,
                running = running,
                workers = workers
            )
        } else {
            tr!(
                "show.monitoring_running",
                storage = storage,
                running = running,
                workers = workers
            )
        }
    );
    let _ = writeln!(
        out,
        "{}",
        tr!(
            "show.mcp",
            mcp = on(s.host.mcp_enabled),
            telegram = on(s.host.mcp_telegram_enabled)
        )
    );
    let n = &s.notification;
    let _ = writeln!(
        out,
        "{}",
        tr!(
            "show.notifications_summary",
            state = on(n.enabled),
            token = token(n.token),
            chat = if n.chat_id.is_some() {
                tr!("show.value.set")
            } else {
                tr!("show.value.not_set")
            },
            language =
                languages::in_use(n.templates_id.as_ref(), &s.host.watchdog_version).describe()
        )
    );
    if s.tunnels.is_empty() {
        let _ = writeln!(out, "{}", tr!("show.no_tunnels"));
    } else {
        let _ = writeln!(out, "{}", tr!("show.tunnels", count = s.tunnels.len()));
        for t in s.tunnels.iter() {
            let _ = writeln!(out, "{}", tunnel_line(t));
        }
    }
    out
}

fn token(state: CredentialState) -> String {
    match state {
        CredentialState::NotSet => tr!("show.value.not_set"),
        CredentialState::Stored => tr!("show.value.set"),
        CredentialState::Unavailable => tr!("show.token_unavailable"),
    }
}

/// `+08:00`, `-03:30`, `+00:00`.
pub fn offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.unsigned_abs();
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}

/// The notification settings.
pub fn notification(s: &Snapshot, clock: &Clock) -> String {
    let n = &s.notification;
    let mut out = String::new();
    let _ = writeln!(out, "{}", tr!("show.notifications", state = on(n.enabled)));
    let _ = writeln!(
        out,
        "{}",
        if n.device_name.as_str().trim().is_empty() {
            // Messages give the host name then (watchdog 4.1.0).
            match &n.host_name {
                Some(host) => tr!("show.device_name_host", name = host.as_str()),
                None => tr!("show.device_name_none"),
            }
        } else {
            tr!("show.device_name", name = n.device_name.as_str())
        }
    );
    let _ = writeln!(
        out,
        "{}",
        tr!(
            "show.chat_id",
            chat = n
                .chat_id
                .as_ref()
                .map_or_else(|| tr!("show.value.not_set"), |c| c.as_str().to_owned())
        )
    );
    let _ = writeln!(out, "{}", tr!("show.bot_token", token = token(n.token)));
    let _ = writeln!(
        out,
        "{}",
        tr!(
            "show.messages_language",
            language =
                languages::in_use(n.templates_id.as_ref(), &s.host.watchdog_version).describe()
        )
    );
    let _ = writeln!(
        out,
        "{}",
        match &n.last_test {
            Some(t) => match t.failure {
                Some(f) => tr!(
                    "show.last_test_failed",
                    state = word(&t.state),
                    asked = clock.at(t.requested_at_unix_ms),
                    failure = word(&f)
                ),
                None => tr!(
                    "show.last_test",
                    state = word(&t.state),
                    asked = clock.at(t.requested_at_unix_ms)
                ),
            },
            None => tr!("show.last_test_none"),
        }
    );
    if let Some(f) = n.last_failure {
        let _ = writeln!(out, "{}", tr!("show.last_failure", failure = word(&f)));
    }
    let off: Vec<&str> = s
        .tunnels
        .iter()
        .filter(|t| !t.notifications_enabled)
        .map(|t| t.tunnel_id.as_str())
        .collect();
    if !off.is_empty() {
        let _ = writeln!(
            out,
            "{}",
            tr!("show.notifications_off_for", tunnels = off.join(", "))
        );
    }
    out
}

/// The manager's tunnels, and whether WGAL watches each.
pub fn discovered(list: &[DiscoveredTunnel], s: &Snapshot) -> String {
    let mut out = String::new();
    if list.is_empty() {
        let _ = writeln!(out, "{}", tr!("show.no_discovered"));
        return out;
    }
    for d in list {
        let registered = s.tunnels.iter().any(|t| t.tunnel_id == d.tunnel_id);
        let tunnel = d.tunnel_id.as_str();
        let name = d.display_name.as_str();
        let control = word(&d.control);
        let _ = writeln!(
            out,
            "  {}",
            if registered {
                tr!(
                    "show.discovered_registered",
                    tunnel = tunnel,
                    name = name,
                    control = control
                )
            } else {
                tr!(
                    "show.discovered",
                    tunnel = tunnel,
                    name = name,
                    control = control
                )
            }
        );
    }
    out
}

/// One page of the event log, newest first; `filter` is the options that
/// chose it (`--tunnel ID` and so on), kept in the command for the next
/// page.
pub fn log(entries: &[EventEntry], more: bool, filter: &str, clock: &Clock) -> String {
    let mut out = String::new();
    if entries.is_empty() {
        let _ = writeln!(out, "{}", tr!("show.log_empty"));
    }
    for e in entries {
        let tunnel = e.tunnel_id.as_ref().map_or("-", |id| id.as_str());
        let tunnel = if e.removed_tunnel {
            tr!("show.log_removed_tunnel", tunnel = tunnel)
        } else {
            tunnel.to_owned()
        };
        let _ = writeln!(
            out,
            "{:>8}  {}  {}  {}",
            e.seq,
            clock.at(e.at_unix_ms),
            tunnel,
            word(&e.event)
        );
    }
    if more && let Some(oldest) = entries.iter().map(|e| e.seq).min() {
        let _ = writeln!(
            out,
            "{}",
            tr!("show.log_more", options = filter, before = oldest)
        );
    }
    out
}

/// How one part of an import counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counted {
    Done,
    Skipped,
    Failed,
}

pub fn counted(outcome: &ImportOutcome) -> Counted {
    match outcome {
        ImportOutcome::Applied | ImportOutcome::Registered => Counted::Done,
        ImportOutcome::NotFound | ImportOutcome::NotRegistered => Counted::Skipped,
        ImportOutcome::Ambiguous
        | ImportOutcome::DuplicateInFile
        | ImportOutcome::Unsupported
        | ImportOutcome::Refused { .. } => Counted::Failed,
    }
}

/// How a tunnel entry counts: its outcome, and a refused automatic
/// recovery as a failure even when its settings were set.
pub fn item_counted(item: &ImportItem) -> Counted {
    match (counted(&item.outcome), &item.auto_recovery) {
        (Counted::Done, AutoRecoveryImport::Refused { .. }) => Counted::Failed,
        (counted, _) => counted,
    }
}

/// Whether any part of an import failed.
pub fn import_failed(r: &ImportResult) -> bool {
    counted(&r.host) == Counted::Failed
        || r.notifications
            .as_ref()
            .is_some_and(|n| counted(n) == Counted::Failed)
        || r.tunnels.iter().any(|t| item_counted(t) == Counted::Failed)
}

fn auto_recovery(a: &AutoRecoveryImport) -> String {
    match a {
        AutoRecoveryImport::Off => on(false).to_owned(),
        AutoRecoveryImport::On => on(true).to_owned(),
        AutoRecoveryImport::WaitingForBaseline => tr!("show.auto_recovery_waiting"),
        AutoRecoveryImport::Refused { .. } => tr!("show.auto_recovery_refused", reason = word(a)),
    }
}

/// An import's result, part by part.
pub fn import(r: &ImportResult) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}",
        if r.dry_run {
            tr!("show.import_dry_run")
        } else {
            tr!("show.import_done")
        }
    );
    let _ = writeln!(
        out,
        "  {}",
        tr!("show.import_host", outcome = word(&r.host))
    );
    if let Some(n) = &r.notifications {
        let _ = writeln!(
            out,
            "  {}",
            tr!("show.import_notifications", outcome = word(n))
        );
    }
    for t in r.tunnels.iter() {
        let failed = item_counted(t) == Counted::Failed && counted(&t.outcome) == Counted::Done;
        let index = t.index;
        let name = t.name.as_str();
        // The tunnel the entry was matched to, as data: ` -> office-1`.
        let tunnel = t
            .tunnel_id
            .as_ref()
            .map(|id| format!(" -> {}", id.as_str()))
            .unwrap_or_default();
        let outcome = word(&t.outcome);
        let auto_recovery = auto_recovery(&t.auto_recovery);
        let _ = writeln!(
            out,
            "  {}",
            if failed {
                tr!(
                    "show.import_entry_auto_recovery_refused",
                    index = index,
                    name = name,
                    tunnel = tunnel,
                    outcome = outcome,
                    auto_recovery = auto_recovery
                )
            } else {
                tr!(
                    "show.import_entry",
                    index = index,
                    name = name,
                    tunnel = tunnel,
                    outcome = outcome,
                    auto_recovery = auto_recovery
                )
            }
        );
    }
    out
}
