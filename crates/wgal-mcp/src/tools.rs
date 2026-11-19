//! The tools (owner decision of 2026-11-04): each basic need in one call,
//! with what it did read back from the watchdog, so an agent can check its
//! own work; strict about every argument, and saying, when one is wrong,
//! what it must be so the agent can correct it. Nothing here gives out the
//! Bot Token; the Chat ID only while the window lets agents set Telegram
//! (owner decision of 2026-11-06).

use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::ops::RangeInclusive;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use wgal_protocol::{
    AutoRecoveryImport, BotToken, BoundedVec, Capability, ChatId, DeviceName, ErrorCode,
    ErrorReason, EventEntry, EventFilter, HostSettings, ImportMatch, ImportOptions, ImportOutcome,
    ImportResult, LogEvent, MAX_EVENT_PAGE, MAX_OBSERVE_TARGETS, MAX_WORKERS, MIN_WORKERS,
    MonitorSettings, OsState, Outcome, ProbeKind, ProbeResult, ProbeTarget, ProbeTiming,
    ProductVersion, ProtocolError, RecoveryPhase, RecoveryRetry, Request, Response,
    SettingsDocument, Snapshot, TargetAddress, TargetStatus, TestNotificationState, TimeZoneName,
    TunnelId, TunnelSettingsEntry, TunnelStatus, WorkerCount,
};

pub use wgal_client::token::looks_like_token;
use wgal_client::token::without_tokens;
use wgal_client::{Delivery, Unreachable, Watchdog};

/// What a tool answers: data for the agent (`structuredContent`), the same
/// in text, and whether it failed.
pub struct ToolResult {
    data: Value,
    text: String,
    failed: bool,
}

impl ToolResult {
    fn ok(data: Value) -> Self {
        let text = serde_json::to_string_pretty(&data).unwrap_or_default();
        Self {
            data,
            text,
            failed: false,
        }
    }

    fn fail(message: String, data: Value) -> Self {
        Self {
            data: json!({ "error": message, "details": data }),
            text: message,
            failed: true,
        }
    }

    /// This failure, saying what was already done before it, in words and
    /// as `done`.
    fn noting(mut self, done: &[&str]) -> Self {
        if !done.is_empty() {
            let note = format!(" (before this, {})", done.join(" and "));
            self.text.push_str(&note);
            if let Some(Value::String(error)) = self.data.get_mut("error") {
                error.push_str(&note);
            }
            if let Value::Object(data) = &mut self.data {
                data.insert("done".to_owned(), json!(done));
            }
        }
        self
    }

    /// The result as MCP sends it, with nothing shaped like a Bot Token
    /// left in it (see [`hide_tokens`]).
    pub fn into_value(self) -> Value {
        let mut value = json!({
            "content": [{ "type": "text", "text": self.text }],
            "structuredContent": self.data,
            "isError": self.failed,
        });
        hide_tokens(&mut value);
        value
    }
}

type Outcomes<T> = Result<T, ToolResult>;

/// The answer when the watchdog accepted every change in `done` but what
/// followed failed (`failure`): reading the state afterwards, or a step that
/// only reads. Not a failure of the tool, since the change was made (AR-05):
/// `accepted` and `done` say what was, `state_read: false` that its effect
/// is not shown. `extra` (an object, or null) adds fields of the tool's own.
fn accepted_unread(failure: &ToolResult, done: &[&str], extra: Value) -> ToolResult {
    let mut data = json!({
        "accepted": true,
        "state_read": false,
        "done": done,
        "note": format!(
            "The watchdog accepted it ({}), but the state afterwards could not be read, so it is not shown here; call health to see it. What failed: {}",
            done.join(", "),
            failure.text
        ),
    });
    if let (Value::Object(data), Value::Object(extra)) = (&mut data, extra) {
        data.extend(extra);
    }
    ToolResult::ok(data)
}

/// `read`, done after the watchdog accepted `done`: its failure is answered
/// as [`accepted_unread`].
fn after_accepted<T>(done: &[&str], read: Outcomes<T>) -> Outcomes<T> {
    read.map_err(|failure| accepted_unread(&failure, done, Value::Null))
}

fn bad(tool: &str, message: impl Into<String>) -> ToolResult {
    ToolResult::fail(format!("{tool}: {}", message.into()), Value::Null)
}

/// The first watchdog that checks a tunnel once when asked while it is not
/// monitored (owner decision of 2026-11-06).
const ONE_OFF_PROBE_SINCE: &str = "1.5.0";

/// The first watchdog that lets agents read the chat ID and change the
/// device name, chat ID and Bot Token, while the window allows it (owner
/// decision of 2026-11-06).
const TELEGRAM_SINCE: &str = "2.1.0";

/// What an agent tells the user when it may not set Telegram: the same
/// words in `health`, in `set_telegram_settings`' own refusal and in the
/// watchdog's (owner decision of 2026-11-06).
const TELEGRAM_HOW_TO_ALLOW: &str = "The user can allow it on the WGAL window's MCP tab (讓 Agent 設定 Telegram 通知) or by running \"wgal mcp telegram on\" in a terminal, or set the device name, chat ID and Bot Token themselves on the window's 通知 (Notifications) page or with \"wgal notify\".";

/// The pattern `set_telegram_settings` advertises for `chat_id`: `""` to
/// clear, or what [`ChatId`] takes.
fn chat_id_pattern() -> String {
    format!(
        "^(|-?[1-9][0-9]{{0,{}}}|@[A-Za-z][A-Za-z0-9_]{{{},{}}})$",
        ChatId::DIGITS.end() - 1,
        ChatId::USERNAME_CHARS.start() - 1,
        ChatId::USERNAME_CHARS.end() - 1
    )
}

/// The pattern for `bot_token`: `""` to clear, or what [`BotToken`] takes.
fn bot_token_pattern() -> String {
    format!(
        "^(|[0-9]{{{},{}}}:[A-Za-z0-9_-]{{{},{}}})$",
        BotToken::BOT_ID_DIGITS.start(),
        BotToken::BOT_ID_DIGITS.end(),
        BotToken::SECRET_CHARS.start(),
        BotToken::SECRET_CHARS.end()
    )
}

/// Longest an operation tool waits for the tunnel to settle.
const MAX_WAIT_SECS: u64 = 120;
const DEFAULT_WAIT_SECS: u64 = 30;
const POLL: Duration = Duration::from_millis(1000);

// --- Definitions -----------------------------------------------------------

const TUNNEL_ID: &str = "The tunnel's WGAL ID, as list_tunnels or health gives it: 1 to 64 ASCII letters, digits, '-' or '_'.";
const WAIT: &str =
    "Seconds to wait for the tunnel to settle before answering, 0 to 120; default 30.";

fn tunnel_id_schema() -> Value {
    json!({ "type": "string", "description": TUNNEL_ID, "pattern": "^[A-Za-z0-9_-]{1,64}$" })
}

fn target_schema() -> Value {
    json!({
        "type": "object",
        "description": "A probe target: an IPv4 address reached only through the tunnel (no host names), probed with ICMP echo, or with a TCP connect when port is given. A target as get_tunnel shows it, with probe instead of port, is taken as it is.",
        "properties": {
            "address": { "type": "string", "description": "IPv4 address, for example 192.0.2.1. Not loopback, multicast, broadcast, link-local or 0.0.0.0." },
            "port": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "TCP port to connect to; leave out for ICMP." },
            "probe": { "type": "object", "description": "Instead of port, as get_tunnel shows a target: {\"kind\": \"icmp\"} or {\"kind\": \"tcp_connect\", \"port\": 443}." }
        },
        "required": ["address"],
        "additionalProperties": false
    })
}

/// `ms` since the Unix epoch as an RFC 3339 time in UTC, such as
/// `2026-11-06T00:12:03Z`: agents asked for times they can read and
/// convert (MCP agent test of 2026-11-06). Howard Hinnant's
/// `civil_from_days`.
fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Where `new` differs from `old`, by dotted path, with both values;
/// objects are compared field by field over the fields of both (a field
/// only one has counts as `null` in the other), anything else whole.
fn diff(path: &str, old: &Value, new: &Value, out: &mut Vec<Value>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let keys = b.keys().chain(a.keys().filter(|k| !b.contains_key(*k)));
            for key in keys {
                let p = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let get = |m: &Map<String, Value>| m.get(key).cloned().unwrap_or(Value::Null);
                diff(&p, &get(a), &get(b), out);
            }
        }
        _ if old != new => out.push(json!({ "field": path, "from": old, "to": new })),
        _ => {}
    }
}

/// A tunnel's own settings, as the switch and update tools compare them.
fn settings_of(t: &TunnelStatus) -> Value {
    json!({
        "monitoring": t.intent.monitoring_enabled,
        "notifications": t.notifications_enabled,
        "auto_recovery": t.intent.auto_recovery_enabled,
        "auto_recovery_pending": t.auto_recovery_pending,
        "start_when_stopped_externally": t.intent.start_when_stopped_externally,
        "recovery_target": t.recovery_target.as_ref().map(|r| r.target.value()),
        "observe_targets": t.observe_targets.iter().map(|o| o.target.value()).collect::<Vec<_>>(),
        "probe": t.probe.value(),
        "retry": t.retry.value(),
    })
}

/// What a switch or update tool answers: what changed, from and to, so an
/// agent can tell a change from a call that changed nothing (MCP agent
/// test of 2026-11-06), and the tunnel as it is now.
fn changed(before: Option<&TunnelStatus>, after: &TunnelStatus) -> Map<String, Value> {
    let mut changes = Vec::new();
    let was = before.map_or_else(|| json!({}), settings_of);
    diff("", &was, &settings_of(after), &mut changes);
    let note = if changes.is_empty() {
        "Nothing changed: the tunnel already had these settings."
    } else {
        "changed lists each setting that changed, from and to."
    };
    let mut answer = Map::new();
    answer.insert("accepted".to_owned(), json!(true));
    answer.insert("changed".to_owned(), Value::Array(changes));
    answer.insert("note".to_owned(), json!(note));
    answer.insert("tunnel".to_owned(), summary(after));
    answer
}

/// An optional time as `utc` gives it, or `null`.
fn at(ms: Option<i64>) -> Value {
    ms.map_or(Value::Null, |ms| Value::String(utc(ms)))
}

/// `value` with a time `utc` writes beside every `*_unix_ms` field, at any
/// depth, for the answers that pass contract values through whole:
/// `deadline_unix_ms` gets `deadline_at`, `finished_at_unix_ms` gets
/// `finished_at`.
fn with_times(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, v) in map {
                if let Some(stem) = key.strip_suffix("_unix_ms") {
                    let name = if stem.ends_with("_at") {
                        stem.to_owned()
                    } else {
                        format!("{stem}_at")
                    };
                    out.insert(name, at(v.as_i64()));
                }
                out.insert(key, with_times(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(with_times).collect()),
        other => other,
    }
}

fn tool(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "annotations": {
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "openWorldHint": false
        }
    })
}

