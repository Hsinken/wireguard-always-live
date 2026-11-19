use std::fmt;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::ids::RequestId;
use crate::template::{LanguageTag, TemplatesId};
use crate::text::DeviceName;

/// A notification value was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidNotificationValue {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for InvalidNotificationValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for InvalidNotificationValue {}

fn invalid(kind: &'static str, reason: &'static str) -> InvalidNotificationValue {
    InvalidNotificationValue { kind, reason }
}

/// A Telegram Bot API token (plan §7.1), only ever sent to the watchdog in
/// [`crate::Request::SetBotToken`], by the GUI or, while the window lets
/// agents set it (`Request::SetMcpTelegramEnabled`), by `wgal-mcp`.
///
/// It has no `Display`, its `Debug` prints `BotToken(***)`, and no response or
/// snapshot type contains it. The watchdog keeps it in its secret store and
/// reports only a [`CredentialState`].
///
/// Accepted form: the bot's numeric ID of [`Self::BOT_ID_DIGITS`] digits,
/// `:`, then [`Self::SECRET_CHARS`] characters of `A-Z a-z 0-9 _ -`.
/// Anything else is rejected before it can reach a URL.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct BotToken(String);

impl BotToken {
    /// Digits of the bot ID before the colon. Telegram's have 8 to 10 now;
    /// 5 fewer or more are accepted, so a small change on Telegram's side
    /// needs no new release while the form stays narrow (owner decision of
    /// 2026-11-06).
    pub const BOT_ID_DIGITS: RangeInclusive<usize> = 3..=15;
    /// Characters of the secret after the colon. Telegram's have 35; 5
    /// fewer or more are accepted, as for the bot ID.
    pub const SECRET_CHARS: RangeInclusive<usize> = 30..=40;
    /// Longest accepted token in bytes.
    pub const MAX_BYTES: usize = *Self::BOT_ID_DIGITS.end() + 1 + *Self::SECRET_CHARS.end();

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidNotificationValue> {
        let value = value.into();
        let kind = "bot token";
        let Some((id, secret)) = value.split_once(':') else {
            return Err(invalid(kind, "expected <bot id>:<secret>"));
        };
        if !Self::BOT_ID_DIGITS.contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(kind, "the bot id must be 3 to 15 digits"));
        }
        if !Self::SECRET_CHARS.contains(&secret.len())
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(invalid(
                kind,
                "the secret must be 30 to 40 letters, digits, '_' or '-'",
            ));
        }
        Ok(Self(value))
    }

    /// The token itself, for building the one Bot API request that needs it.
    /// Never log, format or store the returned text anywhere else.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken(***)")
    }
}

impl TryFrom<String> for BotToken {
    type Error = InvalidNotificationValue;

    fn try_from(value: String) -> Result<Self, InvalidNotificationValue> {
        Self::new(value)
    }
}

impl From<BotToken> for String {
    fn from(token: BotToken) -> String {
        token.0
    }
}

/// A Telegram chat: a numeric ID of [`Self::DIGITS`] digits, not starting
/// with 0, negative for groups and channels, or a public `@username` of
/// [`Self::USERNAME_CHARS`] letters, digits or `_`, starting with a letter.
/// Kept as a string end to end so no JavaScript number conversion can
/// change it (plan §7.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct ChatId(String);

