//! `wgal` against a scripted watchdog and a scripted terminal: what each
//! command sends, that a mistake sends nothing, how each failure ends and
//! what it writes, that the Bot Token is read only as it should be and
//! nothing written holds one, and that the language table is the window's.
//! Nothing here reaches a real watchdog.

#![allow(clippy::unwrap_used)]

use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use serde_json::Value;
use wgal_client::{Delivery, Unreachable, Watchdog};
use wgal_protocol::{
    AutoRecoveryImport, BotToken, BoundedVec, CredentialState, DisplayName, ErrorCode, ErrorReason,
    ImportItem, ImportOutcome, ImportResult, LanguageTag, Light, MonitorSettings, OsState, Outcome,
    PROTOCOL_VERSION, ProbeKind, ProbeResult, ProbeSummary, ProductVersion, ProtocolError,
    RecoveryPhase, Request, RequestId, Response, ResponseEnvelope, Snapshot, TemplatesId,
    TestNotificationState, TestNotificationStatus, TunnelId, TunnelStatus, codec,
};

use crate::console::{Console, Input, Line, read_all_from, read_line_from};
use crate::languages;
use crate::run::{MAX_IMPORT_BYTES, MAX_TOKEN_BYTES, Report, run};

/// A made-up token, of the shape Telegram's have.
const TOKEN: &str = "123456789:AAEhBP0av28fake_token-for-tests-only0";

fn response(text: &str) -> Response {
    match codec::decode_response(text.as_bytes()).unwrap().outcome {
        Outcome::Ok(response) => response,
        Outcome::Err(e) => panic!("not an answer: {e:?}"),
    }
}

fn fixture_snapshot() -> Snapshot {
    match response(include_str!(
        "../../wgal-protocol/fixtures/response_snapshot.json"
    )) {
        Response::Snapshot { mut snapshot } => {
            snapshot.host.watchdog_version = ProductVersion::new("4.0.0").unwrap();
            // The fixture names a shared language; these tests read English
            // unless one sets it (owner decision of 2026-11-19).
            snapshot.notification.cli_language = None;
            snapshot
        }
        other => panic!("not a snapshot: {other:?}"),
    }
}

fn id(text: &str) -> TunnelId {
    TunnelId::new(text).unwrap()
}

fn edit(s: &mut Snapshot, tunnel: &TunnelId, change: impl FnOnce(&mut TunnelStatus)) {
    let mut tunnels: Vec<TunnelStatus> = s.tunnels.iter().cloned().collect();
    if let Some(t) = tunnels.iter_mut().find(|t| &t.tunnel_id == tunnel) {
        change(t);
    }
    s.tunnels = BoundedVec::try_from(tunnels).unwrap();
}

/// Only the first look, which finds the language, was sent: nothing was
/// changed (owner decision of 2026-11-19, the plan's §3.2).
fn only_looked(fake: &Fake) {
    assert_eq!(
        fake.sent,
        vec![Request::GetSnapshot],
        "sent {:?}",
        fake.sent
    );
}

/// What the fake does with one request instead of carrying it out.
#[derive(Clone)]
enum Scripted {
    Refuse(ProtocolError),
    Lose(Delivery),
    Answer(Box<Response>),
}

/// Which requests a script entry is for, and what is done with them.
type Script = Vec<(fn(&Request) -> bool, Scripted)>;

/// A scripted watchdog: it records every request, carries out changes on
/// its snapshot, and answers the rest from the contract's fixtures.
struct Fake {
    snapshot: Snapshot,
    sent: Vec<Request>,
    script: Script,
    /// From the n-th snapshot request (counted from 0) on, snapshots fail.
    snapshots_fail_from: Option<usize>,
    snapshots: usize,
    /// Whether changes are carried out on the snapshot.
    apply: bool,
    /// Rounds an operation stays in progress.
    busy_rounds: usize,
    /// Whether a probe's observe targets answer.
    observe_replies: bool,
    /// What a probe does to the targets' results.
    probe_effect: ProbeEffect,
    /// The clock the terminal reads, and how long each look after the
    /// first takes on it.
    clock: Rc<Cell<Duration>>,
    look_takes: Duration,
    /// What changes in the state just before the second look.
    second_look: Option<fn(&mut Snapshot)>,
}

/// What [`Fake`] does to a tunnel's targets when it is probed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeEffect {
    /// Every target gets a new result.
    All,
    /// Only the recovery target does.
    RecoveryOnly,
    /// Every result is cleared.
    Clear,
    /// The recovery target is replaced by another one, with a result.
    Replace,
}

impl Fake {
    fn new() -> Self {
        Self {
            snapshot: fixture_snapshot(),
            sent: Vec::new(),
            script: Vec::new(),
            snapshots_fail_from: None,
            snapshots: 0,
            apply: true,
            busy_rounds: 0,
            observe_replies: true,
            probe_effect: ProbeEffect::All,
            clock: Rc::new(Cell::new(Duration::ZERO)),
            look_takes: Duration::ZERO,
            second_look: None,
        }
    }

    fn on(mut self, matches: fn(&Request) -> bool, what: Scripted) -> Self {
        self.script.push((matches, what));
        self
    }

    /// The requests sent other than snapshots.
    fn changes(&self) -> Vec<&Request> {
        self.sent
            .iter()
            .filter(|r| !matches!(r, Request::GetSnapshot))
            .collect()
    }

    fn carry_out(&mut self, request: &Request) {
        if !self.apply {
            return;
        }
        let s = &mut self.snapshot;
        match request {
            Request::SetTunnelMonitoring { tunnel_id, enabled } => {
                edit(s, tunnel_id, |t| t.intent.monitoring_enabled = *enabled);
            }
            Request::SetAutoRecovery { tunnel_id, enabled } => {
                edit(s, tunnel_id, |t| t.intent.auto_recovery_enabled = *enabled);
            }
            Request::SetStartWhenStoppedExternally { tunnel_id, enabled } => {
                edit(s, tunnel_id, |t| {
                    t.intent.start_when_stopped_externally = *enabled;
                })
            }
            Request::SetTunnelNotifications { tunnel_id, enabled } => {
                edit(s, tunnel_id, |t| t.notifications_enabled = *enabled);
            }
            Request::SetRecoveryRetry { tunnel_id, retry } => {
                edit(s, tunnel_id, |t| t.retry = *retry);
            }
            Request::UpdateMonitorSettings {
                tunnel_id,
                settings,
            } => edit(s, tunnel_id, |t| {
                t.probe = settings.probe;
                t.recovery_target =
                    settings
                        .recovery_target
                        .map(|target| wgal_protocol::TargetStatus {
                            target,
                            last_probe: None,
                        });
                t.observe_targets = BoundedVec::try_from(
                    settings
                        .observe_targets
                        .iter()
                        .map(|&target| wgal_protocol::TargetStatus {
                            target,
                            last_probe: None,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            }),
            Request::RegisterTunnel { tunnel_id } => {
                let mut t = s.tunnels.iter().next().unwrap().clone();
                t.tunnel_id = tunnel_id.clone();
                let mut tunnels: Vec<TunnelStatus> = s.tunnels.iter().cloned().collect();
                tunnels.push(t);
                s.tunnels = BoundedVec::try_from(tunnels).unwrap();
            }
            Request::UnregisterTunnel { tunnel_id } => {
                let tunnels: Vec<TunnelStatus> = s
                    .tunnels
                    .iter()
                    .filter(|t| &t.tunnel_id != tunnel_id)
                    .cloned()
                    .collect();
                s.tunnels = BoundedVec::try_from(tunnels).unwrap();
            }
            Request::Connect { tunnel_id } | Request::Reconnect { tunnel_id } => {
                edit(s, tunnel_id, |t| {
                    t.os_state = OsState::Connected;
                    t.recovery = RecoveryPhase::Starting;
                    t.light = Light::Yellow;
                });
            }
            Request::Disconnect { tunnel_id } => edit(s, tunnel_id, |t| {
                t.os_state = OsState::Disconnected;
                t.recovery = RecoveryPhase::Stopping;
                t.light = Light::Yellow;
            }),
            Request::ProbeNow { tunnel_id } => {
                let observe = if self.observe_replies {
                    ProbeResult::Reply { rtt_ms: 7 }
                } else {
                    ProbeResult::NoReply
                };
                let probed = |result| {
                    Some(ProbeSummary {
                        started_at_unix_ms: 1_767_226_000_000,
                        finished_at_unix_ms: 1_767_226_000_100,
                        result,
                    })
                };
                let effect = self.probe_effect;
                edit(s, tunnel_id, |t| {
                    if let Some(r) = &mut t.recovery_target {
                        r.last_probe = match effect {
                            ProbeEffect::Clear => None,
                            _ => probed(ProbeResult::Reply { rtt_ms: 9 }),
                        };
                        if effect == ProbeEffect::Replace {
                            r.target = crate::args::target("set", "test", "198.51.100.7").unwrap();
                        }
                    }
                    let observed: Vec<_> = t
                        .observe_targets
                        .iter()
                        .map(|o| wgal_protocol::TargetStatus {
                            target: o.target,
                            last_probe: match effect {
                                ProbeEffect::All | ProbeEffect::Replace => probed(observe),
                                ProbeEffect::RecoveryOnly => o.last_probe.clone(),
                                ProbeEffect::Clear => None,
                            },
                        })
                        .collect();
                    t.observe_targets = BoundedVec::try_from(observed).unwrap();
                });
            }
            Request::SetMonitoringPaused { paused } => s.monitoring_paused = *paused,
            Request::UpdateNotificationSettings {
                enabled,
                device_name,
                chat_id,
            } => {
                s.notification.enabled = *enabled;
                s.notification.device_name = device_name.clone();
                s.notification.chat_id = chat_id.clone();
            }
            Request::SetBotToken { .. } => s.notification.token = CredentialState::Stored,
            Request::ClearBotToken => s.notification.token = CredentialState::NotSet,
            Request::SetNotificationTemplates { templates } => {
                s.notification.templates_id = Some(templates.id().clone());
            }
            Request::SetCliLanguage { language } => {
                s.notification.cli_language = language.clone();
            }
            Request::SetMcpEnabled { enabled } => s.host.mcp_enabled = *enabled,
            Request::SetMcpTelegramEnabled { enabled } => s.host.mcp_telegram_enabled = *enabled,
            Request::SendTestNotification => {
                s.notification.last_test = Some(TestNotificationStatus {
                    request_id: RequestId::new("cli-test").unwrap(),
                    state: TestNotificationState::Accepted,
                    requested_at_unix_ms: 1_767_226_000_000,
                    finished_at_unix_ms: Some(1_767_226_001_000),
                    failure: None,
                });
            }
            Request::UpdateHostSettings { settings } => {
                s.host.workers.configured = settings.workers;
                s.host.time_zone.configured = settings.time_zone.clone();
            }
            _ => {}
        }
    }

    /// Operations settle after `busy_rounds` looks.
    fn settle(&mut self) {
        if self.busy_rounds > 0 {
            self.busy_rounds -= 1;
            return;
        }
        let tunnels: Vec<TunnelStatus> = self
            .snapshot
            .tunnels
            .iter()
            .cloned()
            .map(|mut t| {
                if matches!(
                    t.recovery,
                    RecoveryPhase::Starting | RecoveryPhase::Stopping
                ) {
                    t.recovery = RecoveryPhase::Idle;
                    t.light = if t.os_state == OsState::Connected {
                        Light::Green
                    } else {
                        Light::Gray
                    };
                }
                t
            })
            .collect();
        self.snapshot.tunnels = BoundedVec::try_from(tunnels).unwrap();
    }
}

fn answer(outcome: Outcome) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(RequestId::new("cli-1-1").unwrap()),
        outcome,
    }
}

impl Watchdog for Fake {
    fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.respond(request, Duration::from_secs(10))
    }

    fn send_within(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        self.respond(request, most.min(Duration::from_secs(10)))
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        Ok(Vec::new())
    }
}