/// Every tool, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    let id = || json!({ "tunnel_id": tunnel_id_schema() });
    let id_wait = || json!({ "tunnel_id": tunnel_id_schema(), "wait_secs": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECS, "description": WAIT } });
    vec![
        tool(
            "health",
            "Everything about WGAL in one call: the watchdog (version, storage, whether monitoring is paused), notifications (on or off, the device name, whether agents may set Telegram, then the chat ID while they may or how_to_allow, what to tell the user, while they may not; never the Bot Token), and every registered tunnel's light (green = traffic verified, yellow = a time-limited connect, verify or confirmation, red = fault, recovery failed or needs attention, gray = off or disconnected), reason, OS state, settings in short, and open incident with its retry count. Per tunnel, traffic_verified is true when WGAL has verified traffic recently (the green light); false means only that it is not verified now, not that traffic is down: with monitoring off, for example, WGAL is not checking, so say so, look at reason and last_probe, and use probe_tunnel for a one-off check if the user wants one. last_probe gives the latest probe of the recovery target (kept while monitoring is off, so it may be old), and while the light is yellow deadline_at says when that step ends and the tunnel is judged again. Notifications also say whether a token and a chat are stored. Call this first, and again after a change to see its effect. For one tunnel's every setting (probe timing, watch-only targets), call get_tunnel: it gives the same fields under the same names, and more.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "list_tunnels",
            "Every tunnel WGAL monitors (registered: each as get_tunnel gives it) and every tunnel the official WireGuard manager has that WGAL does not monitor yet (not_registered: tunnel_id, name, whether WGAL may control it), so you can compare WGAL with WireGuard. For one tunnel, get_tunnel is shorter.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "get_tunnel",
            "One tunnel WGAL monitors, in full: health's fields for it under the same names, plus every setting and the rest of its state. probe is its probe timing; recovery_target, observe_targets, probe and retry are written the way update_tunnel_settings takes them, so you can change one and give it back. last_probe is the recovery target's latest probe, observe_last_probes each watch-only target's; light_since_at says since when the light is what it is. Read only.",
            id(),
            &["tunnel_id"],
            true,
            false,
        ),
        tool(
            "register_tunnel",
            "Starts monitoring a tunnel the official WireGuard manager has (see list_tunnels). It starts observe-only, with no recovery target; set one with update_tunnel_settings. Never starts or stops the tunnel. Limited by the installation's access list.",
            id(),
            &["tunnel_id"],
            false,
            false,
        ),
        tool(
            "unregister_tunnel",
            "Forgets WGAL's registration of a tunnel and everything WGAL kept for it. Never starts, stops or changes the tunnel or its WireGuard configuration.",
            id(),
            &["tunnel_id"],
            false,
            true,
        ),
        tool(
            "set_tunnel_monitoring",
            "One tunnel: turns its monitoring on or off. Off: WGAL probes nothing for it on its own and takes no automatic action (probe_tunnel can still check it once); the tunnel itself is not touched.",
            json!({ "tunnel_id": tunnel_id_schema(), "enabled": { "type": "boolean" } }),
            &["tunnel_id", "enabled"],
            false,
            false,
        ),
        tool(
            "set_monitoring_paused",
            "Whole computer: pauses or resumes monitoring of every tunnel at once; no tunnel is started or stopped because of it, and probe_tunnel can still check one tunnel once while paused. For one tunnel, use set_tunnel_monitoring.",
            json!({ "paused": { "type": "boolean" } }),
            &["paused"],
            false,
            false,
        ),
        tool(
            "connect_tunnel",
            "WARNING: starts the real VPN tunnel and changes this computer's routing as its WireGuard configuration says. Use only when the user wants this tunnel up. Waits for it to settle and answers its state.",
            id_wait(),
            &["tunnel_id"],
            false,
            true,
        ),
        tool(
            "disconnect_tunnel",
            "WARNING: stops the real VPN tunnel; traffic that goes through it stops, which may cut connections the user relies on (remote desktop, SSH, file shares). WGAL then keeps it down until connect_tunnel. Use only when the user wants this tunnel down. Waits for it to settle and answers its state.",
            id_wait(),
            &["tunnel_id"],
            false,
            true,
        ),
        tool(
            "reconnect_tunnel",
            "WARNING: stops and starts the real VPN tunnel, interrupting traffic through it for a few seconds. Waits for it to settle, then checks traffic through it once (a one-off probe of its targets, also while monitoring is off) and answers whether traffic came back (traffic.recovered: true; false when every probe ran and no target answered; or null when it could not check, a probe that could not run included, or a target answered but this computer cannot confirm it was through the tunnel; traffic.reason names the case and traffic.why then says more), with its state. traffic.targets[].through_tunnel_verified is true only for a target that answered this check through a probe bound to the tunnel. Only probes that started after the check began count. wait_secs applies to each of the two waits. If its answer is lost the reconnect is not sent again; if the state cannot be read after it was accepted, the answer says accepted with state_read false.",
            id_wait(),
            &["tunnel_id"],
            false,
            true,
        ),
        tool(
            "probe_tunnel",
            "Probes the tunnel's targets now, waits for the result (wait_secs) and answers the tunnel as get_tunnel gives it, with probed saying whether a new result came in; tunnel.last_probe is the recovery target's answer and tunnel.observe_last_probes the watch-only targets'. With monitoring on, the next round runs now and is judged as usual. With monitoring off (this tunnel's switch, or paused for every tunnel) it is a one-off check: the targets are probed once and last_probe shows the answer, but the light stays gray, no fault or automatic action follows, and monitoring stays off; there is no need to turn monitoring on for it (watchdog 1.5.0 or later). Nothing is started or stopped.",
            id_wait(),
            &["tunnel_id"],
            false,
            false,
        ),
        tool(
            "set_auto_recovery",
            "Turns automatic recovery of one tunnel on or off: when on, WGAL restarts only this tunnel after a confirmed fault and retries as its retry settings say. Turning it on needs a recovery target that has answered a probe through the tunnel, and the tunnel on the installation's access list. start_when_stopped_externally (optional) also starts the tunnel when something else stopped it.",
            json!({ "tunnel_id": tunnel_id_schema(), "enabled": { "type": "boolean" }, "start_when_stopped_externally": { "type": "boolean" } }),
            &["tunnel_id", "enabled"],
            false,
            false,
        ),
        tool(
            "update_tunnel_settings",
            "Changes one tunnel's settings in one call. A setting left out keeps its value, and so does a field left out of probe or retry; a target is replaced whole (leaving port out of one means ICMP). null means something in two places only: recovery_target: null removes the recovery target, and retry.limit: null means no limit; anywhere else null is refused. recovery_target: the target whose failure means the tunnel is down; changing or removing it turns automatic recovery off, so turn it on again with set_auto_recovery once the new target has answered a probe. observe_targets: up to 4 targets that are only shown; the list replaces the current one, and [] removes them all. probe: interval_secs 10-3600, timeout_secs 1-10, retries 0-10 (failed rounds before a fault is confirmed, plus one), min_fault_secs 0-3600. retry: how automatic recovery tries again after its first round: limit 0-3600 rounds, or null to keep trying while the fault lasts; interval_secs 10-10800. Examples: {\"probe\": {\"interval_secs\": 60}} changes the interval and keeps the other three; {\"retry\": {\"limit\": null}} keeps trying and keeps the interval; {\"retry\": {\"interval_secs\": 120}} keeps the limit, null included; {\"recovery_target\": null} removes the recovery target; {\"observe_targets\": []} removes the watch-only targets. Targets and timing read from get_tunnel can be given back as they are. The answer's changed lists each setting that changed, from and to (empty when nothing did), and tunnel is the tunnel as get_tunnel gives it.",
            json!({
                "tunnel_id": tunnel_id_schema(),
                "recovery_target": { "oneOf": [target_schema(), { "type": "null" }], "description": "Leave out to keep; null removes it (and turns automatic recovery off)." },
                "observe_targets": { "type": "array", "maxItems": MAX_OBSERVE_TARGETS, "items": target_schema(), "description": "Leave out to keep; a list replaces the current one; [] removes them all. Not null." },
                "probe": { "type": "object", "description": "Leave out, or leave a field out, to keep it; not null.", "properties": {
                    "interval_secs": { "type": "integer", "minimum": 10, "maximum": 3600 },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 10 },
                    "retries": { "type": "integer", "minimum": 0, "maximum": 10 },
                    "min_fault_secs": { "type": "integer", "minimum": 0, "maximum": 3600 }
                }, "additionalProperties": false },
                "retry": { "type": "object", "description": "Leave out, or leave a field out, to keep it.", "properties": {
                    "limit": { "type": ["integer", "null"], "minimum": 0, "maximum": 3600, "description": "Rounds after the first, or null to keep trying while the fault lasts; leave out to keep." },
                    "interval_secs": { "type": "integer", "minimum": 10, "maximum": 10800, "description": "Seconds between rounds; leave out to keep; not null." }
                }, "additionalProperties": false }
            }),
            &["tunnel_id"],
            false,
            false,
        ),
        tool(
            "set_whole_computer_notifications",
            "Whole computer: turns Telegram notifications on or off, keeping the device name, chat ID and Bot Token as they are. To change those three, use set_telegram_settings. For one tunnel, use set_one_tunnel_notifications.",
            json!({ "enabled": { "type": "boolean" } }),
            &["enabled"],
            false,
            false,
        ),
        tool(
            "set_telegram_settings",
            "Whole computer: changes or clears the device name, chat ID and Bot Token that Telegram notifications use. It works only while the user lets agents do it, on the WGAL window's MCP tab (讓 Agent 設定 Telegram 通知) or with `wgal mcp telegram on` (off by default): health says whether they do (notifications.agent_may_set) and, if not, what to tell the user (notifications.how_to_allow): they can allow it there, or set these themselves on the window's 通知 (Notifications) page. Give only what changes; anything left out stays as it is. To clear one, give null or \"\", or list it in clear. Without a chat ID or Bot Token no notification is sent; without a device name, messages call this computer unnamed. The Bot Token is write-only: no tool ever gives it out (health says only whether one is stored), so never repeat one back. Notifications are turned on or off with set_whole_computer_notifications; check the result with send_test_notification.",
            json!({
                "device_name": { "type": ["string", "null"], "description": "The name messages give this computer: any text the user likes, of at most 64 characters; a longer name is cut to 61 characters and '...'. Control characters become spaces. null or \"\" clears it." },
                "chat_id": { "type": ["string", "null"], "pattern": chat_id_pattern(), "description": "Where messages go: a numeric chat ID of 1 to 19 digits not starting with 0, negative for groups and channels (for example -1001234567890), or a public @username of 1 to 37 letters, digits or '_' starting with a letter. null or \"\" clears it." },
                "bot_token": { "type": ["string", "null"], "pattern": bot_token_pattern(), "description": "The bot's token as @BotFather gives it: <bot id of 3 to 15 digits>:<secret of 30 to 40 letters, digits, '_' or '-'>. Write-only. null or \"\" clears it." },
                "clear": { "type": "array", "uniqueItems": true, "items": { "enum": ["device_name", "chat_id", "bot_token"] }, "description": "Settings to clear, the same as giving each null or \"\". A setting may not be both given and listed here." }
            }),
            &[],
            false,
            true,
        ),
        tool(
            "set_one_tunnel_notifications",
            "One tunnel: turns its Telegram notifications on or off, under the whole computer's switch (set_whole_computer_notifications).",
            json!({ "tunnel_id": tunnel_id_schema(), "enabled": { "type": "boolean" } }),
            &["tunnel_id", "enabled"],
            false,
            false,
        ),
        tool(
            "send_test_notification",
            "Sends a Telegram test message through the watchdog and waits for Telegram's answer.",
            json!({ "wait_secs": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECS, "description": "Seconds to wait for the result, 0 to 120; default 30." } }),
            &[],
            false,
            false,
        ),
        tool(
            "update_host_settings",
            "Whole computer: sets the watchdog's worker threads (workers: 2 to 17, or null for automatic) and the time zone messages are written in (time_zone: an IANA name such as Asia/Taipei or UTC, or null to follow the system). Give only what changes.",
            json!({
                "workers": { "type": ["integer", "null"], "minimum": MIN_WORKERS, "maximum": MAX_WORKERS },
                "time_zone": { "type": ["string", "null"] }
            }),
            &[],
            false,
            false,
        ),
        tool(
            "export_settings",
            "WGAL's settings as a document: every tunnel's settings and the host-wide ones, never the device name, Bot Token or Chat ID. Keep it before changing anything; import_settings puts it back.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "import_settings",
            "Puts a document from export_settings back. match_by: \"id\" (default, this computer) or \"name\" (another computer; a name more than one tunnel here has is not used). register_missing (default false): also register a tunnel WireGuard has but WGAL does not monitor. monitor_new (default false): turn monitoring on for those as the document says. dry_run (default false): change nothing and say what would happen, with changes listing each setting that would change, from the current value to the document's. Answers the outcome of the host settings (worker threads, time zone), of the notification switch (stored apart, so its own) and of each entry; each part is set whole or not at all.",
            json!({
                "document": { "type": "object", "description": "The document export_settings gave, unchanged or edited." },
                "match_by": { "type": "string", "enum": ["id", "name"] },
                "register_missing": { "type": "boolean" },
                "monitor_new": { "type": "boolean" },
                "dry_run": { "type": "boolean" }
            }),
            &["document"],
            false,
            false,
        ),
        tool(
            "read_event_log",
            "Reads WGAL's event log (about the last 72 hours), newest first: faults, probe results that changed, recoveries, connects and disconnects, setting changes (an agent's are marked agent_request), notifications, and the watchdog's own starts and stops. Give tunnel_id for one tunnel's lines, or scope \"system\" for the lines about no tunnel or \"watchdog\" for its starts and stops; neither gives everything. skip_agent_requests leaves out the lines that record agents' own requests. limit (1-100, default 20) caps the lines; when more is true, call again with before_seq set to next_before_seq for older ones. Times are in UTC (at) and Unix milliseconds.",
            json!({
                "tunnel_id": tunnel_id_schema(),
                "scope": { "type": "string", "enum": ["all", "system", "watchdog"], "description": "Which lines when tunnel_id is not given; default all." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_EVENT_PAGE, "description": "Most lines to answer, 1 to 100; default 20." },
                "before_seq": { "type": "integer", "minimum": 1, "description": "Only lines older than this seq: next_before_seq from the previous answer." },
                "skip_agent_requests": { "type": "boolean", "description": "Leave out the agent_request lines that record what agents asked for (yours included); default false." }
            }),
            &[],
            true,
            false,
        ),
        tool(
            "log_locations",
            "Where WGAL's event log and service log files are on this computer. To read the event log, use read_event_log; this tool only gives the paths. The files are not read for you: read them with your own permissions (on Windows and Linux they are in a folder only administrators or root can read; on macOS in your own; on Linux the service log is in the journal, `journalctl -u wgal-watchdog`).",
            json!({}),
            &[],
            true,
            false,
        ),
    ]
}

