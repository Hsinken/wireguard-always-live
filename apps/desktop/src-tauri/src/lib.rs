//! Tauri shell for the WGAL desktop GUI.
//!
//! The GUI renders state produced by the watchdog and forwards the fixed
//! operations defined in `wgal-protocol`. It never controls tunnels, spawns
//! processes, sends notifications or holds elevated privileges. Every command
//! exposed to the webview must also be listed in `build.rs` and granted in
//! `capabilities/`, and still validates its input.
//!
//! Closing the window only hides it; quitting the GUI never stops the
//! watchdog, which runs as its own background service (plan §1.1, T13).

mod dot;
mod export;
#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
mod ipc;
mod lang;
mod mcp;
mod mcpclients;

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent, Wry};
use wgal_protocol::{
    DisplayName, ErrorCode, ErrorReason, Outcome, PROTOCOL_VERSION, ProtocolError, ProtocolInfo,
    RequestEnvelope, RequestId, ResponseEnvelope, SettingsDocument, codec,
};

const TRAY_ID: &str = "main";
/// The tray falls back to "unknown" when the window has not refreshed it for
/// this long, or for three of the window's refresh intervals when that is
/// longer (see [`stale_after`]): the plan's 15 s lost-link rule (§6.2),
/// which a hidden window's throttled timers could otherwise outlast.
const TRAY_STALE_AFTER: Duration = Duration::from_secs(15);

/// How often the window refreshes, in seconds, as its tray summary says:
/// 3 to 300 (owner's request of 2026-11-03), anything else refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u16")]
struct RefreshSecs(u16);

impl TryFrom<u16> for RefreshSecs {
    type Error = &'static str;

    fn try_from(secs: u16) -> Result<Self, Self::Error> {
        if (3..=300).contains(&secs) {
            Ok(Self(secs))
        } else {
            Err("refresh_secs must be 3 to 300")
        }
    }
}

/// How long a tray summary stays current: 15 s, or three of the window's
/// intervals when that is longer, the same rule the window uses for its
/// lost-link overlay; 15 s before the first summary says the interval.
fn stale_after(refresh: Option<RefreshSecs>) -> Duration {
    let intervals = refresh.map_or(Duration::ZERO, |r| Duration::from_secs(3 * u64::from(r.0)));
    TRAY_STALE_AFTER.max(intervals)
}

/// How long, in milliseconds from when its summary arrives, a green dot may
/// stay: until the evidence behind it runs out, as the window worked it out
/// (`trayGreenForMs` in `view.ts`). 0 to 3,700,000: the longest freshness a
/// tunnel can have (3,672 s) with a margin, `TRAY_GREEN_MAX_MS` there;
/// anything else refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u32")]
struct GreenForMs(u32);

impl GreenForMs {
    const MAX: u32 = 3_700_000;
}

impl TryFrom<u32> for GreenForMs {
    type Error = &'static str;

    fn try_from(ms: u32) -> Result<Self, Self::Error> {
        if ms <= Self::MAX {
            Ok(Self(ms))
        } else {
            Err("green_for_ms must be 0 to 3700000")
        }
    }
}

/// How often the shell looks at the tray summary's age and its green dot's
/// deadline, so neither outlives its time by more than this.
const TRAY_WATCH_EVERY: Duration = Duration::from_secs(1);

/// The window's last tray summary on the shell's own monotonic clock: when
/// it came, how often the next one is due and until when its green dot
/// holds; and whether the tray says "unknown" for it already. It decides
/// everything [`watch_tray`] does, without the window: a hidden window's
/// timers may stall, this clock does not.
#[derive(Clone, Copy, Debug)]
struct TrayClock {
    started: Instant,
    last: Option<LastSummary>,
    stale_shown: bool,
}

#[derive(Clone, Copy, Debug)]
struct LastSummary {
    at: Instant,
    refresh: RefreshSecs,
    /// `None` when the summary has no green dot.
    green_until: Option<Instant>,
}

impl TrayClock {
    fn new(started: Instant) -> Self {
        Self {
            started,
            last: None,
            stale_shown: false,
        }
    }

    /// `summary` arrived at `now` and is shown. Its green dot holds for as
    /// long as it says, from now; a deadline that came without a green dot
    /// means nothing.
    fn summary(&mut self, summary: &TraySummary, now: Instant) {
        let green_for = Duration::from_millis(u64::from(summary.green_for_ms.0));
        self.last = Some(LastSummary {
            at: now,
            refresh: summary.refresh_secs,
            // Past the end of `Instant`, the dot holds no longer than now.
            green_until: (summary.dot == Dot::Green)
                .then(|| now.checked_add(green_for).unwrap_or(now)),
        });
        self.stale_shown = false;
    }

    /// Whether the tray must say "unknown" at `now` instead of the last
    /// summary: none for [`stale_after`] its interval (15 s from the start
    /// before the first), or a green dot whose evidence ran out before the
    /// next summary came.
    fn stale(&self, now: Instant) -> bool {
        match self.last {
            None => now.saturating_duration_since(self.started) > TRAY_STALE_AFTER,
            Some(last) => {
                now.saturating_duration_since(last.at) > stale_after(Some(last.refresh))
                    || last.green_until.is_some_and(|until| now > until)
            }
        }
    }

    /// Whether the tray must be switched to "unknown" at `now`: stale, and
    /// not shown so yet.
    fn due(&self, now: Instant) -> bool {
        !self.stale_shown && self.stale(now)
    }

