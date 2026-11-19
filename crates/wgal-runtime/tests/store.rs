//! The file store: atomic replace, previous copy, single writer, refusal of
//! symlinks and fail-closed behaviour (plan §9.1, T17, T25). Every test works
//! in its own fresh directory under the system temp directory.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wgal_core::{
    IncidentRecord, PersistedState, SCHEMA_VERSION, Startup, StateStore, StoreError, Timing,
    TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterCapabilities, Capability, DesiredState, ErrorCode, ErrorReason, FirstRecovery,
    HostSettings, IncidentId, NotificationState, OsState, Outcome, ProbeTiming, RecoveryRetry,
    Request, StorageHealth, TunnelId,
};
use wgal_runtime::store::{
    Barrier, Durability, FileStore, Flushed, PREVIOUS_FILE, Renamer, STATE_FILE, Source,
};
use wgal_testkit::{FakeClock, FakePlatform};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("wgal-store-{}-{name}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn state(installation: &str) -> PersistedState {
    PersistedState::new(installation.to_owned())
}

fn open(dir: &TempDir) -> (FileStore, Startup, Source) {
    FileStore::open(dir.path()).unwrap()
}

fn restored(startup: Startup) -> PersistedState {
    match startup {
        Startup::Restored(state) => state,
        other => panic!("expected restored state, got {other:?}"),
    }
}

fn restored_from_backup(startup: Startup) -> PersistedState {
    match startup {
        Startup::RestoredFromBackup(state) => state,
        other => panic!("expected state restored from the backup, got {other:?}"),
    }
}

#[test]
fn saves_load_back_and_keep_the_previous_version() {
    let dir = TempDir::new("roundtrip");
    let (mut store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::Empty));
    assert_eq!(source, Source::Nothing);
    store.save(&state("first")).unwrap();
    store.save(&state("second")).unwrap();
    drop(store);

    let (_store, startup, source) = open(&dir);
    assert_eq!(source, Source::Current);
    assert_eq!(restored(startup).installation_id, "second");
    let previous: PersistedState =
        serde_json::from_slice(&fs::read(dir.file(PREVIOUS_FILE)).unwrap()).unwrap();
    assert_eq!(previous.installation_id, "first");
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temporary files remain");
}

#[test]
fn truncated_current_file_falls_back_to_the_previous_one() {
    let dir = TempDir::new("truncated");
    let (mut store, _, _) = open(&dir);
    store.save(&state("first")).unwrap();
    store.save(&state("second")).unwrap();
    drop(store);
    let full = fs::read(dir.file(STATE_FILE)).unwrap();
    fs::write(dir.file(STATE_FILE), &full[..full.len() / 2]).unwrap();

    let (_store, startup, source) = open(&dir);
    assert_eq!(source, Source::Previous);
    assert_eq!(
        restored_from_backup(startup).installation_id,
        "first",
        "a damaged current file was a completed save, so the copy may be out of date"
    );
}

#[test]
fn missing_current_file_after_a_crash_between_renames_uses_the_previous_one() {
    let dir = TempDir::new("between-renames");
    let (mut store, _, _) = open(&dir);
    store.save(&state("only")).unwrap();
    drop(store);
    fs::rename(dir.file(STATE_FILE), dir.file(PREVIOUS_FILE)).unwrap();

    let (_store, startup, source) = open(&dir);
    assert_eq!(source, Source::Previous);
    assert_eq!(restored(startup).installation_id, "only");
}

#[test]
fn unusable_state_opens_read_only_and_is_never_overwritten() {
    let dir = TempDir::new("corrupt");
    fs::write(dir.file(STATE_FILE), b"{ not json").unwrap();
    fs::write(dir.file(PREVIOUS_FILE), b"").unwrap();

    let (mut store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed));
    assert_eq!(source, Source::Unusable);
    assert!(matches!(
        store.save(&state("x")),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), b"{ not json");
    drop(store);

    // A change is refused as one over state that could not be loaded.
    let (store, startup, _) = open(&dir);
    let mut wd = Watchdog::new(
        platform(),
        FakeClock::new(1_767_225_600_000),
        Timing::default(),
        Box::new(store),
        startup,
    );
    let Outcome::Err(error) = wd.handle(&Request::SetMonitoringPaused { paused: true }) else {
        panic!("a change was accepted over state that could not be loaded");
    };
    assert_eq!(
        (error.code, error.reason),
        (
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        )
    );
    assert_eq!(wd.storage_reason(), ErrorReason::StorageLoadFailed);
    drop(wd);
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), b"{ not json");
}

