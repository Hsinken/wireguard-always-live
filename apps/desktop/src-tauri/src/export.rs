//! Saving a log export (the log tab's export button) or a settings file
//! (owner decision of 2026-11-04). The webview hands over only the text, or
//! the watchdog's settings document and the device name, and the UTC offset
//! for the file name's local time; this side picks the folder (the user's
//! Downloads, else Documents, from Tauri's path resolver) and the name,
//! `wgal-log-<yyyyMMdd-HHmmss>.txt` or
//! `wgal-settings-<device>-<yyyyMMdd-HHmmss>.json`,
//! and creates the file as new: an existing file or link of that name is
//! never overwritten or followed, the next free `-2`, `-3`… is taken
//! instead (AGENTS.md rules 8 and 9). The folder itself must be a
//! directory, not a symbolic link or a junction; other reparse points, such
//! as a OneDrive folder kept in the cloud, do not lead elsewhere and are
//! accepted. The folder is opened as itself, never through a link of its
//! name, and that one handle says both that it is a directory and where it
//! is. Each new file is checked to be directly in the folder before
//! anything is written, which also catches any reparse point that does lead
//! elsewhere (see [`ExportFolder`]). The check asks the handle the file was
//! created with where the file is (`wgal_platform::<os>::final_path_of_file`:
//! `GetFinalPathNameByHandleW` on Windows, `/proc/self/fd` on Ubuntu,
//! `F_GETPATH` on macOS), and the text is written through that same handle,
//! so a folder swapped before or after the file was created, or swapped and
//! put back, cannot make the check look at another file. A file that fails
//! the check gets nothing and is never removed by its path, which may name
//! another file by then: it stays where it was created, empty. Nor is a file
//! that could not be written whole removed: it is emptied through its handle
//! and left. On an OS without such a function the export is refused. What it
//! answers is a folder, a name or a reason, never a sentence: the window
//! words them from its language file.
//!
//! What remains is that a process of the same user decides where the user's
//! own Downloads folder is: links above the folder are the user's own
//! layout and are followed, so such a process can put one there before the
//! export starts. Only the user and administrators can change the user's
//! profile folders, and what is written is the export: masked text that
//! never overwrites anything.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// The largest export written.
pub(crate) const MAX_BYTES: usize = 8 * 1024 * 1024;
/// UTC offsets in use span −12 h to +14 h; anything past ±14 h is refused.
const MAX_OFFSET_MINUTES: i32 = 14 * 60;
/// How many names are tried before giving up: `-2` to `-99`.
const MAX_TRIES: u32 = 99;
/// Windows' `FILE_FLAG_OPEN_REPARSE_POINT` (winbase.h): a reparse point at
/// the path is opened itself, never what it points to.
#[cfg(windows)]
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// Windows' `FILE_FLAG_BACKUP_SEMANTICS` (winbase.h): lets a directory be
/// opened as a handle.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// Windows' `FILE_READ_ATTRIBUTES` (winnt.h): all the folder's handle is
/// opened for.
#[cfg(windows)]
const FILE_READ_ATTRIBUTES: u32 = 0x0080;

/// Which folder an export went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Folder {
    Downloads,
    Documents,
}

/// Where an export was saved: the folder and the file's name in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Saved {
    pub(crate) folder: Folder,
    pub(crate) name: String,
}

/// Why an export was not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    Empty,
    TooLarge,
    BadOffset,
    NoFolder,
    Interrupted,
    CannotCreate,
    CannotWrite,
    TooMany,
    /// The folder is a symbolic link or a junction, or not a directory.
    LinkedFolder,
    /// The new file was not directly in the folder, or where it is could not
    /// be told: nothing was written, and the file was left where it is,
    /// empty (the detail says `NotRemoved`).
    Redirected,
}

/// An export that was not saved: the reason, and the OS's error kind
/// (`NotFound`, `PermissionDenied`…) when one says more, for the details.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct NotSaved {
    pub(crate) reason: Reason,
    pub(crate) detail: Option<String>,
}

