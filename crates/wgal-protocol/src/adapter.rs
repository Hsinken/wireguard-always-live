use serde::{Deserialize, Serialize};

use crate::ids::TunnelId;
use crate::text::DisplayName;

/// Most tunnels one discovery result can list.
pub const MAX_DISCOVERED: usize = 32;

/// Why a platform operation failed (plan §4.1; success is the `Ok` side).
///
/// A failed read is reported as one of these, never as a disconnected tunnel,
/// and a probe tool without permission is `PermissionDenied`, never a target
/// that did not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum AdapterErrorKind {
    Timeout,
    PermissionDenied,
    NotFound,
    Unsupported,
    /// Traffic to the target would not leave through the tunnel's interface,
    /// or the path cannot be determined.
    RouteMismatch,
    /// Another manager or tool controls the tunnel, or an operation on it is
    /// already in progress.
    ManagerConflict,
    ParseError,
    LocalNetworkOffline,
}

/// Whether a platform can do something. `Unknown` means not yet proven either
/// way; it is never reported as `Unsupported` without evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Supported,
    Unsupported,
    Unknown,
}

/// What the platform adapter on this machine can do (plan §4.1).
///
/// `verified_concurrent_tunnels` counts tunnels proven to run and recover side
/// by side on this platform; zero means not yet verified, not unsupported.
/// `background_operation` says whether the other abilities were verified under
/// the real background service identity, not just from a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct AdapterCapabilities {
    pub query_state: Capability,
    pub start_stop: Capability,
    pub bound_icmp_probe: Capability,
    pub bound_tcp_probe: Capability,
    pub read_metrics: Capability,
    pub verified_concurrent_tunnels: u32,
    pub background_operation: Capability,
}

impl AdapterCapabilities {
    /// Nothing proven yet.
    pub const UNKNOWN: Self = Self {
        query_state: Capability::Unknown,
        start_stop: Capability::Unknown,
        bound_icmp_probe: Capability::Unknown,
        bound_tcp_probe: Capability::Unknown,
        read_metrics: Capability::Unknown,
        verified_concurrent_tunnels: 0,
        background_operation: Capability::Unknown,
    };
}

/// A tunnel the official manager already knows about, offered for
/// registration. `control` is `Unsupported` for a management method WGAL
/// recognizes but does not drive, such as NetworkManager on Ubuntu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct DiscoveredTunnel {
    pub tunnel_id: TunnelId,
    pub display_name: DisplayName,
    pub control: Capability,
}
