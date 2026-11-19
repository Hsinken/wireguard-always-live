//! Each command: the requests it sends, in order, what it reads back to
//! show what it did, and how it ends (owner decisions of 2026-11-18, the
//! plan's §3.2). A change is sent once and never again; when the state
//! cannot be read back after it was accepted, that is said, with what was
//! already done, and the end is "may have been carried out" (4).
//!
//! Exit codes: 0 done; 1 refused by the watchdog, or done with a failed
//! result; 2 a mistake in the arguments or the input, nothing changed; 3
//! the watchdog could not be reached, nothing changed; 4 it may have been
//! carried out but that is not confirmed; 128 and a signal's number when
//! the token prompt was interrupted.
//!
//! The language (owner decisions of 2026-11-19, the plan's §3.1 and §3.2):
//! a run writes in the language its `--lang` names; without one, it first
//! reads the watchdog's state, even to say a mistake, and writes in the
//! shared language kept there, or in English when there is none or the
//! state cannot be read. Only `--version` reads nothing. That first look is
//! also the state the command starts from, unless it then asks for
//! something on this computer (the Bot Token, a typed yes, the settings
//! file), when it looks again.
//!
//! Everything written, to standard output and standard error, in text and
//! JSON alike, has whatever is shaped like a Bot Token replaced
//! ([`wgal_client::token::without_tokens`]); `export`, whose output must go
//! back in unchanged, refuses instead.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::time::Duration;

use serde_json::{Map, Value, json};
use wgal_client::token::{looks_like_token, without_tokens};
use wgal_client::{Delivery, Unreachable, Watchdog};
use wgal_protocol::{
    BotToken, BoundedVec, DeviceName, ErrorCode, ErrorReason, EventFilter, HostSettings,
    LanguageTag, Light, MAX_MESSAGE_BYTES, MonitorSettings, OsState, Outcome, ProbeTarget,
    ProductVersion, ProtocolError, RecoveryPhase, RecoveryRetry, Request, Response,
    SettingsDocument, Snapshot, TestNotificationState, TunnelId, TunnelStatus,
};

use crate::args::{self, Command, Invocation, LanguageArg, Notify, Operation, SetArgs, Usage};
use crate::console::{Console, Input, Line};
use crate::languages;
use crate::show::{self, Clock};
use crate::text::{self, tr};

/// The longest Bot Token line read: far above any token, and refused, not
/// cut, beyond it.
pub const MAX_TOKEN_BYTES: usize = 256;
/// The largest settings file read from standard input: one message.
pub const MAX_IMPORT_BYTES: usize = MAX_MESSAGE_BYTES as usize;
/// The pause between two looks at the watchdog while waiting.
const POLL: Duration = Duration::from_secs(1);
/// The oldest watchdog with every request `wgal` sends.
pub const MIN_WATCHDOG: &str = "2.1.0";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a run prints and how it ends.
#[derive(Debug)]
pub struct Report {
    code: u8,
    stdout: String,
    stderr: String,
    /// What the command did, for what a failed write of its result says.
    effect: Effect,
}

/// What a command did to the watchdog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    /// It only read: it changed nothing.
    Read,
    /// It was a change, and it was done.
    Changed,
    /// It failed: its message says what was done before (`done`), and
    /// nothing more may be claimed (Codex review of the CLI, second round,
    /// N-P2-5).
    Failed,
}

impl Report {
    #[cfg(test)]
    pub fn code(&self) -> u8 {
        self.code
    }

    #[cfg(test)]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    #[cfg(test)]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Nothing shaped like a Bot Token leaves, whatever it came from.
    fn hidden(self) -> Self {
        Self {
            stdout: without_tokens(&self.stdout),
            stderr: without_tokens(&self.stderr),
            ..self
        }
    }

    /// Writes the report and says how the program ends. A result that
    /// cannot be written whole (a full disk under `wgal export >
    /// file`, a closed pipe) does not end as done (Codex review of the
    /// CLI, P2-4): as not done (1) when nothing was changed, as "may have
    /// been carried out" (4) when a change was, said on standard error; a
    /// failure keeps its own code, and its message, also on standard
    /// error, says what was done before it. Nothing is sent again.
    pub fn emit(&self, out: &mut impl io::Write, err: &mut impl io::Write) -> u8 {
        let mut code = self.code;
        let mut stderr = self.stderr.clone();
        if let Err(e) = out
            .write_all(self.stdout.as_bytes())
            .and_then(|()| out.flush())
        {
            code = match self.effect {
                Effect::Read => 1,
                Effect::Changed => 4,
                Effect::Failed => code,
            };
            let error = e.kind();
            let _ = writeln!(
                stderr,
                "wgal: {}",
                match self.effect {
                    Effect::Read => tr!("unwritten.read", error = error),
                    Effect::Changed => tr!("unwritten.changed", error = error),
                    Effect::Failed => tr!("unwritten.failed", error = error),
                }
            );
        }
        let _ = err.write_all(stderr.as_bytes()).and_then(|()| err.flush());
        code
    }
}

/// A command that worked: what it says and its data for `--json`, and,
/// when the command alone does not tell, whether it changed anything.
#[derive(Debug)]
struct Done {
    text: String,
    data: Value,
    effect: Option<Effect>,
}

impl Done {
    fn new(text: impl Into<String>, data: Value) -> Self {
        Self {
            text: text.into(),
            data,
            effect: None,
        }
    }

    /// What this run did, whatever the command usually does.
    fn with_effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }
}

/// How a command failed (the plan's §3.2 table).
#[derive(Debug)]
enum Kind {
    Usage,
    Input,
    Refused { code: Value, reason: Value },
    Failed,
    ExportRefused { place: String },
    Unreachable(Delivery),
    Unconfirmed,
    Interrupted(u8),
}

#[derive(Debug)]
struct Inner {
    kind: Kind,
    message: String,
    done: Vec<String>,
    /// The command's result, for a failed one.
    data: Option<Value>,
    /// The help topic that shows the right use.
    topic: Option<&'static str>,
}

/// A failed command, boxed so every step's result stays small.
#[derive(Debug)]
struct Failure(Box<Inner>);

impl std::ops::Deref for Failure {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl std::ops::DerefMut for Failure {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.0
    }
}

type Step<T> = Result<T, Failure>;