/// The tools' names, for an error about an unknown one.
pub fn names() -> Vec<String> {
    definitions()
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect()
}

// --- Arguments -------------------------------------------------------------

struct Args<'a> {
    tool: &'static str,
    map: &'a Map<String, Value>,
}

static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

/// Replaces, in every string `value` holds, whatever is shaped like a Bot
/// Token: nothing MCP sends ever carries one, wherever it came from (a
/// value the agent put in the wrong place, or a name typed into the window;
/// AGENTS.md rule 5, Codex review of 2026-11-06).
pub fn hide_tokens(value: &mut Value) {
    match value {
        Value::String(text) if looks_like_token(text) => *text = without_tokens(text),
        Value::Array(items) => items.iter_mut().for_each(hide_tokens),
        Value::Object(map) => {
            let keys: Vec<String> = map
                .keys()
                .filter(|k| looks_like_token(k))
                .cloned()
                .collect();
            for key in keys {
                if let Some(v) = map.remove(&key) {
                    map.insert(without_tokens(&key), v);
                }
            }
            map.values_mut().for_each(hide_tokens);
        }
        _ => {}
    }
}

/// What an error message may repeat of something the agent gave: never
/// anything shaped like a Bot Token (AGENTS.md rule 5; Codex review of
/// 2026-11-06), so a token put in the wrong place is not sent back.
pub fn shown(given: impl std::fmt::Display) -> String {
    let text = given.to_string();
    if looks_like_token(&text) {
        "a value shaped like a Bot Token (not repeated here)".to_owned()
    } else {
        text
    }
}

fn args<'a>(tool: &'static str, value: &'a Value, accepted: &[&str]) -> Outcomes<Args<'a>> {
    args_with(tool, value, accepted, |_, _| None)
}

/// [`args`], with `hint` saying where an unknown argument belongs, if it
/// can tell.
fn args_with<'a>(
    tool: &'static str,
    value: &'a Value,
    accepted: &[&str],
    hint: impl Fn(&str, &Value) -> Option<String>,
) -> Outcomes<Args<'a>> {
    let map = match value {
        Value::Object(map) => map,
        Value::Null => &EMPTY,
        _ => return Err(bad(tool, "the arguments must be a JSON object")),
    };
    for (key, given) in map {
        if !accepted.contains(&key.as_str()) {
            return Err(bad(
                tool,
                if accepted.is_empty() {
                    format!("takes no arguments, but got {}", shown(format!("{key:?}")))
                } else {
                    let hint = hint(key, given)
                        .map(|h| format!(": {h}"))
                        .unwrap_or_default();
                    format!(
                        "unknown argument {}{hint}; it takes: {}",
                        shown(format!("{key:?}")),
                        accepted.join(", ")
                    )
                },
            ));
        }
    }
    Ok(Args { tool, map })
}

const PROBE_FIELDS: [&str; 4] = ["interval_secs", "timeout_secs", "retries", "min_fault_secs"];
const RETRY_FIELDS: [&str; 2] = ["limit", "interval_secs"];

/// The field of `fields` that `name` stands for: itself, or itself with
/// `_secs` added (`interval` for `interval_secs`).
fn field_named(name: &str, fields: &[&'static str]) -> Option<&'static str> {
    fields
        .iter()
        .copied()
        .find(|f| *f == name || f.strip_suffix("_secs") == Some(name))
}

/// `{"group": {"field": value}}`, with the agent's own number, or `...`.
fn nested_example(group: &str, field: &str, given: &Value) -> String {
    let value = if given.is_number() {
        given.to_string()
    } else {
        "...".to_owned()
    };
    format!("{{\"{group}\": {{\"{field}\": {value}}}}}")
}

/// Where a probe or retry setting given flat belongs in
/// `update_tunnel_settings` (MCP agent test of 2026-11-06: an agent sent
/// `probe_interval_secs` for `probe.interval_secs`).
fn flat_setting_hint(key: &str, given: &Value) -> Option<String> {
    let in_group = |group: &str, fields: &[&'static str]| {
        let rest = key
            .strip_prefix(group)
            .and_then(|r| r.strip_prefix('_').or_else(|| r.strip_prefix('.')))?;
        let field = field_named(rest, fields)?;
        Some(format!(
            "{group} settings go inside {group}, for example {}",
            nested_example(group, field, given)
        ))
    };
    if let Some(hint) =
        in_group("probe", &PROBE_FIELDS).or_else(|| in_group("retry", &RETRY_FIELDS))
    {
        return Some(hint);
    }
    match (
        field_named(key, &PROBE_FIELDS),
        field_named(key, &RETRY_FIELDS),
    ) {
        (Some(field), Some(_)) => Some(format!(
            "{field} is inside probe (how often WGAL probes) and inside retry (the time between recovery retries): {} or {}",
            nested_example("probe", field, given),
            nested_example("retry", field, given)
        )),
        (Some(field), None) => Some(format!(
            "{field} goes inside probe, for example {}",
            nested_example("probe", field, given)
        )),
        (None, Some(field)) => Some(format!(
            "{field} goes inside retry, for example {}",
            nested_example("retry", field, given)
        )),
        (None, None) => None,
    }
}

/// The field of `fields` an unknown key inside `group` stands for, as in
/// `probe: {"probe_interval_secs": 60}`.
fn inner_hint(
    group: &'static str,
    fields: &'static [&'static str],
) -> impl Fn(&str, &Value) -> Option<String> {
    move |key, _| {
        let rest = key
            .strip_prefix(group)
            .and_then(|r| r.strip_prefix('_').or_else(|| r.strip_prefix('.')))
            .unwrap_or(key);
        field_named(rest, fields).map(|field| format!("inside {group} it is called {field:?}"))
    }
}

impl Args<'_> {
    fn tunnel_id(&self) -> Outcomes<TunnelId> {
        let Some(value) = self.map.get("tunnel_id") else {
            return Err(bad(
                self.tool,
                "tunnel_id is required; call health or list_tunnels for the IDs",
            ));
        };
        let Some(text) = value.as_str() else {
            return Err(bad(self.tool, "tunnel_id must be a string"));
        };
        TunnelId::new(text).map_err(|_| {
            bad(
                self.tool,
                format!("tunnel_id {} is not a WGAL tunnel ID: it is 1 to 64 ASCII letters, digits, '-' or '_'; call list_tunnels for the IDs", shown(format!("{text:?}"))),
            )
        })
    }

    fn bool(&self, key: &str) -> Outcomes<bool> {
        self.opt_bool(key)?
            .ok_or_else(|| bad(self.tool, format!("{key} is required: true or false")))
    }

    fn opt_bool(&self, key: &str) -> Outcomes<Option<bool>> {
        match self.map.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(other) => Err(bad(
                self.tool,
                format!("{key} must be true or false, not {}", shown(other)),
            )),
        }
    }

    fn wait(&self) -> Outcomes<Duration> {
        let secs = whole(
            self.tool,
            "wait_secs",
            self.map.get("wait_secs"),
            0..=MAX_WAIT_SECS,
        )?
        .unwrap_or(DEFAULT_WAIT_SECS);
        Ok(Duration::from_secs(secs))
    }
}

/// An optional whole number in `range`; JSON `null` counts as left out.
fn whole(
    tool: &str,
    key: &str,
    value: Option<&Value>,
    range: RangeInclusive<u64>,
) -> Outcomes<Option<u64>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    match value.as_u64() {
        Some(n) if range.contains(&n) => Ok(Some(n)),
        _ => Err(bad(
            tool,
            format!(
                "{key} must be a whole number from {} to {}, not {}",
                range.start(),
                range.end(),
                shown(value)
            ),
        )),
    }
}

/// Like `whole`, for a setting where `null` means nothing: it is refused
/// instead of being taken as left out, so `null` keeps a meaning only where
/// a tool says so (MCP agent test of 2026-11-06).
fn setting(
    tool: &str,
    key: &str,
    value: Option<&Value>,
    range: RangeInclusive<u64>,
) -> Outcomes<Option<u64>> {
    if value.is_some_and(Value::is_null) {
        return Err(bad(
            tool,
            format!(
                "{key} cannot be null: give a whole number from {} to {}, or leave {key} out to keep it",
                range.start(),
                range.end()
            ),
        ));
    }
    whole(tool, key, value, range)
}

fn narrow<T: TryFrom<u64>>(tool: &str, key: &str, n: u64) -> Outcomes<T> {
    T::try_from(n).map_err(|_| bad(tool, format!("{key} is out of range")))
}

/// A target as an agent writes it, `{"address": ..}` with `port` for TCP,
/// or as the tools show it, `{"address": .., "probe": {"kind": ..}}`, so one
/// read from get_tunnel can be given back as it is (MCP agent test of
/// 2026-11-06).
fn target(tool: &str, key: &str, value: &Value) -> Outcomes<ProbeTarget> {
    let Some(map) = value.as_object() else {
        return Err(bad(
            tool,
            format!(
                "{key} must be an object like {{\"address\": \"192.0.2.1\"}} or {{\"address\": \"192.0.2.1\", \"port\": 443}}"
            ),
        ));
    };
    for k in map.keys() {
        if k != "address" && k != "port" && k != "probe" {
            return Err(bad(
                tool,
                format!(
                    "{key} has an unknown field {}; a target has address and, for TCP, port",
                    shown(format!("{k:?}"))
                ),
            ));
        }
    }
    if let Some(probe) = map.get("probe") {
        if map.contains_key("port") {
            return Err(bad(
                tool,
                format!("{key} has both port and probe; give one of them"),
            ));
        }
        let mut written = map.clone();
        written.remove("probe");
        match probe.get("kind").and_then(Value::as_str) {
            Some("icmp") if probe.as_object().is_some_and(|p| p.len() == 1) => {}
            Some("tcp_connect")
                if probe.as_object().is_some_and(|p| {
                    p.len() == 2 && p.get("port").is_some_and(|port| !port.is_null())
                }) =>
            {
                written.insert("port".to_owned(), probe["port"].clone());
            }
            _ => {
                return Err(bad(
                    tool,
                    format!(
                        "{key}.probe must be {{\"kind\": \"icmp\"}} or {{\"kind\": \"tcp_connect\", \"port\": 443}}, as get_tunnel shows it; or leave probe out and give port for TCP"
                    ),
                ));
            }
        }
        return target(tool, key, &Value::Object(written));
    }
    let Some(text) = map.get("address").and_then(Value::as_str) else {
        return Err(bad(
            tool,
            format!("{key}.address is required: an IPv4 address such as 192.0.2.1"),
        ));
    };
    let ip: Ipv4Addr = text.trim().parse().map_err(|_| {
        bad(tool, format!("{key}.address {} is not an IPv4 address; give four numbers 0-255 such as 192.0.2.1 (no host names, no IPv6)", shown(format!("{text:?}"))))
    })?;
    let address =
        TargetAddress::new(ip).map_err(|e| bad(tool, format!("{key}.address {text}: {e}")))?;
    // Not null either: it would quietly make a TCP target ICMP (Codex P2).
    if map.get("port").is_some_and(Value::is_null) {
        return Err(bad(
            tool,
            format!(
                "{key}.port cannot be null: give a TCP port from 1 to 65535, or leave port out for ICMP"
            ),
        ));
    }
    let probe = match whole(tool, &format!("{key}.port"), map.get("port"), 1..=65535)? {
        None => ProbeKind::Icmp,
        Some(port) => ProbeKind::TcpConnect {
            port: NonZeroU16::new(narrow(tool, "port", port)?)
                .ok_or_else(|| bad(tool, "port must be 1 to 65535"))?,
        },
    };
    Ok(ProbeTarget { address, probe })
}

// --- Talking to the watchdog ----------------------------------------------

/// What an agent can do about a refusal.
fn hint(reason: Option<ErrorReason>, code: ErrorCode) -> &'static str {
    match reason {
        Some(ErrorReason::McpDisabled) => {
            "Ask the user to turn MCP on in the WGAL window, on its MCP tab, or by running \"wgal mcp on\" in a terminal."
        }
        Some(ErrorReason::McpTelegramDisabled) => TELEGRAM_HOW_TO_ALLOW,
        Some(ErrorReason::NeedsAdministratorOrListedTunnel) => {
            "The tunnel is not on this installation's access list; only an administrator may do this."
        }
        Some(ErrorReason::CallerNotAllowed) => {
            "The account you run as is not on this installation's access list."
        }
        Some(ErrorReason::BaselineNotPassed) => {
            "The recovery target has not answered a probe through the tunnel yet: set a target reachable only through it (update_tunnel_settings), wait for a probe (probe_tunnel), then try again."
        }
        Some(ErrorReason::TunnelNotRegistered) => {
            "WGAL does not monitor this tunnel: call list_tunnels, then register_tunnel."
        }
        Some(ErrorReason::TunnelNotDiscovered) => {
            "The official WireGuard manager does not list this tunnel: call list_tunnels for the IDs it has."
        }
        Some(ErrorReason::OperationInProgress) => {
            "WGAL is starting or stopping this tunnel now: wait a few seconds and call health."
        }
        Some(ErrorReason::UnknownTimeZone) => {
            "Use an IANA time zone name such as Asia/Taipei, Europe/Berlin or UTC."
        }
        Some(ErrorReason::CannotControl) => {
            "The official WireGuard manager does not let WGAL start or stop this tunnel."
        }
        Some(ErrorReason::BindingLost) => {
            "The official WireGuard manager no longer has this tunnel; register it again once it does."
        }
        Some(ErrorReason::StorageLoadFailed | ErrorReason::StorageSaveFailed) => {
            "WGAL cannot store its state right now; nothing was changed. Try again later, or ask the user to check the WGAL window."
        }
        _ if code == ErrorCode::Busy => "Wait a few seconds and try again.",
        _ => "",
    }
}