impl ChatId {
    /// Digits of a numeric chat ID, after any `-`. Telegram's fit in 52 bits
    /// (16 digits; a supergroup's is `-100` and 10 more now); 5 more are
    /// accepted as far as a 64-bit integer goes, 19 digits (owner decision
    /// of 2026-11-06).
    pub const DIGITS: RangeInclusive<usize> = 1..=19;
    /// Characters of a public username after the `@`. Telegram's have 5 to
    /// 32 (4 for collectible ones); 5 fewer or more are accepted, at least
    /// one.
    pub const USERNAME_CHARS: RangeInclusive<usize> = 1..=37;
    /// Longest accepted chat ID in bytes (`@` and the longest username).
    pub const MAX_BYTES: usize = 1 + *Self::USERNAME_CHARS.end();

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidNotificationValue> {
        let value = value.into();
        let kind = "chat id";
        let ok = if let Some(user) = value.strip_prefix('@') {
            Self::USERNAME_CHARS.contains(&user.len())
                && user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && user.as_bytes()[0].is_ascii_alphabetic()
        } else {
            let digits = value.strip_prefix('-').unwrap_or(&value);
            Self::DIGITS.contains(&digits.len())
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits.starts_with('0')
        };
        if ok {
            Ok(Self(value))
        } else {
            Err(invalid(
                kind,
                "expected a numeric chat id of 1 to 19 digits not starting with 0, or an @username of 1 to 37 letters, digits or '_' starting with a letter",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ChatId {
    type Error = InvalidNotificationValue;

    fn try_from(value: String) -> Result<Self, InvalidNotificationValue> {
        Self::new(value)
    }
}

impl From<ChatId> for String {
    fn from(id: ChatId) -> String {
        id.0
    }
}

/// Whether the watchdog holds a bot token. The token itself is never reported.
///
/// `unavailable`: one was stored but cannot be read now, for example a locked
/// Keychain or a secret file with the wrong owner. Notifications wait; the
/// watchdog never prompts for it in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    NotSet,
    Stored,
    Unavailable,
}

/// Why a notification, formal or test, was not accepted by Telegram.
///
/// - `not_configured`: notifications are off, or no token or chat ID is set.
/// - `credential_unavailable`: the stored token cannot be read now.
/// - `credentials_rejected`: Telegram refused the token (401, 403).
/// - `chat_rejected`: Telegram refused the chat or the request (400).
/// - `rate_limited`: Telegram asked to retry later (429).
/// - `network_unreachable`: the request provably never left this host.
/// - `outcome_unknown`: it may have been delivered; it is not sent again.
/// - `storage_unavailable`: the reservation could not be persisted, so
///   nothing was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NotificationFailure {
    NotConfigured,
    CredentialUnavailable,
    CredentialsRejected,
    ChatRejected,
    RateLimited,
    NetworkUnreachable,
    OutcomeUnknown,
    StorageUnavailable,
}

/// Where the latest test notification stands (plan §7.4). `sending` until a
/// result is known; `accepted` means the Bot API accepted it, not that anyone
/// read it; `unknown` means it may have been delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum TestNotificationState {
    Sending,
    Accepted,
    Failed,
    Unknown,
}

/// The latest test notification, identified by the envelope `request_id` of
/// the [`crate::Request::SendTestNotification`] that asked for it. A request
/// repeated with the same ID is not sent again. Tests never change any
/// incident's notification state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TestNotificationStatus {
    pub request_id: RequestId,
    pub state: TestNotificationState,
    #[cfg_attr(test, ts(type = "number"))]
    pub requested_at_unix_ms: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub finished_at_unix_ms: Option<i64>,
    pub failure: Option<NotificationFailure>,
}

/// Notification settings and results as the GUI shows them (plan §7.1, §7.4).
///
/// `device_name` is the name formal messages give this computer
/// ([`DeviceName`], at most 64 characters).
/// `last_failure` is why the most recent formal notification was not
/// accepted, if it was not; incidents carry their own notification state.
/// `templates_id` names the message templates in use
/// (`Request::SetNotificationTemplates`), `null` for the watchdog's
/// built-in set, so the GUI can tell whether to send its own.
/// `cli_language` is `wgal`'s shared language (`Request::SetCliLanguage`,
/// watchdog 4.0.0 and later), kept with these settings; `null` when it was
/// never set, or from an older watchdog, when `wgal` writes English.
/// `host_name` is this computer's host name, which messages give when
/// `device_name` is blank (watchdog 4.1.0 and later, owner decision of
/// 2026-11-19), read again before every message; `null` when it cannot be
/// read, or from an older watchdog. Never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct NotificationStatus {
    pub enabled: bool,
    pub device_name: DeviceName,
    pub chat_id: Option<ChatId>,
    pub token: CredentialState,
    pub last_test: Option<TestNotificationStatus>,
    pub last_failure: Option<NotificationFailure>,
    pub templates_id: Option<TemplatesId>,
    #[serde(default)]
    pub cli_language: Option<LanguageTag>,
    #[serde(default)]
    pub host_name: Option<DeviceName>,
}

