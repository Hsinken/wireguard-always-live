//! Parser for iputils `ping -4 -n -I <interface> -c 1` output.
//!
//! A reply counts only if the header shows the probe was bound to the
//! interface (and, when known, its address), exactly one reply came from the
//! target for sequence 1, and the summary agrees. Exit status 2 (ping could
//! not run as asked) is always an error, never "no reply".
//!
//! Neither is an echo the host refused to send. iputils then prints
//! `sendmsg: <error>` on stderr ("Hard local error. Pretend we sent packet."
//! in `pinger()`), counts the packet as transmitted and exits 1 with the same
//! summary as a silent target (plan §4.1, T23). So a run is `NoReply` only
//! with exit status 1 and nothing at all on stderr.

use std::net::Ipv4Addr;
use std::time::Duration;

use wgal_core::{AdapterError, ProbeOutcome};
use wgal_protocol::AdapterErrorKind;

use crate::exec::Output;

use super::name::InterfaceName;

/// Most bytes a one-packet ping may print before it is refused.
pub const MAX_PING_BYTES: usize = 8 * 1024;

fn parse_error(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::ParseError, detail)
}

/// Reads one finished ping run. `source` is the interface's own address when
/// it has exactly one; the header must then show ping used it.
pub fn parse_ping(
    output: &Output,
    target: Ipv4Addr,
    interface: &InterfaceName,
    source: Option<Ipv4Addr>,
) -> Result<ProbeOutcome, AdapterError> {
    match output.exit_code {
        Some(0) | Some(1) => {}
        Some(2) => return Err(classify_failure(output)),
        _ => return Err(parse_error("ping ended with an unexpected status")),
    }
    if let Some(error) = send_error(&output.stderr) {
        return Err(error);
    }
    if output.exit_code == Some(1) && !output.stderr.is_empty() {
        return Err(parse_error(
            "ping reported an error on a run without a reply",
        ));
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| parse_error("ping output is not UTF-8"))?;
    if !text.ends_with('\n') {
        return Err(parse_error("ping output is incomplete"));
    }
    check_header(text, target, interface, source)?;

    let reply_prefix = format!(" bytes from {target}: ");
    let mut replies = text.lines().filter(|l| l.contains(" bytes from "));
    let reply = replies.next();
    if replies.next().is_some() {
        return Err(parse_error("ping printed more than one reply"));
    }
    let received = summary_received(text)?;

    match (output.exit_code, reply, received) {
        (Some(0), Some(line), 1) => {
            let rest = line
                .split_once(&reply_prefix)
                .map(|(_, rest)| rest)
                .ok_or_else(|| parse_error("ping reply came from another address"))?;
            if !rest.split_whitespace().any(|f| f == "icmp_seq=1") {
                return Err(parse_error("ping reply is not for this probe"));
            }
            // iputils ends a run with replies with the rtt statistics line.
            if !text
                .lines()
                .any(|l| l.starts_with("rtt min/avg/max/mdev = "))
            {
                return Err(parse_error("ping output is incomplete"));
            }
            Ok(ProbeOutcome::Reply {
                rtt: parse_rtt(rest)?,
            })
        }
        (Some(1), None, 0) => Ok(ProbeOutcome::NoReply),
        _ => Err(parse_error("ping status, replies and summary disagree")),
    }
}

/// `PING <t> (<t>) from <source> <interface>: 56(84) bytes of data.`
fn check_header(
    text: &str,
    target: Ipv4Addr,
    interface: &InterfaceName,
    source: Option<Ipv4Addr>,
) -> Result<(), AdapterError> {
    let header = text
        .lines()
        .next()
        .ok_or_else(|| parse_error("ping printed nothing"))?;
    let rest = header
        .strip_prefix(&format!("PING {target} ({target}) from "))
        .ok_or_else(|| parse_error("ping header does not show a bound probe to the target"))?;
    let (from, rest) = rest
        .split_once(' ')
        .ok_or_else(|| parse_error("ping header is malformed"))?;
    let from: Ipv4Addr = from
        .parse()
        .map_err(|_| parse_error("ping header has no source address"))?;
    if !rest.starts_with(&format!("{interface}: ")) {
        return Err(AdapterError::new(
            AdapterErrorKind::RouteMismatch,
            "ping was not bound to this interface",
        ));
    }
    if source.is_some_and(|s| s != from) {
        return Err(AdapterError::new(
            AdapterErrorKind::RouteMismatch,
            "ping used a different source address",
        ));
    }
    Ok(())
}

