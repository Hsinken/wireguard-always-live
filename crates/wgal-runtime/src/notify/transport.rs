//! What one Bot API `sendMessage` call came to, and how its response is read
//! (plan §7.3).

use serde::Deserialize;
use wgal_protocol::{BotToken, ChatId};

/// The only host notifications are ever sent to (plan §7.1). Not configurable.
pub const API_HOST: &str = "api.telegram.org";

/// Longest response body read.
pub const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Wait used when a 429 carries no `retry_after`.
pub const DEFAULT_RETRY_AFTER_SECS: u32 = 60;
/// Longest `retry_after` honoured.
pub const MAX_RETRY_AFTER_SECS: u32 = 3600;

/// What became of one message, as far as the sender can prove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The Bot API accepted it. That says nothing about anyone reading it.
    Accepted { message_id: i64 },
    /// Provably never left this host: no connection to the API was made.
    NotSent,
    /// Refused for now (429); nothing was accepted.
    RetryAfter { seconds: u32 },
    /// Refused; trying again will not help until the settings change.
    Rejected(Rejection),
    /// It may have been delivered. Never sent again automatically.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// 401, 403 or 404 (the API's answer to an unknown token): the token is
    /// wrong or the bot may not post there.
    Credentials,
    /// Any other refusal, such as 400 "chat not found".
    Request,
}

/// Sends one plain-text message. Implementations never retry, never follow a
/// redirect, and never put the token anywhere but the request URL.
pub trait Transport: Send + Sync {
    fn send_message(&self, token: &BotToken, chat: &ChatId, text: &str) -> Delivery;
}

#[derive(Deserialize)]
struct ApiResponse {
    ok: bool,
    result: Option<ApiMessage>,
    parameters: Option<ApiParameters>,
}

#[derive(Deserialize)]
struct ApiMessage {
    message_id: i64,
}

#[derive(Deserialize)]
struct ApiParameters {
    retry_after: Option<u32>,
}

/// Reads a complete HTTP response. Anything that does not prove acceptance or
/// refusal is `Unknown` (plan §7.3): a 200 without a readable message ID, a
/// redirect (never followed), 408, and every 5xx.
pub fn classify_response(status: u16, body: &[u8]) -> Delivery {
    let parsed = serde_json::from_slice::<ApiResponse>(body).ok();
    match status {
        200 => match parsed {
            Some(ApiResponse {
                ok: true,
                result: Some(ApiMessage { message_id }),
                ..
            }) => Delivery::Accepted { message_id },
            _ => Delivery::Unknown,
        },
        429 => {
            let seconds = parsed
                .and_then(|r| r.parameters)
                .and_then(|p| p.retry_after)
                .unwrap_or(DEFAULT_RETRY_AFTER_SECS)
                .clamp(1, MAX_RETRY_AFTER_SECS);
            Delivery::RetryAfter { seconds }
        }
        401 | 403 | 404 => Delivery::Rejected(Rejection::Credentials),
        408 => Delivery::Unknown,
        400..=499 => Delivery::Rejected(Rejection::Request),
        _ => Delivery::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_readable_message_id_is_acceptance() {
        let ok = br#"{"ok":true,"result":{"message_id":42,"chat":{"id":1},"text":"x"}}"#;
        assert_eq!(
            classify_response(200, ok),
            Delivery::Accepted { message_id: 42 }
        );
        for body in [
            &b""[..],
            b"not json",
            br#"{"ok":true}"#,
            br#"{"ok":false,"result":{"message_id":42}}"#,
            br#"{"ok":true,"result":{"message_id":"42"}}"#,
        ] {
            assert_eq!(classify_response(200, body), Delivery::Unknown, "{body:?}");
        }
    }

    #[test]
    fn rate_limits_say_when_to_retry() {
        let body = br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#;
        assert_eq!(
            classify_response(429, body),
            Delivery::RetryAfter { seconds: 7 }
        );
        assert_eq!(
            classify_response(429, b""),
            Delivery::RetryAfter {
                seconds: DEFAULT_RETRY_AFTER_SECS
            }
        );
        let huge = br#"{"ok":false,"parameters":{"retry_after":999999}}"#;
        assert_eq!(
            classify_response(429, huge),
            Delivery::RetryAfter {
                seconds: MAX_RETRY_AFTER_SECS
            }
        );
    }

    #[test]
    fn refusals_and_uncertain_answers() {
        for status in [401, 403, 404] {
            assert_eq!(
                classify_response(status, b"{}"),
                Delivery::Rejected(Rejection::Credentials)
            );
        }
        let chat = br#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#;
        assert_eq!(
            classify_response(400, chat),
            Delivery::Rejected(Rejection::Request)
        );
        for status in [201, 204, 301, 302, 307, 308, 408, 500, 502, 503, 504] {
            assert_eq!(
                classify_response(status, b"{}"),
                Delivery::Unknown,
                "{status}"
            );
        }
    }
}