#[test]
fn state_from_a_newer_watchdog_is_not_replaced() {
    let dir = TempDir::new("newer");
    let mut newer = state("future");
    newer.schema_version = SCHEMA_VERSION + 1;
    fs::write(dir.file(STATE_FILE), serde_json::to_vec(&newer).unwrap()).unwrap();
    let (_store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed));
    assert_eq!(source, Source::Unusable);
}

/// Integrated review of `121b42a`, finding 1: a newer watchdog's file is
/// recognised by its version even when it no longer parses as this version
/// (here: a field this version does not know). It must not be classed as
/// damaged, so the store neither falls back to the previous file nor ever
/// overwrites or rotates the newer one away.
#[test]
fn i1_newer_state_with_unknown_fields_is_refused_not_treated_as_damaged() {
    let dir = TempDir::new("newer-fields");
    let mut newer = serde_json::to_value(state("future")).unwrap();
    newer["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
    newer["added_by_a_newer_watchdog"] = serde_json::json!({ "anything": [1, 2, 3] });
    let newer = serde_json::to_vec_pretty(&newer).unwrap();
    fs::write(dir.file(STATE_FILE), &newer).unwrap();
    let older = serde_json::to_vec_pretty(&state("older")).unwrap();
    fs::write(dir.file(PREVIOUS_FILE), &older).unwrap();

    let (mut store, startup, source) = open(&dir);
    assert!(
        matches!(startup, Startup::StorageFailed),
        "the previous file must not be used: {startup:?}"
    );
    assert_eq!(source, Source::Unusable);
    assert!(matches!(
        store.save(&state("x")),
        Err(StoreError::Refused(_))
    ));
    drop(store);
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), newer);
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), older);

    // Still refused on the next start; nothing was rotated in between.
    let (_store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed));
    assert_eq!(source, Source::Unusable);
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), newer);
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), older);
}

/// Finding 1, the previous file: with the current file missing, a newer
/// previous file is refused too, whatever fields it has.
#[test]
fn i1_newer_previous_file_with_unknown_fields_is_refused() {
    let dir = TempDir::new("newer-prev");
    let mut newer = serde_json::to_value(state("future")).unwrap();
    newer["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
    newer["added_by_a_newer_watchdog"] = serde_json::json!(true);
    let newer = serde_json::to_vec(&newer).unwrap();
    fs::write(dir.file(PREVIOUS_FILE), &newer).unwrap();

    let (mut store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed), "{startup:?}");
    assert_eq!(source, Source::Unusable);
    assert!(store.save(&state("x")).is_err());
    assert!(!dir.file(STATE_FILE).exists());
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), newer);
}

#[test]
fn unknown_older_schema_version_is_refused_rather_than_guessed() {
    let dir = TempDir::new("older");
    let mut older = state("past");
    older.schema_version = 0;
    fs::write(dir.file(STATE_FILE), serde_json::to_vec(&older).unwrap()).unwrap();
    let (_store, startup, _) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed));
}

#[test]
fn a_missing_directory_is_an_error() {
    let dir = TempDir::new("missing");
    let missing = dir.file("nope");
    assert!(matches!(
        FileStore::open(&missing),
        Err(StoreError::Io(io::ErrorKind::NotFound))
    ));
}

#[test]
fn only_one_store_can_hold_the_lock() {
    let dir = TempDir::new("lock");
    let (_store, _, _) = open(&dir);
    assert!(matches!(
        FileStore::open(dir.path()),
        Err(StoreError::Refused(_))
    ));
}

/// T25: a full disk mid-write leaves the stored state intact and no
/// temporary file behind.
#[test]
fn failed_write_keeps_the_stored_state() {
    let dir = TempDir::new("disk-full");
    let (mut store, _, _) = open(&dir);
    store.save(&state("good")).unwrap();
    let before = fs::read(dir.file(STATE_FILE)).unwrap();
    let mut store = store.with_writer(Box::new(|file, bytes| {
        file.write_all(&bytes[..bytes.len() / 3])?;
        Err(io::Error::from(io::ErrorKind::StorageFull))
    }));

    let result = store.save(&state("bad"));
    assert_eq!(result, Err(StoreError::Io(io::ErrorKind::StorageFull)));
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), before);
    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains("tmp")), "{names:?}");
}

