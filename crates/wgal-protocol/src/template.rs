use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::MAX_ID_LEN;
use crate::text::{DeviceName, DisplayName};

/// A message template or a set of them was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidTemplate {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for InvalidTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for InvalidTemplate {}

fn invalid(kind: &'static str, reason: &'static str) -> InvalidTemplate {
    InvalidTemplate { kind, reason }
}

/// Longest placeholder name, in bytes.
const MAX_NAME_BYTES: usize = 24;

/// One piece of a template: text copied as it is (a `{{` or `}}` becomes
/// the single brace), or a placeholder's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    Placeholder(&'a str),
}

fn is_name(name: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Splits a template into its pieces, or says why it is not one. Braces
/// are ASCII, so every split falls on a character boundary.
fn parse(value: &str) -> Result<Vec<Piece<'_>>, &'static str> {
    let mut pieces = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find(['{', '}']) {
        let (text, tail) = rest.split_at(at);
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        if let Some(after) = tail.strip_prefix("{{") {
            pieces.push(Piece::Text("{"));
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            pieces.push(Piece::Text("}"));
            rest = after;
        } else if let Some(body) = tail.strip_prefix('{') {
            let Some((name, after)) = body.split_once('}') else {
                return Err("a '{' is not closed; write '{{' for a literal brace");
            };
            if !is_name(name) {
                return Err("a placeholder is '{' and 1 to 24 of 'a' to 'z' and '_', then '}'");
            }
            pieces.push(Piece::Placeholder(name));
            rest = after;
        } else {
            return Err("a lone '}'; write '}}' for a literal brace");
        }
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest));
    }
    Ok(pieces)
}

/// The text of one notification message, or of a part of one, with
/// placeholders the watchdog fills in when it sends (see
/// [`NotificationTemplates`]).
///
/// 1 to [`Self::MAX_BYTES`] bytes of UTF-8, starting and ending with
/// something other than whitespace. Line breaks (`\n`) are allowed; any
/// other control character, a carriage return or tab included, is not, so
/// a template can forge no terminal or log output and its JSON form is at
/// most twice its length. A placeholder is `{name}`, the name 1 to 24 of
/// `a`–`z` and `_`; a literal brace is written `{{` or `}}`, and any other
/// `{` or `}` is rejected. Which names a template may use depends on the
/// message it is for; [`NotificationTemplates`] checks that.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct MessageTemplate(String);

impl MessageTemplate {
    /// Maximum length in UTF-8 bytes.
    pub const MAX_BYTES: usize = 1024;

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidTemplate> {
        let value = value.into();
        let reason = if value.is_empty() || value.len() > Self::MAX_BYTES {
            "must be 1 to 1024 bytes"
        } else if value.chars().any(|c| c.is_control() && c != '\n') {
            "control characters other than a line break are not allowed"
        } else if value.starts_with(char::is_whitespace) || value.ends_with(char::is_whitespace) {
            "must not start or end with whitespace"
        } else {
            match parse(&value) {
                Ok(_) => return Ok(Self(value)),
                Err(reason) => reason,
            }
        };
        Err(invalid("message template", reason))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn pieces(&self) -> Vec<Piece<'_>> {
        // Checked on construction, so this never comes out empty-handed.
        parse(&self.0).unwrap_or_default()
    }

    /// The placeholder names the template uses, in order of appearance.
    pub fn placeholders(&self) -> Vec<&str> {
        self.pieces()
            .into_iter()
            .filter_map(|piece| match piece {
                Piece::Placeholder(name) => Some(name),
                Piece::Text(_) => None,
            })
            .collect()
    }

    /// The text with each placeholder replaced by the first value `values`
    /// gives for its name, inserted as it is, and `{{`, `}}` written as one
    /// brace. A name `values` does not give is left empty.
    pub fn render(&self, values: &[(&str, &str)]) -> String {
        let mut out = String::with_capacity(self.0.len());
        for piece in self.pieces() {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Placeholder(name) => {
                    if let Some((_, value)) = values.iter().find(|(n, _)| *n == name) {
                        out.push_str(value);
                    }
                }
            }
        }
        out
    }
}

impl TryFrom<String> for MessageTemplate {
    type Error = InvalidTemplate;

    fn try_from(value: String) -> Result<Self, InvalidTemplate> {
        Self::new(value)
    }
}

impl From<MessageTemplate> for String {
    fn from(template: MessageTemplate) -> String {
        template.0
    }
}

/// The GUI's name for one [`NotificationTemplates`] set, for example its
/// language and a hash of the texts (`zh-TW/3f2a91c0`), so it can tell from
/// a snapshot whether the watchdog has its current set.
///
/// 1 to [`Self::MAX_BYTES`] bytes of ASCII letters, digits and `. _ @ / -`,
/// starting with a letter or digit. The watchdog only stores it and reports
/// it back; it never names a file or anything else with it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct TemplatesId(String);

impl TemplatesId {
    /// Maximum length in bytes.
    pub const MAX_BYTES: usize = 48;

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidTemplate> {
        let value = value.into();
        let reason = if value.is_empty() || value.len() > Self::MAX_BYTES {
            "must be 1 to 48 bytes"
        } else if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'/' | b'-'))
        {
            "only ASCII letters, digits, '.', '_', '@', '/' and '-' are allowed"
        } else if !value.as_bytes()[0].is_ascii_alphanumeric() {
            "must start with a letter or digit"
        } else {
            return Ok(Self(value));
        };
        Err(invalid("templates id", reason))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TemplatesId {
    type Error = InvalidTemplate;

    fn try_from(value: String) -> Result<Self, InvalidTemplate> {
        Self::new(value)
    }
}

impl From<TemplatesId> for String {
    fn from(id: TemplatesId) -> String {
        id.0
    }
}

