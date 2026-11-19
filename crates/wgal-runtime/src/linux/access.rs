//! The installation's access list on Ubuntu, `/etc/wgal/access.conf`
//! ([`ACCESS_FILE`]; plan §9.2): which users may use the watchdog besides
//! root, and which tunnels they may register and hand to automatic
//! actions. The installer (`packaging/linux/install.sh`) writes it; the
//! watchdog only reads it, at start.
//!
//! # Format
//!
//! UTF-8, at most [`MAX_ACCESS_FILE_BYTES`], one entry per line:
//!
//! ```text
//! # comment
//! user 1000
//! tunnel wg0
//! ```
//!
//! Blank lines and lines starting with `#` (after leading blanks) are
//! ignored. A `user` is a numeric user ID other than root's (root needs no
//! entry); a `tunnel` is a wg-quick interface name as the adapter accepts
//! it ([`InterfaceName`]). Each may appear once. Anything else rejects the
//! whole file: the watchdog then runs with no listed users and no tunnels
//! (only root can use it) and says why in its log, by line number and
//! problem, never quoting the file.
//!
//! # Where it is read from
//!
//! The folder `/etc/wgal` and the file must be root's, writable by nobody
//! else, and not symlinks; the file is opened without following a symlink
//! and checked through the open handle. A missing file means no listed
//! users and no tunnels. A list that changed takes effect when the service
//! restarts.

use std::collections::BTreeSet;
use std::fmt;

use wgal_core::TunnelBinding;
use wgal_platform::linux::InterfaceName;

/// The folder of the access list.
pub const ACCESS_DIR: &str = "/etc/wgal";

/// The access list.
pub const ACCESS_FILE: &str = "/etc/wgal/access.conf";

/// The largest access list read.
pub const MAX_ACCESS_FILE_BYTES: usize = 64 * 1024;

/// What the access list allows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessList {
    pub users: BTreeSet<u32>,
    pub tunnels: BTreeSet<TunnelBinding>,
}

/// What is wrong with one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineProblem {
    /// Neither `user`, `tunnel`, a comment nor blank.
    UnknownEntry,
    /// The entry has no value, or more than one.
    WrongFieldCount,
    /// Not a decimal user ID other than root's.
    BadUser,
    /// Not a wg-quick interface name the adapter accepts.
    BadTunnel,
    /// The same user or tunnel again.
    Duplicate,
}

impl fmt::Display for LineProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownEntry => "not a user or tunnel entry",
            Self::WrongFieldCount => "an entry takes exactly one value",
            Self::BadUser => "not a numeric user ID other than root's",
            Self::BadTunnel => "not a wg-quick interface name",
            Self::Duplicate => "listed twice",
        })
    }
}

/// Why the access list was not used.
#[derive(Debug)]
pub enum AccessError {
    /// The folder or the file is not root's alone, or is a symlink.
    NotProtected,
    TooLarge,
    NotUtf8,
    Line {
        number: usize,
        problem: LineProblem,
    },
    Io(std::io::ErrorKind),
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotProtected => write!(
                f,
                "{ACCESS_DIR} or {ACCESS_FILE} is not root's alone, or is a symlink"
            ),
            Self::TooLarge => write!(
                f,
                "{ACCESS_FILE} is larger than {MAX_ACCESS_FILE_BYTES} bytes"
            ),
            Self::NotUtf8 => write!(f, "{ACCESS_FILE} is not UTF-8"),
            Self::Line { number, problem } => write!(f, "{ACCESS_FILE} line {number}: {problem}"),
            Self::Io(kind) => write!(f, "{ACCESS_FILE} cannot be read: {kind}"),
        }
    }
}

impl std::error::Error for AccessError {}

/// The access list in `text` (see the module documentation).
pub fn parse(text: &str) -> Result<AccessList, AccessError> {
    let mut list = AccessList::default();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let fail = |problem| AccessError::Line { number, problem };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.split_ascii_whitespace();
        let (Some(kind), Some(value), None) = (fields.next(), fields.next(), fields.next()) else {
            return Err(match trimmed.split_ascii_whitespace().next() {
                Some("user" | "tunnel") => fail(LineProblem::WrongFieldCount),
                _ => fail(LineProblem::UnknownEntry),
            });
        };
        match kind {
            "user" => {
                let uid = parse_uid(value).ok_or(fail(LineProblem::BadUser))?;
                if !list.users.insert(uid) {
                    return Err(fail(LineProblem::Duplicate));
                }
            }
            "tunnel" => {
                let name = InterfaceName::new(value).ok_or(fail(LineProblem::BadTunnel))?;
                if !list.tunnels.insert(name.binding()) {
                    return Err(fail(LineProblem::Duplicate));
                }
            }
            _ => return Err(fail(LineProblem::UnknownEntry)),
        }
    }
    Ok(list)
}