fn refused(tool: &str, error: &ProtocolError) -> ToolResult {
    let code = serde_json::to_value(error.code).unwrap_or(Value::Null);
    let reason = serde_json::to_value(error.reason).unwrap_or(Value::Null);
    let hint = hint(error.reason, error.code);
    let message = format!(
        "{tool}: the watchdog refused: {} ({}{}){}{}",
        error.message.as_str(),
        code.as_str().unwrap_or("error"),
        reason
            .as_str()
            .map(|r| format!(", {r}"))
            .unwrap_or_default(),
        if hint.is_empty() { "" } else { ". " },
        hint
    );
    ToolResult::fail(
        message,
        json!({ "code": code, "reason": reason, "hint": hint }),
    )
}

/// A request that got no answer, with whether it may have reached the
/// watchdog (`delivery`: `not_sent` or `unknown`).
fn unreachable(tool: &str, u: Unreachable) -> ToolResult {
    let delivery = match u.delivery {
        Delivery::NotSent => "not_sent",
        Delivery::Unknown => "unknown",
    };
    ToolResult::fail(
        format!("{tool}: {}", u.message),
        json!({ "delivery": delivery }),
    )
}

/// One request and its answer. Sent once: a lost answer or a `busy` one is
/// never sent again (AR-05), only a connection that was busy before
/// anything was sent is opened again ([`crate::watchdog`]).
fn ask<W: Watchdog>(w: &mut W, tool: &str, request: &Request) -> Outcomes<Response> {
    let answer = w.send(request).map_err(|u| unreachable(tool, u))?;
    match answer.outcome {
        Outcome::Ok(response) => Ok(response),
        Outcome::Err(error) => Err(refused(tool, &error)),
    }
}

fn snapshot<W: Watchdog>(w: &mut W, tool: &str) -> Outcomes<Snapshot> {
    match ask(w, tool, &Request::GetSnapshot)? {
        Response::Snapshot { snapshot } => Ok(snapshot),
        _ => Err(bad(
            tool,
            "the watchdog answered something other than a snapshot",
        )),
    }
}

fn tunnel_in(snapshot: &Snapshot, id: &TunnelId) -> Option<TunnelStatus> {
    snapshot
        .tunnels
        .iter()
        .find(|t| &t.tunnel_id == id)
        .cloned()
}

fn not_monitored(tool: &str, id: &TunnelId) -> ToolResult {
    bad(
        tool,
        format!(
            "WGAL does not monitor a tunnel {:?}; call list_tunnels, and register_tunnel to start monitoring one",
            id.as_str()
        ),
    )
}

fn find<W: Watchdog>(w: &mut W, tool: &str, id: &TunnelId) -> Outcomes<TunnelStatus> {
    tunnel_in(&snapshot(w, tool)?, id).ok_or_else(|| not_monitored(tool, id))
}

/// A tunnel in short, as health and the operation tools answer it.
fn summary(t: &TunnelStatus) -> Value {
    let v = |x: &dyn erased::Ser| x.value();
    json!({
        "tunnel_id": t.tunnel_id.as_str(),
        "name": t.display_name.as_str(),
        "light": v(&t.light),
        // One answer to "is traffic flowing?" (MCP agent test of
        // 2026-11-06): green means a probe through the tunnel answered
        // recently enough.
        "traffic_verified": t.light == wgal_protocol::Light::Green,
        "reason": v(&t.light_reason),
        "os_state": v(&t.os_state),
        "wanted": v(&t.intent.desired),
        "monitoring": t.intent.monitoring_enabled,
        "auto_recovery": t.intent.auto_recovery_enabled,
        "auto_recovery_pending": t.auto_recovery_pending,
        "auto_recovery_allowed": t.auto_recovery_allowed,
        "start_when_stopped_externally": t.intent.start_when_stopped_externally,
        "notifications": t.notifications_enabled,
        "recovery": v(&t.recovery),
        "consecutive_failures": t.consecutive_failures,
        "deadline_unix_ms": t.deadline_unix_ms,
        "deadline_at": at(t.deadline_unix_ms),
        "next_retry_unix_ms": t.next_retry_unix_ms,
        "next_retry_at": at(t.next_retry_unix_ms),
        "last_verified_unix_ms": t.last_verified_unix_ms,
        "last_verified_at": at(t.last_verified_unix_ms),
        "recovery_target": t.recovery_target.as_ref().map(|r| v(&r.target)),
        // The latest probe of the recovery target: whether traffic flowed
        // when it was last checked, beside whether the tunnel is up.
        "last_probe": t.recovery_target.as_ref().map_or(Value::Null, last_probe),
        "retry": v(&t.retry),
        "fault": t.fault.as_ref().map(|f| json!({ "kind": v(&f.kind), "message": f.message.as_str() })),
        "incident": t.incident.as_ref().map(|i| json!({
            "incident_id": i.incident_id.as_str(),
            "opened_at_unix_ms": i.opened_at_unix_ms,
            "opened_at": utc(i.opened_at_unix_ms),
            "first_recovery": v(&i.first_recovery),
            "retries_used": i.retries_used,
            "retries_exhausted": i.retries_exhausted,
        })),
    })
}

/// A target's latest probe, when it finished and what it answered, or
/// `null` before its first.
fn last_probe(target: &TargetStatus) -> Value {
    target.last_probe.as_ref().map_or(Value::Null, |p| {
        json!({
            "finished_at": utc(p.finished_at_unix_ms),
            "result": p.result.value(),
        })
    })
}

/// A tunnel in full, as get_tunnel, list_tunnels and update_tunnel_settings
/// answer it: `summary` and, under the same names, every setting and the
/// rest of its state, so an agent reads one set of names wherever a tunnel
/// is shown (MCP agent test of 2026-11-06). Targets are written as
/// update_tunnel_settings takes them back.
fn detail(t: &TunnelStatus) -> Value {
    let mut view = summary(t);
    if let Value::Object(map) = &mut view {
        let more = json!({
            "light_since_unix_ms": t.light_since_unix_ms,
            "light_since_at": utc(t.light_since_unix_ms),
            "probe": t.probe.value(),
            "freshness_secs": t.freshness_secs,
            "observe_targets": t.observe_targets.iter().map(|o| o.target.value()).collect::<Vec<_>>(),
            "observe_last_probes": t.observe_targets.iter().map(|o| json!({
                "target": o.target.value(),
                "last_probe": last_probe(o),
            })).collect::<Vec<_>>(),
            "takeover": t.takeover.value(),
            "data_health": t.data_health.value(),
            "recovered_notification": t.recovered_notification.value(),
            // The incident with every field the summary leaves out.
            "incident": with_times(t.incident.value()),
        });
        if let Value::Object(more) = more {
            map.extend(more);
        }
    }
    view
}

mod erased {
    /// Any contract value as JSON.
    pub trait Ser {
        fn value(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Ser for T {
        fn value(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
        }
    }
}

use erased::Ser;

/// Whether nothing about the tunnel is still moving: no stop, start or
/// verification under way and no yellow light.
fn settled(t: &TunnelStatus) -> bool {
    !matches!(
        t.recovery,
        RecoveryPhase::Queued
            | RecoveryPhase::Stopping
            | RecoveryPhase::Starting
            | RecoveryPhase::Verifying
    ) && t.light != wgal_protocol::Light::Yellow
}

/// Waits until `done` says the tunnel is where it should be, at most
/// `wait`, and answers its state then.
fn until<W: Watchdog>(
    w: &mut W,
    tool: &str,
    id: &TunnelId,
    wait: Duration,
    done: impl Fn(&TunnelStatus) -> bool,
) -> Outcomes<(TunnelStatus, bool)> {
    let start = Instant::now();
    loop {
        let t = find(w, tool, id)?;
        if done(&t) {
            return Ok((t, true));
        }
        if start.elapsed() >= wait {
            return Ok((t, false));
        }
        thread::sleep(POLL.min(wait.saturating_sub(start.elapsed())));
    }
}

fn after_operation<W: Watchdog>(
    w: &mut W,
    tool: &str,
    id: &TunnelId,
    wait: Duration,
    accepted: &str,
) -> Outcomes<ToolResult> {
    // Give the watchdog a moment to take the operation up before judging it.
    thread::sleep(POLL.min(wait));
    let (t, done) = after_accepted(&[accepted], until(w, tool, id, wait, settled))?;
    Ok(ToolResult::ok(json!({
        "accepted": true,
        "settled": done,
        "note": if done { "The tunnel has settled; see its state below." } else { "Still in progress when the wait ended; call health to follow it." },
        "tunnel": summary(&t),
    })))
}

// --- The tools -------------------------------------------------------------

/// Runs tool `name`; `None` for a tool that does not exist.
pub fn call<W: Watchdog>(w: &mut W, name: &str, arguments: &Value) -> Option<ToolResult> {
    let result = match name {
        "health" => health(w, arguments),
        "list_tunnels" => list_tunnels(w, arguments),
        "get_tunnel" => get_tunnel(w, arguments),
        "register_tunnel" => simple(
            w,
            "register_tunnel",
            arguments,
            |id| Request::RegisterTunnel { tunnel_id: id },
            "the tunnel was registered",
        ),
        "unregister_tunnel" => unregister(w, arguments),
        "set_tunnel_monitoring" => switch(
            w,
            "set_tunnel_monitoring",
            arguments,
            |tunnel_id, enabled| Request::SetTunnelMonitoring { tunnel_id, enabled },
        ),
        "set_monitoring_paused" => paused(w, arguments),
        "connect_tunnel" => operation(
            w,
            "connect_tunnel",
            arguments,
            |tunnel_id| Request::Connect { tunnel_id },
            "the connect was accepted",
        ),
        "disconnect_tunnel" => operation(
            w,
            "disconnect_tunnel",
            arguments,
            |tunnel_id| Request::Disconnect { tunnel_id },
            "the disconnect was accepted",
        ),
        "reconnect_tunnel" => reconnect(w, arguments),
        "probe_tunnel" => probe(w, arguments),
        "set_auto_recovery" => auto_recovery(w, arguments),
        "update_tunnel_settings" => update_settings(w, arguments),
        "set_whole_computer_notifications" => notifications(w, arguments),
        "set_telegram_settings" => telegram(w, arguments),
        "set_one_tunnel_notifications" => switch(
            w,
            "set_one_tunnel_notifications",
            arguments,
            |tunnel_id, enabled| Request::SetTunnelNotifications { tunnel_id, enabled },
        ),
        "send_test_notification" => test_notification(w, arguments),
        "update_host_settings" => host_settings(w, arguments),
        "export_settings" => export(w, arguments),
        "import_settings" => import(w, arguments),
        "read_event_log" => read_event_log(w, arguments),
        "log_locations" => log_locations(w, arguments),
        _ => return None,
    };
    Some(result.unwrap_or_else(|failure| failure))
}

fn health<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    args("health", arguments, &[])?;
    let s = snapshot(w, "health")?;
    let mut lights = Map::new();
    for t in s.tunnels.iter() {
        let key = t.light.value().as_str().unwrap_or("unknown").to_owned();
        let n = lights.get(&key).and_then(Value::as_u64).unwrap_or(0) + 1;
        lights.insert(key, json!(n));
    }
    let data = json!({
        "watchdog": {
            "version": s.host.watchdog_version.as_str(),
            "mcp_enabled": s.host.mcp_enabled,
            "storage": s.storage.value(),
            "monitoring_paused": s.monitoring_paused,
            "time_zone": s.host.time_zone.effective.as_ref().map(TimeZoneName::as_str),
            "workers": { "configured": s.host.workers.configured.map(|c| c.get()), "effective": s.host.workers.effective },
            "generated_at_unix_ms": s.generated_at_unix_ms,
            "generated_at": utc(s.generated_at_unix_ms),
        },
        "notifications": notifications_of(&s),
        "lights": lights,
        "tunnels": s.tunnels.iter().map(summary).collect::<Vec<_>>(),
    });
    Ok(ToolResult::ok(data))
}

fn list_tunnels<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    args("list_tunnels", arguments, &[])?;
    let s = snapshot(w, "list_tunnels")?;
    let found = match ask(w, "list_tunnels", &Request::DiscoverTunnels)? {
        Response::Discovered { tunnels } => tunnels.into_vec(),
        _ => Vec::new(),
    };
    let registered: Vec<Value> = s.tunnels.iter().map(detail).collect();
    let not_registered: Vec<Value> = found
        .iter()
        .filter(|d| tunnel_in(&s, &d.tunnel_id).is_none())
        .map(|d| json!({ "tunnel_id": d.tunnel_id.as_str(), "name": d.display_name.as_str(), "control": d.control.value() }))
        .collect();
    Ok(ToolResult::ok(
        json!({ "registered": registered, "not_registered": not_registered }),
    ))
}