impl fmt::Display for TemplatesId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A language's tag, such as `en`, `zh-TW` or `pt-BR`: the command line's
/// shared language (`Request::SetCliLanguage`, owner decision of
/// 2026-11-19), kept with the notification settings.
///
/// 2 to [`Self::MAX_BYTES`] bytes of ASCII letters, digits and `-`,
/// starting with a letter, not ending with `-` and without `--`. Only its
/// shape is checked, not a list of languages, so a language added later
/// needs no new watchdog. It is kept and compared as given; `wgal` sends
/// the spelling of its own language table and matches what it reads back
/// without regard to case. The watchdog only stores it and reports it
/// back; it never names a file or anything else with it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Maximum length in bytes.
    pub const MAX_BYTES: usize = 16;
    /// Minimum length in bytes.
    pub const MIN_BYTES: usize = 2;

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidTemplate> {
        let value = value.into();
        let bytes = value.as_bytes();
        let reason = if !(Self::MIN_BYTES..=Self::MAX_BYTES).contains(&value.len()) {
            "must be 2 to 16 bytes"
        } else if !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        {
            "only ASCII letters, digits and '-' are allowed"
        } else if !bytes[0].is_ascii_alphabetic() {
            "must start with a letter"
        } else if bytes.ends_with(b"-") || value.contains("--") {
            "must not end with '-' or have '--'"
        } else {
            return Ok(Self(value));
        };
        Err(invalid("language tag", reason))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LanguageTag {
    type Error = InvalidTemplate;

    fn try_from(value: String) -> Result<Self, InvalidTemplate> {
        Self::new(value)
    }
}

impl From<LanguageTag> for String {
    fn from(tag: LanguageTag) -> String {
        tag.0
    }
}

impl fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The texts of one [`NotificationTemplates`] set before it is checked, one
/// field per template, as documented there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateTexts {
    pub formal: MessageTemplate,
    pub result_failed: MessageTemplate,
    pub result_not_executed: MessageTemplate,
    pub retry_limited: MessageTemplate,
    pub retry_forever: MessageTemplate,
    pub retry_none: MessageTemplate,
    pub exhausted: MessageTemplate,
    pub recovered: MessageTemplate,
    pub test: MessageTemplate,
    pub unnamed_device: MessageTemplate,
}

/// The texts Telegram notifications are written with (owner requests of
/// 2026-11-03 and 2026-11-04): the GUI writes them from its language file
/// and hands them to the watchdog in `Request::SetNotificationTemplates`.
/// The watchdog keeps them and fills the placeholders in when it sends, so
/// it can still notify while no GUI is running. Until a GUI has sent a set,
/// and whenever the stored set cannot be read, it uses its built-in one,
/// the English texts of `fixtures/request_set_notification_templates.json`
/// (English is the default language, owner decision of 2026-11-18; the
/// `wgal` command line hands over the set of the language the user names);
/// `NotificationStatus::templates_id` names the set in use, `null` for the
/// built-in one, so a GUI then sends its own again. A set from before
/// watchdog 1.2.0, without the retry and recovery texts, is not a set, nor
/// is one from before 1.3.0, without `{time_zone}`.
///
/// - `id`: the GUI's name for the set ([`TemplatesId`]).
/// - `formal`: the alert for an incident whose first automatic recovery
///   failed or could not be carried out (plan §7.2). It must use each of
///   these placeholders (`FORMAL_PLACEHOLDERS`) exactly once, so a language
///   file cannot leave out what plan §7.4 says the alert carries:
///   `{device}`, this computer's name, or `unnamed_device` when that is
///   blank; `{tunnel}`, the tunnel's display name; `{incident}`, the
///   incident ID; `{time_zone}`, the time zone the message's times are in
///   (owner request of 2026-11-05); `{started_at}`, when the fault started;
///   `{result}`,
///   `result_failed` or `result_not_executed`; `{retry}`, what automatic
///   recovery does next: `retry_limited`, `retry_forever` or `retry_none`.
/// - `result_failed`: the first recovery round ran and traffic did not
///   come back. `result_not_executed`: it could not be carried out, for
///   example the tunnel could not be started or stopped; never reported
///   as a failed recovery. No placeholders.
/// - `retry_limited`: it tries again every `{interval}` seconds, at most
///   `{limit}` more times. `retry_forever`: every `{interval}` seconds for
///   as long as the fault lasts. `retry_none`: not again (the limit is 0,
///   or already used up). The tunnel's `RecoveryRetry` decides which.
/// - `exhausted`: the message when a limited retry count is used up and
///   automatic recovery stops (owner decision of 2026-11-04). It must use
///   `{device}`, `{tunnel}`, `{incident}`, `{time_zone}`, `{started_at}` as
///   above and `{retries}`, how many rounds followed the first.
/// - `recovered`: the message when an incident that automatic recovery
///   worked on, and that no manual connect or reconnect ended, has stayed
///   healthy through the closing period. It must use `{device}`,
///   `{tunnel}`, `{incident}`, `{time_zone}`, `{started_at}` as above and
///   `{recovered_at}`, when traffic came back.
/// - `test`: the test message (plan §7.4), which must say it is a test.
///   It must use each of these placeholders (`TEST_PLACEHOLDERS`) exactly
///   once: `{device}` and `{time_zone}` as above, and `{time}`, when it was
///   written.
/// - `unnamed_device`: the device name used when none is set. No
///   placeholders; it stands where a name goes, so like one it is a single
///   line of at most `MAX_UNNAMED_DEVICE_BYTES`.
///
/// Times are local time in the configured time zone, `2026-11-05 00:26:01`,
/// in at most `MAX_TIME_BYTES` bytes. `{time_zone}` names the zone and its
/// offset from UTC at the message's first time, `Asia/Taipei, UTC+08:00`
/// (`UTC` for UTC itself, the offset alone for a zone without a name), in
/// at most `MAX_TIME_ZONE_BYTES`; a later time whose offset differs, a
/// daylight saving change having come between, is followed by its own
/// offset, so every time stays exact. Numbers are decimal digits, at most
/// `MAX_NUMBER_BYTES`. A template that leaves out a placeholder it must
/// use, uses one twice, or uses one that is not its own is rejected.
/// `unnamed_device`, the result texts and the retry texts are inserted
/// rendered (`{{` as `{`) and are not read for placeholders again, nor is
/// any other value. So each message is at most its `MAX_*_BYTES`, within the
/// Bot API's 4096 characters.
///
/// Messages are sent as plain text without `parse_mode` (plan §7.1), so
/// neither a template nor a name in it can add formatting. A template can
/// never add an endpoint, internal address, UUID, tunnel ID or token to a
/// message (plan §7.4): there is no placeholder for any of them, and a set
/// using any other name is rejected, on construction and when decoding.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "NotificationTemplatesFields")]
pub struct NotificationTemplates {
    id: TemplatesId,
    formal: MessageTemplate,
    result_failed: MessageTemplate,
    result_not_executed: MessageTemplate,
    retry_limited: MessageTemplate,
    retry_forever: MessageTemplate,
    retry_none: MessageTemplate,
    exhausted: MessageTemplate,
    recovered: MessageTemplate,
    test: MessageTemplate,
    unnamed_device: MessageTemplate,
}

