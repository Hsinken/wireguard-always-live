use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum length, in bytes, of a [`TunnelId`] or [`RequestId`].
pub const MAX_ID_LEN: usize = 64;

/// A string was rejected as an identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidId {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for InvalidId {}

// Identifiers cross a privilege boundary and may end up in logs or argv, so they
// are restricted to a short ASCII alphabet with no separators or shell syntax.
fn validate(kind: &'static str, value: &str) -> Result<(), InvalidId> {
    let reason = if value.is_empty() {
        "must not be empty"
    } else if value.len() > MAX_ID_LEN {
        "must be at most 64 bytes"
    } else if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        "only ASCII letters, digits, '-' and '_' are allowed"
    } else {
        return Ok(());
    };
    Err(InvalidId { kind, reason })
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(test, derive(ts_rs::TS), ts(export))]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                validate($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidId;

            fn try_from(value: String) -> Result<Self, InvalidId> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_id!(
    /// Stable identifier of a tunnel registered with the watchdog. It is assigned
    /// by the watchdog and is not the platform's interface or service name.
    TunnelId,
    "tunnel id"
);

define_id!(
    /// Caller-chosen identifier that pairs a response with its request.
    RequestId,
    "request id"
);

define_id!(
    /// Identifier of one incident on one tunnel, assigned by the watchdog when
    /// a fault is confirmed and kept until the tunnel has been stably healthy
    /// for the closing period (plan §7.2). Deduplicates notifications.
    IncidentId,
    "incident id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_identifiers() {
        for ok in ["a", "office-1", "tunnel_A", &"x".repeat(MAX_ID_LEN)] {
            assert!(TunnelId::new(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn rejects_empty_long_and_non_alphabet_identifiers() {
        let too_long = "x".repeat(MAX_ID_LEN + 1);
        let rejected = [
            "",
            too_long.as_str(),
            "wg0; rm -rf /",
            "$(id)",
            "../etc/passwd",
            "WireGuardTunnel$office",
            "a b",
            "tünnel",
        ];
        for bad in rejected {
            assert!(TunnelId::new(bad).is_err(), "{bad:?} should be rejected");
            assert!(RequestId::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn deserialization_applies_validation() {
        assert!(serde_json::from_str::<TunnelId>(r#""office-1""#).is_ok());
        assert!(serde_json::from_str::<TunnelId>(r#""a/b""#).is_err());
    }
}