/// One tunnel in full, from the snapshot alone (MCP agent test of
/// 2026-11-06: list_tunnels also asks WireGuard and answers every tunnel).
fn get_tunnel<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "get_tunnel";
    let a = args(tool, arguments, &["tunnel_id"])?;
    let id = a.tunnel_id()?;
    let t = find(w, tool, &id)?;
    Ok(ToolResult::ok(json!({ "tunnel": detail(&t) })))
}

fn simple<W: Watchdog>(
    w: &mut W,
    tool: &'static str,
    arguments: &Value,
    request: impl Fn(TunnelId) -> Request,
    accepted: &str,
) -> Outcomes<ToolResult> {
    let a = args(tool, arguments, &["tunnel_id"])?;
    let id = a.tunnel_id()?;
    ask(w, tool, &request(id.clone()))?;
    let t = after_accepted(&[accepted], find(w, tool, &id))?;
    Ok(ToolResult::ok(
        json!({ "accepted": true, "tunnel": summary(&t) }),
    ))
}

fn unregister<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "unregister_tunnel";
    let a = args(tool, arguments, &["tunnel_id"])?;
    let id = a.tunnel_id()?;
    ask(
        w,
        tool,
        &Request::UnregisterTunnel {
            tunnel_id: id.clone(),
        },
    )?;
    let s = after_accepted(
        &["WGAL's registration of the tunnel was removed"],
        snapshot(w, tool),
    )?;
    Ok(ToolResult::ok(
        json!({ "accepted": true, "tunnel_id": id.as_str(), "registered": tunnel_in(&s, &id).is_some() }),
    ))
}

fn switch<W: Watchdog>(
    w: &mut W,
    tool: &'static str,
    arguments: &Value,
    request: impl Fn(TunnelId, bool) -> Request,
) -> Outcomes<ToolResult> {
    let a = args(tool, arguments, &["tunnel_id", "enabled"])?;
    let id = a.tunnel_id()?;
    let enabled = a.bool("enabled")?;
    // Only read, so that the watchdog still judges the request and says
    // why it refuses one, an unknown tunnel included (Codex P2).
    let before = tunnel_in(&snapshot(w, tool)?, &id);
    ask(w, tool, &request(id.clone(), enabled))?;
    let done = match (tool, enabled) {
        ("set_tunnel_monitoring", true) => "the tunnel's monitoring was turned on",
        ("set_tunnel_monitoring", false) => "the tunnel's monitoring was turned off",
        (_, true) => "the tunnel's notifications were turned on",
        (_, false) => "the tunnel's notifications were turned off",
    };
    let t = after_accepted(&[done], find(w, tool, &id))?;
    Ok(ToolResult::ok(Value::Object(changed(before.as_ref(), &t))))
}

fn paused<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let a = args("set_monitoring_paused", arguments, &["paused"])?;
    let paused = a.bool("paused")?;
    ask(
        w,
        "set_monitoring_paused",
        &Request::SetMonitoringPaused { paused },
    )?;
    let done = if paused {
        "monitoring of every tunnel was paused"
    } else {
        "monitoring of every tunnel was resumed"
    };
    let s = after_accepted(&[done], snapshot(w, "set_monitoring_paused"))?;
    Ok(ToolResult::ok(
        json!({ "accepted": true, "monitoring_paused": s.monitoring_paused }),
    ))
}

fn operation<W: Watchdog>(
    w: &mut W,
    tool: &'static str,
    arguments: &Value,
    request: impl Fn(TunnelId) -> Request,
    accepted: &str,
) -> Outcomes<ToolResult> {
    let a = args(tool, arguments, &["tunnel_id", "wait_secs"])?;
    let id = a.tunnel_id()?;
    let wait = a.wait()?;
    find(w, tool, &id)?;
    ask(w, tool, &request(id.clone()))?;
    after_operation(w, tool, &id, wait, accepted)
}

/// `reconnect_tunnel` (owner decision of 2026-11-06, from the MCP agent
/// test): the reconnect, then one traffic check, so the answer says
/// whether traffic came back, also while monitoring is off.
fn reconnect<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "reconnect_tunnel";
    let a = args(tool, arguments, &["tunnel_id", "wait_secs"])?;
    let id = a.tunnel_id()?;
    let wait = a.wait()?;
    find(w, tool, &id)?;
    ask(
        w,
        tool,
        &Request::Reconnect {
            tunnel_id: id.clone(),
        },
    )?;
    const ACCEPTED: &str = "the reconnect was accepted";
    // Give the watchdog a moment to take the operation up before judging it.
    thread::sleep(POLL.min(wait));
    // `reason` says the case in one word, `why` in words (N-05).
    let not_checked = |reason: &str, why: &str| json!({ "checked": false, "recovered": Value::Null, "reason": reason, "why": why });
    // The reconnect was accepted: a read that fails from here on does not
    // make the tool fail (AR-05).
    let unread = |failure: ToolResult| {
        accepted_unread(
            &failure,
            &[ACCEPTED],
            json!({
                "settled": Value::Null,
                "traffic": not_checked(
                    "state_unread",
                    "the tunnel's state could not be read after the reconnect, so traffic was not checked",
                ),
            }),
        )
    };
    let (t, settled_in_time) = until(w, tool, &id, wait, settled).map_err(unread)?;
    let (traffic, t) = if !settled_in_time {
        (
            not_checked(
                "not_settled",
                "the reconnect was still in progress when the wait ended, so traffic was not checked",
            ),
            t,
        )
    } else if t.os_state != OsState::Connected {
        (
            not_checked(
                "not_connected",
                "the tunnel is not connected after the reconnect, so traffic was not checked; see os_state",
            ),
            t,
        )
    } else if t.recovery_target.is_none() && t.observe_targets.is_empty() {
        (
            not_checked(
                "no_targets",
                "the tunnel has no target to probe; set recovery_target or observe_targets with update_tunnel_settings",
            ),
            t,
        )
    } else {
        // The check begins now, by the watchdog's own clock (Codex reviews
        // of 2026-11-06). A probe counts only if it is a new result (not
        // the one the start saw) that started strictly after the start, so
        // a round already under way or one in the same millisecond does
        // not pass for this check. Any sign that the clock moved backwards
        // (a probe stamped later than the snapshot that shows it, or a
        // later snapshot older than the start) makes the check
        // inconclusive, never "down". A result planned before the
        // reconnect is never shown after it: the reconnect advances the
        // tunnel's generation, and the core drops results of an older one
        // (`Watchdog::apply`).
        let at_start = snapshot(w, tool).map_err(unread)?;
        let began = at_start.generated_at_unix_ms;
        let capabilities = at_start.capabilities;
        let seen: Vec<(ProbeTarget, Option<wgal_protocol::ProbeSummary>)> =
            tunnel_in(&at_start, &id)
                .map(|t| {
                    t.recovery_target
                        .iter()
                        .chain(t.observe_targets.iter())
                        .map(|r| (r.target, r.last_probe.clone()))
                        .collect()
                })
                .unwrap_or_default();
        let from_the_future = |t: &TunnelStatus, now: i64| {
            t.recovery_target
                .iter()
                .chain(t.observe_targets.iter())
                .filter_map(|r| r.last_probe.as_ref())
                .any(|p| p.started_at_unix_ms > now || p.finished_at_unix_ms > now)
        };
        let clock_back_at_start =
            tunnel_in(&at_start, &id).is_some_and(|t| from_the_future(&t, began));
        let fresh_of = |t: &TunnelStatus| -> Vec<TargetStatus> {
            t.recovery_target
                .iter()
                .chain(t.observe_targets.iter())
                .filter(|r| {
                    let before = seen
                        .iter()
                        .find(|(target, _)| *target == r.target)
                        .and_then(|(_, p)| p.as_ref());
                    r.last_probe
                        .as_ref()
                        .is_some_and(|p| Some(p) != before && p.started_at_unix_ms > began)
                })
                .cloned()
                .collect()
        };
        let mut clock_back = clock_back_at_start;
        // What ended the check before a result: the probe could not be
        // asked for, or its results could not be read (AR-05).
        let mut stopped: Option<Value> = None;
        let (after, probed) = if clock_back {
            (t, false)
        } else if let Err(failure) = ask(
            w,
            tool,
            &Request::ProbeNow {
                tunnel_id: id.clone(),
            },
        ) {
            // A request that may have reached the watchdog may have started
            // the probe: not "not started" (Codex review of FIX-03, P3-01).
            let unknown = failure.data["details"]["delivery"] == "unknown";
            stopped = Some(not_checked(
                if unknown {
                    "probe_unknown"
                } else {
                    "probe_not_started"
                },
                &format!(
                    "{}, so traffic was not checked: {}",
                    if unknown {
                        "the check's probe may or may not have started (its answer was lost)"
                    } else {
                        "the check's probe could not be asked for"
                    },
                    failure.text
                ),
            ));
            (t, false)
        } else {
            let start = Instant::now();
            loop {
                let read = snapshot(w, tool).and_then(|s| {
                    let now = tunnel_in(&s, &id).ok_or_else(|| not_monitored(tool, &id))?;
                    Ok((s, now))
                });
                let (s, now) = match read {
                    Ok(read) => read,
                    Err(failure) => {
                        stopped = Some(not_checked(
                            "state_unread",
                            &format!(
                                "the check's probe was asked for, but its results could not be read, so traffic was not checked: {}",
                                failure.text
                            ),
                        ));
                        break (t, false);
                    }
                };
                if s.generated_at_unix_ms < began || from_the_future(&now, s.generated_at_unix_ms) {
                    clock_back = true;
                }
                if clock_back || !fresh_of(&now).is_empty() {
                    break (now, !clock_back);
                }
                if start.elapsed() >= wait {
                    break (now, false);
                }
                thread::sleep(POLL.min(wait.saturating_sub(start.elapsed())));
            }
        };
        let fresh = if probed { fresh_of(&after) } else { Vec::new() };
        // An answer shows traffic through the tunnel only where this
        // computer can bind the probe to it (Codex review of 2026-11-06).
        let bound = |r: &TargetStatus| {
            let capability = match r.target.probe {
                ProbeKind::Icmp => capabilities.bound_icmp_probe,
                ProbeKind::TcpConnect { .. } => capabilities.bound_tcp_probe,
            };
            capability == Capability::Supported
        };
        let answers = |r: &TargetStatus| {
            r.last_probe.as_ref().is_some_and(|p| {
                matches!(
                    p.result,
                    ProbeResult::Reply { .. } | ProbeResult::Refused { .. }
                )
            })
        };
        let could_not_run = |r: &TargetStatus| {
            r.last_probe
                .as_ref()
                .is_some_and(|p| matches!(p.result, ProbeResult::Error { .. }))
        };
        let targets: Vec<Value> = fresh
            .iter()
            .map(|r| {
                json!({
                    "target": r.target.value(),
                    "last_probe": last_probe(r),
                    // Only this probe's answer, through a probe bound to
                    // the tunnel, shows the path works: never a probe that
                    // could not run or got no answer (N-05).
                    "through_tunnel_verified": answers(r) && bound(r),
                    "bound_probe": bound(r),
                })
            })
            .collect();
        let errors = fresh.iter().filter(|r| could_not_run(r)).count();
        let check = if let Some(stopped) = stopped {
            stopped
        } else if clock_back {
            not_checked(
                "clock_moved",
                "the computer's clock moved backwards around the check, so new probes cannot be told from earlier ones; call probe_tunnel to check again",
            )
        } else if !probed {
            not_checked(
                "no_new_probe",
                "no probe of this check came back before the wait ended; call probe_tunnel with a longer wait_secs",
            )
        } else if fresh.iter().any(|r| answers(r) && bound(r)) {
            json!({ "checked": true, "recovered": true, "reason": "recovered", "targets": targets })
        } else if fresh.iter().any(answers) {
            json!({
                "checked": true,
                "recovered": Value::Null,
                "reason": "path_unverified",
                "targets": targets,
                "why": "a target answered, but this computer cannot confirm the answer came through the tunnel (through_tunnel_verified is false), so traffic is not confirmed",
            })
        } else if errors == 0 {
            // Every probe ran and none got an answer.
            json!({ "checked": true, "recovered": false, "reason": "no_reply", "targets": targets })
        } else {
            // A probe that could not run (a route that is not the tunnel's,
            // a missing tool) is not a target that did not answer (N-05).
            let why = if errors == fresh.len() {
                "the check's probes could not run (each target's last_probe says why), so whether traffic came back is not known: a probe that cannot run is not a target that did not answer. Call probe_tunnel to check again, or read_event_log for the cause".to_owned()
            } else {
                format!(
                    "no target answered, but the probes of {errors} of the {} targets could not run (each target's last_probe says why), so whether traffic came back is not known. Call probe_tunnel to check again, or read_event_log for the cause",
                    fresh.len()
                )
            };
            json!({
                "checked": false,
                "recovered": Value::Null,
                "reason": "probe_error",
                "targets": targets,
                "why": why,
            })
        };
        (check, after)
    };
    let note = match traffic["reason"].as_str() {
        Some("recovered") => {
            "The tunnel has settled and a target answered through it: traffic came back."
        }
        Some("no_reply") => {
            "The tunnel has settled, but no target answered the check: traffic has not come back. See traffic.targets."
        }
        Some("path_unverified") => {
            "A target answered, but not provably through the tunnel; traffic.why says more."
        }
        Some("probe_error") => {
            "The tunnel has settled, but the check's probes could not run, so it is not known whether traffic came back; traffic.why and traffic.targets say more."
        }
        _ => "Traffic was not checked; traffic.why says why.",
    };
    Ok(ToolResult::ok(json!({
        "accepted": true,
        "settled": settled_in_time,
        "note": note,
        "traffic": traffic,
        "tunnel": summary(&t),
    })))
}