// The wire form of `NotificationTemplates`, before the set is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationTemplatesFields {
    id: TemplatesId,
    formal: MessageTemplate,
    result_failed: MessageTemplate,
    result_not_executed: MessageTemplate,
    retry_limited: MessageTemplate,
    retry_forever: MessageTemplate,
    retry_none: MessageTemplate,
    exhausted: MessageTemplate,
    recovered: MessageTemplate,
    test: MessageTemplate,
    unnamed_device: MessageTemplate,
}

// The Bot API's limit on one message, in characters. Every character is at
// least as many UTF-8 bytes as UTF-16 units, so a message within this many
// bytes is within the limit however it is counted.
const TELEGRAM_MAX_CHARS: usize = 4096;
const _: () = assert!(
    NotificationTemplates::MAX_FORMAL_BYTES <= TELEGRAM_MAX_CHARS
        && NotificationTemplates::MAX_EXHAUSTED_BYTES <= TELEGRAM_MAX_CHARS
        && NotificationTemplates::MAX_RECOVERED_BYTES <= TELEGRAM_MAX_CHARS
        && NotificationTemplates::MAX_TEST_BYTES <= TELEGRAM_MAX_CHARS
);

/// That `template` uses each name in `required` exactly once, in any order,
/// and no other.
fn check(
    kind: &'static str,
    template: &MessageTemplate,
    required: &[&str],
    not_its_own: &'static str,
) -> Result<(), InvalidTemplate> {
    let names = template.placeholders();
    for (i, name) in names.iter().enumerate() {
        if !required.contains(name) {
            return Err(invalid(kind, not_its_own));
        }
        if names[..i].contains(name) {
            return Err(invalid(kind, "uses a placeholder more than once"));
        }
    }
    // Only required names, none twice: all are there if as many are.
    if names.len() != required.len() {
        return Err(invalid(kind, "leaves out a placeholder it must use"));
    }
    Ok(())
}

