//! The service's construction, serving, watching and stopping, with a
//! temporary folder, the fake platform and a test pipe
//! (`\\.\pipe\wgal-test-host-<pid>-<n>`, never the watchdog's). Local tests:
//! no SCM, no service, no tunnel.

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use wgal_core::{
    AdapterError, DiscoveredBinding, ProbeOutcome, ProbeRequest, ResolvedInterface, RouteEvidence,
    TunnelBinding,
};
use wgal_platform::windows::{
    PIPE_CLIENT_ACCESS, PIPE_CLIENT_QOS, PipeListener, TokenIdentity, current_identity,
};
use wgal_protocol::{
    AdapterCapabilities, FrameError, OsState, ResponseEnvelope, TargetAddress, codec,
};
use wgal_testkit::FakePlatform;

use super::*;
use crate::notify::Delivery;
use crate::windows::access::ACCESS_FILE;
use crate::windows::log::SERVICE_LOG;
use crate::windows::server::ServerLimits;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh folder standing in for `<ProgramData>\WGAL`, removed when
/// dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("wgal-host-{}-{name}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn layout(&self) -> Layout {
        Layout::at(&self.0)
    }

    fn access(&self, text: &str) {
        fs::write(self.0.join(ACCESS_FILE), text).unwrap();
    }

    fn log(&self) -> String {
        fs::read_to_string(self.0.join(SERVICE_LOG)).unwrap_or_default()
    }

    fn events(&self) -> String {
        fs::read_to_string(self.0.join(EVENTS_DIR).join("events.jsonl")).unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Offline;

impl Transport for Offline {
    fn send_message(
        &self,
        _: &wgal_protocol::BotToken,
        _: &wgal_protocol::ChatId,
        _: &str,
    ) -> Delivery {
        Delivery::NotSent
    }
}

fn me() -> TokenIdentity {
    current_identity().unwrap()
}

fn elevated_admin(me: &TokenIdentity) -> bool {
    me.elevated && me.administrator
}

fn pipe_name() -> String {
    format!(
        r"\\.\pipe\wgal-test-host-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// Binds a test pipe with the watchdog's DACL plus full access for this
/// process, which must add the pipe's later instances (the service has that
/// as SYSTEM).
fn test_bind(name: &str) -> impl FnMut(AccessList) -> Result<Server, ServerError> + use<> {
    let name = name.to_owned();
    let sid = me().sid;
    move |access: AccessList| {
        let sddl = format!("{}(A;;GA;;;{sid})", access.pipe_dacl());
        Server::bind(&name, &sddl, access, ServerLimits::DEFAULT)
    }
}

/// Short times for tests: a health check every 50 ms, waiting 500 ms (3
/// failures end the service), a new pipe attempt every 50 ms, and a stop
/// given up after 2 s.
fn quick() -> Limits {
    Limits {
        health: Health {
            every: Duration::from_millis(50),
            failures: 3,
            deadline: Duration::from_millis(500),
        },
        rebind: [Duration::from_millis(50); 5],
        server_stop: Duration::from_secs(5),
        loop_stop: Duration::from_secs(1),
        loop_margin: Duration::from_secs(1),
    }
}

struct Started {
    hosted: Hosted,
    controls: Sender<Control>,
    inbox: Receiver<Control>,
    name: String,
}

fn start_with<A: PlatformAdapter + 'static>(
    scratch: &Scratch,
    adapter: A,
    name: &str,
) -> Result<Started, StartError> {
    let (controls, inbox) = mpsc::channel();
    let hosted = start(
        &scratch.layout(),
        Protection::Unchecked,
        adapter,
        Box::new(Offline),
        test_bind(name),
        controls.clone(),
        quick(),
    )?;
    Ok(Started {
        hosted,
        controls,
        inbox,
        name: name.to_owned(),
    })
}

fn start_in(scratch: &Scratch) -> Result<Started, StartError> {
    start_with(scratch, FakePlatform::new(), &pipe_name())
}

/// Asks for a snapshot over the pipe until one comes back (the pipe may not
/// be served yet), for at most 10 s.
fn snapshot_soon(name: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if is_snapshot(&ask_snapshot(name)) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Sends a snapshot request over the pipe as the GUI would and returns the
/// answer, or how the connection ended. Gives up after 10 s.
fn ask_snapshot(name: &str) -> Result<ResponseEnvelope, String> {
    let name = name.to_owned();
    let (done, answer) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| {
            let mut pipe = OpenOptions::new()
                .access_mode(PIPE_CLIENT_ACCESS)
                .security_qos_flags(PIPE_CLIENT_QOS)
                .open(&name)
                .map_err(|e| e.to_string())?;
            let request = codec::encode_request(&RequestEnvelope {
                origin: None,
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("r-host").unwrap(),
                request: Request::GetSnapshot,
            })
            .unwrap();
            codec::write_frame(&mut pipe, &request).map_err(|e| e.to_string())?;
            match codec::read_frame(&mut pipe) {
                Ok(frame) => Ok(codec::decode_response(&frame).unwrap()),
                Err(FrameError::Closed) => Err("closed".to_owned()),
                Err(e) => Err(e.to_string()),
            }
        })();
        let _ = done.send(result);
    });
    answer
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| Err("no answer in time".to_owned()))
}

