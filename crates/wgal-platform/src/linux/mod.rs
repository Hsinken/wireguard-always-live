//! Ubuntu adapter: `wg-quick@<interface>.service` units driven through
//! systemd, interfaces and routes read with `ip -j`, probes bound with
//! `ping -I <interface>` (plan §4.4). Owned by work package WGW-001-C.
//!
//! A tunnel's [`TunnelBinding`] is its wg-quick interface name, validated as
//! an [`InterfaceName`] before it reaches any argument vector. Every command is
//! one of the fixed argument vectors built in this module, run through an
//! [`exec::Runner`](crate::exec::Runner) with an output cap and a deadline.
//! The adapter only ever names the unit of the tunnel it was given; it never
//! runs wg-quick itself, NetworkManager or networkd tools, `wg show all dump`
//! or `wg showconf`, and never reads WireGuard configuration contents.
//!
//! `start` and `stop` use `systemctl --no-block`: they return as soon as
//! systemd has queued the job (or refused it), and the core waits for
//! `active (exited)` or `inactive` through [`PlatformAdapter::query_state`],
//! as the trait asks. A blocking `systemctl start` would instead wait for
//! `wg-quick up`, which can hang on endpoint name resolution, and a killed
//! client would leave the job running anyway.
//!
//! Both `wg-quick up` and `wg-quick down` run, as root, whatever the unit, its
//! drop-ins and the tunnel's configuration say (PreUp, PostUp, PreDown,
//! PostDown). So [`PlatformAdapter::check_registration`] also runs
//! immediately before every `start` and every `stop`: if any of those files
//! has become writable by an ordinary user since registration, the command is
//! never issued. Refusing a stop is deliberate: a disconnect that fails
//! visibly is safer than running a user's hook as root. The check reads the
//! configuration and binary metadata first, so a bad configuration is refused
//! before any command runs; only the unit file and drop-ins need the
//! read-only `systemctl show` that names them.
//!
//! An interface's incarnation is its ifindex, which the kernel does not hand
//! out again while the old interface could still be referenced. `probe` and
//! `check_route` read the interface again after their commands and refuse with
//! `NotFound` if it was replaced meanwhile, so nothing measured through a new
//! interface of the same name is credited to the old one.
//!
//! # Bound probes (`bound_icmp_probe`)
//!
//! A probe is `ping -4 -n -I <interface>`, after both the plain and the
//! `oif <interface>` route lookups have named the interface and its own
//! address; the core checks the route again after the probe and discards the
//! reply if it moved. `bound_icmp_probe` was `Unknown` until the on-device
//! tests summarized in `docs/testing.md`. It is `Supported`
//! since the owner's request of 2026-11-19, after these passed on a
//! one-tunnel Ubuntu Server 26.04 through the installed watchdog (root),
//! end to end: the target answered through the tunnel before and after a
//! stop and start (U3 items 1 and 3), a silent address inside the tunnel's
//! routes got no reply (`NoReply`, item 3a), and the LAN gateway and the
//! host's own addresses were refused before sending (`RouteMismatch`); the
//! unit stopped and started within 30 s (U1) with its ifindex changed (U2);
//! the service runs as root with files an ordinary user cannot write (U4 in
//! part; U6 directly where a user can read, indirectly inside
//! `/etc/wireguard`). A probe through an interface that vanished during it
//! is covered by fakes only. After the 4.2.1 upgrade on the same server the
//! light turned green, a connect and a reconnect were verified, automatic
//! recovery could be turned on, and the tunnel's settings were kept across
//! the watchdog's restart and across a reboot, after which the light was
//! green again within 15 s. Still unproven: two tunnels and overlapping
//! routes, and automatic recovery's fault path (alert, retries, recovered),
//! for which no authorized way to silence the target exists; this is not
//! acceptance on a device. TCP probes are `Unsupported`
//! until a source-bound connect is available; the other abilities stay
//! `Unknown`, as the core does not use them.

mod files;
#[cfg(target_os = "linux")]
mod final_path;
mod ip;
mod name;
mod ping;
pub mod socket;
mod systemd;
mod wg;

use std::net::Ipv4Addr;
use std::time::Duration;

use wgal_core::{
    AdapterError, DiscoveredBinding, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, TunnelBinding, TunnelMetrics,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, DisplayName, OsState, ProbeKind,
    TargetAddress,
};

