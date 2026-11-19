//! `route -n get <target>`: which interface the kernel would send traffic
//! for the target through, and whether that route carries it to one remote
//! host. The route to this computer's own address on a tunnel leaves
//! through the tunnel too, but with the `LOCAL` flag (on-device check,
//! 2026-11-20), so the flags are read as well.
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

/// Most flag names accepted on one route, and the longest name.
const MAX_FLAGS: usize = 32;
const MAX_FLAG_BYTES: usize = 16;

/// Flags of a route that does not carry a probe to one remote host: the
/// target is an address of this computer, a broadcast or a multicast
/// address, or the route drops or rejects what is sent.
const REFUSED_FLAGS: [(&str, &str); 5] = [
    ("LOCAL", "the target is an address of this computer"),
    ("BROADCAST", "the target is a broadcast address"),
    ("MULTICAST", "the target is a multicast address"),
    ("BLACKHOLE", "the route to the target drops what is sent"),
    ("REJECT", "the route to the target rejects what is sent"),
];

/// What `route get` said: the interface and the route's flags.
struct Route<'a> {
    interface: String,
    flags: Vec<&'a str>,
}

/// The interface the route to `target` leaves through. No route at all, a
/// route that is not up, and a route with any of [`REFUSED_FLAGS`] are a
/// route mismatch: a probe would not travel through a tunnel to the target.
pub fn interpret(output: &Output, target: TargetAddress) -> Result<String, AdapterError> {
    let malformed = |m: Malformed| m.into_error("route");
    if output.truncated {
        return Err(malformed(Malformed("output was truncated")));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|_| malformed(Malformed("output is not UTF-8")))?;
    match output.exit_code {
        Some(0) => {
            let route = parse(stdout, target.get()).map_err(malformed)?;
            usable(&route.flags)?;
            Ok(route.interface)
        }
        Some(_) if String::from_utf8_lossy(&output.stderr).contains("not in table") => Err(
            AdapterError::new(AdapterErrorKind::RouteMismatch, "no route to the target"),
        ),
        _ => Err(malformed(Malformed("unexpected exit status"))),
    }
}

fn usable(flags: &[&str]) -> Result<(), AdapterError> {
    let mismatch = |why| Err(AdapterError::new(AdapterErrorKind::RouteMismatch, why));
    if !flags.contains(&"UP") {
        return mismatch("the route to the target is not up");
    }
    for (flag, why) in REFUSED_FLAGS {
        if flags.contains(&flag) {
            return mismatch(why);
        }
    }
    Ok(())
}

/// Requires one `route to` naming the target, one `interface`, one `flags`,
/// no repeated key, and the metrics table as the last two lines.
fn parse(stdout: &str, target: Ipv4Addr) -> Result<Route<'_>, Malformed> {
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
    let mut flags = None;
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
            "flags" => flags = Some(value),
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
    if !valid {
        return Err(Malformed("invalid interface name"));
    }
    Ok(Route {
        interface: interface.to_owned(),
        flags: parse_flags(flags.ok_or(Malformed("no flags"))?)?,
    })
}

/// `<UP,HOST,DONE,LOCAL>`: one to [`MAX_FLAGS`] names of ASCII letters and
/// digits, each at most [`MAX_FLAG_BYTES`] long, none repeated.
fn parse_flags(value: &str) -> Result<Vec<&str>, Malformed> {
    let names: Vec<&str> = value
        .strip_prefix('<')
        .and_then(|v| v.strip_suffix('>'))
        .ok_or(Malformed("malformed flags"))?
        .split(',')
        .collect();
    if names.len() > MAX_FLAGS {
        return Err(Malformed("too many flags"));
    }
    for (i, name) in names.iter().enumerate() {
        if name.is_empty()
            || name.len() > MAX_FLAG_BYTES
            || !name.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(Malformed("malformed flags"));
        }
        if names[..i].contains(name) {
            return Err(Malformed("repeated flag"));
        }
    }
    Ok(names)
}
