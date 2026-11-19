//! Which MCP server is installed beside this window (owner request of
//! 2026-11-04): the version mark `wgal-mcp` carries in its executable
//! (`wgal_protocol::MCP_VERSION_MARK`), read from the file next to this
//! binary. The window never runs it (AGENTS.md rule 9). The path is fixed,
//! nothing from the webview names it, and a link there is not followed:
//! the file is opened as itself and judged from that one handle, so a link
//! put there after any check cannot be read through. The read is capped.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;
use wgal_protocol::{ProductVersion, mcp_version_in};

#[cfg(windows)]
use crate::export::FILE_FLAG_OPEN_REPARSE_POINT;

/// The MCP server's executable, beside the window's (the installer puts
/// them in one folder).
#[cfg(windows)]
const EXECUTABLE: &str = "wgal-mcp.exe";
#[cfg(not(windows))]
const EXECUTABLE: &str = "wgal-mcp";

/// Most bytes read from it: well above the server's few megabytes.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// What the window can say about the MCP server beside it: `installed` with
/// the version it carries, `missing` when there is no such file, or
/// `unreadable` when it cannot be read, is not a plain file, or carries no
/// version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum McpServer {
    Installed { version: ProductVersion },
    Missing,
    Unreadable,
}

/// The MCP server installed beside the running window.
pub(crate) fn installed() -> McpServer {
    match beside_window() {
        Some(path) => read(&path),
        None => McpServer::Unreadable,
    }
}

/// Where the MCP server beside the running window is, or would be.
pub(crate) fn beside_window() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(EXECUTABLE))
}

/// The MCP server beside the running window, only when one is installed
/// there (a plain file carrying the version mark): what the one-click
/// client setup writes into a tool's settings (`mcpclients`).
pub(crate) fn installed_path() -> Option<PathBuf> {
    let path = beside_window()?;
    matches!(read(&path), McpServer::Installed { .. }).then_some(path)
}

/// The MCP server at `path`: opened as itself, and by that handle a plain
/// file, not a link, of at most [`MAX_BYTES`], carrying the version mark.
fn read(path: &Path) -> McpServer {
    let file = match open_itself(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return McpServer::Missing,
        Err(_) => return McpServer::Unreadable,
    };
    // `is_symlink` is true for symbolic links and, on Windows, for every
    // name-surrogate reparse point, junctions included.
    let plain = file.metadata().is_ok_and(|meta| {
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_BYTES
    });
    if !plain {
        return McpServer::Unreadable;
    }
    let mut bytes = Vec::new();
    if file.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_BYTES {
        return McpServer::Unreadable;
    }
    match mcp_version_in(&bytes) {
        Some(version) => McpServer::Installed { version },
        None => McpServer::Unreadable,
    }
}

/// Opens `path` itself for reading, never what a link of that name points
/// to: on Windows a reparse point there is opened as itself, and [`read`]
/// then refuses it as a link; elsewhere a symbolic link there does not open
/// (`O_NOFOLLOW`), and a pipe put there does not hold the read up
/// (`O_NONBLOCK`).
pub(crate) fn open_itself(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    /// Windows' `ERROR_PRIVILEGE_NOT_HELD` (winerror.h): the account may not
    /// make symbolic links.
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wgal-mcp-version-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_version_is_read_from_the_mark() {
        let dir = temp_dir("mark");
        let path = dir.join(EXECUTABLE);
        fs::write(&path, b"MZ\x90\x00junk WGAL-MCP-VERSION=1.2.1\x00 more").unwrap();
        assert_eq!(
            read(&path),
            McpServer::Installed {
                version: ProductVersion::new("1.2.1").unwrap()
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_a_folder_or_no_mark_is_said_so() {
        let dir = temp_dir("odd");
        assert_eq!(read(&dir.join(EXECUTABLE)), McpServer::Missing);
        let folder = dir.join("folder");
        fs::create_dir(&folder).unwrap();
        assert_eq!(read(&folder), McpServer::Unreadable);
        let plain = dir.join("plain");
        fs::write(&plain, b"an executable from before the mark").unwrap();
        assert_eq!(read(&plain), McpServer::Unreadable);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_over_the_cap_is_not_read() {
        let dir = temp_dir("large");
        let path = dir.join(EXECUTABLE);
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(read(&path), McpServer::Unreadable);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_at_the_path_is_not_followed() {
        let dir = temp_dir("link");
        let target = dir.join("elsewhere");
        fs::write(&target, b"WGAL-MCP-VERSION=9.9.9\x00").unwrap();
        let link = dir.join(EXECUTABLE);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &link);
        if let Err(e) = made {
            // Windows makes file links only with the privilege for it; any
            // other failure to make one fails the test.
            assert!(
                cfg!(windows) && e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD),
                "cannot make a file link: {e}"
            );
            eprintln!("SKIPPED (NOT RUN): no privilege to make a file link here");
            fs::remove_dir_all(&dir).unwrap();
            return;
        }
        assert_eq!(read(&link), McpServer::Unreadable);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_answer_is_tagged_for_the_window() {
        let text = |s: &McpServer| serde_json::to_string(s).unwrap();
        assert_eq!(
            text(&McpServer::Installed {
                version: ProductVersion::new("1.2.1").unwrap()
            }),
            r#"{"state":"installed","version":"1.2.1"}"#
        );
        assert_eq!(text(&McpServer::Missing), r#"{"state":"missing"}"#);
        assert_eq!(text(&McpServer::Unreadable), r#"{"state":"unreadable"}"#);
    }
}