fn is_snapshot(answer: &Result<ResponseEnvelope, String>) -> bool {
    matches!(
        answer,
        Ok(ResponseEnvelope {
            outcome: Outcome::Ok(Response::Snapshot { .. }),
            ..
        })
    )
}

#[test]
fn an_untrusted_or_missing_folder_stops_the_start_with_nothing_written() {
    let scratch = Scratch::new("untrusted");
    let (controls, _inbox) = mpsc::channel();
    // A folder this account created is not owned and closed by SYSTEM and
    // Administrators alone.
    let refused = start(
        &scratch.layout(),
        Protection::AdminOnly,
        FakePlatform::new(),
        Box::new(Offline),
        test_bind(&pipe_name()),
        controls.clone(),
        quick(),
    )
    .unwrap_err();
    assert!(matches!(
        refused,
        StartError::Folder(AccessError::NotProtected)
    ));
    assert_eq!(refused.exit_code(), EXIT_FOLDER);
    assert_eq!(
        refused.to_string(),
        "the folder WGAL is owned or changeable by someone other than SYSTEM and Administrators"
    );
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);

    let missing = start(
        &Layout::at(&scratch.0.join("nope")),
        Protection::Unchecked,
        FakePlatform::new(),
        Box::new(Offline),
        test_bind(&pipe_name()),
        controls,
        quick(),
    )
    .unwrap_err();
    assert!(matches!(missing, StartError::Folder(AccessError::Missing)));
    assert_eq!(missing.exit_code(), EXIT_FOLDER);
}

#[test]
fn subfolders_are_created_and_one_that_is_not_a_folder_stops_the_start() {
    let scratch = Scratch::new("subfolder");
    fs::write(scratch.0.join(EVENTS_DIR), b"").unwrap();
    let refused = start_in(&scratch).map(|_| ()).unwrap_err();
    assert!(matches!(
        refused,
        StartError::Subfolder {
            name: EVENTS_DIR,
            error: AccessError::WrongKind
        }
    ));
    assert_eq!(refused.exit_code(), EXIT_SUBFOLDER);
    assert!(scratch.0.join(STATE_DIR).is_dir());
    assert!(scratch.0.join(NOTIFY_DIR).is_dir());
    assert!(
        scratch
            .log()
            .contains("not started: the folder WGAL\\events is not a folder")
    );
}

#[test]
fn the_service_answers_through_the_pipe_and_stops_cleanly() {
    let me = me();
    let scratch = Scratch::new("serve");
    scratch.access(&format!(
        "# written by the test\nuser {}\ntunnel office\n",
        me.sid
    ));
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    assert!(is_snapshot(&ask_snapshot(&name)));
    assert_eq!(hosted.server().unwrap().stats().admitted, 1);

    controls.send(Control::Wake).unwrap();
    controls.send(Control::Stop).unwrap();
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::Requested);
    let mut pending = 0;
    assert_eq!(
        hosted.finish(ending, &mut || pending += 1),
        Finished {
            exit_code: None,
            clean: true
        }
    );

    let log = scratch.log();
    for line in [
        "starting",
        "access list: 1 listed users, 1 tunnels",
        "no stored state; starting empty",
        "running",
        "resumed from sleep",
        "stopping as asked",
        " stopped\n",
    ] {
        assert!(log.contains(line), "{line:?} missing from {log}");
    }
    assert!(!log.contains(&me.sid), "the log names an account");
    let events = scratch.events();
    assert!(events.contains("\"event\":\"started\""), "{events}");
    assert!(events.contains("\"event\":\"stopped\""), "{events}");
    assert!(scratch.0.join(STATE_DIR).join("state.lock").is_file());
    // The pipe is gone with the service.
    assert!(!is_snapshot(&ask_snapshot(&name)));
}