/// The `received` count from `1 packets transmitted, N received, ...`.
fn summary_received(text: &str) -> Result<u32, AdapterError> {
    let mut summaries = text
        .lines()
        .filter_map(|l| l.strip_prefix("1 packets transmitted, "));
    let summary = summaries
        .next()
        .ok_or_else(|| parse_error("ping printed no summary for one packet"))?;
    if summaries.next().is_some() {
        return Err(parse_error("ping printed more than one summary"));
    }
    let (count, _) = summary
        .split_once(" received")
        .ok_or_else(|| parse_error("ping summary is malformed"))?;
    count
        .parse()
        .map_err(|_| parse_error("ping summary is malformed"))
}

/// `time=56.53 ms`, kept to the microsecond.
fn parse_rtt(reply: &str) -> Result<Duration, AdapterError> {
    let mut fields = reply.split_whitespace();
    let value = fields
        .find_map(|f| f.strip_prefix("time="))
        .ok_or_else(|| parse_error("ping reply has no time"))?;
    if fields.next() != Some("ms") {
        return Err(parse_error("ping reply time is not in ms"));
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !(fraction.is_empty() || digits(fraction)) {
        return Err(parse_error("ping reply time is malformed"));
    }
    let millis: u64 = whole
        .parse()
        .map_err(|_| parse_error("ping reply time is malformed"))?;
    let mut micros = 0_u64;
    for (i, digit) in fraction.bytes().take(3).enumerate() {
        micros += u64::from(digit - b'0') * 10_u64.pow(2 - i as u32);
    }
    Ok(Duration::from_millis(millis) + Duration::from_micros(micros))
}

/// The first send error iputils reported on stderr: a whole line
/// `<name>: sendmsg: <error>` (`pinger()`) or `<name>: local error: <error>`
/// (an error queued on the socket by the local stack), where `<name>` is
/// `ping` (older iputils, `perror("ping: sendmsg")`) or the fixed path the
/// runner starts (newer iputils print glibc's `program_invocation_name`).
/// Classified by the exact C-locale `strerror` text; stderr is already capped
/// by the runner.
///
/// - `Operation not permitted`, `Permission denied`: the host refused to send
///   (for example a netfilter OUTPUT drop) — `PermissionDenied`.
/// - `Network is unreachable`, `No route to host`, `Cannot assign requested
///   address`: the kernel would not send the echo through this interface
///   from its address, although both route lookups just before passed, so
///   the route moved or a local rule rejected it. `RouteMismatch`, as for the
///   same errors at exit status 2: the probe never reached the tunnel path,
///   which the core shows as a path problem and never counts as a failure.
///   Not `LocalNetworkOffline`: nothing here says the local network as a
///   whole is down.
/// - any other send error is still an error (`ParseError`), never `NoReply`.
fn send_error(stderr: &[u8]) -> Option<AdapterError> {
    let text = String::from_utf8_lossy(stderr);
    let message = text.lines().find_map(|line| {
        let rest = line
            .strip_prefix("ping: ")
            .or_else(|| line.strip_prefix(&format!("{}: ", super::PING.path())))?;
        rest.strip_prefix("sendmsg: ")
            .or_else(|| rest.strip_prefix("local error: "))
    })?;
    Some(match message {
        "Operation not permitted" | "Permission denied" => AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            "ping was not permitted to send",
        ),
        "Network is unreachable" | "No route to host" | "Cannot assign requested address" => {
            AdapterError::new(
                AdapterErrorKind::RouteMismatch,
                "the system would not send the probe through this interface",
            )
        }
        _ => parse_error("ping could not send the probe for an unrecognised reason"),
    })
}

