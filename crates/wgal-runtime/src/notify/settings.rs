//! Notification settings (plan §7.1): on or off, the device name and the chat,
//! the message templates a GUI handed over (wave three, stage 8), and
//! `wgal`'s shared language (owner decision of 2026-11-19). The bot token is
//! never part of them; it lives in the secret store.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Deserializer, Serialize};
use wgal_protocol::{ChatId, DeviceName, LanguageTag, NotificationTemplates};

use super::files;

/// Version of the settings file. `templates` and `cli_language` came later
/// without a new version: a file without them still reads, with none.
pub const SETTINGS_VERSION: u32 = 1;
/// Name of the settings file in the watchdog's state directory.
pub const SETTINGS_FILE: &str = "notify.json";
/// Largest settings file read or written. The largest settings, templates
/// included, take about 21 KiB as written since the retry and recovery
/// texts of 1.2.0 (see the tests).
const MAX_SETTINGS_BYTES: usize = 32 * 1024;

/// `token_set` records that a token was stored, never the token: a store
/// that then finds nothing (a Keychain item it may not read without asking)
/// is reported unavailable rather than "not set".
///
/// `templates` is the set a GUI handed over (`Request::SetNotificationTemplates`),
/// `None` for the built-in one. A file from a build before templates has
/// none, and a set kept that does not read as one now (changed by hand, or
/// kept under other rules) reads as none too, so the rest of the settings
/// still load and notifications go on with the built-in set. Written only
/// when there is one.
///
/// `cli_language` is `wgal`'s shared language (`Request::SetCliLanguage`,
/// watchdog 4.0.0, owner decision of 2026-11-19), `None` when it was never
/// set or was cleared. It is written only when there is one, so a file
/// without it still reads in a 3.x watchdog; one with it does not (this
/// struct, there, refuses fields it does not have), which is why 4.0.0 is a
/// major version and why `wgal language --clear` exists. One stored that
/// does not read as a tag reads as none, and the rest still loads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationSettings {
    pub schema_version: u32,
    pub enabled: bool,
    #[serde(deserialize_with = "device_name_fitted")]
    pub device_name: DeviceName,
    pub chat_id: Option<ChatId>,
    #[serde(default)]
    pub token_set: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "templates_or_none"
    )]
    pub templates: Option<NotificationTemplates>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "language_or_none"
    )]
    pub cli_language: Option<LanguageTag>,
}

/// The stored device name, fitted ([`DeviceName::fitted`]): a name stored
/// under other rules is kept, cut if it has more than 64 characters, rather
/// than failing the whole file and leaving notifications off.
fn device_name_fitted<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DeviceName, D::Error> {
    Ok(DeviceName::fitted(&String::deserialize(deserializer)?))
}

/// The stored templates, or none when what is stored is not a valid set.
/// Only JSON that does not parse at all fails, as it would fail the whole
/// file anyway.
fn templates_or_none<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<NotificationTemplates>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// The stored shared language, or none when what is stored is not a tag.
fn language_or_none<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LanguageTag>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_VERSION,
            enabled: false,
            device_name: DeviceName::fitted(""),
            chat_id: None,
            token_set: false,
            templates: None,
            cli_language: None,
        }
    }
}

/// Settings could not be read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsError;

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("notification settings could not be stored")
    }
}

impl std::error::Error for SettingsError {}

/// Where the settings are kept.
pub trait SettingsStore: Send + fmt::Debug {
    /// The stored settings, or the defaults when none were stored yet.
    fn load(&self) -> Result<NotificationSettings, SettingsError>;
    fn save(&mut self, settings: &NotificationSettings) -> Result<(), SettingsError>;
}

/// `notify.json` in the watchdog's state directory.
#[derive(Debug)]
pub struct FileSettingsStore {
    path: PathBuf,
}

impl FileSettingsStore {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(SETTINGS_FILE),
        }
    }
}

impl SettingsStore for FileSettingsStore {
    fn load(&self) -> Result<NotificationSettings, SettingsError> {
        match files::read_capped(&self.path, MAX_SETTINGS_BYTES) {
            Ok(None) => Ok(NotificationSettings::default()),
            Ok(Some(bytes)) => {
                let settings: NotificationSettings =
                    serde_json::from_slice(&bytes).map_err(|_| SettingsError)?;
                if settings.schema_version == SETTINGS_VERSION {
                    Ok(settings)
                } else {
                    Err(SettingsError)
                }
            }
            Err(_) => Err(SettingsError),
        }
    }

    fn save(&mut self, settings: &NotificationSettings) -> Result<(), SettingsError> {
        let bytes = serde_json::to_vec_pretty(settings).map_err(|_| SettingsError)?;
        // Never write a file that would not be read back.
        if bytes.len() > MAX_SETTINGS_BYTES {
            return Err(SettingsError);
        }
        files::replace(&self.path, &bytes).map_err(|_| SettingsError)
    }
}