/// A network change reaches the loop as `on_network_change` and is written
/// to `service.log`; the service goes on serving.
#[test]
fn a_network_change_restarts_the_grace_period_and_is_logged() {
    let scratch = Scratch::new("network");
    scratch.access(&format!("user {}\ntunnel office\n", me().sid));
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    controls.send(Control::NetworkChange).unwrap();
    controls.send(Control::NetworkChange).unwrap();
    controls.send(Control::Stop).unwrap();
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::Requested);
    assert!(is_snapshot(&ask_snapshot(&name)));
    assert!(hosted.finish(ending, &mut || ()).clean);
    let log = scratch.log();
    assert_eq!(
        log.matches("the network changed; the grace period starts again")
            .count(),
        2,
        "{log}"
    );
}

/// The real registration for interface notices, made and cancelled with the
/// service; no network change is caused. Ignored only because it registers
/// with the real IP Helper.
#[test]
#[ignore = "registers a real IP Helper notification; run on its own"]
fn network_changes_are_watched_while_the_service_runs() {
    let scratch = Scratch::new("network-live");
    let Started {
        mut hosted,
        controls,
        inbox,
        ..
    } = start_in(&scratch).unwrap();
    hosted.watch_network_changes();
    controls.send(Control::Stop).unwrap();
    let ending = hosted.watch(&inbox);
    let started = Instant::now();
    assert!(hosted.finish(ending, &mut || ()).clean);
    assert!(started.elapsed() < Duration::from_secs(5));
    let log = scratch.log();
    assert!(log.contains("watching network changes"), "{log}");
}

#[test]
fn a_bad_access_list_fails_closed_and_the_service_still_runs() {
    let me = me();
    let scratch = Scratch::new("badlist");
    scratch.access("user QQZX-not-a-sid\ntunnel office\n");
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    let answer = ask_snapshot(&name);
    if elevated_admin(&me) {
        assert!(is_snapshot(&answer));
    } else {
        // Nobody is listed: this account is refused before anything is read.
        assert_eq!(answer.unwrap_err(), "closed");
        assert_eq!(hosted.server().unwrap().stats().refused, 1);
    }
    controls.send(Control::Stop).unwrap();
    let ending = hosted.watch(&inbox);
    assert_eq!(hosted.finish(ending, &mut || {}).exit_code, None);
    let log = scratch.log();
    assert!(
        log.contains(
            "access list refused, so no users are listed and no tunnels allowed: \
             access.txt line 1: not a SID in canonical form"
        ),
        "{log}"
    );
    assert!(!log.contains("QQZX"), "the log quotes the access file");
}

#[test]
fn a_store_another_watchdog_holds_stops_the_start() {
    let scratch = Scratch::new("store");
    fs::create_dir(scratch.0.join(STATE_DIR)).unwrap();
    let held = FileStore::open(&scratch.0.join(STATE_DIR)).unwrap();
    let refused = start_in(&scratch).map(|_| ()).unwrap_err();
    assert!(matches!(refused, StartError::Store(_)));
    assert_eq!(refused.exit_code(), EXIT_STORE);
    assert!(scratch.log().contains(
        "not started: the state store could not be opened: storage refused: another watchdog \
         holds the state lock"
    ));
    drop(held);
}

