//! The command line, parsed against a fixed table of commands: every value
//! is checked with the contract's own types before anything is sent, and
//! nothing here is a file path, a command or a service name (AGENTS.md
//! rule 8). A mistake is said in words that tell what is accepted.

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::num::NonZeroU16;

use wgal_client::token::looks_like_token;
use wgal_protocol::{
    ChatId, EventFilter, ImportMatch, ImportOptions, MAX_OBSERVE_TARGETS, ProbeKind, ProbeTarget,
    TargetAddress, TimeZoneName, TunnelId, WorkerCount,
};

use crate::text::{self, tr};

/// The longest wait for an operation to settle, as `wgal-mcp`'s.
pub const MAX_WAIT_SECS: u64 = 120;
/// The wait when none is given.
pub const DEFAULT_WAIT_SECS: u64 = 30;

/// What to run, whether to answer in JSON, and the language `--lang`
/// names, as the language files spell it.
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub json: bool,
    pub lang: Option<&'static str>,
    pub command: Command,
}

/// What `wgal language` is asked to do (owner decisions of 2026-11-19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageArg {
    /// Show the shared language, the messages' and the 13.
    Show,
    /// Make one of the 13, as the language files spell it, the shared
    /// language, and the messages' too.
    Set(&'static str),
    /// Clear the shared language: back to English, as never set.
    Clear,
}

/// One of the operations a tunnel can be asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Connect,
    Disconnect,
    Reconnect,
}

/// Every command.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help(Option<&'static str>),
    Version,
    Status,
    List,
    Show(TunnelId),
    Discover,
    Register(TunnelId),
    Unregister(TunnelId),
    Monitor(TunnelId, bool),
    Set(SetArgs),
    AutoRecovery(TunnelId, bool),
    StartWhenStopped(TunnelId, bool),
    Retry {
        tunnel: TunnelId,
        /// `Some(None)`: keep trying.
        limit: Option<Option<u16>>,
        interval: Option<u16>,
    },
    Operate {
        operation: Operation,
        tunnel: TunnelId,
        wait_secs: u64,
    },
    Probe {
        tunnel: TunnelId,
        wait_secs: u64,
    },
    Pause(bool),
    Notify(Notify),
    Host {
        /// `Some(None)`: the suggested count.
        workers: Option<Option<WorkerCount>>,
        /// `Some(None)`: the system's zone.
        time_zone: Option<Option<TimeZoneName>>,
    },
    Mcp(bool),
    McpTelegram(bool),
    Log {
        filter: EventFilter,
        before: Option<u64>,
    },
    Export,
    Import(ImportOptions),
    Language(LanguageArg),
}

/// The probe settings `set` changes; what is not given stays.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetArgs {
    pub tunnel: Option<TunnelId>,
    pub interval: Option<u16>,
    pub timeout: Option<u8>,
    pub retries: Option<u8>,
    pub min_fault: Option<u16>,
    /// `Some(None)`: no recovery target.
    pub recovery: Option<Option<ProbeTarget>>,
    /// `Some(vec![])`: no observe targets.
    pub observe: Option<Vec<ProbeTarget>>,
}

/// The notification commands.
#[derive(Debug, PartialEq, Eq)]
pub enum Notify {
    Show,
    Enabled(bool),
    /// The name as given; the device name's rules are applied when it is
    /// sent, as the window applies them.
    Device(String),
    /// `None`: clear it.
    Chat(Option<ChatId>),
    Tunnel(TunnelId, bool),
    TokenSet,
    TokenClear,
    Test {
        wait_secs: u64,
    },
    /// `None`: list the languages.
    Language(Option<String>),
}

/// A mistake on the command line: nothing was changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Usage {
    pub message: String,
    /// The help topic that shows the right use.
    pub topic: Option<&'static str>,
}

fn usage(topic: &'static str, message: impl Into<String>) -> Usage {
    Usage {
        message: message.into(),
        topic: Some(topic),
    }
}

/// What a mistake may repeat of the argument: never text shaped like a
/// Bot Token (the command line is refused before that anyway).
fn shown(text: &str) -> String {
    let mut shown: String = text.chars().take(64).collect();
    if shown.len() < text.len() {
        shown.push_str("...");
    }
    format!("{shown:?}")
}

/// The arguments still to read, with the command's name for its messages.
struct Rest {
    topic: &'static str,
    words: Vec<String>,
}