    /// The tray says "unknown" now, until the next summary.
    fn shown_stale(&mut self) {
        self.stale_shown = true;
    }
}

use dot::Dot;
use lang::{Language, Text};

/// The window's last tray summary on the shell's clock ([`TrayClock`]),
/// whether a tray exists, the tray's three icons and which of them it shows,
/// the language the window last said it shows, and the menu's items,
/// relabelled when that language changes.
#[derive(Debug)]
struct TrayState {
    clock: Mutex<TrayClock>,
    available: Mutex<bool>,
    icons: OnceLock<TrayIcons>,
    shown: Mutex<Dot>,
    language: Mutex<&'static Language>,
    menu: OnceLock<TrayMenu>,
}

/// The tray menu's items with words, in the order of [`TrayMenu::TEXTS`].
struct TrayMenu {
    items: [MenuItem<Wry>; 4],
}

impl std::fmt::Debug for TrayMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tauri's menu items have no `Debug` of their own.
        f.debug_struct("TrayMenu").finish_non_exhaustive()
    }
}

impl TrayMenu {
    /// What each item says, and its ID.
    const TEXTS: [(Text, &'static str); 4] = [
        (Text::MenuOpen, "open"),
        (Text::MenuPause, "pause"),
        (Text::MenuQuit, "quit"),
        (Text::MenuStop, "stop"),
    ];

    fn relabel(&self, language: &Language) {
        for (item, (text, _)) in self.items.iter().zip(Self::TEXTS) {
            let _ = item.set_text(language.text(text));
        }
    }
}

/// The base icon and the two with a dot, made once from the default window
/// icon so a heartbeat never redraws one.
#[derive(Debug)]
struct TrayIcons {
    none: Image<'static>,
    green: Image<'static>,
    red: Image<'static>,
}

impl TrayIcons {
    fn new(base: &Image<'_>) -> Self {
        let (width, height) = (base.width(), base.height());
        let variant = |dot| {
            Image::new_owned(
                dot::with_dot(base.rgba(), width, height, dot),
                width,
                height,
            )
        };
        Self {
            none: Image::new_owned(base.rgba().to_vec(), width, height),
            green: variant(Dot::Green),
            red: variant(Dot::Red),
        }
    }

    fn get(&self, dot: Dot) -> &Image<'static> {
        match dot {
            Dot::None => &self.none,
            Dot::Green => &self.green,
            Dot::Red => &self.red,
        }
    }
}

/// Contract parameters this build speaks, so the frontend can detect a
/// mismatched bundle.
#[tauri::command]
fn protocol_info() -> ProtocolInfo {
    ProtocolInfo::CURRENT
}

/// The version of this desktop binary, for the about tab and exports: the
/// workspace version, which the watchdog reports for itself in every
/// snapshot (`HostStatus::watchdog_version`). Takes no input.
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// The MCP server installed beside this window, for the about tab and the
/// MCP settings (owner request of 2026-11-04): the version its executable
/// carries, read without running it (see `mcp`). Takes no input; the path
/// is fixed.
#[tauri::command]
async fn mcp_version() -> mcp::McpServer {
    tauri::async_runtime::spawn_blocking(mcp::installed)
        .await
        .unwrap_or(mcp::McpServer::Unreadable)
}

/// The user's folders the MCP clients' settings are found from.
fn mcp_client_folders(app: &AppHandle) -> Option<mcpclients::Folders> {
    Some(mcpclients::Folders {
        home: app.path().home_dir().ok()?,
        config: app.path().config_dir().ok()?,
        codex_home: mcpclients::Folders::absolute(std::env::var_os("CODEX_HOME")),
        claude_config: mcpclients::Folders::absolute(std::env::var_os("CLAUDE_CONFIG_DIR")),
    })
}

/// Each AI tool the window can add the MCP server to, with whether its
/// settings already have it (owner request of 2026-11-12; see
/// `mcpclients`). Takes no input; the places are fixed.
#[tauri::command]
async fn mcp_clients(app: AppHandle) -> Vec<mcpclients::ClientView> {
    let Some(folders) = mcp_client_folders(&app) else {
        return Vec::new();
    };
    tauri::async_runtime::spawn_blocking(move || {
        let fallback = mcp::beside_window().unwrap_or_default();
        mcpclients::all(&folders, mcp::installed_path().as_deref(), &fallback)
    })
    .await
    .unwrap_or_default()
}

/// Adds the MCP server beside this window to one tool's settings, after
/// the user confirmed it in the window. Takes only the tool, one of a
/// closed set; the file and the command are fixed (see `mcpclients`).
#[tauri::command]
async fn set_up_mcp_client(
    app: AppHandle,
    client: mcpclients::Client,
) -> Result<mcpclients::ClientView, mcpclients::NotSetUp> {
    let folders = mcp_client_folders(&app).ok_or(mcpclients::NotSetUp::NoFolder)?;
    tauri::async_runtime::spawn_blocking(move || {
        mcpclients::set_up(client, &folders, mcp::installed_path().as_deref())
    })
    .await
    .map_err(|_| mcpclients::NotSetUp::Io)?
}

