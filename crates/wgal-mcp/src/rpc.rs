//! The Model Context Protocol's JSON-RPC: `initialize`, `ping`,
//! `tools/list` and `tools/call`; notifications get no answer. A tool's own
//! failure, a refused request or an argument out of range included, is a
//! tool result with `isError`, worded so the agent can correct the call;
//! only a message that is not one of these is a JSON-RPC error. Every
//! message is checked against JSON-RPC 2.0 and MCP first: `"jsonrpc":
//! "2.0"`, an `id` that is a string or an integer, a `method` that is a
//! string, `params` that are an object, and the parameters `initialize`
//! requires (Codex P3-N25).

use serde_json::{Value, json};

use crate::tools;
use wgal_client::Watchdog;

/// Protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// What the server tells an agent about WGAL when it connects.
const INSTRUCTIONS: &str = "WG Always Live (WGAL) watches the official WireGuard tunnels on this computer, \
checks that traffic really flows through each one, can restart only the one that failed (automatic \
recovery) and sends Telegram notifications. You act as the user who started you, within the same \
access list as the WGAL window. Start with `health` for every tunnel's state in one call, \
`get_tunnel` for one tunnel's every setting, and `list_tunnels` to compare WGAL's tunnels with \
the official WireGuard manager's. Tools that start or stop a real VPN \
(`connect_tunnel`, `disconnect_tunnel`, `reconnect_tunnel`) interrupt traffic through it: use them \
only when the user wants that. Before changing settings, call `export_settings` and keep the \
document, so `import_settings` can put it back. `read_event_log` reads what happened recently. \
Times are given in UTC (`*_at`) and in Unix milliseconds. The Bot Token is never given out. The \
device name, Chat ID and Bot Token are set in the WGAL window on its 通知 (Notifications) page; \
`set_telegram_settings` changes them, and `health` shows the Chat ID, only while the user allows \
it there on the window's MCP tab, or with `wgal mcp telegram on` in a terminal (off by default). \
MCP itself is switched on or off only by the user, there or with `wgal mcp on|off`, so tell the \
user where to go. Every change you make is written to WGAL's log as made by an agent.";

/// One server, one agent connection.
pub struct Server<W> {
    watchdog: W,
}

impl<W: Watchdog> Server<W> {
    pub fn new(watchdog: W) -> Self {
        Self { watchdog }
    }