impl Failure {
    fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self(Box::new(Inner {
            kind,
            message: message.into(),
            done: Vec::new(),
            data: None,
            topic: None,
        }))
    }

    fn usage(u: Usage) -> Self {
        let mut f = Self::new(Kind::Usage, u.message);
        f.topic = u.topic;
        f
    }

    fn input(message: impl Into<String>) -> Self {
        Self::new(Kind::Input, message)
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::new(Kind::Failed, message)
    }

    fn with(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The way to the watchdog failed: wgal's own sentence for how, in the
    /// language of this run, with `wgal-client`'s English words after it
    /// (the plan's §3.4).
    fn unreachable(u: Unreachable) -> Self {
        let message = match u.delivery {
            Delivery::NotSent => tr!("failure.unreachable_not_sent", detail = u.message),
            Delivery::Unknown => tr!("failure.unreachable_unknown", detail = u.message),
        };
        Self::new(Kind::Unreachable(u.delivery), message)
    }

    fn refused(e: &ProtocolError) -> Self {
        let code = serde_json::to_value(e.code).unwrap_or(Value::Null);
        let reason = serde_json::to_value(e.reason).unwrap_or(Value::Null);
        let detail = e.message.as_str();
        let code_name = code.as_str().unwrap_or("error");
        let refusal = match reason.as_str() {
            Some(reason) => tr!(
                "failure.refused_with_reason",
                message = detail,
                code = code_name,
                reason = reason
            ),
            None => tr!("failure.refused", message = detail, code = code_name),
        };
        let message = match hint(e.reason, e.code) {
            Some(hint) => tr!("failure.refused_hint", refusal = refusal, hint = hint),
            None => refusal,
        };
        Self::new(Kind::Refused { code, reason }, message)
    }

    /// An answer of another kind than the request's: it may have been
    /// carried out.
    fn unexpected() -> Self {
        Self::new(
            Kind::Unreachable(Delivery::Unknown),
            tr!("failure.unexpected_answer"),
        )
    }

    /// Says what was already done before this failure.
    fn noting(mut self, done: &[String]) -> Self {
        self.done.extend_from_slice(done);
        self
    }

    /// A later step's failure, once an earlier change of the same run was
    /// accepted (the plan's §4): one that was not sent is a failed result
    /// (1), since the earlier change stands, never "nothing changed" (3).
    /// A refusal stays one (1), and one sent without an answer stays "may
    /// have been carried out" (4).
    fn after_accepted(self, done: &[String]) -> Self {
        let f = if matches!(self.kind, Kind::Unreachable(Delivery::NotSent)) {
            Self::failed(tr!("failure.next_not_sent", failure = self.message.clone()))
        } else {
            self
        };
        f.noting(done)
    }

    /// A failure to read the state after a change was accepted: it was
    /// carried out, but what it led to is not known.
    fn accepted_unread(mut self, done: &[String]) -> Self {
        self.kind = Kind::Unconfirmed;
        self.message = tr!("failure.accepted_unread", failure = self.message);
        self.noting(done)
    }

    fn exit(&self) -> u8 {
        match self.kind {
            Kind::Usage | Kind::Input => 2,
            Kind::Refused { .. } | Kind::Failed | Kind::ExportRefused { .. } => 1,
            Kind::Unreachable(Delivery::NotSent) => 3,
            Kind::Unreachable(Delivery::Unknown) | Kind::Unconfirmed => 4,
            Kind::Interrupted(code) => code,
        }
    }

    fn json(&self) -> Value {
        let (kind, extra) = match &self.kind {
            Kind::Usage => ("usage", Map::new()),
            Kind::Input => ("input", Map::new()),
            Kind::Refused { code, reason } => {
                let mut m = Map::new();
                m.insert(
                    "refused".to_owned(),
                    json!({ "code": code, "reason": reason }),
                );
                ("refused", m)
            }
            Kind::Failed => ("failed", Map::new()),
            Kind::ExportRefused { place } => {
                let mut m = Map::new();
                m.insert("where".to_owned(), json!(place));
                ("export_refused", m)
            }
            Kind::Unreachable(delivery) => {
                let mut m = Map::new();
                m.insert(
                    "delivery".to_owned(),
                    json!(match delivery {
                        Delivery::NotSent => "not_sent",
                        Delivery::Unknown => "unknown",
                    }),
                );
                ("unreachable", m)
            }
            Kind::Unconfirmed => {
                let mut m = Map::new();
                m.insert("accepted".to_owned(), json!(true));
                ("unconfirmed", m)
            }
            Kind::Interrupted(_) => ("interrupted", Map::new()),
        };
        let mut error = Map::new();
        error.insert("kind".to_owned(), json!(kind));
        error.insert("exit".to_owned(), json!(self.exit()));
        error.insert("message".to_owned(), json!(self.text()));
        error.insert("done".to_owned(), json!(self.done));
        error.extend(extra);
        if let Some(data) = &self.data {
            error.insert("result".to_owned(), data.clone());
        }
        json!({ "error": Value::Object(error) })
    }

    /// The message, with what was done before it.
    fn text(&self) -> String {
        let done = self
            .done
            .iter()
            .cloned()
            .reduce(|done, more| tr!("failure.done_and", done = done, more = more));
        match done {
            None => self.message.clone(),
            Some(done) => tr!("failure.before_this", failure = self.message, done = done),
        }
    }
}

/// What the reader can do about a refusal, when there is something.
fn hint(reason: Option<ErrorReason>, code: ErrorCode) -> Option<String> {
    Some(match reason {
        Some(ErrorReason::NeedsAdministratorOrListedTunnel) => {
            tr!("hint.needs_administrator_or_listed_tunnel")
        }
        Some(ErrorReason::CallerNotAllowed) => tr!("hint.caller_not_allowed"),
        Some(ErrorReason::BaselineNotPassed) => tr!("hint.baseline_not_passed"),
        Some(ErrorReason::TunnelNotRegistered) => tr!("hint.tunnel_not_registered"),
        Some(ErrorReason::TunnelNotDiscovered) => tr!("hint.tunnel_not_discovered"),
        Some(ErrorReason::OperationInProgress) => tr!("hint.operation_in_progress"),
        Some(ErrorReason::UnknownTimeZone) => tr!("hint.unknown_time_zone"),
        Some(ErrorReason::CannotControl) => tr!("hint.cannot_control"),
        Some(ErrorReason::BindingLost) => tr!("hint.binding_lost"),
        Some(ErrorReason::StorageLoadFailed | ErrorReason::StorageSaveFailed) => {
            tr!("hint.storage_failed")
        }
        Some(ErrorReason::SecretStoreUnavailable) => tr!("hint.secret_store_unavailable"),
        _ if code == ErrorCode::Busy => tr!("hint.busy"),
        _ => return None,
    })
}

/// Runs the command line `arguments` against `watchdog`.
pub fn run<W: Watchdog, C: Console>(arguments: &[OsString], w: &mut W, c: &mut C) -> Report {
    text::english();
    // Asked for JSON even when the rest is wrong.
    let json_asked = arguments.iter().any(|a| a == "--json");
    let chosen = args::chosen_language(arguments);
    let parsed = args::parse(arguments);
    // The language of this run: the one `--lang` names, or the shared one,
    // read first (the module documentation).
    let version_only = matches!(
        parsed,
        Ok(Invocation {
            command: Command::Version,
            ..
        })
    );
    let first = match chosen {
        Some(tag) => {
            text::set(tag);
            None
        }
        None if version_only => None,
        None => {
            let s = snapshot(w);
            if let Ok(s) = &s {
                write_in_shared_language(s);
            }
            Some(s)
        }
    };
    // A mistake is said again, now in that language.
    let parsed = if parsed.is_err() {
        args::parse(arguments)
    } else {
        parsed
    };
    let report = match parsed {
        Err(u) => failure_report(json_asked, &Failure::usage(u)),
        Ok(Invocation { json, command, .. }) => {
            match execute(&command, w, c, first, chosen.is_some()) {
                Ok(done) => {
                    let effect = done.effect.unwrap_or(if changes_nothing(&command) {
                        Effect::Read
                    } else {
                        Effect::Changed
                    });
                    Report {
                        effect,
                        ..success_report(json, done)
                    }
                }
                Err(f) => failure_report(json, &f),
            }
        }
    };
    report.hidden()
}

/// Writes in the shared language `s` names from now on, when it is one of
/// the 13; in English otherwise.
fn write_in_shared_language(s: &Snapshot) {
    match s
        .notification
        .cli_language
        .as_ref()
        .and_then(|tag| text::known(tag.as_str()))
    {
        Some(tag) => {
            text::set(tag);
        }
        None => text::english(),
    }
}

/// Whether a command only reads.
fn changes_nothing(command: &Command) -> bool {
    matches!(
        command,
        Command::Help(_)
            | Command::Version
            | Command::Status
            | Command::List
            | Command::Show(_)
            | Command::Discover
            | Command::Log { .. }
            | Command::Export
            | Command::Notify(Notify::Show | Notify::Language(None))
            | Command::Language(LanguageArg::Show)
    ) || matches!(command, Command::Import(o) if o.dry_run)
}

fn success_report(json: bool, done: Done) -> Report {
    let stdout = if json {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({ "result": done.data })).unwrap_or_default()
        )
    } else {
        done.text
    };
    Report {
        code: 0,
        stdout,
        stderr: String::new(),
        effect: Effect::Read,
    }
}

fn failure_report(json: bool, f: &Failure) -> Report {
    let mut stderr = format!("wgal: {}\n", f.text());
    if let Some(topic) = f.topic {
        let _ = writeln!(stderr, "{}", tr!("failure.see_help_topic", topic = topic));
    } else if matches!(f.kind, Kind::Usage) {
        let _ = writeln!(stderr, "{}", tr!("failure.see_help"));
    }
    let stdout = if json {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&f.json()).unwrap_or_default()
        )
    } else {
        String::new()
    };
    Report {
        code: f.exit(),
        stdout,
        stderr,
        effect: Effect::Failed,
    }
}

// --- Talking to the watchdog ------------------------------------------------

fn ask<W: Watchdog>(w: &mut W, request: &Request) -> Step<Response> {
    answered(w.send(request))
}