/// Removes the MCP server's entry from one tool's settings, after the user
/// confirmed it in the window. Takes only the tool, one of a closed set;
/// the file is fixed (see `mcpclients`). Works without an installed server.
#[tauri::command]
async fn remove_mcp_client(
    app: AppHandle,
    client: mcpclients::Client,
) -> Result<mcpclients::ClientView, mcpclients::NotSetUp> {
    let folders = mcp_client_folders(&app).ok_or(mcpclients::NotSetUp::NoFolder)?;
    tauri::async_runtime::spawn_blocking(move || {
        let shown = mcp::installed_path()
            .or_else(mcp::beside_window)
            .unwrap_or_default();
        mcpclients::remove(client, &folders, &shown)
    })
    .await
    .map_err(|_| mcpclients::NotSetUp::Io)?
}

/// The languages the system prefers, most preferred first, for the
/// window's "follow the system" choice (owner request of 2026-11-05): plain
/// tags only, at most a few (see `lang::system_languages`); the window
/// picks its language from them. Takes no input.
#[tauri::command]
fn system_languages() -> Vec<String> {
    lang::system_languages(sys_locale::get_locales())
}

/// The shell's own refusal of a request: the `code` and `reason` the window
/// words from its language file, and an English `message` it shows only
/// among the details.
fn refuse(
    request_id: Option<RequestId>,
    code: ErrorCode,
    reason: Option<ErrorReason>,
    message: &str,
) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        outcome: Outcome::Err(ProtocolError::new(code, reason, message)),
    }
}

/// What the webview is told when there is no watchdog to talk to. On a
/// snapshot the GUI then shows the watchdog as lost.
const NOT_RUNNING: &str = "the background watchdog service is not installed or not running";

/// The one way the webview reaches the watchdog: a `Request` from the closed
/// contract. Tauri's deserialization already enforces every type (IDs, text,
/// targets, the request allowlist, no unknown fields); the codec then checks
/// the version and the message size before anything is sent.
///
/// On Windows the request goes to the watchdog's named pipe (see [`ipc`]),
/// whose server checks the caller's OS identity itself. That I/O blocks, and
/// a plain sync command runs on the main thread and would freeze the window
/// meanwhile (tauri-macros 2.7.4, the `#[tauri::command]` docs: `async`
/// "runs the command on the async runtime instead of blocking the main
/// thread"). So this is an `async` command, and the pipe I/O runs on the
/// runtime's blocking pool rather than on an async worker. The answer is the
/// watchdog's, checked to belong to this request, or an error; never a state
/// the GUI made up.
///
/// macOS and Ubuntu get their sockets later. Until then every request there
/// is answered with an error, and the GUI shows the watchdog as lost.
#[tauri::command]
async fn watchdog_request(envelope: RequestEnvelope) -> ResponseEnvelope {
    let request_id = envelope.request_id.clone();
    match outgoing(envelope) {
        Ok(frame) => forward(request_id, frame).await,
        Err((code, message)) => refuse(Some(request_id), code, None, message),
    }
}

/// Checks the version and the size and encodes the request, or says why it
/// is refused: with a code that says it all, so no reason. The envelope is
/// not kept once encoded.
fn outgoing(envelope: RequestEnvelope) -> Result<Vec<u8>, (ErrorCode, &'static str)> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err((
            ErrorCode::UnsupportedVersion,
            "unsupported protocol version",
        ));
    }
    codec::encode_request(&envelope)
        .map_err(|_| (ErrorCode::MessageTooLarge, "request is too large"))
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
async fn forward(request_id: RequestId, frame: Vec<u8>) -> ResponseEnvelope {
    #[cfg(windows)]
    let open = ipc::open_pipe;
    #[cfg(not(windows))]
    let open = ipc::open_socket;
    let late = request_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ipc::to_watchdog(open, frame, request_id, &ipc::IN_FLIGHT)
    })
    .await
    .unwrap_or_else(|_| join_failed(late))
}

