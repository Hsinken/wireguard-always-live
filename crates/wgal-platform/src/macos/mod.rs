//! macOS adapter: the official WireGuard app's VPN configurations, driven
//! through `/usr/sbin/scutil --nc` and probed with `/sbin/ping -b <utun>`
//! (plan §4.3). Owned by work package WGW-001-B.
//!
//! - A tunnel is bound by its configuration UUID ([`VpnUuid`]). Its `utun`
//!   interface is read from the current extended status on every
//!   [`resolve_interface`](PlatformAdapter::resolve_interface) and never
//!   reused from an earlier call: the app gets a new `utun` on each connect,
//!   and a number freed by one tunnel can later be given to another.
//! - The interface's `incarnation` is the connection's `ConnectCount` in its
//!   low [`COUNT_BITS`] bits, so a reconnect that lands on the same `utun`
//!   with the same address is still a different interface. The bits above
//!   hold the slot this adapter gave the configuration the interface was
//!   resolved for, so two configurations that get the same `utun`, address
//!   and count one after the other still resolve to different interfaces.
//!   `check_route` and `probe` re-read the status of exactly that
//!   configuration before they run and again after, and fail with `NotFound`
//!   unless both equal the interface they were given, so a result can never
//!   be credited across a reconnect or to another configuration.
//! - `check_registration` only validates the UUID: the official app keeps the
//!   configuration in its own store, `scutil` runs no user-supplied hooks, and
//!   WGAL runs as the same logged-in user, so no privilege boundary is crossed.
//! - `discover` lists only configurations whose service type is
//!   [`list::WIREGUARD_KINDS`]. Other VPN services (IPsec, L2TP, other apps)
//!   are not WireGuard tunnels and are left out. Disabled WireGuard services
//!   are listed with control `Unknown`.
//! - `start` and `stop` only issue `scutil --nc start|stop`; the core waits for
//!   `Connected` or `Disconnected` through `query_state`.
//! - ICMP probes use `ping -b`. TCP probes are `Unsupported`: binding a TCP
//!   connect to an interface needs either `IP_BOUND_IF` (unsafe code, not
//!   allowed here) or a tool whose binding has not been verified on a Mac.
//! - The app exposes no handshake or transfer counters through `scutil`, so
//!   `read_metrics` stays `Unsupported`; WGAL does not install `wg` to get
//!   them.
//! - `scutil --nc show` is never run: it prints the configuration itself.
//!
//!
//! # Bound probes (`bound_icmp_probe`)
//!
//! A probe is `ping -n -b <utun>` (`IP_BOUND_IF`), after `route -n get` has
//! named the same `utun` for the target with a route that is up and is not
//! to an address of this computer (`LOCAL`), a broadcast or multicast
//! address, nor one that drops or rejects what is sent; the core checks the
//! route again after the probe and discards the reply if it moved.
//! `bound_icmp_probe` was `Unknown` until the on-device tests summarized in
//! `docs/testing.md`. It is `Supported` since 4.3.0, by the owner's decision
//! of 2026-11-20, after these ran with 4.2.2 on a one-tunnel Mac (macOS
//! 27.2.1) through the installed watchdog, the user's own LaunchAgent, end
//! to end: the target answered through the tunnel before and after a stop
//! and start; a silent address inside the tunnel's routes got no reply
//! (`NoReply`); the LAN gateway and the Mac's LAN address were refused
//! before sending (`RouteMismatch`); the tunnel stopped and started within
//! 30 s through the watchdog, without a prompt, its `ConnectCount` higher
//! after; after the SSH session that stopped it had ended, a request from a
//! job left running connected it again through the watchdog; the
//! watchdog's folder and socket are the user's alone.
//!
//! One check did not pass on 4.2.2: the tunnel's own address routes through
//! the tunnel with the `LOCAL` flag, which 4.2.2 did not read, so one echo
//! was sent to it (no reply came back). 4.3.0 refuses such a route, and
//! reads exit 1 with `ping: bad interface name`, what this macOS prints for
//! an interface that no longer exists, as `NotFound`, as exit 68 already
//! was. The same
//! checks run again on the installed 4.3.0 decide whether this stays
//! `Supported`; until they pass, this is not accepted on a device.
//!
//! Still unproven: two tunnels and overlapping routes; a probe through an
//! interface that vanished during it (fakes only); automatic recovery's
//! fault path, for which no authorized way to silence the target exists.
//! When the Network Extension stalls while `scutil` still says `Connected`,
//! `ping` may print a `sendto` error that is not listed in `ping.rs`; that
//! is read as `ParseError` (the probe is unavailable), so it neither starts
//! automatic recovery nor sends a notification, until a Mac shows what it
//! prints. `query_state`, `start_stop` and `background_operation` stay
//! `Unknown`, as on the other platforms: the core does not need them to be
//! `Supported`.