/// A decimal user ID, 1 to 4294967294: no sign, no leading zero, not root
/// (0) and not the "no user" value (4294967295).
fn parse_uid(value: &str) -> Option<u32> {
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if value.starts_with('0') {
        return None;
    }
    let uid: u32 = value.parse().ok()?;
    (uid != u32::MAX).then_some(uid)
}

/// Reads the installation's access list (see "Where it is read from").
#[cfg(target_os = "linux")]
pub fn load() -> Result<AccessList, AccessError> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use wgal_platform::socket::{entry_of, trusted_folder};

    let folder =
        entry_of(std::path::Path::new(ACCESS_DIR)).map_err(|e| AccessError::Io(e.kind()))?;
    if !trusted_folder(&folder, 0, false) {
        return Err(AccessError::NotProtected);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(ACCESS_FILE)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => AccessError::NotProtected,
            _ => AccessError::Io(e.kind()),
        })?;
    let meta = file.metadata().map_err(|e| AccessError::Io(e.kind()))?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(AccessError::NotProtected);
    }
    let mut bytes = Vec::new();
    let cap = u64::try_from(MAX_ACCESS_FILE_BYTES).unwrap_or(u64::MAX) + 1;
    file.take(cap)
        .read_to_end(&mut bytes)
        .map_err(|e| AccessError::Io(e.kind()))?;
    if bytes.len() > MAX_ACCESS_FILE_BYTES {
        return Err(AccessError::TooLarge);
    }
    let text = String::from_utf8(bytes).map_err(|_| AccessError::NotUtf8)?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(name: &str) -> TunnelBinding {
        InterfaceName::new(name).unwrap().binding()
    }

    #[test]
    fn users_and_tunnels_are_read_with_comments_and_blank_lines() {
        let list =
            parse("# who may use WGAL\n\nuser 1000\n  user 1001  \ntunnel wg0\ntunnel office-1\n")
                .unwrap();
        assert_eq!(list.users, BTreeSet::from([1000, 1001]));
        assert_eq!(
            list.tunnels,
            BTreeSet::from([binding("wg0"), binding("office-1")])
        );
        assert_eq!(parse("").unwrap(), AccessList::default());
    }

    #[test]
    fn anything_else_rejects_the_whole_file_by_line_and_problem() {
        let cases: [(&str, usize, LineProblem); 12] = [
            ("group 100", 1, LineProblem::UnknownEntry),
            ("user", 1, LineProblem::WrongFieldCount),
            ("user 1000 1001", 1, LineProblem::WrongFieldCount),
            ("user 0", 1, LineProblem::BadUser),
            ("user 01000", 1, LineProblem::BadUser),
            ("user -1", 1, LineProblem::BadUser),
            ("user 4294967295", 1, LineProblem::BadUser),
            ("user alice", 1, LineProblem::BadUser),
            ("tunnel all", 1, LineProblem::BadTunnel),
            ("tunnel ../wg0", 1, LineProblem::BadTunnel),
            ("user 1000\nuser 1000", 2, LineProblem::Duplicate),
            ("tunnel wg0\n\ntunnel wg0", 3, LineProblem::Duplicate),
        ];
        for (text, line, problem) in cases {
            match parse(text) {
                Err(AccessError::Line {
                    number,
                    problem: got,
                }) => {
                    assert_eq!((number, got), (line, problem), "{text:?}");
                }
                other => panic!("{text:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_largest_user_id_is_one_below_the_no_user_value() {
        assert_eq!(
            parse("user 4294967294").unwrap().users,
            BTreeSet::from([4294967294])
        );
    }

    #[test]
    fn an_error_names_the_line_and_problem_never_the_text() {
        let error = parse("user 1000\nsecret-looking-text").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("line 2"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }
}