impl Rest {
    /// Takes `--name`, with no value; given twice is a mistake.
    fn flag(&mut self, name: &str) -> Result<bool, Usage> {
        let count = self.words.iter().filter(|w| *w == name).count();
        if count > 1 {
            return Err(usage(self.topic, tr!("usage.given_twice", option = name)));
        }
        self.words.retain(|w| w != name);
        Ok(count == 1)
    }

    /// Every value of `--name VALUE` or `--name=VALUE`, in order.
    fn values(&mut self, name: &str) -> Result<Vec<String>, Usage> {
        let mut found = Vec::new();
        let mut kept = Vec::new();
        let mut words = std::mem::take(&mut self.words).into_iter();
        while let Some(word) = words.next() {
            if word == name {
                match words.next() {
                    Some(value) if !value.starts_with("--") => found.push(value),
                    _ => return Err(usage(self.topic, tr!("usage.needs_value", option = name))),
                }
            } else if let Some(value) = word
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
            {
                found.push(value.to_owned());
            } else {
                kept.push(word);
            }
        }
        self.words = kept;
        Ok(found)
    }

    /// The value of `--name VALUE`, given at most once.
    fn value(&mut self, name: &str) -> Result<Option<String>, Usage> {
        let mut values = self.values(name)?;
        if values.len() > 1 {
            return Err(usage(self.topic, tr!("usage.given_twice", option = name)));
        }
        Ok(values.pop())
    }

    /// The words left, which must all be plain (no unknown option).
    fn positional(self, most: usize) -> Result<Vec<String>, Usage> {
        if let Some(unknown) = self.words.iter().find(|w| w.starts_with("--")) {
            return Err(usage(
                self.topic,
                tr!(
                    "usage.not_an_option",
                    argument = shown(unknown),
                    command = self.topic
                ),
            ));
        }
        if self.words.len() > most {
            let extra: Vec<String> = self.words[most..].iter().map(|w| shown(w)).collect();
            return Err(usage(
                self.topic,
                tr!("usage.too_many_arguments", arguments = extra.join(" ")),
            ));
        }
        Ok(self.words)
    }
}

fn tunnel(topic: &'static str, word: Option<&String>) -> Result<TunnelId, Usage> {
    let word = word.ok_or_else(|| usage(topic, tr!("usage.give_tunnel")))?;
    TunnelId::new(word.as_str())
        .map_err(|_| usage(topic, tr!("usage.not_a_tunnel_id", argument = shown(word))))
}

fn on_off(topic: &'static str, word: Option<&String>) -> Result<bool, Usage> {
    match word.map(String::as_str) {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(other) => Err(usage(
            topic,
            tr!("usage.on_or_off_not", argument = shown(other)),
        )),
        None => Err(usage(topic, tr!("usage.on_or_off"))),
    }
}

fn number<T: TryFrom<u64>>(
    topic: &'static str,
    name: &str,
    text: &str,
    range: std::ops::RangeInclusive<u64>,
) -> Result<T, Usage> {
    let wrong = || {
        usage(
            topic,
            tr!(
                "usage.not_a_number",
                option = name,
                min = range.start(),
                max = range.end(),
                argument = shown(text)
            ),
        )
    };
    let n: u64 = text.parse().map_err(|_| wrong())?;
    if !range.contains(&n) {
        return Err(wrong());
    }
    T::try_from(n).map_err(|_| wrong())
}

/// A probe target: `192.0.2.1` (ICMP) or `192.0.2.1:443` (a TCP connect
/// to that port).
pub fn target(topic: &'static str, name: &str, text: &str) -> Result<ProbeTarget, Usage> {
    let argument = shown(text);
    let not_ipv4 = || {
        usage(
            topic,
            tr!("usage.target_not_ipv4", option = name, argument = argument),
        )
    };
    let bad_port = || {
        usage(
            topic,
            tr!("usage.target_bad_port", option = name, argument = argument),
        )
    };
    let (address, port) = match text.split_once(':') {
        Some((address, port)) => (address, Some(port)),
        None => (text, None),
    };
    let ip: Ipv4Addr = address.parse().map_err(|_| not_ipv4())?;
    let address = TargetAddress::new(ip).map_err(|e| {
        usage(
            topic,
            tr!(
                "usage.target_refused",
                option = name,
                argument = argument,
                reason = e
            ),
        )
    })?;
    let probe = match port {
        None => ProbeKind::Icmp,
        Some(port) => {
            let port: u16 = port.parse().map_err(|_| bad_port())?;
            ProbeKind::TcpConnect {
                port: NonZeroU16::new(port).ok_or_else(bad_port)?,
            }
        }
    };
    Ok(ProbeTarget { address, probe })
}

