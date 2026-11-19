//! The installation's access list, `<ProgramData>\WGAL\access.txt` (plan
//! §9.2): which accounts may use the watchdog's pipe besides SYSTEM and
//! elevated administrators, and which tunnels may be registered. An
//! administrator (later the installer) writes it; the watchdog only reads it.
//!
//! # Format
//!
//! UTF-8 (a leading byte order mark is ignored), at most
//! [`MAX_ACCESS_FILE_BYTES`], one entry per line:
//!
//! ```text
//! # comment
//! user S-1-5-21-1000000000-2000000000-3000000000-1001
//! tunnel office
//! ```
//!
//! Blank lines and lines starting with `#` (after leading blanks) are
//! ignored; there are no comments after an entry. A `user` is a local,
//! domain or Microsoft account SID (`S-1-5-21-` with four more fields) or a
//! Microsoft Entra ID user SID (`S-1-12-1-` with four more fields), in the
//! canonical form Windows prints, checked by converting it to a SID and back.
//! Group and well-known SIDs are refused: the watchdog compares only the
//! connecting token's user, so a group would never match, and the pipe's
//! DACL would grant it to every member. A `tunnel` is a WireGuard for Windows
//! tunnel name as the adapter validates it
//! ([`TunnelName`](wgal_platform::windows::TunnelName)), spelled exactly as
//! the official manager names it. The same user, or a tunnel name differing
//! only in case (the SCM compares service names that way), may appear once.
//!
//! Anything else rejects the whole file: the watchdog then runs with no
//! listed users and no tunnels (only SYSTEM and elevated administrators can
//! connect) and reports the [`AccessError`], which names the line and the
//! problem but never quotes the file.
//!
//! # Where it is read from
//!
//! [`AccessList::load_installation`] finds ProgramData through the shell,
//! never an environment variable, and refuses the `WGAL` folder or the file
//! if either is a symlink, junction or other reparse point, if the folder
//! is not a folder or the file not a file, and if either is owned by anyone
//! but SYSTEM or Administrators or lets anyone else change it. Each is opened
//! without following reparse points and held open, with no delete sharing,
//! while it is checked and read, so neither can be swapped in between.
//! Whoever can change the list decides who may control tunnels, so it must
//! be as protected as the service itself.
//!
//! A changed list takes effect when the service restarts: the pipe's DACL is
//! fixed when the pipe is created.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use wgal_core::TunnelBinding;
use wgal_platform::windows::{
    PIPE_CLIENT_ACCESS, TunnelName, is_canonical_sid, is_reparse_point, object_security,
    open_file_no_follow, open_folder_no_follow, program_data_dir,
};

/// The installation's folder under ProgramData.
pub const ACCESS_DIR: &str = "WGAL";
/// The access list's file in [`ACCESS_DIR`].
pub const ACCESS_FILE: &str = "access.txt";
/// Largest access file read.
pub const MAX_ACCESS_FILE_BYTES: usize = 16 * 1024;

/// SID prefixes of user accounts, each followed by exactly
/// [`ACCOUNT_SID_FIELDS`] more fields: local, domain and Microsoft accounts
/// (`S-1-5-21-<domain>-<rid>`), and Microsoft Entra ID users.
const ACCOUNT_SID_PREFIXES: [&str; 2] = ["S-1-5-21-", "S-1-12-1-"];
const ACCOUNT_SID_FIELDS: usize = 4;

/// A user account SID allowed on the access list: canonical, and of a user
/// account rather than a group or well-known principal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserSid(String);

