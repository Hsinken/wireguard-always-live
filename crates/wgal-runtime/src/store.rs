//! Durable watchdog state in one directory (plan §9.1).
//!
//! - `state.json` is the current state and `state.prev.json` the one before.
//!   A save writes a new file next to them, flushes and syncs it, moves the
//!   current file over the previous one and the new file into place (on
//!   Windows `std::fs::rename` replaces an existing file), then runs a
//!   [`Barrier`] that makes the renames durable before `save` returns. The
//!   only copy is never truncated.
//! - The barrier on Unix syncs the state directory. On Windows it opens the
//!   directory itself (`FILE_FLAG_BACKUP_SEMANTICS`, with write access, never
//!   following a reparse point) and flushes that handle (`FlushFileBuffers`),
//!   then opens the new `state.json` and flushes it as well. NTFS journals
//!   renames but writes its journal out lazily, so without a flush a power
//!   loss could undo a save that had already returned. If the file system
//!   refuses to flush the directory handle (access denied, or a file system
//!   that does not support it), the barrier falls back to flushing the new
//!   file alone and records this as [`Flushed::FileOnly`] in
//!   [`FileStore::durability`]. Residual risk: that a file flush on NTFS also
//!   writes out the journal holding the renames is how NTFS is understood to
//!   work, not something tested here by cutting power; on file systems
//!   without a journal (FAT, exFAT) the renames may not be durable then. A
//!   failure to flush the new file itself fails the barrier.
//! - A barrier that fails after the new file is already `state.json` does
//!   not fail the save: the save took effect, a restart loads it, and an
//!   error would make the watchdog revert in memory what is on disk. It is
//!   recorded as [`Durability::Unconfirmed`] instead, and the next save runs
//!   the barrier again before changing anything. While it still fails, that
//!   save is refused with nothing on disk changed, so the watchdog reports
//!   storage as failed and stops new automatic actions and notifications.
//!   The cost: until then, the one save whose flush failed was reported as
//!   stored without being confirmed durable; a power loss in that window can
//!   still lose it (for a reserved notification, the alert could then be
//!   sent again after the restart).
//! - If the current file is missing (a crash between the two renames), the
//!   previous one is the last completed save and is restored as usual. If the
//!   current file is truncated or corrupt, the previous one is restored as
//!   [`Startup::RestoredFromBackup`], which the watchdog treats as possibly out
//!   of date. If neither can be used, the store opens read-only: the watchdog
//!   reports storage as failed and nothing on disk is overwritten.
//! - After such a restore from the previous file, the first save replaces
//!   the damaged current file with one rename and leaves the previous file
//!   alone, so it stays the last valid save until a valid current file
//!   exists. Rotating the damaged file over it would destroy the only valid
//!   copy, and a crash between the two renames would leave nothing usable.
//!   Later saves rotate as usual.
//! - `schema_version` is read on its own before the rest of a file. A file
//!   from a newer watchdog is refused by its version, even when it no longer
//!   parses as this version: it is never taken for damage, never replaced by
//!   the previous file and never overwritten, and the store opens read-only.
//! - `state.lock` is held with an exclusive OS file lock for as long as the
//!   store is open, so only one watchdog writes the state.
//! - Symlinks are refused everywhere: the directory, the state files, the lock.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wgal_core::{PersistedState, SCHEMA_VERSION, Startup, StateStore, StoreError};

pub const STATE_FILE: &str = "state.json";
pub const PREVIOUS_FILE: &str = "state.prev.json";
pub const LOCK_FILE: &str = "state.lock";
const TEMP_PREFIX: &str = "state.json.tmp-";

/// Largest state file read or written.
pub const MAX_STATE_BYTES: usize = 1 << 20;

/// Writes the serialized state into the new file. Replaced in tests to
/// simulate a full disk or an interrupted write.
pub type Writer = Box<dyn FnMut(&mut File, &[u8]) -> io::Result<()> + Send>;

/// Renames one state file over another (`std::fs::rename`, which replaces an
/// existing file on Windows too). Replaced in tests to simulate a crash
/// between the steps of a save.
pub type Renamer = Box<dyn FnMut(&Path, &Path) -> io::Result<()> + Send>;

/// Makes a save's renames durable. Called with the state directory and the
/// path of `state.json` once the new file is in place there, and before
/// `save` returns. Replaced in tests to observe it or make it fail.
pub type Barrier = Box<dyn FnMut(&Path, &Path) -> io::Result<Flushed> + Send>;

