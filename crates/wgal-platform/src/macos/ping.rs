//! One ICMP echo bound to a tunnel interface with Apple's `ping -b`, which
//! sets `IP_BOUND_IF` (unlike `-I`, which on macOS selects the multicast
//! interface). Exit status follows `ping(8)`: 0 a reply was heard, 2 none
//! was, anything else an error from `<sysexits.h>`.

use std::net::Ipv4Addr;
use std::time::Duration;

use wgal_core::{AdapterError, ProbeOutcome};
use wgal_protocol::{AdapterErrorKind, TargetAddress};

use super::text::{Malformed, lines};
use crate::exec::{Output, Program};

pub const PING: Program = Program::new("/sbin/ping");

/// Longest per-reply wait passed to `-W`.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

const EX_USAGE: i32 = 64;
const EX_NOHOST: i32 = 68;
const EX_NOPERM: i32 = 77;

/// `-n -b <interface> -c 1 -W <ms> -t <s> <target>`: numeric output, bound to
/// the interface, one echo, wait up to `timeout` for the reply and exit one
/// second after that at the latest. `interface` must already be validated.
pub fn args(interface: &str, target: TargetAddress, timeout: Duration) -> Vec<String> {
    let wait_ms = wait_ms(timeout);
    vec![
        "-n".into(),
        "-b".into(),
        interface.into(),
        "-c".into(),
        "1".into(),
        "-W".into(),
        wait_ms.to_string(),
        "-t".into(),
        (wait_ms.div_ceil(1000) + 1).to_string(),
        target.get().to_string(),
    ]
}

/// The runner's deadline: ping's own `-t` limit plus slack to start it.
pub fn deadline(timeout: Duration) -> Duration {
    Duration::from_millis(wait_ms(timeout).div_ceil(1000) * 1000 + 3000)
}

fn wait_ms(timeout: Duration) -> u64 {
    u64::try_from(timeout.min(MAX_WAIT).as_millis())
        .unwrap_or(30_000)
        .max(1)
}

/// Interprets a finished ping. A ping that could not run properly is an
/// error, never `NoReply`, so a broken probe is not taken for a dead target.
///
/// That includes an echo the host refused to send: ping then warns
/// `ping: sendto: <error>`, still counts the packet as transmitted and exits
/// 2 with the same summary as a target that stayed silent (plan §4.1, T23).
/// So a send error makes the run an error whatever the exit status, and a
/// run without a reply is `NoReply` only when stderr is empty.
pub fn interpret(output: &Output, target: TargetAddress) -> Result<ProbeOutcome, AdapterError> {
    let malformed = |m: Malformed| m.into_error("ping");
    if output.truncated {
        return Err(malformed(Malformed("output was truncated")));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| malformed(Malformed("output is not UTF-8")))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(error) = send_error(&stderr) {
        return Err(error);
    }
    match output.exit_code {
        Some(0) => parse_reply(stdout, target.get()).map_err(malformed),
        Some(2) if !output.stderr.is_empty() => Err(malformed(Malformed(
            "an error was reported on a run without a reply",
        ))),
        Some(2) => {
            no_reply_summary(stdout, target.get()).map_err(malformed)?;
            Ok(ProbeOutcome::NoReply)
        }
        Some(EX_NOHOST) if stderr.contains("bad interface name") => Err(AdapterError::new(
            AdapterErrorKind::NotFound,
            "the interface no longer exists",
        )),
        Some(EX_NOPERM) => Err(denied()),
        _ if stderr.contains("Operation not permitted") || stderr.contains("Permission denied") => {
            Err(denied())
        }
        Some(EX_USAGE) => Err(AdapterError::new(
            AdapterErrorKind::Unsupported,
            "ping rejected its arguments; interface binding may be unsupported",
        )),
        _ => Err(malformed(Malformed("unexpected exit status"))),
    }
}

fn denied() -> AdapterError {
    AdapterError::new(
        AdapterErrorKind::PermissionDenied,
        "ping is not permitted to send",
    )
}

/// The first `ping: sendto: <error>` line on stderr (from `warn("sendto")`
/// in ping's `pinger()`), classified by the exact `strerror` text. Only whole
/// lines with that exact prefix count; stderr is already capped by the runner.
///
/// - `Operation not permitted`, `Permission denied`: the host refused to send
///   (a filter or policy) — `PermissionDenied`.
/// - `No route to host`, `Network is unreachable`, `Can't assign requested
///   address`: the kernel would not send the echo through this interface
///   from its address, although the route check just before passed, so the
///   route moved or a local filter dropped it. `RouteMismatch`: the probe
///   never reached the tunnel path, which the core shows as a path problem
///   and never counts as a failure. Not `LocalNetworkOffline`: nothing here
///   says the local network as a whole is down.
/// - any other send error is still an error (`ParseError`), never `NoReply`.
fn send_error(stderr: &str) -> Option<AdapterError> {
    let message = stderr
        .lines()
        .find_map(|line| line.strip_prefix("ping: sendto: "))?;
    Some(match message {
        "Operation not permitted" | "Permission denied" => denied(),
        "No route to host" | "Network is unreachable" | "Can't assign requested address" => {
            AdapterError::new(
                AdapterErrorKind::RouteMismatch,
                "the system would not send the probe through this interface",
            )
        }
        _ => AdapterError::new(
            AdapterErrorKind::ParseError,
            "ping could not send the probe for an unrecognised reason",
        ),
    })
}