impl UserSid {
    pub fn new(text: &str) -> Result<Self, LineProblem> {
        if !is_canonical_sid(text) {
            return Err(LineProblem::InvalidSid);
        }
        let account = ACCOUNT_SID_PREFIXES.iter().any(|prefix| {
            text.strip_prefix(prefix)
                .is_some_and(|rest| rest.split('-').count() == ACCOUNT_SID_FIELDS)
        });
        if account {
            Ok(Self(text.to_owned()))
        } else {
            Err(LineProblem::NotAUserAccount)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserSid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What is wrong with one line of the access file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineProblem {
    /// Neither `user` nor `tunnel`.
    UnknownEntry,
    /// The entry has no value, or more than one.
    NotOneValue,
    /// A control character other than a tab.
    ControlCharacter,
    InvalidSid,
    /// A valid SID, but of a group or well-known principal.
    NotAUserAccount,
    InvalidTunnelName,
    DuplicateUser,
    DuplicateTunnel,
}

impl fmt::Display for LineProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownEntry => "not a `user` or `tunnel` entry",
            Self::NotOneValue => "an entry needs exactly one value",
            Self::ControlCharacter => "contains a control character",
            Self::InvalidSid => "not a SID in canonical form",
            Self::NotAUserAccount => "not the SID of a user account",
            Self::InvalidTunnelName => "not a WireGuard for Windows tunnel name",
            Self::DuplicateUser => "the user is listed twice",
            Self::DuplicateTunnel => "the tunnel is listed twice (names compare ignoring case)",
        })
    }
}

/// Why the access list could not be used. The watchdog then serves no
/// listed users and no tunnels.
#[derive(Debug)]
pub enum AccessError {
    /// The ProgramData folder could not be found (its Windows error code).
    ProgramData(u32),
    /// The folder or the file does not exist.
    Missing,
    /// The folder or the file is a symlink, junction or other reparse point.
    ReparsePoint,
    /// The folder is not a folder, or the file not a regular file.
    WrongKind,
    /// The folder or the file is owned by someone other than SYSTEM or
    /// Administrators, or lets someone else change it.
    NotProtected,
    /// Its security could not be read (the Windows error code).
    Security(u32),
    TooLarge,
    NotUtf8,
    Line {
        number: usize,
        problem: LineProblem,
    },
    Io(io::Error),
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProgramData(code) => {
                write!(
                    f,
                    "the ProgramData folder was not found (Windows error {code})"
                )
            }
            Self::Missing => write!(f, "{ACCESS_DIR}\\{ACCESS_FILE} does not exist"),
            Self::ReparsePoint => write!(
                f,
                "{ACCESS_DIR}\\{ACCESS_FILE} or its folder is a link or junction"
            ),
            Self::WrongKind => write!(f, "{ACCESS_DIR}\\{ACCESS_FILE} is not a file in a folder"),
            Self::NotProtected => write!(
                f,
                "{ACCESS_DIR}\\{ACCESS_FILE} or its folder can be changed by someone other \
                 than SYSTEM and Administrators"
            ),
            Self::Security(code) => write!(
                f,
                "the security of {ACCESS_DIR}\\{ACCESS_FILE} could not be read \
                 (Windows error {code})"
            ),
            Self::TooLarge => write!(
                f,
                "{ACCESS_FILE} is larger than {MAX_ACCESS_FILE_BYTES} bytes"
            ),
            Self::NotUtf8 => write!(f, "{ACCESS_FILE} is not UTF-8"),
            Self::Line { number, problem } => write!(f, "{ACCESS_FILE} line {number}: {problem}"),
            Self::Io(e) => write!(f, "{ACCESS_FILE} could not be read: {e}"),
        }
    }
}

impl std::error::Error for AccessError {}

impl From<io::Error> for AccessError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Self::Missing
        } else {
            Self::Io(e)
        }
    }
}

/// Whether [`AccessList::load_dir`] requires the folder and file to be
/// changeable by SYSTEM and Administrators only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protection {
    AdminOnly,
    /// Only for tests in a temporary folder the test account owns.
    #[cfg(test)]
    Unchecked,
}

/// The accounts listed to use the pipe and the tunnels that may be
/// registered. [`Default`] is the fail-closed list: nobody, nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessList {
    users: BTreeSet<UserSid>,
    tunnels: BTreeSet<TunnelBinding>,
}