fn answered(answer: Result<wgal_protocol::ResponseEnvelope, Unreachable>) -> Step<Response> {
    let answer = answer.map_err(Failure::unreachable)?;
    match answer.outcome {
        Outcome::Ok(response) => Ok(response),
        Outcome::Err(error) => Err(Failure::refused(&error)),
    }
}

/// A change, which the watchdog answers `accepted`.
fn change<W: Watchdog>(w: &mut W, request: &Request) -> Step<()> {
    match ask(w, request)? {
        Response::Accepted => Ok(()),
        _ => Err(Failure::unexpected()),
    }
}

fn snapshot<W: Watchdog>(w: &mut W) -> Step<Snapshot> {
    match ask(w, &Request::GetSnapshot)? {
        Response::Snapshot { snapshot } => Ok(snapshot),
        _ => Err(Failure::unexpected()),
    }
}

/// The state after an accepted change, or that it could not be read.
fn after<W: Watchdog>(w: &mut W, done: &[String]) -> Step<Snapshot> {
    snapshot(w).map_err(|f| f.accepted_unread(done))
}

/// The end of a wait, by the console's clock (Codex review of the CLI,
/// P2-2): the looks count, not only the pauses between them, so a wait of
/// S seconds ends within S seconds and one last look of at most a second.
struct Deadline {
    end: Duration,
}

impl Deadline {
    fn after<C: Console>(c: &C, secs: u64) -> Self {
        Self {
            end: c.elapsed() + Duration::from_secs(secs),
        }
    }

    fn left<C: Console>(&self, c: &C) -> Duration {
        self.end.saturating_sub(c.elapsed())
    }

    fn passed<C: Console>(&self, c: &C) -> bool {
        self.left(c).is_zero()
    }
}

/// A pause, then a look at the state after an accepted change, both within
/// what the wait still has (the look at least a second).
fn next_look<W: Watchdog, C: Console>(
    w: &mut W,
    c: &mut C,
    deadline: &Deadline,
    done: &[String],
) -> Step<Snapshot> {
    c.pause(POLL.min(deadline.left(c)));
    let most = deadline.left(c).max(POLL);
    match answered(w.send_within(&Request::GetSnapshot, most)) {
        Ok(Response::Snapshot { snapshot }) => Ok(snapshot),
        Ok(_) => Err(Failure::unexpected().accepted_unread(done)),
        Err(f) => Err(f.accepted_unread(done)),
    }
}

fn find<'a>(s: &'a Snapshot, id: &TunnelId) -> Step<&'a TunnelStatus> {
    s.tunnels
        .iter()
        .find(|t| &t.tunnel_id == id)
        .ok_or_else(|| Failure::failed(tr!("failure.not_registered", tunnel = id.as_str())))
}

/// The tunnel after an accepted change: gone from the snapshot is "not
/// confirmed".
fn find_after<'a>(s: &'a Snapshot, id: &TunnelId, done: &[String]) -> Step<&'a TunnelStatus> {
    find(s, id).map_err(|f| f.accepted_unread(done))
}

fn value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn check_version(s: &Snapshot) -> Step<()> {
    let oldest = ProductVersion::new(MIN_WATCHDOG).map_err(|e| Failure::failed(e.to_string()))?;
    if s.host.watchdog_version.is_at_least(&oldest) {
        Ok(())
    } else {
        Err(Failure::failed(tr!(
            "failure.watchdog_too_old",
            version = s.host.watchdog_version.as_str(),
            needed = MIN_WATCHDOG
        )))
    }
}

/// Whether nothing about the tunnel is still moving, as `wgal-mcp` judges
/// it: no stop, start or verification under way, and no yellow light.
fn settled(t: &TunnelStatus) -> bool {
    !matches!(
        t.recovery,
        RecoveryPhase::Queued
            | RecoveryPhase::Stopping
            | RecoveryPhase::Starting
            | RecoveryPhase::Verifying
    ) && t.light != Light::Yellow
}

// --- The commands -----------------------------------------------------------

/// What a command reads or asks on this computer before anything is changed
/// (Codex review of the CLI, P2-1): a mistake there changes nothing. With
/// `--lang` it sends nothing at all; without, only the first look that
/// finds the language was sent (owner decision of 2026-11-19).
enum Prepared {
    Nothing,
    Token(BotToken),
    Language(&'static languages::Language),
    Document(Box<SettingsDocument>),
    /// A person typed yes.
    Confirmed,
}

impl Prepared {
    /// Whether something was asked on this computer, which may have taken
    /// a while: the state is then looked at again.
    fn asked(&self) -> bool {
        matches!(self, Self::Token(_) | Self::Document(_) | Self::Confirmed)
    }

    fn mismatch() -> Failure {
        Failure::failed(tr!("failure.prepared_for_another"))
    }

    fn token(self) -> Step<BotToken> {
        match self {
            Self::Token(token) => Ok(token),
            _ => Err(Self::mismatch()),
        }
    }

    fn language(self) -> Step<&'static languages::Language> {
        match self {
            Self::Language(l) => Ok(l),
            _ => Err(Self::mismatch()),
        }
    }

    fn document(self) -> Step<SettingsDocument> {
        match self {
            Self::Document(d) => Ok(*d),
            _ => Err(Self::mismatch()),
        }
    }
}

fn prepare<C: Console>(command: &Command, c: &mut C) -> Step<Prepared> {
    Ok(match command {
        Command::Notify(Notify::TokenSet) => Prepared::Token(read_token(c)?),
        Command::Notify(Notify::Language(Some(tag))) => Prepared::Language(find_language(tag)?),
        Command::Mcp(true) => {
            confirm_on(c, false)?;
            Prepared::Confirmed
        }
        Command::McpTelegram(true) => {
            confirm_on(c, true)?;
            Prepared::Confirmed
        }
        Command::Import(_) => Prepared::Document(Box::new(read_document(c)?)),
        _ => Prepared::Nothing,
    })
}