/// The answer when the exchange's task ended without one: the request may
/// have reached the watchdog, so its answer is lost, not unsent.
#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
fn join_failed(request_id: RequestId) -> ResponseEnvelope {
    refuse(
        Some(request_id),
        ErrorCode::Internal,
        Some(ErrorReason::AnswerLost),
        "the request to the background watchdog stopped unexpectedly",
    )
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
async fn forward(request_id: RequestId, _frame: Vec<u8>) -> ResponseEnvelope {
    refuse(
        Some(request_id),
        ErrorCode::Internal,
        Some(ErrorReason::NotRunning),
        NOT_RUNNING,
    )
}

/// Saves a log export the window built (see [`export`]). The webview passes
/// only the text and the UTC offset for the file name's local time, both
/// checked; the folder (Downloads, else Documents) and the name are chosen
/// here, and the file is created as new. The answer is which folder and
/// which name, or why not, as codes the window words itself.
#[tauri::command]
async fn save_log_export(
    app: AppHandle,
    text: String,
    utc_offset_minutes: i32,
) -> Result<export::Saved, export::NotSaved> {
    export::check(&text, utc_offset_minutes)?;
    let (dir, folder) = match app.path().download_dir() {
        Ok(dir) => (dir, export::Folder::Downloads),
        Err(_) => (
            app.path()
                .document_dir()
                .map_err(|_| export::NotSaved::from(export::Reason::NoFolder))?,
            export::Folder::Documents,
        ),
    };
    let stamp = export::stamp(std::time::SystemTime::now(), utc_offset_minutes);
    let name = tauri::async_runtime::spawn_blocking(move || {
        export::write_new(&dir, &stamp, text.as_bytes())
    })
    .await
    .map_err(|_| export::NotSaved::from(export::Reason::Interrupted))??;
    Ok(export::Saved { folder, name })
}

/// Saves a settings file (owner decision of 2026-11-04). The webview passes
/// the watchdog's settings document, checked again here by its type, the
/// device name, which only goes into the file's name (as
/// [`export::file_name_part`] makes it), and the UTC offset for the name's
/// local time; folder, name and creation are as for [`save_log_export`].
#[tauri::command]
async fn save_settings_export(
    app: AppHandle,
    document: SettingsDocument,
    device_name: DisplayName,
    utc_offset_minutes: i32,
) -> Result<export::Saved, export::NotSaved> {
    let text = serde_json::to_string_pretty(&document)
        .map_err(|_| export::NotSaved::from(export::Reason::Interrupted))?;
    export::check(&text, utc_offset_minutes)?;
    let (dir, folder) = match app.path().download_dir() {
        Ok(dir) => (dir, export::Folder::Downloads),
        Err(_) => (
            app.path()
                .document_dir()
                .map_err(|_| export::NotSaved::from(export::Reason::NoFolder))?,
            export::Folder::Documents,
        ),
    };
    let stamp = export::stamp(std::time::SystemTime::now(), utc_offset_minutes);
    let name = tauri::async_runtime::spawn_blocking(move || {
        export::write_settings(&dir, device_name.as_str(), &stamp, text.as_bytes())
    })
    .await
    .map_err(|_| export::NotSaved::from(export::Reason::Interrupted))??;
    Ok(export::Saved { folder, name })
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Level {
    Lost,
    Fault,
    /// A green light whose evidence has run out: not known to be fine.
    Expired,
    Host,
    Paused,
    Ok,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Monitor {
    Ok,
    Paused,
    Lost,
    StorageFailed,
}

/// What the tray shows, computed by the frontend from the latest snapshot
/// (`trayCommand` in `view.ts`). Counts and fixed choices only: no free text
/// crosses into the shell, so the tooltip stays the same short size with 1
/// tunnel or 16 (plan §8). `dot` picks one of the three prepared icons;
/// `refresh_secs` is how often the window will send the next one; and
/// `green_for_ms` how long a green dot, and with it the summary, holds
/// without one.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraySummary {
    level: Level,
    vpn_faults: u8,
    host_issues: u8,
    monitor: Monitor,
    dot: Dot,
    refresh_secs: RefreshSecs,
    green_for_ms: GreenForMs,
}

/// The tooltip for `summary` in `language`: counts and fixed words only.
fn tooltip(summary: TraySummary, language: &Language) -> String {
    let monitor = language.text(match summary.monitor {
        Monitor::Ok => Text::MonitorOk,
        Monitor::Paused => Text::MonitorPaused,
        Monitor::Lost => Text::MonitorLost,
        Monitor::StorageFailed => Text::MonitorStorageFailed,
    });
    let level = language.text(match (summary.level, summary.monitor) {
        (Level::Lost, _) => Text::LevelLost,
        (Level::Fault, _) => Text::LevelFault,
        (Level::Expired, _) => Text::LevelExpired,
        (Level::Host, _) => Text::LevelHost,
        (Level::Paused, _) => Text::LevelPaused,
        // Nothing is down, but the watchdog can no longer save its state, so
        // it has stopped automatic recovery: not "normal" (plan §9.1).
        (Level::Ok, Monitor::StorageFailed) => Text::LevelAttention,
        (Level::Ok, _) => Text::LevelOk,
    });
    if matches!(summary.level, Level::Lost) {
        lang::fill(
            language.text(Text::TooltipLost),
            &[("level", level), ("monitor", monitor)],
        )
    } else {
        // A host target that is down is counted on its own and never shown
        // as a VPN fault (plan §8).
        lang::fill(
            language.text(Text::Tooltip),
            &[
                ("level", level),
                ("vpn_faults", &summary.vpn_faults.to_string()),
                ("host_issues", &summary.host_issues.to_string()),
                ("monitor", monitor),
            ],
        )
    }
}

/// Updates the tray tooltip and dot, in the language the window shows
/// (`language`, its tag: looked up in the compiled list, an unknown one
/// gets Traditional Chinese). Called with every snapshot the window renders
/// (every refresh interval), which also tells [`watch_tray`] the summary is
/// current, how long it stays so and until when its green dot holds. A
/// plain command, so it runs on the main thread, as the watcher's changes
/// do: the two never cross.
#[tauri::command]
fn update_tray(
    app: AppHandle,
    state: tauri::State<'_, TrayState>,
    summary: TraySummary,
    language: String,
) {
    let language = lang::find(&language);
    if let Ok(mut clock) = state.clock.lock() {
        clock.summary(&summary, Instant::now());
    }
    if let Ok(mut current) = state.language.lock()
        && current.tag != language.tag
    {
        *current = language;
        if let Some(menu) = state.menu.get() {
            menu.relabel(language);
        }
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip(summary, language)));
    }
    show_dot(&app, &state, summary.dot);
}

/// Shows `dot` on the tray icon, only when it changed; whether it is shown
/// now. Called on the main thread only, where the tray runs the change at
/// once, so the lock is never held while waiting for another thread.
fn show_dot(app: &AppHandle, state: &TrayState, dot: Dot) -> bool {
    let Ok(mut shown) = state.shown.lock() else {
        return false;
    };
    if *shown == dot {
        return true;
    }
    if let (Some(icons), Some(tray)) = (state.icons.get(), app.tray_by_id(TRAY_ID))
        && tray.set_icon(Some(icons.get(dot).clone())).is_ok()
    {
        *shown = dot;
        return true;
    }
    false
}