impl AccessList {
    /// Reads and checks `<ProgramData>\WGAL\access.txt` as the module docs
    /// describe. On error, use [`AccessList::default`] and report the error.
    pub fn load_installation() -> Result<Self, AccessError> {
        let program_data = program_data_dir().map_err(|e| AccessError::ProgramData(e.code))?;
        Self::load_dir(&program_data.join(ACCESS_DIR), Protection::AdminOnly)
    }

    pub(crate) fn load_dir(dir: &Path, protection: Protection) -> Result<Self, AccessError> {
        let folder = open_folder_no_follow(dir)?;
        check(&folder, protection, true)?;
        let file = open_file_no_follow(&dir.join(ACCESS_FILE))?;
        check(&file, protection, false)?;
        let mut bytes = Vec::new();
        file.take(MAX_ACCESS_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        // The folder is held until the file has been read.
        drop(folder);
        if bytes.len() > MAX_ACCESS_FILE_BYTES {
            return Err(AccessError::TooLarge);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| AccessError::NotUtf8)?;
        Self::parse(text)
    }

    /// Parses the access file's text. Any problem rejects all of it.
    pub fn parse(text: &str) -> Result<Self, AccessError> {
        if text.len() > MAX_ACCESS_FILE_BYTES {
            return Err(AccessError::TooLarge);
        }
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut list = Self::default();
        let mut tunnel_keys = BTreeSet::new();
        for (index, line) in text.lines().enumerate() {
            let fail = |problem| AccessError::Line {
                number: index + 1,
                problem,
            };
            if line.chars().any(|c| c.is_control() && c != '\t') {
                return Err(fail(LineProblem::ControlCharacter));
            }
            let line = line.trim_matches([' ', '\t']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split([' ', '\t']).filter(|f| !f.is_empty());
            let (Some(entry), value, None) = (fields.next(), fields.next(), fields.next()) else {
                return Err(fail(LineProblem::NotOneValue));
            };
            if entry != "user" && entry != "tunnel" {
                return Err(fail(LineProblem::UnknownEntry));
            }
            let value = value.ok_or_else(|| fail(LineProblem::NotOneValue))?;
            if entry == "user" {
                let user = UserSid::new(value).map_err(fail)?;
                if !list.users.insert(user) {
                    return Err(fail(LineProblem::DuplicateUser));
                }
            } else {
                let name =
                    TunnelName::new(value).ok_or_else(|| fail(LineProblem::InvalidTunnelName))?;
                if !tunnel_keys.insert(name.as_str().to_ascii_lowercase()) {
                    return Err(fail(LineProblem::DuplicateTunnel));
                }
                list.tunnels.insert(name.binding());
            }
        }
        Ok(list)
    }

    pub fn users(&self) -> &BTreeSet<UserSid> {
        &self.users
    }

    pub fn tunnels(&self) -> &BTreeSet<TunnelBinding> {
        &self.tunnels
    }

    /// Whether `sid`, a connecting token's user, is listed.
    pub fn lists_user(&self, sid: &str) -> bool {
        self.users.iter().any(|user| user.as_str() == sid)
    }

    /// The pipe's DACL in SDDL: protected from inheritance (`D:P`); NETWORK
    /// and ANONYMOUS LOGON denied everything; SYSTEM and Administrators
    /// full access; each listed user exactly [`PIPE_CLIENT_ACCESS`] (read,
    /// write data, read control, synchronize: not create-instance, which on
    /// a pipe is append-data, and not `WRITE_DAC` or `WRITE_OWNER`). For
    /// example, with one listed user:
    ///
    /// ```text
    /// D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12008b;;;S-1-5-21-…-1001)
    /// ```
    ///
    /// Without a mandatory label the pipe has the default medium one, so a
    /// low-integrity process cannot write to it even as a listed user.
    pub fn pipe_dacl(&self) -> String {
        let mut dacl = String::from("D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;BA)");
        for user in &self.users {
            // Every SID here is canonical: `S-1-` then digits and dashes, so
            // it cannot close the entry or add another.
            dacl.push_str(&format!("(A;;{PIPE_CLIENT_ACCESS:#x};;;{user})"));
        }
        dacl
    }

    /// The pipe's whole security descriptor: owned by SYSTEM (`O:SY`), with
    /// [`pipe_dacl`](Self::pipe_dacl). Only a process running as SYSTEM can
    /// create a pipe with it, so a client can tell the watchdog's pipe from
    /// one another account created under the same name by its owner.
    pub fn pipe_sddl(&self) -> String {
        format!("O:SY{}", self.pipe_dacl())
    }
}

/// Checks a folder of the installation the way [`AccessList::load_dir`]
/// checks the access file's: opened without following a reparse point, not
/// one, a folder, and with [`Protection::AdminOnly`] owned and changeable by
/// SYSTEM and Administrators only.
pub(crate) fn check_folder(dir: &Path, protection: Protection) -> Result<(), AccessError> {
    check(&open_folder_no_follow(dir)?, protection, true)
}

fn check(file: &File, protection: Protection, folder: bool) -> Result<(), AccessError> {
    if is_reparse_point(file)? {
        return Err(AccessError::ReparsePoint);
    }
    let meta = file.metadata()?;
    let right_kind = if folder {
        meta.is_dir()
    } else {
        meta.is_file()
    };
    if !right_kind {
        return Err(AccessError::WrongKind);
    }
    match protection {
        Protection::AdminOnly => {
            let security = object_security(file).map_err(|e| AccessError::Security(e.code))?;
            if !security.owner_privileged || security.others_may_change {
                return Err(AccessError::NotProtected);
            }
        }
        #[cfg(test)]
        Protection::Unchecked => {}
    }
    if !folder && meta.len() > MAX_ACCESS_FILE_BYTES as u64 {
        return Err(AccessError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    // Invented identifiers only.
    const ALICE: &str = "S-1-5-21-1000000000-2000000000-3000000000-1001";
    const BOB: &str = "S-1-5-21-1000000000-2000000000-3000000000-1002";
    const ENTRA: &str = "S-1-12-1-1000000000-2000000000-3000000000-4000000000";

    fn line_problem(text: &str) -> (usize, LineProblem) {
        match AccessList::parse(text) {
            Err(AccessError::Line { number, problem }) => (number, problem),
            other => panic!("expected a line error for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn parses_users_tunnels_comments_and_blank_lines() {
        let text = format!(
            "\u{feff}# who may use WGAL\r\n\r\nuser {ALICE}\r\n  user\t{ENTRA}  \n\
             \t# office tunnels\ntunnel office\ntunnel lab-2\n\n"
        );
        let list = AccessList::parse(&text).unwrap();
        let users: Vec<_> = list.users().iter().map(UserSid::as_str).collect();
        // Sorted as text: `S-1-12-` before `S-1-5-`.
        assert_eq!(users, [ENTRA, ALICE]);
        let tunnels: Vec<_> = list.tunnels().iter().map(TunnelBinding::as_str).collect();
        assert_eq!(tunnels, ["lab-2", "office"]);
        assert!(list.lists_user(ALICE));
        assert!(!list.lists_user(BOB));
        assert_eq!(AccessList::parse("").unwrap(), AccessList::default());
    }

    #[test]
    fn rejects_entries_it_does_not_know() {
        assert_eq!(line_problem("user"), (1, LineProblem::NotOneValue));
        assert_eq!(line_problem("tunnel"), (1, LineProblem::NotOneValue));
        assert_eq!(
            line_problem(&format!("user {ALICE} {BOB}")),
            (1, LineProblem::NotOneValue)
        );
        assert_eq!(
            line_problem(&format!("user {ALICE} # alice")),
            (1, LineProblem::NotOneValue)
        );
        assert_eq!(
            line_problem(&format!("\nUser {ALICE}")),
            (2, LineProblem::UnknownEntry)
        );
        assert_eq!(
            line_problem("group S-1-5-32-545"),
            (1, LineProblem::UnknownEntry)
        );
        assert_eq!(line_problem("allow all"), (1, LineProblem::UnknownEntry));
        assert_eq!(
            line_problem("tunnel office\u{0}"),
            (1, LineProblem::ControlCharacter)
        );
        assert_eq!(
            line_problem("tunnel office\u{1b}[0m"),
            (1, LineProblem::ControlCharacter)
        );
    }

    #[test]
    fn only_canonical_user_account_sids_are_listed() {
        for bad in [
            "s-1-5-21-1000000000-2000000000-3000000000-1001",
            "S-1-5-21-1000000000-2000000000-3000000000-01001",
            "S-1-5-21-0x3B9ACA00-2000000000-3000000000-1001",
            "S-1-5-21-1000000000-2000000000-3000000000-1001)(A;;GA;;;WD)",
            "BA",
            "WD",
            "alice",
            "DESKTOP\\alice",
        ] {
            assert_eq!(
                line_problem(&format!("user {bad}")),
                (1, LineProblem::InvalidSid),
                "{bad}"
            );
        }
        for group in [
            "S-1-1-0",
            "S-1-5-7",
            "S-1-5-11",
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-5-32-545",
            // A domain, not an account in it; and one field too many.
            "S-1-5-21-1000000000-2000000000-3000000000",
            "S-1-5-21-1000000000-2000000000-3000000000-1001-1",
            "S-1-12-1-1000000000-2000000000-3000000000",
        ] {
            assert_eq!(
                line_problem(&format!("user {group}")),
                (1, LineProblem::NotAUserAccount),
                "{group}"
            );
        }
    }

    #[test]
    fn tunnels_use_the_adapter_rules() {
        let long = "x".repeat(33);
        for bad in [
            "has\u{a0}space",
            long.as_str(),
            "CON",
            "nul",
            ".",
            "..",
            "a/b",
            "a\\b",
            "WireGuardTunnel$office",
            "t\u{fc}nnel",
        ] {
            assert_eq!(
                line_problem(&format!("tunnel {bad}")),
                (1, LineProblem::InvalidTunnelName),
                "{bad}"
            );
        }
    }

    #[test]
    fn duplicates_reject_the_file() {
        assert_eq!(
            line_problem(&format!("user {ALICE}\nuser {ALICE}")),
            (2, LineProblem::DuplicateUser)
        );
        assert_eq!(
            line_problem("tunnel office\ntunnel Office"),
            (2, LineProblem::DuplicateTunnel)
        );
    }

    #[test]
    fn a_text_over_the_limit_is_refused_whole() {
        let mut text = format!("tunnel office\n{}", "#".repeat(MAX_ACCESS_FILE_BYTES));
        assert!(matches!(
            AccessList::parse(&text),
            Err(AccessError::TooLarge)
        ));
        text.truncate(MAX_ACCESS_FILE_BYTES);
        assert!(AccessList::parse(&text).is_ok());
    }

    #[test]
    fn the_pipe_dacl_lists_each_user_with_client_access_only() {
        assert_eq!(
            AccessList::default().pipe_dacl(),
            "D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;BA)"
        );
        let list = AccessList::parse(&format!("user {BOB}\nuser {ALICE}\ntunnel office")).unwrap();
        assert_eq!(
            list.pipe_dacl(),
            format!(
                "D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;BA)\
                 (A;;0x12008b;;;{ALICE})(A;;0x12008b;;;{BOB})"
            )
        );
        assert_eq!(list.pipe_sddl(), format!("O:SY{}", list.pipe_dacl()));
        // FILE_GENERIC_READ | FILE_WRITE_DATA: no append data (create
        // instance), WRITE_DAC, WRITE_OWNER or DELETE.
        assert_eq!(PIPE_CLIENT_ACCESS, 0x0012_008b);
        assert_eq!(
            PIPE_CLIENT_ACCESS & (0x4 | 0x0004_0000 | 0x0008_0000 | 0x0001_0000),
            0
        );
    }

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A fresh directory under the temp folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("wgal-access-{}-{name}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, text: &[u8]) {
            fs::write(self.0.join(ACCESS_FILE), text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_a_file_from_a_folder() {
        let dir = Scratch::new("load");
        dir.write(format!("user {ALICE}\ntunnel office\n").as_bytes());
        let list = AccessList::load_dir(&dir.0, Protection::Unchecked).unwrap();
        assert!(list.lists_user(ALICE));
        assert_eq!(list.tunnels().len(), 1);
    }

    #[test]
    fn a_missing_folder_or_file_is_missing() {
        let dir = Scratch::new("missing");
        let none = AccessList::load_dir(&dir.0.join("nope"), Protection::Unchecked);
        assert!(matches!(none, Err(AccessError::Missing)), "{none:?}");
        let none = AccessList::load_dir(&dir.0, Protection::Unchecked);
        assert!(matches!(none, Err(AccessError::Missing)), "{none:?}");
    }

    #[test]
    fn oversized_non_utf8_and_misplaced_files_are_refused() {
        let dir = Scratch::new("bad");
        dir.write(&vec![b'#'; MAX_ACCESS_FILE_BYTES + 1]);
        let read = AccessList::load_dir(&dir.0, Protection::Unchecked);
        assert!(matches!(read, Err(AccessError::TooLarge)), "{read:?}");
        dir.write(b"tunnel off\xffice\n");
        let read = AccessList::load_dir(&dir.0, Protection::Unchecked);
        assert!(matches!(read, Err(AccessError::NotUtf8)), "{read:?}");

        let nested = Scratch::new("kind");
        fs::create_dir(nested.0.join(ACCESS_FILE)).unwrap();
        let read = AccessList::load_dir(&nested.0, Protection::Unchecked);
        assert!(
            matches!(read, Err(AccessError::WrongKind | AccessError::Io(_))),
            "{read:?}"
        );
        let file_as_dir = nested.0.join("plain");
        fs::write(&file_as_dir, b"").unwrap();
        let read = AccessList::load_dir(&file_as_dir, Protection::Unchecked);
        assert!(
            matches!(read, Err(AccessError::WrongKind | AccessError::Io(_))),
            "{read:?}"
        );
    }

    /// A folder the test account created is neither owned by an
    /// administrator principal alone nor closed to that account, so the
    /// installation's checks refuse it, whether or not the test is elevated.
    #[test]
    fn a_folder_the_user_can_change_is_not_trusted() {
        let dir = Scratch::new("unprotected");
        dir.write(format!("user {ALICE}\n").as_bytes());
        let read = AccessList::load_dir(&dir.0, Protection::AdminOnly);
        assert!(matches!(read, Err(AccessError::NotProtected)), "{read:?}");
    }

    /// Creating symlinks needs Developer Mode or the symlink privilege;
    /// each check is skipped where neither is available.
    #[test]
    fn links_are_refused() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let real = Scratch::new("real");
        real.write(format!("user {ALICE}\n").as_bytes());

        let links = Scratch::new("links");
        let folder_link = links.0.join("folder");
        if symlink_dir(&real.0, &folder_link).is_ok() {
            let read = AccessList::load_dir(&folder_link, Protection::Unchecked);
            assert!(matches!(read, Err(AccessError::ReparsePoint)), "{read:?}");
        }
        if symlink_file(real.0.join(ACCESS_FILE), links.0.join(ACCESS_FILE)).is_ok() {
            let read = AccessList::load_dir(&links.0, Protection::Unchecked);
            assert!(matches!(read, Err(AccessError::ReparsePoint)), "{read:?}");
        }
    }

    #[test]
    fn errors_never_quote_the_file() {
        let secretish = "tunnel has-a-very-distinctive-name-that-is-too-long";
        let error = AccessList::parse(secretish).unwrap_err().to_string();
        assert!(!error.contains("distinctive"), "{error}");
        assert_eq!(
            error,
            "access.txt line 1: not a WireGuard for Windows tunnel name"
        );
    }
}