fn wait(rest: &mut Rest) -> Result<u64, Usage> {
    let no_wait = rest.flag("--no-wait")?;
    let given = rest.value("--wait")?;
    match (no_wait, given) {
        (true, Some(_)) => Err(usage(rest.topic, tr!("usage.wait_or_no_wait"))),
        (true, None) => Ok(0),
        (false, Some(secs)) => number(rest.topic, "--wait", &secs, 1..=MAX_WAIT_SECS),
        (false, None) => Ok(DEFAULT_WAIT_SECS),
    }
}

/// `--yes`, which the commands that cannot be undone need; `refusal` says
/// what the command does and that it needs `--yes`.
fn confirmed(rest: &mut Rest, refusal: impl FnOnce() -> String) -> Result<(), Usage> {
    if rest.flag("--yes")? {
        Ok(())
    } else {
        Err(usage(rest.topic, refusal()))
    }
}

/// The help topics, by command.
pub const TOPICS: &[&str] = &[
    "status",
    "list",
    "show",
    "discover",
    "register",
    "unregister",
    "monitor",
    "set",
    "auto-recovery",
    "start-when-stopped",
    "retry",
    "connect",
    "disconnect",
    "reconnect",
    "probe",
    "pause",
    "resume",
    "notify",
    "language",
    "host",
    "mcp",
    "log",
    "export",
    "import",
];

fn topic_of(word: &str) -> Option<&'static str> {
    TOPICS.iter().copied().find(|t| *t == word)
}

/// Parses the arguments after the program's name.
pub fn parse(arguments: &[OsString]) -> Result<Invocation, Usage> {
    let mut words = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let Some(word) = argument.to_str() else {
            return Err(Usage {
                message: tr!("usage.not_utf8"),
                topic: None,
            });
        };
        words.push(word.to_owned());
    }
    // Before anything else: a token on the command line is never used
    // (owner decision of 2026-11-18).
    if words.iter().any(|w| looks_like_token(w)) {
        return Err(Usage {
            message: tr!("usage.token_on_command_line"),
            topic: None,
        });
    }
    let lang = lang_option(&mut words)?;
    let json = words.iter().any(|w| w == "--json");
    if words.iter().filter(|w| *w == "--json").count() > 1 {
        return Err(Usage {
            message: tr!("usage.given_twice", option = "--json"),
            topic: None,
        });
    }
    words.retain(|w| w != "--json");
    let command = command(words)?;
    Ok(Invocation {
        json,
        lang,
        command,
    })
}

/// Takes `--lang TAG` or `--lang=TAG` out of `words`: at most once, its
/// value one of the 13 languages in any case.
fn lang_option(words: &mut Vec<String>) -> Result<Option<&'static str>, Usage> {
    let mut found = Vec::new();
    let mut kept = Vec::new();
    let mut all = std::mem::take(words).into_iter();
    while let Some(word) = all.next() {
        if word == "--lang" {
            found.push(all.next());
        } else if let Some(value) = word.strip_prefix("--lang=") {
            found.push(Some(value.to_owned()));
        } else {
            kept.push(word);
        }
    }
    *words = kept;
    let value = match found.as_slice() {
        [] => return Ok(None),
        [value] => value.clone(),
        _ => {
            return Err(Usage {
                message: tr!("usage.lang_twice", tags = language_tags()),
                topic: None,
            });
        }
    };
    let Some(value) = value else {
        return Err(Usage {
            message: tr!("usage.lang_needs_value", tags = language_tags()),
            topic: None,
        });
    };
    text::known(&value).map(Some).ok_or_else(|| Usage {
        message: tr!(
            "usage.lang_unknown",
            tag = shown(&value),
            tags = language_tags()
        ),
        topic: None,
    })
}

