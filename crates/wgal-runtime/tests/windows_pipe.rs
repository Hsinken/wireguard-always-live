//! The Windows pipe server on real named pipes (plan §3.2, §9.2, T24). Each
//! test serves a pipe of its own under a unique test name
//! (`\\.\pipe\wgal-test-<pid>-<n>`), never the watchdog's, connects to it
//! from this process the way the GUI will, and touches no service, VPN or
//! network setting. These are local Win32 tests of this machine's pipe and
//! token APIs, not the on-device acceptance W6.
//!
//! Which half of the admission rule a test can see depends on the account
//! running it: a UAC-filtered or standard user is refused unless listed; an
//! elevated administrator is admitted whether listed or not. Tests that
//! depend on it check the rule for the account they run as.

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wgal_platform::windows::{
    PIPE_CLIENT_ACCESS, PIPE_CLIENT_QOS, PipeError, TokenIdentity, Win32Error, current_identity,
    object_security,
};
use wgal_protocol::{
    ErrorCode, FrameError, MAX_MESSAGE_BYTES, Outcome, PROTOCOL_VERSION, ProtocolError, Request,
    RequestEnvelope, RequestId, Response, ResponseEnvelope, codec,
};
use wgal_runtime::windows::{
    AccessList, PeerIdentity, Server, ServerError, ServerHandle, ServerLimits, ServerStats,
};

/// The longest a test waits for anything the server should do promptly.
const PROMPT: Duration = Duration::from_secs(5);
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_INVALID_OWNER: u32 = 1307;

const SNAPSHOT_FIXTURE: &str = include_str!("../../wgal-protocol/fixtures/response_snapshot.json");

static NEXT: AtomicU32 = AtomicU32::new(0);

fn pipe_name() -> String {
    format!(
        r"\\.\pipe\wgal-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

fn me() -> TokenIdentity {
    current_identity().unwrap()
}

fn elevated_admin(me: &TokenIdentity) -> bool {
    me.elevated && me.administrator
}

/// The names SDDL text read back from Windows may give the account `sid`:
/// the SID itself, and for a local account's RID 500 or 501 (the built-in
/// Administrator and Guest) the aliases `LA` and `LG`, which Windows writes
/// instead of the SID.
fn sddl_names(sid: &str) -> Vec<String> {
    let mut names = vec![sid.to_owned()];
    if sid.starts_with("S-1-5-21-") {
        if sid.ends_with("-500") {
            names.push("LA".to_owned());
        } else if sid.ends_with("-501") {
            names.push("LG".to_owned());
        }
    }
    names
}

#[test]
fn the_built_in_accounts_have_their_sddl_aliases() {
    let admin = "S-1-5-21-1-2-3-500";
    assert_eq!(sddl_names(admin), [admin, "LA"]);
    assert_eq!(sddl_names("S-1-5-21-1-2-3-501")[1], "LG");
    assert_eq!(sddl_names("S-1-5-21-1-2-3-1001"), ["S-1-5-21-1-2-3-1001"]);
    // Only an account SID: S-1-5-32-500 is no account of this kind.
    assert_eq!(sddl_names("S-1-5-32-500").len(), 1);
}

/// An access list naming `sid` (the account running the tests).
fn listing(sid: &str) -> AccessList {
    AccessList::parse(&format!("user {sid}\ntunnel office\n"))
        .expect("the test account is a user account")
}

/// The watchdog's DACL for `access`, plus full access for this process:
/// it serves the pipe and must be able to add the pipe's later instances,
/// which the service can as SYSTEM. The watchdog's descriptor also makes
/// SYSTEM the owner, which only SYSTEM can do
/// (`only_system_can_create_the_watchdog_pipe`).
fn test_sddl(access: &AccessList, me: &TokenIdentity) -> String {
    format!("{}(A;;GA;;;{})", access.pipe_dacl(), me.sid)
}

fn limits(idle: Duration) -> ServerLimits {
    ServerLimits {
        idle_timeout: idle,
        ..ServerLimits::DEFAULT
    }
}

fn snapshot_answer(id: RequestId) -> ResponseEnvelope {
    let mut answer = codec::decode_response(SNAPSHOT_FIXTURE.as_bytes()).unwrap();
    answer.request_id = Some(id);
    answer
}

/// The test handler: a snapshot request gets the snapshot fixture, anything
/// else `Accepted`.
fn answer(request: RequestEnvelope) -> ResponseEnvelope {
    match request.request {
        Request::GetSnapshot => snapshot_answer(request.request_id),
        _ => ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request.request_id),
            outcome: Outcome::Ok(Response::Accepted),
        },
    }
}