fn probe<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "probe_tunnel";
    let a = args(tool, arguments, &["tunnel_id", "wait_secs"])?;
    let id = a.tunnel_id()?;
    let wait = a.wait()?;
    let s = snapshot(w, tool)?;
    let before = tunnel_in(&s, &id).ok_or_else(|| not_monitored(tool, &id))?;
    // Not monitored: from watchdog 1.5.0 a one-off check (owner decision of
    // 2026-11-06); an older one probes nothing then, so waiting would be
    // for nothing (MCP agent test of 2026-11-06).
    let one_off = s.monitoring_paused || !before.intent.monitoring_enabled;
    let checks_once = ProductVersion::new(ONE_OFF_PROBE_SINCE)
        .is_ok_and(|since| s.host.watchdog_version.is_at_least(&since));
    if one_off && !checks_once && s.monitoring_paused {
        return Err(bad(
            tool,
            format!(
                "monitoring is paused for every tunnel (set_monitoring_paused), and this watchdog ({}) probes nothing then; watchdog {ONE_OFF_PROBE_SINCE} or later checks once. Resuming the pause resumes every tunnel's probes and automatic actions, not only this one's: do it only if the user wants that, and pause it again afterwards, whatever the probe showed, to leave it as it was",
                s.host.watchdog_version.as_str()
            ),
        ));
    }
    if one_off && !checks_once {
        return Err(bad(
            tool,
            format!(
                "monitoring of this tunnel is off, and this watchdog ({}) probes nothing then; watchdog {ONE_OFF_PROBE_SINCE} or later checks once. If the user wants a check, turn monitoring on with set_tunnel_monitoring (enabled: true), probe, and turn it off again to leave it as it was",
                s.host.watchdog_version.as_str()
            ),
        ));
    }
    if before.recovery_target.is_none() && before.observe_targets.is_empty() {
        return Err(bad(
            tool,
            "this tunnel has no target to probe; set recovery_target or observe_targets with update_tunnel_settings",
        ));
    }
    // A round probes every target; a tunnel with only watch-only targets
    // has no recovery target to look at.
    let last = |t: &TunnelStatus| {
        t.recovery_target
            .iter()
            .chain(t.observe_targets.iter())
            .filter_map(|r| r.last_probe.as_ref())
            .map(|p| p.finished_at_unix_ms)
            .max()
    };
    let was = last(&before);
    ask(
        w,
        tool,
        &Request::ProbeNow {
            tunnel_id: id.clone(),
        },
    )?;
    let (t, done) = after_accepted(
        &["a probe of its targets was asked for"],
        until(w, tool, &id, wait, |t| last(t) != was),
    )?;
    Ok(ToolResult::ok(json!({
        "accepted": true,
        "probed": done,
        "one_off": one_off,
        "note": match (one_off, done) {
            (false, true) => "A new probe round finished; see last_probe and the tunnel's light.",
            (false, false) => "No new probe result before the wait ended: a round or a recovery step may still be running. Call health in a little while, or probe_tunnel again with a longer wait_secs.",
            (true, true) => "A one-off check while monitoring is off: last_probe shows what the target answered. The light stays gray and nothing was judged from it; monitoring is still off.",
            (true, false) => "No answer to the one-off check before the wait ended. While monitoring is off WGAL does not follow the tunnel's state: the check reads it only to decide whether to probe and does not show it, and a tunnel that is down is not probed. Call probe_tunnel again with a longer wait_secs, or check the tunnel in the official WireGuard app.",
        },
        // A round probes every target: all their answers.
        "tunnel": detail(&t),
    })))
}

fn auto_recovery<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "set_auto_recovery";
    let a = args(
        tool,
        arguments,
        &["tunnel_id", "enabled", "start_when_stopped_externally"],
    )?;
    let id = a.tunnel_id()?;
    let enabled = a.bool("enabled")?;
    let external = a.opt_bool("start_when_stopped_externally")?;
    let before = tunnel_in(&snapshot(w, tool)?, &id);
    ask(
        w,
        tool,
        &Request::SetAutoRecovery {
            tunnel_id: id.clone(),
            enabled,
        },
    )?;
    // What was done before a later step failed (AR-05).
    let mut done = vec![if enabled {
        "automatic recovery was turned on"
    } else {
        "automatic recovery was turned off"
    }];
    if let Some(enabled) = external {
        ask(
            w,
            tool,
            &Request::SetStartWhenStoppedExternally {
                tunnel_id: id.clone(),
                enabled,
            },
        )
        .map_err(|e| e.noting(&done))?;
        done.push(if enabled {
            "start_when_stopped_externally was turned on"
        } else {
            "start_when_stopped_externally was turned off"
        });
    }
    let t = after_accepted(&done, find(w, tool, &id))?;
    Ok(ToolResult::ok(Value::Object(changed(before.as_ref(), &t))))
}

fn update_settings<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "update_tunnel_settings";
    let a = args_with(
        tool,
        arguments,
        &[
            "tunnel_id",
            "recovery_target",
            "observe_targets",
            "probe",
            "retry",
        ],
        flat_setting_hint,
    )?;
    let id = a.tunnel_id()?;
    let current = find(w, tool, &id)?;
    let mut recovery = current.recovery_target.as_ref().map(|r| r.target);
    if let Some(value) = a.map.get("recovery_target") {
        recovery = if value.is_null() {
            None
        } else {
            Some(target(tool, "recovery_target", value)?)
        };
    }
    let mut observe: Vec<ProbeTarget> = current.observe_targets.iter().map(|o| o.target).collect();
    if let Some(value) = a.map.get("observe_targets") {
        let Some(list) = value.as_array() else {
            return Err(bad(
                tool,
                format!(
                    "observe_targets must be a list of at most {MAX_OBSERVE_TARGETS} targets: the list replaces the current one, [] removes them all, and leaving observe_targets out keeps them"
                ),
            ));
        };
        if list.len() > MAX_OBSERVE_TARGETS {
            return Err(bad(
                tool,
                format!(
                    "observe_targets may have at most {MAX_OBSERVE_TARGETS} targets, not {}",
                    list.len()
                ),
            ));
        }
        observe = list
            .iter()
            .enumerate()
            .map(|(i, v)| target(tool, &format!("observe_targets[{i}]"), v))
            .collect::<Outcomes<_>>()?;
    }
    let mut probe = current.probe;
    if let Some(value) = a.map.get("probe") {
        let Some(map) = value.as_object() else {
            return Err(bad(
                tool,
                "probe must be an object with any of interval_secs, timeout_secs, retries, min_fault_secs; leave probe out to keep them all",
            ));
        };
        let p = args_with(
            tool,
            value,
            &PROBE_FIELDS,
            inner_hint("probe", &PROBE_FIELDS),
        )?;
        let field = |key: &str, range: RangeInclusive<u64>| {
            setting(tool, &format!("probe.{key}"), p.map.get(key), range)
        };
        let interval =
            field("interval_secs", 10..=3600)?.unwrap_or(u64::from(probe.interval_secs()));
        let timeout = field("timeout_secs", 1..=10)?.unwrap_or(u64::from(probe.timeout_secs()));
        let retries = field("retries", 0..=10)?.unwrap_or(u64::from(probe.retries()));
        let min_fault =
            field("min_fault_secs", 0..=3600)?.unwrap_or(u64::from(probe.min_fault_secs()));
        let _ = map;
        probe = ProbeTiming::new(
            narrow(tool, "interval_secs", interval)?,
            narrow(tool, "timeout_secs", timeout)?,
            narrow(tool, "retries", retries)?,
            narrow(tool, "min_fault_secs", min_fault)?,
        )
        .map_err(|e| bad(tool, format!("probe: {e}")))?;
    }
    let mut retry = None;
    if let Some(value) = a.map.get("retry") {
        let r = args_with(
            tool,
            value,
            &RETRY_FIELDS,
            inner_hint("retry", &RETRY_FIELDS),
        )?;
        if !value.is_object() {
            return Err(bad(
                tool,
                "retry must be an object with limit and/or interval_secs; leave retry out to keep both",
            ));
        }
        let limit = match r.map.get("limit") {
            None => current.retry.limit(),
            Some(Value::Null) => None,
            Some(v) => Some(narrow(
                tool,
                "retry.limit",
                whole(tool, "retry.limit", Some(v), 0..=3600)?.unwrap_or(0),
            )?),
        };
        let interval = setting(
            tool,
            "retry.interval_secs",
            r.map.get("interval_secs"),
            10..=10800,
        )?
        .map_or(Ok(current.retry.interval_secs()), |n| {
            narrow(tool, "retry.interval_secs", n)
        })?;
        retry = Some(
            RecoveryRetry::new(limit, interval).map_err(|e| bad(tool, format!("retry: {e}")))?,
        );
    }
    let monitor_changed = ["recovery_target", "observe_targets", "probe"]
        .iter()
        .any(|k| a.map.contains_key(*k));
    if !monitor_changed && retry.is_none() {
        return Err(bad(
            tool,
            "nothing to change: give recovery_target, observe_targets, probe or retry",
        ));
    }
    // What was done before a later step failed (AR-05).
    let mut done: Vec<&str> = Vec::new();
    if monitor_changed {
        let observe_targets =
            BoundedVec::try_from(observe).map_err(|_| bad(tool, "too many observe targets"))?;
        ask(
            w,
            tool,
            &Request::UpdateMonitorSettings {
                tunnel_id: id.clone(),
                settings: MonitorSettings {
                    recovery_target: recovery,
                    observe_targets,
                    probe,
                },
            },
        )?;
        done.push("the recovery target, watch-only targets and probe timing were set");
    }
    if let Some(retry) = retry {
        ask(
            w,
            tool,
            &Request::SetRecoveryRetry {
                tunnel_id: id.clone(),
                retry,
            },
        )
        .map_err(|e| e.noting(&done))?;
        done.push("the retry settings were set");
    }
    let t = after_accepted(&done, find(w, tool, &id))?;
    let mut answer = changed(Some(&current), &t);
    // Every setting it can change, under the same names as get_tunnel's
    // (MCP agent test of 2026-11-06).
    answer.insert("tunnel".to_owned(), detail(&t));
    Ok(ToolResult::ok(Value::Object(answer)))
}

fn notifications<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "set_whole_computer_notifications";
    let a = args(tool, arguments, &["enabled"])?;
    let enabled = a.bool("enabled")?;
    let n = snapshot(w, tool)?.notification;
    ask(
        w,
        tool,
        &Request::UpdateNotificationSettings {
            enabled,
            device_name: n.device_name,
            // Kept as it is; read here, never given out.
            chat_id: n.chat_id,
        },
    )?;
    let done = if enabled {
        "notifications were turned on for the whole computer"
    } else {
        "notifications were turned off for the whole computer"
    };
    let n = after_accepted(&[done], snapshot(w, tool))?.notification;
    Ok(ToolResult::ok(
        json!({ "accepted": true, "enabled": n.enabled, "chat_configured": n.chat_id.is_some(), "token": n.token.value() }),
    ))
}

/// Whether the window lets agents read the chat ID and change the device
/// name, chat ID and Bot Token now (owner decision of 2026-11-06).
fn agents_may_set_telegram(s: &Snapshot) -> bool {
    s.host.mcp_telegram_enabled && has_telegram_switch(s)
}

/// Whether the watchdog has the window's switch at all.
fn has_telegram_switch(s: &Snapshot) -> bool {
    ProductVersion::new(TELEGRAM_SINCE)
        .is_ok_and(|since| s.host.watchdog_version.is_at_least(&since))
}

