//! The IP Helper boundary: adapters, best routes and ICMP echoes, and how
//! their answers map to adapter results.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::Deserialize;
use wgal_core::{AdapterError, ProbeOutcome};
use wgal_protocol::AdapterErrorKind;

use super::local::DefaultRoute;

/// Most adapters one enumeration may return; more fails instead of truncating.
pub const MAX_ADAPTERS: usize = 1024;

/// Most addresses kept per adapter and family; more fails instead of
/// truncating.
pub const MAX_ADDRESSES: usize = 64;

/// The IP Helper API, reduced to what the adapter needs. Every call reads
/// state, sends one echo or asks one neighbor for its link-layer address;
/// none adds, changes or deletes an address, route, interface or static
/// neighbor entry.
pub trait IpHelper: Send + Sync + fmt::Debug {
    /// Every network adapter (`GetAdaptersAddresses`, IPv4 only). Its
    /// `ipv6` is always empty. This is what tunnel adapters, routes and
    /// probes are checked against.
    fn adapters(&self) -> Result<Vec<AdapterInfo>, NetError>;

    /// Every network adapter with its IPv4 and IPv6 addresses
    /// (`GetAdaptersAddresses` for both families). Read only to judge
    /// whether this host itself is offline (see [`local`](super::local)).
    fn adapters_any_family(&self) -> Result<Vec<AdapterInfo>, NetError>;

    /// The route Windows would pick for `destination` from any interface and
    /// any source (`GetBestRoute2` with no constraint), and its source address.
    fn best_route(&self, destination: Ipv4Addr) -> Result<BestRoute, NetError>;

    /// One ICMP echo from `source` (`IcmpSendEcho2Ex`), waiting at most
    /// `timeout`. An echo that got no reply is `Ok` with its IP status.
    fn echo(
        &self,
        source: Ipv4Addr,
        destination: Ipv4Addr,
        timeout: Duration,
    ) -> Result<EchoReply, NetError>;

    /// Every default route, IPv4 (`0.0.0.0/0`) and IPv6 (`::/0`), in the
    /// routing table (`GetIpForwardTable2` for both families), at most
    /// [`MAX_DEFAULT_ROUTES`] of them.
    ///
    /// [`MAX_DEFAULT_ROUTES`]: super::local::MAX_DEFAULT_ROUTES
    fn default_routes(&self) -> Result<Vec<DefaultRoute>, NetError>;

    /// Whether the interface `luid` has a static (permanent) neighbor entry
    /// for `address` (`GetIpNetEntry2`, read only). No entry is `Ok(false)`.
    fn neighbor_is_static(&self, luid: u64, address: IpAddr) -> Result<bool, NetError>;

    /// Asks `address` on the interface `luid` for its link-layer address
    /// afresh (`ResolveIpNetEntry2`): Windows drops its dynamic neighbor
    /// entry for that address and sends ARP requests (IPv4) or neighbor
    /// solicitations (IPv6), as its own neighbor unreachability detection
    /// does. `Ok(true)` if it answered, `Ok(false)` if it did not. Blocks
    /// until Windows gives up, which can take seconds. Never called for an
    /// address with a static entry: resolving would replace it.
    fn resolve_neighbor(&self, luid: u64, address: IpAddr) -> Result<bool, NetError>;
}

/// `IfType` of the virtual adapters WireGuard for Windows creates
/// (`IF_TYPE_PROP_VIRTUAL`). To be confirmed on a device (W9 in
/// `docs/testing.md`).
pub const IF_TYPE_PROP_VIRTUAL: u32 = 53;

/// One network adapter. `friendly_name` is the interface alias, which
/// WireGuard for Windows sets to the tunnel name; `if_type` is its interface
/// type; `if_index` and `luid` are reassigned when the adapter is recreated.
/// `up` is whether its operational status is up. `ipv4` and `ipv6` are its
/// unicast addresses; `ipv6` is filled only by
/// [`IpHelper::adapters_any_family`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterInfo {
    pub friendly_name: String,
    pub if_type: u32,
    pub if_index: u32,
    pub luid: u64,
    pub up: bool,
    pub ipv4: Vec<Ipv4Addr>,
    #[serde(default)]
    pub ipv6: Vec<Ipv6Addr>,
}

/// The interface and source address of the best route to a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BestRoute {
    pub if_index: u32,
    pub luid: u64,
    pub source: Ipv4Addr,
}

/// What came back for one echo. `status` is the IP status (`IP_SUCCESS` or an
/// `IP_*` code), `from` the replying address, `payload_matches` whether the
/// reply carried exactly the data this echo sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EchoReply {
    pub status: u32,
    pub from: Ipv4Addr,
    pub rtt_ms: u32,
    pub payload_matches: bool,
}

/// A failed IP Helper call, by its Win32 error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetError {
    pub code: u32,
}

