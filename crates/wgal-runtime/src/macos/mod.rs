//! The watchdog on macOS (WGW-001-B; owner's decision of 2026-11-05): the
//! signed-in user's own LaunchAgent (plan §3.1: "該使用者的 launchd
//! LaunchAgent … 不預設 root helper"), driving the official WireGuard app's
//! VPN configurations through `scutil --nc` with the user's own rights.
//!
//! Everything is in the user's private folder,
//! `~/Library/Application Support/WGAL` (mode 0700): `state`, `notify` (the
//! notification settings; the bot token is in the login Keychain),
//! `events`, and the socket `watchdog.sock` (0600). The LaunchAgent's
//! standard error, the log, goes to `~/Library/Logs/WGAL/watchdog.log`
//! (`packaging/macos/install.sh` writes the agent's plist).
//!
//! Only the same user is admitted: each peer is the user the kernel
//! recorded for its connection (`getpeereid`). The agent can do nothing the
//! user cannot do already, so there is no installation access list, and
//! that user counts as elevated: registering a tunnel or turning automatic
//! recovery on widens nothing here.
//!
//! `wgal-watchdog --forget-token` removes this user's bot token from the
//! login Keychain and nothing else, for the uninstaller
//! (`packaging/macos/uninstall.sh --remove-data`): the Keychain's own
//! status tells "there was none" from a removal that failed, which the
//! `security` tool cannot (Codex review of 2026-11-05). It takes no input.
//!
//! Not on macOS yet, unlike Windows: resuming from sleep and network
//! changes do not start the grace period again; the probes' own failures
//! still decide.

#[cfg(any(target_os = "macos", test))]
use crate::ipc::Caller;

/// The caller for a peer running as `peer`, the watchdog running as `own`:
/// the same user is allowed and elevated, anyone else neither (and is
/// answered with a refusal; only root could reach the private folder
/// anyway).
#[cfg(any(target_os = "macos", test))]
pub fn caller_for(peer: u32, own: u32) -> Caller {
    Caller {
        account: peer.to_string(),
        allowed: peer == own,
        elevated: peer == own,
    }
}

#[cfg(target_os = "macos")]
const HINT: &str = "wgal-watchdog is the WGAL background agent. launchd starts it for the \
                    signed-in user, as packaging/macos/install.sh sets up; it runs as that user, \
                    never as root, and takes no arguments other than --version (and \
                    --forget-token, which the uninstaller uses).";

/// The executable's entry point.
#[cfg(target_os = "macos")]
pub fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => run(),
        [only] if only == "--version" => {
            println!("wgal-watchdog {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [only] if only == "--forget-token" => forget_token(),
        _ => {
            eprintln!("{HINT}");
            ExitCode::FAILURE
        }
    }
}

/// Removes this user's bot token from the login Keychain: success when it
/// was removed or there was none, failure when the Keychain refused.
/// Nothing of the token is printed.
#[cfg(target_os = "macos")]
fn forget_token() -> std::process::ExitCode {
    use std::process::ExitCode;

    use wgal_platform::macos::socket::{current_uid, effective_uid};

    use crate::notify::{KeychainSecretStore, SecretStore};
    use crate::socket::host::EXIT_USER;

    if current_uid() == 0 || effective_uid() == 0 {
        eprintln!("wgal-watchdog: run --forget-token as the user whose token it is, not as root");
        return ExitCode::from(EXIT_USER);
    }
    match KeychainSecretStore::new().clear() {
        Ok(()) => {
            println!("wgal-watchdog: no WGAL bot token is left in this user's login Keychain");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!(
                "wgal-watchdog: the WGAL bot token could not be removed from this user's login \
                 Keychain"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
fn run() -> std::process::ExitCode {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::process::ExitCode;
    use std::sync::Arc;

    use wgal_platform::macos::MacosAdapter;
    use wgal_platform::macos::socket::{
        current_uid, data_dir, effective_uid, home_dir, peer_uid, socket_path,
    };

    use crate::ipc;
    use crate::notify::KeychainSecretStore;
    use crate::socket::host::{EXIT_FOLDER, EXIT_USER, Layout, Platform, log};

    let uid = current_uid();
    if uid == 0 || effective_uid() == 0 {
        log("not started: the macOS watchdog runs as the signed-in user, never as root");
        eprintln!("{HINT}");
        return ExitCode::from(EXIT_USER);
    }
    let Some(home) = home_dir() else {
        log("not started: this user has no home folder");
        return ExitCode::from(EXIT_FOLDER);
    };
    let (Some(root), Some(socket)) = (data_dir(&home), socket_path(&home)) else {
        log("not started: the home folder's path is too long for the watchdog's socket");
        return ExitCode::from(EXIT_FOLDER);
    };
    // Made on first start; the umask cannot widen 0700.
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {
            if let Err(e) = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            {
                log(&format!(
                    "not started: the data folder cannot be made private ({})",
                    e.kind()
                ));
                return ExitCode::from(EXIT_FOLDER);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            log(&format!(
                "not started: the data folder cannot be made ({})",
                e.kind()
            ));
            return ExitCode::from(EXIT_FOLDER);
        }
    }
    let layout = Layout {
        root,
        socket,
        socket_mode: 0o600,
    };
    let identify = Arc::new(move |conn: &std::os::unix::net::UnixStream| {
        peer_uid(conn).ok().map(|peer| caller_for(peer, uid))
    });
    crate::socket::host::run(
        &layout,
        Platform {
            adapter: MacosAdapter::new(),
            secrets: KeychainSecretStore::new(),
            access: ipc::AccessList::default(),
            identify,
            own: caller_for(uid, uid),
            uid,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_user_is_admitted_and_counts_as_elevated() {
        let same = caller_for(501, 501);
        assert!(same.allowed && same.elevated);
        assert_eq!(same.account, "501");
        let other = caller_for(502, 501);
        assert!(!other.allowed && !other.elevated);
        let root = caller_for(0, 501);
        assert!(!root.allowed && !root.elevated);
    }
}
