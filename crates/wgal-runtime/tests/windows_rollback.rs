//! Rolling a Windows installation back to an older build the way
//! `packaging/windows/README.zh-TW.md` (「回退到較舊的版本」) tells the owner
//! to (Codex review of wave three, P2-9), through real files.
//!
//! The upgrade's backup holds the state as the older build left it. The
//! newer build has since sent the incident's alert and recorded the user's
//! disconnect. Loaded as an ordinary state file, the backup would be trusted:
//! the alert, still `pending` there, could be sent again, and automatic
//! actions would act on intents the user has since changed. The rollback
//! must make the older build load it as [`Startup::RestoredFromBackup`],
//! which turns automatic recovery and "start when stopped externally" off,
//! drops unfinished operations and makes every unsettled alert `unknown`.
//!
//! The store gives that only for an unreadable `state.json` beside a valid
//! `state.prev.json`; a missing `state.json` is a save interrupted between
//! its renames, and the previous file is trusted. So the README puts the
//! backup's state in place as `state.prev.json` and writes a marker that is
//! not JSON over `state.json`. This test reads the marker from the README,
//! and checks that what the README says the older build shows for a tunnel
//! the user disconnected after the upgrade is the light and reason it does
//! show, in the window's words (Codex re-review P3-N2).
//!
//! The store and the watchdog's handling of a restore from the backup are
//! the same in this build as in the older builds installed on the soak
//! machine (`00ad525`, `7339e8e`): `crates/wgal-runtime/src/store.rs` and
//! `distrust` in `crates/wgal-core/src/watchdog.rs` did not change since.
//! So this build stands in for the older one. The README's PowerShell itself
//! is checked separately against a temporary folder; this test pins the file
//! layout it produces. Simulation with the fake platform: no service, no
//! tunnel, no ProgramData.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use wgal_core::{
    IncidentRecord, PersistedState, Startup, StateStore, Timing, TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterCapabilities, Capability, DesiredState, FirstRecovery, IncidentId, Light, LightReason,
    NotificationState, OsState, Request, TunnelId,
};
use wgal_runtime::store::{FileStore, PREVIOUS_FILE, STATE_FILE, Source};
use wgal_testkit::{Call, FakeClock, FakePlatform};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("wgal-rollback-{}-{name}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The README whose rollback this test follows.
const README: &str = include_str!("../../../packaging/windows/README.zh-TW.md");

/// The window's Traditional Chinese language file, whose words for the
/// lights and their reasons the README quotes. The older builds the soak
/// machine had (`00ad525`, `7339e8e`) word the gray light and
/// `not_connected` the same.
const WINDOW_TEXTS: &str = include_str!("../../../apps/desktop/src/lang/zh-TW.json");

/// The window's words for `value` in `section` of its language file, `value`
/// given as the watchdog sends it.
fn window_text(section: &str, value: impl serde::Serialize) -> String {
    let texts: serde_json::Value = serde_json::from_str(WINDOW_TEXTS).unwrap();
    let key = serde_json::to_value(value).unwrap();
    texts[section][key.as_str().unwrap()]
        .as_str()
        .unwrap_or_else(|| panic!("no {section}.{key} in zh-TW.json"))
        .to_owned()
}

/// Step 6's line about a tunnel the user disconnected after the upgrade.
fn readme_on_disconnected_after_upgrade() -> &'static str {
    README
        .lines()
        .find(|line| line.contains("升級後才由你斷線"))
        .expect(
            "the README's rollback says what happens to a tunnel disconnected after the upgrade",
        )
}

/// What the rollback restores, relative to the data folder: the files
/// install.ps1's `Backup-State` copies (`$StateFiles`).
const STATE_FILES: [&str; 3] = [
    "state/state.json",
    "state/state.prev.json",
    "notify/notify.json",
];

fn office() -> TunnelBinding {
    TunnelBinding::new("office").unwrap()
}

fn platform(os_state: OsState) -> FakePlatform {
    let platform = FakePlatform::new();
    platform.set_capabilities(AdapterCapabilities {
        start_stop: Capability::Supported,
        bound_icmp_probe: Capability::Supported,
        ..AdapterCapabilities::UNKNOWN
    });
    platform.add_tunnel(office(), "Office", os_state);
    platform
}

fn watchdog(state_dir: &Path, os_state: OsState) -> Watchdog<FakePlatform, FakeClock> {
    let (store, startup, _) = FileStore::open(state_dir).unwrap();
    Watchdog::new(
        platform(os_state),
        FakeClock::new(1_767_225_600_000),
        Timing::default(),
        Box::new(store),
        startup,
    )
}

/// The notification state of the first tunnel's incident in a state file.
fn stored_notification(path: &Path) -> Option<NotificationState> {
    let state: PersistedState = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(state.tunnels.first()?.incident.as_ref()?.notification)
}