#[cfg(target_os = "macos")]
mod final_path;
mod list;
mod ping;
mod route;
pub mod socket;
mod status;
mod text;
mod uuid;

#[cfg(test)]
mod tests;

/// The one function of this module the desktop GUI uses, for its log export.
#[cfg(target_os = "macos")]
pub use self::final_path::final_path_of_file;

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use wgal_core::{
    AdapterError, DiscoveredBinding, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, TunnelBinding,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, DisplayName, OsState, ProbeKind,
    TargetAddress,
};

use self::status::{NcState, NcStatus};
use self::text::Malformed;
pub use self::uuid::VpnUuid;
use crate::exec::{Invocation, Output, Program, Runner, SystemRunner};

pub const SCUTIL: Program = Program::new("/usr/sbin/scutil");

/// Deadline for `scutil --nc list|status`, and for `route get`.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for `scutil --nc start|stop`, which return once the request is
/// handed to the system.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const SCUTIL_OUTPUT_CAP: usize = 64 * 1024;
const PROBE_OUTPUT_CAP: usize = 8 * 1024;

/// Low bits of an `incarnation` that hold the connection's `ConnectCount`; a
/// larger count is refused. The bits above hold the configuration's slot.
pub const COUNT_BITS: u32 = 48;
/// Most configurations one adapter keeps a slot for: far more than the
/// watchdog registers, and well within the bits above [`COUNT_BITS`].
pub const MAX_CONFIGURATIONS: usize = 1024;
const _: () = assert!(MAX_CONFIGURATIONS <= 1 << (u64::BITS - COUNT_BITS));

/// What this adapter claims. The bound ICMP probe is `Supported` (the
/// on-device checks and the owner's decision in the module documentation);
/// TCP probes and metrics are not implemented, so those are `Unsupported`;
/// everything else stays `Unknown`.
pub const CAPABILITIES: AdapterCapabilities = AdapterCapabilities {
    bound_icmp_probe: Capability::Supported,
    bound_tcp_probe: Capability::Unsupported,
    read_metrics: Capability::Unsupported,
    ..AdapterCapabilities::UNKNOWN
};

/// [`PlatformAdapter`] for the official WireGuard app on macOS. Runs as the
/// logged-in user that owns the configurations (a LaunchAgent), without sudo.
#[derive(Debug)]
pub struct MacosAdapter<R = SystemRunner> {
    runner: R,
    /// Every configuration this adapter has resolved an interface for, in the
    /// order first seen; a configuration's index is its slot. `check_route`
    /// and `probe`, which are given only the interface, read the slot from its
    /// `incarnation` to know whose status to re-read. Entries are never
    /// removed or reordered, so a slot always names the same configuration.
    slots: Mutex<Vec<VpnUuid>>,
}

impl MacosAdapter {
    pub fn new() -> Self {
        Self::with_runner(SystemRunner)
    }
}