/// Exit status 2: ping could not do what was asked. Classified from its
/// (C locale) error text, which is never copied into the error.
fn classify_failure(output: &Output) -> AdapterError {
    let text = String::from_utf8_lossy(&output.stderr);
    if text.contains("No such device") {
        AdapterError::new(AdapterErrorKind::NotFound, "interface no longer exists")
    } else if text.contains("Operation not permitted") || text.contains("Permission denied") {
        AdapterError::new(AdapterErrorKind::PermissionDenied, "ping was not permitted")
    } else if text.contains("Network is unreachable")
        || text.contains("Cannot assign requested address")
    {
        AdapterError::new(
            AdapterErrorKind::RouteMismatch,
            "no route to the target through this interface",
        )
    } else {
        parse_error("ping failed with an unrecognised error")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    const TARGET: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const SOURCE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 11);

    fn run(exit_code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            exit_code: Some(exit_code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            truncated: false,
        }
    }

    fn office() -> InterfaceName {
        InterfaceName::new("wg-office").unwrap()
    }

    fn parse(output: &Output) -> Result<ProbeOutcome, AdapterError> {
        parse_ping(output, TARGET, &office(), Some(SOURCE))
    }

    fn kind(result: Result<ProbeOutcome, AdapterError>) -> AdapterErrorKind {
        result.unwrap_err().kind
    }

    #[test]
    fn a_bound_reply_from_the_target_is_a_reply_with_its_rtt() {
        let outcome = parse(&run(0, &fixture("ping-reply.txt"), "")).unwrap();
        assert_eq!(
            outcome,
            ProbeOutcome::Reply {
                rtt: Duration::from_micros(56_531)
            }
        );
        // Without a known source address the header still has to show the interface.
        let outcome = parse_ping(
            &run(0, &fixture("ping-reply.txt"), ""),
            TARGET,
            &office(),
            None,
        );
        assert!(outcome.is_ok());
    }

    #[test]
    fn rtt_formats() {
        let cases = [
            ("time=0.045 ms", 45),
            ("time=12 ms", 12_000),
            ("time=1234.5 ms", 1_234_500),
            ("time=7.123456 ms", 7_123),
        ];
        for (text, micros) in cases {
            assert_eq!(
                parse_rtt(text).unwrap(),
                Duration::from_micros(micros),
                "{text}"
            );
        }
        for bad in [
            "time=ms",
            "time=1.2",
            "time=-1 ms",
            "time=1.2 s",
            "time=1e3 ms",
            "time=.5 ms",
        ] {
            assert!(parse_rtt(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn silence_and_icmp_errors_are_no_reply_never_zero_ms() {
        for file in ["ping-no-reply.txt", "ping-host-unreachable.txt"] {
            assert_eq!(
                parse(&run(1, &fixture(file), "")).unwrap(),
                ProbeOutcome::NoReply,
                "{file}"
            );
        }
    }

    #[test]
    fn failures_to_run_are_errors_not_no_reply() {
        let header =
            "PING 192.0.2.1 (192.0.2.1) from 192.0.2.11 wg-office: 56(84) bytes of data.\n";
        let cases = [
            ("ping-no-such-device.stderr.txt", AdapterErrorKind::NotFound),
            (
                "ping-permission-denied.stderr.txt",
                AdapterErrorKind::PermissionDenied,
            ),
            (
                "ping-network-unreachable.stderr.txt",
                AdapterErrorKind::RouteMismatch,
            ),
        ];
        for (file, expected) in cases {
            assert_eq!(kind(parse(&run(2, "", &fixture(file)))), expected, "{file}");
        }
        assert_eq!(
            kind(parse(&run(2, header, "ping: new failure\n"))),
            AdapterErrorKind::ParseError
        );
        let mut killed = run(0, &fixture("ping-reply.txt"), "");
        killed.exit_code = None;
        assert_eq!(kind(parse(&killed)), AdapterErrorKind::ParseError);
    }

    #[test]
    fn f1_send_errors_are_errors_never_no_reply() {
        let no_reply = fixture("ping-no-reply.txt");
        let cases = [
            (
                "/usr/bin/ping: sendmsg: Operation not permitted\n",
                AdapterErrorKind::PermissionDenied,
            ),
            (
                "ping: sendmsg: Permission denied\n",
                AdapterErrorKind::PermissionDenied,
            ),
            (
                "/usr/bin/ping: sendmsg: Network is unreachable\n",
                AdapterErrorKind::RouteMismatch,
            ),
            (
                "/usr/bin/ping: sendmsg: No route to host\n",
                AdapterErrorKind::RouteMismatch,
            ),
            (
                "ping: sendmsg: Cannot assign requested address\n",
                AdapterErrorKind::RouteMismatch,
            ),
            (
                "/usr/bin/ping: local error: Operation not permitted\n",
                AdapterErrorKind::PermissionDenied,
            ),
            (
                "/usr/bin/ping: local error: message too long, mtu=1420\n",
                AdapterErrorKind::ParseError,
            ),
            (
                "/usr/bin/ping: sendmsg: No buffer space available\n",
                AdapterErrorKind::ParseError,
            ),
            (
                "/bin/ping: sendmsg: Operation not permitted\n",
                AdapterErrorKind::ParseError,
            ),
            (
                "/usr/bin/ping: Warning: source address might be selected on device other than: wg-office\n",
                AdapterErrorKind::ParseError,
            ),
            ("\u{fffd}\n", AdapterErrorKind::ParseError),
        ];
        for (stderr, expected) in cases {
            assert_eq!(
                kind(parse(&run(1, &no_reply, stderr))),
                expected,
                "{stderr:?}"
            );
        }
        let mut invalid = run(1, &no_reply, "");
        invalid.stderr = vec![0xff, b'\n'];
        assert_eq!(kind(parse(&invalid)), AdapterErrorKind::ParseError);

        // A send error beside a reply contradicts it.
        assert_eq!(
            kind(parse(&run(
                0,
                &fixture("ping-reply.txt"),
                "/usr/bin/ping: sendmsg: No route to host\n"
            ))),
            AdapterErrorKind::RouteMismatch
        );
        // Without one, the same summary is a target that did not answer.
        assert_eq!(
            parse(&run(1, &no_reply, "")).unwrap(),
            ProbeOutcome::NoReply
        );
    }

    #[test]
    fn replies_that_do_not_belong_to_this_probe_are_rejected() {
        let reply = fixture("ping-reply.txt");
        assert_eq!(
            kind(parse(&run(0, &fixture("ping-reply-other-host.txt"), ""))),
            AdapterErrorKind::ParseError
        );
        assert_eq!(
            kind(parse(&run(
                0,
                &reply.replace("icmp_seq=1", "icmp_seq=2"),
                ""
            ))),
            AdapterErrorKind::ParseError
        );
        let doubled = reply.replacen(
            "64 bytes from",
            "64 bytes from 192.0.2.1: icmp_seq=1 ttl=64 time=1.0 ms (DUP!)\n64 bytes from",
            1,
        );
        assert_eq!(
            kind(parse(&run(0, &doubled, ""))),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn an_unbound_or_wrongly_bound_probe_is_not_evidence() {
        assert_eq!(
            kind(parse(&run(0, &fixture("ping-reply-unbound.txt"), ""))),
            AdapterErrorKind::ParseError
        );
        let other_interface = fixture("ping-reply.txt").replace(" wg-office: ", " wg-lab: ");
        assert_eq!(
            kind(parse(&run(0, &other_interface, ""))),
            AdapterErrorKind::RouteMismatch
        );
        let other_source =
            fixture("ping-reply.txt").replace("from 192.0.2.11 ", "from 192.0.2.99 ");
        assert_eq!(
            kind(parse(&run(0, &other_source, ""))),
            AdapterErrorKind::RouteMismatch
        );
    }

    #[test]
    fn status_and_text_must_agree() {
        assert_eq!(
            kind(parse(&run(1, &fixture("ping-reply.txt"), ""))),
            AdapterErrorKind::ParseError
        );
        assert_eq!(
            kind(parse(&run(0, &fixture("ping-no-reply.txt"), ""))),
            AdapterErrorKind::ParseError
        );
        let reply = fixture("ping-reply.txt");
        for cut in 0..reply.len() {
            let prefix = &reply[..cut];
            assert!(parse(&run(0, prefix, "")).is_err(), "cut at {cut}");
        }
    }
}