impl Fake {
    /// A look after the first takes `look_takes` on the shared clock, but
    /// at most `most` (the usual deadline, or a shorter one): one that
    /// would take longer is given up on at the limit.
    fn respond(
        &mut self,
        request: &Request,
        most: Duration,
    ) -> Result<ResponseEnvelope, Unreachable> {
        if matches!(request, Request::GetSnapshot) && self.snapshots > 0 {
            self.clock.set(self.clock.get() + self.look_takes.min(most));
            if self.look_takes > most {
                self.sent.push(request.clone());
                self.snapshots += 1;
                return Err(Unreachable::unknown("the watchdog did not answer in time"));
            }
        }
        self.reply(request)
    }

    fn reply(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
        self.sent.push(request.clone());
        if let Some((_, what)) = self.script.iter().find(|(m, _)| m(request)).cloned() {
            return match what {
                Scripted::Refuse(e) => Ok(answer(Outcome::Err(e))),
                Scripted::Lose(Delivery::NotSent) => Err(Unreachable::not_sent(
                    "the WGAL watchdog (wgal-watchdog.service) is not running",
                )),
                Scripted::Lose(Delivery::Unknown) => Err(Unreachable::unknown(
                    "the WGAL watchdog did not answer within 10 seconds; the request may still be carried out, run \"wgal status\" to see",
                )),
                Scripted::Answer(r) => Ok(answer(Outcome::Ok(*r))),
            };
        }
        let response = match request {
            Request::GetSnapshot => {
                let n = self.snapshots;
                self.snapshots += 1;
                if self.snapshots_fail_from.is_some_and(|from| n >= from) {
                    return Err(Unreachable::unknown("the answer was lost"));
                }
                if n > 0 {
                    self.settle();
                }
                if n == 1
                    && let Some(change) = self.second_look.take()
                {
                    change(&mut self.snapshot);
                }
                Response::Snapshot {
                    snapshot: self.snapshot.clone(),
                }
            }
            Request::DiscoverTunnels => response(include_str!(
                "../../wgal-protocol/fixtures/response_discovered.json"
            )),
            Request::ReadEventLog { .. } => response(include_str!(
                "../../wgal-protocol/fixtures/response_event_log.json"
            )),
            Request::ExportSettings => response(include_str!(
                "../../wgal-protocol/fixtures/response_settings_export.json"
            )),
            Request::ImportSettings { .. } => response(include_str!(
                "../../wgal-protocol/fixtures/response_settings_import.json"
            )),
            other => {
                self.carry_out(&other.clone());
                Response::Accepted
            }
        };
        Ok(answer(Outcome::Ok(response)))
    }
}

/// A scripted terminal and standard input.
#[derive(Default)]
struct Term {
    terminal: bool,
    interactive: bool,
    hidden: VecDeque<io::Result<Line>>,
    prompts: Vec<String>,
    answers: VecDeque<Line>,
    stdin: Vec<u8>,
    pauses: usize,
    clock: Rc<Cell<Duration>>,
}

impl Term {
    /// This terminal on `fake`'s clock.
    fn on(mut self, fake: &Fake) -> Self {
        self.clock = Rc::clone(&fake.clock);
        self
    }

    fn piped(stdin: &[u8]) -> Self {
        Self {
            stdin: stdin.to_vec(),
            ..Self::default()
        }
    }

    fn terminal() -> Self {
        Self {
            terminal: true,
            interactive: true,
            ..Self::default()
        }
    }
}

impl Console for Term {
    fn stdin_is_terminal(&self) -> bool {
        self.terminal
    }

    fn interactive(&self) -> bool {
        self.interactive
    }

    fn read_hidden_line(&mut self, prompt: &str, _max: usize) -> io::Result<Line> {
        self.prompts.push(prompt.to_owned());
        self.hidden.pop_front().unwrap_or(Ok(Line::End))
    }

    fn ask(&mut self, prompt: &str, _max: usize) -> io::Result<Line> {
        self.prompts.push(prompt.to_owned());
        Ok(self.answers.pop_front().unwrap_or(Line::End))
    }

    fn read_line(&mut self, max: usize) -> io::Result<Line> {
        read_line_from(&mut io::Cursor::new(std::mem::take(&mut self.stdin)), max)
    }

    fn read_all(&mut self, max: usize) -> io::Result<Input> {
        read_all_from(&mut io::Cursor::new(std::mem::take(&mut self.stdin)), max)
    }

    fn pause(&mut self, time: Duration) {
        self.pauses += 1;
        self.clock.set(self.clock.get() + time);
    }

    fn elapsed(&self) -> Duration {
        self.clock.get()
    }
}

fn cli(args: &[&str], fake: &mut Fake, term: &mut Term) -> Report {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run(&args, fake, term)
}

/// Runs with `--json` and reads the one object it writes.
fn cli_json(args: &[&str], fake: &mut Fake, term: &mut Term) -> (Report, Value) {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let report = cli(&all, fake, term);
    let value: Value = serde_json::from_str(report.stdout()).unwrap_or_else(|e| {
        panic!("not one JSON object ({e}): {}", report.stdout());
    });
    (report, value)
}

fn refusal(code: ErrorCode, reason: Option<ErrorReason>) -> Scripted {
    Scripted::Refuse(ProtocolError::new(code, reason, "refused for the test"))
}

// --- Arguments --------------------------------------------------------------

/// Every mistake ends with 2 and sends nothing at all.
#[test]
fn a_mistake_on_the_command_line_sends_nothing() {
    let long_observe: Vec<String> = (1..=9)
        .flat_map(|n| ["--observe".to_owned(), format!("192.0.2.{n}")])
        .collect();
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["frobnicate"],
        vec!["connect"],
        vec!["connect", "not an id"],
        vec!["disconnect", "office-1"],
        vec!["unregister", "office-1"],
        vec!["notify", "token", "clear"],
        vec!["notify", "token", "set", "extra"],
        vec!["notify", "token"],
        vec!["import"],
        vec!["set", "office-1"],
        vec!["set", "office-1", "--interval", "5"],
        vec!["set", "office-1", "--timeout", "11"],
        vec!["set", "office-1", "--recovery-target", "127.0.0.1"],
        vec!["set", "office-1", "--recovery-target", "192.0.2.1:0"],
        vec!["set", "office-1", "--observe", "192.0.2.9", "--no-observe"],
        vec!["retry", "office-1"],
        vec!["retry", "office-1", "--limit", "9999"],
        vec!["monitor", "office-1", "maybe"],
        vec!["monitor", "office-1"],
        vec!["host"],
        vec!["host", "--workers", "1"],
        vec!["host", "--time-zone", "Not a zone!"],
        vec!["log", "--system", "--watchdog"],
        vec!["log", "--before", "x"],
        vec!["status", "extra"],
        vec!["status", "--unknown"],
        vec!["connect", "office-1", "--wait", "121"],
        vec!["connect", "office-1", "--wait", "5", "--no-wait"],
        vec!["notify", "chat", "0123"],
        vec!["notify", "chat"],
        vec!["notify", "shout"],
        vec!["mcp", "maybe"],
        vec!["--json", "--json", "status"],
        vec!["help", "nothing"],
    ];
    let mut observe = vec!["set", "office-1"];
    observe.extend(long_observe.iter().map(String::as_str));
    cases.push(observe);
    for case in cases {
        let mut fake = Fake::new();
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 2, "{case:?}: {}", report.stderr());
        only_looked(&fake);
        assert!(report.stderr().starts_with("wgal: "), "{case:?}");
        // With --json the mistake is the one object on standard output.
        if case.contains(&"--json") {
            let v: Value = serde_json::from_str(report.stdout()).unwrap();
            assert_eq!(v["error"]["kind"], "usage", "{case:?}");
        } else {
            assert!(report.stdout().is_empty(), "{case:?}");
        }
    }
}

/// A Bot Token on the command line is never used, wherever it is, and is
/// not repeated in what is written.
#[test]
fn a_token_on_the_command_line_is_refused_and_not_repeated() {
    for case in [
        vec!["notify", "device", TOKEN],
        vec!["notify", "token", "set", TOKEN],
        vec!["status", &format!("--x={TOKEN}")],
        vec!["--json", "show", TOKEN],
    ] {
        let mut fake = Fake::new();
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 2, "{case:?}");
        only_looked(&fake);
        let all = format!("{}{}", report.stdout(), report.stderr());
        assert!(!all.contains(TOKEN), "{all}");
        assert!(!all.contains("AAEhBP0av28"), "{all}");
        assert!(all.contains("@BotFather"), "{all}");
    }
}

/// Help looks at the watchdog once, for the language, and changes
/// nothing; the version reads nothing at all (owner decision of
/// 2026-11-19).
#[test]
fn help_only_looks_and_the_version_sends_nothing() {
    for case in [
        vec![],
        vec!["help"],
        vec!["--help"],
        vec!["help", "set"],
        vec!["set", "--help"],
        vec!["help", "language"],
    ] {
        let mut fake = Fake::new();
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 0, "{case:?}");
        only_looked(&fake);
        assert!(!report.stdout().is_empty(), "{case:?}");
    }
    for case in [vec!["--version"], vec!["-V"], vec!["version"]] {
        let mut fake = Fake::new();
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 0, "{case:?}");
        assert!(fake.sent.is_empty(), "{case:?}");
    }
    let mut fake = Fake::new();
    let report = cli(&["--version"], &mut fake, &mut Term::default());
    assert_eq!(
        report.stdout(),
        format!("wgal {}\n", env!("CARGO_PKG_VERSION"))
    );
}

// --- What each command sends ------------------------------------------------

/// Each command sends its request once, after the first look at the
/// watchdog's state.
#[test]
fn each_command_sends_its_request() {
    let office = || id("office-1");
    let cases: Vec<(Vec<&str>, Vec<Request>)> = vec![
        (vec!["status"], vec![]),
        (vec!["list"], vec![]),
        (vec!["show", "office-1"], vec![]),
        (vec!["discover"], vec![Request::DiscoverTunnels]),
        (
            vec!["register", "new-4"],
            vec![Request::RegisterTunnel {
                tunnel_id: id("new-4"),
            }],
        ),
        (
            vec!["unregister", "office-1", "--yes"],
            vec![Request::UnregisterTunnel {
                tunnel_id: office(),
            }],
        ),
        (
            vec!["monitor", "office-1", "off"],
            vec![Request::SetTunnelMonitoring {
                tunnel_id: office(),
                enabled: false,
            }],
        ),
        (
            vec!["auto-recovery", "office-1", "off"],
            vec![Request::SetAutoRecovery {
                tunnel_id: office(),
                enabled: false,
            }],
        ),
        (
            vec!["start-when-stopped", "office-1", "on"],
            vec![Request::SetStartWhenStoppedExternally {
                tunnel_id: office(),
                enabled: true,
            }],
        ),
        (
            vec!["retry", "office-1", "--limit", "forever"],
            vec![Request::SetRecoveryRetry {
                tunnel_id: office(),
                retry: wgal_protocol::RecoveryRetry::new(None, 60).unwrap(),
            }],
        ),
        (
            vec!["connect", "office-1"],
            vec![Request::Connect {
                tunnel_id: office(),
            }],
        ),
        (
            vec!["reconnect", "office-1"],
            vec![Request::Reconnect {
                tunnel_id: office(),
            }],
        ),
        (
            vec!["disconnect", "office-1", "--yes"],
            vec![Request::Disconnect {
                tunnel_id: office(),
            }],
        ),
        (
            vec!["probe", "office-1"],
            vec![Request::ProbeNow {
                tunnel_id: office(),
            }],
        ),
        (
            vec!["pause"],
            vec![Request::SetMonitoringPaused { paused: true }],
        ),
        (
            vec!["resume"],
            vec![Request::SetMonitoringPaused { paused: false }],
        ),
        (
            vec!["notify", "tunnel", "office-1", "off"],
            vec![Request::SetTunnelNotifications {
                tunnel_id: office(),
                enabled: false,
            }],
        ),
        (
            vec!["notify", "token", "clear", "--yes"],
            vec![Request::ClearBotToken],
        ),
        (vec!["notify", "test"], vec![Request::SendTestNotification]),
        (
            vec!["mcp", "off"],
            vec![Request::SetMcpEnabled { enabled: false }],
        ),
        (
            vec!["mcp", "telegram", "off"],
            vec![Request::SetMcpTelegramEnabled { enabled: false }],
        ),
        (
            vec!["log", "--tunnel", "office-1", "--before", "42"],
            vec![Request::ReadEventLog {
                filter: wgal_protocol::EventFilter::Tunnel {
                    tunnel_id: office(),
                },
                before_seq: Some(42),
            }],
        ),
        (
            vec!["log"],
            vec![Request::ReadEventLog {
                filter: wgal_protocol::EventFilter::All,
                before_seq: None,
            }],
        ),
        (vec!["export"], vec![Request::ExportSettings]),
    ];
    for (case, expected) in cases {
        let mut fake = Fake::new();
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 0, "{case:?}: {}", report.stderr());
        assert_eq!(fake.sent.first(), Some(&Request::GetSnapshot), "{case:?}");
        let changes: Vec<Request> = fake.changes().into_iter().cloned().collect();
        assert_eq!(changes, expected, "{case:?}");
    }
}

