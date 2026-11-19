//! One-click MCP client setup (owner requests of 2026-11-12): adds the
//! `wgal` server, the `wgal-mcp` installed beside this window, to the
//! user-level MCP settings of one of a closed set of AI tools, or removes
//! that entry again.
//!
//! Nothing from the webview names a path: it picks a [`Client`], and the
//! file is that client's fixed place under one of the user's own folders
//! (the home folder, the platform's configuration folder, or the folder a
//! client documents an override for, `CODEX_HOME` or `CLAUDE_CONFIG_DIR`,
//! when it is absolute). The command written is the `wgal-mcp` beside this
//! window, and only when one is installed there; the window never runs it,
//! nor any of the tools (AGENTS.md rules 8 and 9).
//!
//! Where the file is: the base folder as the OS resolves it (a link at or
//! above it is the user's own layout, as for the log export), then the
//! client's own folders and file, none of which may be a link. Every file
//! is checked from the handle it is read and written through
//! (`wgal_platform::<os>::final_path_of_file`), so a link anywhere below
//! the base folder, or a folder swapped while it is used, is refused rather
//! than followed, and so is a file with more than one name (a hard link),
//! whose other names an edit in place would change too.
//!
//! The file is edited, not rewritten: only the `wgal` entry is added,
//! replaced or removed, and the user's other entries, their order, the formatting and
//! (where a tool allows them) comments stay as they were. A file that does
//! not parse, a link, or a file over [`MAX_BYTES`] is refused, never
//! overwritten. The edit is written into the same file through the handle
//! it was read with, so the file keeps its own permissions: on Windows that
//! handle lets no other process write, rename or delete the file until the
//! edit is done; on Linux and macOS a tool that replaces the file meanwhile
//! keeps its own file (the edit lands in the replaced one, and nothing is
//! said to be set up). What no OS can prevent there without the tool's
//! help is a tool rewriting the same file in place in the moment between
//! the last check and the write, so the window asks the user to quit the
//! tool first. Right before the write, the file is checked once more: still
//! one name, still where the client keeps it and (on Linux and macOS) still
//! the file at that path. If the write fails, the original, which is held in
//! memory, is written back through the same handle; only if that fails too
//! is the file said to be possibly damaged. No copy of the file is kept
//! anywhere else (owner decision of 2026-11-12): a settings file may hold
//! other servers' secrets. The file's contents never leave this module: the
//! webview gets only a state, the path and the entry that is (or would be)
//! written.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstContainerNode, CstInputValue, CstNode, CstObject, CstRootNode};
use serde::{Deserialize, Serialize};

use crate::mcp::open_itself;

#[cfg(target_os = "linux")]
use wgal_platform::linux::final_path_of_file;
#[cfg(target_os = "macos")]
use wgal_platform::macos::final_path_of_file;
#[cfg(windows)]
use wgal_platform::windows::{final_path_of_file, link_count};

/// No OS function says where a file is: never told, always refused.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn final_path_of_file(_file: &File) -> io::Result<PathBuf> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The tools the window can set up, in the order it lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    VsCode,
    Trae,
    Kiro,
    Codex,
    Antigravity,
    GeminiCli,
}

pub(crate) const CLIENTS: [Client; 9] = [
    Client::ClaudeCode,
    Client::ClaudeDesktop,
    Client::Cursor,
    Client::VsCode,
    Client::Trae,
    Client::Kiro,
    Client::Codex,
    Client::Antigravity,
    Client::GeminiCli,
];

/// Most bytes of a settings file read: Claude Code's `.claude.json` keeps
/// per-project state and can grow to several megabytes.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The name of the server in every tool's settings.
const NAME: &str = "wgal";

/// Windows' `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`
/// (winerror.h): another process holds the file open for writing.
#[cfg(windows)]
const SHARING_VIOLATIONS: [i32; 2] = [32, 33];

/// `FILE_SHARE_READ` (winnt.h): while the edit's handle is open, others may
/// read the file but not write, rename or delete it.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