/// In-memory settings for tests, shareable to inspect what was saved.
#[derive(Clone, Debug, Default)]
pub struct MemorySettingsStore {
    inner: Arc<Mutex<(Option<NotificationSettings>, bool)>>,
}

impl MemorySettingsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_failing(&self, failing: bool) {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).1 = failing;
    }

    pub fn saved(&self) -> Option<NotificationSettings> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }
}

impl SettingsStore for MemorySettingsStore {
    fn load(&self) -> Result<NotificationSettings, SettingsError> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.1 {
            return Err(SettingsError);
        }
        Ok(inner.0.clone().unwrap_or_default())
    }

    fn save(&mut self, settings: &NotificationSettings) -> Result<(), SettingsError> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.1 {
            return Err(SettingsError);
        }
        inner.0 = Some(settings.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgal_protocol::{MessageTemplate, TemplateTexts, TemplatesId};

    fn template(text: &str) -> MessageTemplate {
        MessageTemplate::new(text).unwrap()
    }

    fn set(id: &str) -> NotificationTemplates {
        NotificationTemplates::new(
            TemplatesId::new(id).unwrap(),
            TemplateTexts {
                formal: template(
                    "{device} {tunnel} {incident} {time_zone} {started_at} {result} {retry}",
                ),
                result_failed: template("failed"),
                result_not_executed: template("not executed"),
                retry_limited: template("{interval} {limit}"),
                retry_forever: template("{interval}"),
                retry_none: template("none"),
                exhausted: template(
                    "{device} {tunnel} {incident} {time_zone} {started_at} {retries}",
                ),
                recovered: template(
                    "{device} {tunnel} {incident} {time_zone} {started_at} {recovered_at}",
                ),
                test: template("test {device} {time_zone} {time}"),
                unnamed_device: template("unnamed"),
            },
        )
        .unwrap()
    }

    fn read(json: &str) -> NotificationSettings {
        serde_json::from_str(json).unwrap()
    }

    const OLDER: &str = r#"{
        "schema_version": 1,
        "enabled": true,
        "device_name": "Lab laptop",
        "chat_id": "-1001234567890",
        "token_set": true
    }"#;

    #[test]
    fn a_file_from_before_templates_reads_with_none() {
        let settings = read(OLDER);
        assert!(settings.enabled);
        assert_eq!(settings.templates, None);
        // And none is not written, so such a file stays as it was.
        let written = serde_json::to_string(&settings).unwrap();
        assert!(!written.contains("templates"), "{written}");
    }

    #[test]
    fn a_kept_set_reads_back() {
        let settings = NotificationSettings {
            templates: Some(set("en/1")),
            ..read(OLDER)
        };
        let written = serde_json::to_vec_pretty(&settings).unwrap();
        let back: NotificationSettings = serde_json::from_slice(&written).unwrap();
        assert_eq!(back, settings);
    }

    #[test]
    fn a_set_that_does_not_read_as_one_is_none_and_the_rest_still_loads() {
        let mut good = serde_json::to_value(NotificationSettings {
            templates: Some(set("en/1")),
            ..read(OLDER)
        })
        .unwrap();
        let mut bad = Vec::new();
        for (field, value) in [
            // Leaves out a placeholder it must use.
            ("formal", serde_json::json!("{device} {tunnel}")),
            // A placeholder that is not its own.
            ("test", serde_json::json!("test {device} {time} {token}")),
            ("id", serde_json::json!("../x")),
            ("formal", serde_json::json!(7)),
        ] {
            let mut value_of = good.clone();
            value_of["templates"][field] = value;
            bad.push(value_of);
        }
        let mut extra = good.clone();
        extra["templates"]["chat_id"] = "-1".into();
        bad.push(extra);
        let mut not_a_set = good.clone();
        not_a_set["templates"] = serde_json::json!(["x"]);
        bad.push(not_a_set);
        let mut null = good.clone();
        null["templates"] = serde_json::Value::Null;
        bad.push(null);
        for value in bad {
            let settings: NotificationSettings = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(settings.templates, None, "{value}");
            assert!(settings.enabled);
            assert_eq!(
                settings.chat_id,
                Some(ChatId::new("-1001234567890").unwrap())
            );
        }
        // The valid one still reads.
        good["enabled"] = false.into();
        let settings: NotificationSettings = serde_json::from_value(good).unwrap();
        assert_eq!(settings.templates, Some(set("en/1")));
    }

    /// The largest settings, with the largest set in the characters JSON
    /// writes longest, fit in the file with room to spare.
    #[test]
    fn the_largest_settings_fit_in_the_file() {
        let fill = |used: &str, limit: usize| format!("{used}{}", "\"".repeat(limit - used.len()));
        let max = MessageTemplate::MAX_BYTES;
        let result = "\\".repeat(max);
        let largest = NotificationTemplates::new(
            TemplatesId::new("a".repeat(TemplatesId::MAX_BYTES)).unwrap(),
            TemplateTexts {
                formal: template(&fill(
                    "{device}{tunnel}{incident}{time_zone}{started_at}{result}{retry}",
                    max,
                )),
                result_failed: template(&result),
                result_not_executed: template(&result),
                retry_limited: template(&fill("{interval}{limit}", max)),
                retry_forever: template(&fill("{interval}", max)),
                retry_none: template(&result),
                exhausted: template(&fill(
                    "{device}{tunnel}{incident}{time_zone}{started_at}{retries}",
                    max,
                )),
                recovered: template(&fill(
                    "{device}{tunnel}{incident}{time_zone}{started_at}{recovered_at}",
                    max,
                )),
                test: template(&fill("{device}{time_zone}{time}", max)),
                unnamed_device: template(
                    &"\"".repeat(NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES),
                ),
            },
        )
        .unwrap();
        let settings = NotificationSettings {
            schema_version: SETTINGS_VERSION,
            enabled: true,
            device_name: DeviceName::new("\u{1F600}".repeat(DeviceName::MAX_CHARS)).unwrap(),
            chat_id: Some(ChatId::new(format!("@{}", "a".repeat(ChatId::MAX_BYTES - 1))).unwrap()),
            token_set: true,
            templates: Some(largest),
            cli_language: Some(LanguageTag::new("l".repeat(LanguageTag::MAX_BYTES)).unwrap()),
        };
        let written = serde_json::to_vec_pretty(&settings).unwrap();
        assert!(written.len() < 24 * 1024, "{} bytes", written.len());
        assert!(written.len() <= MAX_SETTINGS_BYTES);
        let back: NotificationSettings = serde_json::from_slice(&written).unwrap();
        assert_eq!(back, settings);
    }

    /// `notify.json` as watchdog 3.0.0 reads it: the same fields, refusing
    /// any other (`deny_unknown_fields`), without `cli_language`. Kept here
    /// to show what a rollback below 4.0.0 meets (owner decision of
    /// 2026-11-19, the plan's §5).
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct ReadBy300 {
        schema_version: u32,
        enabled: bool,
        device_name: String,
        chat_id: Option<String>,
        #[serde(default)]
        token_set: bool,
        #[serde(default)]
        templates: Option<serde_json::Value>,
    }

    fn language(tag: &str) -> Option<LanguageTag> {
        Some(LanguageTag::new(tag).unwrap())
    }

    #[test]
    fn a_shared_language_reads_back_and_none_is_not_written() {
        let settings = NotificationSettings {
            templates: Some(set("ja/1")),
            cli_language: language("ja"),
            ..read(OLDER)
        };
        let written = serde_json::to_vec_pretty(&settings).unwrap();
        let back: NotificationSettings = serde_json::from_slice(&written).unwrap();
        assert_eq!(back, settings);

        let older = read(OLDER);
        assert_eq!(older.cli_language, None);
        let written = serde_json::to_string(&older).unwrap();
        assert!(!written.contains("cli_language"), "{written}");
    }

    #[test]
    fn a_stored_language_that_is_not_a_tag_is_none_and_the_rest_still_loads() {
        let good = serde_json::to_value(NotificationSettings {
            cli_language: language("de"),
            ..read(OLDER)
        })
        .unwrap();
        for value in [
            serde_json::json!("../de"),
            serde_json::json!("zh--TW"),
            serde_json::json!(""),
            serde_json::json!(7),
            serde_json::json!(["de"]),
            serde_json::Value::Null,
        ] {
            let mut file = good.clone();
            file["cli_language"] = value.clone();
            let settings: NotificationSettings = serde_json::from_value(file).unwrap();
            assert_eq!(settings.cli_language, None, "{value}");
            assert!(settings.enabled);
            assert_eq!(
                settings.chat_id,
                Some(ChatId::new("-1001234567890").unwrap())
            );
        }
        let settings: NotificationSettings = serde_json::from_value(good).unwrap();
        assert_eq!(settings.cli_language, language("de"));
    }

    /// A rollback below 4.0.0 (the plan's §5): a file written without a
    /// shared language (never set, or cleared with `wgal language --clear`)
    /// still reads in 3.0.0; one with it does not, and 3.0.0 then keeps
    /// notifications off.
    #[test]
    fn watchdog_3_0_0_reads_a_file_without_a_shared_language_only() {
        let without = NotificationSettings {
            templates: Some(set("ja/1")),
            ..read(OLDER)
        };
        let written = serde_json::to_string_pretty(&without).unwrap();
        assert!(
            serde_json::from_str::<ReadBy300>(&written).is_ok(),
            "{written}"
        );

        let cleared = NotificationSettings {
            cli_language: None,
            ..NotificationSettings {
                cli_language: language("ja"),
                ..without.clone()
            }
        };
        let written = serde_json::to_string_pretty(&cleared).unwrap();
        assert!(
            serde_json::from_str::<ReadBy300>(&written).is_ok(),
            "{written}"
        );

        let with = NotificationSettings {
            cli_language: language("ja"),
            ..without
        };
        let written = serde_json::to_string_pretty(&with).unwrap();
        assert!(
            serde_json::from_str::<ReadBy300>(&written).is_err(),
            "{written}"
        );
    }
}