impl NotificationTemplates {
    /// The placeholders `formal` must use, each exactly once, and the only
    /// ones it may.
    pub const FORMAL_PLACEHOLDERS: &'static [&'static str] = &[
        "device",
        "tunnel",
        "incident",
        "time_zone",
        "started_at",
        "result",
        "retry",
    ];
    /// The placeholders `result_failed` may use: none.
    pub const RESULT_FAILED_PLACEHOLDERS: &'static [&'static str] = &[];
    /// The placeholders `result_not_executed` may use: none.
    pub const RESULT_NOT_EXECUTED_PLACEHOLDERS: &'static [&'static str] = &[];
    /// The placeholders `retry_limited` must use, each exactly once.
    pub const RETRY_LIMITED_PLACEHOLDERS: &'static [&'static str] = &["interval", "limit"];
    /// The placeholders `retry_forever` must use, each exactly once.
    pub const RETRY_FOREVER_PLACEHOLDERS: &'static [&'static str] = &["interval"];
    /// The placeholders `retry_none` may use: none.
    pub const RETRY_NONE_PLACEHOLDERS: &'static [&'static str] = &[];
    /// The placeholders `exhausted` must use, each exactly once.
    pub const EXHAUSTED_PLACEHOLDERS: &'static [&'static str] = &[
        "device",
        "tunnel",
        "incident",
        "time_zone",
        "started_at",
        "retries",
    ];
    /// The placeholders `recovered` must use, each exactly once.
    pub const RECOVERED_PLACEHOLDERS: &'static [&'static str] = &[
        "device",
        "tunnel",
        "incident",
        "time_zone",
        "started_at",
        "recovered_at",
    ];
    /// The placeholders `test` must use, each exactly once, and the only
    /// ones it may.
    pub const TEST_PLACEHOLDERS: &'static [&'static str] = &["device", "time_zone", "time"];
    /// The placeholders `unnamed_device` may use: none.
    pub const UNNAMED_DEVICE_PLACEHOLDERS: &'static [&'static str] = &[];

    /// Longest `unnamed_device`, in bytes: a device name's.
    pub const MAX_UNNAMED_DEVICE_BYTES: usize = DisplayName::MAX_BYTES;
    /// Longest text the watchdog fills a time placeholder with, in bytes.
    pub const MAX_TIME_BYTES: usize = 120;
    /// Longest text the watchdog fills `{time_zone}` with, in bytes: a zone
    /// name of at most `TimeZoneName::MAX_BYTES`, `, ` and the widest offset.
    pub const MAX_TIME_ZONE_BYTES: usize = 80;
    /// Longest number the watchdog fills `{interval}`, `{limit}` or
    /// `{retries}` with, in bytes: a `u32`'s digits.
    pub const MAX_NUMBER_BYTES: usize = 10;
    /// Longest `{retry}` text: a retry template and two numbers.
    pub const MAX_RETRY_BYTES: usize = MessageTemplate::MAX_BYTES + 2 * Self::MAX_NUMBER_BYTES;
    /// Longest formal message, in bytes: the template, two names (device
    /// and tunnel), the incident ID, the result, the zone, the time and the
    /// retry.
    pub const MAX_FORMAL_BYTES: usize = MessageTemplate::MAX_BYTES
        + DeviceName::MAX_BYTES
        + DisplayName::MAX_BYTES
        + MAX_ID_LEN
        + MessageTemplate::MAX_BYTES
        + Self::MAX_TIME_ZONE_BYTES
        + Self::MAX_TIME_BYTES
        + Self::MAX_RETRY_BYTES;
    /// Longest `exhausted` message: the template, two names, the incident
    /// ID, the zone, the time and a number.
    pub const MAX_EXHAUSTED_BYTES: usize = MessageTemplate::MAX_BYTES
        + DeviceName::MAX_BYTES
        + DisplayName::MAX_BYTES
        + MAX_ID_LEN
        + Self::MAX_TIME_ZONE_BYTES
        + Self::MAX_TIME_BYTES
        + Self::MAX_NUMBER_BYTES;
    /// Longest `recovered` message: the template, two names, the incident
    /// ID, the zone and two times.
    pub const MAX_RECOVERED_BYTES: usize = MessageTemplate::MAX_BYTES
        + DeviceName::MAX_BYTES
        + DisplayName::MAX_BYTES
        + MAX_ID_LEN
        + Self::MAX_TIME_ZONE_BYTES
        + 2 * Self::MAX_TIME_BYTES;
    /// Longest test message, in bytes: the template, the device name, the
    /// zone and the time.
    pub const MAX_TEST_BYTES: usize = MessageTemplate::MAX_BYTES
        + DeviceName::MAX_BYTES
        + Self::MAX_TIME_ZONE_BYTES
        + Self::MAX_TIME_BYTES;

    pub fn new(id: TemplatesId, texts: TemplateTexts) -> Result<Self, InvalidTemplate> {
        let TemplateTexts {
            formal,
            result_failed,
            result_not_executed,
            retry_limited,
            retry_forever,
            retry_none,
            exhausted,
            recovered,
            test,
            unnamed_device,
        } = texts;
        for (kind, template, required, not_its_own) in [
            (
                "formal message template",
                &formal,
                Self::FORMAL_PLACEHOLDERS,
                "may use only {device}, {tunnel}, {incident}, {time_zone}, {started_at}, \
                 {result} and {retry}, each exactly once",
            ),
            (
                "retry_limited template",
                &retry_limited,
                Self::RETRY_LIMITED_PLACEHOLDERS,
                "may use only {interval} and {limit}, each exactly once",
            ),
            (
                "retry_forever template",
                &retry_forever,
                Self::RETRY_FOREVER_PLACEHOLDERS,
                "may use only {interval}, exactly once",
            ),
            (
                "exhausted message template",
                &exhausted,
                Self::EXHAUSTED_PLACEHOLDERS,
                "may use only {device}, {tunnel}, {incident}, {time_zone}, {started_at} \
                 and {retries}, each exactly once",
            ),
            (
                "recovered message template",
                &recovered,
                Self::RECOVERED_PLACEHOLDERS,
                "may use only {device}, {tunnel}, {incident}, {time_zone}, {started_at} \
                 and {recovered_at}, each exactly once",
            ),
            (
                "test message template",
                &test,
                Self::TEST_PLACEHOLDERS,
                "may use only {device}, {time_zone} and {time}, each exactly once",
            ),
        ] {
            check(kind, template, required, not_its_own)?;
        }
        for (kind, template) in [
            ("result_failed template", &result_failed),
            ("result_not_executed template", &result_not_executed),
            ("retry_none template", &retry_none),
            ("unnamed_device template", &unnamed_device),
        ] {
            check(kind, template, &[], "may use no placeholder")?;
        }
        if unnamed_device.as_str().len() > Self::MAX_UNNAMED_DEVICE_BYTES
            || unnamed_device.as_str().contains('\n')
        {
            return Err(invalid(
                "unnamed_device template",
                "must be one line of at most 96 bytes, like a device name",
            ));
        }
        Ok(Self {
            id,
            formal,
            result_failed,
            result_not_executed,
            retry_limited,
            retry_forever,
            retry_none,
            exhausted,
            recovered,
            test,
            unnamed_device,
        })
    }

    pub fn id(&self) -> &TemplatesId {
        &self.id
    }

    pub fn formal(&self) -> &MessageTemplate {
        &self.formal
    }

    pub fn result_failed(&self) -> &MessageTemplate {
        &self.result_failed
    }

    pub fn result_not_executed(&self) -> &MessageTemplate {
        &self.result_not_executed
    }

    pub fn retry_limited(&self) -> &MessageTemplate {
        &self.retry_limited
    }

    pub fn retry_forever(&self) -> &MessageTemplate {
        &self.retry_forever
    }

    pub fn retry_none(&self) -> &MessageTemplate {
        &self.retry_none
    }

    pub fn exhausted(&self) -> &MessageTemplate {
        &self.exhausted
    }

    pub fn recovered(&self) -> &MessageTemplate {
        &self.recovered
    }

    pub fn test(&self) -> &MessageTemplate {
        &self.test
    }

    pub fn unnamed_device(&self) -> &MessageTemplate {
        &self.unnamed_device
    }

    /// The texts the set was made from.
    pub fn texts(&self) -> TemplateTexts {
        TemplateTexts {
            formal: self.formal.clone(),
            result_failed: self.result_failed.clone(),
            result_not_executed: self.result_not_executed.clone(),
            retry_limited: self.retry_limited.clone(),
            retry_forever: self.retry_forever.clone(),
            retry_none: self.retry_none.clone(),
            exhausted: self.exhausted.clone(),
            recovered: self.recovered.clone(),
            test: self.test.clone(),
            unnamed_device: self.unnamed_device.clone(),
        }
    }
}