/// The user's folders the clients' settings are found from.
#[derive(Clone, Debug)]
pub(crate) struct Folders {
    pub home: PathBuf,
    /// The platform's configuration folder: `%APPDATA%` on Windows,
    /// `~/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` or
    /// `~/.config` on Linux.
    pub config: PathBuf,
    /// `CODEX_HOME`, when set to an absolute path.
    pub codex_home: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR`, when set to an absolute path.
    pub claude_config: Option<PathBuf>,
}

impl Folders {
    /// An environment variable's folder, only when it is an absolute path.
    pub(crate) fn absolute(value: Option<OsString>) -> Option<PathBuf> {
        value.map(PathBuf::from).filter(|p| p.is_absolute())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// A JSON object `key` whose `wgal` member is the server; `typed` adds
    /// `"type": "stdio"`, which some tools require.
    Json { key: &'static str, typed: bool },
    /// Codex's `[mcp_servers.wgal]`.
    Toml,
}

/// Where a client keeps its settings, and in what form: the base folder,
/// then the client's own folders and the file. `create_folder`: the file's
/// own folder may be made when its parent exists, for tools whose top
/// folder exists before their settings folder does.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    base: PathBuf,
    rel: &'static [&'static str],
    format: Format,
    create_folder: bool,
}

impl Place {
    fn path(&self) -> PathBuf {
        self.rel
            .iter()
            .fold(self.base.clone(), |p, part| p.join(part))
    }
}

fn place(client: Client, f: &Folders) -> Place {
    let json = |base: &PathBuf, rel, key, typed| Place {
        base: base.clone(),
        rel,
        format: Format::Json { key, typed },
        create_folder: false,
    };
    match client {
        Client::ClaudeCode => json(
            f.claude_config.as_ref().unwrap_or(&f.home),
            &[".claude.json"],
            "mcpServers",
            true,
        ),
        Client::ClaudeDesktop => json(
            &f.config,
            &["Claude", "claude_desktop_config.json"],
            "mcpServers",
            false,
        ),
        Client::Cursor => json(&f.home, &[".cursor", "mcp.json"], "mcpServers", true),
        Client::VsCode => json(&f.config, &["Code", "User", "mcp.json"], "servers", true),
        Client::Trae => json(
            &f.config,
            &["Trae", "User", "mcp.json"],
            "mcpServers",
            false,
        ),
        Client::Kiro => Place {
            create_folder: true,
            ..json(
                &f.home,
                &[".kiro", "settings", "mcp.json"],
                "mcpServers",
                false,
            )
        },
        Client::Codex => Place {
            base: f.codex_home.clone().unwrap_or_else(|| f.home.clone()),
            rel: if f.codex_home.is_some() {
                &["config.toml"]
            } else {
                &[".codex", "config.toml"]
            },
            format: Format::Toml,
            create_folder: false,
        },
        Client::Antigravity => Place {
            create_folder: true,
            ..json(
                &f.home,
                &[".gemini", "config", "mcp_config.json"],
                "mcpServers",
                false,
            )
        },
        Client::GeminiCli => json(&f.home, &[".gemini", "settings.json"], "mcpServers", false),
    }
}

/// What the window can say about one client's settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    /// The `wgal` entry runs this window's `wgal-mcp`.
    Configured,
    /// There is a `wgal` entry, with another command.
    Other,
    /// No `wgal` entry yet (or no file yet, in a folder that is there).
    NotConfigured,
    /// The folder the file belongs in is not there: the tool is probably
    /// not installed.
    NoFolder,
    /// The file or a folder on its way is a link, or the file is too
    /// large, unreadable or does not parse: the window does not touch it.
    Unreadable,
}

/// One client as the webview shows it. `preview` is the entry as it is
/// written, without the user's other settings.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ClientView {
    pub client: Client,
    pub path: String,
    pub state: State,
    pub preview: String,
}

/// Why a client was not set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub(crate) enum NotSetUp {
    /// No `wgal-mcp` beside this window.
    NoServer,
    NoFolder,
    /// The file or a folder on its way is a link.
    Link,
    TooLarge,
    /// The file does not parse, or what holds the servers is not an object
    /// (a table, for Codex).
    NotParsable,
    /// Another process holds the file open, or replaced it while it was
    /// being edited: its own file stands, and the entry may not be there.
    Changed,
    /// Reading or writing failed; nothing was changed (a failed write was
    /// undone).
    Io,
    /// Writing failed and the original could not be written back, or a
    /// file the setup created could not be written whole: the file may be
    /// damaged or incomplete.
    Damaged,
    /// There is no `wgal` entry to remove.
    NotThere,
}

/// What a change does to the `wgal` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Add,
    Remove,
}

/// Every client's view, for the given server (`None`: none installed,
/// which shows the entry with the path beside the window, `fallback`).
pub(crate) fn all(folders: &Folders, server: Option<&Path>, fallback: &Path) -> Vec<ClientView> {
    CLIENTS
        .iter()
        .map(|&client| view(client, folders, server.unwrap_or(fallback)))
        .collect()
}

fn view(client: Client, folders: &Folders, server: &Path) -> ClientView {
    let place = place(client, folders);
    let state = match read_existing(&place) {
        Ok(Some(text)) => match configured(&text, place.format, server) {
            Some(Some(true)) => State::Configured,
            Some(Some(false)) => State::Other,
            Some(None) => State::NotConfigured,
            None => State::Unreadable,
        },
        Ok(None) => State::NotConfigured,
        Err(NotSetUp::NoFolder) => State::NoFolder,
        Err(_) => State::Unreadable,
    };
    ClientView {
        client,
        path: place.path().display().to_string(),
        state,
        preview: preview(place.format, server),
    }
}

/// Adds or replaces the `wgal` entry in `client`'s settings.
pub(crate) fn set_up(
    client: Client,
    folders: &Folders,
    server: Option<&Path>,
) -> Result<ClientView, NotSetUp> {
    set_up_with(client, folders, server, |_| ())
}

/// Removes the `wgal` entry from `client`'s settings, whatever command it
/// runs; the rest of the file stays. `shown` is the server the answer's
/// view compares with (this window's, or where it would be). Needs no
/// installed server: it also cleans up after WGAL was removed.
pub(crate) fn remove(
    client: Client,
    folders: &Folders,
    shown: &Path,
) -> Result<ClientView, NotSetUp> {
    change(client, folders, shown, Action::Remove, |_| ())
}

/// [`set_up`], with `between` run after the file was read and checked and
/// before the edit is written: in the tests, another process's write at
/// that moment.
fn set_up_with(
    client: Client,
    folders: &Folders,
    server: Option<&Path>,
    between: impl FnOnce(&Path),
) -> Result<ClientView, NotSetUp> {
    let server = server.ok_or(NotSetUp::NoServer)?;
    change(client, folders, server, Action::Add, between)
}

/// Adds or removes the entry, with `between` as in [`set_up_with`].
fn change(
    client: Client,
    folders: &Folders,
    server: &Path,
    action: Action,
    between: impl FnOnce(&Path),
) -> Result<ClientView, NotSetUp> {
    let place = place(client, folders);
    let expected = expected_path(&place)?;
    if !folder_ready(&place)? {
        if !place.create_folder || action == Action::Remove {
            return Err(NotSetUp::NoFolder);
        }
        let folder = place
            .path()
            .parent()
            .ok_or(NotSetUp::NoFolder)?
            .to_path_buf();
        fs::create_dir(&folder).map_err(|_| NotSetUp::Io)?;
    }
    let path = place.path();
    match open_for_edit(&path) {
        Ok(file) => edit_existing(file, &place, &expected, server, action, between)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound && action == Action::Remove => {
            return Err(NotSetUp::NotThere);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            create_fresh(&place, &expected, server, between)?
        }
        Err(e) if held_by_another(&e) => return Err(NotSetUp::Changed),
        // A link at the name does not open on Linux and macOS.
        Err(_) if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) => {
            return Err(NotSetUp::Link);
        }
        Err(_) => return Err(NotSetUp::Io),
    }
    Ok(view(client, folders, server))
}

/// The edit of a file that is there, through the one handle it is read,
/// checked and written with.
fn edit_existing(
    mut file: File,
    place: &Place,
    expected: &Path,
    server: &Path,
    action: Action,
    between: impl FnOnce(&Path),
) -> Result<(), NotSetUp> {
    let original = read_plain(&mut file)?;
    ready(&file, expected, &place.path())?;
    let text = match action {
        Action::Add => edit(&original, place.format, server)?,
        Action::Remove => without_entry(&original, place.format)?,
    };
    between(&place.path());
    // The last look, right before the write: what was read is still there,
    // under one name, where the client keeps it.
    file.seek(SeekFrom::Start(0)).map_err(|_| NotSetUp::Io)?;
    if read_plain(&mut file)? != original {
        return Err(NotSetUp::Changed);
    }
    ready(&file, expected, &place.path())?;
    rewrite(&mut file, original.as_bytes(), text.as_bytes())?;
    still_there(&file, &place.path())
}

/// Whether the held file may be written: (on Linux and macOS) still the
/// file at its path, which comes first, as a file replaced or moved away
/// meanwhile has changed rather than become a link; where the client keeps
/// it; and one name.
fn ready(file: &File, expected: &Path, path: &Path) -> Result<(), NotSetUp> {
    still_there(file, path)?;
    at(file, expected)?;
    single_name(file)
}

/// What the edit writes into: the settings file's handle, or in the tests a
/// stand-in that fails.
trait Content {
    /// Makes the whole content `bytes`: from the start, cut to its length,
    /// on disk.
    fn put(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl Content for File {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.seek(SeekFrom::Start(0))?;
        self.write_all(bytes)?;
        self.set_len(bytes.len() as u64)?;
        self.sync_all()
    }
}

/// Writes `text` over `original`; if that fails part way, writes the
/// original back: [`NotSetUp::Io`] when that worked (nothing changed),
/// [`NotSetUp::Damaged`] when it did not.
fn rewrite(target: &mut impl Content, original: &[u8], text: &[u8]) -> Result<(), NotSetUp> {
    match target.put(text) {
        Ok(()) => Ok(()),
        Err(_) => match target.put(original) {
            Ok(()) => Err(NotSetUp::Io),
            Err(_) => Err(NotSetUp::Damaged),
        },
    }
}

/// Refuses a file with more than one name: written in place, it would
/// change under its other names too.
fn single_name(file: &File) -> Result<(), NotSetUp> {
    #[cfg(windows)]
    let names = link_count(file).map_err(|_| NotSetUp::Io)?;
    #[cfg(unix)]
    let names = {
        use std::os::unix::fs::MetadataExt;
        file.metadata().map_err(|_| NotSetUp::Io)?.nlink()
    };
    #[cfg(not(any(windows, unix)))]
    let names = {
        let _ = file;
        2
    };
    if names == 1 {
        Ok(())
    } else {
        Err(NotSetUp::Link)
    }
}

/// The file created where there was none, in its checked folder, held as
/// the edit of an existing file is.
fn create_fresh(
    place: &Place,
    expected: &Path,
    server: &Path,
    between: impl FnOnce(&Path),
) -> Result<(), NotSetUp> {
    let folder = open_folder(place.path().parent().ok_or(NotSetUp::NoFolder)?)
        .map_err(|_| NotSetUp::Link)?;
    at(&folder, expected.parent().ok_or(NotSetUp::NoFolder)?)?;
    let text = edit("", place.format, server)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(FILE_SHARE_READ);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(place.path()) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(NotSetUp::Changed),
        Err(_) => return Err(NotSetUp::Io),
    };
    // Checked before a byte is written; refused, it stays empty where it
    // was created (removing it by its path could remove another file).
    ready(&file, expected, &place.path())?;
    between(&place.path());
    // The last look: still empty (nothing another process wrote into it is
    // written over), one name, where the client keeps it.
    if file.metadata().map_err(|_| NotSetUp::Io)?.len() != 0 {
        return Err(NotSetUp::Changed);
    }
    ready(&file, expected, &place.path())?;
    if file.put(text.as_bytes()).is_err() {
        // Left empty, or as far as it got: there was no file before, so
        // nothing is restored; the window says it may be incomplete.
        let _ = file.put(b"");
        return Err(NotSetUp::Damaged);
    }
    still_there(&file, &place.path())
}

/// The file's text, `None` when there is no file yet. The folder must be
/// a real folder (or creatable, for a client that allows it) and the file
/// a plain file where the client keeps it, of at most [`MAX_BYTES`].
fn read_existing(place: &Place) -> Result<Option<String>, NotSetUp> {
    if !folder_ready(place)? && !place.create_folder {
        return Err(NotSetUp::NoFolder);
    }
    let mut file = match open_itself(&place.path()) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(NotSetUp::Link),
    };
    let text = read_plain(&mut file)?;
    at(&file, &expected_path(place)?)?;
    single_name(&file)?;
    Ok(Some(text))
}

/// The whole text of a plain file, through its handle.
fn read_plain(file: &mut File) -> Result<String, NotSetUp> {
    let meta = file.metadata().map_err(|_| NotSetUp::Io)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(NotSetUp::Link);
    }
    if meta.len() > MAX_BYTES {
        return Err(NotSetUp::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| NotSetUp::Io)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(NotSetUp::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| NotSetUp::NotParsable)
}

/// Where the file must really be: the base folder as the OS resolves it,
/// then the client's own parts.
fn expected_path(place: &Place) -> Result<PathBuf, NotSetUp> {
    let base = open_base(&place.base).map_err(|_| NotSetUp::NoFolder)?;
    let real = final_path_of_file(&base).map_err(|_| NotSetUp::Io)?;
    Ok(place.rel.iter().fold(real, |p, part| p.join(part)))
}

/// Whether the open `file` (or folder) really is at `expected`: refused as
/// a link otherwise, or when the OS cannot tell.
fn at(file: &File, expected: &Path) -> Result<(), NotSetUp> {
    match final_path_of_file(file) {
        Ok(real) if same_path(&real, expected) => Ok(()),
        _ => Err(NotSetUp::Link),
    }
}

/// Paths compared exactly. The OS gives each part of a file's path in the
/// case it has on disk, so a folder whose name differs from the client's
/// only in case is refused: on Windows a folder can be case-sensitive, and
/// a link named in one case could then lead to a real folder named in the
/// other.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

/// Whether the file's folder is there as a real folder: `Ok(false)` when it
/// is missing but its parent is a real folder (it may be made); a link or
/// anything else there refuses.
fn folder_ready(place: &Place) -> Result<bool, NotSetUp> {
    let path = place.path();
    let folder = path.parent().ok_or(NotSetUp::NoFolder)?;
    match fs::symlink_metadata(folder) {
        Ok(meta) if meta.file_type().is_symlink() => Err(NotSetUp::Link),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(NotSetUp::NoFolder),
        Err(_) => {
            let parent = folder.parent().ok_or(NotSetUp::NoFolder)?;
            match fs::symlink_metadata(parent) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(false),
                _ => Err(NotSetUp::NoFolder),
            }
        }
    }
}

/// The base folder, opened as the OS resolves it (links followed).
fn open_base(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY);
    }
    options.open(path)
}

/// A folder, opened as itself: a link there does not open (or opens as the
/// link, which [`at`] then refuses).
fn open_folder(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | crate::export::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// `FILE_FLAG_BACKUP_SEMANTICS` and `FILE_READ_ATTRIBUTES` (winbase.h,
/// winnt.h): what opening a folder for its attributes needs.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_READ_ATTRIBUTES: u32 = 0x0080;

/// The settings file opened as itself for the edit: never through a link
/// at its name; on Windows shared for reading only, so no other process can
/// write, rename or delete it while the handle is open.
fn open_for_edit(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(crate::export::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// Whether opening failed because another process holds the file.
fn held_by_another(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        e.raw_os_error()
            .is_some_and(|code| SHARING_VIOLATIONS.contains(&code))
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

/// Whether the file written through `file` is still the one at `path`. On
/// Windows the handle kept everyone else from replacing it; elsewhere a
/// tool may have replaced it, and then the edit went into the replaced
/// file and the tool's own file stands.
fn still_there(file: &File, path: &Path) -> Result<(), NotSetUp> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata().map_err(|_| NotSetUp::Io)?;
        let named = fs::symlink_metadata(path).map_err(|_| NotSetUp::Changed)?;
        if held.dev() != named.dev() || held.ino() != named.ino() {
            return Err(NotSetUp::Changed);
        }
    }
    #[cfg(not(unix))]
    let _ = (file, path);
    Ok(())
}

/// The server's entry for a JSON tool.
fn entry_json(server: &Path, typed: bool) -> CstInputValue {
    let mut fields = Vec::new();
    if typed {
        fields.push(("type".to_owned(), CstInputValue::String("stdio".to_owned())));
    }
    fields.push((
        "command".to_owned(),
        CstInputValue::String(server.display().to_string()),
    ));
    fields.push(("args".to_owned(), CstInputValue::Array(Vec::new())));
    CstInputValue::Object(fields)
}

fn edit(text: &str, format: Format, server: &Path) -> Result<String, NotSetUp> {
    match format {
        Format::Json { key, typed } => edit_json(text, key, &entry_json(server, typed)),
        Format::Toml => edit_toml(text, server),
    }
}

/// The entry alone, as it is written into an empty file.
fn preview(format: Format, server: &Path) -> String {
    edit("", format, server).unwrap_or_default()
}

fn parse_json(text: &str) -> Result<CstRootNode, NotSetUp> {
    // As the tools read them: comments and trailing commas (VS Code's
    // JSONC), nothing looser.
    let options = ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        ..ParseOptions::default()
    };
    CstRootNode::parse(text, &options).map_err(|_| NotSetUp::NotParsable)
}

fn root_object(root: &CstRootNode) -> Result<CstObject, NotSetUp> {
    match root.value() {
        None => Ok(root.object_value_or_set()),
        Some(CstNode::Container(CstContainerNode::Object(object))) => Ok(object),
        Some(_) => Err(NotSetUp::NotParsable),
    }
}

fn edit_json(text: &str, key: &str, entry: &CstInputValue) -> Result<String, NotSetUp> {
    let (bom, text) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
    let root = parse_json(text)?;
    let servers = root_object(&root)?
        .object_value_or_create(key)
        .ok_or(NotSetUp::NotParsable)?;
    match servers.get(NAME) {
        Some(prop) => prop.set_value(entry.clone()),
        None => {
            servers.append(NAME, entry.clone());
        }
    }
    let mut out = format!("{bom}{root}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The text without the `wgal` entry; [`NotSetUp::NotThere`] when it has
/// none. What held it stays, even when it is left empty.
fn without_entry(text: &str, format: Format) -> Result<String, NotSetUp> {
    match format {
        Format::Json { key, .. } => {
            let (bom, body) = match text.strip_prefix('\u{feff}') {
                Some(rest) => ("\u{feff}", rest),
                None => ("", text),
            };
            let root = parse_json(body)?;
            let object = match root.value() {
                None => return Err(NotSetUp::NotThere),
                Some(node) => node.as_object().ok_or(NotSetUp::NotParsable)?,
            };
            let Some(prop) = object.get(key) else {
                return Err(NotSetUp::NotThere);
            };
            let servers = prop
                .value()
                .and_then(|v| v.as_object())
                .ok_or(NotSetUp::NotParsable)?;
            servers.get(NAME).ok_or(NotSetUp::NotThere)?.remove();
            Ok(format!("{bom}{root}"))
        }
        Format::Toml => {
            let mut doc: toml_edit::DocumentMut =
                text.parse().map_err(|_| NotSetUp::NotParsable)?;
            let Some(servers) = doc.get_mut("mcp_servers") else {
                return Err(NotSetUp::NotThere);
            };
            let servers = servers.as_table_like_mut().ok_or(NotSetUp::NotParsable)?;
            servers.remove(NAME).ok_or(NotSetUp::NotThere)?;
            Ok(doc.to_string())
        }
    }
}

fn edit_toml(text: &str, server: &Path) -> Result<String, NotSetUp> {
    use toml_edit::{Array, DocumentMut, Item, Table, value};
    let mut doc: DocumentMut = text.parse().map_err(|_| NotSetUp::NotParsable)?;
    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let servers = servers.as_table_mut().ok_or(NotSetUp::NotParsable)?;
    let mut entry = Table::new();
    entry["command"] = value(server.display().to_string());
    entry["args"] = value(Array::new());
    servers.insert(NAME, Item::Table(entry));
    Ok(doc.to_string())
}

/// Whether the settings' `wgal` entry runs `server`: `Some(Some(true))`
/// yes, `Some(Some(false))` another command, `Some(None)` no entry, `None`
/// the text does not parse or holds the servers in something else.
fn configured(text: &str, format: Format, server: &Path) -> Option<Option<bool>> {
    let wanted = server.display().to_string();
    match format {
        Format::Json { key, .. } => {
            let root = parse_json(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()?;
            let object = match root.value() {
                None => return Some(None),
                Some(node) => node.as_object()?,
            };
            let Some(prop) = object.get(key) else {
                return Some(None);
            };
            let servers = prop.value()?.as_object()?;
            let Some(entry) = servers.get(NAME) else {
                return Some(None);
            };
            let command = entry
                .value()
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("command"))
                .and_then(|p| p.value())
                .and_then(|v| v.as_string_lit())
                .and_then(|s| s.decoded_value().ok());
            Some(Some(command.as_deref() == Some(wanted.as_str())))
        }
        Format::Toml => {
            let doc: toml_edit::DocumentMut = text.parse().ok()?;
            let Some(servers) = doc.get("mcp_servers") else {
                return Some(None);
            };
            let servers = servers.as_table_like()?;
            let Some(entry) = servers.get(NAME) else {
                return Some(None);
            };
            let command = entry
                .as_table_like()
                .and_then(|t| t.get("command"))
                .and_then(|c| c.as_str());
            Some(Some(command == Some(wanted.as_str())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wgal-mcpclients-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn folders(home: &Path) -> Folders {
        Folders {
            home: home.to_path_buf(),
            config: home.join("config"),
            codex_home: None,
            claude_config: None,
        }
    }

    fn server() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\WGAL\wgal-mcp.exe")
        } else {
            PathBuf::from("/opt/wgal/wgal-mcp")
        }
    }

    #[test]
    fn json_keeps_the_other_entries_order_and_comments() {
        let text = "{\n  // mine\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" },\n  },\n  \"zeta\": 1\n}\n";
        let out = edit_json(text, "mcpServers", &entry_json(&server(), false)).unwrap();
        assert!(out.contains("// mine"), "{out}");
        let theme = out.find("\"theme\"").unwrap();
        let servers = out.find("\"mcpServers\"").unwrap();
        let zeta = out.find("\"zeta\"").unwrap();
        assert!(theme < servers && servers < zeta, "{out}");
        assert!(
            out.find("\"other\"").unwrap() < out.find("\"wgal\"").unwrap(),
            "{out}"
        );
        assert_eq!(
            configured(
                &out,
                Format::Json {
                    key: "mcpServers",
                    typed: false
                },
                &server()
            ),
            Some(Some(true))
        );
    }

    #[test]
    fn json_replaces_only_the_wgal_entry() {
        let text = "{\"mcpServers\":{\"wgal\":{\"command\":\"old\",\"args\":[\"-x\"]},\"b\":{\"command\":\"y\"}}}";
        let format = Format::Json {
            key: "mcpServers",
            typed: true,
        };
        assert_eq!(configured(text, format, &server()), Some(Some(false)));
        let out = edit_json(text, "mcpServers", &entry_json(&server(), true)).unwrap();
        assert!(!out.contains("\"old\"") && !out.contains("-x"), "{out}");
        assert!(out.contains("\"b\""), "{out}");
        assert!(out.contains("\"stdio\""), "{out}");
        assert_eq!(configured(&out, format, &server()), Some(Some(true)));
        // The path's backslashes are escaped: a plain JSON reader agrees.
        let plain: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            plain["mcpServers"]["wgal"]["command"],
            server().display().to_string()
        );
        assert_eq!(plain["mcpServers"]["wgal"]["args"], serde_json::json!([]));
    }

    #[test]
    fn json_refuses_what_it_cannot_edit() {
        let entry = entry_json(&server(), false);
        assert_eq!(
            edit_json("[1]", "mcpServers", &entry),
            Err(NotSetUp::NotParsable)
        );
        assert_eq!(
            edit_json("{\"mcpServers\": 3}", "mcpServers", &entry),
            Err(NotSetUp::NotParsable)
        );
        assert_eq!(
            edit_json("{\"a\": ", "mcpServers", &entry),
            Err(NotSetUp::NotParsable)
        );
        assert_eq!(
            edit_json("{a: 1}", "mcpServers", &entry),
            Err(NotSetUp::NotParsable)
        );
        // An empty file is an empty object; a BOM stays.
        assert!(
            edit_json("", "servers", &entry)
                .unwrap()
                .contains("\"servers\"")
        );
        assert!(
            edit_json("\u{feff}{}", "servers", &entry)
                .unwrap()
                .starts_with('\u{feff}')
        );
    }

    #[test]
    fn toml_keeps_the_rest_and_replaces_the_entry() {
        let text = "# mine\nmodel = \"m\"\n\n[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.wgal]\ncommand = \"old\"\nenv = { A = \"1\" }\n";
        assert_eq!(configured(text, Format::Toml, &server()), Some(Some(false)));
        let out = edit_toml(text, &server()).unwrap();
        assert!(out.starts_with("# mine\nmodel = \"m\""), "{out}");
        assert!(
            out.contains("[mcp_servers.other]") && !out.contains("old") && !out.contains("env"),
            "{out}"
        );
        assert_eq!(configured(&out, Format::Toml, &server()), Some(Some(true)));
        let fresh = edit_toml("", &server()).unwrap();
        assert!(
            !fresh.contains("[mcp_servers]\n"),
            "no empty parent table: {fresh}"
        );
        assert_eq!(
            configured(&fresh, Format::Toml, &server()),
            Some(Some(true))
        );
        assert_eq!(
            edit_toml("mcp_servers = 1\n", &server()),
            Err(NotSetUp::NotParsable)
        );
        assert_eq!(edit_toml("[a\n", &server()), Err(NotSetUp::NotParsable));
    }

    #[test]
    fn places_are_fixed_and_overrides_must_be_absolute() {
        let home = PathBuf::from(if cfg!(windows) {
            r"C:\Users\u"
        } else {
            "/home/u"
        });
        let mut f = folders(&home);
        assert_eq!(
            place(Client::Cursor, &f).path(),
            home.join(".cursor").join("mcp.json")
        );
        assert_eq!(
            place(Client::Codex, &f).path(),
            home.join(".codex").join("config.toml")
        );
        assert_eq!(
            place(Client::VsCode, &f).format,
            Format::Json {
                key: "servers",
                typed: true
            }
        );
        f.codex_home = Folders::absolute(Some(home.join("cx").into_os_string()));
        assert_eq!(
            place(Client::Codex, &f).path(),
            home.join("cx").join("config.toml")
        );
        assert_eq!(Folders::absolute(Some("relative/dir".into())), None);
        assert_eq!(Folders::absolute(None), None);
    }

    #[test]
    fn previews_are_the_entry_alone() {
        let json = preview(
            Format::Json {
                key: "servers",
                typed: true,
            },
            &server(),
        );
        let plain: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            plain,
            serde_json::json!({ "servers": { "wgal": { "type": "stdio", "command": server().display().to_string(), "args": [] } } })
        );
        let toml = preview(Format::Toml, &server());
        assert!(toml.starts_with("[mcp_servers.wgal]\n"), "{toml}");
    }

    #[test]
    fn set_up_edits_in_place_and_leaves_no_other_file() {
        let home = temp_home("setup");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let path = home.join(".cursor").join("mcp.json");
        let original = "{\n  \"mcpServers\": {\n    \"a\": { \"command\": \"a\" }\n  }\n}\n";
        fs::write(&path, original).unwrap();
        assert_eq!(
            view(Client::Cursor, &f, &server()).state,
            State::NotConfigured
        );
        let done = set_up(Client::Cursor, &f, Some(&server())).unwrap();
        assert_eq!(done.state, State::Configured);
        let now = fs::read_to_string(&path).unwrap();
        assert!(now.contains("\"a\"") && now.contains("\"wgal\""), "{now}");
        // Nothing but the settings file in the tool's folder, and no copy
        // anywhere else.
        let names = |dir: &Path| -> Vec<String> {
            let mut n: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .collect();
            n.sort();
            n
        };
        assert_eq!(names(&home.join(".cursor")), ["mcp.json"]);
        assert_eq!(names(&home), [".cursor"]);
        // Again: replaced, still one entry; a shorter text leaves no tail.
        set_up(Client::Cursor, &f, Some(&server())).unwrap();
        let again = fs::read_to_string(&path).unwrap();
        assert_eq!(again.matches("\"wgal\"").count(), 1, "{again}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&again).is_ok(),
            "{again}"
        );
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn set_up_needs_the_server_and_the_tools_folder() {
        let home = temp_home("folders");
        let f = folders(&home);
        assert_eq!(
            set_up(Client::Cursor, &f, None).unwrap_err(),
            NotSetUp::NoServer
        );
        // Cursor is not installed: its folder is not made.
        assert_eq!(view(Client::Cursor, &f, &server()).state, State::NoFolder);
        assert_eq!(
            set_up(Client::Cursor, &f, Some(&server())).unwrap_err(),
            NotSetUp::NoFolder
        );
        assert!(!home.join(".cursor").exists());
        // Claude Desktop is not installed either: its folder under the
        // configuration folder is not made.
        fs::create_dir_all(home.join("config")).unwrap();
        assert_eq!(
            set_up(Client::ClaudeDesktop, &f, Some(&server())).unwrap_err(),
            NotSetUp::NoFolder
        );
        // Kiro's settings folder is made inside its own folder; a new file
        // has no original to keep.
        fs::create_dir_all(home.join(".kiro")).unwrap();
        assert_eq!(
            set_up(Client::Kiro, &f, Some(&server())).unwrap().state,
            State::Configured
        );
        assert!(
            home.join(".kiro")
                .join("settings")
                .join("mcp.json")
                .is_file()
        );
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn set_up_refuses_a_file_it_cannot_parse_and_leaves_it() {
        let home = temp_home("broken");
        let f = folders(&home);
        fs::create_dir_all(home.join(".gemini")).unwrap();
        let path = home.join(".gemini").join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            view(Client::GeminiCli, &f, &server()).state,
            State::Unreadable
        );
        assert_eq!(
            set_up(Client::GeminiCli, &f, Some(&server())).unwrap_err(),
            NotSetUp::NotParsable
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        fs::remove_dir_all(&home).unwrap();
    }

    /// Another process writing in place between the check and the write:
    /// on Windows the edit's handle keeps it out; elsewhere the last look
    /// catches a write made before it.
    #[test]
    fn a_write_by_another_process_meanwhile_is_kept_out_or_kept() {
        let home = temp_home("meanwhile");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let path = home.join(".cursor").join("mcp.json");
        fs::write(&path, "{}").unwrap();
        let mut theirs = None;
        let result = set_up_with(Client::Cursor, &f, Some(&server()), |p| {
            theirs = Some(fs::write(p, "{\"theirs\": 1}"));
        });
        if cfg!(windows) {
            assert!(
                theirs.unwrap().is_err(),
                "the other write is refused while the edit holds the file"
            );
            assert_eq!(result.unwrap().state, State::Configured);
        } else {
            assert!(theirs.unwrap().is_ok());
            assert_eq!(result.unwrap_err(), NotSetUp::Changed);
            assert_eq!(fs::read_to_string(&path).unwrap(), "{\"theirs\": 1}");
        }
        fs::remove_dir_all(&home).unwrap();
    }

    /// Another process replacing the file between the check and the write
    /// (as tools that write a new file and rename it do): its file stands.
    #[test]
    fn a_replacement_by_another_process_meanwhile_stands() {
        let home = temp_home("replaced");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let path = home.join(".cursor").join("mcp.json");
        fs::write(&path, "{}").unwrap();
        fs::write(home.join("theirs.json"), "{\"theirs\": 1}").unwrap();
        let mut renamed = None;
        let result = set_up_with(Client::Cursor, &f, Some(&server()), |p| {
            renamed = Some(fs::rename(home.join("theirs.json"), p));
        });
        if cfg!(windows) {
            assert!(
                renamed.unwrap().is_err(),
                "the file cannot be replaced while the edit holds it"
            );
            assert_eq!(result.unwrap().state, State::Configured);
        } else {
            assert!(renamed.unwrap().is_ok());
            assert_eq!(result.unwrap_err(), NotSetUp::Changed);
            assert_eq!(fs::read_to_string(&path).unwrap(), "{\"theirs\": 1}");
        }
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_file_with_another_name_is_not_edited() {
        let home = temp_home("hardlink");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        fs::write(home.join("other.json"), "{}").unwrap();
        fs::hard_link(
            home.join("other.json"),
            home.join(".cursor").join("mcp.json"),
        )
        .unwrap();
        assert_eq!(view(Client::Cursor, &f, &server()).state, State::Unreadable);
        assert_eq!(
            set_up(Client::Cursor, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        assert_eq!(fs::read_to_string(home.join("other.json")).unwrap(), "{}");
        fs::remove_dir_all(&home).unwrap();
    }

    /// Another process writing into, or replacing, a file the setup has
    /// just created: kept out on Windows; elsewhere its write is not
    /// written over and its replacement stands.
    #[test]
    fn a_new_file_is_held_too() {
        for replace in [false, true] {
            let home = temp_home(if replace {
                "fresh-replaced"
            } else {
                "fresh-written"
            });
            let f = folders(&home);
            fs::create_dir_all(home.join(".cursor")).unwrap();
            let path = home.join(".cursor").join("mcp.json");
            fs::write(home.join("theirs.json"), "{\"theirs\": 1}").unwrap();
            let mut theirs = None;
            let result = set_up_with(Client::Cursor, &f, Some(&server()), |p| {
                theirs = Some(if replace {
                    fs::rename(home.join("theirs.json"), p)
                } else {
                    fs::write(p, "{\"theirs\": 1}")
                });
            });
            if cfg!(windows) {
                assert!(
                    theirs.unwrap().is_err(),
                    "kept out while the setup holds the new file"
                );
                assert_eq!(result.unwrap().state, State::Configured);
            } else {
                assert!(theirs.unwrap().is_ok());
                assert_eq!(result.unwrap_err(), NotSetUp::Changed);
                assert_eq!(fs::read_to_string(&path).unwrap(), "{\"theirs\": 1}");
            }
            fs::remove_dir_all(&home).unwrap();
        }
    }

    /// A stand-in for the file whose writes fail from the `fail_from`-th on.
    struct Failing {
        content: Vec<u8>,
        puts: usize,
        fail_from: usize,
    }

    impl Content for Failing {
        fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.puts += 1;
            if self.puts >= self.fail_from {
                // Part way: the first byte only.
                self.content
                    .splice(..1.min(self.content.len()), bytes.iter().take(1).copied());
                return Err(io::ErrorKind::Other.into());
            }
            self.content = bytes.to_vec();
            Ok(())
        }
    }

    #[test]
    fn a_failed_write_is_undone_or_said_to_damage() {
        let mut ok = Failing {
            content: b"old".to_vec(),
            puts: 0,
            fail_from: 9,
        };
        assert_eq!(rewrite(&mut ok, b"old", b"new text"), Ok(()));
        assert_eq!(ok.content, b"new text");
        let mut undone = Failing {
            content: b"old".to_vec(),
            puts: 0,
            fail_from: 1,
        };
        // The first write fails; the second (the original) is let through.
        let mut first_only = FailOnce(Failing {
            content: b"old".to_vec(),
            puts: 0,
            fail_from: 1,
        });
        assert_eq!(
            rewrite(&mut first_only, b"old", b"new text"),
            Err(NotSetUp::Io)
        );
        assert_eq!(first_only.0.content, b"old");
        assert_eq!(
            rewrite(&mut undone, b"old", b"new text"),
            Err(NotSetUp::Damaged)
        );
    }

    /// Fails only the first write.
    struct FailOnce(Failing);

    impl Content for FailOnce {
        fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.0.puts == 0 {
                self.0.puts = 1;
                return Err(io::ErrorKind::Other.into());
            }
            self.0.content = bytes.to_vec();
            Ok(())
        }
    }

    /// Another name made for the held file between the check and the write:
    /// refused before either name changes (Windows may refuse the link
    /// itself while the file is held).
    #[test]
    fn a_second_name_made_meanwhile_is_refused() {
        for fresh in [false, true] {
            let home = temp_home(if fresh { "link-fresh" } else { "link-existing" });
            let f = folders(&home);
            fs::create_dir_all(home.join(".cursor")).unwrap();
            let path = home.join(".cursor").join("mcp.json");
            if !fresh {
                fs::write(&path, "{}").unwrap();
            }
            let other = home.join("other.json");
            let mut linked = None;
            let result = set_up_with(Client::Cursor, &f, Some(&server()), |p| {
                linked = Some(fs::hard_link(p, &other));
            });
            match linked.unwrap() {
                Ok(()) => {
                    assert_eq!(result.unwrap_err(), NotSetUp::Link);
                    let before = if fresh { "" } else { "{}" };
                    assert_eq!(fs::read_to_string(&path).unwrap(), before);
                    assert_eq!(fs::read_to_string(&other).unwrap(), before);
                }
                Err(_) => assert_eq!(result.unwrap().state, State::Configured),
            }
            fs::remove_dir_all(&home).unwrap();
        }
    }

    /// The new file moved elsewhere and another put at its path between the
    /// check and the write: the moved file gets nothing, the other stands.
    #[test]
    fn a_new_file_moved_away_meanwhile_gets_nothing() {
        let home = temp_home("moved");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let path = home.join(".cursor").join("mcp.json");
        let moved = home.join("moved.json");
        let mut done = None;
        let result = set_up_with(Client::Cursor, &f, Some(&server()), |p| {
            done = Some(fs::rename(p, &moved).and_then(|()| fs::write(p, "{\"theirs\": 1}")));
        });
        match done.unwrap() {
            Ok(()) => {
                assert_eq!(result.unwrap_err(), NotSetUp::Changed);
                assert_eq!(fs::read_to_string(&moved).unwrap(), "");
                assert_eq!(fs::read_to_string(&path).unwrap(), "{\"theirs\": 1}");
            }
            // Windows: the held file cannot be moved.
            Err(_) => assert_eq!(result.unwrap().state, State::Configured),
        }
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn removing_takes_out_only_the_entry() {
        let text = "{\n  // mine\n  \"mcpServers\": {\n    \"a\": { \"command\": \"a\" },\n    \"wgal\": { \"command\": \"x\" }\n  },\n  \"z\": 1\n}\n";
        let json = Format::Json {
            key: "mcpServers",
            typed: false,
        };
        let out = without_entry(text, json).unwrap();
        assert!(
            !out.contains("wgal")
                && out.contains("// mine")
                && out.contains("\"a\"")
                && out.contains("\"z\""),
            "{out}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&out.replace("// mine", "")).is_ok(),
            "{out}"
        );
        assert_eq!(configured(&out, json, &server()), Some(None));
        assert_eq!(without_entry(&out, json), Err(NotSetUp::NotThere));
        assert_eq!(without_entry("{}", json), Err(NotSetUp::NotThere));
        assert_eq!(without_entry("", json), Err(NotSetUp::NotThere));
        assert_eq!(
            without_entry("{\"mcpServers\": 1}", json),
            Err(NotSetUp::NotParsable)
        );
        let toml = "model = \"m\"\n\n[mcp_servers.a]\ncommand = \"a\"\n\n[mcp_servers.wgal]\ncommand = \"x\"\nargs = []\n";
        let out = without_entry(toml, Format::Toml).unwrap();
        assert!(
            !out.contains("wgal") && out.contains("[mcp_servers.a]") && out.starts_with("model"),
            "{out}"
        );
        assert_eq!(without_entry(&out, Format::Toml), Err(NotSetUp::NotThere));
    }

    #[test]
    fn remove_undoes_a_setup_and_needs_no_server() {
        let home = temp_home("remove");
        let f = folders(&home);
        fs::create_dir_all(home.join(".gemini")).unwrap();
        let path = home.join(".gemini").join("settings.json");
        // No file, no entry.
        assert_eq!(
            remove(Client::GeminiCli, &f, &server()).unwrap_err(),
            NotSetUp::NotThere
        );
        assert!(!path.exists());
        fs::write(&path, "{\"theme\": \"dark\"}").unwrap();
        set_up(Client::GeminiCli, &f, Some(&server())).unwrap();
        // Removing an entry that runs another program works too.
        let other = PathBuf::from(if cfg!(windows) {
            r"C:\elsewhere\wgal-mcp.exe"
        } else {
            "/elsewhere/wgal-mcp"
        });
        assert_eq!(view(Client::GeminiCli, &f, &other).state, State::Other);
        let done = remove(Client::GeminiCli, &f, &other).unwrap();
        assert_eq!(done.state, State::NotConfigured);
        let now = fs::read_to_string(&path).unwrap();
        assert!(now.contains("\"theme\"") && !now.contains("wgal"), "{now}");
        assert_eq!(
            remove(Client::GeminiCli, &f, &server()).unwrap_err(),
            NotSetUp::NotThere
        );
        // A tool that is not installed has nothing to remove, and nothing is made.
        assert_eq!(
            remove(Client::Kiro, &f, &server()).unwrap_err(),
            NotSetUp::NoFolder
        );
        assert!(!home.join(".kiro").exists());
        fs::remove_dir_all(&home).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_file_held_open_by_a_tool_is_not_edited() {
        use std::os::windows::fs::OpenOptionsExt;
        let home = temp_home("held");
        let f = folders(&home);
        fs::create_dir_all(home.join(".cursor")).unwrap();
        let path = home.join(".cursor").join("mcp.json");
        fs::write(&path, "{}").unwrap();
        let held = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        assert_eq!(
            set_up(Client::Cursor, &f, Some(&server())).unwrap_err(),
            NotSetUp::Changed
        );
        drop(held);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        fs::remove_dir_all(&home).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn junctions_on_the_way_are_refused() {
        let home = temp_home("junction");
        let f = folders(&home);
        // The settings folder itself is a junction.
        fs::create_dir_all(home.join("real")).unwrap();
        junction::create(home.join("real"), home.join(".cursor")).unwrap();
        assert_eq!(view(Client::Cursor, &f, &server()).state, State::Unreadable);
        assert_eq!(
            set_up(Client::Cursor, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        assert!(fs::read_dir(home.join("real")).unwrap().next().is_none());
        // A folder above it is: .kiro leads elsewhere, where a real
        // settings folder holds a real file.
        fs::create_dir_all(home.join("elsewhere").join("settings")).unwrap();
        let theirs = home.join("elsewhere").join("settings").join("mcp.json");
        fs::write(&theirs, "{}").unwrap();
        junction::create(home.join("elsewhere"), home.join(".kiro")).unwrap();
        assert_eq!(view(Client::Kiro, &f, &server()).state, State::Unreadable);
        assert_eq!(
            set_up(Client::Kiro, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        assert_eq!(fs::read_to_string(&theirs).unwrap(), "{}");
        fs::remove_dir(home.join(".cursor")).unwrap();
        fs::remove_dir(home.join(".kiro")).unwrap();
        fs::remove_dir_all(&home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_on_the_way_are_refused() {
        let home = temp_home("links");
        let f = folders(&home);
        fs::create_dir_all(home.join("real")).unwrap();
        std::os::unix::fs::symlink(home.join("real"), home.join(".cursor")).unwrap();
        assert_eq!(
            set_up(Client::Cursor, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        fs::create_dir_all(home.join(".gemini")).unwrap();
        fs::write(home.join("elsewhere.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            home.join("elsewhere.json"),
            home.join(".gemini").join("settings.json"),
        )
        .unwrap();
        assert_eq!(
            set_up(Client::GeminiCli, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        assert_eq!(
            fs::read_to_string(home.join("elsewhere.json")).unwrap(),
            "{}"
        );
        fs::create_dir_all(home.join("other").join("settings")).unwrap();
        fs::write(home.join("other").join("settings").join("mcp.json"), "{}").unwrap();
        std::os::unix::fs::symlink(home.join("other"), home.join(".kiro")).unwrap();
        assert_eq!(
            set_up(Client::Kiro, &f, Some(&server())).unwrap_err(),
            NotSetUp::Link
        );
        assert_eq!(
            fs::read_to_string(home.join("other").join("settings").join("mcp.json")).unwrap(),
            "{}"
        );
        fs::remove_dir_all(&home).unwrap();
    }
}
