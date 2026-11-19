//! The watchdog on Ubuntu (WGW-001-C; owner's decision of 2026-11-05):
//! a root systemd service, `wgal-watchdog.service`
//! (`packaging/linux/wgal-watchdog.service`), driving `wg-quick@<if>`
//! units through the Linux adapter (plan §3.1).
//!
//! | Path | What | Made by |
//! |---|---|---|
//! | `/var/lib/wgal` (0700) | `state`, `notify` (with the bot token, 0600), `events` | systemd (`StateDirectory=wgal`), the subfolders the watchdog |
//! | `/run/wgal` (0755) | the socket, `watchdog.sock` (0666) | systemd (`RuntimeDirectory=wgal`), the socket the watchdog |
//! | `/etc/wgal/access.conf` | the access list ([`access`]) | the installer |
//!
//! Every local user can reach the socket; the watchdog admits root (as
//! elevated) and the users on the access list, and answers anyone else with
//! `caller_not_allowed` (see `crate::socket::server`). Each peer is the
//! user the kernel recorded for its connection (`SO_PEERCRED`).
//!
//! Not on Linux yet, unlike Windows: resuming from sleep and network
//! changes do not start the grace period again; the probes' own failures
//! still decide.

#[cfg(any(target_os = "linux", test))]
pub mod access;

/// The data folder, made by systemd for the service.
pub const STATE_ROOT: &str = "/var/lib/wgal";

#[cfg(any(target_os = "linux", test))]
use crate::ipc::Caller;

/// The caller for a peer running as `uid`: root is allowed and elevated, a
/// listed user allowed, anyone else neither (and is answered with a
/// refusal).
#[cfg(any(target_os = "linux", test))]
pub fn caller_for(uid: u32, users: &std::collections::BTreeSet<u32>) -> Caller {
    Caller {
        account: uid.to_string(),
        allowed: uid == 0 || users.contains(&uid),
        elevated: uid == 0,
    }
}

#[cfg(target_os = "linux")]
const HINT: &str = "wgal-watchdog is the WGAL background service. systemd starts it as \
                    wgal-watchdog.service, which packaging/linux/install.sh sets up; started \
                    by hand it needs root and takes no arguments other than --version.";

/// The executable's entry point.
#[cfg(target_os = "linux")]
pub fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => run(),
        [only] if only == "--version" => {
            println!("wgal-watchdog {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{HINT}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn run() -> std::process::ExitCode {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;

    use wgal_platform::linux::LinuxAdapter;
    use wgal_platform::linux::socket::{RUNTIME_DIR, SOCKET_PATH, effective_uid, peer_uid};
    use wgal_platform::socket::{entry_of, trusted_folder};

    use crate::ipc;
    use crate::notify::FileSecretStore;
    use crate::socket::host::{EXIT_FOLDER, EXIT_USER, Layout, NOTIFY_DIR, Platform, log};

    if effective_uid() != 0 {
        log("not started: the Linux watchdog runs as root (systemd starts it so)");
        eprintln!("{HINT}");
        return ExitCode::from(EXIT_USER);
    }
    // systemd makes the socket's folder (0755); started by hand, it may be
    // missing, and the umask must not keep the users out of it.
    match std::fs::DirBuilder::new().create(RUNTIME_DIR) {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(RUNTIME_DIR, std::fs::Permissions::from_mode(0o755))
            {
                log(&format!(
                    "not started: {RUNTIME_DIR} cannot be opened to users ({})",
                    e.kind()
                ));
                return ExitCode::from(EXIT_FOLDER);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            log(&format!(
                "not started: {RUNTIME_DIR} cannot be made ({})",
                e.kind()
            ));
            return ExitCode::from(EXIT_FOLDER);
        }
    }
    if !entry_of(Path::new(RUNTIME_DIR)).is_ok_and(|entry| trusted_folder(&entry, 0, false)) {
        log(&format!(
            "not started: {RUNTIME_DIR} is not root's alone (or is a symlink)"
        ));
        return ExitCode::from(EXIT_FOLDER);
    }
    let list = match access::load() {
        Ok(list) => {
            log(&format!(
                "access list: {} listed users, {} tunnels",
                list.users.len(),
                list.tunnels.len()
            ));
            list
        }
        Err(access::AccessError::Io(std::io::ErrorKind::NotFound)) => {
            log("no access list: only root may use the watchdog");
            access::AccessList::default()
        }
        Err(error) => {
            log(&format!(
                "access list refused, so no users are listed and no tunnels allowed: {error}"
            ));
            access::AccessList::default()
        }
    };
    let layout = Layout {
        root: PathBuf::from(STATE_ROOT),
        socket: PathBuf::from(SOCKET_PATH),
        socket_mode: 0o666,
    };
    let users = Arc::new(list.users);
    let identify = Arc::new(move |conn: &std::os::unix::net::UnixStream| {
        peer_uid(conn).ok().map(|uid| caller_for(uid, &users))
    });
    crate::socket::host::run(
        &layout,
        Platform {
            adapter: LinuxAdapter::new(),
            secrets: FileSecretStore::new(&layout.sub(NOTIFY_DIR)),
            access: ipc::AccessList {
                tunnels: list.tunnels,
            },
            identify,
            own: Caller {
                account: "0".to_owned(),
                allowed: true,
                elevated: true,
            },
            uid: 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn root_is_elevated_a_listed_user_allowed_and_anyone_else_neither() {
        let users = BTreeSet::from([1000]);
        let root = caller_for(0, &users);
        assert!(root.allowed && root.elevated);
        let listed = caller_for(1000, &users);
        assert!(listed.allowed && !listed.elevated);
        assert_eq!(listed.account, "1000");
        let other = caller_for(1001, &users);
        assert!(!other.allowed && !other.elevated);
    }
}