impl Default for MacosAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runner> MacosAdapter<R> {
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            slots: Mutex::new(Vec::new()),
        }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn run(
        &self,
        program: Program,
        args: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<Output, AdapterError> {
        let invocation = Invocation {
            program,
            args,
            timeout,
            max_output_bytes,
        };
        self.runner
            .run(&invocation)
            .map_err(|e| e.to_adapter_error(program))
    }

    /// `scutil --nc <verb> [<uuid>]`.
    fn scutil(
        &self,
        verb: &str,
        uuid: Option<&VpnUuid>,
        timeout: Duration,
    ) -> Result<Output, AdapterError> {
        let mut args = vec!["--nc".to_owned(), verb.to_owned()];
        args.extend(uuid.map(|u| u.as_str().to_owned()));
        let output = self.run(SCUTIL, args, timeout, SCUTIL_OUTPUT_CAP)?;
        if output.truncated {
            return Err(Malformed("output was truncated").into_error("scutil"));
        }
        if says_no_service(&output) {
            return Err(AdapterError::new(
                AdapterErrorKind::NotFound,
                "the official app no longer has this configuration",
            ));
        }
        Ok(output)
    }

    fn status(&self, binding: &TunnelBinding) -> Result<NcStatus, AdapterError> {
        let uuid = VpnUuid::from_binding(binding)?;
        let output = self.scutil("status", Some(&uuid), QUERY_TIMEOUT)?;
        if !output.success() {
            return Err(Malformed("unexpected exit status").into_error("scutil"));
        }
        let text = utf8(&output.stdout).map_err(|m| m.into_error("scutil"))?;
        status::parse_status(text).map_err(|m| m.into_error("scutil"))
    }

    fn slots(&self) -> MutexGuard<'_, Vec<VpnUuid>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The configuration's slot, given one the first time it is seen.
    fn slot_for(&self, uuid: &VpnUuid) -> Result<u64, AdapterError> {
        let mut slots = self.slots();
        let index = match slots.iter().position(|known| known == uuid) {
            Some(index) => index,
            None if slots.len() < MAX_CONFIGURATIONS => {
                slots.push(uuid.clone());
                slots.len() - 1
            }
            None => {
                return Err(AdapterError::new(
                    AdapterErrorKind::Unsupported,
                    "this adapter already tracks as many configurations as it can",
                ));
            }
        };
        Ok(index as u64)
    }

    /// The interface the configuration in `slot` is connected through now.
    fn interface_of(&self, uuid: &VpnUuid, slot: u64) -> Result<ResolvedInterface, AdapterError> {
        let status = self.status(&uuid.to_binding())?;
        if status.state != NcState::Connected {
            return Err(AdapterError::new(
                AdapterErrorKind::NotFound,
                "the tunnel is not connected",
            ));
        }
        let found = status::tunnel_interface(&status).map_err(|m| m.into_error("scutil"))?;
        if found.connect_count >> COUNT_BITS != 0 {
            return Err(Malformed("ConnectCount is too large").into_error("scutil"));
        }
        Ok(ResolvedInterface {
            name: found.name,
            index: None,
            source: found.addresses.first().copied(),
            incarnation: Some((slot << COUNT_BITS) | found.connect_count),
        })
    }

    /// Fails with `NotFound` unless `interface` is still exactly what the
    /// configuration it was resolved for is connected through: that
    /// configuration's status is read again and compared in every field,
    /// name, address and connect count included. Which configuration that is
    /// comes from the slot in the interface's `incarnation`, never from its
    /// `utun` name, which another configuration may have been given since.
    fn still_current(&self, interface: &ResolvedInterface) -> Result<(), AdapterError> {
        let unknown = || {
            AdapterError::new(
                AdapterErrorKind::NotFound,
                "the interface was not resolved by this adapter",
            )
        };
        let slot = interface.incarnation.ok_or_else(unknown)? >> COUNT_BITS;
        let uuid = usize::try_from(slot)
            .ok()
            .and_then(|index| self.slots().get(index).cloned())
            .ok_or_else(unknown)?;
        if self.interface_of(&uuid, slot)? == *interface {
            Ok(())
        } else {
            Err(AdapterError::new(
                AdapterErrorKind::NotFound,
                "the interface was replaced or the tunnel reconnected",
            ))
        }
    }

    fn control(&self, verb: &str, binding: &TunnelBinding) -> Result<(), AdapterError> {
        let uuid = VpnUuid::from_binding(binding)?;
        let output = self.scutil(verb, Some(&uuid), CONTROL_TIMEOUT)?;
        if output.success() {
            Ok(())
        } else {
            Err(AdapterError::new(
                AdapterErrorKind::ManagerConflict,
                &format!("the system refused to {verb} the connection"),
            ))
        }
    }
}