impl NetError {
    pub const ACCESS_DENIED: Self = Self { code: 5 };
    pub const NOT_ENOUGH_MEMORY: Self = Self { code: 8 };
    /// `ResolveIpNetEntry2`: the neighbor did not answer.
    pub const BAD_NET_NAME: Self = Self { code: 67 };
    pub const INVALID_PARAMETER: Self = Self { code: 87 };
    pub const BUFFER_OVERFLOW: Self = Self { code: 111 };
    pub const NOT_FOUND: Self = Self { code: 1168 };
    pub const NETWORK_UNREACHABLE: Self = Self { code: 1231 };

    /// The adapter error for a failed call. No route at all to a target is a
    /// route mismatch: it does not leave through the tunnel. Anything else
    /// means the probe machinery itself failed, which never counts as the
    /// target being down (plan §4.1, T23).
    pub fn to_adapter_error(self, what: &str) -> AdapterError {
        let (kind, detail) = match self {
            Self::ACCESS_DENIED => (AdapterErrorKind::PermissionDenied, "was not permitted"),
            Self::NOT_FOUND | Self::NETWORK_UNREACHABLE => {
                (AdapterErrorKind::RouteMismatch, "found no route")
            }
            _ => (AdapterErrorKind::Unsupported, "failed"),
        };
        AdapterError::new(kind, &format!("{what} {detail} (error {})", self.code))
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IP Helper error {}", self.code)
    }
}

impl std::error::Error for NetError {}

/// IP status codes (`ipexport.h`).
pub mod status {
    pub const SUCCESS: u32 = 0;
    pub const BUF_TOO_SMALL: u32 = 11001;
    pub const DEST_NET_UNREACHABLE: u32 = 11002;
    pub const DEST_HOST_UNREACHABLE: u32 = 11003;
    pub const DEST_PROT_UNREACHABLE: u32 = 11004;
    pub const DEST_PORT_UNREACHABLE: u32 = 11005;
    pub const NO_RESOURCES: u32 = 11006;
    pub const BAD_OPTION: u32 = 11007;
    pub const HW_ERROR: u32 = 11008;
    pub const PACKET_TOO_BIG: u32 = 11009;
    pub const REQ_TIMED_OUT: u32 = 11010;
    pub const BAD_REQ: u32 = 11011;
    pub const BAD_ROUTE: u32 = 11012;
    pub const TTL_EXPIRED_TRANSIT: u32 = 11013;
    pub const TTL_EXPIRED_REASSEM: u32 = 11014;
    pub const PARAM_PROBLEM: u32 = 11015;
    pub const SOURCE_QUENCH: u32 = 11016;
    pub const OPTION_TOO_BIG: u32 = 11017;
    pub const BAD_DESTINATION: u32 = 11018;
    pub const DEST_UNREACHABLE: u32 = 11040;
    pub const TIME_EXCEEDED: u32 = 11041;
    pub const BAD_HEADER: u32 = 11042;
    pub const UNRECOGNIZED_NEXT_HEADER: u32 = 11043;
    pub const ICMP_ERROR: u32 = 11044;
    pub const DEST_SCOPE_MISMATCH: u32 = 11045;
    pub const GENERAL_FAILURE: u32 = 11050;

    /// Whether `code` is an IP status rather than a Win32 error. The ICMP
    /// API reports a lost echo through `GetLastError` with one of these.
    pub fn is_ip_status(code: u32) -> bool {
        (11000..=11050).contains(&code)
    }
}