struct Running {
    name: String,
    handle: ServerHandle,
    thread: JoinHandle<Result<(), ServerError>>,
    /// Every peer the handler was called for, in order.
    peers: Arc<Mutex<Vec<PeerIdentity>>>,
    calls: Arc<AtomicUsize>,
}

/// Binds a fresh test pipe for `access` with the descriptor `sddl` builds,
/// and runs the server on a thread of its own.
fn serve(
    access: AccessList,
    sddl: impl FnOnce(&AccessList) -> String,
    limits: ServerLimits,
) -> Running {
    let name = pipe_name();
    let server = Server::bind(&name, &sddl(&access), access, limits).unwrap();
    let handle = server.handle();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let (seen, count) = (Arc::clone(&peers), Arc::clone(&calls));
    let thread = thread::spawn(move || {
        server.run(move |peer, request| {
            count.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(peer.clone());
            answer(request)
        })
    });
    Running {
        name,
        handle,
        thread,
        peers,
        calls,
    }
}

impl Running {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Stops the server; how long `run` took to return, what it returned,
    /// and the final counts.
    fn stop(self) -> (Duration, Result<(), ServerError>, ServerStats) {
        let start = Instant::now();
        self.handle.stop();
        while !self.thread.is_finished() {
            assert!(start.elapsed() < PROMPT, "the server did not stop in time");
            thread::sleep(Duration::from_millis(5));
        }
        let result = self.thread.join().unwrap();
        (start.elapsed(), result, self.handle.stats())
    }
}

/// Aborts the test run if a blocking client call has not returned within
/// `limit`, so a server that never answers fails the run instead of hanging
/// it. Dropping the guard disarms it.
struct Within {
    _disarm: mpsc::Sender<()>,
}

impl Within {
    fn new(limit: Duration, what: &'static str) -> Self {
        let (disarm, armed) = mpsc::channel::<()>();
        thread::spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = armed.recv_timeout(limit) {
                eprintln!("{what} took longer than {limit:?}");
                std::process::abort();
            }
        });
        Self { _disarm: disarm }
    }
}

