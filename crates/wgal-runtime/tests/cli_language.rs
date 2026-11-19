//! `wgal`'s shared language through the watchdog's loop (owner decision of
//! 2026-11-19): who may set and clear it, that an agent never may, whatever
//! its switches, and that the event log gets no line for it. Fake clock and
//! fake platform only; the notification service keeping it is tested in
//! `notify.rs`.

#![allow(clippy::unwrap_used)]

mod support;

use wgal_protocol::{
    ErrorCode, ErrorReason, LanguageTag, Origin, Outcome, Request, Response, ResponseEnvelope,
};

use support::*;

fn set(language: Option<&str>) -> Request {
    Request::SetCliLanguage {
        language: language.map(|tag| LanguageTag::new(tag).unwrap()),
    }
}

fn shared(sim: &mut Sim) -> Option<String> {
    sim.snapshot()
        .notification
        .cli_language
        .map(|tag| tag.as_str().to_owned())
}

fn as_agent(sim: &mut Sim, request: Request) -> ResponseEnvelope {
    let mut envelope = envelope("mcp-1", request);
    envelope.origin = Some(Origin::Mcp);
    sim.engine
        .request(sim.next_call_id(), &user(), envelope)
        .expect("never waits")
}

/// A user on the access list, without administrator rights, sets and
/// clears it; the answer is `accepted`, and the snapshot shows it.
#[test]
fn a_listed_user_sets_and_clears_the_shared_language() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    assert_eq!(shared(&mut sim), None);
    let answer = sim.call(&user(), set(Some("ja"))).expect("never waits");
    assert!(
        matches!(answer.outcome, Outcome::Ok(Response::Accepted)),
        "{answer:?}"
    );
    assert_eq!(shared(&mut sim).as_deref(), Some("ja"));
    let answer = sim.call(&user(), set(None)).expect("never waits");
    assert!(
        matches!(answer.outcome, Outcome::Ok(Response::Accepted)),
        "{answer:?}"
    );
    assert_eq!(shared(&mut sim), None);
}

/// A caller not on the access list is refused, as for every request.
#[test]
fn a_stranger_may_not_set_it() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    for elevated in [false, true] {
        let answer = sim
            .call(&stranger(elevated), set(Some("ja")))
            .expect("never waits");
        let Outcome::Err(error) = answer.outcome else {
            panic!("accepted: {answer:?}");
        };
        assert_eq!(error.code, ErrorCode::Unauthorized);
        assert_eq!(error.reason, Some(ErrorReason::CallerNotAllowed));
    }
    assert_eq!(shared(&mut sim), None);
}

/// An agent never sets or clears it, with MCP and "agents may set
/// Telegram" in any of the four combinations, and its refusal writes no
/// line to the event log; the user's own change writes none either.
#[test]
fn an_agent_never_sets_it_and_the_log_gets_no_line() {
    let dir = std::env::temp_dir().join(format!("wgal-cli-language-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = wgal_runtime::events::EventLog::open(&dir).unwrap();
    let mut sim = Sim::of(&["alpha"], Some(log));
    sim.settle();
    sim.ok(set(Some("de")));
    for (mcp, telegram) in [(false, false), (false, true), (true, false), (true, true)] {
        sim.ok(Request::SetMcpTelegramEnabled { enabled: telegram });
        sim.ok(Request::SetMcpEnabled { enabled: mcp });
        for request in [set(Some("ja")), set(None)] {
            let answer = as_agent(&mut sim, request);
            let Outcome::Err(error) = answer.outcome else {
                panic!("accepted with MCP {mcp}, Telegram {telegram}: {answer:?}");
            };
            assert_eq!(error.code, ErrorCode::Unauthorized);
            let expected = if mcp {
                None
            } else {
                Some(ErrorReason::McpDisabled)
            };
            assert_eq!(error.reason, expected, "MCP {mcp}, Telegram {telegram}");
        }
        assert_eq!(shared(&mut sim).as_deref(), Some("de"));
    }
    // An agent's other changes are still answered and logged as its own.
    let answer = as_agent(
        &mut sim,
        Request::SetTunnelNotifications {
            tunnel_id: id("alpha"),
            enabled: false,
        },
    );
    assert!(matches!(answer.outcome, Outcome::Ok(_)), "{answer:?}");
    sim.ok(set(None));
    let lines = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    assert!(!lines.contains("set_cli_language"), "{lines}");
    let agent: Vec<&str> = lines
        .lines()
        .filter(|l| l.contains("agent_request"))
        .collect();
    assert_eq!(agent.len(), 1, "{lines}");
    assert!(
        agent[0].contains("set_tunnel_notifications"),
        "{}",
        agent[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The request is refused by the core itself, which leaves notification
/// settings to the runtime's service.
#[test]
fn the_core_leaves_it_to_the_notification_service() {
    let mut sim = Sim::of(&["alpha"], None);
    sim.settle();
    let mut wd = prepared(&sim.lab, &sim.clock, &sim.store, &["alpha"]);
    let Outcome::Err(error) = wd.handle(&set(Some("ja"))) else {
        panic!("the core answered it");
    };
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.reason, Some(ErrorReason::NotAvailableHere));
}