/// What a test barrier saw when it ran.
#[derive(Debug)]
struct BarrierCall {
    dir: PathBuf,
    current: PathBuf,
    current_installation: String,
    previous_installation: Option<String>,
    temp_files: Vec<String>,
}

fn installation_in(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let state: PersistedState = serde_json::from_slice(&bytes).ok()?;
    Some(state.installation_id)
}

/// A barrier that records what was on disk when it ran, then answers with
/// `result`.
fn recording_barrier(calls: &Arc<Mutex<Vec<BarrierCall>>>, result: io::Result<Flushed>) -> Barrier {
    let calls = Arc::clone(calls);
    let result = result.map_err(|e| e.kind());
    Box::new(move |dir, current| {
        let temp_files = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        calls.lock().unwrap().push(BarrierCall {
            dir: dir.to_path_buf(),
            current: current.to_path_buf(),
            current_installation: installation_in(current).unwrap_or_default(),
            previous_installation: installation_in(&dir.join(PREVIOUS_FILE)),
            temp_files,
        });
        result.map_err(io::Error::from)
    })
}

/// Integrated review of `121b42a`, finding 2: every save runs the barrier
/// exactly once, after its last rename (the new state is `state.json`, the
/// old one `state.prev.json`, no temporary file is left) and before it
/// returns.
#[test]
fn i2_barrier_runs_after_the_last_rename_before_save_returns() {
    let dir = TempDir::new("barrier-order");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (store, _, _) = open(&dir);
    let mut store = store.with_barrier(recording_barrier(&calls, Ok(Flushed::Directory)));
    assert_eq!(store.durability(), Durability::NothingSaved);

    store.save(&state("first")).unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1, "ran before save returned");
    store.save(&state("second")).unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        assert_eq!(call.dir, dir.path());
        assert_eq!(call.current, dir.file(STATE_FILE));
        assert!(call.temp_files.is_empty(), "{call:?}");
    }
    assert_eq!(calls[0].current_installation, "first");
    assert_eq!(calls[0].previous_installation, None);
    assert_eq!(calls[1].current_installation, "second");
    assert_eq!(calls[1].previous_installation.as_deref(), Some("first"));
    assert_eq!(store.durability(), Durability::Flushed(Flushed::Directory));
}

/// Finding 2: a barrier that could flush only the new file (Windows, when
/// the directory handle cannot be flushed) is recorded as such, not as a
/// full flush.
#[test]
fn i2_a_file_only_flush_is_recorded() {
    let dir = TempDir::new("barrier-file-only");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (store, _, _) = open(&dir);
    let mut store = store.with_barrier(recording_barrier(
        &calls,
        Ok(Flushed::FileOnly(io::ErrorKind::PermissionDenied)),
    ));
    store.save(&state("only")).unwrap();
    assert_eq!(
        store.durability(),
        Durability::Flushed(Flushed::FileOnly(io::ErrorKind::PermissionDenied))
    );
}

/// Finding 2 with the real barrier of the platform running the tests: a
/// save flushes the directory itself.
#[test]
fn i2_the_default_barrier_flushes_the_directory() {
    let dir = TempDir::new("barrier-default");
    let (mut store, _, _) = open(&dir);
    store.save(&state("first")).unwrap();
    store.save(&state("second")).unwrap();
    assert_eq!(store.durability(), Durability::Flushed(Flushed::Directory));
}

/// A barrier that fails while switched to an error, counting its calls.
#[derive(Clone, Default)]
struct BarrierSwitch {
    failing: Arc<Mutex<Option<io::ErrorKind>>>,
    calls: Arc<AtomicU32>,
}

impl BarrierSwitch {
    fn barrier(&self) -> Barrier {
        let switch = self.clone();
        Box::new(move |_, _| {
            switch.calls.fetch_add(1, Ordering::SeqCst);
            match *switch.failing.lock().unwrap() {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok(Flushed::Directory),
            }
        })
    }