/// The 13 languages' tags, for a mistake.
fn language_tags() -> String {
    text::FILES
        .iter()
        .map(|(tag, _)| *tag)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The language a run names with `--lang`, read from the raw arguments the
/// way [`parse`] reads them, but whatever else they hold (the plan's §3.2):
/// `Some` only for exactly one `--lang` whose value is one of the 13. An
/// argument is a `--lang` when it is `--lang` or begins with `--lang=`, as
/// far as its text can be read; the value is the next argument, or what
/// follows `=`, and must be valid UTF-8.
pub fn chosen_language(arguments: &[OsString]) -> Option<&'static str> {
    let mut values: Vec<Option<String>> = Vec::new();
    let mut all = arguments.iter();
    while let Some(argument) = all.next() {
        let lossy = argument.to_string_lossy();
        if lossy == "--lang" {
            values.push(all.next().and_then(|v| v.to_str()).map(str::to_owned));
        } else if lossy.starts_with("--lang=") {
            values.push(
                argument
                    .to_str()
                    .and_then(|a| a.strip_prefix("--lang="))
                    .map(str::to_owned),
            );
        }
    }
    match values.as_slice() {
        [Some(value)] => text::known(value),
        _ => None,
    }
}

fn language(mut rest: Rest) -> Result<Command, Usage> {
    let clear = rest.flag("--clear")?;
    let words = rest.positional(1)?;
    match (words.first(), clear) {
        (None, false) => Ok(Command::Language(LanguageArg::Show)),
        (None, true) => Ok(Command::Language(LanguageArg::Clear)),
        (Some(_), true) => Err(usage("language", tr!("usage.language_or_clear"))),
        (Some(tag), false) => text::known(tag)
            .map(|tag| Command::Language(LanguageArg::Set(tag)))
            .ok_or_else(|| {
                usage(
                    "language",
                    tr!(
                        "usage.lang_unknown",
                        tag = shown(tag),
                        tags = language_tags()
                    ),
                )
            }),
    }
}