/// Another account created the pipe first, as it could while the service
/// was down or being upgraded. The service starts anyway, the loop answers,
/// and the pipe is served as soon as the name is free.
#[test]
fn a_squatted_pipe_name_is_tried_again_while_the_loop_runs() {
    let me = me();
    let scratch = Scratch::new("squat");
    scratch.access(&format!("user {}\n", me.sid));
    let name = pipe_name();
    let squatter = PipeListener::bind(&name, &format!("O:{0}D:P(A;;GA;;;{0})", me.sid), 2).unwrap();
    let Started {
        mut hosted,
        controls,
        inbox,
        ..
    } = start_with(&scratch, FakePlatform::new(), &name).unwrap();
    assert!(hosted.server().is_none());
    // Monitoring and recovery go on without the pipe.
    assert_eq!(
        check(&hosted.client(), Duration::from_secs(2)),
        Check::Answered
    );

    let helper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        drop(squatter);
        let served = snapshot_soon(&name);
        controls.send(Control::Stop).unwrap();
        served
    });
    assert_eq!(hosted.watch(&inbox), Ending::Requested);
    assert!(helper.join().unwrap(), "the pipe was never served");
    assert!(hosted.finish(Ending::Requested, &mut || {}).clean);
    let log = scratch.log();
    assert!(
        log.contains(
            "the pipe could not be served (attempt 1): another process already holds the pipe \
             name; the watchdog goes on without it"
        ),
        "{log}"
    );
    assert!(log.contains("the pipe is served again after "), "{log}");
}

#[test]
fn a_loop_that_stops_answering_ends_the_service_for_a_restart() {
    let scratch = Scratch::new("dead");
    let Started {
        mut hosted, inbox, ..
    } = start_in(&scratch).unwrap();
    hosted.kill_loop();
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::LoopDied);
    assert_eq!(
        hosted.finish(ending, &mut || {}),
        Finished {
            exit_code: Some(EXIT_LOOP_DIED),
            clean: true
        }
    );
    // FIX-04 (AR-07): a loop that is gone is dead at the first check, not
    // after three.
    let log = scratch.log();
    assert!(log.contains("the watchdog loop is not running"), "{log}");
    assert!(!log.contains("did not answer"), "{log}");
    assert!(log.contains("stopping for a restart"));
}

/// A fake platform whose discovery hangs, so the loop's owner thread hangs
/// with it (discovery runs there).
#[derive(Debug)]
struct HangingDiscovery {
    inner: FakePlatform,
    hang: Duration,
}

impl PlatformAdapter for HangingDiscovery {
    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }
    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        thread::sleep(self.hang);
        self.inner.discover()
    }
    fn check_registration(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.inner.check_registration(tunnel)
    }
    fn query_state(&self, tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        self.inner.query_state(tunnel)
    }
    fn resolve_interface(&self, tunnel: &TunnelBinding) -> Result<ResolvedInterface, AdapterError> {
        self.inner.resolve_interface(tunnel)
    }
    fn check_route(
        &self,
        interface: &ResolvedInterface,
        target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        self.inner.check_route(interface, target)
    }
    fn probe(
        &self,
        interface: &ResolvedInterface,
        request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        self.inner.probe(interface, request)
    }
    fn start(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.inner.start(tunnel)
    }
    fn stop(&self, tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        self.inner.stop(tunnel)
    }
}

/// The owner thread hangs: health checks notice it, and stopping gives up
/// on the loop within its own bound instead of waiting with it.
#[test]
fn a_hung_loop_is_noticed_and_the_stop_is_bounded() {
    let scratch = Scratch::new("hung");
    let adapter = HangingDiscovery {
        inner: FakePlatform::new(),
        hang: Duration::from_secs(20),
    };
    let Started {
        mut hosted, inbox, ..
    } = start_with(&scratch, adapter, &pipe_name()).unwrap();
    let client = hosted.client();
    thread::spawn(move || {
        let discover = RequestEnvelope {
            origin: None,
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("r-discover").unwrap(),
            request: Request::DiscoverTunnels,
        };
        client.call(&service_caller(), discover, Duration::from_secs(60))
    });
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::LoopDied);
    let started = Instant::now();
    let finished = hosted.finish(ending, &mut || {});
    // quick(): 1 s for the loop's own deadline and 1 s of margin.
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        finished,
        Finished {
            exit_code: Some(EXIT_LOOP_DIED),
            clean: false
        }
    );
    assert!(scratch.log().contains("did not finish in time"));
}