    fn set_failing(&self, kind: Option<io::ErrorKind>) {
        *self.failing.lock().unwrap() = kind;
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

fn temp_files(dir: &TempDir) -> Vec<String> {
    fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("tmp"))
        .collect()
}

/// Integrated review of `121b42a`, finding 3: a flush that fails after the
/// new file is already `state.json` does not report the save as failed,
/// because it took effect. The store records that durability is not
/// confirmed, and the next save checks the flush again before changing
/// anything: while it still fails, that save is refused and the files stay
/// as the last successful save left them.
#[test]
fn i3_a_failed_flush_after_the_rename_reports_the_save_that_took_effect() {
    let dir = TempDir::new("flush-fails");
    let switch = BarrierSwitch::default();
    let (store, _, _) = open(&dir);
    let mut store = store.with_barrier(switch.barrier());
    store.save(&state("first")).unwrap();

    switch.set_failing(Some(io::ErrorKind::Other));
    assert_eq!(
        store.save(&state("second")),
        Ok(()),
        "the new state is in place, so the save took effect"
    );
    assert_eq!(
        installation_in(&dir.file(STATE_FILE)).as_deref(),
        Some("second")
    );
    assert_eq!(
        installation_in(&dir.file(PREVIOUS_FILE)).as_deref(),
        Some("first")
    );
    assert_eq!(
        store.durability(),
        Durability::Unconfirmed(io::ErrorKind::Other)
    );

    let current = fs::read(dir.file(STATE_FILE)).unwrap();
    let previous = fs::read(dir.file(PREVIOUS_FILE)).unwrap();
    let calls = switch.calls();
    assert_eq!(
        store.save(&state("third")),
        Err(StoreError::Io(io::ErrorKind::Other)),
        "the flush is checked again first, and still fails"
    );
    assert_eq!(switch.calls(), calls + 1);
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), current);
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), previous);
    assert!(temp_files(&dir).is_empty());
    assert_eq!(
        store.durability(),
        Durability::Unconfirmed(io::ErrorKind::Other)
    );

    switch.set_failing(None);
    store.save(&state("third")).unwrap();
    assert_eq!(store.durability(), Durability::Flushed(Flushed::Directory));
    drop(store);
    let (_store, startup, source) = open(&dir);
    assert_eq!(source, Source::Current);
    assert_eq!(restored(startup).installation_id, "third");
    assert_eq!(
        installation_in(&dir.file(PREVIOUS_FILE)).as_deref(),
        Some("second")
    );
}

/// Finding 3 through the watchdog: a disconnect whose save is in place but
/// whose flush failed is accepted, as it is what a restart finds. The next
/// change is refused while the flush still fails, and storage shows failed.
#[test]
fn i3_disconnect_in_place_despite_a_failed_flush_is_accepted_and_kept() {
    let dir = TempDir::new("watchdog-flush-fails");
    let office = TunnelId::new("office").unwrap();
    {
        let mut wd = watchdog(&dir);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: office.clone(),
        });
    }
    let switch = BarrierSwitch::default();
    switch.set_failing(Some(io::ErrorKind::Other));
    let (store, startup, _) = open(&dir);
    let mut wd = Watchdog::new(
        platform(),
        FakeClock::new(1_767_225_600_000),
        Timing::default(),
        Box::new(store.with_barrier(switch.barrier())),
        startup,
    );
    let outcome = wd.handle(&Request::Disconnect {
        tunnel_id: office.clone(),
    });
    assert!(
        matches!(outcome, Outcome::Ok(_)),
        "the disconnect is on disk; refusing it would contradict a restart: {outcome:?}"
    );
    assert_eq!(
        wd.snapshot().tunnels[0].intent.desired,
        DesiredState::Disconnected
    );

    let outcome = wd.handle(&Request::SetStartWhenStoppedExternally {
        tunnel_id: office.clone(),
        enabled: true,
    });
    let Outcome::Err(error) = outcome else {
        panic!("a change was accepted while the flush still fails");
    };
    assert_eq!(error.code, ErrorCode::StorageUnavailable);
    assert_eq!(error.reason, Some(ErrorReason::StorageSaveFailed));
    assert_eq!(wd.snapshot().storage, StorageHealth::Failed);
    drop(wd);

    let (_store, startup, _) = open(&dir);
    let stored = restored(startup);
    assert_eq!(stored.tunnels[0].intent.desired, DesiredState::Disconnected);
    assert!(!stored.tunnels[0].intent.start_when_stopped_externally);
}

/// Renames for real until `crash_after` renames are done; every later one
/// fails, as nothing reaches the disk after a crash (the save's own attempt
/// to undo its first rename included).
fn crashing_renamer(crash_after: u32) -> Renamer {
    let mut done = 0;
    Box::new(move |from, to| {
        if done >= crash_after {
            return Err(io::Error::other("simulated crash"));
        }
        done += 1;
        fs::rename(from, to)
    })
}

