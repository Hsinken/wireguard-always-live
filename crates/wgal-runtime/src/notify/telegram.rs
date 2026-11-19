//! The real transport: one HTTPS POST to the Bot API per message (plan §7.1).
//!
//! - The URL is always `https://api.telegram.org/bot<token>/sendMessage`; the
//!   host is fixed and HTTPS is required.
//! - Redirects are not followed (a redirect response is `Unknown`), nothing is
//!   retried, and connections are never reused, so no request is ever sent
//!   twice on a stale connection.
//! - ureq's errors are never formatted, logged or kept: they can carry the
//!   request URL, which holds the token. Only the category below survives.
//!   ureq writes the URL path to the `log` crate at trace level only; the
//!   watchdog must never install a `log` backend that records `ureq` at trace
//!   level.
//! - Proxies set through the standard environment variables (`HTTPS_PROXY`,
//!   `ALL_PROXY`, `NO_PROXY`) are used as configured, so notifications follow
//!   the host's existing network policy and never bypass it (plan §1.2). The
//!   token stays inside TLS through a proxy (`CONNECT`).
//! - Only failures that ureq reports before it writes any request byte count
//!   as not sent: name resolution, a refused or timed-out connection setup
//!   (which includes the proxy CONNECT and the whole TLS handshake), a proxy
//!   that refused CONNECT, and a TLS client that could not be set up. A TLS
//!   handshake that fails on the wire surfaces as
//!   an I/O error indistinguishable from one while reading the response, so it
//!   stays Unknown. Everything else may have been delivered.

use std::io;
use std::time::Duration;

use ureq::{Agent, Error, Timeout};
use wgal_protocol::{BotToken, ChatId};

use super::transport::{API_HOST, Delivery, MAX_RESPONSE_BYTES, Transport, classify_response};

/// Sends through the Bot API. One instance is enough for the whole process.
pub struct HttpTransport {
    agent: Agent,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport").finish_non_exhaustive()
    }
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport {
    pub fn new() -> Self {
        // Every phase has its own limit and the phases add up to less than
        // the total, so a timeout names the phase it happened in.
        let config = Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .timeout_resolve(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_send_request(Some(Duration::from_secs(10)))
            .timeout_send_body(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(20)))
            .timeout_recv_body(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(90)))
            .user_agent("wg-always-live")
            .build();
        Self {
            agent: Agent::new_with_config(config),
        }
    }
}

impl Transport for HttpTransport {
    fn send_message(&self, token: &BotToken, chat: &ChatId, text: &str) -> Delivery {
        // Plain text only: no parse_mode, so a tunnel name cannot inject
        // formatting (plan §7.1).
        let Ok(body) = serde_json::to_vec(&serde_json::json!({
            "chat_id": chat.as_str(),
            "text": text,
            "link_preview_options": { "is_disabled": true },
        })) else {
            return Delivery::NotSent;
        };
        let url = format!(
            "https://{API_HOST}/bot{}/sendMessage",
            token.expose_secret()
        );
        let result = self
            .agent
            .post(url.as_str())
            .header("content-type", "application/json")
            .send(&body[..]);
        drop(url);
        match result {
            Ok(mut response) => {
                let status = response.status().as_u16();
                match response
                    .body_mut()
                    .with_config()
                    .limit(MAX_RESPONSE_BYTES)
                    .read_to_vec()
                {
                    Ok(bytes) => classify_response(status, &bytes),
                    // The status alone can still prove a refusal; an
                    // unreadable acceptance is not proof of anything.
                    Err(_) => match classify_response(status, b"") {
                        Delivery::Rejected(r) => Delivery::Rejected(r),
                        Delivery::RetryAfter { seconds } => Delivery::RetryAfter { seconds },
                        _ => Delivery::Unknown,
                    },
                }
            }
            Err(error) => classify_error(&error),
        }
    }
}

/// Whether a failed call provably never reached the API.
fn classify_error(error: &Error) -> Delivery {
    match error {
        Error::HostNotFound
        | Error::ConnectionFailed
        | Error::ConnectProxyFailed(_)
        | Error::Rustls(_)
        | Error::Timeout(Timeout::Resolve | Timeout::Connect) => Delivery::NotSent,
        // Only connect can refuse a connection; other I/O errors, including
        // "address not available", can also come from reading the answer.
        Error::Io(e) if e.kind() == io::ErrorKind::ConnectionRefused => Delivery::NotSent,
        _ => Delivery::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_failures_before_a_connection_are_not_sent() {
        for error in [
            Error::HostNotFound,
            Error::ConnectionFailed,
            Error::Timeout(Timeout::Resolve),
            Error::Timeout(Timeout::Connect),
            Error::Io(io::Error::from(io::ErrorKind::ConnectionRefused)),
            Error::ConnectProxyFailed("407 Proxy Authentication Required".into()),
        ] {
            assert_eq!(classify_error(&error), Delivery::NotSent, "{error:?}");
        }
        for error in [
            Error::Timeout(Timeout::SendRequest),
            Error::Timeout(Timeout::SendBody),
            Error::Timeout(Timeout::RecvResponse),
            Error::Timeout(Timeout::RecvBody),
            Error::Timeout(Timeout::Global),
            Error::Timeout(Timeout::PerCall),
            Error::Io(io::Error::from(io::ErrorKind::ConnectionReset)),
            Error::Io(io::Error::from(io::ErrorKind::BrokenPipe)),
            Error::Io(io::Error::from(io::ErrorKind::TimedOut)),
            // A handshake failing on the wire, or a TLS error while reading
            // the answer: ureq cannot tell them apart.
            Error::Io(io::Error::from(io::ErrorKind::InvalidData)),
            Error::Io(io::Error::from(io::ErrorKind::HostUnreachable)),
            Error::Io(io::Error::from(io::ErrorKind::AddrNotAvailable)),
            Error::Io(io::Error::from(io::ErrorKind::NetworkUnreachable)),
            Error::Tls("handshake"),
            Error::TooManyRedirects,
            Error::RedirectFailed,
        ] {
            assert_eq!(classify_error(&error), Delivery::Unknown, "{error:?}");
        }
    }
}