fn restored(startup: Startup) -> PersistedState {
    match startup {
        Startup::Restored(state) => state,
        other => panic!("expected restored state, got {other:?}"),
    }
}

/// install.ps1's `Backup-State` on the upgrade: each state file that exists
/// is copied as it is into the backup folder, in the same subfolder.
fn back_up(data: &Path, backup: &Path) {
    for name in STATE_FILES {
        let source = data.join(name);
        if source.is_file() {
            let copy = backup.join(name);
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::copy(&source, &copy).unwrap();
        }
    }
}

/// The text step 3 writes over `state.json`, as the README spells it.
fn marker() -> &'static str {
    README
        .lines()
        .find_map(|line| line.trim().strip_prefix("$marker = '")?.strip_suffix('\''))
        .expect("the README's rollback writes a marker over state.json")
}

/// Whether step 3 takes a backup file as usable state: JSON with a
/// `schema_version`.
fn usable(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|json| json.get("schema_version").is_some())
}

/// Step 3 of the README's rollback: the marker over `state.json` first;
/// then the backup's usable state (its `state.json`, else its
/// `state.prev.json`) as `state.prev.json`, or, with no state in the
/// backup, neither file; then the notification settings as in the backup.
fn roll_back(data: &Path, backup: &Path) {
    let current = data.join("state").join(STATE_FILE);
    let previous = data.join("state").join(PREVIOUS_FILE);
    let saved = ["state/state.json", "state/state.prev.json"]
        .into_iter()
        .map(|name| backup.join(name))
        .find(|path| path.is_file() && usable(path));
    fs::write(&current, marker()).unwrap();
    if let Some(saved) = saved {
        fs::copy(saved, &previous).unwrap();
    } else {
        if previous.is_file() {
            fs::remove_file(&previous).unwrap();
        }
        fs::remove_file(&current).unwrap();
    }
    let settings = data.join("notify/notify.json");
    let backup_settings = backup.join("notify/notify.json");
    if backup_settings.is_file() {
        fs::copy(&backup_settings, &settings).unwrap();
    } else if settings.is_file() {
        fs::remove_file(&settings).unwrap();
    }
}