/// `state.prev.json` holds a valid save (`older`) and `state.json` a newer
/// one that is truncated. Returns the previous file's bytes.
fn damaged_current_with_a_valid_previous(dir: &TempDir) -> Vec<u8> {
    let (mut store, _, _) = open(dir);
    store.save(&state("older")).unwrap();
    store.save(&state("newer")).unwrap();
    drop(store);
    let full = fs::read(dir.file(STATE_FILE)).unwrap();
    fs::write(dir.file(STATE_FILE), &full[..full.len() / 2]).unwrap();
    fs::read(dir.file(PREVIOUS_FILE)).unwrap()
}

/// Integrated review of `121b42a`, finding 4: after a damaged current file
/// was restored from the previous one, the first save must not rotate the
/// damaged file over the only valid copy. A crash at any point of that save
/// leaves either the new state as current, or the damaged current file with
/// the valid previous one, which restores conservatively again. Never
/// nothing usable.
#[test]
fn i4_a_crash_in_the_first_save_after_a_backup_restore_keeps_the_backup() {
    for crash_after in 0..=2 {
        let dir = TempDir::new(&format!("damaged-crash-{crash_after}"));
        let older = damaged_current_with_a_valid_previous(&dir);
        let (store, startup, source) = open(&dir);
        assert_eq!(source, Source::Previous);
        assert_eq!(restored_from_backup(startup).installation_id, "older");

        let mut store = store.with_renamer(crashing_renamer(crash_after));
        let result = store.save(&state("conservative"));
        drop(store);

        let (_store, startup, source) = open(&dir);
        match startup {
            Startup::Restored(state) => {
                assert_eq!(source, Source::Current, "crash after {crash_after}");
                assert_eq!(state.installation_id, "conservative");
                assert_eq!(result, Ok(()), "crash after {crash_after}");
            }
            Startup::RestoredFromBackup(state) => {
                assert_eq!(state.installation_id, "older", "crash after {crash_after}");
                assert!(result.is_err(), "crash after {crash_after}");
            }
            other => {
                panic!("a crash after {crash_after} renames lost the last valid state: {other:?}")
            }
        }
        assert_eq!(
            fs::read(dir.file(PREVIOUS_FILE)).unwrap(),
            older,
            "crash after {crash_after}: the previous file stays the last valid save"
        );
    }
}

/// Finding 4: once the damaged file is replaced by a valid save, saves
/// rotate as usual again.
#[test]
fn i4_after_the_damaged_file_is_replaced_saves_rotate_as_usual() {
    let dir = TempDir::new("damaged-then-rotate");
    let older = damaged_current_with_a_valid_previous(&dir);
    let (mut store, startup, _) = open(&dir);
    restored_from_backup(startup);

    store.save(&state("first")).unwrap();
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), older);
    assert_eq!(
        installation_in(&dir.file(STATE_FILE)).as_deref(),
        Some("first")
    );
    store.save(&state("second")).unwrap();
    assert_eq!(
        installation_in(&dir.file(PREVIOUS_FILE)).as_deref(),
        Some("first")
    );
    assert_eq!(
        installation_in(&dir.file(STATE_FILE)).as_deref(),
        Some("second")
    );
    assert!(temp_files(&dir).is_empty());
}

/// Finding 4: a first save that fails before replacing the damaged file
/// changes nothing, and the next one still does not rotate it.
#[test]
fn i4_a_failed_first_save_still_does_not_rotate_the_damaged_file() {
    let dir = TempDir::new("damaged-full");
    let older = damaged_current_with_a_valid_previous(&dir);
    let damaged = fs::read(dir.file(STATE_FILE)).unwrap();
    let (store, startup, _) = open(&dir);
    restored_from_backup(startup);
    let full = Arc::new(Mutex::new(true));
    let disk_full = Arc::clone(&full);
    let mut store = store.with_writer(Box::new(move |file, bytes| {
        if *disk_full.lock().unwrap() {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        file.write_all(bytes)
    }));

    assert_eq!(
        store.save(&state("first")),
        Err(StoreError::Io(io::ErrorKind::StorageFull))
    );
    assert_eq!(fs::read(dir.file(STATE_FILE)).unwrap(), damaged);
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), older);

    *full.lock().unwrap() = false;
    store.save(&state("first")).unwrap();
    assert_eq!(fs::read(dir.file(PREVIOUS_FILE)).unwrap(), older);
    assert_eq!(
        installation_in(&dir.file(STATE_FILE)).as_deref(),
        Some("first")
    );
}