/// What a [`Barrier`] flushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flushed {
    /// The state directory (on Windows, and the new file again).
    Directory,
    /// Windows: flushing the directory handle failed with this error, so
    /// only the new file was flushed (see the module documentation).
    FileOnly(io::ErrorKind),
}

/// How durable the last save of this store is known to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// This store has not saved anything yet.
    NothingSaved,
    /// The last save's barrier completed.
    Flushed(Flushed),
    /// The last save's new file is in place as `state.json`, so the save
    /// took effect, but its barrier failed with this error. The next save
    /// runs the barrier again before changing anything and is refused while
    /// it still fails.
    Unconfirmed(io::ErrorKind),
}

/// Which file the state was loaded from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Nothing,
    Current,
    /// The current file was unusable; the previous one was used.
    Previous,
    /// Neither file could be used. The store is read-only.
    Unusable,
}

pub struct FileStore {
    dir: PathBuf,
    /// Held for the store's lifetime; the OS releases it if the process dies.
    _lock: File,
    writer: Writer,
    renamer: Renamer,
    barrier: Barrier,
    writable: bool,
    /// `state.json` was damaged at open and has not been replaced since;
    /// `state.prev.json` is the last valid save.
    current_damaged: bool,
    durability: Durability,
    next_temp: u64,
}

impl fmt::Debug for FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("dir", &self.dir)
            .field("writable", &self.writable)
            .field("current_damaged", &self.current_damaged)
            .field("durability", &self.durability)
            .finish_non_exhaustive()
    }
}