/// Runs `command`. `first` is the look already taken to find the language,
/// if one was; `chosen` says whether `--lang` named the language.
fn execute<W: Watchdog, C: Console>(
    command: &Command,
    w: &mut W,
    c: &mut C,
    first: Option<Step<Snapshot>>,
    chosen: bool,
) -> Step<Done> {
    match command {
        Command::Help(topic) => return Ok(help(*topic)),
        Command::Version => {
            return Ok(Done::new(
                format!("wgal {VERSION}\n"),
                json!({ "version": VERSION }),
            ));
        }
        _ => {}
    }
    let prepared = prepare(command, c)?;
    // The first look: the state the command starts from, and the
    // watchdog's version. The one that found the language serves, unless
    // something was asked here since.
    let s = match first {
        Some(first) if !prepared.asked() => first?,
        _ => snapshot(w)?,
    };
    check_version(&s)?;
    let clock = Clock::of(&s);
    match command {
        Command::Help(_) | Command::Version => unreachable!("answered above"),
        Command::Status => Ok(Done::new(show::status(&s), value(&s))),
        Command::List => {
            let mut text = String::new();
            for t in s.tunnels.iter() {
                let _ = writeln!(text, "{}", show::tunnel_line(t));
            }
            if s.tunnels.is_empty() {
                let _ = writeln!(text, "{}", tr!("list.none"));
            }
            Ok(Done::new(text, value(&s.tunnels)))
        }
        Command::Show(id) => {
            let t = find(&s, id)?;
            Ok(Done::new(show::tunnel_detail(t, &clock), value(t)))
        }
        Command::Discover => match ask(w, &Request::DiscoverTunnels)? {
            Response::Discovered { tunnels } => Ok(Done::new(
                show::discovered(&tunnels, &s),
                json!({ "tunnels": value(&tunnels) }),
            )),
            _ => Err(Failure::unexpected()),
        },
        Command::Register(id) => {
            change(
                w,
                &Request::RegisterTunnel {
                    tunnel_id: id.clone(),
                },
            )?;
            let done = [tr!("did.registered")];
            let a = after(w, &done)?;
            let t = find_after(&a, id, &done)?;
            Ok(tunnel_done(
                format!("{}\n", tr!("done.registered", tunnel = id.as_str())),
                t,
            ))
        }
        Command::Unregister(id) => {
            find(&s, id)?;
            change(
                w,
                &Request::UnregisterTunnel {
                    tunnel_id: id.clone(),
                },
            )?;
            let done = [tr!("did.forget_asked")];
            let a = after(w, &done)?;
            if a.tunnels.iter().any(|t| &t.tunnel_id == id) {
                return Err(
                    Failure::new(Kind::Unconfirmed, tr!("failure.still_listed")).noting(&done)
                );
            }
            Ok(Done::new(
                format!("{}\n", tr!("done.unregistered", tunnel = id.as_str())),
                json!({ "accepted": true, "tunnel_id": id.as_str() }),
            ))
        }
        Command::Monitor(id, on) => switch(
            w,
            &s,
            id,
            Request::SetTunnelMonitoring {
                tunnel_id: id.clone(),
                enabled: *on,
            },
            Switch::Monitoring,
            *on,
        ),
        Command::AutoRecovery(id, on) => switch(
            w,
            &s,
            id,
            Request::SetAutoRecovery {
                tunnel_id: id.clone(),
                enabled: *on,
            },
            Switch::AutoRecovery,
            *on,
        ),
        Command::StartWhenStopped(id, on) => switch(
            w,
            &s,
            id,
            Request::SetStartWhenStoppedExternally {
                tunnel_id: id.clone(),
                enabled: *on,
            },
            Switch::StartWhenStopped,
            *on,
        ),
        Command::Set(a) => set(w, &s, a, &clock),
        Command::Retry {
            tunnel,
            limit,
            interval,
        } => {
            let t = find(&s, tunnel)?;
            let retry = RecoveryRetry::new(
                limit.unwrap_or(t.retry.limit()),
                interval.unwrap_or(t.retry.interval_secs()),
            )
            .map_err(|e| {
                Failure::usage(usage_in("retry", tr!("usage.not_accepted", detail = e)))
            })?;
            change(
                w,
                &Request::SetRecoveryRetry {
                    tunnel_id: tunnel.clone(),
                    retry,
                },
            )?;
            let done = [tr!("did.retry_changed")];
            let a = after(w, &done)?;
            let t = find_after(&a, tunnel, &done)?;
            Ok(tunnel_done(
                format!(
                    "{}\n",
                    tr!(
                        "done.retry",
                        tunnel = tunnel.as_str(),
                        retry = show::retry(&t.retry)
                    )
                ),
                t,
            ))
        }
        Command::Operate {
            operation,
            tunnel,
            wait_secs,
        } => operate(w, c, &s, *operation, tunnel, *wait_secs),
        Command::Probe { tunnel, wait_secs } => probe(w, c, &s, tunnel, *wait_secs, &clock),
        Command::Pause(paused) => {
            change(w, &Request::SetMonitoringPaused { paused: *paused })?;
            let done = [if *paused {
                tr!("did.paused")
            } else {
                tr!("did.resumed")
            }];
            let a = after(w, &done)?;
            if a.monitoring_paused != *paused {
                return Err(
                    Failure::new(Kind::Unconfirmed, tr!("failure.not_shown_yet")).noting(&done),
                );
            }
            Ok(Done::new(
                format!(
                    "{}\n",
                    if *paused {
                        tr!("done.paused")
                    } else {
                        tr!("done.resumed")
                    }
                ),
                json!({ "accepted": true, "monitoring_paused": a.monitoring_paused }),
            ))
        }
        Command::Notify(n) => notify(w, c, &s, n, &clock, prepared),
        Command::Host { workers, time_zone } => {
            let settings = HostSettings {
                workers: workers.unwrap_or(s.host.workers.configured),
                time_zone: time_zone
                    .clone()
                    .unwrap_or_else(|| s.host.time_zone.configured.clone()),
            };
            change(w, &Request::UpdateHostSettings { settings })?;
            let done = [tr!("did.host_changed")];
            let a = after(w, &done)?;
            let threads = a.host.workers.effective;
            let utc_offset = show::offset(a.host.time_zone.utc_offset_secs);
            let configured = a.host.workers.configured.map(|n| n.get());
            let text = match (configured, a.host.time_zone.effective.as_ref()) {
                (Some(count), Some(zone)) => tr!(
                    "done.host_set",
                    threads = threads,
                    count = count,
                    zone = zone.as_str(),
                    offset = utc_offset
                ),
                (Some(count), None) => tr!(
                    "done.host_set_system_zone",
                    threads = threads,
                    count = count,
                    offset = utc_offset
                ),
                (None, Some(zone)) => tr!(
                    "done.host_suggested",
                    threads = threads,
                    zone = zone.as_str(),
                    offset = utc_offset
                ),
                (None, None) => tr!(
                    "done.host_suggested_system_zone",
                    threads = threads,
                    offset = utc_offset
                ),
            };
            Ok(Done::new(
                format!("{text}\n"),
                json!({ "accepted": true, "host": value(&a.host) }),
            ))
        }
        Command::Mcp(on) => mcp(w, *on, false),
        Command::McpTelegram(on) => mcp(w, *on, true),
        Command::Log { filter, before } => match ask(
            w,
            &Request::ReadEventLog {
                filter: filter.clone(),
                before_seq: *before,
            },
        )? {
            Response::EventLog { entries, more } => Ok(Done::new(
                show::log(&entries, more, &filter_options(filter), &clock),
                json!({ "entries": value(&entries), "more": more }),
            )),
            _ => Err(Failure::unexpected()),
        },
        Command::Export => export(w),
        Command::Import(options) => import(w, prepared.document()?, *options),
        Command::Language(LanguageArg::Show) => Ok(shared_language_shown(&s)),
        Command::Language(LanguageArg::Set(tag)) => set_shared_language(w, &s, tag, chosen),
        Command::Language(LanguageArg::Clear) => clear_shared_language(w, &s, chosen),
    }
}

/// The `wgal log` options that choose `filter`.
fn filter_options(filter: &EventFilter) -> String {
    match filter {
        EventFilter::All => String::new(),
        EventFilter::Tunnel { tunnel_id } => format!(" --tunnel {}", tunnel_id.as_str()),
        EventFilter::System => " --system".to_owned(),
        EventFilter::Watchdog => " --watchdog".to_owned(),
    }
}

fn usage_in(topic: &'static str, e: impl std::fmt::Display) -> Usage {
    Usage {
        message: e.to_string(),
        topic: Some(topic),
    }
}

/// A change to one tunnel, said with its state afterwards.
fn tunnel_done(text: String, t: &TunnelStatus) -> Done {
    Done::new(
        format!("{text}{}\n", show::tunnel_line(t)),
        json!({ "accepted": true, "tunnel": value(t) }),
    )
}

/// One of a tunnel's switches, each said in sentences of its own.
#[derive(Clone, Copy, Debug)]
enum Switch {
    Monitoring,
    AutoRecovery,
    StartWhenStopped,
    Notifications,
}

impl Switch {
    /// That it was turned `state` (`on` or `off`), for what was done.
    fn turned(self, state: &str) -> String {
        match self {
            Self::Monitoring => tr!("did.turned.monitoring", state = state),
            Self::AutoRecovery => tr!("did.turned.auto_recovery", state = state),
            Self::StartWhenStopped => tr!("did.turned.start_when_stopped", state = state),
            Self::Notifications => tr!("did.turned.notifications", state = state),
        }
    }

    /// That it is `state` now on `tunnel`.
    fn is(self, tunnel: &str, state: &str) -> String {
        match self {
            Self::Monitoring => tr!("done.switch.monitoring", tunnel = tunnel, state = state),
            Self::AutoRecovery => {
                tr!("done.switch.auto_recovery", tunnel = tunnel, state = state)
            }
            Self::StartWhenStopped => {
                tr!(
                    "done.switch.start_when_stopped",
                    tunnel = tunnel,
                    state = state
                )
            }
            Self::Notifications => {
                tr!("done.switch.notifications", tunnel = tunnel, state = state)
            }
        }
    }
}

fn switch<W: Watchdog>(
    w: &mut W,
    s: &Snapshot,
    id: &TunnelId,
    request: Request,
    setting: Switch,
    on: bool,
) -> Step<Done> {
    find(s, id)?;
    change(w, &request)?;
    let state = show::on(on);
    let done = [setting.turned(state)];
    let a = after(w, &done)?;
    let t = find_after(&a, id, &done)?;
    let mut text = format!("{}\n", setting.is(id.as_str(), state));
    if matches!(request, Request::SetAutoRecovery { enabled: true, .. }) && t.auto_recovery_pending
    {
        let _ = writeln!(text, "{}", tr!("done.auto_recovery_pending"));
    }
    Ok(tunnel_done(text, t))
}