/// The reviewer's scenario: in the backup the alert is still pending and
/// automatic recovery and the external restart are on; after the upgrade
/// the newer build sent the alert and the user disconnected the tunnel. The
/// older build, started on the rolled-back state, must never send the alert
/// again and must not start the tunnel the user stopped.
#[test]
fn p2_9_the_rolled_back_state_is_restored_from_the_backup() {
    let data = TempDir::new("data");
    let state_dir = data.path().join("state");
    fs::create_dir(&state_dir).unwrap();
    fs::create_dir(data.path().join("notify")).unwrap();
    fs::write(data.path().join("notify/notify.json"), b"{\"older\":true}").unwrap();
    let id = TunnelId::new("office").unwrap();

    // The older build: office registered, its first recovery failed and the
    // alert is due; automatic recovery and the external restart are on.
    {
        let mut wd = watchdog(&state_dir, OsState::Connected);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: id.clone(),
        });
    }
    let (mut store, startup, _) = FileStore::open(&state_dir).unwrap();
    let mut older = restored(startup);
    let rec = &mut older.tunnels[0];
    rec.intent.auto_recovery_enabled = true;
    rec.intent.start_when_stopped_externally = true;
    rec.baseline_passed = true;
    rec.incident = Some(IncidentRecord {
        incident_id: IncidentId::new("inc-000001").unwrap(),
        opened_at_unix_ms: 1_767_225_000_000,
        first_recovery: FirstRecovery::Failed,
        backoff_step: 0,
        counted_from_step: 0,
        retries_exhausted: false,
        exhausted_notice: Default::default(),
        manual_last: false,
        resume_uncounted: false,
        wait_from_unix_ms: None,
        last_round_end_unix_ms: Some(1_767_225_100_000),
        next_retry_unix_ms: Some(1_767_225_700_000),
        notification: NotificationState::Pending,
        notification_message_id: None,
        cancel_requested: false,
    });
    store.save(&older).unwrap();
    drop(store);

    // The upgrade backs the state up while the service is stopped.
    let backup = TempDir::new("backup");
    back_up(data.path(), backup.path());
    let backed_up = fs::read(backup.path().join("state/state.json")).unwrap();

    // The newer build sends the alert and the user disconnects office.
    let (mut store, startup, _) = FileStore::open(&state_dir).unwrap();
    let mut newer = restored(startup);
    let rec = &mut newer.tunnels[0];
    let incident = rec.incident.as_mut().unwrap();
    incident.notification = NotificationState::Sent;
    incident.notification_message_id = Some(77);
    rec.intent.desired = DesiredState::Disconnected;
    store.save(&newer).unwrap();
    drop(store);
    fs::write(data.path().join("notify/notify.json"), b"{\"newer\":true}").unwrap();

    roll_back(data.path(), backup.path());

    // The older build starts on what the rollback left, with office stopped
    // as the user left it.
    let (store, startup, source) = FileStore::open(&state_dir).unwrap();
    assert!(
        matches!(startup, Startup::RestoredFromBackup(_)),
        "the rolled-back state must load as restored from a backup, not as {startup:?}"
    );
    assert_eq!(source, Source::Previous);
    let mut wd = Watchdog::new(
        platform(OsState::Disconnected),
        FakeClock::new(1_767_225_600_000),
        Timing::default(),
        Box::new(store),
        startup,
    );
    let snapshot = wd.snapshot();
    let tunnel = &snapshot.tunnels[0];
    assert_eq!(
        tunnel.incident.as_ref().unwrap().notification,
        NotificationState::Unknown,
        "an alert that may have been sent is never sent again"
    );
    assert!(wd.notifications_due().is_empty());
    assert!(!tunnel.intent.auto_recovery_enabled);
    assert!(!tunnel.intent.start_when_stopped_externally);
    // The conservative state was saved at once over the marker; the backup
    // stays the previous file until a valid current one exists.
    assert_eq!(
        stored_notification(&state_dir.join(STATE_FILE)),
        Some(NotificationState::Unknown)
    );
    assert_eq!(fs::read(state_dir.join(PREVIOUS_FILE)).unwrap(), backed_up);
    for _ in 0..600 {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
    assert!(wd.notifications_due().is_empty());
    // Once the start's grace period is over and the tunnel has been read.
    let later = wd.snapshot();
    assert_shown_as_the_readme_says(later.tunnels[0].light, later.tunnels[0].light_reason);
    let controls: Vec<_> = wd
        .adapter()
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::Start(_) | Call::Stop(_)))
        .collect();
    assert!(controls.is_empty(), "{controls:?}");
    drop(wd);

    // A later start trusts the conservative state, and no file still says
    // the alert is pending. The notification settings are the backup's.
    let (_store, startup, source) = FileStore::open(&state_dir).unwrap();
    assert_eq!(source, Source::Current);
    let stored = restored(startup);
    assert!(!stored.tunnels[0].intent.auto_recovery_enabled);
    for file in [STATE_FILE, PREVIOUS_FILE] {
        assert_eq!(
            stored_notification(&state_dir.join(file)),
            Some(NotificationState::Unknown),
            "{file}"
        );
    }
    assert_eq!(
        fs::read(data.path().join("notify/notify.json")).unwrap(),
        b"{\"older\":true}"
    );
}

/// Codex re-review P3-N2: office, which the user stopped after the upgrade,
/// wants to be connected in the backup. With automatic recovery off and no
/// state seen before it stopped, the older build cannot tell the user's stop
/// from anyone else's: it shows the tunnel gray and "not connected", never
/// "stopped externally", and the README says so in the window's words.
fn assert_shown_as_the_readme_says(light: Light, reason: LightReason) {
    assert_eq!((light, reason), (Light::Gray, LightReason::NotConnected));
    let line = readme_on_disconnected_after_upgrade();
    for words in [
        window_text("light", light),
        window_text("light_reason", reason),
    ] {
        assert!(line.contains(&format!("「{words}」")), "{words}: {line}");
    }
    for never in [
        window_text("light_reason", LightReason::StoppedExternally),
        window_text("takeover", "stopped_externally"),
    ] {
        assert!(!line.contains(&never), "{never}: {line}");
    }
}

/// Why step 3 needs the marker: the backup put in place as `state.prev.json`
/// with no `state.json` beside it is taken for a save interrupted between
/// its renames, and loaded as an ordinary, trusted state.
#[test]
fn p2_9_a_backup_as_the_previous_file_alone_would_be_trusted() {
    let dir = TempDir::new("prev-only");
    let (mut store, _, _) = FileStore::open(dir.path()).unwrap();
    store
        .save(&PersistedState::new("older".to_owned()))
        .unwrap();
    drop(store);
    fs::rename(dir.path().join(STATE_FILE), dir.path().join(PREVIOUS_FILE)).unwrap();
    let (_store, startup, _) = FileStore::open(dir.path()).unwrap();
    assert!(matches!(startup, Startup::Restored(_)), "{startup:?}");
}

