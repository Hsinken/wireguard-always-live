//! The socket server over loopback TCP, which every OS has; the Unix socket
//! itself only adds the binding and the peer's identity, which the platform
//! code reads (`wgal_platform::{linux,macos}::socket::peer_uid`).
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::net::{Shutdown, TcpListener, TcpStream};

use wgal_protocol::{Request, Response};

use super::*;

impl Conn for TcpStream {
    fn set_blocking(&self) -> io::Result<()> {
        self.set_nonblocking(false)
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn shut_down(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

impl Listen for TcpListener {
    type Conn = TcpStream;
    fn set_nonblocking(&self) -> io::Result<()> {
        TcpListener::set_nonblocking(self, true)
    }
    fn accept_conn(&self) -> io::Result<TcpStream> {
        self.accept().map(|(stream, _)| stream)
    }
}

fn caller(allowed: bool) -> Caller {
    Caller {
        account: "1000".to_owned(),
        allowed,
        elevated: false,
    }
}

/// The identities the server gives the next connections, in order.
type Script = Arc<Mutex<VecDeque<Option<Caller>>>>;

struct Running {
    address: std::net::SocketAddr,
    handle: ServerHandle<TcpStream>,
    thread: JoinHandle<Result<(), ServerError>>,
    script: Script,
}

impl Running {
    fn connect(&self, identity: Option<Caller>) -> TcpStream {
        lock(&self.script).push_back(identity);
        let stream = TcpStream::connect(self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn stop(self) -> ServerStats {
        self.handle.stop();
        let ended = self.thread.join().unwrap();
        assert!(ended.is_ok(), "{ended:?}");
        self.handle.stats()
    }
}

/// A server whose handler accepts every request of an allowed caller and,
/// like the loop's own screen, refuses every request of one that is not.
fn start(limits: ServerLimits) -> Running {
    start_with(limits, |_| {})
}

/// As [`start`], with `before` run on every request first (a test's panic).
fn start_with(limits: ServerLimits, before: fn(&RequestEnvelope)) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = Server::new(listener, limits).unwrap();
    let handle = server.handle();
    let script: Script = Arc::default();
    let identities = Arc::clone(&script);
    let thread = thread::spawn(move || {
        server.run(
            move |_| lock(&identities).pop_front().flatten(),
            move |caller, envelope| ResponseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some({
                    before(&envelope);
                    envelope.request_id
                }),
                outcome: if caller.allowed {
                    Outcome::Ok(Response::Accepted)
                } else {
                    Outcome::Err(ProtocolError::new(
                        ErrorCode::Unauthorized,
                        Some(ErrorReason::CallerNotAllowed),
                        "not allowed",
                    ))
                },
            },
        )
    });
    Running {
        address,
        handle,
        thread,
        script,
    }
}

fn request(id: &str) -> Vec<u8> {
    codec::encode_request(&RequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        origin: None,
        request: Request::GetSnapshot,
    })
    .unwrap()
}

fn ask(stream: &mut TcpStream, id: &str) -> Option<ResponseEnvelope> {
    codec::write_frame(stream, &request(id)).ok()?;
    let frame = codec::read_frame(stream).ok()?;
    Some(codec::decode_response(&frame).unwrap())
}

fn closed(stream: &mut TcpStream) -> bool {
    let mut byte = [0u8; 1];
    matches!(stream.read(&mut byte), Ok(0) | Err(_))
}

fn quick() -> ServerLimits {
    ServerLimits {
        idle_timeout: Duration::from_secs(5),
        ..ServerLimits::DEFAULT
    }
}

#[test]
fn an_allowed_caller_is_answered_in_turn_on_one_connection() {
    let server = start(quick());
    let mut stream = server.connect(Some(caller(true)));
    for id in ["r1", "r2", "r3"] {
        let answer = ask(&mut stream, id).unwrap();
        assert_eq!(answer.request_id, Some(RequestId::new(id).unwrap()));
        assert_eq!(answer.outcome, Outcome::Ok(Response::Accepted));
    }
    drop(stream);
    let stats = server.stop();
    assert_eq!(stats.admitted, 1);
    assert_eq!(stats.requests, 3);
    assert_eq!(stats.refused, 0);
}

#[test]
fn a_caller_that_is_not_allowed_gets_one_refusal_and_the_connection_ends() {
    let server = start(quick());
    let mut stream = server.connect(Some(caller(false)));
    let answer = ask(&mut stream, "r1").unwrap();
    assert_eq!(answer.request_id, Some(RequestId::new("r1").unwrap()));
    let Outcome::Err(error) = answer.outcome else {
        panic!("not refused");
    };
    assert_eq!(error.reason, Some(ErrorReason::CallerNotAllowed));
    assert!(closed(&mut stream));
    let stats = server.stop();
    assert_eq!((stats.refused, stats.admitted, stats.requests), (1, 0, 1));
}

#[test]
fn a_peer_whose_identity_cannot_be_read_is_closed_unanswered() {
    let server = start(quick());
    let mut stream = server.connect(None);
    assert!(ask(&mut stream, "r1").is_none());
    let stats = server.stop();
    assert_eq!((stats.turned_away, stats.requests), (1, 0));
}

#[test]
fn callers_that_are_not_allowed_never_take_the_allowed_slots() {
    let server = start(ServerLimits {
        max_connections: 1,
        max_refusals: 1,
        refusal_timeout: Duration::from_secs(3),
        ..quick()
    });
    // One refused caller holds its only slot without sending anything; a
    // second is closed at once.
    let mut holding = server.connect(Some(caller(false)));
    let mut second = server.connect(Some(caller(false)));
    assert!(closed(&mut second));
    // The allowed caller is still served.
    let mut allowed = server.connect(Some(caller(true)));
    assert_eq!(
        ask(&mut allowed, "r1").unwrap().outcome,
        Outcome::Ok(Response::Accepted)
    );
    // The holding caller is dropped after its short deadline, unanswered.
    assert!(closed(&mut holding));
    drop(allowed);
    let stats = server.stop();
    assert_eq!(stats.turned_away, 1);
    assert_eq!(stats.idle_timeouts, 1);
}

#[test]
fn an_allowed_connection_beyond_the_limit_is_closed_at_once() {
    let server = start(ServerLimits {
        max_connections: 1,
        ..quick()
    });
    let mut first = server.connect(Some(caller(true)));
    assert!(ask(&mut first, "r1").is_some());
    let mut second = server.connect(Some(caller(true)));
    assert!(ask(&mut second, "r2").is_none());
    // Once the first ends, its slot is free again.
    drop(first);
    let started = Instant::now();
    let mut third = loop {
        let mut stream = server.connect(Some(caller(true)));
        if ask(&mut stream, "r3").is_some() {
            break stream;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "slot not freed");
        thread::sleep(Duration::from_millis(20));
    };
    assert!(ask(&mut third, "r4").is_some());
    drop(third);
    let stats = server.stop();
    assert!(stats.turned_away >= 1);
}

#[test]
fn an_idle_connection_is_dropped_after_its_deadline() {
    let server = start(ServerLimits {
        idle_timeout: Duration::from_millis(200),
        ..quick()
    });
    let mut stream = server.connect(Some(caller(true)));
    assert!(ask(&mut stream, "r1").is_some());
    let started = Instant::now();
    assert!(closed(&mut stream));
    assert!(started.elapsed() < Duration::from_secs(3));
    // A request trickled in too slowly is dropped the same way.
    let mut slow = server.connect(Some(caller(true)));
    let frame = request("r2");
    let mut framed = Vec::new();
    codec::write_frame(&mut framed, &frame).unwrap();
    slow.write_all(&framed[..3]).unwrap();
    assert!(closed(&mut slow));
    let stats = server.stop();
    assert_eq!(stats.idle_timeouts, 2);
}

#[test]
fn a_request_that_does_not_decode_is_refused_and_the_connection_goes_on() {
    let server = start(quick());
    let mut stream = server.connect(Some(caller(true)));
    codec::write_frame(
        &mut stream,
        br#"{"protocol_version":1,"request_id":"bad1","request":{"op":"no_such_op"}}"#,
    )
    .unwrap();
    let answer = codec::decode_response(&codec::read_frame(&mut stream).unwrap()).unwrap();
    assert_eq!(answer.request_id, Some(RequestId::new("bad1").unwrap()));
    assert!(matches!(answer.outcome, Outcome::Err(_)));
    assert!(ask(&mut stream, "r2").is_some());
    drop(stream);
    server.stop();
}

#[test]
fn an_empty_or_oversized_frame_ends_the_connection_unanswered() {
    let server = start(quick());
    let mut empty = server.connect(Some(caller(true)));
    empty.write_all(&0u32.to_le_bytes()).unwrap();
    assert!(closed(&mut empty));
    let mut huge = server.connect(Some(caller(true)));
    huge.write_all(&(1u32 << 30).to_le_bytes()).unwrap();
    assert!(closed(&mut huge));
    let stats = server.stop();
    assert_eq!(stats.dropped, 2);
    assert_eq!(stats.requests, 0);
}

#[test]
fn a_stop_ends_the_accept_loop_and_every_open_connection() {
    let server = start(ServerLimits {
        idle_timeout: Duration::from_secs(60),
        ..quick()
    });
    let mut open = server.connect(Some(caller(true)));
    assert!(ask(&mut open, "r1").is_some());
    let started = Instant::now();
    server.stop();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(closed(&mut open));
}

#[test]
fn limits_out_of_range_are_refused() {
    for limits in [
        ServerLimits {
            max_connections: 0,
            ..ServerLimits::DEFAULT
        },
        ServerLimits {
            max_connections: 65,
            ..ServerLimits::DEFAULT
        },
        ServerLimits {
            max_refusals: 0,
            ..ServerLimits::DEFAULT
        },
        ServerLimits {
            idle_timeout: Duration::ZERO,
            ..ServerLimits::DEFAULT
        },
        ServerLimits {
            refusal_timeout: Duration::ZERO,
            ..ServerLimits::DEFAULT
        },
        ServerLimits {
            write_timeout: Duration::ZERO,
            ..ServerLimits::DEFAULT
        },
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(matches!(
            Server::new(listener, limits),
            Err(ServerError::InvalidLimits)
        ));
    }
}

#[test]
fn a_connection_whose_handler_panics_is_shut_down_and_its_slot_freed() {
    let server = start_with(
        ServerLimits {
            max_connections: 1,
            ..quick()
        },
        |envelope| {
            if envelope.request_id.as_str() == "boom" {
                panic!("a handler that panics (expected in this test)");
            }
        },
    );
    let mut stream = server.connect(Some(caller(true)));
    assert!(ask(&mut stream, "boom").is_none());
    assert!(closed(&mut stream));
    // Its registration and its only slot are free again.
    let started = Instant::now();
    while server.handle.open_connections() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "still registered"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let mut next = server.connect(Some(caller(true)));
    assert_eq!(
        ask(&mut next, "r1").unwrap().outcome,
        Outcome::Ok(Response::Accepted)
    );
    drop(next);
    let stats = server.stop();
    assert_eq!(stats.dropped, 1);
}