    /// Handles one line from the agent; `None` when it needs no answer.
    pub fn handle_line(&mut self, line: &[u8]) -> Option<String> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        let message: Value = match serde_json::from_slice(line) {
            Ok(message) => message,
            Err(_) => {
                return Some(error(Value::Null, PARSE_ERROR, "not a JSON message").to_string());
            }
        };
        // Nothing shaped like a Bot Token leaves this server, whatever it
        // came from (an unknown method or tool name included; an `id`
        // shaped like one is refused in `handle`).
        self.handle(message).map(|mut answer| {
            for part in ["result", "error"] {
                if let Some(value) = answer.get_mut(part) {
                    tools::hide_tokens(value);
                }
            }
            answer.to_string()
        })
    }

    fn handle(&mut self, message: Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "a message is one JSON object (JSON-RPC 2.0; batches are not used by MCP)",
            ));
        };
        // A string or an integer; anything else cannot be answered by it.
        let id = match object.get("id") {
            None => None,
            Some(id @ Value::String(_)) => Some(id.clone()),
            Some(id @ Value::Number(n)) if n.is_i64() || n.is_u64() => Some(id.clone()),
            Some(_) => {
                return Some(error(
                    Value::Null,
                    INVALID_REQUEST,
                    "\"id\" must be a string or an integer",
                ));
            }
        };
        // N-06: an `id` shaped like a Bot Token is refused here, before
        // anything else is done, and answered without it, so no answer
        // carries it back; an `id` once accepted is never changed, so every
        // answer still names its request. A response is still not answered.
        if let Some(Value::String(text)) = &id
            && tools::looks_like_token(text)
        {
            if !object.contains_key("method")
                && (object.contains_key("result") || object.contains_key("error"))
            {
                return None;
            }
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "\"id\" is shaped like a Bot Token, so the message was refused before anything was done and its id is not repeated; send it again with another id",
            ));
        }
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            // Not JSON-RPC 2.0: a notification of that kind is dropped.
            return id.map(|id| error(id, INVALID_REQUEST, "\"jsonrpc\" must be \"2.0\""));
        }
        let method = match object.get("method") {
            Some(Value::String(method)) => method.as_str(),
            Some(_) => {
                return id.map(|id| error(id, INVALID_REQUEST, "\"method\" must be a string"));
            }
            // A response: this server sends no requests, so it answers
            // none (JSON-RPC 2.0 never answers a response).
            None if object.contains_key("result") || object.contains_key("error") => {
                return None;
            }
            None => {
                return id.map(|id| error(id, INVALID_REQUEST, "a request needs a \"method\""));
            }
        };
        let params = match object.get("params") {
            None => Value::Object(serde_json::Map::new()),
            Some(params @ Value::Object(_)) => params.clone(),
            Some(_) => {
                return id.map(|id| error(id, INVALID_PARAMS, "\"params\" must be an object"));
            }
        };
        let Some(id) = id else {
            // A notification (`notifications/initialized` and others).
            return None;
        };
        Some(match method {
            "initialize" => match initialize(&params) {
                Ok(result) => success(id, result),
                Err(message) => error(id, INVALID_PARAMS, &message),
            },
            "ping" => success(id, json!({})),
            "tools/list" => success(id, json!({ "tools": tools::definitions() })),
            "tools/call" => self.call(id, &params),
            _ => error(
                id,
                METHOD_NOT_FOUND,
                &format!("unknown method {}", tools::shown(format!("{method:?}"))),
            ),
        })
    }

    fn call(&mut self, id: Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(
                id,
                INVALID_PARAMS,
                "tools/call needs \"name\", the tool's name",
            );
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match tools::call(&mut self.watchdog, name, &arguments) {
            Some(result) => success(id, result.into_value()),
            None => error(
                id,
                INVALID_PARAMS,
                &format!(
                    "unknown tool {}; call tools/list for the tools: {}",
                    tools::shown(format!("{name:?}")),
                    tools::names().join(", ")
                ),
            ),
        }
    }
}

/// The answer to `initialize`, once its required parameters are there:
/// `protocolVersion` (a string), `capabilities` (an object) and
/// `clientInfo` with a `name` and a `version` (strings). An error says
/// which one is missing or of the wrong type.
fn initialize(params: &Value) -> Result<Value, String> {
    let Some(asked) = params.get("protocolVersion").and_then(Value::as_str) else {
        return Err(format!(
            "initialize needs \"protocolVersion\", a string such as {:?}; this server speaks {}",
            VERSIONS[0],
            VERSIONS.join(", ")
        ));
    };
    if !params.get("capabilities").is_some_and(Value::is_object) {
        return Err("initialize needs \"capabilities\", an object ({} when there are none)".into());
    }
    let info = params.get("clientInfo");
    let text = |key: &str| info.and_then(|i| i.get(key)).is_some_and(Value::is_string);
    if !info.is_some_and(Value::is_object) || !text("name") || !text("version") {
        return Err(
            "initialize needs \"clientInfo\", an object with \"name\" and \"version\" strings"
                .into(),
        );
    }
    // The newest version this server speaks when it does not speak the
    // one asked for; the client decides whether it can use it.
    let version = Some(asked)
        .filter(|v| VERSIONS.contains(v))
        .unwrap_or(VERSIONS[0]);
    Ok(json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "wgal", "title": "WG Always Live", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    }))
}