fn set<W: Watchdog>(w: &mut W, s: &Snapshot, a: &SetArgs, clock: &Clock) -> Step<Done> {
    let Some(id) = &a.tunnel else {
        return Err(Failure::usage(usage_in("set", tr!("usage.give_tunnel_id"))));
    };
    let t = find(s, id)?;
    let probe = wgal_protocol::ProbeTiming::new(
        a.interval.unwrap_or(t.probe.interval_secs()),
        a.timeout.unwrap_or(t.probe.timeout_secs()),
        a.retries.unwrap_or(t.probe.retries()),
        a.min_fault.unwrap_or(t.probe.min_fault_secs()),
    )
    .map_err(|e| Failure::usage(usage_in("set", tr!("usage.not_accepted", detail = e))))?;
    let recovery_target = match &a.recovery {
        None => t.recovery_target.as_ref().map(|r| r.target),
        Some(target) => *target,
    };
    let observe = match &a.observe {
        None => t.observe_targets.iter().map(|o| o.target).collect(),
        Some(list) => list.clone(),
    };
    let observe_targets = BoundedVec::try_from(observe)
        .map_err(|_| Failure::usage(usage_in("set", tr!("usage.too_many_observe_targets"))))?;
    change(
        w,
        &Request::UpdateMonitorSettings {
            tunnel_id: id.clone(),
            settings: MonitorSettings {
                recovery_target,
                observe_targets,
                probe,
            },
        },
    )?;
    let done = [tr!("did.settings_changed")];
    let after = after(w, &done)?;
    let t = find_after(&after, id, &done)?;
    Ok(Done::new(
        format!(
            "{}\n{}",
            tr!("done.settings_changed", tunnel = id.as_str()),
            show::tunnel_detail(t, clock)
        ),
        json!({ "accepted": true, "tunnel": value(t) }),
    ))
}

fn operate<W: Watchdog, C: Console>(
    w: &mut W,
    c: &mut C,
    s: &Snapshot,
    operation: Operation,
    id: &TunnelId,
    wait_secs: u64,
) -> Step<Done> {
    find(s, id)?;
    let request = match operation {
        Operation::Connect => Request::Connect {
            tunnel_id: id.clone(),
        },
        Operation::Disconnect => Request::Disconnect {
            tunnel_id: id.clone(),
        },
        Operation::Reconnect => Request::Reconnect {
            tunnel_id: id.clone(),
        },
    };
    change(w, &request)?;
    let done = [operation.accepted()];
    if wait_secs == 0 {
        return Ok(Done::new(
            format!("{}\n", operation.not_waiting(id.as_str())),
            json!({ "accepted": true, "settled": false }),
        ));
    }
    let deadline = Deadline::after(c, wait_secs);
    loop {
        // The pause first gives the watchdog a moment to take the
        // operation up.
        let a = next_look(w, c, &deadline, &done)?;
        let t = find_after(&a, id, &done)?;
        if settled(t) {
            return judge(operation, &a, t);
        }
        if deadline.passed(c) {
            return Err(Failure::new(
                Kind::Unconfirmed,
                operation.in_progress(id.as_str(), wait_secs),
            )
            .noting(&done)
            .with(json!({ "tunnel": value(t) })));
        }
    }
}

/// The sentences an operation is said with, one set for each operation, so
/// that every language words each its own way.
impl Operation {
    /// That it was accepted, for what was done.
    fn accepted(self) -> String {
        match self {
            Self::Connect => tr!("did.connect_accepted"),
            Self::Disconnect => tr!("did.disconnect_accepted"),
            Self::Reconnect => tr!("did.reconnect_accepted"),
        }
    }

    /// That it was accepted and is not waited for.
    fn not_waiting(self, tunnel: &str) -> String {
        match self {
            Self::Connect => tr!("done.connect_not_waiting", tunnel = tunnel),
            Self::Disconnect => tr!("done.disconnect_not_waiting", tunnel = tunnel),
            Self::Reconnect => tr!("done.reconnect_not_waiting", tunnel = tunnel),
        }
    }

    /// That it has not settled after the wait.
    fn in_progress(self, tunnel: &str, seconds: u64) -> String {
        match self {
            Self::Connect => tr!(
                "failure.connect_in_progress",
                seconds = seconds,
                tunnel = tunnel
            ),
            Self::Disconnect => tr!(
                "failure.disconnect_in_progress",
                seconds = seconds,
                tunnel = tunnel
            ),
            Self::Reconnect => tr!(
                "failure.reconnect_in_progress",
                seconds = seconds,
                tunnel = tunnel
            ),
        }
    }

    /// That it settled with the tunnel elsewhere than it should be.
    fn left_elsewhere(self, os: &str, light: &str, reason: &str) -> String {
        match self {
            Self::Connect => tr!(
                "failure.connect_left_elsewhere",
                os = os,
                light = light,
                reason = reason
            ),
            Self::Disconnect => tr!(
                "failure.disconnect_left_elsewhere",
                os = os,
                light = light,
                reason = reason
            ),
            Self::Reconnect => tr!(
                "failure.reconnect_left_elsewhere",
                os = os,
                light = light,
                reason = reason
            ),
        }
    }

    /// That it settled where it should.
    fn settled(self) -> String {
        match self {
            Self::Connect => tr!("done.connect_settled"),
            Self::Disconnect => tr!("done.disconnect_settled"),
            Self::Reconnect => tr!("done.reconnect_settled"),
        }
    }
}

/// Whether a settled tunnel is where the operation should have left it.
fn judge(operation: Operation, s: &Snapshot, t: &TunnelStatus) -> Step<Done> {
    let line = show::tunnel_line(t);
    let data = json!({ "accepted": true, "settled": true, "tunnel": value(t) });
    let wanted = match operation {
        Operation::Connect | Operation::Reconnect => OsState::Connected,
        Operation::Disconnect => OsState::Disconnected,
    };
    if t.os_state != wanted {
        return Err(Failure::failed(operation.left_elsewhere(
            &show::word(&t.os_state),
            &show::word(&t.light),
            &show::word(&t.light_reason),
        ))
        .with(data));
    }
    let checked = t.intent.monitoring_enabled && !s.monitoring_paused;
    if wanted == OsState::Connected && checked && t.light == Light::Red {
        return Err(Failure::failed(tr!(
            "failure.no_traffic",
            reason = show::word(&t.light_reason)
        ))
        .with(data));
    }
    let settled = operation.settled();
    let text = match (wanted, checked, t.light) {
        (OsState::Connected, true, Light::Green) => tr!(
            "join.sentences",
            first = settled,
            second = tr!("done.traffic_verified")
        ),
        (OsState::Connected, false, _) => tr!(
            "join.sentences",
            first = settled,
            second = tr!("done.traffic_not_checked")
        ),
        _ => settled,
    };
    Ok(Done::new(format!("{text}\n{line}\n"), data))
}