/// Replaces an old tray summary with "unknown" and drops the dot, on the
/// shell's own clock ([`TrayClock`]), so the tray never keeps saying
/// "normal" or showing a green dot when nothing refreshes it, or once the
/// evidence behind the green dot has run out, whatever the window's timers
/// do. The thread only reads the clock; the change itself runs on the main
/// thread, as [`update_tray`] does, so a summary that arrives meanwhile is
/// never covered with "unknown". The words are the last language the window
/// said it shows.
fn watch_tray(app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(TRAY_WATCH_EVERY);
            let due = app
                .state::<TrayState>()
                .clock
                .lock()
                .is_ok_and(|clock| clock.due(Instant::now()));
            if due {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || show_stale(&handle));
            }
        }
    });
}

/// Says "unknown" on the tray and drops the dot, if the summary is still
/// due for it (a newer one may have come since the watcher looked). Once
/// both are shown, not again until the next summary.
fn show_stale(app: &AppHandle) {
    let state = app.state::<TrayState>();
    if !state
        .clock
        .lock()
        .is_ok_and(|clock| clock.due(Instant::now()))
    {
        return;
    }
    let language = state
        .language
        .lock()
        .map(|l| *l)
        .unwrap_or_else(|_| lang::find(lang::FALLBACK_TAG));
    let said = app.tray_by_id(TRAY_ID).is_none_or(|tray| {
        tray.set_tooltip(Some(language.text(Text::TooltipStale)))
            .is_ok()
    });
    if show_dot(app, &state, Dot::None)
        && said
        && let Ok(mut clock) = state.clock.lock()
    {
        clock.shown_stale();
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The tray or menu bar item: open the window, pause or resume monitoring,
/// quit the interface. Stopping the background watchdog is listed apart and
/// only explained: the GUI has no power over the service (plan §8). On Linux
/// the menu opens with a right click; a desktop launcher opens the window when
/// there is no tray (plan §8, [S12]).
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    // In English until the window says which language it shows.
    let language = lang::find(lang::FALLBACK_TAG);
    let item = |(text, id): (Text, &str)| {
        MenuItem::with_id(app, id, language.text(text), true, None::<&str>)
    };
    let [open, pause, quit, stop] = TrayMenu::TEXTS;
    let items = [item(open)?, item(pause)?, item(quit)?, item(stop)?];
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&items[0], &items[1], &separator, &items[2], &items[3]],
    )?;
    let _ = app.state::<TrayState>().menu.set(TrayMenu { items });
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("WG Always Live")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_window(app),
            "pause" => {
                let _ = app.emit("wgal://toggle-pause", ());
            }
            "quit" => app.exit(0),
            "stop" => {
                show_window(app);
                let _ = app.emit("wgal://explain-stop", ());
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
        let _ = app.state::<TrayState>().icons.set(TrayIcons::new(icon));
    }
    builder.build(app)?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .manage(TrayState {
            clock: Mutex::new(TrayClock::new(Instant::now())),
            available: Mutex::new(false),
            icons: OnceLock::new(),
            shown: Mutex::new(Dot::None),
            language: Mutex::new(lang::find(lang::FALLBACK_TAG)),
            menu: OnceLock::new(),
        })
        .setup(|app| {
            // Without a tray (some Linux desktops) the window and the
            // launcher still work.
            let available = build_tray(app).is_ok();
            if let Ok(mut flag) = app.state::<TrayState>().available.lock() {
                *flag = available;
            }
            watch_tray(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // With a tray, closing the window hides it and the tray brings
                // it back. Without one nothing could, so closing quits the
                // interface instead of leaving a hidden process behind.
                // Monitoring is not affected either way.
                let tray = window
                    .state::<TrayState>()
                    .available
                    .lock()
                    .map(|a| *a)
                    .unwrap_or(false);
                if tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            protocol_info,
            app_version,
            watchdog_request,
            update_tray,
            save_log_export,
            save_settings_export,
            mcp_version,
            mcp_clients,
            set_up_mcp_client,
            remove_mcp_client,
            system_languages
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the WGAL desktop shell");
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgal_protocol::{Request, RequestId};

    fn envelope(request: Request) -> RequestEnvelope {
        RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("r1").unwrap(),
            request,
        }
    }

    /// Without a watchdog endpoint (a platform other than Windows, Linux and
    /// macOS) every request is answered with its ID and an error. Elsewhere
    /// the same answer for a missing endpoint is tested in `ipc` against
    /// fakes, never the real pipe or socket.
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    #[test]
    fn requests_are_answered_with_their_id_and_never_as_a_state() {
        let response =
            tauri::async_runtime::block_on(watchdog_request(envelope(Request::GetSnapshot)));
        assert_eq!(response.request_id, Some(RequestId::new("r1").unwrap()));
        assert!(matches!(response.outcome, Outcome::Err(_)));
    }

    /// Refused before anything is encoded or sent, so the command never
    /// reaches a pipe here on any platform.
    #[test]
    fn a_wrong_version_is_refused_before_anything_is_sent() {
        let mut e = envelope(Request::GetSnapshot);
        e.protocol_version = PROTOCOL_VERSION + 1;
        assert_eq!(
            outgoing(e.clone()).unwrap_err().0,
            ErrorCode::UnsupportedVersion
        );
        let response = tauri::async_runtime::block_on(watchdog_request(e));
        assert_eq!(response.request_id, Some(RequestId::new("r1").unwrap()));
        assert!(matches!(
            response.outcome,
            Outcome::Err(ProtocolError {
                code: ErrorCode::UnsupportedVersion,
                ..
            })
        ));
    }

    /// The shell's own refusals carry the reason the window words them by,
    /// or none where the code says it all.
    #[test]
    fn the_shells_refusals_carry_their_reasons() {
        let reason_of = |response: ResponseEnvelope| match response.outcome {
            Outcome::Err(error) => (error.code, error.reason),
            Outcome::Ok(_) => panic!("a refusal never reads as an answer"),
        };
        let mut wrong = envelope(Request::GetSnapshot);
        wrong.protocol_version = PROTOCOL_VERSION + 1;
        assert_eq!(
            reason_of(tauri::async_runtime::block_on(watchdog_request(wrong))),
            (ErrorCode::UnsupportedVersion, None)
        );
        assert_eq!(
            reason_of(join_failed(RequestId::new("r1").unwrap())),
            (ErrorCode::Internal, Some(ErrorReason::AnswerLost))
        );
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        assert_eq!(
            reason_of(tauri::async_runtime::block_on(watchdog_request(envelope(
                Request::GetSnapshot
            )))),
            (ErrorCode::Internal, Some(ErrorReason::NotRunning))
        );
    }

    #[test]
    fn a_valid_request_is_sent_as_the_codec_encodes_it() {
        let e = envelope(Request::GetSnapshot);
        let encoded = codec::encode_request(&e).unwrap();
        assert_eq!(outgoing(e).unwrap(), encoded);
    }

    #[test]
    fn an_old_tray_summary_turns_unknown_after_15_seconds() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let mut clock = TrayClock::new(start);
        assert!(!clock.stale(at(15)));
        assert!(clock.stale(at(16)));
        clock.summary(&summary_with(Dot::None, 5, 0), at(100));
        assert!(!clock.stale(at(110)));
        assert!(clock.stale(at(116)));
    }

    #[test]
    fn a_longer_interval_gives_three_intervals_before_unknown() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        // max(15 s, 3 intervals), as the window's lost-link overlay.
        assert_eq!(stale_after(None), Duration::from_secs(15));
        assert_eq!(stale_after(Some(RefreshSecs(3))), Duration::from_secs(15));
        assert_eq!(stale_after(Some(RefreshSecs(5))), Duration::from_secs(15));
        assert_eq!(stale_after(Some(RefreshSecs(6))), Duration::from_secs(18));
        assert_eq!(
            stale_after(Some(RefreshSecs(300))),
            Duration::from_secs(900)
        );
        let mut clock = TrayClock::new(start);
        clock.summary(&summary_with(Dot::Red, 60, 0), at(100));
        assert!(!clock.stale(at(280)));
        assert!(clock.stale(at(281)));
    }

    /// A summary with `dot`, from a window that refreshes every `refresh`
    /// seconds, whose green dot has `green_for` ms of evidence left.
    fn summary_with(dot: Dot, refresh: u16, green_for: u32) -> TraySummary {
        TraySummary {
            level: Level::Ok,
            vpn_faults: 0,
            host_issues: 0,
            monitor: Monitor::Ok,
            dot,
            refresh_secs: RefreshSecs(refresh),
            green_for_ms: GreenForMs(green_for),
        }
    }

    /// Codex re-review P1-2: the window refreshes every 300 s, so its summary
    /// stays current for 900 s, but the evidence behind its green dot has
    /// 34 s left. With no tick from the window at all, the shell drops the
    /// dot once those 34 s have passed on its own clock; a new summary
    /// renews it from when it arrives.
    #[test]
    fn a_green_dot_runs_out_on_the_shells_own_clock() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let green = summary_with(Dot::Green, 300, 34_000);
        let mut clock = TrayClock::new(start);
        clock.summary(&green, at(0));
        assert!(!clock.stale(at(34_000)));
        assert!(clock.stale(at(34_001)));
        // Long before the 900 s the summary itself would count.
        assert!(clock.stale(at(35_000)));
        clock.summary(&green, at(40_000));
        assert!(!clock.stale(at(74_000)));
        assert!(clock.stale(at(74_001)));
        // Without a green dot only the summary's age counts, whatever
        // deadline came with it.
        clock.summary(&summary_with(Dot::Red, 300, 0), at(100_000));
        assert!(!clock.stale(at(1_000_000)));
        assert!(clock.stale(at(1_000_001)));
        clock.summary(&summary_with(Dot::None, 300, 3_700_000), at(2_000_000));
        assert!(!clock.stale(at(2_900_000)));
        // A green whose evidence ran out as it was sent is gone at once.
        clock.summary(&summary_with(Dot::Green, 5, 0), at(3_000_000));
        assert!(!clock.stale(at(3_000_000)));
        assert!(clock.stale(at(3_000_001)));
    }

    /// The watcher shows "unknown" once for each summary that went stale,
    /// and never for one that came after it.
    #[test]
    fn the_watcher_shows_unknown_once_and_never_over_a_newer_summary() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut clock = TrayClock::new(start);
        // Before the first summary: 15 s.
        assert!(!clock.due(at(15_000)));
        assert!(clock.due(at(15_001)));
        clock.summary(&summary_with(Dot::Green, 300, 34_000), at(20_000));
        assert!(!clock.due(at(54_000)));
        assert!(clock.due(at(54_001)));
        clock.shown_stale();
        assert!(clock.stale(at(55_000)));
        assert!(!clock.due(at(55_000)));
        // A newer summary is current again, and so is what it says.
        clock.summary(&summary_with(Dot::Green, 300, 34_000), at(56_000));
        assert!(!clock.due(at(56_000)));
        assert!(!clock.due(at(90_000)));
        assert!(clock.due(at(90_001)));
    }

    #[test]
    fn the_green_dots_deadline_is_0_to_3700_seconds_and_required() {
        let summary = |ms: serde_json::Value| {
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "dot": "green",
                "refresh_secs": 300, "green_for_ms": ms,
            }))
        };
        for (ms, want) in [(0, 0), (34_000, 34_000), (3_700_000, 3_700_000)] {
            assert_eq!(
                summary(serde_json::json!(ms)).unwrap().green_for_ms,
                GreenForMs(want)
            );
        }
        for bad in [
            serde_json::json!(3_700_001),
            serde_json::json!(-1),
            serde_json::json!(5.5),
            serde_json::json!("5"),
            serde_json::json!(null),
        ] {
            assert!(summary(bad.clone()).is_err(), "{bad}");
        }
        assert!(
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "dot": "green",
                "refresh_secs": 300,
            }))
            .is_err()
        );
    }

    /// The window counts a green past its evidence as expired, never as
    /// fine, and the tooltip says so.
    #[test]
    fn an_expired_green_is_not_normal_in_the_tooltip() {
        let summary: TraySummary = serde_json::from_value(serde_json::json!({
            "level": "expired", "vpn_faults": 0, "host_issues": 1, "monitor": "ok", "dot": "none",
            "refresh_secs": 300, "green_for_ms": 0,
        }))
        .unwrap();
        let zh = lang::find("zh-TW");
        assert_eq!(
            tooltip(summary, zh),
            "WG Always Live：結果已過期\nVPN 異常 0，主機目標異常 1\n背景監控正常"
        );
        assert!(!tooltip(summary, zh).contains("：正常"));
    }

    #[test]
    fn the_refresh_interval_is_3_to_300_and_required() {
        let summary = |secs: serde_json::Value| {
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "dot": "none",
                "refresh_secs": secs, "green_for_ms": 0,
            }))
        };
        assert_eq!(
            summary(serde_json::json!(3)).unwrap().refresh_secs,
            RefreshSecs(3)
        );
        assert_eq!(
            summary(serde_json::json!(300)).unwrap().refresh_secs,
            RefreshSecs(300)
        );
        for bad in [
            serde_json::json!(2),
            serde_json::json!(301),
            serde_json::json!(0),
            serde_json::json!(-5),
            serde_json::json!(5.5),
            serde_json::json!("5"),
            serde_json::json!(null),
        ] {
            assert!(summary(bad.clone()).is_err(), "{bad}");
        }
        assert!(
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "dot": "none",
                "green_for_ms": 0,
            }))
            .is_err()
        );
    }

    #[test]
    fn the_app_version_is_this_binarys() {
        assert_eq!(app_version(), env!("CARGO_PKG_VERSION"));
        assert!(wgal_protocol::ProductVersion::new(app_version()).is_ok());
    }

    #[test]
    fn the_webview_cannot_smuggle_text_into_the_tray() {
        let json = serde_json::json!({
            "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "refresh_secs": 5, "dot": "none", "green_for_ms": 0,
            "tooltip": "anything",
        });
        assert!(serde_json::from_value::<TraySummary>(json).is_err());
        let summary: TraySummary = serde_json::from_value(serde_json::json!({
            "level": "host", "vpn_faults": 0, "host_issues": 2, "monitor": "ok", "refresh_secs": 5, "dot": "green", "green_for_ms": 0,
        }))
        .unwrap();
        assert_eq!(summary.dot, Dot::Green);
        let text = tooltip(summary, lang::find("zh-TW"));
        assert!(text.contains("VPN 異常 0"));
        assert!(text.contains("主機目標異常 2"));
    }

    /// The window names its language by tag only; the texts are the
    /// shell's own, and an unknown tag is the reference language.
    #[test]
    fn the_tray_speaks_the_windows_language_or_the_reference() {
        let summary = TraySummary {
            level: Level::Fault,
            vpn_faults: 1,
            host_issues: 0,
            monitor: Monitor::Ok,
            dot: Dot::Red,
            refresh_secs: RefreshSecs(5),
            green_for_ms: GreenForMs(0),
        };
        let reference = tooltip(summary, lang::find(lang::FALLBACK_TAG));
        assert_eq!(tooltip(summary, lang::find("xx-YY")), reference);
        assert_eq!(
            tooltip(summary, lang::find("WG Always Live：正常")),
            reference
        );
        for language in lang::languages() {
            let text = tooltip(summary, language);
            assert!(!text.contains('{'), "{}: {text}", language.tag);
            assert!(text.contains('1'), "{}: {text}", language.tag);
        }
    }

    /// Windows keeps a tray tooltip in `NOTIFYICONDATAW::szTip`, 128 UTF-16
    /// units with the terminating NUL; tray-icon 0.25 copies at most that
    /// many and cuts the rest.
    const TOOLTIP_MAX_UTF16: usize = 127;

    const LEVELS: [Level; 6] = [
        Level::Lost,
        Level::Fault,
        Level::Expired,
        Level::Host,
        Level::Paused,
        Level::Ok,
    ];
    const MONITORS: [Monitor; 4] = [
        Monitor::Ok,
        Monitor::Paused,
        Monitor::Lost,
        Monitor::StorageFailed,
    ];

    /// 16 tunnels give at most 16 VPN faults and 64 host problems; the
    /// window clamps both to 255. Every tooltip in every language fits
    /// Windows' limit whole, so none is ever cut, and the stale one does
    /// too.
    #[test]
    fn every_tooltip_fits_the_windows_tray_whole() {
        for language in lang::languages() {
            let mut longest = language.text(Text::TooltipStale).encode_utf16().count();
            for level in LEVELS {
                for monitor in MONITORS {
                    for vpn_faults in [0, 16, 255] {
                        for host_issues in [0, 64, 255] {
                            let text = tooltip(
                                TraySummary {
                                    level,
                                    vpn_faults,
                                    host_issues,
                                    monitor,
                                    dot: Dot::None,
                                    refresh_secs: RefreshSecs(5),
                                    green_for_ms: GreenForMs(0),
                                },
                                language,
                            );
                            longest = longest.max(text.encode_utf16().count());
                        }
                    }
                }
            }
            assert!(
                longest <= TOOLTIP_MAX_UTF16,
                "{}: {longest} UTF-16 units",
                language.tag
            );
        }
    }

    /// The menu's items are worded from the language files too, and every
    /// language has a text for each.
    #[test]
    fn every_menu_item_has_words_in_every_language() {
        for language in lang::languages() {
            for (text, _) in TrayMenu::TEXTS {
                assert!(!language.text(text).trim().is_empty(), "{}", language.tag);
            }
        }
        assert_eq!(
            lang::find("zh-TW").text(Text::MenuQuit),
            "退出介面（背景監控繼續）"
        );
    }

    #[test]
    fn sixteen_tunnels_read_as_counts_with_the_watchdog_status() {
        let summary: TraySummary = serde_json::from_value(serde_json::json!({
            "level": "fault", "vpn_faults": 16, "host_issues": 64, "monitor": "ok", "refresh_secs": 5, "dot": "red", "green_for_ms": 0,
        }))
        .unwrap();
        let zh = lang::find("zh-TW");
        assert_eq!(
            tooltip(summary, zh),
            "WG Always Live：VPN 異常\nVPN 異常 16，主機目標異常 64\n背景監控正常"
        );
        let storage = TraySummary {
            level: Level::Ok,
            vpn_faults: 0,
            host_issues: 0,
            monitor: Monitor::StorageFailed,
            dot: Dot::None,
            refresh_secs: RefreshSecs(5),
            green_for_ms: GreenForMs(0),
        };
        assert!(tooltip(storage, zh).starts_with("WG Always Live：需要注意\n"));
        assert!(!tooltip(storage, zh).contains("正常\n"));
        assert_eq!(
            tooltip(
                TraySummary {
                    level: Level::Lost,
                    monitor: Monitor::Lost,
                    ..storage
                },
                zh
            ),
            "WG Always Live：狀態未知（背景監控失聯）"
        );
        // A count the window could not clamp is refused, not wrapped.
        assert!(
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "fault", "vpn_faults": 256, "host_issues": 0, "monitor": "ok", "refresh_secs": 5, "dot": "red", "green_for_ms": 0,
            }))
            .is_err()
        );
    }

    #[test]
    fn the_dot_is_one_of_three_and_required() {
        for (text, dot) in [
            ("none", Dot::None),
            ("green", Dot::Green),
            ("red", Dot::Red),
        ] {
            let summary: TraySummary = serde_json::from_value(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "refresh_secs": 5, "dot": text,
                "green_for_ms": 0,
            }))
            .unwrap();
            assert_eq!(summary.dot, dot);
        }
        for bad in [
            serde_json::json!("yellow"),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<TraySummary>(serde_json::json!({
                    "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "refresh_secs": 5, "dot": bad,
                    "green_for_ms": 0,
                }))
                .is_err()
            );
        }
        // A window that sends no dot is refused rather than guessed.
        assert!(
            serde_json::from_value::<TraySummary>(serde_json::json!({
                "level": "ok", "vpn_faults": 0, "host_issues": 0, "monitor": "ok", "refresh_secs": 5,
                "green_for_ms": 0,
            }))
            .is_err()
        );
    }

    #[test]
    fn the_three_tray_icons_are_made_from_the_base_icon() {
        let base = Image::new_owned([0, 0, 0, 255].repeat(32 * 32), 32, 32);
        let icons = TrayIcons::new(&base);
        assert_eq!(icons.get(Dot::None).rgba(), base.rgba());
        for (dot, fill) in [
            (Dot::Green, [0x3f, 0xb9, 0x50]),
            (Dot::Red, [0xf8, 0x51, 0x49]),
        ] {
            let icon = icons.get(dot);
            assert_eq!((icon.width(), icon.height()), (32, 32));
            // The dot's center, at the top right.
            let i = (8 * 32 + 24) * 4;
            assert_eq!(&icon.rgba()[i..i + 3], &fill);
            // The bottom left is the base icon.
            let j = (28 * 32 + 4) * 4;
            assert_eq!(&icon.rgba()[j..j + 4], &[0, 0, 0, 255]);
        }
    }
}