/// Reads one echo result. Only a matching echo reply from the target itself
/// is a reply; a timeout or an ICMP error from the path is no reply; a
/// failure to send the echo at all is an error, never a lost reply, and never
/// a 0 ms success.
pub fn classify_echo(reply: &EchoReply, target: Ipv4Addr) -> Result<ProbeOutcome, AdapterError> {
    use status::*;
    match reply.status {
        SUCCESS if reply.from != target => Err(AdapterError::new(
            AdapterErrorKind::ParseError,
            "echo reply came from another address",
        )),
        SUCCESS if !reply.payload_matches => Err(AdapterError::new(
            AdapterErrorKind::ParseError,
            "echo reply does not carry this probe's data",
        )),
        SUCCESS => Ok(ProbeOutcome::Reply {
            rtt: Duration::from_millis(u64::from(reply.rtt_ms)),
        }),
        DEST_NET_UNREACHABLE
        | DEST_HOST_UNREACHABLE
        | DEST_PROT_UNREACHABLE
        | DEST_PORT_UNREACHABLE
        | REQ_TIMED_OUT
        | TTL_EXPIRED_TRANSIT
        | TTL_EXPIRED_REASSEM
        | SOURCE_QUENCH
        | DEST_UNREACHABLE
        | TIME_EXCEEDED => Ok(ProbeOutcome::NoReply),
        BAD_ROUTE => Err(AdapterError::new(
            AdapterErrorKind::RouteMismatch,
            "Windows reported a bad route for the echo",
        )),
        BUF_TOO_SMALL
        | NO_RESOURCES
        | BAD_OPTION
        | HW_ERROR
        | PACKET_TOO_BIG
        | BAD_REQ
        | PARAM_PROBLEM
        | OPTION_TOO_BIG
        | BAD_DESTINATION
        | BAD_HEADER
        | UNRECOGNIZED_NEXT_HEADER
        | ICMP_ERROR
        | DEST_SCOPE_MISMATCH
        | GENERAL_FAILURE => Err(AdapterError::new(
            AdapterErrorKind::Unsupported,
            &format!(
                "the ICMP echo could not be sent (IP status {})",
                reply.status
            ),
        )),
        other => Err(AdapterError::new(
            AdapterErrorKind::ParseError,
            &format!("undocumented IP status {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::status::*;
    use super::*;

    const TARGET: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);

    fn reply(status: u32) -> EchoReply {
        EchoReply {
            status,
            from: TARGET,
            rtt_ms: 0,
            payload_matches: true,
        }
    }

    fn kind(result: Result<ProbeOutcome, AdapterError>) -> AdapterErrorKind {
        result.unwrap_err().kind
    }

    #[test]
    fn a_matching_reply_keeps_its_measured_rtt() {
        let echo = EchoReply {
            rtt_ms: 37,
            ..reply(SUCCESS)
        };
        assert_eq!(
            classify_echo(&echo, TARGET).unwrap(),
            ProbeOutcome::Reply {
                rtt: Duration::from_millis(37)
            }
        );
    }

    #[test]
    fn replies_that_are_not_this_echo_are_rejected() {
        let other_host = EchoReply {
            from: Ipv4Addr::new(203, 0, 113, 2),
            ..reply(SUCCESS)
        };
        assert_eq!(
            kind(classify_echo(&other_host, TARGET)),
            AdapterErrorKind::ParseError
        );
        let other_data = EchoReply {
            payload_matches: false,
            ..reply(SUCCESS)
        };
        assert_eq!(
            kind(classify_echo(&other_data, TARGET)),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn lost_or_refused_echoes_are_no_reply() {
        for code in [
            DEST_NET_UNREACHABLE,
            DEST_HOST_UNREACHABLE,
            DEST_PROT_UNREACHABLE,
            DEST_PORT_UNREACHABLE,
            REQ_TIMED_OUT,
            TTL_EXPIRED_TRANSIT,
            TTL_EXPIRED_REASSEM,
            SOURCE_QUENCH,
            DEST_UNREACHABLE,
            TIME_EXCEEDED,
        ] {
            assert_eq!(
                classify_echo(&reply(code), TARGET).unwrap(),
                ProbeOutcome::NoReply,
                "{code}"
            );
        }
    }

    #[test]
    fn local_failures_are_errors_never_no_reply() {
        for code in [
            BUF_TOO_SMALL,
            NO_RESOURCES,
            BAD_OPTION,
            HW_ERROR,
            PACKET_TOO_BIG,
            BAD_REQ,
            PARAM_PROBLEM,
            OPTION_TOO_BIG,
            BAD_DESTINATION,
            BAD_HEADER,
            UNRECOGNIZED_NEXT_HEADER,
            ICMP_ERROR,
            DEST_SCOPE_MISMATCH,
            GENERAL_FAILURE,
        ] {
            assert_eq!(
                kind(classify_echo(&reply(code), TARGET)),
                AdapterErrorKind::Unsupported,
                "{code}"
            );
        }
        assert_eq!(
            kind(classify_echo(&reply(BAD_ROUTE), TARGET)),
            AdapterErrorKind::RouteMismatch
        );
    }

    #[test]
    fn undocumented_statuses_are_parse_errors() {
        for code in [1, 10_999, 11_019, 11_049, 11_051, u32::MAX] {
            assert_eq!(
                kind(classify_echo(&reply(code), TARGET)),
                AdapterErrorKind::ParseError,
                "{code}"
            );
        }
    }

    #[test]
    fn ip_status_range_is_recognized() {
        assert!(is_ip_status(REQ_TIMED_OUT));
        assert!(is_ip_status(GENERAL_FAILURE));
        assert!(!is_ip_status(5));
        assert!(!is_ip_status(11_051));
    }

    #[test]
    fn helper_failures_never_look_like_a_down_target() {
        assert_eq!(
            NetError::ACCESS_DENIED.to_adapter_error("x").kind,
            AdapterErrorKind::PermissionDenied
        );
        for gone in [NetError::NOT_FOUND, NetError::NETWORK_UNREACHABLE] {
            assert_eq!(
                gone.to_adapter_error("x").kind,
                AdapterErrorKind::RouteMismatch
            );
        }
        for other in [
            NetError::INVALID_PARAMETER,
            NetError::NOT_ENOUGH_MEMORY,
            NetError { code: 50 },
        ] {
            assert_eq!(
                other.to_adapter_error("x").kind,
                AdapterErrorKind::Unsupported
            );
        }
    }
}