/// What to tell the user while agents may not set Telegram, `None` while
/// they may.
fn telegram_how_to_allow(s: &Snapshot) -> Option<String> {
    if agents_may_set_telegram(s) {
        None
    } else if has_telegram_switch(s) {
        Some(TELEGRAM_HOW_TO_ALLOW.to_owned())
    } else {
        Some(format!(
            "This watchdog ({}) does not let agents set Telegram; watchdog {TELEGRAM_SINCE} or later does, while the user allows it. Until then the user sets the device name, chat ID and Bot Token on the WGAL window's 通知 (Notifications) page.",
            s.host.watchdog_version.as_str()
        ))
    }
}

/// The notification settings as an agent may see them: never the Bot
/// Token, and the chat ID only while the window lets agents set Telegram.
fn notifications_of(s: &Snapshot) -> Value {
    let n = &s.notification;
    let may_set = agents_may_set_telegram(s);
    let mut view = json!({
        "enabled": n.enabled,
        "device_name": n.device_name.as_str(),
        // What messages give when device_name is blank (watchdog 4.1.0).
        "host_name": n.host_name.as_ref().map(|h| h.as_str()),
        "chat_configured": n.chat_id.is_some(),
        "token": n.token.value(),
        "last_failure": n.last_failure.value(),
        "agent_may_set": may_set,
    });
    match telegram_how_to_allow(s) {
        None => view["chat_id"] = json!(n.chat_id.as_ref().map(ChatId::as_str)),
        Some(how) => view["how_to_allow"] = json!(how),
    }
    view
}

/// What `set_telegram_settings` does with one setting.
enum Change<'a> {
    Keep,
    Set(&'a str),
    Clear,
}

/// The argument an unknown `set_telegram_settings` argument name stands
/// for, if it is one of a few fixed spellings.
fn telegram_meant(key: &str) -> Option<&'static str> {
    let plain: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match plain.as_str() {
        "token" | "bot" | "bottoken" | "telegramtoken" | "telegrambottoken" => Some("bot_token"),
        "chat" | "chatid" | "channel" | "channelid" | "telegramchatid" => Some("chat_id"),
        "device" | "devicename" | "name" | "displayname" => Some("device_name"),
        _ => None,
    }
}

/// `set_telegram_settings` (owner decision of 2026-11-06). Every argument
/// is checked before anything is sent, and nothing is sent while the
/// window does not let agents set Telegram (the watchdog refuses it too).
/// No message here repeats a value given for `bot_token`.
fn telegram<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    const NAMES: [&str; 3] = ["device_name", "chat_id", "bot_token"];
    const TAKES: [&str; 4] = ["device_name", "chat_id", "bot_token", "clear"];
    let tool = "set_telegram_settings";
    // An unknown name is never repeated: it may be the token itself
    // (Codex review of 2026-11-06).
    if let Some(key) = arguments
        .as_object()
        .and_then(|m| m.keys().find(|k| !TAKES.contains(&k.as_str())))
    {
        let meant = telegram_meant(key)
            .map(|to| format!(" (did you mean {to}?)"))
            .unwrap_or_default();
        return Err(bad(
            tool,
            format!(
                "an argument it does not take{meant}; its name is not repeated here, as it may hold a secret. It takes: {}",
                TAKES.join(", ")
            ),
        ));
    }
    let a = args(tool, arguments, &TAKES)?;
    let mut listed: Vec<&str> = Vec::new();
    if let Some(value) = a.map.get("clear") {
        let Some(list) = value.as_array() else {
            return Err(bad(
                tool,
                "clear must be a list of the settings to clear: any of \"device_name\", \"chat_id\", \"bot_token\"",
            ));
        };
        for (i, item) in list.iter().enumerate() {
            let Some(name) = item
                .as_str()
                .and_then(|n| NAMES.iter().copied().find(|m| *m == n))
            else {
                return Err(bad(
                    tool,
                    format!(
                        "clear[{i}] is not a setting name: clear may list only \"device_name\", \"chat_id\" and \"bot_token\""
                    ),
                ));
            };
            if listed.contains(&name) {
                return Err(bad(tool, format!("clear lists {name} twice")));
            }
            listed.push(name);
        }
    }
    let change = |key: &'static str| -> Outcomes<Change<'_>> {
        match (a.map.get(key), listed.contains(&key)) {
            (Some(_), true) => Err(bad(
                tool,
                format!(
                    "{key} is both given and listed in clear: give it, or list it in clear, not both"
                ),
            )),
            (None, true) | (Some(Value::Null), false) => Ok(Change::Clear),
            (None, false) => Ok(Change::Keep),
            (Some(Value::String(text)), false) if text.trim().is_empty() => Ok(Change::Clear),
            (Some(Value::String(text)), false) => Ok(Change::Set(text.trim())),
            (Some(_), false) => Err(bad(
                tool,
                format!("{key} must be a string; null or \"\" clears it"),
            )),
        }
    };
    // The device name as given, not trimmed: any text the user likes
    // (owner decision of 2026-11-06), cut to 64 characters with "..." when
    // longer; "" or null clears it, as the window does.
    let given_device = a.map.get("device_name").and_then(Value::as_str);
    let device = match change("device_name")? {
        Change::Keep => None,
        Change::Clear if given_device.is_none_or(str::is_empty) => Some(DeviceName::fitted("")),
        // Not stored where the token is not secret (Codex review of
        // 2026-11-06), and not repeated.
        Change::Set(text) if looks_like_token(text) => {
            return Err(bad(
                tool,
                "device_name is shaped like a Bot Token, so it was not taken (and is not repeated here): give the token as bot_token, and a name such as Office PC as device_name",
            ));
        }
        Change::Clear | Change::Set(_) => Some(DeviceName::fitted(given_device.unwrap_or(""))),
    };
    let device_cut = given_device.is_some_and(DeviceName::too_long);
    let chat = match change("chat_id")? {
        Change::Keep => None,
        Change::Clear => Some(None),
        Change::Set(text) => Some(Some(ChatId::new(text).map_err(|_| {
            bad(
                tool,
                "chat_id must be a numeric chat ID of 1 to 19 digits not starting with 0, negative for groups and channels (for example -1001234567890), or a public @username of 1 to 37 letters, digits or '_' starting with a letter",
            )
        })?)),
    };
    let token = match change("bot_token")? {
        Change::Keep => None,
        Change::Clear => Some(None),
        // The check's own words, never the value.
        Change::Set(text) => Some(Some(BotToken::new(text).map_err(|e| {
            bad(
                tool,
                format!("bot_token is not a Telegram bot token ({e}): it is <bot id>:<secret>, as @BotFather gives it"),
            )
        })?)),
    };
    if device.is_none() && chat.is_none() && token.is_none() {
        return Err(bad(
            tool,
            "nothing to change: give device_name, chat_id or bot_token, or list them in clear",
        ));
    }
    let before = snapshot(w, tool)?;
    if let Some(how) = telegram_how_to_allow(&before) {
        // As the watchdog would refuse it, where it has the switch.
        let (code, reason) = if has_telegram_switch(&before) {
            (
                json!(ErrorCode::Unauthorized),
                json!(ErrorReason::McpTelegramDisabled),
            )
        } else {
            (Value::Null, Value::Null)
        };
        return Err(ToolResult::fail(
            format!("{tool}: agents may not set Telegram now, so nothing was sent. {how}"),
            json!({ "code": code, "reason": reason, "hint": how }),
        ));
    }
    let n = &before.notification;
    let replaced = matches!(token, Some(Some(_)));
    let mut done: Vec<&str> = Vec::new();
    match token {
        Some(Some(token)) => {
            ask(w, tool, &Request::SetBotToken { token })?;
            done.push("the Bot Token was replaced");
        }
        Some(None) => {
            ask(w, tool, &Request::ClearBotToken)?;
            done.push("the Bot Token was cleared");
        }
        None => {}
    }
    if device.is_some() || chat.is_some() {
        let said = [
            device.as_ref().map(|d| {
                if d.as_str().is_empty() {
                    "the device name was cleared"
                } else {
                    "the device name was changed"
                }
            }),
            chat.as_ref().map(|c| {
                if c.is_none() {
                    "the chat ID was cleared"
                } else {
                    "the chat ID was changed"
                }
            }),
        ];
        let request = Request::UpdateNotificationSettings {
            enabled: n.enabled,
            device_name: device.unwrap_or_else(|| n.device_name.clone()),
            chat_id: chat.unwrap_or_else(|| n.chat_id.clone()),
        };
        ask(w, tool, &request).map_err(|e| e.noting(&done))?;
        done.extend(said.into_iter().flatten());
    }
    // Every change was accepted: a failed read now is no failure (AR-05).
    let after = after_accepted(&done, snapshot(w, tool))?;
    // The chat ID only while agents may read it in both snapshots: the
    // user may have withdrawn it in between (Codex review of 2026-11-06).
    let show_chat = agents_may_set_telegram(&before) && agents_may_set_telegram(&after);
    let view = |s: &Snapshot| {
        let chat = if show_chat {
            json!(s.notification.chat_id.as_ref().map(ChatId::as_str))
        } else {
            json!(s.notification.chat_id.as_ref().map(|_| "configured"))
        };
        json!({
            "device_name": s.notification.device_name.as_str(),
            "chat_id": chat,
            "bot_token": s.notification.token.value(),
        })
    };
    let mut changes = Vec::new();
    diff("", &view(&before), &view(&after), &mut changes);
    if replaced {
        // A replaced token reads the same as the one before: stored.
        changes.retain(|c| c["field"] != json!("bot_token"));
        changes.push(json!({ "field": "bot_token", "from": n.token.value(), "to": "replaced" }));
    }
    Ok(ToolResult::ok(json!({
        "accepted": true,
        "changed": changes,
        "note": if device_cut {
            "device_name had more than 64 characters, so it was cut to 61 and '...'. changed lists each setting that changed, from and to; the Bot Token only as stored, not_set or replaced. send_test_notification checks that messages arrive."
        } else {
            "changed lists each setting that changed, from and to; the Bot Token only as stored, not_set or replaced. send_test_notification checks that messages arrive."
        },
        "notifications": notifications_of(&after),
    })))
}

fn test_notification<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "send_test_notification";
    let a = args(tool, arguments, &["wait_secs"])?;
    let wait = a.wait()?;
    let before = snapshot(w, tool)?
        .notification
        .last_test
        .map(|t| t.requested_at_unix_ms);
    ask(w, tool, &Request::SendTestNotification)?;
    let start = Instant::now();
    loop {
        let last = after_accepted(&["the test message was asked for"], snapshot(w, tool))?
            .notification
            .last_test;
        let current = last.filter(|t| Some(t.requested_at_unix_ms) != before);
        if let Some(t) = &current
            && t.state != TestNotificationState::Sending
        {
            return Ok(ToolResult::ok(
                json!({ "accepted": true, "result": t.state.value(), "failure": t.failure.value() }),
            ));
        }
        if start.elapsed() >= wait {
            return Ok(ToolResult::ok(
                json!({ "accepted": true, "result": "sending", "note": "No answer from Telegram yet; call health to see notifications.last_failure later." }),
            ));
        }
        thread::sleep(POLL);
    }
}

fn host_settings<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "update_host_settings";
    let a = args(tool, arguments, &["workers", "time_zone"])?;
    let host = snapshot(w, tool)?.host;
    let workers = match a.map.get("workers") {
        None => host.workers.configured,
        Some(Value::Null) => None,
        Some(v) => {
            let n = whole(
                tool,
                "workers",
                Some(v),
                u64::from(MIN_WORKERS)..=u64::from(MAX_WORKERS),
            )?
            .unwrap_or(0);
            Some(
                WorkerCount::new(narrow(tool, "workers", n)?)
                    .map_err(|e| bad(tool, format!("workers: {e}")))?,
            )
        }
    };
    let time_zone = match a.map.get("time_zone") {
        None => host.time_zone.configured,
        Some(Value::Null) => None,
        Some(Value::String(name)) => Some(TimeZoneName::new(name.trim()).map_err(|_| {
            bad(tool, format!("time_zone {} is not an IANA time zone name; use one such as Asia/Taipei, Europe/Berlin or UTC, or null to follow the system", shown(format!("{name:?}"))))
        })?),
        Some(other) => return Err(bad(tool, format!("time_zone must be a string such as \"Asia/Taipei\" or null, not {}", shown(other)))),
    };
    ask(
        w,
        tool,
        &Request::UpdateHostSettings {
            settings: HostSettings { workers, time_zone },
        },
    )?;
    let host = after_accepted(&["the host settings were set"], snapshot(w, tool))?.host;
    Ok(ToolResult::ok(
        json!({ "accepted": true, "workers": host.workers.value(), "time_zone": host.time_zone.value() }),
    ))
}

fn export<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    args("export_settings", arguments, &[])?;
    match ask(w, "export_settings", &Request::ExportSettings)? {
        Response::SettingsExport { document } => Ok(ToolResult::ok(json!({
            "document": document.value(),
            "note": "Keep this document; import_settings with it puts these settings back.",
        }))),
        _ => Err(bad(
            "export_settings",
            "the watchdog answered something other than settings",
        )),
    }
}