/// Exit 0: exactly one reply, from the target, and a summary saying one packet
/// was received. Every line must be one ping prints for a single echo.
fn parse_reply(stdout: &str, target: Ipv4Addr) -> Result<ProbeOutcome, Malformed> {
    let mut rtt = None;
    let mut received = None;
    for line in lines(stdout, true)? {
        if let Some(rest) = line.strip_prefix("PING ") {
            if !rest.starts_with(&format!("{target} ({target}): "))
                || !rest.ends_with(" data bytes")
            {
                return Err(Malformed("unexpected PING line"));
            }
        } else if line.contains(" bytes from ") {
            if rtt.replace(parse_reply_line(line, target)?).is_some() {
                return Err(Malformed("more than one reply"));
            }
        } else if let Some(counts) = summary(line) {
            if received.replace(counts).is_some() {
                return Err(Malformed("more than one summary"));
            }
        } else if !is_known_other_line(line, target) {
            return Err(Malformed("unknown line"));
        }
    }
    match (rtt, received) {
        (Some(rtt), Some((1, 1))) => Ok(ProbeOutcome::Reply { rtt }),
        (None, _) => Err(Malformed("exit status 0 without a reply")),
        _ => Err(Malformed("summary does not match one reply")),
    }
}

/// `64 bytes from 192.0.2.1: icmp_seq=0 ttl=63 time=12.345 ms`.
fn parse_reply_line(line: &str, target: Ipv4Addr) -> Result<Duration, Malformed> {
    let (from, fields) = line.split_once(": ").ok_or(Malformed("malformed reply"))?;
    let (bytes, source) = from
        .split_once(" bytes from ")
        .ok_or(Malformed("malformed reply"))?;
    if bytes.is_empty() || !bytes.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Malformed("malformed reply"));
    }
    if source.parse::<Ipv4Addr>().ok() != Some(target) {
        return Err(Malformed("reply from another address"));
    }
    let fields: Vec<&str> = fields.split(' ').collect();
    let [seq, ttl, time, "ms"] = fields.as_slice() else {
        return Err(Malformed("malformed reply"));
    };
    let numeric = |field: &str, prefix: &str| {
        field
            .strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if !numeric(seq, "icmp_seq=") || !numeric(ttl, "ttl=") {
        return Err(Malformed("malformed reply"));
    }
    time.strip_prefix("time=")
        .and_then(parse_millis)
        .ok_or(Malformed("malformed round-trip time"))
}

/// `12.345` milliseconds, at most three decimals and at most a minute.
fn parse_millis(text: &str) -> Option<Duration> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty()
        || whole.len() > 5
        || fraction.len() > 3
        || !digits(whole)
        || !digits(fraction)
    {
        return None;
    }
    let millis: u64 = whole.parse().ok()?;
    let micros: u64 = format!("{fraction:0<3}").parse().ok()?;
    (millis <= 60_000).then(|| Duration::from_micros(millis * 1000 + micros))
}

/// `1 packets transmitted, 1 packets received, 0.0% packet loss`, as
/// (transmitted, received).
fn summary(line: &str) -> Option<(u32, u32)> {
    let (transmitted, rest) = line.split_once(" packets transmitted, ")?;
    let (received, _) = rest.split_once(" packets received, ")?;
    Some((transmitted.parse().ok()?, received.parse().ok()?))
}

fn is_known_other_line(line: &str, target: Ipv4Addr) -> bool {
    line.is_empty()
        || line == format!("--- {target} ping statistics ---")
        || line.starts_with("round-trip min/avg/max/stddev = ")
        || line.starts_with("Request timeout for icmp_seq ")
}

/// Exit 2: the summary must exist and report no reply received.
fn no_reply_summary(stdout: &str, target: Ipv4Addr) -> Result<(), Malformed> {
    let mut found = false;
    for line in lines(stdout, true)? {
        if let Some((_, received)) = summary(line) {
            if received != 0 || found {
                return Err(Malformed("summary does not match no reply"));
            }
            found = true;
        } else if line.contains(&format!(" bytes from {target}: icmp_seq=")) {
            return Err(Malformed("reply printed with exit status 2"));
        }
    }
    if found {
        Ok(())
    } else {
        Err(Malformed("no summary"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TargetAddress {
        TargetAddress::new(Ipv4Addr::new(192, 0, 2, 1)).unwrap()
    }

    #[test]
    fn builds_the_fixed_argument_vector() {
        assert_eq!(
            args("utun4", target(), Duration::from_secs(2)),
            [
                "-n",
                "-b",
                "utun4",
                "-c",
                "1",
                "-W",
                "2000",
                "-t",
                "3",
                "192.0.2.1"
            ]
        );
        let short = args("utun4", target(), Duration::from_millis(1500));
        assert_eq!((short[6].as_str(), short[8].as_str()), ("1500", "3"));
        let huge = args("utun4", target(), Duration::from_secs(3600));
        assert_eq!(huge[6], "30000");
        assert_eq!(huge[8], "31");
        let zero = args("utun4", target(), Duration::ZERO);
        assert_eq!((zero[6].as_str(), zero[8].as_str()), ("1", "2"));
        assert_eq!(deadline(Duration::from_secs(2)), Duration::from_secs(5));
    }

    #[test]
    fn parses_round_trip_times_exactly() {
        assert_eq!(parse_millis("12.345"), Some(Duration::from_micros(12_345)));
        assert_eq!(parse_millis("0.05"), Some(Duration::from_micros(50)));
        assert_eq!(parse_millis("7"), Some(Duration::from_millis(7)));
        for bad in ["", ".5", "1.2345", "-1.0", "1e3", "60001.0", "1,5", "nan"] {
            assert_eq!(parse_millis(bad), None, "{bad:?}");
        }
    }
}