#[test]
fn symlinked_state_file_is_refused() {
    let dir = TempDir::new("symlink");
    let target = dir.file("elsewhere.json");
    fs::write(&target, b"{}").unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&target, dir.file(STATE_FILE));
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&target, dir.file(STATE_FILE));
    if let Err(e) = linked {
        // Creating symlinks needs a privilege on Windows that test machines
        // may not grant; nothing to check without one.
        eprintln!("symlink not created ({e}); skipping");
        return;
    }
    let (mut store, startup, source) = open(&dir);
    assert!(matches!(startup, Startup::StorageFailed));
    assert_eq!(source, Source::Unusable);
    assert!(matches!(
        store.save(&state("x")),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(
        fs::read(&target).unwrap(),
        b"{}",
        "the link target is untouched"
    );
}

fn platform() -> FakePlatform {
    let platform = FakePlatform::new();
    platform.set_capabilities(AdapterCapabilities {
        start_stop: Capability::Supported,
        bound_icmp_probe: Capability::Supported,
        ..AdapterCapabilities::UNKNOWN
    });
    platform.add_tunnel(
        TunnelBinding::new("office").unwrap(),
        "Office",
        OsState::Connected,
    );
    platform
}

fn watchdog(dir: &TempDir) -> Watchdog<FakePlatform, FakeClock> {
    let (store, startup, _) = open(dir);
    Watchdog::new(
        platform(),
        FakeClock::new(1_767_225_600_000),
        Timing::default(),
        Box::new(store),
        startup,
    )
}

/// T17 through real files: the user's disconnect survives a restart.
#[test]
fn intent_survives_a_restart_through_the_file_store() {
    let dir = TempDir::new("watchdog");
    let office = TunnelId::new("office").unwrap();
    {
        let mut wd = watchdog(&dir);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: office.clone(),
        });
        let outcome = wd.handle(&Request::Disconnect {
            tunnel_id: office.clone(),
        });
        assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    }
    let mut wd = watchdog(&dir);
    let snapshot = wd.snapshot();
    assert_eq!(snapshot.storage, StorageHealth::Ok);
    assert_eq!(
        snapshot.tunnels[0].intent.desired,
        DesiredState::Disconnected
    );
}

/// T25 through real files: when the disk is full the disconnect is refused
/// and the stored intent is unchanged.
#[test]
fn full_disk_refuses_intent_changes() {
    let dir = TempDir::new("watchdog-full");
    let office = TunnelId::new("office").unwrap();
    {
        let mut wd = watchdog(&dir);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: office.clone(),
        });
    }
    let (store, startup, _) = open(&dir);
    let store = store.with_writer(Box::new(|_, _| {
        Err(io::Error::from(io::ErrorKind::StorageFull))
    }));
    let clock = FakeClock::new(1_767_225_600_000);
    let mut wd = Watchdog::new(
        platform(),
        clock,
        Timing::default(),
        Box::new(store),
        startup,
    );
    let outcome = wd.handle(&Request::Disconnect {
        tunnel_id: office.clone(),
    });
    let Outcome::Err(error) = outcome else {
        panic!("disconnect was accepted without being stored");
    };
    assert_eq!(error.code, ErrorCode::StorageUnavailable);
    assert_eq!(error.reason, Some(ErrorReason::StorageSaveFailed));
    assert_eq!(wd.snapshot().storage, StorageHealth::Failed);
    wd.clock().advance(Duration::from_secs(1)).unwrap();
    wd.tick();
    drop(wd);

    let (_store, startup, _) = open(&dir);
    let state = restored(startup);
    assert_eq!(state.tunnels[0].intent.desired, DesiredState::Connected);
}