fn is_symlink(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn refuse_symlink(path: &Path) -> Result<(), StoreError> {
    if is_symlink(path)? {
        return Err(StoreError::Refused("a state path is a symbolic link"));
    }
    Ok(())
}

fn default_writer() -> Writer {
    Box::new(|file, bytes| file.write_all(bytes))
}

fn default_barrier() -> Barrier {
    Box::new(flush_after_rename)
}

/// Reads and checks one state file. `Ok(None)` if it does not exist.
fn read_state(path: &Path) -> Result<Option<PersistedState>, StoreError> {
    refuse_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_STATE_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(StoreError::Corrupt("state file is too large"));
    }
    // The version decides how the rest is read, so it is read on its own
    // first: a newer watchdog's file need not parse as this version (the
    // stored types refuse unknown fields) and must not be taken for damage.
    let version = serde_json::from_slice::<VersionOnly>(&bytes)
        .map_err(|_| StoreError::Corrupt("state file is invalid"))?
        .schema_version;
    if version > u64::from(SCHEMA_VERSION) {
        // Never read as damaged, so never replaced by saves based on an older
        // copy (see `FileStore::open`).
        return Err(StoreError::Refused("state was written by a newer watchdog"));
    }
    if version != u64::from(SCHEMA_VERSION) {
        // Version 1 is the first, so there is nothing to migrate from yet: an
        // older version is unknown and refused rather than guessed at. A
        // migration from a known older version goes here.
        return Err(StoreError::Corrupt("unknown state schema version"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| StoreError::Corrupt("state file is invalid"))
}

/// The one field every version of the state file has. Unknown fields are
/// ignored here on purpose.
#[derive(Deserialize)]
struct VersionOnly {
    schema_version: u64,
}

impl FileStore {
    /// Opens the state directory, takes the writer lock and loads the state.
    /// Fails only if the directory or the lock cannot be used; unusable state
    /// files give [`Startup::StorageFailed`] and a read-only store.
    pub fn open(dir: &Path) -> Result<(Self, Startup, Source), StoreError> {
        let meta = fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(StoreError::Refused(
                "state directory is not a plain directory",
            ));
        }
        let lock_path = dir.join(LOCK_FILE);
        refuse_symlink(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::Refused("another watchdog holds the state lock"));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        remove_leftover_temps(dir);

        let (startup, source) = match read_state(&dir.join(STATE_FILE)) {
            Ok(Some(state)) => (Startup::Restored(state), Source::Current),
            // Only a missing or damaged current file falls back. One that is
            // unreadable, a symlink, or from a newer watchdog must not be
            // replaced by saves based on the older copy.
            Err(StoreError::Io(_) | StoreError::Refused(_)) => {
                (Startup::StorageFailed, Source::Unusable)
            }
            current => {
                let current_missing = matches!(current, Ok(None));
                match read_state(&dir.join(PREVIOUS_FILE)) {
                    // A save moves the current file aside before putting the
                    // new one in place. With the current file missing, the
                    // previous one is the last save that completed, and the
                    // watchdog never acted on the unfinished one.
                    Ok(Some(state)) if current_missing => {
                        (Startup::Restored(state), Source::Previous)
                    }
                    // A damaged current file was a completed save; whatever it
                    // recorded after the previous one is lost.
                    Ok(Some(state)) => (Startup::RestoredFromBackup(state), Source::Previous),
                    Ok(None) if current_missing => (Startup::Empty, Source::Nothing),
                    _ => (Startup::StorageFailed, Source::Unusable),
                }
            }
        };
        let store = Self {
            dir: dir.to_path_buf(),
            _lock: lock,
            writer: default_writer(),
            renamer: Box::new(|from, to| fs::rename(from, to)),
            barrier: default_barrier(),
            writable: source != Source::Unusable,
            current_damaged: matches!(startup, Startup::RestoredFromBackup(_)),
            durability: Durability::NothingSaved,
            next_temp: 0,
        };
        Ok((store, startup, source))
    }

    /// Replaces how the new file is written, to simulate disk faults in tests.
    pub fn with_writer(mut self, writer: Writer) -> Self {
        self.writer = writer;
        self
    }

    /// Replaces how state files are renamed, to simulate a crash between the
    /// steps of a save in tests.
    pub fn with_renamer(mut self, renamer: Renamer) -> Self {
        self.renamer = renamer;
        self
    }

    /// Replaces the barrier run after a save's last rename, to observe it or
    /// simulate a failing flush in tests.
    pub fn with_barrier(mut self, barrier: Barrier) -> Self {
        self.barrier = barrier;
        self
    }

    /// How durable the last save is known to be.
    pub fn durability(&self) -> Durability {
        self.durability
    }

    fn write_new_file(&mut self, bytes: &[u8]) -> Result<PathBuf, StoreError> {
        self.next_temp = self.next_temp.wrapping_add(1);
        let temp = self.dir.join(format!(
            "{TEMP_PREFIX}{}-{}",
            std::process::id(),
            self.next_temp
        ));
        // `create_new` fails on anything already there, a symlink included.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let written = (self.writer)(&mut file, bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all());
        drop(file);
        match written {
            Ok(()) => Ok(temp),
            Err(e) => {
                let _ = fs::remove_file(&temp);
                Err(e.into())
            }
        }
    }
}

fn remove_leftover_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            // Removing a symlink removes the link, never its target.
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The default [`Barrier`] on Unix. The new file was synced before the
/// renames; syncing the directory makes them durable.
#[cfg(unix)]
fn flush_after_rename(dir: &Path, _current: &Path) -> io::Result<Flushed> {
    sync_dir(dir).map(|()| Flushed::Directory)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// The default [`Barrier`] on Windows: flush the directory handle, then the
/// new file (see the module documentation).
#[cfg(windows)]
fn flush_after_rename(dir: &Path, current: &Path) -> io::Result<Flushed> {
    windows_flush(sync_dir(dir), || sync_file(current))
}

/// Combines the two Windows flushes. Flushing the new file is required; a
/// refused directory flush falls back to it and is reported, never hidden.
#[cfg(windows)]
fn windows_flush(
    dir: io::Result<()>,
    file: impl FnOnce() -> io::Result<()>,
) -> io::Result<Flushed> {
    file()?;
    Ok(match dir {
        Ok(()) => Flushed::Directory,
        Err(e) => Flushed::FileOnly(e.kind()),
    })
}

/// Opens a directory itself, not a file in it, so it can be flushed.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Opens a symlink or junction itself instead of following it.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// `FlushFileBuffers` on the directory handle. It needs write access to the
/// directory, which the watchdog has on its own state directory.
#[cfg(windows)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)?
        .sync_all()
}

/// `FlushFileBuffers` on the file, opened again without truncating it.
#[cfg(windows)]
fn sync_file(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?
        .sync_all()
}

#[cfg(not(any(unix, windows)))]
fn flush_after_rename(dir: &Path, _current: &Path) -> io::Result<Flushed> {
    sync_dir(dir).map(|()| Flushed::Directory)
}

#[cfg(not(any(unix, windows)))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

impl StateStore for FileStore {
    fn save(&mut self, state: &PersistedState) -> Result<(), StoreError> {
        if !self.writable {
            return Err(StoreError::Refused(
                "stored state could not be loaded; it is not overwritten",
            ));
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|_| StoreError::Corrupt("state could not be serialized"))?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(StoreError::Refused("state is too large"));
        }
        let current = self.dir.join(STATE_FILE);
        let previous = self.dir.join(PREVIOUS_FILE);
        refuse_symlink(&current)?;
        refuse_symlink(&previous)?;
        if let Durability::Unconfirmed(_) = self.durability {
            // The last save is on disk but was never confirmed durable. Do
            // not build on it unless it can be confirmed now; refusing here
            // changes nothing on disk.
            let flushed = (self.barrier)(&self.dir, &current).map_err(|e| {
                self.durability = Durability::Unconfirmed(e.kind());
                StoreError::from(e)
            })?;
            self.durability = Durability::Flushed(flushed);
        }

        let temp = self.write_new_file(&bytes)?;
        if self.current_damaged {
            // The previous file is the last valid save. Rotating the damaged
            // file over it would leave nothing usable if the process stopped
            // before the second rename. One rename replaces the damaged file
            // instead: before it, both files are as they were at open (and
            // restore conservatively again); after it, the new file is valid.
            if let Err(e) = (self.renamer)(&temp, &current) {
                let _ = fs::remove_file(&temp);
                return Err(e.into());
            }
            self.current_damaged = false;
        } else {
            let had_current = current.exists();
            if had_current && let Err(e) = (self.renamer)(&current, &previous) {
                let _ = fs::remove_file(&temp);
                return Err(e.into());
            }
            if let Err(e) = (self.renamer)(&temp, &current) {
                if had_current {
                    let _ = (self.renamer)(&previous, &current);
                }
                let _ = fs::remove_file(&temp);
                return Err(e.into());
            }
        }
        // The new state is `state.json` now: the save took effect, and a
        // restart finds it whatever the barrier says. Reporting a failure
        // here would make the watchdog undo in memory what is on disk, so
        // a failed barrier is recorded instead and checked again first
        // thing in the next save.
        self.durability = match (self.barrier)(&self.dir, &current) {
            Ok(flushed) => Durability::Flushed(flushed),
            Err(e) => Durability::Unconfirmed(e.kind()),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("wgal-store-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        path
    }

    /// Integrated review of `121b42a`, finding 2: the directory sync really
    /// opens the directory. A no-op cannot tell a missing one apart.
    #[test]
    fn i2_sync_dir_opens_the_directory() {
        let dir = temp_dir("sync-dir");
        sync_dir(&dir).unwrap();
        let missing = dir.join("missing");
        assert_eq!(
            sync_dir(&missing).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotFound)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Finding 2 on the machine running the tests: the default barrier
    /// flushes the directory itself, not only the file. On Windows this is
    /// `FlushFileBuffers` on a directory handle of the test's file system.
    #[test]
    fn i2_default_barrier_flushes_the_directory() {
        let dir = temp_dir("barrier");
        let current = dir.join(STATE_FILE);
        fs::write(&current, b"{}").unwrap();
        assert_eq!(
            flush_after_rename(&dir, &current).map_err(|e| e.kind()),
            Ok(Flushed::Directory)
        );
        assert_eq!(fs::read(&current).unwrap(), b"{}", "the file is unchanged");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Finding 2, Windows fallback: a refused directory flush still requires
    /// the file flush and is reported as such; a failed file flush fails.
    #[cfg(windows)]
    #[test]
    fn i2_windows_falls_back_to_the_file_flush_and_says_so() {
        let refused = || Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(
            windows_flush(refused(), || Ok(())).map_err(|e| e.kind()),
            Ok(Flushed::FileOnly(io::ErrorKind::PermissionDenied))
        );
        assert_eq!(
            windows_flush(Ok(()), || Ok(())).map_err(|e| e.kind()),
            Ok(Flushed::Directory)
        );
        assert_eq!(
            windows_flush(Ok(()), refused).map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(
            windows_flush(refused(), refused).map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
    }

    /// Finding 2, Windows: the new file is flushed through a fresh handle
    /// opened without truncating it, and a missing file is an error.
    #[cfg(windows)]
    #[test]
    fn i2_windows_file_flush_keeps_the_file() {
        let dir = temp_dir("file-flush");
        let current = dir.join(STATE_FILE);
        fs::write(&current, b"{\"kept\":true}").unwrap();
        sync_file(&current).unwrap();
        assert_eq!(fs::read(&current).unwrap(), b"{\"kept\":true}");
        assert_eq!(
            sync_file(&dir.join("missing")).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotFound)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
