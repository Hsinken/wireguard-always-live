//! The watchdog's socket on Ubuntu (plan §3.2): a Unix domain socket at the
//! fixed path [`SOCKET_PATH`], in [`RUNTIME_DIR`], which systemd creates for
//! the service (`RuntimeDirectory=wgal`) and only root can write into. The
//! watchdog runs as root; it reads each peer's user with [`peer_uid`] and
//! admits root and the users on the installation's access list. The window
//! and `wgal-mcp` connect with [`connect_watchdog_socket`], which refuses a
//! socket root does not serve before anything is written to it.

use std::path::Path;

use crate::socket::{Entry, trusted_folder, trusted_socket};

/// The folder of the socket, made by systemd for the service.
pub const RUNTIME_DIR: &str = "/run/wgal";

/// The watchdog's socket. Fixed: never configurable, never from a caller.
pub const SOCKET_PATH: &str = "/run/wgal/watchdog.sock";

/// The user the watchdog runs as.
pub const WATCHDOG_UID: u32 = 0;

/// Whether the socket's folder and the socket, as found, can be the
/// watchdog's: root's folder that only root can write to, and root's socket.
pub fn trusted(folder: &Entry, socket: &Entry) -> bool {
    trusted_folder(folder, WATCHDOG_UID, false) && trusted_socket(socket, WATCHDOG_UID)
}

/// The socket path as a [`Path`].
pub fn socket_path() -> &'static Path {
    Path::new(SOCKET_PATH)
}

/// The user of the process on the other end of a connected Unix socket, as
/// the kernel recorded it when the connection was made (`SO_PEERCRED`).
#[cfg(target_os = "linux")]
pub fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)?;
    Ok(credentials.uid.as_raw())
}

/// This process's effective user ID.
#[cfg(target_os = "linux")]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Connects to the watchdog's socket, only if root serves it: the folder and
/// the socket must pass [`trusted`], and the kernel must name root as the
/// peer. Otherwise the error is [`crate::socket::not_the_watchdog`] and
/// nothing was written. A missing socket is `NotFound`; one nobody listens
/// on, `ConnectionRefused`; one this user may not open, `PermissionDenied`.
#[cfg(target_os = "linux")]
pub fn connect_watchdog_socket() -> std::io::Result<std::os::unix::net::UnixStream> {
    use crate::socket::{entry_of, not_the_watchdog};
    let folder = entry_of(Path::new(RUNTIME_DIR))?;
    let socket = entry_of(socket_path())?;
    if !trusted(&folder, &socket) {
        return Err(not_the_watchdog());
    }
    let stream = std::os::unix::net::UnixStream::connect(SOCKET_PATH)?;
    if peer_uid(&stream)? != WATCHDOG_UID {
        return Err(not_the_watchdog());
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::EntryKind;

    fn entry(kind: EntryKind, uid: u32, mode: u32) -> Entry {
        Entry { kind, uid, mode }
    }

    #[test]
    fn only_roots_socket_in_roots_folder_is_the_watchdog() {
        let folder = entry(EntryKind::Directory, 0, 0o755);
        let socket = entry(EntryKind::Socket, 0, 0o666);
        assert!(trusted(&folder, &socket));
        // A folder anyone may write to, or a user's own, or a user's socket.
        assert!(!trusted(&entry(EntryKind::Directory, 0, 0o777), &socket));
        assert!(!trusted(&entry(EntryKind::Directory, 1000, 0o755), &socket));
        assert!(!trusted(&entry(EntryKind::Symlink, 0, 0o777), &socket));
        assert!(!trusted(&folder, &entry(EntryKind::Socket, 1000, 0o666)));
        assert!(!trusted(&folder, &entry(EntryKind::Other, 0, 0o666)));
    }

    #[test]
    fn the_path_is_fixed_inside_its_folder() {
        assert_eq!(socket_path().parent(), Some(Path::new(RUNTIME_DIR)));
        assert!(SOCKET_PATH.starts_with('/'));
    }
}