#[test]
fn a_healthy_loop_keeps_the_service_running() {
    let scratch = Scratch::new("healthy");
    let Started {
        mut hosted,
        controls,
        inbox,
        ..
    } = start_in(&scratch).unwrap();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        controls.send(Control::Stop).unwrap();
    });
    // About ten checks, every one answered.
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::Requested);
    stopper.join().unwrap();
    assert!(hosted.finish(ending, &mut || {}).clean);
    assert!(!scratch.log().contains("did not answer"));
}

#[test]
fn a_pipe_server_that_stops_on_its_own_is_served_again() {
    let me = me();
    let scratch = Scratch::new("server");
    scratch.access(&format!("user {}\n", me.sid));
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    // It answered a request, so it is served again at once.
    assert!(is_snapshot(&ask_snapshot(&name)));
    hosted.server().unwrap().stop();
    let helper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        let served = snapshot_soon(&name);
        controls.send(Control::Stop).unwrap();
        served
    });
    assert_eq!(hosted.watch(&inbox), Ending::Requested);
    assert!(helper.join().unwrap(), "the pipe was not served again");
    assert!(hosted.finish(Ending::Requested, &mut || {}).clean);
    let log = scratch.log();
    assert!(
        log.contains(
            "the pipe server stopped (0 in a row without answering a request); serving the \
             pipe again in 0 s"
        ),
        "{log}"
    );
}

/// Every server this binder makes stops right after it starts, as a pipe
/// server that fails at once would. The service must back off instead of
/// binding again and again.
#[test]
fn a_pipe_server_that_keeps_ending_backs_off() {
    let scratch = Scratch::new("ending");
    let binds = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&binds);
    let name = pipe_name();
    let sid = me().sid;
    let bind = move |access: AccessList| {
        count.fetch_add(1, Ordering::SeqCst);
        let sddl = format!("{}(A;;GA;;;{sid})", access.pipe_dacl());
        let server = Server::bind(&name, &sddl, access, ServerLimits::DEFAULT)?;
        let handle = server.handle();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            handle.stop();
        });
        Ok(server)
    };
    let limits = Limits {
        rebind: [
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(400),
            Duration::from_millis(400),
        ],
        ..quick()
    };
    let (controls, inbox) = mpsc::channel();
    let mut hosted = start(
        &scratch.layout(),
        Protection::Unchecked,
        FakePlatform::new(),
        Box::new(Offline),
        bind,
        controls.clone(),
        limits,
    )
    .unwrap();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(1200));
        controls.send(Control::Stop).unwrap();
    });
    assert_eq!(hosted.watch(&inbox), Ending::Requested);
    stopper.join().unwrap();
    assert!(hosted.finish(Ending::Requested, &mut || {}).clean);
    // 0, ~120, ~340, ~760 and ~1180 ms: a handful, not hundreds.
    let binds = binds.load(Ordering::SeqCst);
    assert!((3..=7).contains(&binds), "{binds} binds in 1.2 s");
    let log = scratch.log();
    let ended = log
        .lines()
        .filter(|line| line.contains("in a row without answering a request"))
        .count();
    assert!((1..=3).contains(&ended), "{ended} lines in {log}");
    assert!(log.contains(
        "the pipe server stopped (1 in a row without answering a request); serving the pipe \
         again in 100 ms"
    ));
}

#[test]
fn repeated_failures_are_logged_first_three_then_every_tenth() {
    let logged_runs: Vec<usize> = (1..=30).filter(|&n| logged(n)).collect();
    assert_eq!(logged_runs, [1, 2, 3, 10, 20, 30]);
    assert_eq!(seconds(Duration::from_millis(50)), "50 ms");
    assert_eq!(seconds(Duration::from_secs(60)), "60 s");
    assert_eq!(seconds(Duration::ZERO), "0 s");
}

