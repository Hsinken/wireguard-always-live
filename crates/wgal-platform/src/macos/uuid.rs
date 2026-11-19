use std::fmt;

use wgal_core::{AdapterError, TunnelBinding};
use wgal_protocol::AdapterErrorKind;

/// The ID of a VPN configuration as `scutil --nc` prints it: a UUID in the
/// 8-4-4-4-12 form, kept in upper case. It is the persistent binding of a
/// macOS tunnel; interface names such as `utun4` are not.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VpnUuid(String);

impl VpnUuid {
    /// Accepts exactly 36 characters: hex digits with `-` at positions 8, 13,
    /// 18 and 23, in either case. Anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 36 {
            return None;
        }
        let well_formed = bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
        well_formed.then(|| Self(text.to_ascii_uppercase()))
    }

    /// A binding that is not a UUID cannot name a configuration of the
    /// official app, so it is reported like a configuration that is gone.
    pub fn from_binding(binding: &TunnelBinding) -> Result<Self, AdapterError> {
        Self::parse(binding.as_str()).ok_or_else(|| {
            AdapterError::new(
                AdapterErrorKind::NotFound,
                "binding is not a macOS VPN configuration UUID",
            )
        })
    }

    pub fn to_binding(&self) -> TunnelBinding {
        TunnelBinding::new(self.0.clone()).expect("a canonical UUID is a valid binding")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VpnUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_uuids_in_either_case_and_keeps_upper_case() {
        let uuid = VpnUuid::parse("00000000-0000-4000-8000-0000000000b1").unwrap();
        assert_eq!(uuid.as_str(), "00000000-0000-4000-8000-0000000000B1");
        assert_eq!(uuid.to_binding().as_str(), uuid.as_str());
    }

    #[test]
    fn rejects_anything_that_is_not_exactly_a_uuid() {
        for bad in [
            "",
            "00000000-0000-4000-8000-0000000000B",
            "00000000-0000-4000-8000-0000000000B10",
            "00000000000040008000-0000000000B1-----",
            "0000000-00000-4000-8000-0000000000B1",
            "00000000-0000-4000-8000-0000000000BG",
            "{0000000-0000-4000-8000-0000000000B1}",
            "office",
            "00000000-0000-4000-8000-00000000;rm",
        ] {
            assert!(VpnUuid::parse(bad).is_none(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn a_non_uuid_binding_is_not_found() {
        let binding = TunnelBinding::new("wg0").unwrap();
        let error = VpnUuid::from_binding(&binding).unwrap_err();
        assert_eq!(error.kind, AdapterErrorKind::NotFound);
    }
}