/// Opens the pipe as the GUI will: exactly the client access, at
/// identification level. Retries while every instance is busy (the server
/// puts the next one up right after each accept).
fn open(name: &str) -> io::Result<File> {
    let start = Instant::now();
    loop {
        match OpenOptions::new()
            .access_mode(PIPE_CLIENT_ACCESS)
            .security_qos_flags(PIPE_CLIENT_QOS)
            .open(name)
        {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && start.elapsed() < PROMPT => {
                thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

fn request(id: &str, request: Request) -> Vec<u8> {
    codec::encode_request(&RequestEnvelope {
        origin: None,
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        request,
    })
    .unwrap()
}

fn send(pipe: &mut File, message: &[u8]) {
    codec::write_frame(pipe, message).unwrap();
}

fn read(pipe: &mut File) -> Result<Vec<u8>, FrameError> {
    let _within = Within::new(PROMPT, "reading from the pipe");
    codec::read_frame(pipe)
}

fn read_answer(pipe: &mut File) -> ResponseEnvelope {
    codec::decode_response(&read(pipe).unwrap()).unwrap()
}

/// Refused with `expected` and no reason: a request that does not decode
/// says all there is in its code.
fn refused_with(answer: &ResponseEnvelope, expected: ErrorCode) -> bool {
    matches!(
        &answer.outcome,
        Outcome::Err(ProtocolError { code, reason: None, .. }) if *code == expected
    )
}

#[test]
fn the_peer_is_this_process_as_its_token_says() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let mut pipe = open(&running.name).unwrap();
    send(&mut pipe, &request("r-who", Request::GetSnapshot));
    read_answer(&mut pipe);
    let peers = running.peers.lock().unwrap().clone();
    assert_eq!(
        peers,
        [PeerIdentity {
            account: me.sid.clone(),
            local_system: me.local_system,
            elevated_admin: elevated_admin(&me),
            listed: true,
        }]
    );
    drop(pipe);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.admitted, stats.refused, stats.requests), (1, 0, 1));
}

#[test]
fn a_listed_user_gets_a_real_snapshot_and_more_on_one_connection() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let mut pipe = open(&running.name).unwrap();
    send(&mut pipe, &request("req-0001", Request::GetSnapshot));
    assert_eq!(
        read_answer(&mut pipe),
        snapshot_answer(RequestId::new("req-0001").unwrap())
    );
    send(
        &mut pipe,
        &request("req-0002", Request::SetMonitoringPaused { paused: true }),
    );
    let answer = read_answer(&mut pipe);
    assert_eq!(answer.request_id, Some(RequestId::new("req-0002").unwrap()));
    assert_eq!(answer.outcome, Outcome::Ok(Response::Accepted));
    assert_eq!(running.calls(), 2);
    drop(pipe);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.admitted, stats.requests, stats.dropped), (1, 2, 0));
}

#[test]
fn an_unlisted_user_is_disconnected_before_anything_is_read() {
    let me = me();
    // The DACL lets this process in; the server's own check decides.
    let running = serve(
        AccessList::default(),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let mut pipe = open(&running.name).unwrap();
    if elevated_admin(&me) {
        // The other half of the rule: an elevated administrator is admitted
        // without being listed.
        send(&mut pipe, &request("r-admin", Request::GetSnapshot));
        read_answer(&mut pipe);
        assert!(!running.peers.lock().unwrap()[0].listed);
    } else {
        // Nothing was sent, and the server closes the connection.
        assert!(matches!(read(&mut pipe), Err(FrameError::Closed)));
        assert!(codec::write_frame(&mut pipe, &request("r-late", Request::GetSnapshot)).is_err());
        assert_eq!(running.calls(), 0);
    }
    drop(pipe);
    let (_, result, stats) = running.stop();
    result.unwrap();
    if elevated_admin(&me) {
        assert_eq!((stats.admitted, stats.refused), (1, 0));
    } else {
        assert_eq!((stats.admitted, stats.refused, stats.requests), (0, 1, 0));
    }
}

#[test]
fn a_client_that_hides_its_identity_is_refused_even_when_listed() {
    // SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS: the server cannot open
    // the client's token, so it cannot tell who connected.
    const ANONYMOUS_QOS: u32 = 0x0010_0000;
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let mut pipe = OpenOptions::new()
        .access_mode(PIPE_CLIENT_ACCESS)
        .security_qos_flags(ANONYMOUS_QOS)
        .open(&running.name)
        .unwrap();
    assert!(matches!(read(&mut pipe), Err(FrameError::Closed)));
    assert_eq!(running.calls(), 0);
    drop(pipe);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.admitted, stats.refused), (0, 1));
}

#[test]
fn the_watchdog_dacl_keeps_an_unlisted_unelevated_user_out() {
    let me = me();
    let running = serve(
        AccessList::default(),
        AccessList::pipe_dacl,
        ServerLimits::DEFAULT,
    );
    match open(&running.name) {
        Ok(pipe) => {
            assert!(elevated_admin(&me));
            drop(pipe);
        }
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::PermissionDenied),
    }
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!(stats.requests, 0);
}