#[test]
fn peers_become_callers_by_the_servers_own_rule() {
    let peer = |local_system, elevated_admin, listed| PeerIdentity {
        account: "S-1-5-21-1000000000-2000000000-3000000000-1001".to_owned(),
        local_system,
        elevated_admin,
        listed,
    };
    let listed = caller(&peer(false, false, true));
    assert!(listed.allowed && !listed.elevated);
    assert_eq!(
        listed.account,
        "S-1-5-21-1000000000-2000000000-3000000000-1001"
    );
    // An elevated administrator is allowed without being listed, as the
    // loop's `Caller` defines it.
    let admin = caller(&peer(false, true, false));
    assert!(admin.allowed && admin.elevated);
    let system = caller(&peer(true, true, false));
    assert!(system.allowed && system.elevated);
    // Never reaches the handler (the server refuses it), and would be
    // refused by the loop too.
    let stranger = caller(&peer(false, false, false));
    assert!(!stranger.allowed && !stranger.elevated);
}

// FIX-04 (AR-07): what the health checks conclude.

#[test]
fn checks_a_gone_loop_is_dead_at_once() {
    let mut checks = Checks::new(5);
    assert_eq!(checks.record(Check::NotRunning, 5, 3), Verdict::Gone);
}

#[test]
fn checks_too_many_waiting_with_turns_is_a_busy_loop_never_a_miss() {
    let mut checks = Checks::new(0);
    for turns in 1..=100 {
        assert_eq!(
            checks.record(Check::TooManyWaiting, turns, 3),
            Verdict::Healthy
        );
    }
}

#[test]
fn checks_too_many_waiting_without_a_turn_is_a_miss_and_three_end_it() {
    let mut checks = Checks::new(7);
    assert_eq!(
        checks.record(Check::TooManyWaiting, 7, 3),
        Verdict::Missed {
            in_a_row: 1,
            waiting: true,
            dead: false
        }
    );
    assert_eq!(
        checks.record(Check::Missed, 7, 3),
        Verdict::Missed {
            in_a_row: 2,
            waiting: false,
            dead: false
        }
    );
    assert_eq!(
        checks.record(Check::TooManyWaiting, 7, 3),
        Verdict::Missed {
            in_a_row: 3,
            waiting: true,
            dead: true
        }
    );
}

#[test]
fn checks_an_answer_or_a_turn_ends_a_run_of_misses() {
    let mut checks = Checks::new(1);
    let miss = |in_a_row| Verdict::Missed {
        in_a_row,
        waiting: false,
        dead: false,
    };
    assert_eq!(checks.record(Check::Missed, 1, 3), miss(1));
    assert_eq!(checks.record(Check::Missed, 1, 3), miss(2));
    assert_eq!(checks.record(Check::Answered, 1, 3), Verdict::Healthy);
    assert_eq!(checks.record(Check::Missed, 1, 3), miss(1));
    assert_eq!(checks.record(Check::TooManyWaiting, 2, 3), Verdict::Healthy);
    assert_eq!(checks.record(Check::Missed, 2, 3), miss(1));
}

/// FIX-04 (AR-07): the owner hangs and the requests that timed out keep
/// every place among the waiting ones, so each health check is refused for
/// too many waiting requests. That still ends the service: the owner
/// finishes no turn.
#[test]
fn a_hung_loop_whose_waiting_places_are_full_is_still_noticed() {
    let scratch = Scratch::new("hung-full");
    let adapter = HangingDiscovery {
        inner: FakePlatform::new(),
        hang: Duration::from_secs(20),
    };
    let Started {
        mut hosted, inbox, ..
    } = start_with(&scratch, adapter, &pipe_name()).unwrap();
    let client = hosted.client();
    let request = |id: &str, request| RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        request,
    };
    let hanging = client.clone();
    let discover = request("r-discover", Request::DiscoverTunnels);
    thread::spawn(move || hanging.call(&service_caller(), discover, Duration::from_secs(60)));
    // Let the owner take the discovery, then fill the waiting places with
    // requests that time out at once and keep their places.
    thread::sleep(Duration::from_millis(200));
    let mut refused = false;
    for n in 0..100 {
        let answer = client.call(
            &service_caller(),
            request(&format!("r-fill-{n}"), Request::GetSnapshot),
            Duration::from_millis(5),
        );
        if let Outcome::Err(e) = answer.outcome
            && e.reason == Some(ErrorReason::TooManyWaiting)
        {
            refused = true;
            break;
        }
    }
    assert!(refused, "the waiting places never filled");
    let ending = hosted.watch(&inbox);
    assert_eq!(ending, Ending::LoopDied);
    let finished = hosted.finish(ending, &mut || {});
    assert!(!finished.clean);
    let log = scratch.log();
    assert!(
        log.contains(
            "the watchdog loop did not answer (3 in a row; too many requests wait and it \
             finished no turn since the last check)"
        ),
        "{log}"
    );
}