/// `set`, `retry`, `host` and `notify device|chat|on|off` change only what
/// is given; the rest is sent as it was.
#[test]
fn a_change_of_one_setting_keeps_the_others() {
    let mut fake = Fake::new();
    let before = fake.snapshot.tunnels.iter().next().unwrap().clone();
    let report = cli(
        &["set", "office-1", "--timeout", "3"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    let Request::UpdateMonitorSettings { settings, .. } = fake.changes()[0] else {
        panic!("{:?}", fake.sent);
    };
    assert_eq!(
        settings,
        &MonitorSettings {
            recovery_target: before.recovery_target.as_ref().map(|r| r.target),
            observe_targets: BoundedVec::try_from(
                before
                    .observe_targets
                    .iter()
                    .map(|o| o.target)
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            probe: wgal_protocol::ProbeTiming::new(
                before.probe.interval_secs(),
                3,
                before.probe.retries(),
                before.probe.min_fault_secs()
            )
            .unwrap(),
        }
    );

    let mut fake = Fake::new();
    let report = cli(
        &[
            "set",
            "office-1",
            "--recovery-target",
            "192.0.2.7:443",
            "--no-observe",
        ],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    let Request::UpdateMonitorSettings { settings, .. } = fake.changes()[0] else {
        panic!();
    };
    assert_eq!(settings.probe, before.probe);
    assert!(settings.observe_targets.is_empty());
    let target = settings.recovery_target.unwrap();
    assert_eq!(target.address.get().to_string(), "192.0.2.7");
    assert!(matches!(target.probe, ProbeKind::TcpConnect { port } if port.get() == 443));

    let mut fake = Fake::new();
    let n = fake.snapshot.notification.clone();
    cli(
        &["notify", "device", "Lab\tbox"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(
        fake.changes(),
        vec![&Request::UpdateNotificationSettings {
            enabled: n.enabled,
            device_name: wgal_protocol::DeviceName::fitted("Lab box"),
            chat_id: n.chat_id.clone(),
        }]
    );
    let mut fake = Fake::new();
    cli(
        &["notify", "chat", "--clear"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(
        fake.changes(),
        vec![&Request::UpdateNotificationSettings {
            enabled: n.enabled,
            device_name: n.device_name.clone(),
            chat_id: None,
        }]
    );
    let mut fake = Fake::new();
    cli(&["notify", "off"], &mut fake, &mut Term::default());
    assert_eq!(
        fake.changes(),
        vec![&Request::UpdateNotificationSettings {
            enabled: false,
            device_name: n.device_name.clone(),
            chat_id: n.chat_id.clone(),
        }]
    );
    let mut fake = Fake::new();
    let host = fake.snapshot.host.clone();
    cli(&["host", "--workers", "6"], &mut fake, &mut Term::default());
    assert_eq!(
        fake.changes(),
        vec![&Request::UpdateHostSettings {
            settings: wgal_protocol::HostSettings {
                workers: Some(wgal_protocol::WorkerCount::new(6).unwrap()),
                time_zone: host.time_zone.configured.clone(),
            }
        }]
    );
    let mut fake = Fake::new();
    let retry = fake.snapshot.tunnels.iter().next().unwrap().retry;
    cli(
        &["retry", "office-1", "--interval", "300"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(
        fake.changes(),
        vec![&Request::SetRecoveryRetry {
            tunnel_id: id("office-1"),
            retry: wgal_protocol::RecoveryRetry::new(retry.limit(), 300).unwrap(),
        }]
    );
}

#[test]
fn a_tunnel_that_is_not_registered_is_said_and_nothing_is_changed() {
    let mut fake = Fake::new();
    let report = cli(
        &["monitor", "nope-9", "on"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 1);
    assert!(fake.changes().is_empty());
    assert!(
        report.stderr().contains("\"wgal list\""),
        "{}",
        report.stderr()
    );
}

// --- How it ends ------------------------------------------------------------

/// Each kind of failure, its exit code and its JSON object.
#[test]
fn each_failure_has_its_exit_code_and_json_shape() {
    let is_monitor = |r: &Request| matches!(r, Request::SetTunnelMonitoring { .. });
    // Refused: code 1, the code and reason, no delivery.
    let mut fake = Fake::new().on(
        is_monitor,
        refusal(
            ErrorCode::Unauthorized,
            Some(ErrorReason::NeedsAdministratorOrListedTunnel),
        ),
    );
    let (report, v) = cli_json(
        &["monitor", "office-1", "off"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 1);
    assert_eq!(v["error"]["kind"], "refused");
    assert_eq!(v["error"]["exit"], 1);
    assert_eq!(v["error"]["refused"]["code"], "unauthorized");
    assert_eq!(
        v["error"]["refused"]["reason"],
        "needs_administrator_or_listed_tunnel"
    );
    assert!(v["error"].get("delivery").is_none());
    assert!(
        report.stderr().contains("access list"),
        "{}",
        report.stderr()
    );

    // Not reached: 3, nothing changed.
    let mut fake = Fake::new().on(
        |r| matches!(r, Request::GetSnapshot),
        Scripted::Lose(Delivery::NotSent),
    );
    let (report, v) = cli_json(&["status"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 3);
    assert_eq!(v["error"]["kind"], "unreachable");
    assert_eq!(v["error"]["delivery"], "not_sent");

    // Sent, no answer: 4.
    let mut fake = Fake::new().on(is_monitor, Scripted::Lose(Delivery::Unknown));
    let (report, v) = cli_json(
        &["monitor", "office-1", "off"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4);
    assert_eq!(v["error"]["kind"], "unreachable");
    assert_eq!(v["error"]["delivery"], "unknown");

    // Accepted, but the state afterwards could not be read: 4, and what
    // was done is said.
    let mut fake = Fake::new();
    fake.snapshots_fail_from = Some(1);
    let (report, v) = cli_json(
        &["monitor", "office-1", "off"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4);
    assert_eq!(v["error"]["kind"], "unconfirmed");
    assert_eq!(v["error"]["accepted"], true);
    assert_eq!(v["error"]["done"][0], "monitoring was turned off");
    assert!(
        report
            .stderr()
            .contains("before this, monitoring was turned off")
    );
    assert_eq!(fake.changes().len(), 1, "sent once, never again");

    // A usage mistake.
    let (report, v) = cli_json(&["connect"], &mut Fake::new(), &mut Term::default());
    assert_eq!(report.code(), 2);
    assert_eq!(v["error"]["kind"], "usage");
    assert!(v["error"].get("refused").is_none());
    assert!(v["error"].get("delivery").is_none());

    // Input: a token that is not one.
    let mut term = Term::piped(b"not-a-token\n");
    let (report, v) = cli_json(&["notify", "token", "set"], &mut Fake::new(), &mut term);
    assert_eq!(report.code(), 2);
    assert_eq!(v["error"]["kind"], "input");

    // Interrupted while the token is typed.
    let mut term = Term::terminal();
    term.hidden
        .push_back(Err(io::Error::from(io::ErrorKind::Interrupted)));
    let mut fake = Fake::new();
    let (report, v) = cli_json(&["notify", "token", "set"], &mut fake, &mut term);
    assert_eq!(report.code(), 130);
    assert_eq!(v["error"]["kind"], "interrupted");
    assert_eq!(v["error"]["exit"], 130);
    only_looked(&fake);
}

/// An operation that does not settle in time is "not confirmed" (4); one
/// that settles where it should not is a failure (1); one that settles
/// right is done (0).
#[test]
fn an_operation_is_judged_once_it_settles() {
    // Settles after a few looks: done.
    let mut fake = Fake::new();
    fake.busy_rounds = 2;
    let mut term = Term::default();
    let report = cli(&["connect", "office-1"], &mut fake, &mut term);
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert!(report.stdout().contains("Traffic through it is verified"));
    assert_eq!(term.pauses, 3);

    // Never settles within the wait: 4.
    let mut fake = Fake::new();
    fake.busy_rounds = 1000;
    let mut term = Term::default();
    let (report, v) = cli_json(
        &["connect", "office-1", "--wait", "5"],
        &mut fake,
        &mut term,
    );
    assert_eq!(report.code(), 4);
    assert_eq!(v["error"]["kind"], "unconfirmed");
    assert_eq!(term.pauses, 5);
    assert_eq!(fake.changes().len(), 1);

    // Settles disconnected after a connect: 1.
    let mut fake = Fake::new();
    fake.apply = false;
    edit(&mut fake.snapshot, &id("office-1"), |t| {
        t.os_state = OsState::Disconnected;
        t.light = Light::Gray;
    });
    let (report, v) = cli_json(&["connect", "office-1"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 1, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "failed");
    assert_eq!(v["error"]["result"]["tunnel"]["os_state"], "disconnected");

    // Connected, monitored, but no traffic: 1.
    let mut fake = Fake::new();
    fake.apply = false;
    edit(&mut fake.snapshot, &id("office-1"), |t| {
        t.light = Light::Red
    });
    let report = cli(&["connect", "office-1"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 1);
    assert!(report.stderr().contains("no traffic gets through it"));

    // --no-wait: accepted, no look afterwards.
    let mut fake = Fake::new();
    let mut term = Term::default();
    let report = cli(
        &["disconnect", "office-1", "--yes", "--no-wait"],
        &mut fake,
        &mut term,
    );
    assert_eq!(report.code(), 0);
    assert_eq!(term.pauses, 0);
    assert_eq!(fake.snapshots, 1);
}

#[test]
fn a_probe_waits_for_a_new_result_and_says_what_each_target_answered() {
    let mut fake = Fake::new();
    let report = cli(&["probe", "office-1"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert!(
        report
            .stdout()
            .contains("recovery target 192.0.2.1 (ICMP): replied in 9 ms"),
        "{}",
        report.stdout()
    );
    assert!(
        report.stdout().contains("observe target"),
        "{}",
        report.stdout()
    );
    // A target that does not answer: 1, with every answer.
    let mut fake = Fake::new();
    fake.observe_replies = false;
    let report = cli(&["probe", "office-1"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 1, "{}", report.stderr());
    assert!(report.stderr().contains("no reply"), "{}", report.stderr());
    assert!(
        report.stderr().contains("replied in 9 ms"),
        "{}",
        report.stderr()
    );
    // No new result in time: 4.
    let mut fake = Fake::new();
    fake.apply = false;
    let report = cli(
        &["probe", "office-1", "--wait", "3"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4);
}

/// Shows a failed test message once one is asked for.
struct FailingTest(Fake);

impl Watchdog for FailingTest {
    fn send(&mut self, r: &Request) -> Result<ResponseEnvelope, Unreachable> {
        if matches!(r, Request::SendTestNotification) {
            self.0.snapshot.notification.last_test = Some(TestNotificationStatus {
                request_id: RequestId::new("cli-test").unwrap(),
                state: TestNotificationState::Failed,
                requested_at_unix_ms: 1_767_226_000_000,
                finished_at_unix_ms: Some(1_767_226_001_000),
                failure: Some(wgal_protocol::NotificationFailure::NotConfigured),
            });
        }
        self.0.send(r)
    }

    fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
        Ok(Vec::new())
    }
}

#[test]
fn a_test_message_is_followed_until_telegram_answers() {
    let mut fake = Fake::new();
    let report = cli(&["notify", "test"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert!(report.stdout().contains("Telegram accepted"));
    // Failed: 1, with the reason.
    let mut inner = Fake::new();
    inner.apply = false;
    let mut failing = FailingTest(inner);
    let args: Vec<OsString> = ["notify", "test"].iter().map(OsString::from).collect();
    let report = run(&args, &mut failing, &mut Term::default());
    assert_eq!(report.code(), 1, "{}", report.stderr());
    assert!(
        report.stderr().contains("not_configured"),
        "{}",
        report.stderr()
    );
    // No answer from Telegram in time: 4.
    let mut fake = Fake::new();
    fake.apply = false;
    let report = cli(
        &["notify", "test", "--wait", "2"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
}

// --- Import and export ------------------------------------------------------

fn import_result(tunnels: Vec<(ImportOutcome, AutoRecoveryImport)>, dry_run: bool) -> Response {
    Response::SettingsImport {
        result: ImportResult {
            dry_run,
            host: ImportOutcome::Applied,
            notifications: Some(ImportOutcome::Applied),
            tunnels: BoundedVec::try_from(
                tunnels
                    .into_iter()
                    .enumerate()
                    .map(|(i, (outcome, auto_recovery))| ImportItem {
                        index: u16::try_from(i).unwrap(),
                        name: DisplayName::new(format!("t{i}")).unwrap(),
                        tunnel_id: Some(id(&format!("t-{i}"))),
                        outcome,
                        auto_recovery,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        },
    }
}

fn exported_document() -> Vec<u8> {
    let mut fake = Fake::new();
    let report = cli(&["export"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    report.stdout().as_bytes().to_vec()
}

/// What `wgal export` writes, `wgal import` reads back as it was.
#[test]
fn an_export_goes_back_in_unchanged() {
    let file = exported_document();
    let mut fake = Fake::new();
    let report = cli(&["import", "--dry-run"], &mut fake, &mut Term::piped(&file));
    assert_eq!(report.code(), 0, "{}", report.stderr());
    let Request::ImportSettings { document, options } = fake.changes()[0] else {
        panic!("{:?}", fake.sent);
    };
    assert!(options.dry_run);
    let Response::SettingsExport { document: exported } = response(include_str!(
        "../../wgal-protocol/fixtures/response_settings_export.json"
    )) else {
        panic!();
    };
    assert_eq!(document, &exported);
}

/// Each part's result counts, a refused automatic recovery as a failure
/// even when the settings were set, a wait for the baseline not.
#[test]
fn an_import_ends_by_what_each_part_did() {
    let file = exported_document();
    let cases = [
        (
            vec![(ImportOutcome::Applied, AutoRecoveryImport::On)],
            false,
            0,
        ),
        (
            vec![(ImportOutcome::NotFound, AutoRecoveryImport::Off)],
            false,
            0,
        ),
        (
            vec![(
                ImportOutcome::Applied,
                AutoRecoveryImport::WaitingForBaseline,
            )],
            false,
            0,
        ),
        (
            vec![(ImportOutcome::Ambiguous, AutoRecoveryImport::Off)],
            false,
            1,
        ),
        (
            vec![
                (ImportOutcome::Applied, AutoRecoveryImport::Off),
                (
                    ImportOutcome::Applied,
                    AutoRecoveryImport::Refused {
                        code: ErrorCode::Unauthorized,
                        reason: Some(ErrorReason::NeedsAdministratorOrListedTunnel),
                    },
                ),
            ],
            false,
            1,
        ),
        (
            vec![(
                ImportOutcome::Refused {
                    code: ErrorCode::Unauthorized,
                    reason: None,
                },
                AutoRecoveryImport::Off,
            )],
            true,
            1,
        ),
        (
            vec![(ImportOutcome::Registered, AutoRecoveryImport::On)],
            true,
            0,
        ),
    ];
    for (tunnels, dry_run, code) in cases {
        let what = format!("{tunnels:?} dry_run {dry_run}");
        let answer = import_result(tunnels, dry_run);
        let mut fake = Fake::new();
        fake.script.push((
            |r| matches!(r, Request::ImportSettings { .. }),
            Scripted::Answer(Box::new(answer)),
        ));
        let args = if dry_run {
            vec!["import", "--dry-run"]
        } else {
            vec!["import", "--yes"]
        };
        let (report, v) = cli_json(&args, &mut fake, &mut Term::piped(&file));
        assert_eq!(report.code(), code, "{what}: {}", report.stderr());
        if code == 1 {
            assert_eq!(v["error"]["kind"], "failed", "{what}");
            assert!(v["error"]["result"]["tunnels"].is_array(), "{what}");
        } else {
            assert!(v["result"]["tunnels"].is_array(), "{what}");
        }
    }
    // In words: the settings were set, but automatic recovery was refused.
    let mut fake = Fake::new();
    fake.script.push((
        |r| matches!(r, Request::ImportSettings { .. }),
        Scripted::Answer(Box::new(import_result(
            vec![(
                ImportOutcome::Applied,
                AutoRecoveryImport::Refused {
                    code: ErrorCode::Unauthorized,
                    reason: None,
                },
            )],
            false,
        ))),
    ));
    let report = cli(&["import", "--yes"], &mut fake, &mut Term::piped(&file));
    assert!(
        report
            .stderr()
            .contains("the settings were set, but automatic recovery was refused"),
        "{}",
        report.stderr()
    );
    // A host part that failed counts too.
    let mut answer = import_result(vec![], false);
    if let Response::SettingsImport { result } = &mut answer {
        result.host = ImportOutcome::Refused {
            code: ErrorCode::Unauthorized,
            reason: None,
        };
    }
    let mut fake = Fake::new();
    fake.script.push((
        |r| matches!(r, Request::ImportSettings { .. }),
        Scripted::Answer(Box::new(answer)),
    ));
    assert_eq!(
        cli(&["import", "--yes"], &mut fake, &mut Term::piped(&file)).code(),
        1
    );
}

/// Standard input is read up to its limit, and what is not a settings file
/// sends nothing.
#[test]
fn an_import_input_that_is_wrong_sends_nothing() {
    let mut file = exported_document();
    let too_long = vec![b' '; MAX_IMPORT_BYTES + 1];
    let mut trailing = file.clone();
    trailing.extend_from_slice(b"{}");
    file.truncate(file.len() / 2);
    for input in [too_long, trailing, file, b"[]".to_vec(), Vec::new()] {
        let mut fake = Fake::new();
        let (report, v) = cli_json(
            &["import", "--dry-run"],
            &mut fake,
            &mut Term::piped(&input),
        );
        assert_eq!(report.code(), 2, "{}", report.stderr());
        assert_eq!(v["error"]["kind"], "input");
        only_looked(&fake);
    }
    // Standard input a terminal: refused at once instead of waiting.
    let mut fake = Fake::new();
    let (report, v) = cli_json(&["import", "--dry-run"], &mut fake, &mut Term::terminal());
    assert_eq!(report.code(), 2, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "usage");
    only_looked(&fake);
}

/// Text shaped like a token in the settings: the export is refused, not
/// changed, and the text is not repeated.
#[test]
fn an_export_holding_a_token_shape_is_refused() {
    let mut fake = Fake::new();
    let Response::SettingsExport { mut document } = response(include_str!(
        "../../wgal-protocol/fixtures/response_settings_export.json"
    )) else {
        panic!();
    };
    let mut tunnels: Vec<_> = document.tunnels.iter().cloned().collect();
    tunnels[0].name = DisplayName::new(format!("vpn {TOKEN}")).unwrap();
    document.tunnels = BoundedVec::try_from(tunnels).unwrap();
    fake.script.push((
        |r| matches!(r, Request::ExportSettings),
        Scripted::Answer(Box::new(Response::SettingsExport { document })),
    ));
    let (report, v) = cli_json(&["export"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 1);
    assert_eq!(v["error"]["kind"], "export_refused");
    assert_eq!(v["error"]["where"], "tunnels[0].name");
    assert!(v["error"].get("refused").is_none());
    let all = format!("{}{}", report.stdout(), report.stderr());
    assert!(!all.contains("AAEhBP0av28"), "{all}");
}

// --- The Bot Token ----------------------------------------------------------

/// At a terminal the token is asked for unseen; piped in, one line is
/// read. Either way it is sent as it is and never written back.
#[test]
fn the_token_is_read_unseen_or_from_standard_input_and_sent_as_it_is() {
    let token = BotToken::new(TOKEN).unwrap();
    let mut term = Term::terminal();
    term.hidden
        .push_back(Ok(Line::Read(TOKEN.as_bytes().to_vec())));
    let mut fake = Fake::new();
    let report = cli(&["notify", "token", "set"], &mut fake, &mut term);
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(
        fake.changes(),
        vec![&Request::SetBotToken {
            token: token.clone()
        }]
    );
    assert_eq!(term.prompts.len(), 1);
    assert!(term.prompts[0].contains("not shown"));

    let mut term = Term::piped(format!("{TOKEN}\r\n").as_bytes());
    let mut fake = Fake::new();
    let (report, _) = cli_json(&["notify", "token", "set"], &mut fake, &mut term);
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(fake.changes(), vec![&Request::SetBotToken { token }]);
    assert!(!report.stdout().contains("AAEhBP0av28"));
    assert!(!report.stderr().contains("AAEhBP0av28"));

    // Too long, none, not one: refused, nothing changed.
    let long = "1".repeat(MAX_TOKEN_BYTES + 1);
    for input in [long.as_bytes(), b"", b"123:short\n", b"\xff\xfe\n"] {
        let mut fake = Fake::new();
        let report = cli(
            &["notify", "token", "set"],
            &mut fake,
            &mut Term::piped(input),
        );
        assert_eq!(report.code(), 2, "{input:?}");
        only_looked(&fake);
    }
}

/// Nothing written holds text shaped like a token, wherever it came from:
/// a name the watchdog reports, its error, the log.
#[test]
fn nothing_written_holds_a_token_shape() {
    let mut fake = Fake::new();
    fake.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted(TOKEN);
    edit(&mut fake.snapshot, &id("office-1"), |t| {
        t.display_name = DisplayName::new(format!("x{TOKEN}")).unwrap();
    });
    for case in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["list"],
        vec!["show", "office-1"],
        vec!["--json", "show", "office-1"],
        vec!["notify"],
        vec!["--json", "notify"],
        vec!["monitor", "office-1", "off"],
    ] {
        let mut f = Fake::new();
        f.snapshot = fake.snapshot.clone();
        let report = cli(&case, &mut f, &mut Term::default());
        let all = format!("{}{}", report.stdout(), report.stderr());
        assert!(!all.contains("AAEhBP0av28"), "{case:?}: {all}");
        assert!(
            all.contains("[hidden: shaped like a Bot Token]"),
            "{case:?}: {all}"
        );
        if case.contains(&"--json") {
            let _: Value = serde_json::from_str(report.stdout()).unwrap();
        }
    }
    // In a refusal's message.
    let mut fake = Fake::new().on(
        |r| matches!(r, Request::SetTunnelMonitoring { .. }),
        Scripted::Refuse(ProtocolError::new(
            ErrorCode::InvalidSetting,
            None,
            &format!("bad {TOKEN}"),
        )),
    );
    let report = cli(
        &["monitor", "office-1", "off"],
        &mut fake,
        &mut Term::default(),
    );
    assert!(
        !report.stderr().contains("AAEhBP0av28"),
        "{}",
        report.stderr()
    );
}

// --- MCP --------------------------------------------------------------------

#[test]
fn turning_mcp_on_asks_a_person_at_a_terminal() {
    // Not a terminal: refused, nothing changed.
    let mut fake = Fake::new();
    let report = cli(&["mcp", "on"], &mut fake, &mut Term::piped(b"yes\n"));
    assert_eq!(report.code(), 2);
    only_looked(&fake);
    // A terminal, and yes.
    let mut term = Term::terminal();
    term.answers.push_back(Line::Read(b"yes".to_vec()));
    let mut fake = Fake::new();
    let report = cli(&["mcp", "telegram", "on"], &mut fake, &mut term);
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(
        fake.changes(),
        vec![&Request::SetMcpTelegramEnabled { enabled: true }]
    );
    assert!(term.prompts[0].contains("Bot Token"));
    // Anything but yes.
    let mut term = Term::terminal();
    term.answers.push_back(Line::Read(b"y".to_vec()));
    let mut fake = Fake::new();
    let report = cli(&["mcp", "on"], &mut fake, &mut term);
    assert_eq!(report.code(), 2);
    only_looked(&fake);
    // Off needs no confirmation.
    let mut fake = Fake::new();
    fake.snapshot.host.mcp_enabled = true;
    assert_eq!(
        cli(&["mcp", "off"], &mut fake, &mut Term::default()).code(),
        0
    );
}

// --- Versions ---------------------------------------------------------------

#[test]
fn a_watchdog_older_than_2_1_0_is_refused_before_anything_is_changed() {
    let mut fake = Fake::new();
    fake.snapshot.host.watchdog_version = ProductVersion::new("2.0.0").unwrap();
    let report = cli(
        &["monitor", "office-1", "off"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 1);
    assert!(fake.changes().is_empty());
    assert!(report.stderr().contains("2.1.0"));
}

// --- Languages --------------------------------------------------------------

const FIELDS: [&str; 10] = [
    "formal",
    "result_failed",
    "result_not_executed",
    "retry_limited",
    "retry_forever",
    "retry_none",
    "exhausted",
    "recovered",
    "test",
    "unnamed_device",
];

/// The table from the window's language files, as `languages.json` must
/// be; `WGAL_UPDATE_FIXTURES=1` writes it.
#[test]
fn the_language_table_is_the_windows() {
    let lang = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src/lang");
    let list: Value =
        serde_json::from_str(&std::fs::read_to_string(lang.join("languages.json")).unwrap())
            .unwrap();
    let mut table = Vec::new();
    for entry in list["languages"].as_array().unwrap() {
        let tag = entry["tag"].as_str().unwrap();
        let file: Value = serde_json::from_str(
            &std::fs::read_to_string(lang.join(format!("{tag}.json"))).unwrap(),
        )
        .unwrap();
        let texts: Vec<&str> = FIELDS
            .iter()
            .map(|f| file["notification_templates"][f].as_str().unwrap())
            .collect();
        let mut templates = serde_json::Map::new();
        templates.insert(
            "id".to_owned(),
            Value::from(languages::templates_id(tag, &texts)),
        );
        for (field, text) in FIELDS.iter().zip(&texts) {
            templates.insert((*field).to_owned(), Value::from(*text));
        }
        table.push(serde_json::json!({
            "tag": tag,
            "name": file["_meta"]["name"],
            "templates": templates,
        }));
    }
    let expected = Value::Array(table);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/languages.json");
    if std::env::var_os("WGAL_UPDATE_FIXTURES").is_some() {
        let text = format!("{}\n", serde_json::to_string_pretty(&expected).unwrap());
        std::fs::write(&path, text).unwrap();
    }
    let committed: Value = serde_json::from_str(languages::TABLE).unwrap();
    assert_eq!(
        committed, expected,
        "run WGAL_UPDATE_FIXTURES=1 cargo test -p wgal-cli"
    );
    // Every set is one the watchdog takes, and they are 13.
    assert_eq!(languages::all().len(), 13);
    for l in languages::all() {
        assert!(
            l.templates
                .id()
                .as_str()
                .starts_with(&format!("{}/", l.tag))
        );
    }
}

/// The window's own test values (`apps/desktop/src/templates.test.ts`).
#[test]
fn sets_are_named_as_the_window_names_them() {
    assert_eq!(format!("{:x}", languages::fnv1a32(b"")), "811c9dc5");
    assert_eq!(format!("{:x}", languages::fnv1a32(b"a")), "e40c292c");
    assert_eq!(format!("{:x}", languages::fnv1a32(b"foobar")), "bf9cf968");
    assert_eq!(
        languages::find("zh-TW").unwrap().templates.id().as_str(),
        "zh-TW/f967042d"
    );
    // The English set is the watchdog's built-in one, by name too.
    let builtin =
        include_str!("../../wgal-protocol/fixtures/request_set_notification_templates.json");
    let request: Value = serde_json::from_str(builtin).unwrap();
    let en = &languages::find("EN").unwrap().templates;
    assert_eq!(request["request"]["templates"]["id"], en.id().as_str());
    assert_eq!(
        serde_json::to_value(en).unwrap(),
        request["request"]["templates"]
    );
}

/// Which set is in use, by the watchdog's version for its built-in one.
#[test]
fn the_language_in_use_is_told_by_the_set_and_the_version() {
    let v = |s: &str| ProductVersion::new(s).unwrap();
    let ja = languages::find("ja").unwrap();
    assert_eq!(
        languages::in_use(None, &v("3.0.0")),
        languages::InUse::BuiltIn { english: true }
    );
    assert_eq!(
        languages::in_use(None, &v("2.4.0")),
        languages::InUse::BuiltIn { english: false }
    );
    assert_eq!(
        languages::in_use(Some(ja.templates.id()), &v("2.4.0")),
        languages::InUse::Known(ja)
    );
    assert_eq!(
        languages::in_use(Some(&TemplatesId::new("ja/00000000").unwrap()), &v("3.0.0")),
        languages::InUse::OtherTexts(ja)
    );
    assert_eq!(
        languages::in_use(Some(&TemplatesId::new("xx/1").unwrap()), &v("3.0.0")),
        languages::InUse::Unknown("xx/1".to_owned())
    );
    // As status and notify say it.
    let mut fake = Fake::new();
    fake.snapshot.notification.templates_id = None;
    fake.snapshot.host.watchdog_version = v("2.4.0");
    let report = cli(&["notify"], &mut fake, &mut Term::default());
    assert!(
        report.stdout().contains("Traditional Chinese"),
        "{}",
        report.stdout()
    );
}

#[test]
fn a_language_is_handed_over_and_checked() {
    let mut fake = Fake::new();
    let report = cli(
        &["notify", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    let ja = languages::find("ja").unwrap();
    assert_eq!(
        fake.changes(),
        vec![&Request::SetNotificationTemplates {
            templates: ja.templates.clone()
        }]
    );
    // Accepted, but another set is in use afterwards: 4.
    let mut fake = Fake::new();
    fake.apply = false;
    let report = cli(
        &["notify", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    // Not a language: 2, nothing changed.
    let mut fake = Fake::new();
    let report = cli(
        &["notify", "language", "xx"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 2);
    only_looked(&fake);
    // The list.
    let mut fake = Fake::new();
    let report = cli(&["notify", "language"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0);
    assert!(report.stdout().contains("日本語"));
    assert!(fake.changes().is_empty());
}

// --- Reading lines ----------------------------------------------------------

#[test]
fn a_line_is_read_up_to_its_limit_and_never_cut() {
    let read =
        |input: &[u8], max| read_line_from(&mut io::Cursor::new(input.to_vec()), max).unwrap();
    assert_eq!(read(b"abc\n", 3), Line::Read(b"abc".to_vec()));
    assert_eq!(read(b"abc\r\n", 3), Line::Read(b"abc".to_vec()));
    assert_eq!(read(b"abc", 3), Line::Read(b"abc".to_vec()));
    assert_eq!(read(b"abcd\n", 3), Line::TooLong);
    assert_eq!(read(b"abcdefgh", 3), Line::TooLong);
    assert_eq!(read(b"", 3), Line::End);
    let all = |input: &[u8], max| read_all_from(&mut io::Cursor::new(input.to_vec()), max).unwrap();
    assert_eq!(all(b"abc", 3), Input::Read(b"abc".to_vec()));
    assert_eq!(all(b"abcd", 3), Input::TooLong);
}

#[test]
fn times_are_written_in_the_watchdogs_zone() {
    let s = fixture_snapshot();
    let clock = crate::show::Clock::of(&s);
    assert_eq!(clock.at(1_767_225_600_000), "2026-01-01 08:00:00 +08:00");
    assert_eq!(
        crate::show::Clock::utc().at(0),
        "1970-01-01 00:00:00 +00:00"
    );
    assert_eq!(crate::show::offset(-12_600), "-03:30");
}

// --- Codex review of the CLI, first round -----------------------------------

/// A wait ends on time by the clock, however long each look takes, not
/// after so many looks (P2-2): within the wait and one last look of at
/// most a second.
#[test]
fn a_wait_ends_on_time_however_slow_the_looks_are() {
    for takes in [4, 30] {
        let what = format!("each look takes {takes} s");
        // connect, never settling.
        let mut fake = Fake::new();
        fake.busy_rounds = 1000;
        fake.look_takes = Duration::from_secs(takes);
        let mut term = Term::default().on(&fake);
        let report = cli(
            &["connect", "office-1", "--wait", "10"],
            &mut fake,
            &mut term,
        );
        assert_eq!(report.code(), 4, "{what}: {}", report.stderr());
        assert!(
            term.elapsed() <= Duration::from_secs(11),
            "{what}: {:?}",
            term.elapsed()
        );
        assert!(
            term.elapsed() >= Duration::from_secs(10),
            "{what}: {:?}",
            term.elapsed()
        );
        // probe, no new result.
        let mut fake = Fake::new();
        fake.apply = false;
        fake.look_takes = Duration::from_secs(takes);
        let mut term = Term::default().on(&fake);
        let report = cli(&["probe", "office-1", "--wait", "10"], &mut fake, &mut term);
        assert_eq!(report.code(), 4, "{what}: {}", report.stderr());
        assert!(
            term.elapsed() <= Duration::from_secs(11),
            "{what}: {:?}",
            term.elapsed()
        );
        // test message, no answer.
        let mut fake = Fake::new();
        fake.apply = false;
        fake.look_takes = Duration::from_secs(takes);
        let mut term = Term::default().on(&fake);
        let report = cli(&["notify", "test", "--wait", "10"], &mut fake, &mut term);
        assert_eq!(report.code(), 4, "{what}: {}", report.stderr());
        assert!(
            term.elapsed() <= Duration::from_secs(11),
            "{what}: {:?}",
            term.elapsed()
        );
    }
}

/// Only strictly newer results of every target asked for count (P2-3): a
/// result cleared, one target left behind, or targets changed meanwhile do
/// not end the wait as probed.
#[test]
fn a_probe_counts_only_newer_results_of_the_targets_asked_for() {
    for (effect, said) in [
        (ProbeEffect::RecoveryOnly, "no new probe result"),
        (ProbeEffect::Clear, "no new probe result"),
        (
            ProbeEffect::Replace,
            "changed while the probe was waited for",
        ),
    ] {
        let mut fake = Fake::new();
        fake.probe_effect = effect;
        let mut term = Term::default().on(&fake);
        let (report, v) = cli_json(&["probe", "office-1", "--wait", "3"], &mut fake, &mut term);
        assert_eq!(report.code(), 4, "{}", report.stderr());
        assert_eq!(v["error"]["kind"], "unconfirmed");
        assert!(report.stderr().contains(said), "{}", report.stderr());
    }
    // Every target new: done.
    let mut fake = Fake::new();
    let report = cli(&["probe", "office-1"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
}

/// A writer that takes `room` bytes and then fails.
struct Full {
    room: usize,
}

/// Takes everything written, and fails to flush it.
#[derive(Default)]
struct NoFlush(Vec<u8>);

impl io::Write for NoFlush {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("the flush failed"))
    }
}

impl io::Write for Full {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.room == 0 {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let n = buf.len().min(self.room);
        self.room -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A result that cannot be written whole does not end as done (P2-4): not
/// done (1) after a read, "may have been carried out" (4) after a change,
/// and a failure keeps its code; standard error says so.
#[test]
fn a_result_that_cannot_be_written_does_not_end_as_done() {
    for (args, code) in [
        (vec!["export"], 1),
        (vec!["status"], 1),
        (vec!["--json", "monitor", "office-1", "off"], 4),
        (vec!["monitor", "office-1", "off"], 4),
        (vec!["--json", "monitor", "nope-9", "off"], 1),
    ] {
        let mut fake = Fake::new();
        let report = cli(&args, &mut fake, &mut Term::default());
        let mut err = Vec::new();
        let got = report.emit(&mut Full { room: 7 }, &mut err);
        let err = String::from_utf8(err).unwrap();
        assert!(!report.stdout().is_empty(), "{args:?}");
        assert_eq!(got, code, "{args:?}: {err}");
        assert!(
            err.contains("could not be written to standard output"),
            "{args:?}: {err}"
        );
    }
    // Codex review of the CLI, second round, N-P2-5: a change accepted,
    // the state afterwards unread, and then the result not written: the
    // code stays 4 and nothing claims that nothing was changed.
    let token = format!(
        "{TOKEN}
"
    );
    let mut fake = Fake::new();
    // The look for the language and the one after the token was read
    // work; the one after the change fails.
    fake.snapshots_fail_from = Some(2);
    let report = cli(
        &["--json", "notify", "token", "set"],
        &mut fake,
        &mut Term::piped(token.as_bytes()),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    let mut err = Vec::new();
    let got = report.emit(&mut Full { room: 7 }, &mut err);
    let err = String::from_utf8(err).unwrap();
    assert_eq!(got, 4);
    assert!(!err.contains("nothing was changed"), "{err}");
    assert!(err.contains("the Bot Token was handed over"), "{err}");
    assert!(!err.contains("AAEhBP0av28"), "{err}");
    // Written whole: the report's own code, standard error as it was.
    let mut fake = Fake::new();
    let report = cli(&["status"], &mut fake, &mut Term::default());
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(report.emit(&mut out, &mut err), 0);
    assert_eq!(String::from_utf8(out).unwrap(), report.stdout());
    assert!(err.is_empty());
}

/// Every part of an import counts, in a dry run too, the notification
/// switch included (P2-9).
#[test]
fn each_part_of_an_import_counts_in_a_dry_run_too() {
    let file = exported_document();
    let refused = || ImportOutcome::Refused {
        code: ErrorCode::Unauthorized,
        reason: None,
    };
    let host_refused = {
        let mut r = import_result(vec![], false);
        if let Response::SettingsImport { result } = &mut r {
            result.host = refused();
        }
        r
    };
    let notifications_refused = {
        let mut r = import_result(vec![], false);
        if let Response::SettingsImport { result } = &mut r {
            result.notifications = Some(refused());
        }
        r
    };
    let cases: Vec<(&str, Response, u8)> = vec![
        (
            "applied",
            import_result(
                vec![(ImportOutcome::Applied, AutoRecoveryImport::On)],
                false,
            ),
            0,
        ),
        (
            "skipped",
            import_result(
                vec![(ImportOutcome::NotRegistered, AutoRecoveryImport::Off)],
                false,
            ),
            0,
        ),
        (
            "baseline",
            import_result(
                vec![(
                    ImportOutcome::Registered,
                    AutoRecoveryImport::WaitingForBaseline,
                )],
                false,
            ),
            0,
        ),
        (
            "failed",
            import_result(
                vec![(ImportOutcome::DuplicateInFile, AutoRecoveryImport::Off)],
                false,
            ),
            1,
        ),
        (
            "auto-recovery refused",
            import_result(
                vec![(
                    ImportOutcome::Applied,
                    AutoRecoveryImport::Refused {
                        code: ErrorCode::Unauthorized,
                        reason: None,
                    },
                )],
                false,
            ),
            1,
        ),
        ("host refused", host_refused, 1),
        ("notifications refused", notifications_refused, 1),
    ];
    for (name, answer, code) in cases {
        for dry_run in [false, true] {
            let mut answer = answer.clone();
            if let Response::SettingsImport { result } = &mut answer {
                result.dry_run = dry_run;
            }
            let mut fake = Fake::new();
            fake.script.push((
                |r| matches!(r, Request::ImportSettings { .. }),
                Scripted::Answer(Box::new(answer)),
            ));
            let args = if dry_run {
                vec!["import", "--dry-run"]
            } else {
                vec!["import", "--yes"]
            };
            let report = cli(&args, &mut fake, &mut Term::piped(&file));
            assert_eq!(
                report.code(),
                code,
                "{name}, dry run {dry_run}: {}",
                report.stderr()
            );
            if dry_run {
                let text = format!("{}{}", report.stdout(), report.stderr());
                assert!(
                    text.contains("Dry run: nothing was changed"),
                    "{name}: {text}"
                );
            }
        }
    }
}

/// The next page of the log keeps the options that chose this one (P3-2).
#[test]
fn the_next_page_of_the_log_keeps_its_filter() {
    let Response::EventLog { entries, .. } = response(include_str!(
        "../../wgal-protocol/fixtures/response_event_log.json"
    )) else {
        panic!("not a log page");
    };
    let oldest = entries.iter().map(|e| e.seq).min().unwrap();
    let mut fake = Fake::new();
    fake.script.push((
        |r| matches!(r, Request::ReadEventLog { .. }),
        Scripted::Answer(Box::new(Response::EventLog {
            entries,
            more: true,
        })),
    ));
    let report = cli(
        &["log", "--tunnel", "office-1"],
        &mut fake,
        &mut Term::default(),
    );
    let next = format!("More: wgal log --tunnel office-1 --before {oldest}");
    assert!(report.stdout().contains(&next), "{}", report.stdout());
}

/// Each tunnel's line in status, and `show`, say how long a probe result
/// counts (P3-1): `freshness_secs` is that limit, not the age of the data
/// (wording of 2026-11-19).
#[test]
fn the_tunnel_lines_say_how_long_a_probe_result_counts() {
    let mut fake = Fake::new();
    edit(&mut fake.snapshot, &id("office-1"), |t| {
        t.freshness_secs = 7
    });
    for (args, said) in [
        (&["status"][..], "probe results count for 7 s"),
        (&["show", "office-1"][..], "probe results count for 7 s"),
        (
            &["--lang", "zh-TW", "status"][..],
            "採認最近 7 秒內的探測結果",
        ),
        (
            &["--lang", "zh-TW", "show", "office-1"][..],
            "採認最近 7 秒內的探測結果",
        ),
    ] {
        let report = cli(args, &mut fake, &mut Term::default());
        let out = report.stdout();
        assert!(out.contains(said), "{args:?}: {out}");
        assert!(
            !out.contains(" s old") && !out.contains("秒前"),
            "{args:?}: {out}"
        );
    }
}

// --- The language of a run (owner decisions of 2026-11-19) ------------------

/// `text` as an argument that is not valid UTF-8 after it.
fn not_utf8(text: &str) -> OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0xD800);
        OsString::from_wide(&wide)
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0xFF);
        OsString::from_vec(bytes)
    }
}

fn cli_os(args: Vec<OsString>, fake: &mut Fake, term: &mut Term) -> Report {
    run(&args, fake, term)
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A fake watchdog that keeps `tag` as the shared language.
fn sharing(tag: &str) -> Fake {
    let mut fake = Fake::new();
    fake.snapshot.notification.cli_language = Some(LanguageTag::new(tag).unwrap());
    fake
}

/// The language the last run on this thread wrote in.
fn written_in() -> &'static str {
    crate::text::current()
}

#[test]
fn a_run_writes_in_its_lang_then_in_the_shared_language_then_in_english() {
    // --lang first, in either form and any case, before or after the
    // command; then the shared language.
    for case in [
        vec!["--lang", "en", "status"],
        vec!["status", "--lang", "EN"],
        vec!["--lang=en", "status"],
        vec!["status", "--json", "--lang=En"],
    ] {
        let mut fake = sharing("ja");
        let report = cli(&case, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 0, "{case:?}: {}", report.stderr());
        assert_eq!(written_in(), "en", "{case:?}");
    }
    let mut fake = sharing("ja");
    assert_eq!(cli(&["status"], &mut fake, &mut Term::default()).code(), 0);
    assert_eq!(written_in(), "ja");
    let mut fake = Fake::new();
    assert_eq!(
        cli(
            &["--lang", "zh-tw", "status"],
            &mut fake,
            &mut Term::default()
        )
        .code(),
        0
    );
    assert_eq!(written_in(), "zh-TW");
    // In any case the watchdog kept it in, and English for none or a tag
    // this wgal does not have.
    for (shared, expected) in [
        ("JA", "ja"),
        ("pt-br", "pt-BR"),
        ("xx", "en"),
        ("en-GB", "en"),
    ] {
        let mut fake = sharing(shared);
        assert_eq!(cli(&["status"], &mut fake, &mut Term::default()).code(), 0);
        assert_eq!(written_in(), expected, "{shared}");
    }
    let mut fake = Fake::new();
    assert_eq!(cli(&["status"], &mut fake, &mut Term::default()).code(), 0);
    assert_eq!(written_in(), "en");
    // A watchdog that cannot be reached: English.
    let mut fake = sharing("ja");
    fake.snapshots_fail_from = Some(0);
    assert_eq!(cli(&["status"], &mut fake, &mut Term::default()).code(), 4);
    assert_eq!(written_in(), "en");
}

/// A valid `--lang` sends nothing for a mistake; any other mistake looks
/// at the watchdog once, for the shared language, and changes nothing
/// (the plan's §3.2).
#[test]
fn a_mistake_reads_the_language_first_unless_lang_names_it() {
    let with_lang: Vec<Vec<OsString>> = vec![
        os(&["--lang", "en", "status", "extra"]),
        os(&["status", "--lang=EN", "--unknown"]),
        os(&["--lang", "en", "status", "--language"]),
        os(&["--lang", "en", "status", "--lang-extra"]),
        os(&["--lang", "en", "show"]),
        vec![
            OsString::from("--lang"),
            OsString::from("en"),
            OsString::from("status"),
            not_utf8("x"),
        ],
        vec![
            OsString::from("--lang=en"),
            OsString::from("status"),
            not_utf8("--language"),
        ],
        vec![
            OsString::from("--lang=en"),
            OsString::from("status"),
            not_utf8("--lang-x"),
        ],
    ];
    for case in with_lang {
        let mut fake = sharing("ja");
        let report = cli_os(case.clone(), &mut fake, &mut Term::default());
        assert_eq!(report.code(), 2, "{case:?}: {}", report.stderr());
        assert!(fake.sent.is_empty(), "{case:?} sent {:?}", fake.sent);
        assert_eq!(written_in(), "en", "{case:?}");
    }
    let without: Vec<Vec<OsString>> = vec![
        os(&["status", "extra"]),
        os(&["--lang", "xx", "status"]),
        os(&["status", "--lang"]),
        os(&["--lang", "en", "--lang", "en", "status"]),
        os(&["--lang=en", "--lang", "en", "status"]),
        os(&["--lang=", "status"]),
        vec![
            OsString::from("--lang"),
            not_utf8("en"),
            OsString::from("status"),
        ],
        vec![not_utf8("--lang="), OsString::from("status")],
        vec![OsString::from("status"), not_utf8("x")],
        os(&["language", "ja", "--clear"]),
        os(&["language", "klingon"]),
        os(&["status", TOKEN]),
    ];
    for case in without {
        let mut fake = sharing("ja");
        let report = cli_os(case.clone(), &mut fake, &mut Term::default());
        assert_eq!(report.code(), 2, "{case:?}: {}", report.stderr());
        only_looked(&fake);
        assert_eq!(written_in(), "ja", "{case:?}");
        let all = format!("{}{}", report.stdout(), report.stderr());
        assert!(!all.contains("AAEhBP0av28"), "{all}");
    }
    // Not reached: still a mistake (2), said in English.
    let mut fake = sharing("ja");
    fake.snapshots_fail_from = Some(0);
    let report = cli(&["status", "extra"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 2, "{}", report.stderr());
    assert_eq!(written_in(), "en");
    // Help too, which still ends as done.
    let mut fake = sharing("ja");
    fake.snapshots_fail_from = Some(0);
    let report = cli(&["help"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0);
    assert!(report.stdout().contains("Usage: wgal"));
    assert_eq!(written_in(), "en");
    let mut fake = sharing("ja");
    assert_eq!(
        cli(&["--lang", "en", "help"], &mut fake, &mut Term::default()).code(),
        0
    );
    assert!(fake.sent.is_empty(), "{:?}", fake.sent);
}

/// After asking something here, the state is looked at again, and the
/// command goes by that second look (the plan's §3.2).
#[test]
fn the_state_is_looked_at_again_after_asking_here() {
    let token = BotToken::new(TOKEN).unwrap();
    let mut fake = Fake::new();
    let report = cli(
        &["notify", "token", "set"],
        &mut fake,
        &mut Term::piped(format!("{TOKEN}\n").as_bytes()),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(
        fake.sent,
        vec![
            Request::GetSnapshot,
            Request::GetSnapshot,
            Request::SetBotToken {
                token: token.clone()
            },
            Request::GetSnapshot,
        ]
    );
    // With --lang there is no look for the language: one look, after the
    // token was read, as before.
    let mut fake = Fake::new();
    let report = cli(
        &["--lang", "en", "notify", "token", "set"],
        &mut fake,
        &mut Term::piped(format!("{TOKEN}\n").as_bytes()),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(fake.sent.len(), 3, "{:?}", fake.sent);
    // The second look fails: nothing is sent, and the token is not written.
    let mut fake = Fake::new();
    fake.snapshots_fail_from = Some(1);
    let report = cli(
        &["notify", "token", "set"],
        &mut fake,
        &mut Term::piped(format!("{TOKEN}\n").as_bytes()),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    assert!(fake.changes().is_empty(), "{:?}", fake.sent);
    let all = format!("{}{}", report.stdout(), report.stderr());
    assert!(!all.contains("AAEhBP0av28"), "{all}");
    // The second look is the one that counts: a watchdog too old by then
    // gets nothing.
    let mut fake = Fake::new();
    fake.second_look = Some(|s: &mut Snapshot| {
        s.host.watchdog_version = ProductVersion::new("2.0.0").unwrap();
    });
    let report = cli(
        &["notify", "token", "set"],
        &mut fake,
        &mut Term::piped(format!("{TOKEN}\n").as_bytes()),
    );
    assert_eq!(report.code(), 1, "{}", report.stderr());
    assert!(fake.changes().is_empty(), "{:?}", fake.sent);
    // A typed yes and a settings file look again too.
    let mut term = Term::terminal();
    term.answers.push_back(Line::Read(b"yes".to_vec()));
    let mut fake = Fake::new();
    assert_eq!(cli(&["mcp", "on"], &mut fake, &mut term).code(), 0);
    assert_eq!(fake.sent[..2], [Request::GetSnapshot, Request::GetSnapshot]);
}

fn ja() -> &'static languages::Language {
    languages::find("ja").unwrap()
}

fn set_ja() -> Request {
    Request::SetCliLanguage {
        language: Some(LanguageTag::new("ja").unwrap()),
    }
}

fn templates_ja() -> Request {
    Request::SetNotificationTemplates {
        templates: ja().templates.clone(),
    }
}

#[test]
fn wgal_language_shows_both_languages_and_the_thirteen() {
    let mut fake = Fake::new();
    let (report, v) = cli_json(&["language"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert!(fake.changes().is_empty());
    assert_eq!(v["result"]["cli_language"], Value::Null);
    assert_eq!(v["result"]["writes_in"], "en");
    assert_eq!(v["result"]["languages"].as_array().unwrap().len(), 13);
    let mut fake = Fake::new();
    let report = cli(
        &["--lang", "en", "language"],
        &mut fake,
        &mut Term::default(),
    );
    assert!(
        report.stdout().contains("English (en), the default"),
        "{}",
        report.stdout()
    );
    assert!(report.stdout().contains("日本語"));
    let mut fake = sharing("xx");
    let report = cli(
        &["--lang", "en", "language"],
        &mut fake,
        &mut Term::default(),
    );
    assert!(
        report
            .stdout()
            .contains("\"xx\", which this wgal does not have"),
        "{}",
        report.stdout()
    );
    let mut fake = sharing("ja");
    let (_, v) = cli_json(
        &["--lang", "en", "language"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(v["result"]["cli_language"], "ja");
    assert_eq!(v["result"]["writes_in"], "ja");
}

/// `wgal language <tag>`: the shared language, then the messages' set,
/// read back; what follows the accepted language is written in it, unless
/// --lang named another.
#[test]
fn wgal_language_sets_both_and_writes_on_in_it() {
    let mut fake = Fake::new();
    let (report, v) = cli_json(&["language", "JA"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(
        fake.sent,
        vec![
            Request::GetSnapshot,
            set_ja(),
            templates_ja(),
            Request::GetSnapshot
        ]
    );
    assert_eq!(written_in(), "ja");
    assert_eq!(v["result"]["accepted"], true);
    assert_eq!(
        v["result"]["changed"],
        serde_json::json!(["set_cli_language", "set_notification_templates"])
    );
    assert_eq!(v["result"]["cli_language"], "ja");
    assert_eq!(v["result"]["templates_id"], ja().templates.id().as_str());
    // --lang keeps the run's language.
    let mut fake = Fake::new();
    let report = cli(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(written_in(), "en");
    assert!(
        report.stdout().contains("日本語 (ja)"),
        "{}",
        report.stdout()
    );
    // Only what is not that already: the same spelling is skipped, another
    // case is sent in the table's spelling.
    let mut fake = sharing("ja");
    let report = cli(&["language", "ja"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(fake.changes(), vec![&templates_ja()]);
    let mut fake = sharing("JA");
    fake.snapshot.notification.templates_id = Some(ja().templates.id().clone());
    assert_eq!(
        cli(&["language", "ja"], &mut fake, &mut Term::default()).code(),
        0
    );
    assert_eq!(fake.changes(), vec![&set_ja()]);
}

/// Both already that language: nothing is sent, it says so, and the run
/// only read (a result that cannot be written then ends as 1).
#[test]
fn wgal_language_that_is_already_set_changes_nothing() {
    let mut fake = sharing("ja");
    fake.snapshot.notification.templates_id = Some(ja().templates.id().clone());
    let (report, v) = cli_json(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(fake.sent, vec![Request::GetSnapshot]);
    assert_eq!(v["result"]["changed"], serde_json::json!([]));
    assert!(v["result"].get("accepted").is_none(), "{v}");
    let mut err = Vec::new();
    assert_eq!(report.emit(&mut Full { room: 3 }, &mut err), 1);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("nothing was changed"), "{err}");
    // A change that was made ends as 4 when its result cannot be written.
    let mut fake = Fake::new();
    let report = cli(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    let mut err = Vec::new();
    assert_eq!(report.emit(&mut Full { room: 3 }, &mut err), 4);
    // Also when only flushing fails.
    let mut fake = Fake::new();
    let report = cli(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    let mut err = Vec::new();
    assert_eq!(report.emit(&mut NoFlush::default(), &mut err), 4);
    let mut fake = sharing("ja");
    fake.snapshot.notification.templates_id = Some(ja().templates.id().clone());
    let report = cli(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    let mut err = Vec::new();
    assert_eq!(report.emit(&mut NoFlush::default(), &mut err), 1);
}

/// Which step fails, how, and what the run then ends with: its exit, its
/// JSON kind, and whether `done` names the shared language.
type StepCase = (fn(&Request) -> bool, Scripted, u8, &'static str, bool);

/// What each step's failure makes of the run (the plan's §4 table), and
/// that nothing is sent twice.
#[test]
fn wgal_language_says_what_was_done_when_a_step_fails() {
    let refused = || {
        Scripted::Refuse(ProtocolError::new(
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed),
            "could not be stored",
        ))
    };
    let is_set: fn(&Request) -> bool = |r| matches!(r, Request::SetCliLanguage { .. });
    let is_templates: fn(&Request) -> bool =
        |r| matches!(r, Request::SetNotificationTemplates { .. });
    // (which step, what happens, exit, kind, done has the language)
    let cases: Vec<StepCase> = vec![
        (is_set, refused(), 1, "refused", false),
        (
            is_set,
            Scripted::Lose(Delivery::NotSent),
            3,
            "unreachable",
            false,
        ),
        (
            is_set,
            Scripted::Lose(Delivery::Unknown),
            4,
            "unreachable",
            false,
        ),
        (is_templates, refused(), 1, "refused", true),
        (
            is_templates,
            Scripted::Lose(Delivery::NotSent),
            1,
            "failed",
            true,
        ),
        (
            is_templates,
            Scripted::Lose(Delivery::Unknown),
            4,
            "unreachable",
            true,
        ),
    ];
    for (step, what, exit, kind, done) in cases {
        let mut fake = Fake::new().on(step, what);
        let (report, v) = cli_json(
            &["--lang", "en", "language", "ja"],
            &mut fake,
            &mut Term::default(),
        );
        assert_eq!(report.code(), exit, "{kind}: {}", report.stderr());
        assert_eq!(v["error"]["kind"], kind, "{v}");
        assert_eq!(v["error"]["exit"], exit);
        let done_list = v["error"]["done"].as_array().unwrap();
        assert_eq!(!done_list.is_empty(), done, "{v}");
        if done {
            assert!(done_list[0].as_str().unwrap().contains("日本語"), "{v}");
            assert!(
                report.stderr().contains("before this"),
                "{}",
                report.stderr()
            );
        }
        let sent_sets = fake.sent.iter().filter(|r| is_set(r)).count();
        let sent_templates = fake.sent.iter().filter(|r| is_templates(r)).count();
        assert_eq!(sent_sets, 1, "never sent twice");
        assert!(sent_templates <= 1, "never sent twice");
        if exit == 1 && !done {
            assert!(report.stderr().contains("refused"), "{}", report.stderr());
        }
    }
    // Only the set was needed, and it was not sent: nothing changed (3).
    let mut fake = sharing("ja").on(is_templates, Scripted::Lose(Delivery::NotSent));
    let (report, v) = cli_json(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 3, "{}", report.stderr());
    assert_eq!(v["error"]["done"], serde_json::json!([]));
    // Both accepted, the state afterwards unread: not confirmed (4).
    let mut fake = Fake::new();
    fake.snapshots_fail_from = Some(1);
    let (report, v) = cli_json(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "unconfirmed");
    assert_eq!(v["error"]["done"].as_array().unwrap().len(), 2);
    // Accepted but not shown afterwards: not confirmed (4).
    let mut fake = Fake::new();
    fake.apply = false;
    let (report, v) = cli_json(
        &["--lang", "en", "language", "ja"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "unconfirmed");
    // Without --lang, the failure of the second step is said in the new
    // language, the failure of the first in the old one.
    let mut fake = sharing("de").on(is_templates, refused());
    cli(&["language", "ja"], &mut fake, &mut Term::default());
    assert_eq!(written_in(), "ja");
    let mut fake = sharing("de").on(is_set, refused());
    cli(&["language", "ja"], &mut fake, &mut Term::default());
    assert_eq!(written_in(), "de");
}

#[test]
fn wgal_language_needs_watchdog_4_and_clear_is_always_sent() {
    for args in [vec!["language", "ja"], vec!["language", "--clear"]] {
        let mut fake = Fake::new();
        fake.snapshot.host.watchdog_version = ProductVersion::new("3.0.0").unwrap();
        let (report, v) = cli_json(&args, &mut fake, &mut Term::default());
        assert_eq!(report.code(), 1, "{args:?}: {}", report.stderr());
        assert_eq!(v["error"]["kind"], "failed");
        assert_eq!(fake.sent, vec![Request::GetSnapshot], "{args:?}");
        assert!(
            report.stderr().contains("wgal notify language"),
            "{}",
            report.stderr()
        );
    }
    // Cleared even when the state shows none: the watchdog writes its
    // settings again without the key.
    let mut fake = Fake::new();
    let (report, v) = cli_json(&["language", "--clear"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(
        fake.sent,
        vec![
            Request::GetSnapshot,
            Request::SetCliLanguage { language: None },
            Request::GetSnapshot
        ]
    );
    assert_eq!(
        v["result"]["changed"],
        serde_json::json!(["set_cli_language"])
    );
    let mut fake = sharing("ja");
    let report = cli(&["language", "--clear"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 0, "{}", report.stderr());
    assert_eq!(written_in(), "en");
    assert_eq!(fake.snapshot.notification.cli_language, None);
    // Its only step not sent: nothing changed (3), nothing done.
    let mut fake = sharing("ja").on(
        |r| matches!(r, Request::SetCliLanguage { .. }),
        Scripted::Lose(Delivery::NotSent),
    );
    let (report, v) = cli_json(&["language", "--clear"], &mut fake, &mut Term::default());
    assert_eq!(report.code(), 3, "{}", report.stderr());
    assert_eq!(v["error"]["done"], serde_json::json!([]));
}

/// Every language writes every kind of output whole: no value left
/// unfilled, nothing shaped like a Bot Token, valid JSON, and help in its
/// own words.
#[test]
fn every_language_writes_whole_sentences_and_hides_tokens() {
    let mut shaped = Fake::new();
    shaped.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted(TOKEN);
    edit(&mut shaped.snapshot, &id("office-1"), |t| {
        t.display_name = DisplayName::new(format!("x{TOKEN}")).unwrap();
    });
    let english_help = cli(
        &["--lang", "en", "help"],
        &mut Fake::new(),
        &mut Term::default(),
    )
    .stdout()
    .to_owned();
    for (tag, _) in crate::text::FILES {
        for case in [
            vec!["help"],
            vec!["help", "notify"],
            vec!["help", "language"],
            vec!["help", "set"],
            vec!["status"],
            vec!["show", "office-1"],
            vec!["notify"],
            vec!["notify", "language"],
            vec!["language"],
            vec!["log"],
            vec!["status", "extra"],
            vec!["show", "nowhere"],
            vec!["language", "klingon"],
            vec!["--json", "status", "extra"],
            vec!["--json", "language"],
        ] {
            let mut fake = Fake::new();
            fake.snapshot = shaped.snapshot.clone();
            let mut args = vec!["--lang", *tag];
            args.extend(case.iter().copied());
            let report = cli(&args, &mut fake, &mut Term::default());
            let all = format!("{}{}", report.stdout(), report.stderr());
            assert!(!all.trim().is_empty(), "{tag} {case:?}");
            assert!(!all.contains("AAEhBP0av28"), "{tag} {case:?}: {all}");
            if case.contains(&"--json") {
                let _: Value = serde_json::from_str(report.stdout())
                    .unwrap_or_else(|e| panic!("{tag} {case:?}: {e}: {}", report.stdout()));
            } else {
                assert!(
                    crate::text::names(&all).is_empty(),
                    "{tag} {case:?} left a value unfilled: {all}"
                );
            }
        }
        let help = cli(
            &["--lang", tag, "help"],
            &mut Fake::new(),
            &mut Term::default(),
        );
        assert_eq!(help.stdout() == english_help, *tag == "en", "{tag}");
    }
}

/// After asking here, a second look that fails or finds a watchdog too
/// old sends nothing, for the typed yes and the settings file too (Codex
/// review of the implementation, first round, B1).
#[test]
fn every_question_here_is_followed_by_a_second_look_that_counts() {
    let too_old: fn(&mut Snapshot) = |s| {
        s.host.watchdog_version = ProductVersion::new("2.0.0").unwrap();
    };
    let file = cli(&["export"], &mut Fake::new(), &mut Term::default())
        .stdout()
        .to_owned();
    for (failing, expected) in [(true, 4), (false, 1)] {
        let mut term = Term::terminal();
        term.answers.push_back(Line::Read(b"yes".to_vec()));
        let mut fake = Fake::new();
        if failing {
            fake.snapshots_fail_from = Some(1);
        } else {
            fake.second_look = Some(too_old);
        }
        let report = cli(&["mcp", "on"], &mut fake, &mut term);
        assert_eq!(
            report.code(),
            expected,
            "mcp, failing {failing}: {}",
            report.stderr()
        );
        assert!(fake.changes().is_empty(), "{:?}", fake.sent);

        let mut fake = Fake::new();
        if failing {
            fake.snapshots_fail_from = Some(1);
        } else {
            fake.second_look = Some(too_old);
        }
        let report = cli(
            &["import", "--yes"],
            &mut fake,
            &mut Term::piped(file.as_bytes()),
        );
        assert_eq!(
            report.code(),
            expected,
            "import, failing {failing}: {}",
            report.stderr()
        );
        assert!(fake.changes().is_empty(), "{:?}", fake.sent);
    }
}

/// `--clear` accepted, then the state unread or not showing it: not
/// confirmed (4), with only the clearing as done.
#[test]
fn a_clear_that_is_not_confirmed_names_only_itself() {
    let mut fake = sharing("ja");
    fake.snapshots_fail_from = Some(1);
    let (report, v) = cli_json(
        &["--lang", "en", "language", "--clear"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "unconfirmed");
    assert_eq!(v["error"]["done"].as_array().unwrap().len(), 1, "{v}");
    let mut fake = sharing("ja");
    fake.apply = false;
    let (report, v) = cli_json(
        &["--lang", "en", "language", "--clear"],
        &mut fake,
        &mut Term::default(),
    );
    assert_eq!(report.code(), 4, "{}", report.stderr());
    assert_eq!(v["error"]["kind"], "unconfirmed");
    assert_eq!(v["error"]["done"].as_array().unwrap().len(), 1, "{v}");
}

/// In every language: the watchdog that cannot be reached is said in that
/// language with the client's English words after it, in text and JSON;
/// the token prompt and the typed-yes prompt are whole; and JSON holding
/// data shaped like a token hides it (Codex review of the implementation,
/// first round, R1 and B1).
#[test]
fn every_language_says_unreachable_prompts_and_json_whole() {
    let mut shaped = Fake::new();
    shaped.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted(TOKEN);
    edit(&mut shaped.snapshot, &id("office-1"), |t| {
        t.display_name = DisplayName::new(format!("x{TOKEN}")).unwrap();
    });
    for (tag, _) in crate::text::FILES {
        crate::text::set(tag);
        let lost = crate::text::render(
            "failure.unreachable_unknown",
            &[("detail", &"the answer was lost")],
        );
        crate::text::english();
        let mut fake = Fake::new();
        fake.snapshots_fail_from = Some(0);
        let (report, v) = cli_json(&["--lang", tag, "status"], &mut fake, &mut Term::default());
        assert_eq!(report.code(), 4, "{tag}");
        assert!(
            report.stderr().contains(&lost),
            "{tag}: {}",
            report.stderr()
        );
        assert_eq!(v["error"]["message"], lost.as_str(), "{tag}");

        let mut term = Term::terminal();
        term.hidden
            .push_back(Ok(Line::Read(TOKEN.as_bytes().to_vec())));
        let report = cli(
            &["--lang", tag, "notify", "token", "set"],
            &mut Fake::new(),
            &mut term,
        );
        assert_eq!(report.code(), 0, "{tag}: {}", report.stderr());
        assert!(!term.prompts.is_empty(), "{tag}");
        for prompt in &term.prompts {
            assert!(crate::text::names(prompt).is_empty(), "{tag}: {prompt}");
            assert!(!prompt.contains("AAEhBP0av28"), "{tag}: {prompt}");
        }
        let all = format!("{}{}", report.stdout(), report.stderr());
        assert!(!all.contains("AAEhBP0av28"), "{tag}: {all}");
        let mut term = Term::terminal();
        term.answers.push_back(Line::Read(b"no".to_vec()));
        let report = cli(&["--lang", tag, "mcp", "on"], &mut Fake::new(), &mut term);
        assert_eq!(report.code(), 2, "{tag}");
        for prompt in &term.prompts {
            assert!(crate::text::names(prompt).is_empty(), "{tag}: {prompt}");
            assert!(prompt.contains("yes"), "{tag}: {prompt}");
        }

        for case in [
            vec!["status"],
            vec!["show", "office-1"],
            vec!["notify"],
            vec!["list"],
        ] {
            let mut fake = Fake::new();
            fake.snapshot = shaped.snapshot.clone();
            let mut args = vec!["--lang", *tag];
            args.extend(case.iter().copied());
            let (report, _) = cli_json(&args, &mut fake, &mut Term::default());
            assert!(!report.stdout().contains("AAEhBP0av28"), "{tag} {case:?}");
        }
    }
}

/// With no device name, `wgal notify` names the host name messages give
/// (watchdog 4.1.0), in every language, and the JSON carries it; with a
/// device name, or no host name, it says what it said before.
#[test]
fn notify_names_the_host_name_messages_give_when_no_device_name_is_set() {
    for (tag, _) in crate::text::FILES {
        let mut fake = Fake::new();
        fake.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted("   ");
        fake.snapshot.notification.host_name =
            Some(wgal_protocol::DeviceName::new("office-pc").unwrap());
        let report = cli(&["--lang", tag, "notify"], &mut fake, &mut Term::default());
        assert_eq!(report.code(), 0, "{tag}");
        assert!(
            report.stdout().contains("\"office-pc\""),
            "{tag}: {}",
            report.stdout()
        );
        assert!(crate::text::names(report.stdout()).is_empty(), "{tag}");
    }
    let mut fake = Fake::new();
    fake.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted("");
    fake.snapshot.notification.host_name =
        Some(wgal_protocol::DeviceName::new("office-pc").unwrap());
    let (_, v) = cli_json(&["--lang", "en", "notify"], &mut fake, &mut Term::default());
    assert_eq!(v["result"]["notification"]["host_name"], "office-pc", "{v}");
    let report = cli(&["--lang", "en", "notify"], &mut fake, &mut Term::default());
    assert!(
        report
            .stdout()
            .contains("not set; messages use the host name \"office-pc\"")
    );
    // A device name set: shown, whatever the host name.
    let mut fake = Fake::new();
    fake.snapshot.notification.host_name =
        Some(wgal_protocol::DeviceName::new("office-pc").unwrap());
    let report = cli(&["--lang", "en", "notify"], &mut fake, &mut Term::default());
    assert!(
        !report.stdout().contains("office-pc"),
        "{}",
        report.stdout()
    );
    // No host name (an older watchdog): as before.
    let mut fake = Fake::new();
    fake.snapshot.notification.device_name = wgal_protocol::DeviceName::fitted("");
    fake.snapshot.notification.host_name = None;
    let report = cli(&["--lang", "en", "notify"], &mut fake, &mut Term::default());
    assert!(
        report.stdout().contains("Device name: (none"),
        "{}",
        report.stdout()
    );
}