#[test]
fn a_listed_user_gets_in_through_the_watchdog_dacl_with_client_access_only() {
    let me = me();
    // One connection at a time, so the server tries to add an instance only
    // after this client is done: a test process, unlike SYSTEM, may not add
    // instances to a pipe with the watchdog's DACL unless it is elevated.
    let running = serve(
        listing(&me.sid),
        AccessList::pipe_dacl,
        ServerLimits {
            max_connections: 1,
            ..ServerLimits::DEFAULT
        },
    );
    // More access than the DACL grants a listed user (GENERIC_WRITE carries
    // FILE_CREATE_PIPE_INSTANCE) is refused at the door.
    let wider = OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(PIPE_CLIENT_QOS)
        .open(&running.name);
    if elevated_admin(&me) {
        drop(wider);
    } else {
        assert_eq!(wider.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
    let mut pipe = open(&running.name).unwrap();
    send(&mut pipe, &request("r-dacl", Request::GetSnapshot));
    assert_eq!(
        read_answer(&mut pipe),
        snapshot_answer(RequestId::new("r-dacl").unwrap())
    );
    drop(pipe);
    // Not checked: unless elevated, this account cannot add the next
    // instance, so `run` fails or not depending on whether the stop comes
    // first (`a_server_without_the_right_to_add_instances_reports_it`).
    let _ = running.stop();
}

#[test]
fn the_pipe_is_owned_by_its_server_and_its_dacl_is_protected() {
    let me = me();
    let access = listing(&me.sid);
    let running = serve(access, |a| test_sddl(a, &me), ServerLimits::DEFAULT);
    let pipe = open(&running.name).unwrap();
    let security = object_security(&pipe).unwrap();
    // The owner is who created the pipe: this account, or Administrators
    // when an elevated administrator created it. A client can require the
    // watchdog's pipe to be owned by SYSTEM.
    assert!(security.owner == me.sid || (elevated_admin(&me) && security.owner == "S-1-5-32-544"));
    let dacl = security
        .sddl
        .split_once("D:")
        .map(|(_, dacl)| dacl)
        .unwrap();
    assert!(dacl.starts_with("P("), "the DACL is not protected: {dacl}");
    for entry in [
        "(D;;FA;;;NU)",
        "(D;;FA;;;AN)",
        "(A;;FA;;;SY)",
        "(A;;FA;;;BA)",
    ] {
        assert!(dacl.contains(entry), "missing {entry} in {dacl}");
    }
    // The listed account's client entry, under the name Windows itself
    // writes this account with: the trustee of the full-access entry that
    // `test_sddl` adds last for the same SID. On GitHub's Windows runners the
    // tests run as the built-in Administrator, written `LA`; a domain
    // account with RID 500 would be written as its SID there, so a local
    // `LA` entry cannot stand in for it. `sddl_names` only says which names
    // may stand for the SID at all.
    let written = dacl
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once("(A;;FA;;;"))
        .map(|(_, name)| name)
        .unwrap_or_else(|| panic!("the test's own entry is not last in {dacl}"));
    assert!(
        sddl_names(&me.sid).iter().any(|name| name == written),
        "the account is written as {written}"
    );
    assert!(
        dacl.contains(&format!("(A;;0x12008b;;;{written})")),
        "no client entry for the test account in {dacl}"
    );
    assert!(
        listing(&me.sid)
            .pipe_dacl()
            .contains(&format!("(A;;0x12008b;;;{})", me.sid))
    );
    assert!(security.others_may_change);
    drop(pipe);
    running.stop().1.unwrap();
}

#[test]
fn only_system_can_create_the_watchdog_pipe() {
    let me = me();
    let sddl = AccessList::default().pipe_sddl();
    let bound = Server::bind(
        &pipe_name(),
        &sddl,
        AccessList::default(),
        ServerLimits::DEFAULT,
    );
    if me.local_system {
        assert!(bound.is_ok());
    } else {
        assert!(matches!(
            bound,
            Err(ServerError::Pipe(PipeError::InvalidSecurity(Win32Error {
                code: ERROR_INVALID_OWNER
            })))
        ));
    }
}

#[test]
fn requests_that_do_not_decode_are_refused_with_their_id_and_the_connection_goes_on() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let mut pipe = open(&running.name).unwrap();
    let cases: [(&[u8], Option<&str>, ErrorCode); 5] = [
        (
            br#"{"protocol_version":1,"request_id":"r-exec","request":{"op":"execute","command":"calc.exe"}}"#,
            Some("r-exec"),
            ErrorCode::MalformedRequest,
        ),
        (
            br#"{"protocol_version":1,"request_id":"r-svc","request":{"op":"connect","tunnel_id":"WireGuardTunnel$x & calc"}}"#,
            Some("r-svc"),
            ErrorCode::MalformedRequest,
        ),
        (
            br#"{"protocol_version":1,"request_id":"r-who","caller":"S-1-5-18","request":{"op":"get_snapshot"}}"#,
            Some("r-who"),
            ErrorCode::MalformedRequest,
        ),
        (
            br#"{"protocol_version":2,"request_id":"r-v2","request":{"op":"get_snapshot"}}"#,
            Some("r-v2"),
            ErrorCode::UnsupportedVersion,
        ),
        (b"not json at all", None, ErrorCode::MalformedRequest),
    ];
    for (message, id, code) in cases {
        send(&mut pipe, message);
        let answer = read_answer(&mut pipe);
        assert_eq!(answer.request_id, id.map(|id| RequestId::new(id).unwrap()));
        assert!(refused_with(&answer, code), "{answer:?}");
    }
    assert_eq!(running.calls(), 0);
    send(&mut pipe, &request("r-ok", Request::GetSnapshot));
    assert_eq!(
        read_answer(&mut pipe),
        snapshot_answer(RequestId::new("r-ok").unwrap())
    );
    drop(pipe);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.requests, stats.dropped), (6, 0));
}