fn probe<W: Watchdog, C: Console>(
    w: &mut W,
    c: &mut C,
    s: &Snapshot,
    id: &TunnelId,
    wait_secs: u64,
    clock: &Clock,
) -> Step<Done> {
    let before = find(s, id)?;
    if before.recovery_target.is_none() && before.observe_targets.is_empty() {
        return Err(Failure::failed(tr!(
            "failure.no_target",
            tunnel = id.as_str()
        )));
    }
    let one_off = s.monitoring_paused || !before.intent.monitoring_enabled;
    let asked = probed_targets(before);
    change(
        w,
        &Request::ProbeNow {
            tunnel_id: id.clone(),
        },
    )?;
    let done = [tr!("did.probe_asked")];
    if wait_secs == 0 {
        return Ok(Done::new(
            format!("{}\n", tr!("done.probe_not_waiting")),
            json!({ "accepted": true, "probed": false }),
        ));
    }
    let deadline = Deadline::after(c, wait_secs);
    let t = loop {
        let a = next_look(w, c, &deadline, &done)?;
        let t = find_after(&a, id, &done)?.clone();
        let now = probed_targets(&t);
        let mut all_new = true;
        for (role, target, was) in &asked {
            match now.iter().find(|(r, g, _)| r == role && g == target) {
                // Another user changed the targets meanwhile: what was
                // asked for cannot be told any more.
                None => {
                    return Err(Failure::new(
                        Kind::Unconfirmed,
                        tr!("failure.targets_changed", tunnel = id.as_str()),
                    )
                    .noting(&done));
                }
                Some((_, _, at)) => all_new &= newer(*at, *was),
            }
        }
        if all_new {
            break t;
        }
        if deadline.passed(c) {
            let message = if one_off {
                tr!("failure.no_new_result_monitoring_off", seconds = wait_secs)
            } else {
                tr!("failure.no_new_result", seconds = wait_secs)
            };
            return Err(Failure::new(Kind::Unconfirmed, message).noting(&done));
        }
    };
    let mut text = String::new();
    let mut all_reached = true;
    for (role, target) in t
        .recovery_target
        .iter()
        .map(|r| (Role::Recovery, r))
        .chain(t.observe_targets.iter().map(|o| (Role::Observe, o)))
        .filter(|(role, r)| asked.iter().any(|(a, g, _)| a == role && *g == r.target))
    {
        let reached = target
            .last_probe
            .as_ref()
            .is_some_and(|p| show::reached(&p.result));
        all_reached &= reached;
        let probed = show::target(&target.target);
        let result = target.last_probe.as_ref().map_or_else(
            || tr!("probe.not_probed"),
            |p| {
                format!(
                    "{} ({})",
                    show::result(&p.result),
                    clock.at(p.finished_at_unix_ms)
                )
            },
        );
        let _ = writeln!(
            text,
            "{}",
            match role {
                Role::Recovery => tr!("probe.recovery_target", target = probed, result = result),
                Role::Observe => tr!("probe.observe_target", target = probed, result = result),
            }
        );
    }
    if one_off {
        let _ = writeln!(text, "{}", tr!("done.one_off_probe"));
    }
    let data = json!({ "accepted": true, "probed": true, "one_off": one_off, "tunnel": value(&t) });
    if all_reached {
        Ok(Done::new(text, data))
    } else {
        Err(Failure::failed(tr!("failure.not_every_target", targets = text.trim_end())).with(data))
    }
}

/// Which of a tunnel's targets one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Recovery,
    Observe,
}

/// Each target a probe round covers: its role, what it probes and when
/// its last probe finished.
fn probed_targets(t: &TunnelStatus) -> Vec<(Role, ProbeTarget, Option<i64>)> {
    let at = |r: &wgal_protocol::TargetStatus| r.last_probe.as_ref().map(|p| p.finished_at_unix_ms);
    t.recovery_target
        .iter()
        .map(|r| (Role::Recovery, r.target, at(r)))
        .chain(
            t.observe_targets
                .iter()
                .map(|o| (Role::Observe, o.target, at(o))),
        )
        .collect()
}