/// The marker is no state file: next to a valid previous file it makes the
/// store restore that one from the backup.
#[test]
fn p2_9_the_readme_marker_makes_the_previous_file_a_backup() {
    assert!(serde_json::from_str::<serde_json::Value>(marker()).is_err());
    let dir = TempDir::new("marker");
    let (mut store, _, _) = FileStore::open(dir.path()).unwrap();
    store
        .save(&PersistedState::new("older".to_owned()))
        .unwrap();
    drop(store);
    fs::rename(dir.path().join(STATE_FILE), dir.path().join(PREVIOUS_FILE)).unwrap();
    fs::write(dir.path().join(STATE_FILE), marker()).unwrap();
    let (_store, startup, source) = FileStore::open(dir.path()).unwrap();
    assert_eq!(source, Source::Previous);
    match startup {
        Startup::RestoredFromBackup(state) => assert_eq!(state.installation_id, "older"),
        other => panic!("{other:?}"),
    }
}

/// The English README (`packaging/windows/README.md`), which gives the same
/// rollback in English and ships beside the Traditional Chinese one.
const README_EN: &str = include_str!("../../../packaging/windows/README.md");

/// The window's English language file, whose words the English README
/// quotes.
const WINDOW_TEXTS_EN: &str = include_str!("../../../apps/desktop/src/lang/en.json");

/// The English window's words for `value` in `section`, `value` given as
/// the watchdog sends it.
fn window_text_en(section: &str, value: impl serde::Serialize) -> String {
    let texts: serde_json::Value = serde_json::from_str(WINDOW_TEXTS_EN).unwrap();
    let key = serde_json::to_value(value).unwrap();
    texts[section][key.as_str().unwrap()]
        .as_str()
        .unwrap_or_else(|| panic!("no {section}.{key} in en.json"))
        .to_owned()
}

/// The lines of the rollback's step 3 code block in `readme`: the
/// PowerShell block that sets `$marker`. Each `throw` keeps only what
/// comes before its message, which the two READMEs word in their own
/// language.
fn step_three_code(readme: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in readme.lines() {
        let trimmed = line.trim();
        match current.as_mut() {
            None if trimmed == "```powershell" => current = Some(Vec::new()),
            None => {}
            Some(_) if trimmed == "```" => blocks.push(current.take().unwrap()),
            Some(block) => block.push(match trimmed.find("throw ") {
                Some(at) => trimmed[..at + "throw".len()].to_owned(),
                None => trimmed.to_owned(),
            }),
        }
    }
    let mut found = blocks
        .into_iter()
        .filter(|block| block.iter().any(|line| line.starts_with("$marker = '")));
    let block = found.next().expect("a code block sets the marker");
    assert!(found.next().is_none(), "one code block sets the marker");
    block
}

/// The English README's line about a tunnel the user disconnected after
/// the upgrade.
fn english_readme_on_disconnected_after_upgrade() -> &'static str {
    let mut lines = README_EN
        .lines()
        .filter(|line| line.contains("disconnected after the upgrade"));
    let line = lines
        .next()
        .expect("the English README's rollback says what happens to a tunnel disconnected after the upgrade");
    assert!(lines.next().is_none(), "one such line");
    line
}

/// The English README writes the same marker and runs the same step 3 as
/// the Traditional Chinese one, whose rollback the tests above follow.
#[test]
fn the_english_readme_rolls_back_the_same_way() {
    let marker_en = README_EN
        .lines()
        .find_map(|line| line.trim().strip_prefix("$marker = '")?.strip_suffix('\''))
        .expect("the English README's rollback writes a marker over state.json");
    assert_eq!(marker_en, marker());
    assert_eq!(step_three_code(README_EN), step_three_code(README));
}

/// What the English README says the older build shows for a tunnel the
/// user disconnected after the upgrade, in the English window's words: the
/// light and reason `p2_9_the_rolled_back_state_is_restored_from_the_backup`
/// finds (gray, `not_connected`), never "stopped externally".
#[test]
fn the_english_readme_names_what_the_window_shows() {
    let line = english_readme_on_disconnected_after_upgrade();
    for words in [
        window_text_en("light", Light::Gray),
        window_text_en("light_reason", LightReason::NotConnected),
    ] {
        assert!(line.contains(&format!("\"{words}\"")), "{words}: {line}");
    }
    for never in [
        window_text_en("light_reason", LightReason::StoppedExternally),
        window_text_en("takeover", "stopped_externally"),
    ] {
        assert!(!line.contains(&never), "{never}: {line}");
    }
}

// --- Step 3a: wgal's language out of notify.json (owner decision of
// 2026-11-19) ------------------------------------------------------------

/// The lines of the rollback's step 3a code block in `readme`: the one
/// that names `cli_language`.
fn step_three_a_code(readme: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in readme.lines() {
        let trimmed = line.trim();
        match current.as_mut() {
            None if trimmed == "```powershell" => current = Some(Vec::new()),
            None => {}
            Some(_) if trimmed == "```" => blocks.push(current.take().unwrap()),
            Some(block) => block.push(trimmed.to_owned()),
        }
    }
    let mut found = blocks
        .into_iter()
        .filter(|block| block.iter().any(|line| line.contains("cli_language")));
    let block = found.next().expect("a code block removes cli_language");
    assert!(
        found.next().is_none(),
        "one code block removes cli_language"
    );
    block
}