#[test]
fn an_oversized_or_empty_frame_closes_the_connection_unanswered() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    for prefix in [MAX_MESSAGE_BYTES + 1, u32::MAX, 0] {
        let mut pipe = open(&running.name).unwrap();
        pipe.write_all(&prefix.to_le_bytes()).unwrap();
        assert!(
            matches!(read(&mut pipe), Err(FrameError::Closed)),
            "prefix {prefix}"
        );
    }
    assert_eq!(running.calls(), 0);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.admitted, stats.requests, stats.dropped), (3, 0, 3));
}

#[test]
fn an_idle_or_stalled_client_is_dropped_after_the_idle_timeout() {
    let me = me();
    let idle = Duration::from_millis(300);
    let running = serve(listing(&me.sid), |a| test_sddl(a, &me), limits(idle));

    let start = Instant::now();
    let mut pipe = open(&running.name).unwrap();
    assert!(matches!(read(&mut pipe), Err(FrameError::Closed)));
    let waited = start.elapsed();
    assert!(waited >= idle && waited < PROMPT, "{waited:?}");

    // Half a length prefix, then nothing: the whole frame is late.
    let start = Instant::now();
    let mut pipe = open(&running.name).unwrap();
    pipe.write_all(&[1, 0]).unwrap();
    assert!(matches!(read(&mut pipe), Err(FrameError::Closed)));
    let waited = start.elapsed();
    assert!(waited >= idle && waited < PROMPT, "{waited:?}");

    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.idle_timeouts, stats.requests), (2, 0));
}