/// The settings document in `value`, or what is wrong with it and where.
fn document(tool: &str, value: Option<&Value>) -> Outcomes<SettingsDocument> {
    let Some(value) = value else {
        return Err(bad(
            tool,
            "document is required: the object export_settings gave",
        ));
    };
    if !value.is_object() {
        return Err(bad(
            tool,
            "document must be the object export_settings gave",
        ));
    }
    // What it is first: another kind of file or version is said as such.
    if value.get("format") != Some(&json!("wgal-settings")) {
        return Err(bad(
            tool,
            "document.format must be \"wgal-settings\": give the document export_settings gave",
        ));
    }
    let version = value.get("version");
    if version != Some(&json!(wgal_protocol::SETTINGS_DOCUMENT_VERSION)) {
        return Err(bad(
            tool,
            format!(
                "document.version is {}; this WGAL reads version {} only",
                version.map_or("missing".to_owned(), shown),
                wgal_protocol::SETTINGS_DOCUMENT_VERSION
            ),
        ));
    }
    serde_json::from_value::<SettingsDocument>(value.clone()).map_err(|whole| {
        // Say which entry is wrong when one is.
        if let Some(list) = value.get("tunnels").and_then(Value::as_array) {
            for (i, entry) in list.iter().enumerate() {
                if let Err(e) = serde_json::from_value::<TunnelSettingsEntry>(entry.clone()) {
                    return bad(tool, format!("document.tunnels[{i}]: {}", shown(e)));
                }
            }
        }
        if let Some(host) = value.get("host")
            && let Err(e) = serde_json::from_value::<wgal_protocol::HostSettingsEntry>(host.clone())
        {
            return bad(tool, format!("document.host: {}", shown(e)));
        }
        bad(tool, format!("document: {}", shown(whole)))
    })
}

fn import<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "import_settings";
    let a = args(
        tool,
        arguments,
        &[
            "document",
            "match_by",
            "register_missing",
            "monitor_new",
            "dry_run",
        ],
    )?;
    let match_by = match a.map.get("match_by") {
        None => ImportMatch::Id,
        Some(Value::String(s)) if s == "id" => ImportMatch::Id,
        Some(Value::String(s)) if s == "name" => ImportMatch::Name,
        Some(other) => {
            return Err(bad(
                tool,
                format!("match_by must be \"id\" or \"name\", not {}", shown(other)),
            ));
        }
    };
    let options = ImportOptions {
        match_by,
        register_missing: a.opt_bool("register_missing")?.unwrap_or(false),
        monitor_new: a.opt_bool("monitor_new")?.unwrap_or(false),
        dry_run: a.opt_bool("dry_run")?.unwrap_or(false),
    };
    let document = document(tool, a.map.get("document"))?;
    let given = document.clone();
    let (dry_run, monitor_new) = (options.dry_run, options.monitor_new);
    let result = match ask(w, tool, &Request::ImportSettings { document, options })? {
        Response::SettingsImport { result } => result,
        _ => {
            return Err(bad(
                tool,
                "the watchdog answered something other than an import result",
            ));
        }
    };
    if !dry_run {
        return Ok(ToolResult::ok(json!({ "result": result.value() })));
    }
    // A preview also says what would change, field by field (MCP agent
    // test of 2026-11-06: "applied" alone did not say whether anything
    // would).
    let live = snapshot(w, tool)?;
    let current = match ask(w, tool, &Request::ExportSettings)? {
        Response::SettingsExport { document } => document,
        _ => {
            return Err(bad(
                tool,
                "the watchdog answered something other than settings",
            ));
        }
    };
    Ok(ToolResult::ok(json!({
        "result": result.value(),
        "changes": import_changes(&current, &given, &result, monitor_new, &live),
        "note": "A dry run: nothing was changed. changes follows the watchdog's verdict for each part (outcome): for a part it would set, each setting that would change, from the current value to the document's (an empty list: nothing would change); for a part it would not set, changes is null and outcome says why. start_when_stopped_externally is not predicted: it is set as the document has it where the access list allows.",
    })))
}

/// What importing `given` would change from `current`, part by part as
/// the watchdog's dry run `result` judged it (Codex P2): only a part the
/// import would set (`applied`, or `registered` for a tunnel it would
/// register) is compared, field by field, with the values the watchdog
/// would set (`import_tunnel` in the runtime's driver): a tunnel it
/// registers is monitored only with `monitor_new`, automatic recovery is
/// on only when its outcome is `on` and is armed (pending) when it waits
/// for its baseline; `start_when_stopped_externally` is left out, since
/// whether the access list allows it is not known here. Any other part
/// has its outcome and no changes.
fn import_changes(
    current: &SettingsDocument,
    given: &SettingsDocument,
    result: &ImportResult,
    monitor_new: bool,
    live: &Snapshot,
) -> Value {
    let (current, given) = (current.value(), given.value());
    let sets = |o: &ImportOutcome| matches!(o, ImportOutcome::Applied | ImportOutcome::Registered);
    let field = |doc: &Value, key: &str| {
        let mut m = Map::new();
        m.insert(key.to_owned(), doc["host"][key].clone());
        Value::Object(m)
    };
    let host_part = |keys: &[&str]| {
        let mut out = Vec::new();
        for key in keys {
            diff("host", &field(&current, key), &field(&given, key), &mut out);
        }
        Value::Array(out)
    };
    let host = json!({
        "outcome": result.host.value(),
        "changes": if sets(&result.host) { host_part(&["workers", "time_zone"]) } else { Value::Null },
    });
    // An older watchdog reports the switch with the host settings.
    let switch = result.notifications.as_ref().unwrap_or(&result.host);
    let notifications = json!({
        "outcome": switch.value(),
        "changes": if sets(switch) { host_part(&["notifications_enabled"]) } else { Value::Null },
    });
    let none = Vec::new();
    let entries = given["tunnels"].as_array().unwrap_or(&none);
    // A tunnel's current settings come from its status, not from the
    // export, whose auto_recovery is "on or armed" (Codex P2): the switch
    // and whether it waits for its baseline are compared apart.
    let now = |id: &TunnelId| {
        let mut m = live
            .tunnels
            .iter()
            .find(|t| &t.tunnel_id == id)
            .map(settings_of)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        m.remove("start_when_stopped_externally");
        m
    };
    let own = |v: Option<&Value>| {
        let mut m = v.and_then(Value::as_object).cloned().unwrap_or_default();
        m.remove("tunnel_id");
        m.remove("name");
        m.remove("start_when_stopped_externally");
        m
    };
    let tunnels: Vec<Value> = result
        .tunnels
        .iter()
        .map(|item| {
            let entry = entries.get(usize::from(item.index));
            let (changes, note) = match (entry, &item.tunnel_id, sets(&item.outcome)) {
                (None, ..) => (
                    Value::Null,
                    json!("the watchdog's answer names an entry the document does not have; nothing is predicted for it"),
                ),
                (Some(entry), Some(id), true) => {
                    // A tunnel the import would register is not here yet:
                    // changes lists each setting it would get a value for.
                    let now = now(id);
                    let mut want = own(Some(entry));
                    if matches!(item.outcome, ImportOutcome::Registered) {
                        let monitored = monitor_new && entry["monitoring"] == json!(true);
                        want.insert("monitoring".to_owned(), json!(monitored));
                    }
                    let on = matches!(item.auto_recovery, AutoRecoveryImport::On);
                    want.insert("auto_recovery".to_owned(), json!(on));
                    // Armed while it waits for its baseline; any other
                    // outcome disarms it. A tunnel that is not armed and
                    // would not be does not mention it.
                    let waits = matches!(item.auto_recovery, AutoRecoveryImport::WaitingForBaseline);
                    if waits || now.get("auto_recovery_pending") == Some(&json!(true)) {
                        want.insert("auto_recovery_pending".to_owned(), json!(waits));
                    } else {
                        want.remove("auto_recovery_pending");
                    }
                    let mut now = now;
                    if want.get("auto_recovery_pending").is_none() {
                        now.remove("auto_recovery_pending");
                    }
                    let mut out = Vec::new();
                    diff("", &Value::Object(now), &Value::Object(want), &mut out);
                    (Value::Array(out), Value::Null)
                }
                _ => (Value::Null, Value::Null),
            };
            json!({
                "index": item.index,
                "tunnel_id": item.tunnel_id.as_ref().map(TunnelId::as_str),
                "name": item.name.as_str(),
                "outcome": item.outcome.value(),
                "auto_recovery": item.auto_recovery.value(),
                "changes": changes,
                "note": note,
            })
        })
        .collect();
    json!({ "host": host, "notifications": notifications, "tunnels": tunnels })
}

/// The event log, as the window's Log page reads it (MCP agent test of
/// 2026-11-06: agents could only be told where the file is, in a folder
/// they may not read).
fn read_event_log<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    let tool = "read_event_log";
    let a = args(
        tool,
        arguments,
        &[
            "tunnel_id",
            "scope",
            "limit",
            "before_seq",
            "skip_agent_requests",
        ],
    )?;
    let skip_agents = a.opt_bool("skip_agent_requests")?.unwrap_or(false);
    let scope = match a.map.get("scope") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(other) => {
            return Err(bad(
                tool,
                format!(
                    "scope must be \"all\", \"system\" or \"watchdog\", not {}",
                    shown(other)
                ),
            ));
        }
    };
    let filter = match (a.map.get("tunnel_id").is_some_and(|v| !v.is_null()), scope) {
        (true, None) => EventFilter::Tunnel {
            tunnel_id: a.tunnel_id()?,
        },
        (true, Some(_)) => {
            return Err(bad(
                tool,
                "give tunnel_id or scope, not both: tunnel_id already picks that tunnel's lines",
            ));
        }
        (false, None | Some("all")) => EventFilter::All,
        (false, Some("system")) => EventFilter::System,
        (false, Some("watchdog")) => EventFilter::Watchdog,
        (false, Some(other)) => {
            return Err(bad(
                tool,
                format!(
                    "scope must be \"all\", \"system\" or \"watchdog\", not {}",
                    shown(format!("{other:?}"))
                ),
            ));
        }
    };
    let page = u64::try_from(MAX_EVENT_PAGE).unwrap_or(u64::MAX);
    let limit = whole(tool, "limit", a.map.get("limit"), 1..=page)?.unwrap_or(20);
    let limit = usize::try_from(limit).unwrap_or(MAX_EVENT_PAGE);
    let before_seq = whole(tool, "before_seq", a.map.get("before_seq"), 1..=u64::MAX)?;
    let names = snapshot(w, tool)?;
    let (entries, more) = match ask(w, tool, &Request::ReadEventLog { filter, before_seq })? {
        Response::EventLog { entries, more } => (entries.into_vec(), more),
        _ => {
            return Err(bad(
                tool,
                "the watchdog answered something other than an event log page",
            ));
        }
    };
    // Leaving lines out happens within the page; the next page starts
    // after the last line looked at, so none is skipped over.
    let kept: Vec<&EventEntry> = entries
        .iter()
        .filter(|e| !(skip_agents && matches!(e.event, LogEvent::AgentRequest { .. })))
        .collect();
    let shown = &kept[..kept.len().min(limit)];
    let more = more || kept.len() > shown.len();
    let next = if kept.len() > shown.len() {
        shown.last().map(|e| e.seq)
    } else {
        entries.last().map(|e| e.seq)
    };
    let lines: Vec<Value> = shown
        .iter()
        .map(|e| {
            let name = e
                .tunnel_id
                .as_ref()
                .and_then(|id| tunnel_in(&names, id))
                .map(|t| t.display_name.as_str().to_owned());
            json!({
                "seq": e.seq,
                "at": utc(e.at_unix_ms),
                "at_unix_ms": e.at_unix_ms,
                "tunnel_id": e.tunnel_id.as_ref().map(TunnelId::as_str),
                "tunnel_name": name,
                "removed_tunnel": e.removed_tunnel,
                "event": e.event.value(),
            })
        })
        .collect();
    Ok(ToolResult::ok(json!({
        "entries": lines,
        "more": more,
        "next_before_seq": if more { next } else { None },
    })))
}

fn log_locations<W: Watchdog>(w: &mut W, arguments: &Value) -> Outcomes<ToolResult> {
    args("log_locations", arguments, &[])?;
    let found = w
        .log_locations()
        .map_err(|u| unreachable("log_locations", u))?;
    let mut paths = Map::new();
    for (name, path) in found {
        paths.insert(name, json!(path.display().to_string()));
    }
    Ok(ToolResult::ok(json!({
        "paths": paths,
        "note": "read_event_log reads the event log for you. The file is JSON lines, newest last; it holds no secrets. Reading the file itself needs the permissions of its folder (administrators on Windows, root on Linux, your own on macOS).",
    })))
}

#[cfg(test)]
mod tests;