/// FIX-04 (AR-07): a pipe server thread that panics reports its end like
/// one that returned, so the pipe is cleaned up and served again (after
/// the back-off of a server that answered nothing).
#[test]
fn a_pipe_server_thread_that_panics_is_noticed_and_served_again() {
    let me = me();
    let scratch = Scratch::new("server-panic");
    scratch.access(&format!("user {}\n", me.sid));
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    assert!(is_snapshot(&ask_snapshot(&name)));
    // The server that comes after this one panics; the one after that
    // serves again.
    hosted.panic_next_server();
    hosted.server().unwrap().stop();
    let helper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let served = snapshot_soon(&name);
        controls.send(Control::Stop).unwrap();
        served
    });
    assert_eq!(hosted.watch(&inbox), Ending::Requested);
    assert!(helper.join().unwrap(), "the pipe was not served again");
    assert!(hosted.finish(Ending::Requested, &mut || {}).clean);
    let log = scratch.log();
    assert!(
        log.contains(
            "the pipe server's thread panicked (1 in a row without answering a request); \
             serving the pipe again in 50 ms"
        ),
        "{log}"
    );
}

/// FIX-04, Codex review: what the service does with a panic notice ends
/// the connections the server had handed out, and then the pipe is served
/// again. Here the notice is handled while the server still runs with a
/// connection open (a real panic leaves that connection's thread running,
/// without the server joining it): stopping the server ends the
/// connection, the server's thread is joined in time, and a new server
/// answers.
#[test]
fn a_panic_notice_ends_the_open_connections_and_the_pipe_is_served_again() {
    let me = me();
    let scratch = Scratch::new("server-panic-open");
    scratch.access(&format!(
        "user {}
",
        me.sid
    ));
    let Started {
        mut hosted,
        controls,
        inbox,
        name,
    } = start_in(&scratch).unwrap();
    // A connection the server took and answered, kept open.
    let mut pipe = OpenOptions::new()
        .access_mode(PIPE_CLIENT_ACCESS)
        .security_qos_flags(PIPE_CLIENT_QOS)
        .open(&name)
        .unwrap();
    let request = codec::encode_request(&RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("r-open").unwrap(),
        request: Request::GetSnapshot,
    })
    .unwrap();
    codec::write_frame(&mut pipe, &request).unwrap();
    assert!(codec::read_frame(&mut pipe).is_ok());
    assert_eq!(hosted.server().unwrap().stats().requests, 1);

    // `Hosted` cannot move to another thread, so a guard ends the test run
    // if handling the notice does not return in time (a join that hangs
    // would otherwise hang the run instead of failing it).
    let (returned, guard) = mpsc::channel::<()>();
    let watcher = thread::spawn(move || {
        if guard.recv_timeout(Duration::from_secs(10)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("pipe_ended did not return within 10 s");
            std::process::exit(101);
        }
    });
    let started = Instant::now();
    hosted.pipe_ended(ServerEnd::Panicked);
    let took = started.elapsed();
    returned.send(()).unwrap();
    watcher.join().unwrap();
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(hosted.server().is_none());
    // The open connection was ended by the stop.
    let (done, ended) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(codec::read_frame(&mut pipe).is_err());
    });
    assert_eq!(ended.recv_timeout(Duration::from_secs(5)), Ok(true));

    // The joined server's own end notice is for a server already handled.
    while inbox.try_recv().is_ok() {}
    let helper = thread::spawn(move || {
        let served = snapshot_soon(&name);
        controls.send(Control::Stop).unwrap();
        served
    });
    assert_eq!(hosted.watch(&inbox), Ending::Requested);
    assert!(helper.join().unwrap(), "the pipe was not served again");
    assert!(hosted.finish(Ending::Requested, &mut || {}).clean);
    assert!(
        scratch
            .log()
            .contains("the pipe server's thread panicked (1 in a row"),
        "{}",
        scratch.log()
    );
}