/// Review finding 3 through real files: the save that recorded a reserved
/// notification is damaged, and the previous file still says `pending`. The
/// watchdog restores conservatively: the alert is never sent again and no
/// automatic control resumes.
#[test]
fn f03_damaged_current_file_restores_the_backup_conservatively() {
    let dir = TempDir::new("backup-reserved");
    let office = TunnelId::new("office").unwrap();
    {
        let mut wd = watchdog(&dir);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: office.clone(),
        });
    }
    let (mut store, startup, _) = open(&dir);
    let mut state = restored(startup);
    state.tunnels[0].intent.auto_recovery_enabled = true;
    state.tunnels[0].baseline_passed = true;
    state.tunnels[0].incident = Some(IncidentRecord {
        incident_id: IncidentId::new("inc-000001").unwrap(),
        opened_at_unix_ms: 1_767_225_600_000,
        first_recovery: FirstRecovery::Failed,
        backoff_step: 0,
        counted_from_step: 0,
        retries_exhausted: false,
        exhausted_notice: Default::default(),
        manual_last: false,
        resume_uncounted: false,
        wait_from_unix_ms: None,
        last_round_end_unix_ms: None,
        next_retry_unix_ms: None,
        notification: NotificationState::Pending,
        notification_message_id: None,
        cancel_requested: false,
    });
    store.save(&state).unwrap();
    if let Some(incident) = &mut state.tunnels[0].incident {
        incident.notification = NotificationState::Reserved;
    }
    store.save(&state).unwrap();
    drop(store);
    let full = fs::read(dir.file(STATE_FILE)).unwrap();
    fs::write(dir.file(STATE_FILE), &full[..full.len() / 2]).unwrap();

    let mut wd = watchdog(&dir);
    let snapshot = wd.snapshot();
    let tunnel = &snapshot.tunnels[0];
    assert_eq!(snapshot.storage, StorageHealth::Ok);
    assert_eq!(
        tunnel.incident.as_ref().unwrap().notification,
        NotificationState::Unknown
    );
    assert!(!tunnel.intent.auto_recovery_enabled);
    assert!(wd.notifications_due().is_empty());
    drop(wd);

    let (_store, startup, source) = open(&dir);
    assert_eq!(source, Source::Current, "the conservative state was saved");
    let stored = restored(startup);
    assert_eq!(
        stored.tunnels[0].incident.as_ref().unwrap().notification,
        NotificationState::Unknown
    );
}

/// Review of wave two, P3-1, migration through real files: a state file
/// written before tunnels had event-log labels loads as it is, and the
/// watchdog gives each tunnel a label and saves it at once.
#[test]
fn a_state_file_from_before_log_labels_loads_and_gains_them() {
    let dir = TempDir::new("labels");
    let office = TunnelId::new("office").unwrap();
    {
        let mut wd = watchdog(&dir);
        wd.handle(&Request::DiscoverTunnels);
        wd.handle(&Request::RegisterTunnel {
            tunnel_id: office.clone(),
        });
        assert!(wd.log_label(&office).is_some());
    }
    // Rewrite the file as an older watchdog wrote it: no `log_label`.
    let path = dir.file(STATE_FILE);
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for tunnel in json["tunnels"].as_array_mut().unwrap() {
        assert!(
            tunnel
                .as_object_mut()
                .unwrap()
                .remove("log_label")
                .is_some()
        );
    }
    fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

    let wd = watchdog(&dir);
    assert_eq!(wd.storage_health(), StorageHealth::Ok);
    let label = wd.log_label(&office).unwrap().to_owned();
    drop(wd);
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["tunnels"][0]["log_label"], label.as_str());
    assert_eq!(watchdog(&dir).log_label(&office), Some(label.as_str()));
}

/// A state file exactly as the build installed for day 1 of the Windows soak
/// (`00ad525`) wrote it: before probe timing, the per-tunnel notification
/// switch and host settings. Invented IDs and a documentation address.
const DAY_1_STATE: &str = r#"{
  "schema_version": 1,
  "installation_id": "wgal-19b7a2c4e00",
  "next_incident": 3,
  "monitoring_paused": false,
  "tunnels": [
    {
      "tunnel_id": "office",
      "binding": "office",
      "display_name": "Office",
      "intent": {
        "desired": "connected",
        "monitoring_enabled": false,
        "auto_recovery_enabled": true,
        "start_when_stopped_externally": false
      },
      "recovery_target": {
        "address": "192.0.2.1",
        "probe": {
          "kind": "icmp"
        }
      },
      "observe_targets": [],
      "baseline_passed": true,
      "external": null,
      "binding_lost": false,
      "generation": 41,
      "incident": {
        "incident_id": "inc-000002",
        "opened_at_unix_ms": 1767225000000,
        "first_recovery": "failed",
        "backoff_step": 1,
        "last_round_end_unix_ms": 1767225100000,
        "next_retry_unix_ms": null,
        "notification": "sent",
        "notification_message_id": 77,
        "cancel_requested": false
      },
      "resume": null,
      "resume_until_unix_ms": null,
      "log_label": "t-0a1b2c3d"
    }
  ]
}"#;

