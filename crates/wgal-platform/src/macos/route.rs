//! `route -n get <target>`: which interface the kernel would send traffic
//! for the target through.
//!
//! ```text
//!    route to: 192.0.2.1
//! destination: 192.0.2.0
//!        mask: 255.255.255.0
//!   interface: utun4
//!       flags: <UP,DONE,STATIC,PRCLONING>
//!  recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
//!        0         0         0         0         0         0      1420         0
//! ```

use std::net::Ipv4Addr;

use wgal_core::AdapterError;
use wgal_protocol::{AdapterErrorKind, TargetAddress};

use super::text::{Malformed, lines};
use crate::exec::{Output, Program};

pub const ROUTE: Program = Program::new("/sbin/route");

pub fn args(target: TargetAddress) -> Vec<String> {
    vec!["-n".into(), "get".into(), target.get().to_string()]
}

const METRICS_HEADER: [&str; 8] = [
    "recvpipe", "sendpipe", "ssthresh", "rtt,msec", "rttvar", "hopcount", "mtu", "expire",
];

/// The interface the route to `target` leaves through. No route at all is a
/// route mismatch: traffic to the target would not use any tunnel.
pub fn interpret(output: &Output, target: TargetAddress) -> Result<String, AdapterError> {
    let malformed = |m: Malformed| m.into_error("route");
    if output.truncated {
        return Err(malformed(Malformed("output was truncated")));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| malformed(Malformed("output is not UTF-8")))?;
    match output.exit_code {
        Some(0) => parse(stdout, target.get()).map_err(malformed),
        Some(_) if String::from_utf8_lossy(&output.stderr).contains("not in table") => Err(
            AdapterError::new(AdapterErrorKind::RouteMismatch, "no route to the target"),
        ),
        _ => Err(malformed(Malformed("unexpected exit status"))),
    }
}

/// Requires one `route to` naming the target, one `interface`, no repeated
/// key, and the metrics table as the last two lines.
fn parse(stdout: &str, target: Ipv4Addr) -> Result<String, Malformed> {
    let lines = lines(stdout, false)?;
    let (fields, table) = lines.split_at(lines.len().saturating_sub(2));
    let [header, values] = table else {
        return Err(Malformed("no metrics table"));
    };
    if header.split_whitespace().ne(METRICS_HEADER) {
        return Err(Malformed("unknown metrics header"));
    }
    let values: Vec<&str> = values.split_whitespace().collect();
    let numeric = |v: &&str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    if values.len() != METRICS_HEADER.len() || !values.iter().all(numeric) {
        return Err(Malformed("malformed metrics values"));
    }

    let mut seen: Vec<&str> = Vec::new();
    let mut route_to = None;
    let mut interface = None;
    for line in fields {
        let (key, value) = line
            .trim_start_matches(' ')
            .split_once(": ")
            .ok_or(Malformed("line is not 'key: value'"))?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b' ') {
            return Err(Malformed("invalid key"));
        }
        if seen.contains(&key) {
            return Err(Malformed("duplicate key"));
        }
        seen.push(key);
        match key {
            "route to" => route_to = Some(value),
            "interface" => interface = Some(value),
            _ => {}
        }
    }
    if route_to.and_then(|v| v.parse::<Ipv4Addr>().ok()) != Some(target) {
        return Err(Malformed("route is not for the target"));
    }
    let interface = interface.ok_or(Malformed("no interface"))?;
    let valid = !interface.is_empty()
        && interface.len() <= 15
        && interface
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if valid {
        Ok(interface.to_owned())
    } else {
        Err(Malformed("invalid interface name"))
    }
}
