//! Small files the notification service keeps next to the watchdog state:
//! read with a size cap and replaced atomically (temporary file, flush, sync,
//! rename), refusing symbolic links (plan §9.1, §9.2).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn refuse_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a notification file is a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Reads `path`, at most `max` bytes. `Ok(None)` if it does not exist.
pub(crate) fn read_capped(path: &Path, max: usize) -> io::Result<Option<Vec<u8>>> {
    refuse_symlink(path)?;
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(u64::try_from(max).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a notification file is too large",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<File> {
    // Checked by `refuse_symlink` just before.
    File::open(path)
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<File> {
    // `create_new` fails on anything already there, a symlink included. The
    // file inherits the directory's ACL; restricting that directory to the
    // service account is the installer's job.
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Replaces `path` with `bytes`: a new private file next to it is written,
/// flushed and synced, then renamed over it. The only copy is never
/// truncated.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    refuse_symlink(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp: PathBuf = dir.join(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = create_private(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    sync_dir(dir);
    Ok(())
}

/// Removes `path` if it exists. Removing a symlink removes only the link.
// Used by the file-based secret stores; macOS keeps the token in the Keychain.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    // The rename has happened either way; a failed directory sync only
    // weakens durability across a power loss.
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}
