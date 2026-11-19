use std::fmt;

use wgal_core::{AdapterError, TunnelBinding};
use wgal_protocol::AdapterErrorKind;

/// Longest interface name the kernel accepts (`IFNAMSIZ - 1`).
pub const MAX_INTERFACE_LEN: usize = 15;

/// A wg-quick interface name that is safe to put in an argument vector and
/// in a systemd unit name.
///
/// wg-quick itself accepts `[A-Za-z0-9_=+.-]{1,15}`. This is narrower:
///
/// - `=` and `+` are not valid in systemd unit names, so such an interface
///   cannot have a `wg-quick@<interface>.service` unit that passes it to
///   wg-quick unchanged;
/// - a leading `-` would read as an option to `ip`, `ping` or `wg`;
/// - `all` and `interfaces` are keywords of `wg show`;
/// - `.` and `..` are not interface names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceName(String);

impl InterfaceName {
    pub fn new(value: &str) -> Option<Self> {
        let valid = !value.is_empty()
            && value.len() <= MAX_INTERFACE_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            && !value.starts_with('-')
            && !matches!(value, "." | ".." | "all" | "interfaces");
        valid.then(|| Self(value.to_owned()))
    }

    /// The interface named by `binding`. A binding that cannot name a wg-quick
    /// interface is reported as `NotFound`: no such tunnel exists here.
    pub fn from_binding(binding: &TunnelBinding) -> Result<Self, AdapterError> {
        Self::new(binding.as_str()).ok_or_else(|| {
            AdapterError::new(
                AdapterErrorKind::NotFound,
                "binding is not a wg-quick interface name",
            )
        })
    }

    /// The interface named by a `wg-quick@<interface>.service` unit, if it is
    /// one and its instance is a valid name.
    pub fn from_unit(unit: &str) -> Option<Self> {
        unit.strip_prefix("wg-quick@")?
            .strip_suffix(".service")
            .and_then(Self::new)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn unit(&self) -> String {
        format!("wg-quick@{}.service", self.0)
    }

    /// `/etc/wireguard/<interface>.conf`, the file wg-quick reads for the unit.
    pub fn config_path(&self) -> String {
        format!("/etc/wireguard/{}.conf", self.0)
    }

    pub fn binding(&self) -> TunnelBinding {
        TunnelBinding::new(self.0.clone())
            .unwrap_or_else(|e| unreachable!("interface names are valid bindings: {e}"))
    }
}

impl fmt::Display for InterfaceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_wg_quick_names() {
        for ok in ["wg0", "wg-office", "a.b_c-d", "x", "abcdefghijklmno"] {
            let name = InterfaceName::new(ok).unwrap();
            assert_eq!(name.binding().as_str(), ok);
        }
        let name = InterfaceName::new("wg-office").unwrap();
        assert_eq!(name.unit(), "wg-quick@wg-office.service");
        assert_eq!(name.config_path(), "/etc/wireguard/wg-office.conf");
    }

    #[test]
    fn rejects_names_unsafe_for_argv_or_units() {
        for bad in [
            "",
            ".",
            "..",
            "all",
            "interfaces",
            "-h",
            "--help",
            "abcdefghijklmnop",
            "a=b",
            "a+b",
            "wg 0",
            "wg0;id",
            "wg/0",
            "wg\\x2d0",
            "wg0\n",
            "wg\u{fc}",
        ] {
            assert!(
                InterfaceName::new(bad).is_none(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn binding_errors_are_not_found() {
        let binding = TunnelBinding::new("a=b").unwrap();
        let error = InterfaceName::from_binding(&binding).unwrap_err();
        assert_eq!(error.kind, AdapterErrorKind::NotFound);
    }

    #[test]
    fn reads_interface_names_from_units() {
        let from = |unit| InterfaceName::from_unit(unit).map(|n| n.to_string());
        assert_eq!(from("wg-quick@wg-lab.service").as_deref(), Some("wg-lab"));
        assert_eq!(from("wg-quick@.service"), None);
        assert_eq!(from("wg-quick@a\\x3db.service"), None);
        assert_eq!(from("wg-quick@wg0.socket"), None);
        assert_eq!(from("wireguard@wg0.service"), None);
    }
}
