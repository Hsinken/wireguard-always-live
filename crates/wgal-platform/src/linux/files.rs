//! File ownership checks for registering a wg-quick tunnel (plan §4.4, §9.2).
//!
//! The watchdog restarts wg-quick units with privileges, and wg-quick runs
//! whatever the unit, its drop-ins and the tunnel's configuration (PostUp and
//! friends) say. Before a tunnel is registered, each of those files and every
//! directory above it must be owned by root and not writable by group or
//! others, so an ordinary user cannot turn a restart into running their own
//! code. Only metadata is read, never file contents. Symbolic links are
//! refused rather than followed: a link's target could sit under a directory
//! nobody checked.

use std::fmt;
use std::io;

use wgal_core::AdapterError;
use wgal_protocol::AdapterErrorKind;

/// What the check needs to know about one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFacts {
    pub kind: FileKind,
    pub uid: u32,
    /// Permission bits, as in `st_mode & 0o7777`.
    pub mode: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Reads file metadata without following a final symbolic link (`lstat`).
/// Implemented by [`SystemFiles`]; tests use an in-memory table.
pub trait FileInspector: Send + Sync + fmt::Debug {
    fn lstat(&self, path: &str) -> io::Result<FileFacts>;
}

/// The real [`FileInspector`]. Only Unix has the owner and mode bits the check
/// needs; elsewhere every lookup is `Unsupported`, so the check fails closed.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemFiles;

impl FileInspector for SystemFiles {
    #[cfg(unix)]
    fn lstat(&self, path: &str) -> io::Result<FileFacts> {
        use std::os::unix::fs::MetadataExt;

        let meta = std::fs::symlink_metadata(path)?;
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_dir() {
            FileKind::Directory
        } else if file_type.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };
        Ok(FileFacts {
            kind,
            uid: meta.uid(),
            mode: meta.mode() & 0o7777,
        })
    }

    #[cfg(not(unix))]
    fn lstat(&self, _path: &str) -> io::Result<FileFacts> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Group- or world-writable.
const SHARED_WRITE: u32 = 0o022;

fn conflict(path: &str, problem: &str) -> AdapterError {
    AdapterError::new(
        AdapterErrorKind::ManagerConflict,
        &format!("{path} {problem}; WGAL will not control this tunnel"),
    )
}

/// What a path given to [`check_file`] is, which decides what its absence
/// means. `NotFound` tells the core the binding is lost and the tunnel must
/// be registered again, so only the tunnel's own configuration being gone
/// may say that (plan §4.1, T23).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// `/etc/wireguard/<if>.conf`: gone means the tunnel is gone, `NotFound`.
    Config,
    /// A binary the wg-quick unit runs: gone is a missing tool,
    /// `Unsupported`, as [`crate::exec`] reports a tool it cannot run.
    Tool,
    /// The unit file or a drop-in systemd says it loaded: gone means systemd
    /// and the disk disagree (for example a daemon-reload is pending),
    /// `ManagerConflict`.
    Unit,
}

fn lookup_error(path: &str, error: &io::Error, role: Role) -> AdapterError {
    match error.kind() {
        io::ErrorKind::NotFound => {
            let (kind, problem) = match role {
                Role::Config => (AdapterErrorKind::NotFound, "does not exist"),
                Role::Tool => (
                    AdapterErrorKind::Unsupported,
                    "does not exist; wg-quick is not installed as expected",
                ),
                Role::Unit => (
                    AdapterErrorKind::ManagerConflict,
                    "is named by systemd but does not exist",
                ),
            };
            AdapterError::new(kind, &format!("{path} {problem}"))
        }
        io::ErrorKind::PermissionDenied => AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            &format!("{path} cannot be inspected"),
        ),
        _ => AdapterError::new(
            AdapterErrorKind::Unsupported,
            &format!("{path} cannot be inspected here"),
        ),
    }
}

/// `path` split into its components, if it is absolute and normalized: no
/// empty, `.` or `..` component and no trailing slash.
fn components(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    let normal = parts
        .iter()
        .all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains('\0'));
    normal.then_some(parts)
}