impl From<Reason> for NotSaved {
    fn from(reason: Reason) -> Self {
        Self {
            reason,
            detail: None,
        }
    }
}

/// Why an export is refused before anything is written.
pub(crate) fn check(text: &str, utc_offset_minutes: i32) -> Result<(), NotSaved> {
    if text.is_empty() {
        Err(Reason::Empty.into())
    } else if text.len() > MAX_BYTES {
        Err(Reason::TooLarge.into())
    } else if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&utc_offset_minutes) {
        Err(Reason::BadOffset.into())
    } else {
        Ok(())
    }
}

/// "20261103-213005": `now` at `utc_offset_minutes` from UTC.
pub(crate) fn stamp(now: SystemTime, utc_offset_minutes: i32) -> String {
    let unix = now
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let local = unix.saturating_add(i64::from(utc_offset_minutes) * 60);
    let (days, secs) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The calendar date `days` after 1970-01-01 (proleptic Gregorian; Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The OS's error kind as a detail.
fn failed(reason: Reason, e: &io::Error) -> NotSaved {
    NotSaved {
        reason,
        detail: Some(format!("{:?}", e.kind())),
    }
}

/// A directory that is not a link: `is_symlink` is true for symbolic links
/// and, on Windows, for every name-surrogate reparse point, junctions
/// included; other reparse points (cloud placeholders, say) pass.
fn plain_dir(meta: &fs::Metadata) -> bool {
    meta.is_dir() && !meta.file_type().is_symlink()
}

/// Creates `path` as a new file: fails on any existing entry of that name,
/// a link included (on Windows a link at the path is opened itself, so a
/// dangling one is not followed either).
fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Opens the folder `dir` itself as a handle, never what a link of that
/// name points to: on Windows a reparse point there is opened as itself;
/// elsewhere a symbolic link there does not open (`O_NOFOLLOW`), nor
/// anything that is not a directory (`O_DIRECTORY`).
fn open_folder(dir: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    #[cfg(not(any(windows, unix)))]
    options.read(true);
    options.open(dir)
}

/// The folder an export goes into, checked before anything is written: a
/// real directory, and where it really is, which every new file in it is
/// checked against. Both come from one handle on the folder itself, so a
/// swap of the folder for a link while it is checked cannot make the check
/// look at one thing and keep where another is.
#[derive(Debug)]
pub(crate) struct ExportFolder {
    path: PathBuf,
    real: PathBuf,
}

impl ExportFolder {
    /// `dir`, when it is a directory; a symbolic link or a junction is
    /// refused.
    pub(crate) fn check(dir: &Path) -> Result<Self, NotSaved> {
        Self::check_with(dir, |_| ())
    }

    /// [`Self::check`], with `between` run once the folder is open and
    /// found to be a directory, before its handle is asked where it is: in
    /// the tests, a swap at that moment.
    fn check_with(dir: &Path, between: impl FnOnce(&Path)) -> Result<Self, NotSaved> {
        let folder = match open_folder(dir) {
            Ok(folder) => folder,
            // Why it did not open is looked up by name only to word the
            // answer: a link or a non-directory there is refused as such.
            Err(e) => {
                return Err(match fs::symlink_metadata(dir) {
                    Ok(meta) if !plain_dir(&meta) => Reason::LinkedFolder.into(),
                    _ => failed(Reason::CannotCreate, &e),
                });
            }
        };
        let meta = folder
            .metadata()
            .map_err(|e| failed(Reason::CannotCreate, &e))?;
        if !plain_dir(&meta) {
            return Err(Reason::LinkedFolder.into());
        }
        between(dir);
        let real = final_path_of_file(&folder).map_err(|e| failed(Reason::CannotCreate, &e))?;
        Ok(Self {
            path: dir.to_path_buf(),
            real,
        })
    }

    /// Writes `text` into a new file here and returns its name:
    /// `wgal-log-<stamp>.txt`, or the first free `wgal-log-<stamp>-<n>.txt`.
    /// A file that is not in this folder once created gets nothing; one that
    /// could not be written whole is emptied. Neither is removed (see the
    /// module's documentation).
    pub(crate) fn write_new(&self, stamp: &str, text: &[u8]) -> Result<String, NotSaved> {
        self.write_new_with(&FileName::log(stamp), text, landed_at)
    }

    /// [`Self::write_new`], told where a new file really is by `landed_at`
    /// (the file as created, and the path it was created at): [`landed_at`]
    /// itself, or in the tests a stand-in for a swap made after the file was
    /// created.
    fn write_new_with(
        &self,
        names: &FileName,
        text: &[u8],
        landed_at: impl Fn(&File, &Path) -> io::Result<PathBuf>,
    ) -> Result<String, NotSaved> {
        for n in 1..=MAX_TRIES {
            let name = names.nth(n);
            let path = self.path.join(&name);
            match create_new(&path) {
                Ok(mut file) => {
                    // Where it really is, before a byte is written; then
                    // written through the same handle. Refused, the handle
                    // is closed with nothing written.
                    self.holds(landed_at(&file, &path), &name)?;
                    return match file.write_all(text).and_then(|()| file.sync_all()) {
                        Ok(()) => Ok(name),
                        Err(e) => Err(unwritten(&file, &e)),
                    };
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(failed(Reason::CannotCreate, &e)),
            }
        }
        Err(Reason::TooMany.into())
    }

    /// Whether a file named `name`, which `landed` says is where it really
    /// is just after it was created, is directly in this folder; refused as
    /// redirected when it is not, or when that could not be told. The file
    /// stays where it is either way: removing it by its path could remove
    /// another file.
    fn holds(&self, landed: io::Result<PathBuf>, name: &str) -> Result<(), NotSaved> {
        let detail = match landed {
            Ok(real)
                if real.parent() == Some(self.real.as_path())
                    && real.file_name() == Some(OsStr::new(name)) =>
            {
                return Ok(());
            }
            Ok(_) => "NotRemoved".to_owned(),
            Err(e) => format!("NotRemoved, {:?}", e.kind()),
        };
        Err(NotSaved {
            reason: Reason::Redirected,
            detail: Some(detail),
        })
    }
}

/// Where the new file `file` really is, from its handle, never from the path
/// it was created at: nothing done to the folders after the file was created
/// can make the answer another file's. Each OS's own
/// `wgal_platform::<os>::final_path_of_file`; on any other OS it cannot be
/// told, and the export is refused.
fn landed_at(file: &File, _path: &Path) -> io::Result<PathBuf> {
    final_path_of_file(file)
}

#[cfg(target_os = "linux")]
use wgal_platform::linux::final_path_of_file;
#[cfg(target_os = "macos")]
use wgal_platform::macos::final_path_of_file;
#[cfg(windows)]
use wgal_platform::windows::final_path_of_file;

/// No OS function says where a file is: never told, always refused.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn final_path_of_file(_file: &File) -> io::Result<PathBuf> {
    Err(ErrorKind::Unsupported.into())
}

/// The refusal for a new file that `error` kept from being written whole:
/// the file is emptied through its handle, so no part of the export is left
/// in it, and left where it is, since its path may name another file by now.
/// The detail is the OS's error kind, `NotRemoved`, and `NotEmptied` when
/// even emptying it failed.
fn unwritten(file: &File, error: &io::Error) -> NotSaved {
    let emptied = file.set_len(0).is_ok();
    NotSaved {
        reason: Reason::CannotWrite,
        detail: Some(format!(
            "{:?}, NotRemoved{}",
            error.kind(),
            if emptied { "" } else { ", NotEmptied" }
        )),
    }
}

/// Writes `text` into a new file in `dir` and returns its name (see
/// [`ExportFolder::write_new`]), when `dir` is a real directory.
pub(crate) fn write_new(dir: &Path, stamp: &str, text: &[u8]) -> Result<String, NotSaved> {
    ExportFolder::check(dir)?.write_new(stamp, text)
}

/// The names a new file may get: `<stem>.<ext>`, then `<stem>-2.<ext>` and
/// so on.
pub(crate) struct FileName {
    stem: String,
    ext: &'static str,
}

impl FileName {
    fn log(stamp: &str) -> Self {
        Self {
            stem: format!("wgal-log-{stamp}"),
            ext: "txt",
        }
    }

    /// `wgal-settings-<device>-<stamp>.json`: the device name only in the
    /// name, never in the file (owner decision of 2026-11-04), as
    /// [`file_name_part`] makes it; without one, `wgal-settings-<stamp>`.
    pub(crate) fn settings(device: &str, stamp: &str) -> Self {
        let part = file_name_part(device);
        Self {
            stem: if part.is_empty() {
                format!("wgal-settings-{stamp}")
            } else {
                format!("wgal-settings-{part}-{stamp}")
            },
            ext: "json",
        }
    }

    fn nth(&self, n: u32) -> String {
        if n == 1 {
            format!("{}.{}", self.stem, self.ext)
        } else {
            format!("{}-{n}.{}", self.stem, self.ext)
        }
    }
}

/// Most characters of a device name kept in a file name.
const MAX_NAME_PART_CHARS: usize = 40;

/// A device name as part of a file name: letters and digits of any script,
/// `-` and `_` kept, whitespace as `-`, anything else (`/ \ : * ? " < > |`,
/// dots, control characters) left out; at most [`MAX_NAME_PART_CHARS`], and
/// no `-` at either end. Nothing in it can name another folder or file.
pub(crate) fn file_name_part(device: &str) -> String {
    let mut part = String::new();
    for c in device.chars() {
        let c = if c.is_whitespace() { '-' } else { c };
        if !(c.is_alphanumeric() || c == '-' || c == '_') || (c == '-' && part.ends_with('-')) {
            continue;
        }
        if part.chars().count() == MAX_NAME_PART_CHARS {
            break;
        }
        part.push(c);
    }
    part.trim_matches('-').to_owned()
}

/// Saves a settings file in `dir` under [`FileName::settings`].
pub(crate) fn write_settings(
    dir: &Path,
    device: &str,
    stamp: &str,
    text: &[u8],
) -> Result<String, NotSaved> {
    ExportFolder::check(dir)?.write_new_with(&FileName::settings(device, stamp), text, landed_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A fresh directory of this test's own under the system temp folder.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wgal-export-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_name_is_new_and_never_overwrites() {
        let dir = temp_dir("names");
        assert_eq!(
            write_new(&dir, "20261103-213005", b"one").unwrap(),
            "wgal-log-20261103-213005.txt"
        );
        assert_eq!(
            write_new(&dir, "20261103-213005", b"two").unwrap(),
            "wgal-log-20261103-213005-2.txt"
        );
        // Something else already has the next name: it is left alone.
        fs::write(dir.join("wgal-log-20261103-213005-3.txt"), b"theirs").unwrap();
        assert_eq!(
            write_new(&dir, "20261103-213005", "三".as_bytes()).unwrap(),
            "wgal-log-20261103-213005-4.txt"
        );
        assert_eq!(
            fs::read(dir.join("wgal-log-20261103-213005.txt")).unwrap(),
            b"one"
        );
        assert_eq!(
            fs::read(dir.join("wgal-log-20261103-213005-3.txt")).unwrap(),
            b"theirs"
        );
        assert_eq!(
            fs::read_to_string(dir.join("wgal-log-20261103-213005-4.txt")).unwrap(),
            "三"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn it_gives_up_after_the_last_name_and_on_a_missing_folder() {
        let dir = temp_dir("full");
        for n in 1..=MAX_TRIES {
            write_new(&dir, "20261103-000000", b"x").unwrap();
            assert!(
                dir.join(if n == 1 {
                    "wgal-log-20261103-000000.txt".to_owned()
                } else {
                    format!("wgal-log-20261103-000000-{n}.txt")
                })
                .exists()
            );
        }
        assert_eq!(
            write_new(&dir, "20261103-000000", b"x"),
            Err(Reason::TooMany.into())
        );
        let missing = write_new(&dir.join("missing"), "20261103-000000", b"x").unwrap_err();
        assert_eq!(missing.reason, Reason::CannotCreate);
        assert_eq!(missing.detail.as_deref(), Some("NotFound"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A directory link at `link` to `target`: a symbolic link, or on
    /// Windows a junction (the `junction` crate, which any user may make,
    /// so no symlink privilege is needed and no shell is run). A test that
    /// needs one fails when it cannot be made, rather than passing without
    /// having looked.
    fn dir_link(target: &Path, link: &Path) {
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link)
            .or_else(|_| junction::create(target, link));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        if let Err(e) = made {
            panic!(
                "BLOCKED: cannot make a directory link here ({:?})",
                e.kind()
            );
        }
    }

    /// Codex P2-8: Downloads replaced by a link to somewhere else.
    #[test]
    fn a_folder_that_is_a_link_is_refused_and_nothing_lands_behind_it() {
        let dir = temp_dir("linked");
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let downloads = dir.join("Downloads");
        dir_link(&elsewhere, &downloads);
        let refused = write_new(&downloads, "20261103-213005", b"x");
        assert_eq!(refused, Err(Reason::LinkedFolder.into()));
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        // Not a directory at all.
        let file = dir.join("file");
        fs::write(&file, b"theirs").unwrap();
        assert_eq!(
            write_new(&file, "20261103-213005", b"x"),
            Err(Reason::LinkedFolder.into())
        );
        assert_eq!(fs::read(&file).unwrap(), b"theirs");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Codex third re-review P2-N4: the folder was found to be a directory,
    /// and swapped for a link before it was asked where it is. Asking the
    /// path then would have kept the link's target as the folder and passed
    /// the export written there; the handle says where the folder that was
    /// checked is, so the file behind the link is refused, left empty, and
    /// the folder that was checked gets nothing either.
    #[test]
    fn a_folder_swapped_for_a_link_while_it_is_checked_gets_nothing() {
        let dir = temp_dir("while");
        let downloads = dir.join("Downloads");
        let elsewhere = dir.join("elsewhere");
        let aside = dir.join("aside");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let folder = ExportFolder::check_with(&downloads, |path| {
            fs::rename(path, &aside).unwrap();
            dir_link(&elsewhere, path);
        });
        // What asking the path would have said.
        assert_eq!(
            fs::canonicalize(&downloads).unwrap(),
            fs::canonicalize(&elsewhere).unwrap()
        );
        let refused = folder.unwrap().write_new("20261103-213005", b"x");
        assert_eq!(
            refused,
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some("NotRemoved".to_owned()),
            })
        );
        assert_eq!(
            fs::read(elsewhere.join("wgal-log-20261103-213005.txt")).unwrap(),
            b""
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&aside).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The folder passed its check, then was swapped for a link before the
    /// file was created: the file is found outside and refused; nothing is
    /// written into it, and it is left there, empty.
    #[test]
    fn a_folder_swapped_for_a_link_after_its_check_gets_nothing() {
        let dir = temp_dir("swapped");
        let downloads = dir.join("Downloads");
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let folder = ExportFolder::check(&downloads).unwrap();
        fs::rename(&downloads, dir.join("moved")).unwrap();
        dir_link(&elsewhere, &downloads);
        let refused = folder.write_new("20261103-213005", b"x");
        assert_eq!(
            refused,
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some("NotRemoved".to_owned()),
            })
        );
        assert_eq!(
            fs::read(elsewhere.join("wgal-log-20261103-213005.txt")).unwrap(),
            b""
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        assert_eq!(fs::read_dir(dir.join("moved")).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Codex re-review P2-N1: the file was created in the folder, and then
    /// its path was made to name another, empty file elsewhere (a folder
    /// above swapped for a link, say). Simulated by telling the check that
    /// file's place, which is what resolving the path again would have
    /// given. Nothing is written, the other file is left as it was, and the
    /// new one stays, empty, where it was created.
    #[test]
    fn a_swap_after_creation_is_refused_and_nothing_is_removed() {
        let dir = temp_dir("after");
        let downloads = dir.join("Downloads");
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let name = "wgal-log-20261103-213005.txt";
        let theirs = elsewhere.join(name);
        fs::write(&theirs, b"").unwrap();
        let folder = ExportFolder::check(&downloads).unwrap();
        let refused = folder.write_new_with(&FileName::log("20261103-213005"), b"x", |_, _| {
            fs::canonicalize(&theirs)
        });
        assert_eq!(
            refused,
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some("NotRemoved".to_owned()),
            })
        );
        assert_eq!(fs::read(&theirs).unwrap(), b"");
        assert_eq!(fs::read(downloads.join(name)).unwrap(), b"");
        // Where the file is cannot be told at all: refused the same way.
        let lost = folder.write_new_with(&FileName::log("20261103-213005"), b"x", |_, _| {
            Err(io::Error::from(ErrorKind::PermissionDenied))
        });
        assert_eq!(
            lost,
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some("NotRemoved, PermissionDenied".to_owned()),
            })
        );
        assert_eq!(
            fs::read(downloads.join("wgal-log-20261103-213005-2.txt")).unwrap(),
            b""
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Codex second re-review P2-8: the folder was swapped for a link after
    /// its check, the file was created behind the link, and then the folder
    /// was put back with a file of the same name in it before the check ran
    /// (the stand-in for `landed_at` does the putting back, then asks the real
    /// one). Resolving the path again would find that other file in the
    /// folder and pass; the handle says where the export really went, and it
    /// is refused. Nothing is written anywhere and the other file is left as
    /// it was.
    #[test]
    fn a_folder_swapped_and_put_back_around_the_creation_is_refused() {
        let dir = temp_dir("put-back");
        let downloads = dir.join("Downloads");
        let elsewhere = dir.join("elsewhere");
        let aside = dir.join("aside");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let name = "wgal-log-20261103-213005.txt";
        let folder = ExportFolder::check(&downloads).unwrap();
        fs::rename(&downloads, &aside).unwrap();
        dir_link(&elsewhere, &downloads);
        let refused =
            folder.write_new_with(&FileName::log("20261103-213005"), b"x", |file, path| {
                #[cfg(windows)]
                fs::remove_dir(&downloads).unwrap();
                #[cfg(unix)]
                fs::remove_file(&downloads).unwrap();
                fs::rename(&aside, &downloads).unwrap();
                fs::write(downloads.join(name), b"").unwrap();
                // What resolving the path again would have said.
                assert_eq!(
                    fs::canonicalize(path).unwrap(),
                    fs::canonicalize(&downloads).unwrap().join(name)
                );
                landed_at(file, path)
            });
        assert_eq!(
            refused,
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some("NotRemoved".to_owned()),
            })
        );
        assert_eq!(fs::read(downloads.join(name)).unwrap(), b"");
        assert_eq!(fs::read(elsewhere.join(name)).unwrap(), b"");
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The check reads where the new file is from the handle it was created
    /// with, never from its path again: once the file has moved and another
    /// one taken its path, the check still finds the file it holds.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_check_follows_the_handle_not_the_path() {
        let dir = temp_dir("handle");
        let path = dir.join("new.txt");
        let file = create_new(&path).unwrap();
        let moved = dir.join("moved");
        fs::create_dir(&moved).unwrap();
        fs::rename(&path, moved.join("new.txt")).unwrap();
        fs::write(&path, b"another").unwrap();
        assert_eq!(
            landed_at(&file, &path).unwrap(),
            fs::canonicalize(&moved).unwrap().join("new.txt")
        );
        drop(file);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A link already at the file's name, even one that points nowhere yet,
    /// is never followed: the next name is taken.
    #[test]
    fn a_link_at_the_files_name_is_never_followed() {
        let dir = temp_dir("named-link");
        let downloads = dir.join("Downloads");
        let outside = dir.join("outside");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&outside).unwrap();
        let planted = outside.join("planted.txt");
        let link = downloads.join("wgal-log-20261103-213005.txt");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&planted, &link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&planted, &link);
        match made {
            Ok(()) => {
                assert_eq!(
                    write_new(&downloads, "20261103-213005", b"x").unwrap(),
                    "wgal-log-20261103-213005-2.txt"
                );
                assert!(!planted.exists());
                assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            }
            // Windows makes file links only with the privilege for it
            // (`ERROR_PRIVILEGE_NOT_HELD`, 1314); any other failure to make
            // one fails the test.
            Err(e) => {
                assert!(
                    cfg!(windows) && e.raw_os_error() == Some(1314),
                    "cannot make a file link: {e}"
                );
                eprintln!("SKIPPED (NOT RUN): no privilege to make a file link here");
            }
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A link above the folder that is already there is the user's own
    /// layout (a profile moved with a junction, say): it is followed.
    #[test]
    fn a_link_above_the_folder_is_followed() {
        let dir = temp_dir("above");
        let real = dir.join("real");
        fs::create_dir_all(real.join("Downloads")).unwrap();
        let profile = dir.join("profile");
        dir_link(&real, &profile);
        assert_eq!(
            write_new(&profile.join("Downloads"), "20261103-213005", b"x").unwrap(),
            "wgal-log-20261103-213005.txt"
        );
        assert_eq!(
            fs::read(real.join("Downloads").join("wgal-log-20261103-213005.txt")).unwrap(),
            b"x"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_file_must_resolve_into_the_folder() {
        let dir = temp_dir("holds");
        let downloads = dir.join("Downloads");
        fs::create_dir(&downloads).unwrap();
        let folder = ExportFolder::check(&downloads).unwrap();
        let real = fs::canonicalize(&downloads).unwrap();
        let name = "wgal-log-20261103-213005.txt";
        assert!(folder.holds(Ok(real.join(name)), name).is_ok());
        // Elsewhere, under another name, one level deeper, or not found.
        let redirected = |detail: &str| {
            Err(NotSaved {
                reason: Reason::Redirected,
                detail: Some(detail.to_owned()),
            })
        };
        let elsewhere = fs::canonicalize(&dir).unwrap().join(name);
        assert_eq!(folder.holds(Ok(elsewhere), name), redirected("NotRemoved"));
        assert_eq!(
            folder.holds(Ok(real.join("other.txt")), name),
            redirected("NotRemoved")
        );
        assert_eq!(
            folder.holds(Ok(real.join("sub").join(name)), name),
            redirected("NotRemoved")
        );
        assert_eq!(
            folder.holds(Err(ErrorKind::NotFound.into()), name),
            redirected("NotRemoved, NotFound")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A file that could not be written whole is emptied through its handle
    /// and left where it is, never removed by its path.
    #[test]
    fn a_file_not_written_whole_is_emptied_and_left() {
        let dir = temp_dir("unwritten");
        let path = dir.join("wgal-log-20261103-213005.txt");
        let mut file = create_new(&path).unwrap();
        file.write_all(b"part of an export").unwrap();
        let refused = unwritten(&file, &io::Error::from(ErrorKind::StorageFull));
        assert_eq!(
            refused,
            NotSaved {
                reason: Reason::CannotWrite,
                detail: Some("StorageFull, NotRemoved".to_owned()),
            }
        );
        drop(file);
        assert_eq!(fs::read(&path).unwrap(), b"");
        // Without write access it cannot even be emptied, and says so.
        fs::write(&path, b"part of an export").unwrap();
        let read_only = File::open(&path).unwrap();
        assert_eq!(
            unwritten(&read_only, &io::Error::from(ErrorKind::StorageFull)).detail,
            Some("StorageFull, NotRemoved, NotEmptied".to_owned())
        );
        drop(read_only);
        assert_eq!(fs::read(&path).unwrap(), b"part of an export");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_text_must_fit_and_the_offset_must_exist() {
        assert!(check("x", 480).is_ok());
        assert!(check(&"x".repeat(MAX_BYTES), 0).is_ok());
        assert_eq!(
            check(&"x".repeat(MAX_BYTES + 1), 0),
            Err(Reason::TooLarge.into())
        );
        assert_eq!(check("", 0), Err(Reason::Empty.into()));
        assert!(check("x", 14 * 60).is_ok());
        assert!(check("x", -14 * 60).is_ok());
        assert_eq!(check("x", 14 * 60 + 1), Err(Reason::BadOffset.into()));
        assert_eq!(check("x", i32::MIN), Err(Reason::BadOffset.into()));
    }

    /// The window gets codes it words itself, never a sentence.
    #[test]
    fn answers_are_folders_names_and_reasons() {
        let saved = Saved {
            folder: Folder::Downloads,
            name: "wgal-log-20261103-213005.txt".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&saved).unwrap(),
            serde_json::json!({ "folder": "downloads", "name": "wgal-log-20261103-213005.txt" })
        );
        let refused = NotSaved {
            reason: Reason::CannotCreate,
            detail: Some("PermissionDenied".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            serde_json::json!({ "reason": "cannot_create", "detail": "PermissionDenied" })
        );
        assert_eq!(
            serde_json::to_value(NotSaved::from(Reason::TooMany)).unwrap(),
            serde_json::json!({ "reason": "too_many", "detail": null })
        );
        for (reason, code) in [
            (Reason::LinkedFolder, "linked_folder"),
            (Reason::Redirected, "redirected"),
        ] {
            assert_eq!(
                serde_json::to_value(NotSaved::from(reason)).unwrap(),
                serde_json::json!({ "reason": code, "detail": null })
            );
        }
    }

    #[test]
    fn the_stamp_is_local_time_at_the_offset() {
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        // 2026-01-01 00:00:00 UTC.
        assert_eq!(stamp(at(1_767_225_600), 480), "20260101-080000");
        assert_eq!(stamp(at(1_767_225_600), 0), "20260101-000000");
        assert_eq!(stamp(at(1_767_225_600), -300), "20251231-190000");
        assert_eq!(stamp(at(0), 0), "19700101-000000");
        // A leap day, and the day after.
        assert_eq!(stamp(at(1_709_164_800), 0), "20240229-000000");
        assert_eq!(stamp(at(1_709_251_199), 0), "20240229-235959");
        assert_eq!(stamp(at(1_709_251_200), 0), "20240301-000000");
        // 2026-11-03 13:30:05 UTC is 21:30:05 in Taipei.
        assert_eq!(stamp(at(1_793_712_605), 480), "20261103-213005");
    }

    /// Owner decision of 2026-11-04: the device name goes only into the
    /// settings file's name, and nothing of it can lead elsewhere.
    #[test]
    fn a_settings_file_is_named_after_the_device_safely() {
        assert_eq!(file_name_part("Office Laptop"), "Office-Laptop");
        assert_eq!(file_name_part("辦公室 筆電"), "辦公室-筆電");
        assert_eq!(file_name_part(r#"..\..\x/y:z*?"<>|"#), "xyz");
        assert_eq!(file_name_part("  --a  b--  "), "a-b");
        assert_eq!(file_name_part("\u{0}\u{1b}[31m"), "31m");
        assert_eq!(file_name_part("..."), "");
        assert_eq!(file_name_part(&"x".repeat(100)).chars().count(), 40);
        let names = FileName::settings("Office Laptop", "20261104-150000");
        assert_eq!(
            names.nth(1),
            "wgal-settings-Office-Laptop-20261104-150000.json"
        );
        assert_eq!(
            names.nth(2),
            "wgal-settings-Office-Laptop-20261104-150000-2.json"
        );
        assert_eq!(
            FileName::settings("", "20261104-150000").nth(1),
            "wgal-settings-20261104-150000.json"
        );
        let dir = temp_dir("settings");
        let name = write_settings(&dir, "Lab", "20261104-150000", b"{}").unwrap();
        assert_eq!(name, "wgal-settings-Lab-20261104-150000.json");
        assert_eq!(fs::read(dir.join(&name)).unwrap(), b"{}");
        // Never overwritten: the next name is taken.
        let again = write_settings(&dir, "Lab", "20261104-150000", b"{}").unwrap();
        assert_eq!(again, "wgal-settings-Lab-20261104-150000-2.json");
    }
}