/// The answer to a line longer than `limit` bytes, which was skipped
/// unread: no `id` can be known from it.
pub fn too_long(limit: usize) -> String {
    error(
        Value::Null,
        INVALID_REQUEST,
        &format!(
            "the message is longer than {limit} bytes and was skipped; send one JSON-RPC message of at most {limit} bytes per line"
        ),
    )
    .to_string()
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use serde_json::{Value, json};
    use wgal_protocol::{Request, ResponseEnvelope};

    use super::Server;
    use wgal_client::{Unreachable, Watchdog};

    /// A watchdog that is never there.
    struct Gone;

    impl Watchdog for Gone {
        fn send(&mut self, _: &Request) -> Result<ResponseEnvelope, Unreachable> {
            Err(Unreachable::not_sent(
                "the WGAL background service (WGALWatchdog) is not running",
            ))
        }

        fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
            Ok(Vec::new())
        }
    }

    fn initialize_params(version: &str) -> Value {
        json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": { "name": "test-agent", "version": "1.0.0" },
        })
    }

    fn ask(server: &mut Server<Gone>, message: Value) -> Option<Value> {
        server
            .handle_line(message.to_string().as_bytes())
            .map(|answer| serde_json::from_str(&answer).unwrap())
    }

    #[test]
    fn it_speaks_the_handshake_and_lists_its_tools() {
        let mut server = Server::new(Gone);
        let init = ask(&mut server, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params("2025-06-18") })).unwrap();
        assert_eq!(init["result"]["protocolVersion"], json!("2025-06-18"));
        assert_eq!(init["result"]["serverInfo"]["name"], json!("wgal"));
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("export_settings")
        );
        let newer = ask(&mut server, json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": initialize_params("2099-01-01") })).unwrap();
        assert_eq!(newer["result"]["protocolVersion"], json!("2025-11-25"));
        assert_eq!(
            ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
            ),
            None
        );
        let list = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        )
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 22);
        assert_eq!(
            ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" })
            )
            .unwrap()["result"],
            json!({})
        );
    }

    #[test]
    fn mistakes_get_answers_that_say_what_to_do() {
        let mut server = Server::new(Gone);
        let parse = server.handle_line(b"{not json").unwrap();
        assert!(parse.contains("-32700"));
        let unknown = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 5, "method": "shell/exec" }),
        )
        .unwrap();
        assert_eq!(unknown["error"]["code"], json!(-32601));
        let tool = ask(&mut server, json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": { "name": "run_command" } })).unwrap();
        assert!(
            tool["error"]["message"]
                .as_str()
                .unwrap()
                .contains("call tools/list")
        );
        // A watchdog that is not there is the tool's own failure, worded.
        let down = ask(&mut server, json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "health", "arguments": {} } })).unwrap();
        assert_eq!(down["result"]["isError"], json!(true));
        assert!(
            down["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("WGALWatchdog")
        );
        assert_eq!(server.handle_line(b"   "), None);
    }

    /// Codex P3-N25: a message that is not JSON-RPC 2.0 as MCP uses it is
    /// refused before anything is done, with a message that says what is
    /// expected; responses and malformed notifications get no answer.
    /// A token sent as a method or tool name, or in arguments, is not sent
    /// back (Codex review of 2026-11-06). The token is an invented one.
    #[test]
    fn no_answer_repeats_a_token() {
        let token = "123456789:EXAMPLE-not-a-real-token_0000000000";
        let mut server = Server::new(Gone);
        for message in [
            json!({ "jsonrpc": "2.0", "id": 1, "method": token }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": token } }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "health", "arguments": { token: 1 } } }),
        ] {
            let answer = ask(&mut server, message).unwrap();
            assert!(
                !answer.to_string().contains("EXAMPLE-not-a-real-token"),
                "{answer}"
            );
        }
    }

    /// A watchdog that counts what it is asked, and is never there.
    #[derive(Default)]
    struct Counted(usize);

    impl Watchdog for Counted {
        fn send(&mut self, request: &Request) -> Result<ResponseEnvelope, Unreachable> {
            self.0 += 1;
            Gone.send(request)
        }

        fn log_locations(&self) -> Result<Vec<(String, PathBuf)>, Unreachable> {
            Ok(Vec::new())
        }
    }

    /// N-06: an `id` shaped like a Bot Token is refused before anything is
    /// done, and no answer carries it back, on the success path (`ping`),
    /// the error path (an unknown method) and before a tool would run. The
    /// token is an invented one; an ordinary id is still answered with it.
    #[test]
    fn an_id_shaped_like_a_token_is_refused_and_not_repeated() {
        let token = "123456789:EXAMPLE-not-a-real-token_0000000000";
        let mut server = Server::new(Counted::default());
        for message in [
            json!({ "jsonrpc": "2.0", "id": token, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": token, "method": "no/such/method" }),
            json!({ "jsonrpc": "2.0", "id": format!("call-{token}"), "method": "tools/call", "params": { "name": "health", "arguments": {} } }),
            json!({ "jsonrpc": "1.0", "id": token, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": token, "method": "tools/list", "params": [1] }),
        ] {
            let answer = server.handle_line(message.to_string().as_bytes()).unwrap();
            assert!(!answer.contains("EXAMPLE-not-a-real-token"), "{answer}");
            let answer: Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(answer["id"], Value::Null, "{answer}");
            assert_eq!(answer["error"]["code"], json!(-32600), "{answer}");
            assert!(
                answer["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("shaped like a Bot Token"),
                "{answer}"
            );
        }
        assert_eq!(server.watchdog.0, 0, "no tool ran");
        // A response with such an id is still not answered.
        assert_eq!(
            server.handle_line(
                json!({ "jsonrpc": "2.0", "id": token, "result": {} })
                    .to_string()
                    .as_bytes()
            ),
            None
        );
        let ok = server
            .handle_line(
                json!({ "jsonrpc": "2.0", "id": "123:short", "method": "ping" })
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let ok: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(ok["id"], json!("123:short"));
        assert_eq!(ok["result"], json!({}));
    }

    #[test]
    fn the_envelope_is_checked_before_anything_is_done() {
        let mut server = Server::new(Gone);
        let code = |answer: &Value| answer["error"]["code"].as_i64().unwrap();
        let text = |answer: &Value| answer["error"]["message"].as_str().unwrap().to_owned();
        // No or a wrong `jsonrpc`.
        for message in [
            json!({ "id": 1, "method": "ping" }),
            json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }),
            json!({ "jsonrpc": 2, "id": 1, "method": "ping" }),
        ] {
            let answer = ask(&mut server, message).unwrap();
            assert_eq!(code(&answer), -32600);
            assert_eq!(answer["id"], json!(1));
            assert!(text(&answer).contains("\"2.0\""));
        }
        // An `id` of the wrong type is answered without it.
        for id in [
            json!([1]),
            json!({ "a": 1 }),
            json!(true),
            json!(1.5),
            Value::Null,
        ] {
            let answer = ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }),
            )
            .unwrap();
            assert_eq!(code(&answer), -32600);
            assert_eq!(answer["id"], Value::Null);
            assert!(text(&answer).contains("string or an integer"));
        }
        let ok = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": "a-1", "method": "ping" }),
        )
        .unwrap();
        assert_eq!(ok["id"], json!("a-1"));
        assert_eq!(ok["result"], json!({}));
        // A method that is not a string, params that are not an object.
        let answer = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 2, "method": 7 }),
        )
        .unwrap();
        assert_eq!(code(&answer), -32600);
        let answer = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": [1] }),
        )
        .unwrap();
        assert_eq!(code(&answer), -32602);
        // A response, or a notification that is not JSON-RPC 2.0: no answer.
        assert_eq!(
            ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "id": 4, "result": {} })
            ),
            None
        );
        assert_eq!(
            ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "id": 5, "error": { "code": 1, "message": "x" } })
            ),
            None
        );
        assert_eq!(
            ask(
                &mut server,
                json!({ "method": "notifications/initialized" })
            ),
            None
        );
        // `initialize` without what it requires says what is missing.
        for (params, missing) in [
            (json!({}), "protocolVersion"),
            (json!({ "protocolVersion": "2025-11-25" }), "capabilities"),
            (
                json!({ "protocolVersion": "2025-11-25", "capabilities": {} }),
                "clientInfo",
            ),
            (
                json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "a" } }),
                "clientInfo",
            ),
            (
                json!({ "protocolVersion": 20251125, "capabilities": {}, "clientInfo": { "name": "a", "version": "1" } }),
                "protocolVersion",
            ),
        ] {
            let answer = ask(
                &mut server,
                json!({ "jsonrpc": "2.0", "id": 6, "method": "initialize", "params": params }),
            )
            .unwrap();
            assert_eq!(code(&answer), -32602, "{answer}");
            assert!(text(&answer).contains(missing), "{answer}");
        }
        // A line over the limit is answered with the limit.
        let long: Value = serde_json::from_str(&super::too_long(1024)).unwrap();
        assert_eq!(code(&long), -32600);
        assert!(text(&long).contains("1024 bytes"));
    }
}