fn check_root_owned(
    files: &dyn FileInspector,
    path: &str,
    kind: FileKind,
    role: Role,
) -> Result<(), AdapterError> {
    let facts = files
        .lstat(path)
        .map_err(|e| lookup_error(path, &e, role))?;
    if facts.kind == FileKind::Symlink {
        return Err(conflict(path, "is a symbolic link"));
    }
    if facts.kind != kind {
        return Err(conflict(path, "is not the expected kind of file"));
    }
    if facts.uid != 0 {
        return Err(conflict(path, "is not owned by root"));
    }
    if facts.mode & SHARED_WRITE != 0 {
        return Err(conflict(path, "is writable by group or others"));
    }
    Ok(())
}

/// Checks that `path` is a regular file and that it and every directory above
/// it are root-owned, not group- or world-writable and not symbolic links.
/// `role` says what it means if the file or a directory above it is missing.
pub fn check_file(files: &dyn FileInspector, path: &str, role: Role) -> Result<(), AdapterError> {
    let parts = components(path).ok_or_else(|| conflict(path, "is not a plain absolute path"))?;
    check_root_owned(files, "/", FileKind::Directory, role)?;
    let mut prefix = String::new();
    for (i, part) in parts.iter().enumerate() {
        prefix.push('/');
        prefix.push_str(part);
        let kind = if i + 1 == parts.len() {
            FileKind::File
        } else {
            FileKind::Directory
        };
        check_root_owned(files, &prefix, kind, role)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fake {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    /// In-memory [`FileInspector`]. Paths not in the table do not exist.
    #[derive(Debug, Default)]
    pub struct FakeFiles {
        table: Mutex<BTreeMap<String, Result<FileFacts, io::ErrorKind>>>,
    }

    impl FakeFiles {
        /// A root-owned tree holding the wg-quick unit, its binaries and the
        /// given tunnels' configurations, as a packaged Ubuntu install has it.
        pub fn ubuntu(tunnels: &[&str]) -> Self {
            let files = Self::default();
            for dir in [
                "/",
                "/usr",
                "/usr/lib",
                "/usr/lib/systemd",
                "/usr/lib/systemd/system",
                "/usr/bin",
                "/etc",
                "/etc/wireguard",
                "/etc/systemd",
                "/etc/systemd/system",
            ] {
                files.set(dir, FileKind::Directory, 0, 0o755);
            }
            files.set("/etc/wireguard", FileKind::Directory, 0, 0o700);
            files.set(
                "/usr/lib/systemd/system/wg-quick@.service",
                FileKind::File,
                0,
                0o644,
            );
            files.set("/usr/bin/wg-quick", FileKind::File, 0, 0o755);
            files.set("/usr/bin/wg", FileKind::File, 0, 0o755);
            for tunnel in tunnels {
                files.set(
                    &format!("/etc/wireguard/{tunnel}.conf"),
                    FileKind::File,
                    0,
                    0o600,
                );
            }
            files
        }

        pub fn set(&self, path: &str, kind: FileKind, uid: u32, mode: u32) {
            self.lock()
                .insert(path.to_owned(), Ok(FileFacts { kind, uid, mode }));
        }

        pub fn fail(&self, path: &str, error: io::ErrorKind) {
            self.lock().insert(path.to_owned(), Err(error));
        }

        pub fn remove(&self, path: &str) {
            self.lock().remove(path);
        }

        fn lock(
            &self,
        ) -> std::sync::MutexGuard<'_, BTreeMap<String, Result<FileFacts, io::ErrorKind>>> {
            self.table
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    impl FileInspector for FakeFiles {
        fn lstat(&self, path: &str) -> io::Result<FileFacts> {
            match self.lock().get(path) {
                Some(Ok(facts)) => Ok(*facts),
                Some(Err(kind)) => Err(io::Error::from(*kind)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeFiles;
    use super::*;

    const CONF: &str = "/etc/wireguard/wg-office.conf";

    fn kind(result: Result<(), AdapterError>) -> AdapterErrorKind {
        result.unwrap_err().kind
    }

    fn check_conf(files: &FakeFiles) -> Result<(), AdapterError> {
        check_file(files, CONF, Role::Config)
    }

    #[test]
    fn a_packaged_root_owned_tree_passes() {
        let files = FakeFiles::ubuntu(&["wg-office"]);
        check_conf(&files).unwrap();
        check_file(
            &files,
            "/usr/lib/systemd/system/wg-quick@.service",
            Role::Unit,
        )
        .unwrap();
        check_file(&files, "/usr/bin/wg-quick", Role::Tool).unwrap();
    }

    #[test]
    fn user_owned_or_shared_writable_files_and_directories_are_refused() {
        let cases: [(&str, u32, u32); 5] = [
            (CONF, 1000, 0o600),
            (CONF, 0, 0o620),
            (CONF, 0, 0o602),
            ("/etc/wireguard", 1000, 0o700),
            ("/etc", 0, 0o777),
        ];
        for (path, uid, mode) in cases {
            let files = FakeFiles::ubuntu(&["wg-office"]);
            let kind_of = if path == CONF {
                FileKind::File
            } else {
                FileKind::Directory
            };
            files.set(path, kind_of, uid, mode);
            assert_eq!(
                kind(check_conf(&files)),
                AdapterErrorKind::ManagerConflict,
                "{path}"
            );
        }
    }

    #[test]
    fn symbolic_links_anywhere_on_the_path_are_refused() {
        for path in [CONF, "/etc/wireguard"] {
            let files = FakeFiles::ubuntu(&["wg-office"]);
            files.set(path, FileKind::Symlink, 0, 0o777);
            assert_eq!(
                kind(check_conf(&files)),
                AdapterErrorKind::ManagerConflict,
                "{path}"
            );
        }
    }

    #[test]
    fn wrong_kinds_and_unnormalized_paths_are_refused() {
        let files = FakeFiles::ubuntu(&["wg-office"]);
        files.set(CONF, FileKind::Directory, 0, 0o700);
        assert_eq!(kind(check_conf(&files)), AdapterErrorKind::ManagerConflict);

        let files = FakeFiles::ubuntu(&["wg-office"]);
        for path in [
            "etc/wireguard/wg-office.conf",
            "/etc/wireguard/../wireguard/wg-office.conf",
            "/etc/./wireguard/wg-office.conf",
            "/etc//wireguard/wg-office.conf",
            "/etc/wireguard/",
            "",
        ] {
            assert_eq!(
                kind(check_file(&files, path, Role::Config)),
                AdapterErrorKind::ManagerConflict,
                "{path:?}"
            );
        }
    }

    #[test]
    fn lookup_failures_keep_their_meaning() {
        let files = FakeFiles::ubuntu(&[]);
        assert_eq!(kind(check_conf(&files)), AdapterErrorKind::NotFound);
        files.fail("/etc/wireguard", io::ErrorKind::PermissionDenied);
        assert_eq!(kind(check_conf(&files)), AdapterErrorKind::PermissionDenied);
    }

    #[test]
    fn only_a_missing_configuration_is_not_found() {
        // Missing as the file itself and as a directory above it.
        for missing in ["/etc/wireguard/wg-gone.conf", "/opt/wg/wg-gone.conf"] {
            let files = FakeFiles::ubuntu(&[]);
            let expected = [
                (Role::Config, AdapterErrorKind::NotFound),
                (Role::Tool, AdapterErrorKind::Unsupported),
                (Role::Unit, AdapterErrorKind::ManagerConflict),
            ];
            for (role, kind_of) in expected {
                assert_eq!(
                    kind(check_file(&files, missing, role)),
                    kind_of,
                    "{missing} {role:?}"
                );
            }
        }
        // Other lookup failures do not depend on the role.
        let files = FakeFiles::ubuntu(&["wg-office"]);
        files.fail("/usr/bin", io::ErrorKind::PermissionDenied);
        for role in [Role::Config, Role::Tool, Role::Unit] {
            assert_eq!(
                kind(check_file(&files, "/usr/bin/wg", role)),
                AdapterErrorKind::PermissionDenied,
                "{role:?}"
            );
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn the_real_inspector_fails_closed_off_unix() {
        assert_eq!(
            kind(check_file(&SystemFiles, CONF, Role::Config)),
            AdapterErrorKind::Unsupported
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_real_inspector_reads_root_owned_system_directories() {
        // Read-only metadata of directories every Unix has; nothing about tunnels.
        let facts = SystemFiles.lstat("/").unwrap();
        assert_eq!(facts.kind, FileKind::Directory);
        assert_eq!(facts.uid, 0);
    }
}