fn command(mut words: Vec<String>) -> Result<Command, Usage> {
    if words.is_empty() {
        return Ok(Command::Help(None));
    }
    let name = words.remove(0);
    match name.as_str() {
        "--version" | "-V" | "version" if words.is_empty() => return Ok(Command::Version),
        "--help" | "-h" | "help" => {
            return match words.as_slice() {
                [] => Ok(Command::Help(None)),
                [topic] => topic_of(topic)
                    .map(|t| Command::Help(Some(t)))
                    .ok_or_else(|| Usage {
                        message: tr!("usage.no_such_command", argument = shown(topic)),
                        topic: None,
                    }),
                _ => Err(Usage {
                    message: tr!("usage.help_takes_one"),
                    topic: None,
                }),
            };
        }
        _ => {}
    }
    let Some(topic) = topic_of(&name) else {
        return Err(Usage {
            message: tr!("usage.no_such_command", argument = shown(&name)),
            topic: None,
        });
    };
    if words.iter().any(|w| w == "--help" || w == "-h") {
        return Ok(Command::Help(Some(topic)));
    }
    let mut rest = Rest { topic, words };
    match topic {
        "status" => rest.positional(0).map(|_| Command::Status),
        "list" => rest.positional(0).map(|_| Command::List),
        "discover" => rest.positional(0).map(|_| Command::Discover),
        "show" => Ok(Command::Show(tunnel(topic, rest.positional(1)?.first())?)),
        "register" => Ok(Command::Register(tunnel(
            topic,
            rest.positional(1)?.first(),
        )?)),
        "unregister" => {
            confirmed(&mut rest, || tr!("usage.confirm_unregister"))?;
            Ok(Command::Unregister(tunnel(
                topic,
                rest.positional(1)?.first(),
            )?))
        }
        "monitor" | "auto-recovery" | "start-when-stopped" => {
            let words = rest.positional(2)?;
            let id = tunnel(topic, words.first())?;
            let on = on_off(topic, words.get(1))?;
            Ok(match topic {
                "monitor" => Command::Monitor(id, on),
                "auto-recovery" => Command::AutoRecovery(id, on),
                _ => Command::StartWhenStopped(id, on),
            })
        }
        "set" => set(rest),
        "retry" => {
            let limit = match rest.value("--limit")? {
                None => None,
                Some(word) if word == "forever" => Some(None),
                Some(word) => Some(Some(number(topic, "--limit", &word, 0..=3600)?)),
            };
            let interval = rest
                .value("--interval")?
                .map(|w| number(topic, "--interval", &w, 10..=10800))
                .transpose()?;
            let words = rest.positional(1)?;
            let id = tunnel(topic, words.first())?;
            if limit.is_none() && interval.is_none() {
                return Err(usage(topic, tr!("usage.limit_or_interval")));
            }
            Ok(Command::Retry {
                tunnel: id,
                limit,
                interval,
            })
        }
        "connect" | "disconnect" | "reconnect" => {
            let operation = match topic {
                "connect" => Operation::Connect,
                "disconnect" => {
                    confirmed(&mut rest, || tr!("usage.confirm_disconnect"))?;
                    Operation::Disconnect
                }
                _ => Operation::Reconnect,
            };
            let wait_secs = wait(&mut rest)?;
            let id = tunnel(topic, rest.positional(1)?.first())?;
            Ok(Command::Operate {
                operation,
                tunnel: id,
                wait_secs,
            })
        }
        "probe" => {
            let wait_secs = wait(&mut rest)?;
            let id = tunnel(topic, rest.positional(1)?.first())?;
            Ok(Command::Probe {
                tunnel: id,
                wait_secs,
            })
        }
        "pause" => rest.positional(0).map(|_| Command::Pause(true)),
        "resume" => rest.positional(0).map(|_| Command::Pause(false)),
        "notify" => notify(rest),
        "language" => language(rest),
        "host" => host(rest),
        "mcp" => {
            let words = rest.positional(2)?;
            match words.as_slice() {
                [telegram, state] if telegram == "telegram" => {
                    Ok(Command::McpTelegram(on_off(topic, Some(state))?))
                }
                [state] => Ok(Command::Mcp(on_off(topic, Some(state))?)),
                _ => Err(usage(topic, tr!("usage.mcp_forms"))),
            }
        }
        "log" => {
            let tunnel_id = rest.value("--tunnel")?;
            let system = rest.flag("--system")?;
            let watchdog = rest.flag("--watchdog")?;
            let before = rest
                .value("--before")?
                .map(|w| number(topic, "--before", &w, 1..=u64::MAX))
                .transpose()?;
            rest.positional(0)?;
            let filter = match (tunnel_id, system, watchdog) {
                (None, false, false) => EventFilter::All,
                (Some(id), false, false) => EventFilter::Tunnel {
                    tunnel_id: tunnel(topic, Some(&id))?,
                },
                (None, true, false) => EventFilter::System,
                (None, false, true) => EventFilter::Watchdog,
                _ => {
                    return Err(usage(topic, tr!("usage.one_log_filter")));
                }
            };
            Ok(Command::Log { filter, before })
        }
        "export" => rest.positional(0).map(|_| Command::Export),
        "import" => {
            let match_by = match rest.value("--match")?.as_deref() {
                None | Some("name") => ImportMatch::Name,
                Some("id") => ImportMatch::Id,
                Some(other) => {
                    return Err(usage(
                        topic,
                        tr!("usage.match_name_or_id", argument = shown(other)),
                    ));
                }
            };
            let register_missing = rest.flag("--register-missing")?;
            let monitor_new = rest.flag("--monitor-new")?;
            let dry_run = rest.flag("--dry-run")?;
            if !dry_run {
                confirmed(&mut rest, || tr!("usage.confirm_import"))?;
            }
            rest.positional(0)?;
            Ok(Command::Import(ImportOptions {
                match_by,
                register_missing,
                monitor_new,
                dry_run,
            }))
        }
        _ => Err(usage(topic, tr!("usage.unknown_command"))),
    }
}

fn set(mut rest: Rest) -> Result<Command, Usage> {
    let topic = rest.topic;
    let mut args = SetArgs {
        interval: rest
            .value("--interval")?
            .map(|w| number(topic, "--interval", &w, 10..=3600))
            .transpose()?,
        timeout: rest
            .value("--timeout")?
            .map(|w| number(topic, "--timeout", &w, 1..=10))
            .transpose()?,
        retries: rest
            .value("--retries")?
            .map(|w| number(topic, "--retries", &w, 0..=10))
            .transpose()?,
        min_fault: rest
            .value("--min-fault")?
            .map(|w| number(topic, "--min-fault", &w, 0..=3600))
            .transpose()?,
        ..SetArgs::default()
    };
    args.recovery = match rest.value("--recovery-target")? {
        None => None,
        Some(word) if word == "none" => Some(None),
        Some(word) => Some(Some(target(topic, "--recovery-target", &word)?)),
    };
    let observe = rest.values("--observe")?;
    let no_observe = rest.flag("--no-observe")?;
    args.observe = match (observe.is_empty(), no_observe) {
        (true, false) => None,
        (true, true) => Some(Vec::new()),
        (false, true) => {
            return Err(usage(topic, tr!("usage.observe_or_no_observe")));
        }
        (false, false) => {
            if observe.len() > MAX_OBSERVE_TARGETS {
                return Err(usage(
                    topic,
                    tr!("usage.too_many_observe", count = MAX_OBSERVE_TARGETS),
                ));
            }
            Some(
                observe
                    .iter()
                    .map(|w| target(topic, "--observe", w))
                    .collect::<Result<_, _>>()?,
            )
        }
    };
    let words = rest.positional(1)?;
    args.tunnel = Some(tunnel(topic, words.first())?);
    if args
        == (SetArgs {
            tunnel: args.tunnel.clone(),
            ..SetArgs::default()
        })
    {
        return Err(usage(topic, tr!("usage.nothing_to_set")));
    }
    Ok(Command::Set(args))
}