use crate::exec::{Invocation, Output, Program, Runner, SystemRunner};

pub use files::{FileFacts, FileInspector, FileKind, SystemFiles};
/// The one function of this module the desktop GUI uses, for its log export.
#[cfg(target_os = "linux")]
pub use final_path::final_path_of_file;
pub use name::{InterfaceName, MAX_INTERFACE_LEN};

pub const SYSTEMCTL: Program = Program::new("/usr/bin/systemctl");
pub const IP: Program = Program::new("/usr/sbin/ip");
pub const PING: Program = Program::new("/usr/bin/ping");
pub const WG: Program = Program::new("/usr/bin/wg");

/// `systemctl` arguments listing loaded wg-quick instances, including stopped
/// and failed ones. The pattern is matched by systemctl, not a shell.
const LIST_UNITS: [&str; 8] = [
    "list-units",
    "--type=service",
    "--all",
    "--plain",
    "--no-legend",
    "--no-pager",
    "--full",
    "wg-quick@*",
];

/// `systemctl` arguments listing wg-quick instances that have their own unit
/// file entry, such as enabled ones that were never started.
const LIST_UNIT_FILES: [&str; 6] = [
    "list-unit-files",
    "--type=service",
    "--plain",
    "--no-legend",
    "--no-pager",
    "wg-quick@*",
];

/// Binaries the wg-quick unit runs as root; checked at registration.
const WG_QUICK_BINARIES: [&str; 2] = ["/usr/bin/wg-quick", "/usr/bin/wg"];

const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);
const IP_TIMEOUT: Duration = Duration::from_secs(5);
const WG_TIMEOUT: Duration = Duration::from_secs(5);
/// Time ping gets beyond its wait for the reply (`-W`) and one more second
/// before the runner kills it.
const PING_GRACE: Duration = Duration::from_secs(2);
/// Longest per-probe wait accepted, in seconds.
const MAX_PROBE_SECS: u64 = 30;
const MAX_LIST_BYTES: usize = 64 * 1024;
const MAX_CONTROL_BYTES: usize = 4 * 1024;

/// [`PlatformAdapter`] for wg-quick tunnels managed by systemd on Ubuntu.
///
/// [`capabilities`](PlatformAdapter::capabilities) reports a capability as
/// `Unknown` until on-device acceptance proves it (`docs/testing.md`): the
/// bound ICMP probe is `Supported` since the on-device tests of 2026-11-19
/// (the module documentation). Bound TCP probes are `Unsupported`: no fixed
/// system tool binds a TCP connect to an interface.
#[derive(Debug)]
pub struct LinuxAdapter<R = SystemRunner, F = SystemFiles> {
    runner: R,
    files: F,
}

impl LinuxAdapter {
    pub fn new() -> Self {
        Self::with_parts(SystemRunner, SystemFiles)
    }
}

impl Default for LinuxAdapter {
    fn default() -> Self {
        Self::new()
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn parse_error(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::ParseError, detail)
}

impl<R: Runner, F: FileInspector> LinuxAdapter<R, F> {
    pub fn with_parts(runner: R, files: F) -> Self {
        Self { runner, files }
    }