impl Default for NotificationStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            device_name: DeviceName::fitted(""),
            chat_id: None,
            token: CredentialState::NotSet,
            last_test: None,
            last_failure: None,
            templates_id: None,
            cli_language: None,
            host_name: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456789:AAbbCCddEEffGGhhIIjjKKllMMnnOOpp_-q";

    #[test]
    fn bot_token_accepts_the_bot_api_form_only() {
        assert!(BotToken::new(TOKEN).is_ok());
        // The bounds, 5 either side of Telegram's 8 to 10 digits and 35
        // characters (owner decision of 2026-11-06).
        for (id, secret) in [(3, 30), (15, 40), (10, 35)] {
            let token = format!("{}:{}", "9".repeat(id), "a".repeat(secret));
            assert!(BotToken::new(&token).is_ok(), "{token}");
        }
        assert_eq!(BotToken::MAX_BYTES, 56);
        let too_long = format!("1234567890:{}", "a".repeat(41));
        let id_too_long = format!("{}:{}", "9".repeat(16), "a".repeat(35));
        let secret_short = format!("1234567890:{}", "a".repeat(29));
        for bad in [
            "",
            "123456789",
            ":AAbbCCddEEffGGhhIIjjKKllMMnnOOpp",
            "12:AAbbCCddEEffGGhhIIjjKKllMMnnOOpp",
            "12a:AAbbCCddEEffGGhhIIjjKKllMMnnOOpp",
            "123:short",
            "123:AAbbCCddEEffGGhhIIjjKKllMMnnOO/../x",
            "123:AAbbCCddEEffGGhhIIjjKKllMMnnOOpp?x=1",
            "123:AAbbCCddEEffGGhhIIjjKKllMMnnOOpp\n",
            "123:AAbb:CCddEEffGGhhIIjjKKllMMnnOOpp",
            too_long.as_str(),
            id_too_long.as_str(),
            secret_short.as_str(),
        ] {
            assert!(BotToken::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn bot_token_never_prints_itself() {
        let token = BotToken::new(TOKEN).unwrap();
        assert_eq!(format!("{token:?}"), "BotToken(***)");
        assert!(!format!("{:?}", Some(&token)).contains("AAbb"));
        assert_eq!(token.expose_secret(), TOKEN);
    }

    #[test]
    fn bot_token_deserialization_applies_validation() {
        assert!(serde_json::from_str::<BotToken>(&format!("\"{TOKEN}\"")).is_ok());
        assert!(serde_json::from_str::<BotToken>(r#""123:x""#).is_err());
    }

    #[test]
    fn chat_id_stays_a_string() {
        let username_37 = format!("@{}", "a".repeat(37));
        for ok in [
            "123456789",
            "-1001234567890",
            "@my_channel",
            // A collectible four-letter username, and the bounds.
            "@abcd",
            "@a",
            username_37.as_str(),
            "-9999999999999999999",
            "7",
        ] {
            let id = ChatId::new(ok).unwrap();
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{ok}\""));
            assert_eq!(serde_json::from_str::<ChatId>(&json).unwrap(), id);
        }
        let username_38 = format!("@{}", "a".repeat(38));
        for bad in [
            "",
            "0",
            "00",
            "-000",
            "-",
            "12 34",
            "1e5",
            "@",
            "@1abcde",
            "@a-bcdef",
            " 123",
            "123\n",
            "007",
            "-0123",
            "-99999999999999999999",
            username_38.as_str(),
            "@<script>",
        ] {
            assert!(ChatId::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(serde_json::from_str::<ChatId>("123456789").is_err());
    }
}