impl<R: Runner> PlatformAdapter for MacosAdapter<R> {
    fn capabilities(&self) -> AdapterCapabilities {
        CAPABILITIES
    }

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        let output = self.scutil("list", None, QUERY_TIMEOUT)?;
        if !output.success() {
            return Err(Malformed("unexpected exit status").into_error("scutil"));
        }
        let text = utf8(&output.stdout).map_err(|m| m.into_error("scutil"))?;
        let services = list::parse_list(text).map_err(|m| m.into_error("scutil"))?;
        Ok(services
            .into_iter()
            .filter(list::Service::is_wireguard)
            .map(|service| {
                let display_name = if service.name.trim().is_empty() {
                    DisplayName::lossy(service.uuid.as_str())
                } else {
                    DisplayName::lossy(&service.name)
                };
                DiscoveredBinding {
                    binding: service.uuid.to_binding(),
                    display_name,
                    control: if service.enabled {
                        Capability::Supported
                    } else {
                        Capability::Unknown
                    },
                }
            })
            .collect())
    }

    /// Validates the UUID and runs nothing; see the module documentation for
    /// why the official app needs no ownership check.
    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        VpnUuid::from_binding(tunnel).map(|_| ())
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        Ok(self.status(tunnel)?.state.os_state())
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        let uuid = VpnUuid::from_binding(tunnel)?;
        let slot = self.slot_for(&uuid)?;
        self.interface_of(&uuid, slot)
    }

    /// The evidence carries no source address: `route get` does not report
    /// the one the kernel would pick.
    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        self.still_current(interface)?;
        let output = self.run(
            route::ROUTE,
            route::args(target),
            QUERY_TIMEOUT,
            PROBE_OUTPUT_CAP,
        )?;
        let via = route::interpret(&output, target)?;
        if via != interface.name {
            return Err(AdapterError::new(
                AdapterErrorKind::RouteMismatch,
                "the target routes through another interface",
            ));
        }
        self.still_current(interface)?;
        Ok(RouteEvidence {
            target,
            interface_name: via,
            interface_index: None,
            source: None,
        })
    }

    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        if let ProbeKind::TcpConnect { .. } = request.target.probe {
            return Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "TCP probes bound to an interface are not available on macOS yet",
            ));
        }
        if !status::is_utun(&interface.name) {
            return Err(AdapterError::new(
                AdapterErrorKind::NotFound,
                "not a utun interface",
            ));
        }
        self.still_current(interface)?;
        let target = request.target.address;
        let output = self.run(
            ping::PING,
            ping::args(&interface.name, target, request.timeout),
            ping::deadline(request.timeout),
            PROBE_OUTPUT_CAP,
        )?;
        let outcome = ping::interpret(&output, target)?;
        self.still_current(interface)?;
        Ok(outcome)
    }

    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control("start", tunnel)
    }

    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control("stop", tunnel)
    }
}

/// `scutil` prints `No service` when the ID names no configuration.
fn says_no_service(output: &Output) -> bool {
    [&output.stdout, &output.stderr].iter().any(|stream| {
        String::from_utf8_lossy(stream)
            .lines()
            .any(|l| l == "No service")
    })
}

fn utf8(bytes: &[u8]) -> Result<&str, Malformed> {
    std::str::from_utf8(bytes).map_err(|_| Malformed("output is not UTF-8"))
}