/// Whether a probe finished at `now` is newer than the one at `was`: a
/// result that is gone, or no newer, is not.
fn newer(now: Option<i64>, was: Option<i64>) -> bool {
    match (now, was) {
        (Some(now), Some(was)) => now > was,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn notify<W: Watchdog, C: Console>(
    w: &mut W,
    c: &mut C,
    s: &Snapshot,
    n: &Notify,
    clock: &Clock,
    prepared: Prepared,
) -> Step<Done> {
    let current = &s.notification;
    let update =
        |enabled: bool, device_name: DeviceName, chat_id| Request::UpdateNotificationSettings {
            enabled,
            device_name,
            chat_id,
        };
    match n {
        Notify::Show => Ok(Done::new(
            show::notification(s, clock),
            json!({
                "notification": value(current),
                "language": in_use_json(s),
            }),
        )),
        Notify::Enabled(on) => {
            change(
                w,
                &update(*on, current.device_name.clone(), current.chat_id.clone()),
            )?;
            let done = [if *on {
                tr!("did.notifications_on")
            } else {
                tr!("did.notifications_off")
            }];
            let a = after(w, &done)?;
            Ok(notification_done(
                format!(
                    "{}\n",
                    tr!(
                        "done.notifications",
                        state = show::on(a.notification.enabled)
                    )
                ),
                &a,
            ))
        }
        Notify::Device(name) => {
            let fitted = DeviceName::fitted(name);
            let adjusted = fitted.as_str() != name;
            change(w, &update(current.enabled, fitted, current.chat_id.clone()))?;
            let done = [tr!("did.device_name_changed")];
            let a = after(w, &done)?;
            let name = a.notification.device_name.as_str();
            Ok(notification_done(
                format!(
                    "{}\n",
                    if adjusted {
                        tr!("done.device_name_adjusted", name = name)
                    } else {
                        tr!("done.device_name", name = name)
                    }
                ),
                &a,
            ))
        }
        Notify::Chat(chat) => {
            change(
                w,
                &update(current.enabled, current.device_name.clone(), chat.clone()),
            )?;
            let done = [if chat.is_some() {
                tr!("did.chat_changed")
            } else {
                tr!("did.chat_cleared")
            }];
            let a = after(w, &done)?;
            Ok(notification_done(
                format!(
                    "{}\n",
                    match a.notification.chat_id.as_ref() {
                        Some(chat) => tr!("done.chat_id", chat = chat.as_str()),
                        None => tr!("done.chat_id_not_set"),
                    }
                ),
                &a,
            ))
        }
        Notify::Tunnel(id, on) => switch(
            w,
            s,
            id,
            Request::SetTunnelNotifications {
                tunnel_id: id.clone(),
                enabled: *on,
            },
            Switch::Notifications,
            *on,
        ),
        Notify::TokenSet => {
            let token = prepared.token()?;
            change(w, &Request::SetBotToken { token })?;
            let done = [tr!("did.token_handed_over")];
            let a = after(w, &done)?;
            if a.notification.token != wgal_protocol::CredentialState::Stored {
                return Err(Failure::new(
                    Kind::Unconfirmed,
                    tr!(
                        "failure.token_not_stored",
                        state = show::word(&a.notification.token)
                    ),
                )
                .noting(&done));
            }
            Ok(notification_done(
                format!("{}\n", tr!("done.token_set")),
                &a,
            ))
        }
        Notify::TokenClear => {
            change(w, &Request::ClearBotToken)?;
            let done = [tr!("did.token_cleared")];
            let a = after(w, &done)?;
            Ok(notification_done(
                format!("{}\n", tr!("done.token_cleared")),
                &a,
            ))
        }
        Notify::Test { wait_secs } => test(w, c, s, *wait_secs),
        Notify::Language(None) => Ok(languages_list(s)),
        Notify::Language(Some(_)) => language(w, prepared.language()?),
    }
}

fn notification_done(text: String, a: &Snapshot) -> Done {
    Done::new(
        text,
        json!({ "accepted": true, "notification": value(&a.notification) }),
    )
}

fn in_use_json(s: &Snapshot) -> Value {
    let in_use = languages::in_use(
        s.notification.templates_id.as_ref(),
        &s.host.watchdog_version,
    );
    json!({
        "tag": in_use.tag(),
        "description": in_use.describe(),
        "templates_id": value(&s.notification.templates_id),
    })
}

/// Reads the Bot Token: unseen at a terminal, or one line of standard
/// input; never echoed, never repeated in a message.
fn read_token<C: Console>(c: &mut C) -> Step<BotToken> {
    let line = if c.stdin_is_terminal() {
        c.read_hidden_line(&tr!("prompt.token"), MAX_TOKEN_BYTES)
    } else {
        c.read_line(MAX_TOKEN_BYTES)
    };
    let bytes = match line {
        Ok(Line::Read(bytes)) => bytes,
        Ok(Line::End) => {
            return Err(Failure::input(tr!("input.no_token")));
        }
        Ok(Line::TooLong) => {
            return Err(Failure::input(tr!(
                "input.token_line_too_long",
                bytes = MAX_TOKEN_BYTES
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
            return Err(Failure::new(
                Kind::Interrupted(130),
                tr!("input.interrupted"),
            ));
        }
        Err(e) => {
            return Err(Failure::input(tr!(
                "input.token_unreadable",
                error = e.kind()
            )));
        }
    };
    let text = String::from_utf8(bytes).map_err(|_| Failure::input(tr!("input.not_a_token")))?;
    BotToken::new(text.trim()).map_err(|_| Failure::input(tr!("input.not_a_token_shape")))
}

fn test<W: Watchdog, C: Console>(w: &mut W, c: &mut C, s: &Snapshot, wait_secs: u64) -> Step<Done> {
    let before = s
        .notification
        .last_test
        .as_ref()
        .map(|t| t.requested_at_unix_ms);
    change(w, &Request::SendTestNotification)?;
    let done = [tr!("did.test_asked")];
    if wait_secs == 0 {
        return Ok(Done::new(
            format!("{}\n", tr!("done.test_not_waiting")),
            json!({ "accepted": true, "result": "sending" }),
        ));
    }
    let deadline = Deadline::after(c, wait_secs);
    loop {
        let a = next_look(w, c, &deadline, &done)?;
        let current = a
            .notification
            .last_test
            .filter(|t| Some(t.requested_at_unix_ms) != before);
        if let Some(t) = current.filter(|t| t.state != TestNotificationState::Sending) {
            let data = json!({ "accepted": true, "result": value(&t.state), "failure": value(&t.failure) });
            return match t.state {
                TestNotificationState::Accepted => {
                    Ok(Done::new(format!("{}\n", tr!("done.test_accepted")), data))
                }
                TestNotificationState::Failed => Err(Failure::failed(match t.failure {
                    Some(f) => tr!("failure.test_not_sent", reason = show::word(&f)),
                    None => tr!("failure.test_not_sent_no_reason"),
                })
                .with(data)),
                _ => Err(
                    Failure::new(Kind::Unconfirmed, tr!("failure.test_outcome_unknown"))
                        .noting(&done)
                        .with(data),
                ),
            };
        }
        if deadline.passed(c) {
            return Err(Failure::new(
                Kind::Unconfirmed,
                tr!("failure.test_no_answer", seconds = wait_secs),
            )
            .noting(&done));
        }
    }
}

fn languages_list(s: &Snapshot) -> Done {
    let in_use = languages::in_use(
        s.notification.templates_id.as_ref(),
        &s.host.watchdog_version,
    );
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{}",
        tr!("notify.language_in_use", language = in_use.describe())
    );
    let _ = writeln!(text, "{}", tr!("notify.languages"));
    for l in languages::all() {
        let _ = writeln!(text, "  {:<6} {}", l.tag, l.name);
    }
    Done::new(
        text,
        json!({
            "in_use": in_use_json(s),
            "languages": languages::all().iter().map(|l| json!({
                "tag": l.tag,
                "name": l.name,
                "templates_id": l.templates.id().as_str(),
            })).collect::<Vec<_>>(),
        }),
    )
}

/// The language `tag` names, or the mistake.
fn find_language(tag: &str) -> Step<&'static languages::Language> {
    languages::find(tag).ok_or_else(|| {
        let tags: Vec<&str> = languages::all().iter().map(|l| l.tag.as_str()).collect();
        Failure::usage(usage_in(
            "notify",
            tr!(
                "notify.language_unknown",
                tag = format!("{tag:?}"),
                tags = tags.join(", ")
            ),
        ))
    })
}

fn language<W: Watchdog>(w: &mut W, l: &'static languages::Language) -> Step<Done> {
    change(
        w,
        &Request::SetNotificationTemplates {
            templates: l.templates.clone(),
        },
    )?;
    let done = [tr!("did.templates_handed_over")];
    let a = after(w, &done)?;
    if a.notification.templates_id.as_ref() != Some(l.templates.id()) {
        return Err(Failure::new(
            Kind::Unconfirmed,
            tr!(
                "failure.other_set_in_use",
                language = l.name,
                in_use = languages::in_use(
                    a.notification.templates_id.as_ref(),
                    &a.host.watchdog_version
                )
                .describe()
            ),
        )
        .noting(&done));
    }
    Ok(Done::new(
        format!("{}\n", tr!("done.language", language = l.name, tag = l.tag)),
        json!({ "accepted": true, "language": in_use_json(&a) }),
    ))
}

// --- The shared language (owner decisions of 2026-11-19) -------------------

/// The oldest watchdog that keeps `wgal`'s shared language.
pub const MIN_LANGUAGE_WATCHDOG: &str = "4.0.0";

fn require_language_watchdog(s: &Snapshot) -> Step<()> {
    let oldest =
        ProductVersion::new(MIN_LANGUAGE_WATCHDOG).map_err(|e| Failure::failed(e.to_string()))?;
    if s.host.watchdog_version.is_at_least(&oldest) {
        Ok(())
    } else {
        Err(Failure::failed(tr!(
            "failure.language_needs_watchdog",
            version = s.host.watchdog_version.as_str(),
            needed = MIN_LANGUAGE_WATCHDOG
        )))
    }
}

/// The language the shared setting in `s` makes `wgal` write in, as the
/// language files spell it.
fn shared_tag(s: &Snapshot) -> &'static str {
    s.notification
        .cli_language
        .as_ref()
        .and_then(|tag| text::known(tag.as_str()))
        .unwrap_or("en")
}

/// The shared language in `s`, in words.
fn shared_language_described(s: &Snapshot) -> String {
    match s.notification.cli_language.as_ref() {
        None => tr!("language.shared_default"),
        Some(tag) => match languages::find(tag.as_str()) {
            Some(l) => tr!("language.known", language = l.name, tag = l.tag),
            None => tr!(
                "language.shared_unknown",
                tag = format!("{:?}", tag.as_str())
            ),
        },
    }
}

fn shared_language_shown(s: &Snapshot) -> Done {
    let in_use = languages::in_use(
        s.notification.templates_id.as_ref(),
        &s.host.watchdog_version,
    );
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{}",
        tr!("language.shared", language = shared_language_described(s))
    );
    let _ = writeln!(
        text,
        "{}",
        tr!("notify.language_in_use", language = in_use.describe())
    );
    let _ = writeln!(text, "{}", tr!("language.list_heading"));
    for l in languages::all() {
        let _ = writeln!(text, "  {:<6} {}", l.tag, l.name);
    }
    Done::new(
        text,
        json!({
            "cli_language": value(&s.notification.cli_language),
            "writes_in": shared_tag(s),
            "in_use": in_use_json(s),
            "languages": languages::all().iter().map(|l| json!({
                "tag": l.tag,
                "name": l.name,
                "templates_id": l.templates.id().as_str(),
            })).collect::<Vec<_>>(),
        }),
    )
}

/// `wgal language <tag>`: the shared language, then the messages' set,
/// each only when it is not that already (the plan's §4). Without
/// `--lang`, what follows the accepted language is written in it.
fn set_shared_language<W: Watchdog>(
    w: &mut W,
    s: &Snapshot,
    tag: &'static str,
    chosen: bool,
) -> Step<Done> {
    require_language_watchdog(s)?;
    let l =
        languages::find(tag).ok_or_else(|| Failure::failed(tr!("failure.prepared_for_another")))?;
    let wanted = LanguageTag::new(l.tag.clone()).map_err(|e| Failure::failed(e.to_string()))?;
    let need_shared = s.notification.cli_language.as_ref() != Some(&wanted);
    let need_set = s.notification.templates_id.as_ref() != Some(l.templates.id());
    if !need_shared && !need_set {
        return Ok(Done::new(
            format!(
                "{}\n",
                tr!("done.language_already", language = l.name, tag = l.tag)
            ),
            json!({
                "changed": [],
                "cli_language": l.tag,
                "templates_id": l.templates.id().as_str(),
            }),
        )
        .with_effect(Effect::Read));
    }
    let mut done: Vec<String> = Vec::new();
    let mut changed: Vec<&str> = Vec::new();
    if need_shared {
        change(
            w,
            &Request::SetCliLanguage {
                language: Some(wanted.clone()),
            },
        )?;
        if !chosen {
            text::set(&l.tag);
        }
        done.push(tr!("did.shared_language_set", language = l.name));
        changed.push("set_cli_language");
    }
    if need_set {
        if let Err(f) = change(
            w,
            &Request::SetNotificationTemplates {
                templates: l.templates.clone(),
            },
        ) {
            return Err(if done.is_empty() {
                f
            } else {
                f.after_accepted(&done)
            });
        }
        done.push(tr!("did.templates_handed_over"));
        changed.push("set_notification_templates");
    }
    let a = after(w, &done)?;
    if a.notification.cli_language.as_ref() != Some(&wanted)
        || a.notification.templates_id.as_ref() != Some(l.templates.id())
    {
        return Err(Failure::new(
            Kind::Unconfirmed,
            tr!("failure.language_not_shown", language = l.name),
        )
        .noting(&done));
    }
    Ok(Done::new(
        format!(
            "{}\n",
            tr!("done.shared_language", language = l.name, tag = l.tag)
        ),
        json!({
            "accepted": true,
            "changed": changed,
            "cli_language": value(&a.notification.cli_language),
            "templates_id": value(&a.notification.templates_id),
        }),
    ))
}

/// `wgal language --clear`: always sent, so that the watchdog writes its
/// settings again without the language (the plan's §2.1, §4).
fn clear_shared_language<W: Watchdog>(w: &mut W, s: &Snapshot, chosen: bool) -> Step<Done> {
    require_language_watchdog(s)?;
    change(w, &Request::SetCliLanguage { language: None })?;
    if !chosen {
        text::english();
    }
    let done = vec![tr!("did.shared_language_cleared")];
    let a = after(w, &done)?;
    if a.notification.cli_language.is_some() {
        return Err(
            Failure::new(Kind::Unconfirmed, tr!("failure.language_still_set")).noting(&done),
        );
    }
    Ok(Done::new(
        format!("{}\n", tr!("done.shared_language_cleared")),
        json!({
            "accepted": true,
            "changed": ["set_cli_language"],
            "cli_language": null,
            "templates_id": value(&a.notification.templates_id),
        }),
    ))
}

/// Turning MCP, or agents setting Telegram, on is asked of a person at an
/// interactive terminal, who types yes; only a check against doing it in
/// passing, not proof of who it is.
fn confirm_on<C: Console>(c: &mut C, telegram: bool) -> Step<()> {
    if !c.interactive() {
        return Err(Failure::usage(usage_in(
            "mcp",
            tr!("usage.mcp_needs_terminal"),
        )));
    }
    let prompt = if telegram {
        tr!("prompt.mcp_telegram_on")
    } else {
        tr!("prompt.mcp_on")
    };
    let answer = c
        .ask(&prompt, 16)
        .map_err(|e| Failure::input(tr!("input.answer_unreadable", error = e.kind())))?;
    if answer != Line::Read(b"yes".to_vec()) {
        return Err(Failure::input(tr!("input.not_confirmed")));
    }
    Ok(())
}

fn mcp<W: Watchdog>(w: &mut W, on: bool, telegram: bool) -> Step<Done> {
    let request = if telegram {
        Request::SetMcpTelegramEnabled { enabled: on }
    } else {
        Request::SetMcpEnabled { enabled: on }
    };
    change(w, &request)?;
    let state = show::on(on);
    let done = [if telegram {
        tr!("did.turned.mcp_telegram", state = state)
    } else {
        tr!("did.turned.mcp", state = state)
    }];
    let a = after(w, &done)?;
    let now = if telegram {
        a.host.mcp_telegram_enabled
    } else {
        a.host.mcp_enabled
    };
    if now != on {
        return Err(Failure::new(Kind::Unconfirmed, tr!("failure.not_shown")).noting(&done));
    }
    let text = match (telegram, on) {
        (false, true) => tr!("done.mcp_on"),
        (false, false) => tr!("done.mcp_off"),
        (true, true) => tr!("done.mcp_telegram_on"),
        (true, false) => tr!("done.mcp_telegram_off"),
    };
    Ok(Done::new(
        format!("{text}\n"),
        json!({
            "accepted": true,
            "mcp_enabled": a.host.mcp_enabled,
            "mcp_telegram_enabled": a.host.mcp_telegram_enabled,
        }),
    ))
}

/// Where in `value` text shaped like a Bot Token is, as a path such as
/// `tunnels[2].name`; never the text itself.
fn token_place(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::String(text) if looks_like_token(text) => Some(path.to_owned()),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(i, v)| token_place(v, &format!("{path}[{i}]"))),
        Value::Object(map) => map.iter().find_map(|(k, v)| {
            let key = if looks_like_token(k) { "(a name)" } else { k };
            let here = if path.is_empty() {
                key.to_owned()
            } else {
                format!("{path}.{key}")
            };
            if looks_like_token(k) {
                Some(here)
            } else {
                token_place(v, &here)
            }
        }),
        _ => None,
    }
}

