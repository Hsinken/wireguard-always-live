//! Tunnel names as WireGuard for Windows defines them, and the one service
//! name each maps to.

use std::fmt;

use wgal_core::{AdapterError, TunnelBinding};
use wgal_protocol::AdapterErrorKind;

/// Longest tunnel name WireGuard for Windows accepts, in bytes.
pub const MAX_TUNNEL_NAME_LEN: usize = 32;

/// Prefix of every tunnel service the official manager installs. Matched
/// exactly; WGAL never looks at any other service.
pub const SERVICE_PREFIX: &str = "WireGuardTunnel$";

/// Names Windows reserves for devices. WireGuard for Windows refuses them as
/// tunnel names, so they can never belong to a tunnel.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A WireGuard for Windows tunnel name: `[A-Za-z0-9_=+.-]{1,32}`, not a
/// reserved device name, and not `.` or `..` (never a tunnel's file name,
/// and not a [`TunnelBinding`]). It is the [`TunnelBinding`] on Windows and
/// the only value that ever reaches a service name or an adapter lookup.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TunnelName(String);

impl TunnelName {
    pub fn new(value: &str) -> Option<Self> {
        let valid = !value.is_empty()
            && value.len() <= MAX_TUNNEL_NAME_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_=+.-".contains(&b))
            && value != "."
            && value != ".."
            && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(value));
        valid.then(|| Self(value.to_owned()))
    }

    /// The tunnel named by `binding`. A binding that is not a valid tunnel
    /// name cannot have a service, so it is reported like a removed one.
    pub fn from_binding(binding: &TunnelBinding) -> Result<Self, AdapterError> {
        Self::new(binding.as_str()).ok_or_else(|| {
            AdapterError::new(
                AdapterErrorKind::NotFound,
                "not a WireGuard for Windows tunnel name; register the tunnel again",
            )
        })
    }

    /// The tunnel named by a service the SCM listed, if it is one of the
    /// official manager's tunnel services with a valid name.
    pub fn from_service_name(service: &str) -> Option<Self> {
        service.strip_prefix(SERVICE_PREFIX).and_then(Self::new)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn service_name(&self) -> ServiceName {
        ServiceName(format!("{SERVICE_PREFIX}{}", self.0))
    }

    pub fn binding(&self) -> TunnelBinding {
        // Every tunnel name is within TunnelBinding's alphabet and length.
        TunnelBinding::new(self.0.clone())
            .unwrap_or_else(|e| unreachable!("tunnel name {:?}: {e}", self.0))
    }
}

impl fmt::Display for TunnelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `WireGuardTunnel$<name>` for a validated [`TunnelName`]. The only way to
/// name a service to [`ServiceControl`](super::ServiceControl), so no other
/// service can be opened through it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceName(String);

impl ServiceName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_official_alphabet() {
        for ok in [
            "office",
            "lab-2",
            "a=b+c.d_e",
            "A",
            &"x".repeat(MAX_TUNNEL_NAME_LEN),
        ] {
            let name = TunnelName::new(ok).unwrap();
            assert_eq!(
                name.service_name().as_str(),
                format!("WireGuardTunnel${ok}")
            );
            assert_eq!(name.binding().as_str(), ok);
        }
    }

    #[test]
    fn rejects_everything_else() {
        let too_long = "x".repeat(MAX_TUNNEL_NAME_LEN + 1);
        let rejected = [
            "",
            too_long.as_str(),
            "WireGuardTunnel$office",
            "office$",
            "has space",
            "a/b",
            "a\\b",
            "..\\x",
            "C:office",
            "office;calc",
            "\u{5be6}\u{9a57}\u{5ba4}",
            "t\u{fc}nnel",
            "tab\there",
            "CON",
            "nul",
            "Com1",
            "lpt9",
            ".",
            "..",
        ];
        for bad in rejected {
            assert!(TunnelName::new(bad).is_none(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn reserved_names_are_only_rejected_whole() {
        assert!(TunnelName::new("CONSOLE").is_some());
        assert!(TunnelName::new("com10").is_some());
        assert!(TunnelName::new("...").is_some());
    }

    #[test]
    fn every_accepted_name_is_a_binding() {
        // `binding()` must never panic: "." and ".." are refused up front, and
        // a listed service named `WireGuardTunnel$.` is skipped, not a crash.
        for name in ["...", ".a", "a.", "-", "=", "+", "_"] {
            assert_eq!(TunnelName::new(name).unwrap().binding().as_str(), name);
        }
        assert!(TunnelName::from_service_name("WireGuardTunnel$.").is_none());
        assert!(TunnelName::from_service_name("WireGuardTunnel$..").is_none());
    }

    #[test]
    fn only_exact_tunnel_service_names_map_to_tunnels() {
        assert_eq!(
            TunnelName::from_service_name("WireGuardTunnel$office").map(|n| n.0),
            Some("office".to_owned())
        );
        for other in [
            "WireGuardManager",
            "WireGuardTunnel$",
            "wireguardtunnel$office",
            "WireGuardTunnel$bad name",
            "WireGuardTunnel$a$b",
            "XWireGuardTunnel$office",
            "Spooler",
        ] {
            assert!(TunnelName::from_service_name(other).is_none(), "{other:?}");
        }
    }

    #[test]
    fn bindings_outside_the_alphabet_read_as_lost() {
        // Valid as a binding on other platforms, but never a Windows tunnel.
        let binding = TunnelBinding::new("x".repeat(40)).unwrap();
        let error = TunnelName::from_binding(&binding).unwrap_err();
        assert_eq!(error.kind, AdapterErrorKind::NotFound);
        let reserved = TunnelBinding::new("AUX").unwrap();
        assert!(TunnelName::from_binding(&reserved).is_err());
    }
}
