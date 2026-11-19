use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, Detail, DisplayName, OsState, ProbeTarget,
    TargetAddress,
};

/// Longest [`TunnelBinding`] accepted, in bytes.
pub const MAX_BINDING_LEN: usize = 64;

/// How the official manager itself names a tunnel: the `<name>` of a Windows
/// `WireGuardTunnel$<name>` service, a macOS VPN configuration UUID, or the
/// interface of an Ubuntu `wg-quick@<interface>.service` unit.
///
/// The watchdog persists the mapping from its own [`TunnelId`] to this binding
/// and passes the binding to every adapter call; the GUI never supplies one.
/// This type only checks the alphabet shared by all three managers
/// (`[A-Za-z0-9_=+.-]`, at most [`MAX_BINDING_LEN`] bytes, not `.` or `..`).
/// Each adapter validates its own format again before the value reaches an
/// argument vector or an OS API.
///
/// [`TunnelId`]: wgal_protocol::TunnelId
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TunnelBinding(String);

/// A string was rejected as a [`TunnelBinding`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidBinding {
    reason: &'static str,
}

impl fmt::Display for InvalidBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid tunnel binding: {}", self.reason)
    }
}

impl std::error::Error for InvalidBinding {}

impl TunnelBinding {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidBinding> {
        let value = value.into();
        let reason = if value.is_empty() {
            "must not be empty"
        } else if value.len() > MAX_BINDING_LEN {
            "must be at most 64 bytes"
        } else if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_=+.-".contains(&b))
        {
            "only ASCII letters, digits and '_', '=', '+', '.', '-' are allowed"
        } else if value == "." || value == ".." {
            "must not be '.' or '..'"
        } else {
            return Ok(Self(value));
        };
        Err(InvalidBinding { reason })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TunnelBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A tunnel the official manager has, as found by
/// [`PlatformAdapter::discover`]. `control` is `Unsupported` for tunnels owned
/// by a manager WGAL recognizes but does not drive, such as NetworkManager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredBinding {
    pub binding: TunnelBinding,
    pub display_name: DisplayName,
    pub control: Capability,
}

/// The interface carrying a tunnel right now.
///
/// Observation only: names such as `utun4` and interface indexes are
/// reassigned when an interface is recreated, so callers resolve again after
/// every start, stop or reconnect and never persist these values.
///
/// `index` is the OS interface index where the platform reports one without
/// native calls, and `None` otherwise. `source` is the tunnel's own IPv4
/// address, if it has one.
///
/// `incarnation` tells one instance of the interface from the next when the
/// name, index and address are all reused: the adapter LUID on Windows, the
/// connection's connect count tagged with the configuration that resolved it
/// on macOS, the interface index on Ubuntu. Two resolutions describe the same
/// interface only if they are equal in every field, `incarnation` included. It is `None` only where the platform offers
/// no such value, and the adapter's capability document must then say how a
/// rebuilt interface is detected instead.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResolvedInterface {
    pub name: String,
    pub index: Option<u32>,
    pub source: Option<Ipv4Addr>,
    pub incarnation: Option<u64>,
}

/// Evidence that traffic to `target` leaves through the named interface, from
/// the source address the OS would use. `interface_index` is set when the
/// platform reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteEvidence {
    pub target: TargetAddress,
    pub interface_name: String,
    pub interface_index: Option<u32>,
    pub source: Option<Ipv4Addr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeRequest {
    pub target: ProbeTarget,
    pub timeout: Duration,
}

/// Result of a probe that actually ran.
///
/// Failing to *run* the probe (missing tool, no permission, unparseable output)
/// is an [`AdapterError`], never `NoReply`, so a broken probe is not mistaken
/// for an unreachable target.
///
/// `Refused` is for TCP probes only: the target answered through the bound
/// interface but refused the connection. That proves the path through the
/// tunnel and says the service is down, so it must never count as a tunnel
/// failure or trigger recovery on its own (plan §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    Reply { rtt: Duration },
    Refused { rtt: Duration },
    NoReply,
}

/// Optional counters some managers expose. Each is `None` when that manager
/// does not provide it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TunnelMetrics {
    pub latest_handshake_unix_ms: Option<i64>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterError {
    pub kind: AdapterErrorKind,
    /// Bounded and free of control characters. Must never contain secrets.
    pub detail: Detail,
}

impl AdapterError {
    /// `detail` is sanitized and cut to [`Detail::MAX_BYTES`], so OS output can
    /// be passed in directly once secrets are removed from it.
    pub fn new(kind: AdapterErrorKind, detail: &str) -> Self {
        Self {
            kind,
            detail: Detail::lossy(detail),
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for AdapterError {}

/// What the watchdog needs from one platform's official WireGuard manager
/// (plan §4.1). Reconnect is not an adapter operation: the core composes it
/// from `stop`, `start`, `resolve_interface` and probes.
///
/// Implementations act only on the tunnel they are given, run fixed executables
/// with fixed argument vectors (never a shell), and report a failed read as an
/// error rather than as a disconnected tunnel. Every call returns within its
/// own bounded deadline.
///
/// `start` and `stop` ask the manager to change state and return once it has
/// accepted or refused the request; they do not wait for the tunnel to
/// settle. The core waits for the target state by polling `query_state` under
/// its own deadline (plan §6.2), so a tunnel that never gets there is a
/// timeout in the core, not here.
pub trait PlatformAdapter: Send + Sync {
    /// What this adapter can do here. Anything not yet proven is
    /// [`Capability::Unknown`].
    fn capabilities(&self) -> AdapterCapabilities;

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError>;

    /// Checks that controlling this tunnel cannot run code or configuration an
    /// unprivileged user can change (plan §4.4, §9.2), for example that
    /// Ubuntu's unit, hooks and configuration are root-owned and not writable
    /// by others. The core calls it when a tunnel is registered and again
    /// before every start it issues, and refuses both on any error. Adapters
    /// whose manager keeps configuration out of users' reach still validate
    /// the binding here and say why nothing else is needed.
    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError>;

    /// The manager's view of the tunnel. A binding the manager no longer has is
    /// `NotFound`, which the core reports as a lost binding, never as
    /// `Disconnected`.
    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError>;

    /// Finds the interface carrying the tunnel now. `NotFound` if it has none.
    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError>;

    /// Checks that traffic to `target` leaves through `interface`. A route
    /// through anything else, or an ambiguous one, is `RouteMismatch`.
    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError>;

    /// Sends one probe bound to `interface`. If the platform cannot bind the
    /// probe to it, this fails; it never falls back to an unbound probe. An
    /// interface that no longer exists is `NotFound`.
    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError>;

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError>;

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError>;

    /// Optional; managers without counters keep this default.
    fn read_metrics(&self, _tunnel: &TunnelBinding) -> Result<TunnelMetrics, AdapterError> {
        Err(AdapterError::new(
            AdapterErrorKind::Unsupported,
            "this manager does not provide metrics",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_managers_identifiers() {
        let uuid = "00000000-0000-4000-8000-000000000001";
        for ok in [
            "office",
            "wg0",
            "a=b+c.d-e_f",
            uuid,
            &"x".repeat(MAX_BINDING_LEN),
        ] {
            assert!(TunnelBinding::new(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn rejects_everything_else() {
        let too_long = "x".repeat(MAX_BINDING_LEN + 1);
        let rejected = [
            "",
            ".",
            "..",
            too_long.as_str(),
            "WireGuardTunnel$office",
            "wg0; reboot",
            "a b",
            "../wg0",
            "wg0\n",
            "t\u{fc}nnel",
        ];
        for bad in rejected {
            assert!(
                TunnelBinding::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