impl TryFrom<NotificationTemplatesFields> for NotificationTemplates {
    type Error = InvalidTemplate;

    fn try_from(fields: NotificationTemplatesFields) -> Result<Self, InvalidTemplate> {
        Self::new(
            fields.id,
            TemplateTexts {
                formal: fields.formal,
                result_failed: fields.result_failed,
                result_not_executed: fields.result_not_executed,
                retry_limited: fields.retry_limited,
                retry_forever: fields.retry_forever,
                retry_none: fields.retry_none,
                exhausted: fields.exhausted,
                recovered: fields.recovered,
                test: fields.test,
                unnamed_device: fields.unnamed_device,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(text: &str) -> MessageTemplate {
        MessageTemplate::new(text).unwrap()
    }

    #[test]
    fn placeholders_are_found_in_order() {
        let t = template("裝置：{device}\n隧道：{tunnel}，{started_at}");
        assert_eq!(t.placeholders(), ["device", "tunnel", "started_at"]);
        assert!(template("no placeholders").placeholders().is_empty());
        let longest = "a".repeat(MAX_NAME_BYTES);
        assert_eq!(
            template(&format!("{{{longest}}}")).placeholders(),
            [longest]
        );
        assert_eq!(template("{a_b}{c}").placeholders(), ["a_b", "c"]);
    }

    #[test]
    fn doubled_braces_are_literal() {
        let t = template("{{device}} is {device}; }} and {{ stay");
        assert_eq!(t.placeholders(), ["device"]);
        assert_eq!(t.render(&[("device", "x")]), "{device} is x; } and { stay");
        assert_eq!(template("{{{device}}}").render(&[("device", "x")]), "{x}");
        assert_eq!(template("{{}}").render(&[]), "{}");
        assert!(template("{{}}").placeholders().is_empty());
    }

    #[test]
    fn rejects_anything_else_in_braces() {
        let too_long = format!("{{{}}}", "a".repeat(MAX_NAME_BYTES + 1));
        for bad in [
            "{}",
            "{ device}",
            "{device }",
            "{Device}",
            "{DEVICE}",
            "{a-b}",
            "{a1}",
            "{測試}",
            "{device",
            "x {",
            "{a{b}",
            "}",
            "a } b",
            "{device}}",
            too_long.as_str(),
        ] {
            assert!(
                MessageTemplate::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_control_characters_but_line_breaks() {
        assert!(MessageTemplate::new("line 1\nline 2").is_ok());
        assert!(MessageTemplate::new("a\n\nb").is_ok());
        for bad in [
            "a\rb",
            "a\r\nb",
            "tab\there",
            "nul\0",
            "del\u{7f}",
            "c1\u{85}",
            "esc\u{1b}[31m",
        ] {
            assert!(
                MessageTemplate::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_empty_text_and_surrounding_whitespace() {
        for bad in [
            "",
            " ",
            "\n",
            "   \n  ",
            " x",
            "x ",
            "\nx",
            "x\n",
            "\u{3000}x",
            "x\u{3000}",
        ] {
            assert!(
                MessageTemplate::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(MessageTemplate::new("x  y").is_ok());
    }

    #[test]
    fn accepts_text_up_to_the_byte_limit() {
        let full = "x".repeat(MessageTemplate::MAX_BYTES);
        assert!(MessageTemplate::new(full.clone()).is_ok());
        assert!(MessageTemplate::new(full + "x").is_err());
        // Multi-byte characters count by their bytes: 341 three-byte
        // characters and one byte fill the limit exactly.
        let wide = format!("{}x", "測".repeat(MessageTemplate::MAX_BYTES / 3));
        assert_eq!(wide.len(), MessageTemplate::MAX_BYTES);
        assert!(MessageTemplate::new(wide.clone()).is_ok());
        assert!(MessageTemplate::new(format!("{wide}測")).is_err());
        let four = "\u{10FFFF}".repeat(MessageTemplate::MAX_BYTES / 4);
        assert_eq!(four.len(), MessageTemplate::MAX_BYTES);
        assert!(MessageTemplate::new(four.clone()).is_ok());
        assert!(MessageTemplate::new(four + "x").is_err());
    }

    #[test]
    fn render_substitutes_and_never_panics() {
        let t = template("{device}：{time}");
        assert_eq!(
            t.render(&[("time", "12:00"), ("device", "筆電")]),
            "筆電：12:00"
        );
        // The first value for a name wins; values are inserted as they are.
        assert_eq!(
            t.render(&[("device", "{tunnel}"), ("device", "y"), ("time", "}}")]),
            "{tunnel}：}}"
        );
        // A name without a value is left empty.
        assert_eq!(t.render(&[]), "：");
        assert_eq!(template("plain").render(&[("device", "x")]), "plain");
    }

    #[test]
    fn deserialization_applies_validation() {
        assert_eq!(
            serde_json::from_str::<MessageTemplate>(r#""a {device}""#).unwrap(),
            template("a {device}")
        );
        for bad in [r#""""#, r#""{x""#, r#""a\tb""#, r#""a ""#, "1"] {
            assert!(
                serde_json::from_str::<MessageTemplate>(bad).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn language_tag_accepts_its_form_only() {
        let longest = format!("a{}", "b".repeat(LanguageTag::MAX_BYTES - 1));
        for ok in [
            "en",
            "zh-TW",
            "pt-BR",
            "ZH-tw",
            "x1",
            "sr-Latn-RS",
            longest.as_str(),
        ] {
            assert_eq!(LanguageTag::new(ok).unwrap().as_str(), ok);
        }
        let too_long = format!("{longest}b");
        for bad in [
            "",
            "e",
            "1x",
            "-en",
            "en-",
            "zh--TW",
            "zh_TW",
            "zh TW",
            "zh/TW",
            "en\n",
            "中文",
            "../en",
            too_long.as_str(),
        ] {
            assert!(LanguageTag::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert_eq!(
            serde_json::from_str::<LanguageTag>(r#""zh-TW""#).unwrap(),
            LanguageTag::new("zh-TW").unwrap()
        );
        assert!(serde_json::from_str::<LanguageTag>(r#""zh--TW""#).is_err());
        assert!(serde_json::from_str::<LanguageTag>("7").is_err());
    }

    #[test]
    fn templates_id_accepts_its_form_only() {
        let longest = format!("a{}", "b".repeat(TemplatesId::MAX_BYTES - 1));
        for ok in ["zh-TW/3f2a91c0", "en", "1", "a.b_c@d/e-f", longest.as_str()] {
            assert_eq!(TemplatesId::new(ok).unwrap().as_str(), ok);
        }
        let too_long = format!("{longest}b");
        for bad in [
            "",
            "/zh-TW",
            ".hidden",
            "-x",
            "_x",
            "@x",
            "zh TW",
            "zh-TW\n",
            "zh\\TW",
            "zh:TW",
            "中文",
            too_long.as_str(),
        ] {
            assert!(TemplatesId::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(serde_json::from_str::<TemplatesId>(r#""zh-TW/1""#).is_ok());
        assert!(serde_json::from_str::<TemplatesId>(r#""../x""#).is_err());
    }

    /// Templates that use exactly their placeholders.
    const FORMAL: &str = "{device} {tunnel} {incident} {time_zone} {started_at} {result} {retry}";
    const TEST: &str = "測試 {device} {time_zone} {time}";
    const EXHAUSTED: &str =
        "{device} {tunnel} {incident} {time_zone} {started_at} 已重試 {retries} 次";
    const RECOVERED: &str =
        "{device} {tunnel} {incident} {time_zone} {started_at} 恢復 {recovered_at}";

    fn texts() -> TemplateTexts {
        TemplateTexts {
            formal: template(FORMAL),
            result_failed: template("第一輪自動復原失敗"),
            result_not_executed: template("未能執行自動復原"),
            retry_limited: template("每 {interval} 秒，最多 {limit} 次"),
            retry_forever: template("每 {interval} 秒持續"),
            retry_none: template("不再重試"),
            exhausted: template(EXHAUSTED),
            recovered: template(RECOVERED),
            test: template(TEST),
            unnamed_device: template("未命名"),
        }
    }

    fn with(
        change: impl FnOnce(&mut TemplateTexts),
    ) -> Result<NotificationTemplates, InvalidTemplate> {
        let mut texts = texts();
        change(&mut texts);
        NotificationTemplates::new(TemplatesId::new("zh-TW/3f2a91c0").unwrap(), texts)
    }

    fn set(
        formal: &str,
        test: &str,
        unnamed_device: &str,
    ) -> Result<NotificationTemplates, InvalidTemplate> {
        let formal = MessageTemplate::new(formal)?;
        let test = MessageTemplate::new(test)?;
        let unnamed_device = MessageTemplate::new(unnamed_device)?;
        with(|t| {
            t.formal = formal;
            t.test = test;
            t.unnamed_device = unnamed_device;
        })
    }

    #[test]
    fn a_set_uses_exactly_its_placeholders() {
        assert!(set(FORMAL, TEST, "未命名").is_ok());
        // In any order, among any text.
        assert!(
            set(
                "{retry}結果：{result}；{started_at}（{time_zone}）{incident} {tunnel}（{device}）",
                "{time}（{time_zone}）：{device}",
                "未命名"
            )
            .is_ok()
        );
        // Nothing beyond them.
        for (formal, test) in [
            (format!("{FORMAL} {{time}}"), TEST.to_owned()),
            (format!("{FORMAL} {{tunnel_id}}"), TEST.to_owned()),
            (format!("{FORMAL} {{address}}"), TEST.to_owned()),
            (format!("{FORMAL} {{endpoint}}"), TEST.to_owned()),
            (format!("{FORMAL} {{token}}"), TEST.to_owned()),
            (format!("{FORMAL} {{interval}}"), TEST.to_owned()),
            (FORMAL.to_owned(), format!("{TEST} {{tunnel}}")),
            (FORMAL.to_owned(), format!("{TEST} {{started_at}}")),
            (FORMAL.to_owned(), format!("{TEST} {{result}}")),
        ] {
            assert!(
                set(&formal, &test, "未命名").is_err(),
                "{formal:?} {test:?}"
            );
        }
        // The new texts too.
        for bad in [
            format!("{EXHAUSTED} {{chat_id}}"),
            EXHAUSTED.replace("{retries}", "x"),
            format!("{EXHAUSTED} {{recovered_at}}"),
        ] {
            let bad = template(&bad);
            assert!(with(|t| t.exhausted = bad).is_err());
        }
        for bad in [
            format!("{RECOVERED} {{token}}"),
            RECOVERED.replace("{recovered_at}", "x"),
            format!("{RECOVERED} {{retries}}"),
        ] {
            let bad = template(&bad);
            assert!(with(|t| t.recovered = bad).is_err());
        }
        assert!(with(|t| t.retry_limited = template("每 {interval} 秒")).is_err());
        assert!(with(|t| t.retry_limited = template("{interval} {limit} {device}")).is_err());
        assert!(with(|t| t.retry_forever = template("持續")).is_err());
        assert!(with(|t| t.retry_forever = template("{interval} {limit}")).is_err());
        // The texts inserted into others use none.
        for placeholder in ["{device}", "{result}", "{time}", "{interval}"] {
            let p = template(placeholder);
            assert!(with(|t| t.result_failed = p.clone()).is_err());
            assert!(with(|t| t.result_not_executed = p.clone()).is_err());
            assert!(with(|t| t.retry_none = p.clone()).is_err());
            assert!(with(|t| t.unnamed_device = p.clone()).is_err());
        }
    }

    #[test]
    fn a_set_leaves_out_no_placeholder_it_must_use() {
        for name in NotificationTemplates::FORMAL_PLACEHOLDERS {
            let formal = FORMAL.replace(&format!("{{{name}}}"), "x");
            assert!(set(&formal, TEST, "未命名").is_err(), "{formal:?}");
        }
        for name in NotificationTemplates::TEST_PLACEHOLDERS {
            let test = TEST.replace(&format!("{{{name}}}"), "x");
            assert!(set(FORMAL, &test, "未命名").is_err(), "{test:?}");
        }
        for name in NotificationTemplates::EXHAUSTED_PLACEHOLDERS {
            let bad = template(&EXHAUSTED.replace(&format!("{{{name}}}"), "x"));
            assert!(with(|t| t.exhausted = bad).is_err(), "{name}");
        }
        for name in NotificationTemplates::RECOVERED_PLACEHOLDERS {
            let bad = template(&RECOVERED.replace(&format!("{{{name}}}"), "x"));
            assert!(with(|t| t.recovered = bad).is_err(), "{name}");
        }
        assert!(set("沒有欄位", TEST, "未命名").is_err());
        assert!(set(FORMAL, "測試", "未命名").is_err());
        // A doubled brace is not a placeholder, so it does not count.
        let formal = FORMAL.replace("{device}", "{{device}}");
        assert!(set(&formal, TEST, "未命名").is_err());
    }

    #[test]
    fn a_set_uses_each_placeholder_only_once() {
        assert!(set(&format!("{FORMAL} {{device}}"), TEST, "y").is_err());
        assert!(set(&format!("{{result}} {FORMAL}"), TEST, "y").is_err());
        assert!(set(&format!("{{retry}} {FORMAL}"), TEST, "y").is_err());
        assert!(set(FORMAL, &format!("{TEST} {{time}}"), "y").is_err());
        let twice = template(&format!("{EXHAUSTED} {{retries}}"));
        assert!(with(|t| t.exhausted = twice).is_err());
        // A doubled brace is not a placeholder.
        assert!(set(&format!("{{{{device}}}} {FORMAL}"), TEST, "y").is_ok());
    }

    #[test]
    fn the_unnamed_device_is_like_a_name() {
        let longest = "測".repeat(NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES / 3);
        assert_eq!(
            longest.len(),
            NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES
        );
        assert!(set(FORMAL, TEST, &longest).is_ok());
        assert!(set(FORMAL, TEST, &format!("{longest}x")).is_err());
        assert!(set(FORMAL, TEST, "two\nlines").is_err());
    }

    #[test]
    fn a_set_is_checked_when_decoding() {
        let json = |formal: &str| {
            serde_json::json!({
                "id": "zh-TW/3f2a91c0",
                "formal": formal,
                "result_failed": "失敗",
                "result_not_executed": "未執行",
                "retry_limited": "每 {interval} 秒，最多 {limit} 次",
                "retry_forever": "每 {interval} 秒",
                "retry_none": "不再重試",
                "exhausted": EXHAUSTED,
                "recovered": RECOVERED,
                "test": TEST,
                "unnamed_device": "未命名",
            })
        };
        let ok: NotificationTemplates = serde_json::from_value(json(FORMAL)).unwrap();
        assert_eq!(ok.formal().as_str(), FORMAL);
        assert_eq!(serde_json::to_value(&ok).unwrap(), json(FORMAL));
        assert_eq!(
            NotificationTemplates::new(ok.id().clone(), ok.texts()).unwrap(),
            ok
        );
        for bad in [
            format!("{FORMAL} {{token}}"),
            format!("{FORMAL} {{device}}"),
            FORMAL.replace("{result}", "x"),
            FORMAL.replace("{retry}", "x"),
            "{device} {result}".to_owned(),
        ] {
            assert!(
                serde_json::from_value::<NotificationTemplates>(json(&bad)).is_err(),
                "{bad:?}"
            );
        }
        let mut no_time = json(FORMAL);
        no_time["test"] = "測試 {device} {time_zone}".into();
        assert!(serde_json::from_value::<NotificationTemplates>(no_time).is_err());
        // A set from before 1.3.0, its times followed by their zone, is not
        // a set: the zone is a placeholder of its own.
        let mut before = json(&FORMAL.replace(" {time_zone}", ""));
        for (field, text) in [
            ("exhausted", EXHAUSTED),
            ("recovered", RECOVERED),
            ("test", TEST),
        ] {
            before[field] = text.replace(" {time_zone}", "").into();
        }
        assert!(serde_json::from_value::<NotificationTemplates>(before.clone()).is_err());
        for field in ["formal", "exhausted", "recovered", "test"] {
            let mut one = json(FORMAL);
            one[field] = before[field].clone();
            assert!(
                serde_json::from_value::<NotificationTemplates>(one).is_err(),
                "{field}"
            );
        }
        let mut extra = json(FORMAL);
        extra["chat_id"] = "-100".into();
        assert!(serde_json::from_value::<NotificationTemplates>(extra).is_err());
        for field in ["unnamed_device", "retry_none", "recovered"] {
            let mut missing = json(FORMAL);
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<NotificationTemplates>(missing).is_err(),
                "{field}"
            );
        }
        // A set from before 1.2.0 is not a set.
        let old = serde_json::json!({
            "id": "zh-TW/3f2a91c0",
            "formal": "{device} {tunnel} {incident} {started_at} {result}",
            "result_failed": "失敗",
            "result_not_executed": "未執行",
            "test": TEST,
            "unnamed_device": "未命名",
        });
        assert!(serde_json::from_value::<NotificationTemplates>(old).is_err());
    }

    /// The largest messages any valid set can make, with the longest values
    /// the watchdog can fill in, stay within the bounds and the Bot API's
    /// 4096 characters.
    #[test]
    fn the_longest_messages_fit_in_one_telegram_message() {
        // The module also checks them against the limit when it compiles.
        // A device name holds 64 characters of up to four bytes (owner
        // decision of 2026-11-06), 160 bytes more than a tunnel's name.
        assert_eq!(NotificationTemplates::MAX_FORMAL_BYTES, 3708);
        assert_eq!(NotificationTemplates::MAX_EXHAUSTED_BYTES, 1650);
        assert_eq!(NotificationTemplates::MAX_RECOVERED_BYTES, 1760);
        assert_eq!(NotificationTemplates::MAX_TEST_BYTES, 1480);
        // The widest zone text fits its bound: the longest name and offset.
        let widest_zone = format!(
            "{}, UTC+25:59:59",
            "A".repeat(crate::TimeZoneName::MAX_BYTES)
        );
        assert!(widest_zone.len() <= NotificationTemplates::MAX_TIME_ZONE_BYTES);

        // Each template at its limit, using every placeholder it may.
        let fill = |names: &[&str], limit: usize| {
            let used: String = names.iter().map(|n| format!("{{{n}}}")).collect();
            let text = format!("{used}{}", "x".repeat(limit - used.len()));
            assert_eq!(text.len(), limit);
            (template(&text), used.len())
        };
        let max = MessageTemplate::MAX_BYTES;
        let (formal, formal_names) = fill(NotificationTemplates::FORMAL_PLACEHOLDERS, max);
        let (exhausted, exhausted_names) = fill(NotificationTemplates::EXHAUSTED_PLACEHOLDERS, max);
        let (recovered, recovered_names) = fill(NotificationTemplates::RECOVERED_PLACEHOLDERS, max);
        let (test, test_names) = fill(NotificationTemplates::TEST_PLACEHOLDERS, max);
        let (retry_limited, retry_names) =
            fill(NotificationTemplates::RETRY_LIMITED_PLACEHOLDERS, max);
        let (retry_forever, _) = fill(NotificationTemplates::RETRY_FOREVER_PLACEHOLDERS, max);
        let result = template(&"x".repeat(max));
        let templates = NotificationTemplates::new(
            TemplatesId::new("x").unwrap(),
            TemplateTexts {
                formal,
                result_failed: result.clone(),
                result_not_executed: result.clone(),
                retry_limited,
                retry_forever,
                retry_none: result,
                exhausted,
                recovered,
                test,
                unnamed_device: template(
                    &"x".repeat(NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES),
                ),
            },
        )
        .unwrap();

        let name = "\u{10FFFF}".repeat(DisplayName::MAX_BYTES / 4);
        let device = "\u{10FFFF}".repeat(DeviceName::MAX_CHARS);
        let incident = "i".repeat(MAX_ID_LEN);
        let time = "9".repeat(NotificationTemplates::MAX_TIME_BYTES);
        let zone = "9".repeat(NotificationTemplates::MAX_TIME_ZONE_BYTES);
        let number = u32::MAX.to_string();
        assert_eq!(number.len(), NotificationTemplates::MAX_NUMBER_BYTES);
        let rendered_result = templates.result_failed().render(&[]);
        let retry = templates
            .retry_limited()
            .render(&[("interval", &number), ("limit", &number)]);
        assert_eq!(
            retry.len(),
            NotificationTemplates::MAX_RETRY_BYTES - retry_names
        );
        let fits = |message: &str, bound: usize, names: usize| {
            // The bound, but for the placeholders' own text.
            assert_eq!(message.len(), bound - names);
            assert!(message.chars().count() <= TELEGRAM_MAX_CHARS);
            assert!(message.encode_utf16().count() <= TELEGRAM_MAX_CHARS);
        };
        let message = templates.formal().render(&[
            ("device", &device),
            ("tunnel", &name),
            ("incident", &incident),
            ("time_zone", &zone),
            ("started_at", &time),
            ("result", &rendered_result),
            ("retry", &retry),
        ]);
        fits(
            &message,
            NotificationTemplates::MAX_FORMAL_BYTES - retry_names,
            formal_names,
        );
        let message = templates.exhausted().render(&[
            ("device", &device),
            ("tunnel", &name),
            ("incident", &incident),
            ("time_zone", &zone),
            ("started_at", &time),
            ("retries", &number),
        ]);
        fits(
            &message,
            NotificationTemplates::MAX_EXHAUSTED_BYTES,
            exhausted_names,
        );
        let message = templates.recovered().render(&[
            ("device", &device),
            ("tunnel", &name),
            ("incident", &incident),
            ("time_zone", &zone),
            ("started_at", &time),
            ("recovered_at", &time),
        ]);
        fits(
            &message,
            NotificationTemplates::MAX_RECOVERED_BYTES,
            recovered_names,
        );

        // The unnamed device's text is no longer than a device name, so the
        // device name's bound covers it too.
        let unnamed = templates.unnamed_device().render(&[]);
        assert!(unnamed.len() <= DeviceName::MAX_BYTES);
        let message =
            templates
                .test()
                .render(&[("device", &device), ("time_zone", &zone), ("time", &time)]);
        fits(&message, NotificationTemplates::MAX_TEST_BYTES, test_names);
    }
}