fn export<W: Watchdog>(w: &mut W) -> Step<Done> {
    let document = match ask(w, &Request::ExportSettings)? {
        Response::SettingsExport { document } => document,
        _ => return Err(Failure::unexpected()),
    };
    let data = value(&document);
    if let Some(place) = token_place(&data, "") {
        return Err(Failure::new(
            Kind::ExportRefused {
                place: place.clone(),
            },
            tr!("failure.export_holds_token", place = place),
        ));
    }
    let text = serde_json::to_string_pretty(&document)
        .map_err(|e| Failure::failed(tr!("failure.export_unwritable", error = e)))?;
    Ok(Done::new(format!("{text}\n"), json!({ "document": data })))
}

/// The settings file on standard input, read and checked whole before
/// anything is sent.
fn read_document<C: Console>(c: &mut C) -> Step<SettingsDocument> {
    // A terminal would only wait for the end of input.
    if c.stdin_is_terminal() {
        return Err(Failure::usage(usage_in(
            "import",
            tr!("usage.import_needs_stdin"),
        )));
    }
    let bytes = match c.read_all(MAX_IMPORT_BYTES) {
        Ok(Input::Read(bytes)) => bytes,
        Ok(Input::TooLong) => {
            return Err(Failure::input(tr!(
                "input.import_too_large",
                kib = MAX_IMPORT_BYTES / 1024
            )));
        }
        Err(e) => {
            return Err(Failure::input(tr!(
                "input.stdin_unreadable",
                error = e.kind()
            )));
        }
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| Failure::input(tr!("input.not_a_settings_file", error = e)))
}

fn import<W: Watchdog>(
    w: &mut W,
    document: SettingsDocument,
    options: wgal_protocol::ImportOptions,
) -> Step<Done> {
    let result = match ask(w, &Request::ImportSettings { document, options })? {
        Response::SettingsImport { result } => result,
        _ => return Err(Failure::unexpected()),
    };
    let text = show::import(&result);
    let data = value(&result);
    if show::import_failed(&result) {
        let message = if result.dry_run {
            tr!("failure.import_would_fail", result = text.trim_end())
        } else {
            tr!("failure.import_failed", result = text.trim_end())
        };
        return Err(Failure::failed(message).with(data));
    }
    Ok(Done::new(text, data))
}

// --- Help -------------------------------------------------------------------

fn help(topic: Option<&'static str>) -> Done {
    let text = topic
        .and_then(topic_help)
        .unwrap_or_else(|| tr!("help.general"));
    Done::new(text.clone(), json!({ "help": text }))
}

/// The help for one command, by its name.
fn topic_help(topic: &str) -> Option<String> {
    Some(match topic {
        "status" => tr!("help.topic.status"),
        "list" => tr!("help.topic.list"),
        "show" => tr!("help.topic.show"),
        "discover" => tr!("help.topic.discover"),
        "register" => tr!("help.topic.register"),
        "unregister" => tr!("help.topic.unregister"),
        "monitor" => tr!("help.topic.monitor"),
        "set" => tr!("help.topic.set"),
        "auto-recovery" => tr!("help.topic.auto_recovery"),
        "start-when-stopped" => tr!("help.topic.start_when_stopped"),
        "retry" => tr!("help.topic.retry"),
        "connect" => tr!("help.topic.connect"),
        "disconnect" => tr!("help.topic.disconnect"),
        "reconnect" => tr!("help.topic.reconnect"),
        "probe" => tr!("help.topic.probe"),
        "pause" => tr!("help.topic.pause"),
        "resume" => tr!("help.topic.resume"),
        "notify" => tr!("help.topic.notify"),
        "language" => tr!("help.topic.language"),
        "host" => tr!("help.topic.host"),
        "mcp" => tr!("help.topic.mcp"),
        "log" => tr!("help.topic.log"),
        "export" => tr!("help.topic.export"),
        "import" => tr!("help.topic.import"),
        _ => return None,
    })
}