/// `code` without what the two READMEs word in their own language: each
/// `throw`'s message, and a line that only writes a sentence.
fn without_messages(code: &[String]) -> Vec<String> {
    code.iter()
        .map(|line| match line.find("throw ") {
            Some(at) => line[..at + "throw".len()].to_owned(),
            None if line.starts_with('"') => "\"".to_owned(),
            None => line.clone(),
        })
        .collect()
}

#[test]
fn both_readmes_remove_wgal_s_language_the_same_way() {
    assert_eq!(
        without_messages(&step_three_a_code(README_EN)),
        without_messages(&step_three_a_code(README))
    );
}

/// `notify.json` as a watchdog before 4.0.0 reads it: refusing fields it
/// does not have.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ReadBy300 {
    schema_version: u32,
    enabled: bool,
    device_name: String,
    chat_id: Option<String>,
    #[serde(default)]
    token_set: bool,
    #[serde(default)]
    templates: Option<serde_json::Value>,
}

/// What Windows PowerShell 5.1 printed and how it ended.
#[cfg(windows)]
struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// The longest a run of step 3a may take before it is ended.
#[cfg(windows)]
const RUN_DEADLINE: Duration = Duration::from_secs(60);
/// The most of each output stream kept.
#[cfg(windows)]
const RUN_OUTPUT: u64 = 64 * 1024;

/// Runs `code` with Windows PowerShell 5.1 (the fixed system path),
/// `$data` set to `data`, as an administrator would paste it (no
/// `$ErrorActionPreference` of its own). Only files under `data` are read
/// or written. Ended after [`RUN_DEADLINE`]; each stream is kept up to
/// [`RUN_OUTPUT`] bytes.
#[cfg(windows)]
fn powershell(data: &Path, code: &str) -> Ran {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let script = format!(
        "$data = '{}'\r\n{}\r\n",
        data.display().to_string().replace('\'', "''"),
        code
    );
    let file = data.join("run.ps1");
    // A byte order mark, so that Windows PowerShell reads the script's
    // own non-ASCII text as UTF-8.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    fs::write(&file, bytes).unwrap();
    let windows = std::env::var_os("SystemRoot").expect("SystemRoot is set");
    let exe = Path::new(&windows)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    // A PSModulePath inherited from PowerShell 7 (cargo run from pwsh)
    // makes Windows PowerShell load 7's modules and fail; an
    // administrator's own Windows PowerShell window has its own.
    let mut child = Command::new(exe)
        .env_remove("PSModulePath")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let keep = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(stream) = stream {
                let _ = stream.take(RUN_OUTPUT).read_to_end(&mut kept);
            }
            kept
        })
    };
    let stdout = keep(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = keep(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > RUN_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("step 3a did not end within {RUN_DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let ran = Ran {
        ok: status.success(),
        stdout: String::from_utf8_lossy(&stdout.join().unwrap()).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.join().unwrap()).into_owned(),
    };
    fs::remove_file(&file).unwrap();
    ran
}

/// The README's step 3a, as written in `readme`.
#[cfg(windows)]
fn run_step_three_a(data: &Path, readme: &str) -> Ran {
    powershell(data, &step_three_a_code(readme).join("\r\n"))
}

/// Settings as 4.0.0 writes them, with the window's Traditional Chinese
/// templates, the language last when given.
#[cfg(windows)]
fn written_by_4_0_0(language: Option<&str>) -> Vec<u8> {
    use wgal_protocol::{ChatId, DeviceName, LanguageTag, NotificationTemplates};
    use wgal_runtime::notify::NotificationSettings;
    let texts: serde_json::Value = serde_json::from_str(WINDOW_TEXTS).unwrap();
    let mut set = texts["notification_templates"].clone();
    set["id"] = "zh-TW/3a3a3a3a".into();
    let settings = NotificationSettings {
        enabled: true,
        device_name: DeviceName::new("辦公室\"Lab\"\\筆電").unwrap(),
        chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        token_set: true,
        templates: Some(serde_json::from_value::<NotificationTemplates>(set).unwrap()),
        cli_language: language.map(|tag| LanguageTag::new(tag).unwrap()),
        ..NotificationSettings::default()
    };
    serde_json::to_vec_pretty(&settings).unwrap()
}