fn notify(mut rest: Rest) -> Result<Command, Usage> {
    let topic = rest.topic;
    let first = rest.words.first().cloned();
    match first.as_deref() {
        None => rest.positional(0).map(|_| Command::Notify(Notify::Show)),
        Some("on" | "off") => {
            let words = rest.positional(1)?;
            Ok(Command::Notify(Notify::Enabled(on_off(
                topic,
                words.first(),
            )?)))
        }
        Some("device") => {
            rest.words.remove(0);
            let words = rest.positional(1)?;
            let name = words
                .first()
                .cloned()
                .ok_or_else(|| usage(topic, tr!("usage.give_device_name")))?;
            Ok(Command::Notify(Notify::Device(name)))
        }
        Some("chat") => {
            rest.words.remove(0);
            let clear = rest.flag("--clear")?;
            let words = rest.positional(1)?;
            match (clear, words.first()) {
                (true, None) => Ok(Command::Notify(Notify::Chat(None))),
                (false, Some(id)) => ChatId::new(id.as_str())
                    .map(|id| Command::Notify(Notify::Chat(Some(id))))
                    .map_err(|_| usage(topic, tr!("usage.not_a_chat_id", argument = shown(id)))),
                _ => Err(usage(topic, tr!("usage.give_chat_id"))),
            }
        }
        Some("tunnel") => {
            rest.words.remove(0);
            let words = rest.positional(2)?;
            let id = tunnel(topic, words.first())?;
            Ok(Command::Notify(Notify::Tunnel(
                id,
                on_off(topic, words.get(1))?,
            )))
        }
        Some("token") => {
            rest.words.remove(0);
            let action = rest.words.first().cloned();
            match action.as_deref() {
                Some("set") => {
                    rest.words.remove(0);
                    rest.positional(0)?;
                    Ok(Command::Notify(Notify::TokenSet))
                }
                Some("clear") => {
                    rest.words.remove(0);
                    confirmed(&mut rest, || tr!("usage.confirm_token_clear"))?;
                    rest.positional(0)?;
                    Ok(Command::Notify(Notify::TokenClear))
                }
                _ => Err(usage(topic, tr!("usage.token_forms"))),
            }
        }
        Some("test") => {
            rest.words.remove(0);
            let wait_secs = wait(&mut rest)?;
            rest.positional(0)?;
            Ok(Command::Notify(Notify::Test { wait_secs }))
        }
        Some("language") => {
            rest.words.remove(0);
            let words = rest.positional(1)?;
            Ok(Command::Notify(Notify::Language(words.first().cloned())))
        }
        Some(other) => Err(usage(
            topic,
            tr!("usage.not_a_notify_command", argument = shown(other)),
        )),
    }
}

fn host(mut rest: Rest) -> Result<Command, Usage> {
    let topic = rest.topic;
    let workers = match rest.value("--workers")? {
        None => None,
        Some(word) if word == "auto" => Some(None),
        Some(word) => {
            let n: u8 = number(topic, "--workers", &word, 2..=17)?;
            Some(Some(
                WorkerCount::new(n).map_err(|e| usage(topic, e.to_string()))?,
            ))
        }
    };
    let time_zone = match rest.value("--time-zone")? {
        None => None,
        Some(word) if word == "system" => Some(None),
        Some(word) => Some(Some(TimeZoneName::new(word.as_str()).map_err(|e| {
            usage(
                topic,
                tr!("usage.not_a_time_zone", argument = shown(&word), reason = e),
            )
        })?)),
    };
    rest.positional(0)?;
    if workers.is_none() && time_zone.is_none() {
        return Err(usage(topic, tr!("usage.workers_or_time_zone")));
    }
    Ok(Command::Host { workers, time_zone })
}