#[test]
fn a_second_server_cannot_take_a_name_that_is_held() {
    let me = me();
    let first = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits::DEFAULT,
    );
    let access = listing(&me.sid);
    let sddl = test_sddl(&access, &me);
    let second = Server::bind(&first.name, &sddl, access.clone(), ServerLimits::DEFAULT);
    assert!(matches!(
        second,
        Err(ServerError::Pipe(PipeError::NameInUse))
    ));
    // Still the first server answering.
    let mut pipe = open(&first.name).unwrap();
    send(&mut pipe, &request("r-first", Request::GetSnapshot));
    read_answer(&mut pipe);
    assert_eq!(first.calls(), 1);
    drop(pipe);
    let name = first.name.clone();
    first.stop().1.unwrap();
    // Once the first server is gone, the name is free again.
    Server::bind(&name, &sddl, access, ServerLimits::DEFAULT).unwrap();
}

#[test]
fn stopping_ends_the_accept_loop_and_open_connections_promptly() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        limits(Duration::from_secs(60)),
    );
    // A connection waiting for its next request, and an accept waiting for
    // the next client.
    let mut pipe = open(&running.name).unwrap();
    send(&mut pipe, &request("r-1", Request::GetSnapshot));
    read_answer(&mut pipe);
    thread::sleep(Duration::from_millis(100));
    let (took, result, stats) = running.stop();
    result.unwrap();
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert!(matches!(read(&mut pipe), Err(FrameError::Closed)));
    assert_eq!(
        (stats.admitted, stats.idle_timeouts, stats.dropped),
        (1, 0, 0)
    );
}

#[test]
fn a_client_beyond_the_connection_limit_waits_its_turn() {
    let me = me();
    let running = serve(
        listing(&me.sid),
        |a| test_sddl(a, &me),
        ServerLimits {
            max_connections: 1,
            ..ServerLimits::DEFAULT
        },
    );
    let mut first = open(&running.name).unwrap();
    send(&mut first, &request("r-first", Request::GetSnapshot));
    read_answer(&mut first);

    // The second client gets the instance waiting for the next client, but
    // nothing serves it while the first holds the only connection.
    let mut second = open(&running.name).unwrap();
    send(&mut second, &request("r-second", Request::GetSnapshot));
    let mut reader = second.try_clone().unwrap();
    let (frames, answers) = mpsc::channel();
    thread::spawn(move || {
        let _ = frames.send(codec::read_frame(&mut reader));
    });
    assert!(matches!(
        answers.recv_timeout(Duration::from_millis(300)),
        Err(RecvTimeoutError::Timeout)
    ));
    assert_eq!(running.calls(), 1);

    drop(first);
    let frame = answers.recv_timeout(PROMPT).unwrap().unwrap();
    assert_eq!(
        codec::decode_response(&frame).unwrap(),
        snapshot_answer(RequestId::new("r-second").unwrap())
    );
    drop(second);
    let (_, result, stats) = running.stop();
    result.unwrap();
    assert_eq!((stats.admitted, stats.requests), (2, 2));
}

#[test]
fn a_server_without_the_right_to_add_instances_reports_it() {
    let me = me();
    if elevated_admin(&me) {
        // Administrators may add instances under the watchdog's DACL.
        return;
    }
    let running = serve(
        listing(&me.sid),
        AccessList::pipe_dacl,
        ServerLimits::DEFAULT,
    );
    // Accepting this client makes the server add the next instance, which
    // this account may not do, and the next accept fails.
    let pipe = open(&running.name).unwrap();
    let start = Instant::now();
    while !running.thread.is_finished() {
        assert!(start.elapsed() < PROMPT, "the server kept running");
        thread::sleep(Duration::from_millis(5));
    }
    drop(pipe);
    let (_, result, _) = running.stop();
    assert!(matches!(
        result,
        Err(ServerError::Pipe(PipeError::Os(Win32Error {
            code: ERROR_ACCESS_DENIED
        })))
    ));
}