/// A settings file of 4.0.0's fields with `cli_language` holding `value`
/// at `place` among them (0 first), as a hand or another program could
/// have left it; 4.0.0 still loads every one of these.
#[cfg(windows)]
fn with_language_at(value: &serde_json::Value, place: usize) -> String {
    let mut fields: Vec<(&str, serde_json::Value)> = vec![
        ("schema_version", 1.into()),
        ("enabled", true.into()),
        ("device_name", "Lab".into()),
        ("chat_id", "-1001234567890".into()),
        ("token_set", true.into()),
    ];
    fields.insert(place, ("cli_language", value.clone()));
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("  {}: {}", serde_json::to_string(k).unwrap(), v))
        .collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// `data` with an empty `notify` folder, and where `notify.json` goes.
#[cfg(windows)]
fn notify_dir(dir: &TempDir) -> PathBuf {
    use wgal_runtime::notify::SETTINGS_FILE;
    let notify = dir.path().join("notify");
    fs::create_dir(&notify).unwrap();
    notify.join(SETTINGS_FILE)
}

/// A backup made by an upgrade from 4.0.0 or later after `wgal language`:
/// in either README's words, step 3a deletes the file, whatever the
/// language holds and wherever it is, and nothing else in the folder.
#[cfg(windows)]
#[test]
fn step_three_a_deletes_settings_that_hold_wgal_s_language() {
    for readme in [README_EN, README] {
        let dir = TempDir::new("step-3a");
        let path = notify_dir(&dir);
        let neighbour = path.with_file_name("telegram-bot-token.dpapi");
        fs::write(&neighbour, b"not a real token").unwrap();

        let with = written_by_4_0_0(Some("zh-TW"));
        assert!(serde_json::from_slice::<ReadBy300>(&with).is_err());
        fs::write(&path, &with).unwrap();
        let ran = run_step_three_a(dir.path(), readme);
        assert!(ran.ok, "{}", ran.stderr);
        assert!(!path.exists(), "{}", ran.stdout);
        assert!(ran.stdout.contains("notify.json"), "{}", ran.stdout);
        assert_eq!(fs::read(&neighbour).unwrap(), b"not a real token");

        for value in [
            serde_json::json!("ja"),
            serde_json::Value::Null,
            serde_json::json!(7),
            serde_json::json!(["ja"]),
            serde_json::json!({ "tag": "ja" }),
            serde_json::json!("../ja"),
            serde_json::json!(""),
        ] {
            for place in [0, 3, 5] {
                let text = with_language_at(&value, place);
                assert!(serde_json::from_str::<ReadBy300>(&text).is_err());
                fs::write(&path, &text).unwrap();
                let ran = run_step_three_a(dir.path(), readme);
                assert!(ran.ok, "{value} at {place}: {}", ran.stderr);
                assert!(!path.exists(), "{value} at {place}");
            }
        }
    }
}

/// Settings without the language, the ones the older version wrote itself
/// among them, and no settings at all are left as they are.
#[cfg(windows)]
#[test]
fn step_three_a_leaves_other_settings_alone() {
    for readme in [README_EN, README] {
        step_three_a_leaves_other_settings_alone_in(readme);
    }
}

#[cfg(windows)]
fn step_three_a_leaves_other_settings_alone_in(readme: &str) {
    let dir = TempDir::new("step-3a-none");
    let path = notify_dir(&dir);
    let without = written_by_4_0_0(None);
    assert!(serde_json::from_slice::<ReadBy300>(&without).is_ok());
    // The key's name inside a value is not the key.
    let named = String::from_utf8(without.clone())
        .unwrap()
        .replace("辦公室\\\"Lab\\\"\\\\筆電", "cli_language");
    assert!(
        named.contains("\"device_name\": \"cli_language\""),
        "{named}"
    );
    for file in [without, named.into_bytes()] {
        fs::write(&path, &file).unwrap();
        let ran = run_step_three_a(dir.path(), readme);
        assert!(ran.ok, "{}", ran.stderr);
        assert_eq!(fs::read(&path).unwrap(), file);
    }
    fs::remove_file(&path).unwrap();
    let ran = run_step_three_a(dir.path(), readme);
    assert!(ran.ok, "{}", ran.stderr);
    assert!(!path.exists());
}

