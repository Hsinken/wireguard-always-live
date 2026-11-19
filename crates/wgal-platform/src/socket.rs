//! What the Linux and macOS watchdog sockets share (plan §3.2: "macOS／Ubuntu
//! 使用受保護目錄中的 Unix domain socket"): the facts a client checks about
//! the socket's folder and the socket itself before it connects, and the
//! error it returns when they say another user serves the socket. Each
//! platform's `socket` module (`linux::socket`, `macos::socket`) knows the
//! fixed path and the user the watchdog runs as; the rules are here, and
//! are pure, so they are tested on every OS.
//!
//! A socket path is trusted only through its folder: whoever can write into
//! the folder can replace the socket. So the folder must be a real folder
//! (not a symlink) owned by the watchdog's user, and nobody else may write
//! into it; the socket must be a socket, owned by that user too. After
//! connecting, the client also asks the kernel who the peer is
//! (`peer_uid`), which no file trick can fake.

use std::fmt;
use std::io;

/// What a client knows about a path without following it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The owner's user ID.
    pub uid: u32,
    /// The permission bits (`st_mode & 0o7777`).
    pub mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    Socket,
    Symlink,
    Other,
}

/// Whether `entry` is a folder the watchdog's socket can be trusted in: a
/// real folder owned by `owner`, which no group or other user may write
/// to. `private` also refuses every group and other permission, for a
/// folder that holds the user's own data (macOS).
pub fn trusted_folder(entry: &Entry, owner: u32, private: bool) -> bool {
    let refused = if private { 0o077 } else { 0o022 };
    entry.kind == EntryKind::Directory && entry.uid == owner && entry.mode & refused == 0
}

/// Whether `entry` is a socket owned by `owner`.
pub fn trusted_socket(entry: &Entry, owner: u32) -> bool {
    entry.kind == EntryKind::Socket && entry.uid == owner
}

/// The error a client returns when the socket is not the watchdog's: its
/// folder or the socket fails the checks above, or the kernel names another
/// peer. Nothing was written to it.
#[derive(Debug)]
struct NotTheWatchdog;

impl fmt::Display for NotTheWatchdog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the socket is not served by the WGAL watchdog")
    }
}

impl std::error::Error for NotTheWatchdog {}

/// The error for a socket that is not the watchdog's, as `PermissionDenied`
/// carrying a marker [`is_not_the_watchdog`] recognizes.
pub fn not_the_watchdog() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, NotTheWatchdog)
}

/// Whether `error` says the socket is not the watchdog's (see
/// [`not_the_watchdog`]); a plain permission error is not.
pub fn is_not_the_watchdog(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<NotTheWatchdog>())
}

/// `path`'s facts, without following a symlink.
#[cfg(unix)]
pub fn entry_of(path: &std::path::Path) -> io::Result<Entry> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path)?;
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_socket() {
        EntryKind::Socket
    } else {
        EntryKind::Other
    };
    Ok(Entry {
        kind,
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: EntryKind, uid: u32, mode: u32) -> Entry {
        Entry { kind, uid, mode }
    }

    #[test]
    fn a_folder_is_trusted_only_when_its_owner_alone_can_write_to_it() {
        assert!(trusted_folder(
            &entry(EntryKind::Directory, 0, 0o755),
            0,
            false
        ));
        assert!(trusted_folder(
            &entry(EntryKind::Directory, 0, 0o700),
            0,
            false
        ));
        // Another owner, a group or anyone able to write, not a folder.
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 1000, 0o755),
            0,
            false
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 0, 0o775),
            0,
            false
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 0, 0o757),
            0,
            false
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 0, 0o1777),
            0,
            false
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Symlink, 0, 0o777),
            0,
            false
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Other, 0, 0o755),
            0,
            false
        ));
        // A private folder: no group or other permission at all.
        assert!(trusted_folder(
            &entry(EntryKind::Directory, 501, 0o700),
            501,
            true
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 501, 0o750),
            501,
            true
        ));
        assert!(!trusted_folder(
            &entry(EntryKind::Directory, 501, 0o701),
            501,
            true
        ));
    }

    #[test]
    fn a_socket_is_trusted_only_as_a_socket_of_its_owner() {
        assert!(trusted_socket(&entry(EntryKind::Socket, 0, 0o666), 0));
        assert!(!trusted_socket(&entry(EntryKind::Socket, 1000, 0o666), 0));
        assert!(!trusted_socket(&entry(EntryKind::Other, 0, 0o666), 0));
        assert!(!trusted_socket(&entry(EntryKind::Symlink, 0, 0o777), 0));
    }

    #[test]
    fn the_marker_tells_the_watchdog_apart_from_a_plain_refusal() {
        assert!(is_not_the_watchdog(&not_the_watchdog()));
        assert_eq!(not_the_watchdog().kind(), io::ErrorKind::PermissionDenied);
        assert!(!is_not_the_watchdog(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!is_not_the_watchdog(&io::Error::other("x")));
    }
}