/// Wave three adds probe timing, the per-tunnel notification switch and the
/// host settings to the stored state. A file from before them loads as it
/// is, with the plan's timing, notifications on and automatic host settings,
/// and is not rewritten until something changes; the first save adds the
/// new fields and leaves every other one as it was.
#[test]
fn a_state_file_from_the_day_1_build_loads_unchanged() {
    let dir = TempDir::new("day-1");
    let path = dir.file(STATE_FILE);
    fs::write(&path, DAY_1_STATE).unwrap();
    let office = TunnelId::new("office").unwrap();

    let mut wd = watchdog(&dir);
    assert_eq!(wd.storage_health(), StorageHealth::Ok);
    assert_eq!(wd.host_settings(), &HostSettings::default());
    assert_eq!(wd.log_label(&office), Some("t-0a1b2c3d"));
    let snapshot = wd.snapshot();
    let tunnel = &snapshot.tunnels[0];
    assert_eq!(tunnel.probe, ProbeTiming::DEFAULT);
    // Owner decision of 2026-11-04: a tunnel from before gets the default
    // retry, at most 100 more rounds a minute apart.
    assert_eq!(tunnel.retry, RecoveryRetry::DEFAULT);
    assert_eq!(tunnel.freshness_secs, 35);
    assert!(tunnel.notifications_enabled);
    assert!(!tunnel.intent.monitoring_enabled);
    assert!(tunnel.intent.auto_recovery_enabled);
    let incident = tunnel.incident.as_ref().unwrap();
    assert_eq!(incident.incident_id.as_str(), "inc-000002");
    assert_eq!(incident.notification, NotificationState::Sent);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        DAY_1_STATE,
        "not rewritten"
    );

    let outcome = wd.handle(&Request::SetTunnelNotifications {
        tunnel_id: office.clone(),
        enabled: false,
    });
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    drop(wd);
    let mut stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let state = stored.as_object_mut().unwrap();
    assert_eq!(state.remove("mcp_enabled").unwrap(), false, "MCP stays off");
    assert_eq!(
        state.remove("mcp_telegram_enabled").unwrap(),
        false,
        "agents may not set Telegram"
    );
    assert_eq!(
        state.remove("host").unwrap(),
        serde_json::json!({ "workers": null, "time_zone": null })
    );
    let record = state["tunnels"][0].as_object_mut().unwrap();
    assert_eq!(
        record.remove("probe").unwrap(),
        serde_json::json!({
            "interval_secs": 15,
            "timeout_secs": 2,
            "retries": 2,
            "min_fault_secs": 60
        })
    );
    assert_eq!(record.remove("notifications_enabled").unwrap(), false);
    assert_eq!(
        record.remove("retry").unwrap(),
        serde_json::json!({ "limit": 100, "interval_secs": 60 })
    );
    assert_eq!(
        record.remove("recovered_notices").unwrap(),
        serde_json::json!([])
    );
    assert_eq!(
        record.remove("auto_recovery_after_baseline").unwrap(),
        serde_json::Value::Null
    );
    let incident = record["incident"].as_object_mut().unwrap();
    assert_eq!(incident.remove("counted_from_step").unwrap(), 0);
    assert_eq!(incident.remove("retries_exhausted").unwrap(), false);
    assert_eq!(incident.remove("manual_last").unwrap(), false);
    assert_eq!(incident.remove("resume_uncounted").unwrap(), false);
    // Migrated on the first save: the start of the incident's wait, which a
    // state from then has only as the end of its last round (Codex P2-N60,
    // P2-N61).
    let ended = incident["last_round_end_unix_ms"].clone();
    assert!(ended.is_number());
    assert_eq!(incident.remove("wait_from_unix_ms").unwrap(), ended);
    assert_eq!(
        incident.remove("exhausted_notice").unwrap(),
        serde_json::json!({ "state": "not_eligible", "cancel_requested": false, "message_id": null })
    );
    let day_1: serde_json::Value = serde_json::from_str(DAY_1_STATE).unwrap();
    assert_eq!(stored, day_1, "every field from before is kept as it was");

    let mut wd = watchdog(&dir);
    assert_eq!(wd.storage_health(), StorageHealth::Ok);
    assert!(!wd.snapshot().tunnels[0].notifications_enabled);
}
