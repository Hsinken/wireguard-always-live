use std::fmt;
use std::net::Ipv4Addr;
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

use crate::adapter::AdapterErrorKind;

/// Most observe-only targets per tunnel, besides its one recovery target.
pub const MAX_OBSERVE_TARGETS: usize = 4;

/// An address was rejected as a probe target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidTarget {
    reason: &'static str,
}

impl fmt::Display for InvalidTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid probe target: {}", self.reason)
    }
}

impl std::error::Error for InvalidTarget {}

/// IPv4 literal to probe through a tunnel. Version 1 probes IPv4 only and
/// never resolves names, so DNS success cannot pass for VPN reachability.
/// Addresses that cannot identify a host behind a tunnel are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(try_from = "Ipv4Addr", into = "Ipv4Addr")]
pub struct TargetAddress(Ipv4Addr);

impl TargetAddress {
    pub fn new(addr: Ipv4Addr) -> Result<Self, InvalidTarget> {
        let reason = if addr.is_unspecified() {
            "unspecified address"
        } else if addr.is_loopback() {
            "loopback address"
        } else if addr.is_multicast() {
            "multicast address"
        } else if addr.is_broadcast() {
            "broadcast address"
        } else if addr.is_link_local() {
            "link-local address"
        } else {
            return Ok(Self(addr));
        };
        Err(InvalidTarget { reason })
    }

    pub const fn get(self) -> Ipv4Addr {
        self.0
    }
}

impl TryFrom<Ipv4Addr> for TargetAddress {
    type Error = InvalidTarget;

    fn try_from(addr: Ipv4Addr) -> Result<Self, InvalidTarget> {
        Self::new(addr)
    }
}

impl From<TargetAddress> for Ipv4Addr {
    fn from(target: TargetAddress) -> Ipv4Addr {
        target.0
    }
}

impl fmt::Display for TargetAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// ICMP echo is the default; `TcpConnect` succeeds when the service accepts
/// the connection. A refused connection says the service is down, not that the
/// tunnel is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProbeKind {
    Icmp,
    TcpConnect { port: NonZeroU16 },
}

/// One address and method to probe through a tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(deny_unknown_fields)]
pub struct ProbeTarget {
    pub address: TargetAddress,
    pub probe: ProbeKind,
}

/// A configured target and the latest probe of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TargetStatus {
    pub target: ProbeTarget,
    pub last_probe: Option<ProbeSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ProbeSummary {
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at_unix_ms: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub finished_at_unix_ms: i64,
    pub result: ProbeResult,
}

/// `Error` means the probe itself could not run (missing tool, no permission,
/// unparseable output). It is never folded into `NoReply`, so a broken probe is
/// not mistaken for an unreachable target, and a failed probe has no latency.
///
/// `Refused` comes from TCP probes only: the target answered through the
/// tunnel but refused the connection. The path works and the service does not
/// (plan §4.3), so it is shown as a service problem, never as a tunnel fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProbeResult {
    Reply { rtt_ms: u32 },
    Refused { rtt_ms: u32 },
    NoReply,
    Error { error: AdapterErrorKind },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_unicast_targets_including_documentation_ranges() {
        for ok in ["192.0.2.1", "198.51.100.7", "203.0.113.9", "10.0.0.1"] {
            assert!(TargetAddress::new(ok.parse().unwrap()).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_addresses_that_cannot_be_a_tunnel_host() {
        for bad in [
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "169.254.1.1",
        ] {
            assert!(TargetAddress::new(bad.parse().unwrap()).is_err(), "{bad}");
        }
    }

    #[test]
    fn deserialization_applies_validation() {
        assert!(serde_json::from_str::<TargetAddress>(r#""192.0.2.1""#).is_ok());
        assert!(serde_json::from_str::<TargetAddress>(r#""127.0.0.1""#).is_err());
        assert!(serde_json::from_str::<TargetAddress>(r#""example.com""#).is_err());
        assert!(serde_json::from_str::<TargetAddress>(r#""2001:db8::1""#).is_err());
    }

    #[test]
    fn tcp_port_zero_is_rejected() {
        assert!(serde_json::from_str::<ProbeKind>(r#"{"kind":"tcp_connect","port":443}"#).is_ok());
        assert!(serde_json::from_str::<ProbeKind>(r#"{"kind":"tcp_connect","port":0}"#).is_err());
    }
}