/// A file that is not a JSON object stops step 3a with an error, and stays
/// as it is.
#[cfg(windows)]
#[test]
fn step_three_a_stops_on_what_it_cannot_read() {
    for readme in [README_EN, README] {
        let dir = TempDir::new("step-3a-fail");
        let path = notify_dir(&dir);
        for text in ["[1, 2]", "\"cli_language\"", "{ not json", "", "7"] {
            fs::write(&path, text).unwrap();
            let ran = run_step_three_a(dir.path(), readme);
            assert!(!ran.ok, "{text:?}: {}", ran.stdout);
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
    }
}

// --- Step 3 and hard links (Codex review of the language plan, sixth
// round, R6-1) -------------------------------------------------------------

/// The rollback's step 3 code block in `readme`, as written.
#[cfg(windows)]
fn step_three_as_written(readme: &str) -> String {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in readme.lines() {
        let trimmed = line.trim();
        match current.as_mut() {
            None if trimmed == "```powershell" => current = Some(Vec::new()),
            None => {}
            Some(_) if trimmed == "```" => blocks.push(current.take().unwrap()),
            Some(block) => block.push(trimmed.to_owned()),
        }
    }
    blocks
        .into_iter()
        .find(|block| block.iter().any(|line| line.starts_with("$marker = '")))
        .expect("a code block sets the marker")
        .join("\r\n")
}

/// Runs step 3, as `readme` writes it, on a temporary `$data` and backup,
/// with `Get-Service` answering that the service is stopped (the real
/// service is never asked).
#[cfg(windows)]
fn run_step_three(data: &Path, backup: &Path, readme: &str) -> Ran {
    run_step_three_with(data, backup, readme, "")
}

/// [`run_step_three`] with more functions standing in for commands.
#[cfg(windows)]
fn run_step_three_with(data: &Path, backup: &Path, readme: &str, stand_ins: &str) -> Ran {
    let code = format!(
        "function Get-Service {{ [pscustomobject]@{{ Status = 'Stopped' }} }}\r\n{stand_ins}\r\n$from = '{}'\r\n{}",
        backup.display().to_string().replace('\'', "''"),
        step_three_as_written(readme)
    );
    powershell(data, &code)
}

/// The three files step 3 writes, as they are now.
#[cfg(windows)]
fn written_by_step_three(data: &Path) -> Vec<Option<Vec<u8>>> {
    [
        Path::new("state").join(STATE_FILE),
        Path::new("state").join(PREVIOUS_FILE),
        Path::new("notify").join("notify.json"),
    ]
    .iter()
    .map(|f| fs::read(data.join(f)).ok())
    .collect()
}

/// A data folder with state, a previous copy and notification settings,
/// and a backup with its own; returns (data, backup).
#[cfg(windows)]
fn step_three_folders(dir: &TempDir) -> (PathBuf, PathBuf) {
    let data = dir.path().join("data");
    let backup = dir.path().join("backup");
    for folder in [&data, &backup] {
        fs::create_dir_all(folder.join("state")).unwrap();
        fs::create_dir_all(folder.join("notify")).unwrap();
    }
    fs::write(
        data.join("state").join(STATE_FILE),
        br#"{"schema_version":1,"newer":true}"#,
    )
    .unwrap();
    fs::write(
        data.join("state").join(PREVIOUS_FILE),
        br#"{"schema_version":1,"newer":true}"#,
    )
    .unwrap();
    fs::write(
        data.join("notify").join("notify.json"),
        br#"{"newer":true}"#,
    )
    .unwrap();
    fs::write(
        backup.join("state").join(STATE_FILE),
        br#"{"schema_version":1,"older":true}"#,
    )
    .unwrap();
    fs::write(
        backup.join("notify").join("notify.json"),
        br#"{"older":true}"#,
    )
    .unwrap();
    (data, backup)
}

/// A file step 3 writes over that has another name (a hard link) stops it
/// before anything is written: the other name's file, and every file of
/// the rollback, stay as they were.
#[cfg(windows)]
#[test]
fn step_three_stops_on_a_hard_link_before_writing() {
    for readme in [README_EN, README] {
        for linked in [
            Path::new("state").join(STATE_FILE),
            Path::new("state").join(PREVIOUS_FILE),
            Path::new("notify").join("notify.json"),
        ] {
            let dir = TempDir::new("step-3-link");
            let (data, backup) = step_three_folders(&dir);
            let sentinel = dir.path().join("sentinel.txt");
            fs::write(&sentinel, b"another file").unwrap();
            let target = data.join(&linked);
            fs::remove_file(&target).unwrap();
            fs::hard_link(&sentinel, &target).unwrap();
            let before: Vec<Vec<u8>> = [
                Path::new("state").join(STATE_FILE),
                Path::new("state").join(PREVIOUS_FILE),
                Path::new("notify").join("notify.json"),
            ]
            .iter()
            .map(|f| fs::read(data.join(f)).unwrap())
            .collect();
            let ran = run_step_three(&data, &backup, readme);
            assert!(!ran.ok, "{linked:?}: {}", ran.stdout);
            assert!(
                ran.stderr.contains("hard link"),
                "{linked:?}: {}",
                ran.stderr
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"another file", "{linked:?}");
            let after: Vec<Vec<u8>> = [
                Path::new("state").join(STATE_FILE),
                Path::new("state").join(PREVIOUS_FILE),
                Path::new("notify").join("notify.json"),
            ]
            .iter()
            .map(|f| fs::read(data.join(f)).unwrap())
            .collect();
            assert_eq!(after, before, "{linked:?}");
        }
    }
}

/// A folder where step 3 writes a file stops it before anything is
/// written, a hard link inside that folder included (Codex review of the
/// language plan, seventh round, R7-1).
#[cfg(windows)]
#[test]
fn step_three_stops_on_a_folder_where_it_writes_a_file() {
    for readme in [README_EN, README] {
        for written in [
            Path::new("state").join(STATE_FILE),
            Path::new("state").join(PREVIOUS_FILE),
            Path::new("notify").join("notify.json"),
        ] {
            let dir = TempDir::new("step-3-folder");
            let (data, backup) = step_three_folders(&dir);
            let sentinel = dir.path().join("sentinel.txt");
            fs::write(&sentinel, b"another file").unwrap();
            let target = data.join(&written);
            fs::remove_file(&target).unwrap();
            fs::create_dir(&target).unwrap();
            let inner = target.join(written.file_name().unwrap());
            fs::hard_link(&sentinel, &inner).unwrap();
            let before = written_by_step_three(&data);
            let ran = run_step_three(&data, &backup, readme);
            assert!(!ran.ok, "{written:?}: {}", ran.stdout);
            assert!(
                ran.stderr.contains("hard link"),
                "{written:?}: {}",
                ran.stderr
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"another file", "{written:?}");
            assert_eq!(fs::read(&inner).unwrap(), b"another file", "{written:?}");
            assert_eq!(written_by_step_three(&data), before, "{written:?}");
        }
    }
}

/// Without a hard link or a folder in the way, step 3 goes ahead in either
/// README's words and ends well, with no error: the marker, the backup's
/// state as the previous copy, the backup's notification settings. Setting
/// the owner to Administrators needs an administrator, so here `Set-Acl`
/// is a stand-in that does nothing, and the owner is not checked (NOT RUN).
#[cfg(windows)]
#[test]
fn step_three_puts_the_backup_back_without_hard_links() {
    for readme in [README_EN, README] {
        let dir = TempDir::new("step-3-plain");
        let (data, backup) = step_three_folders(&dir);
        let ran = run_step_three_with(&data, &backup, readme, "function Set-Acl { }");
        assert!(ran.ok, "{}", ran.stderr);
        assert!(ran.stderr.trim().is_empty(), "{}", ran.stderr);
        assert_eq!(
            fs::read_to_string(data.join("state").join(STATE_FILE)).unwrap(),
            marker()
        );
        assert_eq!(
            fs::read(data.join("state").join(PREVIOUS_FILE)).unwrap(),
            br#"{"schema_version":1,"older":true}"#
        );
        assert_eq!(
            fs::read(data.join("notify").join("notify.json")).unwrap(),
            br#"{"older":true}"#
        );
    }
}

/// A write that fails stops step 3 there (Codex review of the language
/// plan, eighth round, R8-1): when the marker cannot be written over
/// `state.json` (a read-only file here), the backup's state and
/// notification settings are not put back either, and it ends with an
/// error, so that step 4 is not done.
#[cfg(windows)]
#[test]
fn step_three_stops_at_a_write_that_fails() {
    for readme in [README_EN, README] {
        let dir = TempDir::new("step-3-stop");
        let (data, backup) = step_three_folders(&dir);
        let current = data.join("state").join(STATE_FILE);
        let mut permissions = fs::metadata(&current).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&current, permissions.clone()).unwrap();
        let before = written_by_step_three(&data);
        let ran = run_step_three_with(&data, &backup, readme, "function Set-Acl { }");
        assert!(!ran.ok, "{}", ran.stdout);
        assert_eq!(written_by_step_three(&data), before);
        // Windows only: writable again, so that the folder can be removed.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&current, permissions).unwrap();
    }
}

/// Step 3a keeps nothing from an earlier run in the same PowerShell window
/// (Codex review of the language plan, ninth round, R9-1): with a parsed
/// set of settings that holds the language still about from before, a file
/// that cannot be read as a JSON object now stops it, and stays as it is.
#[cfg(windows)]
#[test]
fn step_three_a_keeps_nothing_from_an_earlier_run() {
    let earlier = "Add-Type -AssemblyName System.Web.Extensions\r\n\
                   $object = (New-Object Web.Script.Serialization.JavaScriptSerializer).DeserializeObject('{\"cli_language\":\"ja\"}')\r\n";
    for readme in [README_EN, README] {
        let dir = TempDir::new("step-3a-earlier");
        let path = notify_dir(&dir);
        for text in ["{ not json", "", "[1, 2]"] {
            fs::write(&path, text).unwrap();
            let code = format!("{earlier}{}", step_three_a_code(readme).join("\r\n"));
            let ran = powershell(dir.path(), &code);
            assert!(!ran.ok, "{text:?}: {}", ran.stdout);
            assert_eq!(fs::read_to_string(&path).unwrap(), text, "{text:?}");
        }
    }
}