    /// Runs one fixed command. Output past the cap is refused here, so no
    /// parser ever sees a truncated answer.
    fn run(
        &self,
        program: Program,
        args: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<Output, AdapterError> {
        let output = self
            .runner
            .run(&Invocation {
                program,
                args,
                timeout,
                max_output_bytes,
            })
            .map_err(|e| e.to_adapter_error(program))?;
        if output.truncated {
            return Err(parse_error(&format!(
                "{program} printed more than expected"
            )));
        }
        Ok(output)
    }

    fn show(&self, name: &InterfaceName) -> Result<systemd::UnitStatus, AdapterError> {
        let output = self.run(
            SYSTEMCTL,
            vec![
                "show".to_owned(),
                name.unit(),
                format!("--property={}", systemd::SHOW_PROPERTIES),
                "--no-pager".to_owned(),
            ],
            SYSTEMCTL_TIMEOUT,
            systemd::MAX_SHOW_BYTES,
        )?;
        if !output.success() {
            return Err(systemctl_failure(&output));
        }
        systemd::UnitStatus::parse(&output.stdout)
    }

    /// Whether the tunnel's configuration file still exists. `None` when that
    /// cannot be told (for example without the privileges to look).
    fn config_exists(&self, name: &InterfaceName) -> Option<bool> {
        match self.files.lstat(&name.config_path()) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }

    fn binding_lost() -> AdapterError {
        AdapterError::new(
            AdapterErrorKind::NotFound,
            "the wg-quick configuration no longer exists; register the tunnel again",
        )
    }

    /// Issues `systemctl start|stop` for the unit, after the ownership guard
    /// passed for this very call.
    fn control(&self, name: &InterfaceName, verb: &str) -> Result<(), AdapterError> {
        self.ownership_guard(name)?;
        let output = self.run(
            SYSTEMCTL,
            vec![
                "--no-block".to_owned(),
                "--no-ask-password".to_owned(),
                verb.to_owned(),
                name.unit(),
            ],
            SYSTEMCTL_TIMEOUT,
            MAX_CONTROL_BYTES,
        )?;
        if output.success() {
            Ok(())
        } else {
            Err(systemctl_failure(&output))
        }
    }

    /// Checks that starting or stopping this tunnel's unit with privileges
    /// cannot run code an ordinary user controls: the tunnel's configuration
    /// and the wg-quick binaries must be root-owned and not writable by group
    /// or others, with no symbolic links on their paths, and the unit must load
    /// from files that pass the same test (including drop-ins). Only metadata
    /// is read, the configuration and binaries before any command runs. A
    /// masked unit is refused: WGAL never unmasks. Root-owned files under
    /// root-owned, non-shared directories cannot be changed by anyone but root
    /// between this check and the command it guards.
    ///
    /// Only a missing configuration is `NotFound` (the binding is lost). A
    /// missing wg-quick binary is `Unsupported`, like any tool that cannot be
    /// run, and a unit file or drop-in systemd names but the disk lacks is
    /// `ManagerConflict` (see [`files::Role`]).
    fn ownership_guard(&self, name: &InterfaceName) -> Result<(), AdapterError> {
        if self.config_exists(name) == Some(false) {
            return Err(Self::binding_lost());
        }
        files::check_file(&self.files, &name.config_path(), files::Role::Config)?;
        for binary in WG_QUICK_BINARIES {
            files::check_file(&self.files, binary, files::Role::Tool)?;
        }
        let unit = self.show(name)?;
        if let Some(error) = unit.load.error() {
            return Err(error);
        }
        if unit.load == systemd::LoadState::Masked {
            return Err(AdapterError::new(
                AdapterErrorKind::ManagerConflict,
                "the wg-quick unit is masked; WGAL does not unmask it",
            ));
        }
        let fragment = unit
            .fragment_path
            .as_deref()
            .ok_or_else(|| parse_error("systemd did not report the unit file"))?;
        files::check_file(&self.files, fragment, files::Role::Unit)?;
        for drop_in in &unit.drop_in_paths {
            files::check_file(&self.files, drop_in, files::Role::Unit)?;
        }
        Ok(())
    }

    fn resolve(&self, name: &InterfaceName) -> Result<ResolvedInterface, AdapterError> {
        let output = self.run(
            IP,
            vec![
                "-j".to_owned(),
                "address".to_owned(),
                "show".to_owned(),
                "dev".to_owned(),
                name.as_str().to_owned(),
            ],
            IP_TIMEOUT,
            ip::MAX_IP_BYTES,
        )?;
        if !output.success() {
            return Err(ip::classify_failure(&output));
        }
        ip::parse_address(&output.stdout, name)
    }

    /// Fails with `NotFound` unless `interface` is still the interface with
    /// that name, incarnation (ifindex) and address included.
    fn ensure_unchanged(
        &self,
        interface: &ResolvedInterface,
        name: &InterfaceName,
    ) -> Result<(), AdapterError> {
        if self.resolve(name)? == *interface {
            Ok(())
        } else {
            Err(AdapterError::new(
                AdapterErrorKind::NotFound,
                "the interface was replaced; resolve it again",
            ))
        }
    }

    fn route(
        &self,
        interface: &ResolvedInterface,
        name: &InterfaceName,
        target: TargetAddress,
        bound: bool,
    ) -> Result<RouteEvidence, AdapterError> {
        let mut argv = vec![
            "-j".to_owned(),
            "route".to_owned(),
            "get".to_owned(),
            target.get().to_string(),
        ];
        if bound {
            argv.push("oif".to_owned());
            argv.push(name.as_str().to_owned());
        }
        let output = self.run(IP, argv, IP_TIMEOUT, ip::MAX_IP_BYTES)?;
        if !output.success() {
            return Err(ip::classify_failure(&output));
        }
        ip::parse_route(&output.stdout, target, interface)
    }

    fn ping(
        &self,
        name: &InterfaceName,
        source: Option<Ipv4Addr>,
        target: TargetAddress,
        timeout: Duration,
    ) -> Result<ProbeOutcome, AdapterError> {
        let wait =
            (timeout.as_secs() + u64::from(timeout.subsec_nanos() > 0)).clamp(1, MAX_PROBE_SECS);
        // No `-w`: with `-c 1 -w <deadline>` iputils keeps sending one echo
        // a second until a reply or the deadline, so a silent target ended
        // as "3 packets transmitted" and was read as a broken probe (AR-01).
        // With `-c 1 -W <wait>` it sends one echo and waits for it; the
        // runner's deadline is the only bound on the run.
        let deadline = wait + 1;
        let output = self.run(
            PING,
            vec![
                "-4".to_owned(),
                "-n".to_owned(),
                "-I".to_owned(),
                name.as_str().to_owned(),
                "-c".to_owned(),
                "1".to_owned(),
                "-W".to_owned(),
                wait.to_string(),
                target.get().to_string(),
            ],
            Duration::from_secs(deadline) + PING_GRACE,
            ping::MAX_PING_BYTES,
        )?;
        ping::parse_ping(&output, target.get(), name, source)
    }

    fn wg_show(&self, name: &InterfaceName, what: &str) -> Result<Vec<u8>, AdapterError> {
        let output = self.run(
            WG,
            vec!["show".to_owned(), name.as_str().to_owned(), what.to_owned()],
            WG_TIMEOUT,
            wg::MAX_WG_BYTES,
        )?;
        if output.success() {
            Ok(output.stdout)
        } else {
            Err(wg::classify_failure(&output))
        }
    }
}

/// A `systemctl` run that exited non-zero, classified from its (C locale)
/// error text without copying it.
fn systemctl_failure(output: &Output) -> AdapterError {
    let text = String::from_utf8_lossy(&output.stderr);
    if text.contains("Access denied")
        || text.contains("Interactive authentication required")
        || text.contains("Permission denied")
    {
        AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            "systemd refused the request for lack of privileges",
        )
    } else if text.contains("not found") {
        AdapterError::new(
            AdapterErrorKind::NotFound,
            "systemd has no such wg-quick unit",
        )
    } else if text.contains("masked") {
        AdapterError::new(
            AdapterErrorKind::ManagerConflict,
            "the wg-quick unit is masked; WGAL does not unmask it",
        )
    } else {
        AdapterError::new(
            AdapterErrorKind::ManagerConflict,
            "systemctl failed with an unrecognised error",
        )
    }
}

impl<R: Runner, F: FileInspector> PlatformAdapter for LinuxAdapter<R, F> {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            // U1-U4 and U6 on a device, owner's request (module documentation).
            bound_icmp_probe: Capability::Supported,
            bound_tcp_probe: Capability::Unsupported,
            ..AdapterCapabilities::UNKNOWN
        }
    }

    /// wg-quick units systemd has loaded or that are enabled, plus WireGuard
    /// interfaces that exist now. An interface no wg-quick unit owns belongs to
    /// another manager (NetworkManager, networkd, a manual `ip link`) and is
    /// listed with control `Unsupported`; WGAL never takes it over.
    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        let list = |argv: &[&str]| {
            let output = self.run(SYSTEMCTL, args(argv), SYSTEMCTL_TIMEOUT, MAX_LIST_BYTES)?;
            if output.success() {
                Ok(output.stdout)
            } else {
                Err(systemctl_failure(&output))
            }
        };
        let mut units = systemd::parse_list_units(&list(&LIST_UNITS)?)?;
        units.extend(systemd::parse_list_unit_files(&list(&LIST_UNIT_FILES)?)?);

        let links = self.run(
            IP,
            args(&["-j", "link", "show", "type", "wireguard"]),
            IP_TIMEOUT,
            MAX_LIST_BYTES,
        )?;
        if !links.success() {
            return Err(ip::classify_failure(&links));
        }
        let links = ip::parse_wireguard_links(&links.stdout)?;

        let mut found = std::collections::BTreeMap::new();
        for name in units {
            found.insert(name.binding(), Capability::Supported);
        }
        for link in links {
            if let Ok(binding) = TunnelBinding::new(link) {
                found.entry(binding).or_insert(Capability::Unsupported);
            }
        }
        Ok(found
            .into_iter()
            .map(|(binding, control)| DiscoveredBinding {
                display_name: DisplayName::lossy(binding.as_str()),
                binding,
                control,
            })
            .collect())
    }

    /// The unit's state as systemd reports it (see
    /// [`UnitStatus::os_state`](systemd::UnitStatus::os_state)). A stopped
    /// unit whose configuration file is gone is `NotFound`: systemd still
    /// loads any instance of the template, so only the missing file shows the
    /// tunnel no longer exists.
    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.ownership_guard(&InterfaceName::from_binding(tunnel)?)
    }

    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        let name = InterfaceName::from_binding(tunnel)?;
        let state = self.show(&name)?.os_state()?;
        if state == OsState::Disconnected && self.config_exists(&name) == Some(false) {
            return Err(Self::binding_lost());
        }
        Ok(state)
    }

    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        self.resolve(&InterfaceName::from_binding(tunnel)?)
    }

    /// Both the route ordinary traffic takes and the route bound to the
    /// interface must leave through it from its own address. The first
    /// catches a target that other traffic would reach through another
    /// tunnel; the second that the interface itself can reach it. The
    /// interface must still be the same one afterwards.
    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        let name = InterfaceName::new(&interface.name).ok_or_else(|| {
            AdapterError::new(AdapterErrorKind::NotFound, "not a wg-quick interface name")
        })?;
        let evidence = self.route(interface, &name, target, false)?;
        self.route(interface, &name, target, true)?;
        self.ensure_unchanged(interface, &name)?;
        Ok(evidence)
    }

    /// A reply or a missing reply counts only if the interface is still the
    /// same one after the ping; otherwise it was measured through another.
    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        let name = InterfaceName::new(&interface.name).ok_or_else(|| {
            AdapterError::new(AdapterErrorKind::NotFound, "not a wg-quick interface name")
        })?;
        match request.target.probe {
            ProbeKind::Icmp => {
                let outcome = self.ping(
                    &name,
                    interface.source,
                    request.target.address,
                    request.timeout,
                )?;
                self.ensure_unchanged(interface, &name)?;
                Ok(outcome)
            }
            ProbeKind::TcpConnect { .. } => Err(AdapterError::new(
                AdapterErrorKind::Unsupported,
                "TCP probes bound to an interface are not available on Ubuntu yet",
            )),
        }
    }

    /// Runs the registration check again first (see the module docs), which
    /// also refuses with `NotFound` when the tunnel's configuration is gone
    /// instead of queueing a job that can only fail.
    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(&InterfaceName::from_binding(tunnel)?, "start")
    }

    /// Runs the registration check again first: `wg-quick down` runs the
    /// configuration's PreDown and PostDown hooks as root too.
    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.control(&InterfaceName::from_binding(tunnel)?, "stop")
    }

    /// Newest handshake and byte counters from `wg show`, which needs the
    /// privileges the watchdog service has; without them this is
    /// `PermissionDenied`, shown as "not provided", never as a fault.
    fn read_metrics(&self, tunnel: &TunnelBinding) -> Result<TunnelMetrics, AdapterError> {
        let name = InterfaceName::from_binding(tunnel)?;
        let handshakes = self.wg_show(&name, "latest-handshakes")?;
        let transfer = self.wg_show(&name, "transfer")?;
        wg::parse_metrics(&handshakes, &transfer)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    /// Reads `tests/fixtures/linux/<name>` from the workspace root.
    pub fn fixture(name: &str) -> String {
        let path: PathBuf = [
            env!("CARGO_MANIFEST_DIR"),
            "..",
            "..",
            "tests",
            "fixtures",
            "linux",
            name,
        ]
        .iter()
        .collect();
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
    }
}
