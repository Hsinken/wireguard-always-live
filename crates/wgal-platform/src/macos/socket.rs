//! The watchdog's socket on macOS (plan §3.1, §3.2): the watchdog is the
//! signed-in user's own LaunchAgent, so its socket lives in that user's
//! private folder, `~/Library/Application Support/WGAL` ([`DATA_DIR`],
//! mode 0700), as [`SOCKET_NAME`]. Only the same user is admitted: the
//! watchdog checks each peer with [`peer_uid`], and the window and
//! `wgal-mcp` connect with [`connect_watchdog_socket`], which refuses a
//! socket the user does not serve before anything is written to it.
//!
//! A Unix socket path on macOS holds at most [`MAX_SOCKET_PATH`] bytes; a
//! home folder too long for it gets no socket ([`socket_path`] is `None`),
//! and both sides say so instead of using a shortened or other path.

use std::path::{Path, PathBuf};

use crate::socket::{Entry, trusted_folder, trusted_socket};

/// The watchdog's folder, under the user's home folder.
pub const DATA_DIR: &str = "Library/Application Support/WGAL";

/// The socket's name in [`DATA_DIR`].
pub const SOCKET_NAME: &str = "watchdog.sock";

/// The longest path a Unix socket address takes on macOS: `sun_path` is
/// 104 bytes, one of them the terminating NUL.
pub const MAX_SOCKET_PATH: usize = 103;

/// The watchdog's folder for the user whose home is `home`, or `None` when
/// `home` does not start at the root (on macOS: is not absolute).
pub fn data_dir(home: &Path) -> Option<PathBuf> {
    home.has_root().then(|| home.join(DATA_DIR))
}

/// The socket for the user whose home is `home`, or `None` when `home` does
/// not start at the root or the path would not fit a socket address.
pub fn socket_path(home: &Path) -> Option<PathBuf> {
    let path = data_dir(home)?.join(SOCKET_NAME);
    (path.as_os_str().len() <= MAX_SOCKET_PATH).then_some(path)
}

/// Whether the folder and the socket, as found, can be this user's
/// watchdog: the user's own folder that nobody else may enter, and the
/// user's socket.
pub fn trusted(folder: &Entry, socket: &Entry, uid: u32) -> bool {
    trusted_folder(folder, uid, true) && trusted_socket(socket, uid)
}

/// The user of the process on the other end of a connected Unix socket, as
/// the kernel recorded it (`getpeereid`).
#[cfg(target_os = "macos")]
pub fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    let (uid, _) = nix::unistd::getpeereid(stream).map_err(std::io::Error::from)?;
    Ok(uid.as_raw())
}

/// This process's real user ID.
#[cfg(target_os = "macos")]
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// This process's effective user ID.
#[cfg(target_os = "macos")]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The user's home folder, as the C library and launchd give it.
#[cfg(target_os = "macos")]
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

/// Connects to this user's watchdog, only if this user serves it: the
/// folder and the socket must pass [`trusted`], and the kernel must name
/// this user as the peer. Otherwise the error is
/// [`crate::socket::not_the_watchdog`] and nothing was written. A missing
/// socket is `NotFound`; one nobody listens on, `ConnectionRefused`; a
/// home folder whose socket path is too long, `InvalidInput`.
#[cfg(target_os = "macos")]
pub fn connect_watchdog_socket() -> std::io::Result<std::os::unix::net::UnixStream> {
    use crate::socket::{entry_of, not_the_watchdog};
    use std::io::{Error, ErrorKind};
    let home = home_dir().ok_or_else(|| Error::from(ErrorKind::NotFound))?;
    let path = socket_path(&home).ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
    let folder = data_dir(&home).ok_or_else(|| Error::from(ErrorKind::NotFound))?;
    let uid = current_uid();
    if !trusted(&entry_of(&folder)?, &entry_of(&path)?, uid) {
        return Err(not_the_watchdog());
    }
    let stream = std::os::unix::net::UnixStream::connect(&path)?;
    if peer_uid(&stream)? != uid {
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
    fn the_socket_is_in_the_users_private_folder() {
        let folder = entry(EntryKind::Directory, 501, 0o700);
        let socket = entry(EntryKind::Socket, 501, 0o600);
        assert!(trusted(&folder, &socket, 501));
        // Another user's, or a folder others may enter, or a symlink.
        assert!(!trusted(&folder, &socket, 502));
        assert!(!trusted(
            &entry(EntryKind::Directory, 501, 0o755),
            &socket,
            501
        ));
        assert!(!trusted(
            &entry(EntryKind::Symlink, 501, 0o755),
            &socket,
            501
        ));
        assert!(!trusted(&folder, &entry(EntryKind::Socket, 0, 0o600), 501));
    }

    #[test]
    fn a_path_too_long_for_a_socket_address_gives_no_socket() {
        let home = Path::new("/Users/someone");
        let path = socket_path(home).expect("short enough");
        assert_eq!(
            path,
            PathBuf::from("/Users/someone/Library/Application Support/WGAL/watchdog.sock")
        );
        // "/Users/" and the rest take 54 bytes, leaving 49 for the name.
        let fits = format!("/Users/{}", "a".repeat(49));
        assert!(socket_path(Path::new(&fits)).is_some());
        let too_long = format!("/Users/{}", "a".repeat(50));
        assert_eq!(socket_path(Path::new(&too_long)), None);
        assert_eq!(socket_path(Path::new("relative/home")), None);
        assert_eq!(data_dir(Path::new("relative/home")), None);
    }
}
