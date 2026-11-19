use std::fmt;

use serde::{Deserialize, Serialize};

/// A string was rejected as contract text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidText {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for InvalidText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for InvalidText {}

// Text ends up in the GUI and in logs, so control characters are rejected: they
// could forge log lines or terminal output. Without them, only `"` and `\` need
// escaping in JSON, so a value's encoded form is at most twice its byte length,
// which the message size budget relies on.
fn validate(kind: &'static str, max_bytes: usize, value: &str) -> Result<(), InvalidText> {
    let reason = if value.len() > max_bytes {
        "too long"
    } else if value.chars().any(char::is_control) {
        "control characters are not allowed"
    } else {
        return Ok(());
    };
    Err(InvalidText { kind, reason })
}

fn sanitize(max_bytes: usize, value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(max_bytes));
    for c in value.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

macro_rules! define_text {
    ($(#[$meta:meta])* $name:ident, $kind:literal, $max_bytes:expr) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(test, derive(ts_rs::TS), ts(export))]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Maximum length in UTF-8 bytes.
            pub const MAX_BYTES: usize = $max_bytes;

            pub fn new(value: impl Into<String>) -> Result<Self, InvalidText> {
                let value = value.into();
                validate($kind, Self::MAX_BYTES, &value)?;
                Ok(Self(value))
            }

            /// Accepts any text by replacing control characters with spaces and
            /// cutting it to [`Self::MAX_BYTES`] at a character boundary. Meant
            /// for text read from the OS or another untrusted source.
            pub fn lossy(value: &str) -> Self {
                Self(sanitize(Self::MAX_BYTES, value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidText;

            fn try_from(value: String) -> Result<Self, InvalidText> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(text: $name) -> String {
                text.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_text!(
    /// Name of a tunnel as shown to the user, as the official manager reports it.
    DisplayName,
    "display name",
    96
);

define_text!(
    /// Human-readable detail for an error or fault. Must never contain secrets
    /// such as keys or tokens.
    Detail,
    "detail",
    256
);

/// The name formal messages give this computer
/// (`Request::UpdateNotificationSettings`): any text the user likes, of at
/// most [`Self::MAX_CHARS`] characters (owner decision of 2026-11-06).
/// Only control characters are refused, as in every contract text (see
/// [`validate`]); [`Self::fitted`] turns them into spaces and cuts a longer
/// text, so whatever an agent gives can be kept.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "String", into = "String")]
pub struct DeviceName(String);

impl DeviceName {
    /// Most characters a device name has.
    pub const MAX_CHARS: usize = 64;
    /// Longest in UTF-8 bytes: four for each character.
    pub const MAX_BYTES: usize = 4 * Self::MAX_CHARS;
    /// What ends a name [`Self::fitted`] had to cut.
    pub const CUT_MARK: &'static str = "...";

    pub fn new(value: impl Into<String>) -> Result<Self, InvalidText> {
        let value = value.into();
        let reason = if value.chars().count() > Self::MAX_CHARS {
            "more than 64 characters"
        } else if value.chars().any(char::is_control) {
            "control characters are not allowed"
        } else {
            return Ok(Self(value));
        };
        Err(InvalidText {
            kind: "device name",
            reason,
        })
    }

    /// Any text made a device name: control characters become spaces, and a
    /// text of more than [`Self::MAX_CHARS`] characters is cut to
    /// `MAX_CHARS - 3` of them and [`Self::CUT_MARK`].
    pub fn fitted(value: &str) -> Self {
        let clean: String = value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if clean.chars().count() <= Self::MAX_CHARS {
            return Self(clean);
        }
        let mut cut: String = clean
            .chars()
            .take(Self::MAX_CHARS - Self::CUT_MARK.len())
            .collect();
        cut.push_str(Self::CUT_MARK);
        Self(cut)
    }

    /// Whether [`Self::fitted`] cuts `value`.
    pub fn too_long(value: &str) -> bool {
        value.chars().count() > Self::MAX_CHARS
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeviceName {
    type Error = InvalidText;

    fn try_from(value: String) -> Result<Self, InvalidText> {
        Self::new(value)
    }
}

impl From<DeviceName> for String {
    fn from(name: DeviceName) -> String {
        name.0
    }
}

impl fmt::Display for DeviceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_name_is_any_text_of_up_to_64_characters() {
        // Markup, joiners, selectors and emoji are kept as they are.
        for name in [
            "",
            "<b>Office</b>",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{2764}\u{FE0F}",
            "می\u{200C}خواهم",
            "\u{202E}abc",
        ] {
            assert_eq!(DeviceName::new(name).unwrap().as_str(), name);
            assert_eq!(DeviceName::fitted(name).as_str(), name);
        }
        let full = "\u{1F600}".repeat(64);
        assert_eq!(full.len(), DeviceName::MAX_BYTES);
        assert!(DeviceName::new(full.as_str()).is_ok());
        assert!(DeviceName::new(full + "x").is_err());
        assert!(DeviceName::new("a\nb").is_err());
    }

    #[test]
    fn a_device_name_is_fitted_by_cutting_and_spacing() {
        let long = "測".repeat(70);
        assert!(DeviceName::too_long(&long));
        let cut = DeviceName::fitted(&long);
        assert_eq!(cut.as_str(), format!("{}...", "測".repeat(61)));
        assert_eq!(cut.as_str().chars().count(), 64);
        assert!(DeviceName::new(cut.as_str()).is_ok());
        assert!(!DeviceName::too_long(&"x".repeat(64)));
        assert_eq!(DeviceName::fitted(&"x".repeat(64)).as_str(), "x".repeat(64));
        assert_eq!(DeviceName::fitted("a\nb\tc").as_str(), "a b c");
    }

    #[test]
    fn accepts_text_up_to_the_byte_limit() {
        // 32 three-byte characters fill the limit exactly.
        let full = "測".repeat(DisplayName::MAX_BYTES / 3);
        assert_eq!(full.len(), DisplayName::MAX_BYTES);
        assert!(DisplayName::new(full.clone()).is_ok());
        assert!(DisplayName::new(full + "x").is_err());
        assert!(DisplayName::new("").is_ok());
    }

    #[test]
    fn rejects_control_characters() {
        for bad in [
            "a\nb",
            "a\rb",
            "tab\there",
            "nul\0",
            "del\u{7f}",
            "c1\u{85}",
        ] {
            assert!(Detail::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn lossy_replaces_controls_and_cuts_at_a_character_boundary() {
        assert_eq!(Detail::lossy("line 1\nline 2").as_str(), "line 1 line 2");

        let long = "測".repeat(40);
        let cut = DisplayName::lossy(&long);
        assert_eq!(cut.as_str(), "測".repeat(32));
        assert!(DisplayName::new(cut.as_str()).is_ok());
    }

    #[test]
    fn deserialization_applies_validation() {
        assert!(serde_json::from_str::<Detail>(r#""fine""#).is_ok());
        assert!(serde_json::from_str::<Detail>(r#""bad\nline""#).is_err());
        let too_long = format!("\"{}\"", "x".repeat(Detail::MAX_BYTES + 1));
        assert!(serde_json::from_str::<Detail>(&too_long).is_err());
    }
}
