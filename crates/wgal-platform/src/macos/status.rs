//! `scutil --nc status <UUID>`: the connection state on the first line,
//! then optionally `Extended Status <dictionary> {` and its body.

use std::net::Ipv4Addr;

use wgal_protocol::OsState;

use super::text::{Dict, Malformed, Node, lines, parse_dict};

/// The states `scutil --nc` reports for a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NcState {
    Invalid,
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Reasserting,
    Unknown,
}

impl NcState {
    /// Exact match only; anything else is unknown output, never a state.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "Invalid" => Self::Invalid,
            "Disconnected" => Self::Disconnected,
            "Connecting" => Self::Connecting,
            "Connected" => Self::Connected,
            "Disconnecting" => Self::Disconnecting,
            "Reasserting" => Self::Reasserting,
            "Unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// `Invalid` and `Unknown` are what the system itself could not determine;
    /// they are reported as unknown, never as disconnected.
    pub fn os_state(self) -> OsState {
        match self {
            Self::Connected => OsState::Connected,
            Self::Disconnected => OsState::Disconnected,
            Self::Connecting | Self::Disconnecting | Self::Reasserting => OsState::Transitioning,
            Self::Invalid | Self::Unknown => OsState::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NcStatus {
    pub state: NcState,
    pub extended: Option<Dict>,
}

const EXTENDED: &str = "Extended Status <dictionary> {";

/// Parses the whole output; anything after the extended status, such as a
/// second status block, is rejected.
pub fn parse_status(text: &str) -> Result<NcStatus, Malformed> {
    let lines = lines(text, false)?;
    let mut iter = lines.into_iter();
    let first = iter.next().ok_or(Malformed("empty output"))?;
    let state = NcState::parse(first).ok_or(Malformed("unknown connection status"))?;
    let extended = match iter.next() {
        None => None,
        Some(EXTENDED) => Some(parse_dict(&mut iter, 1)?),
        Some(_) => return Err(Malformed("unexpected line after the status")),
    };
    if iter.next().is_some() {
        return Err(Malformed("unexpected output after the extended status"));
    }
    Ok(NcStatus { state, extended })
}

/// The interface and addresses the connection currently uses, read only from
/// the extended status's top-level `IPv4` dictionary, and the connection's
/// `ConnectCount`, which tells this connection from the next one even when the
/// app reuses the same `utun` and address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelInterface {
    pub name: String,
    pub addresses: Vec<Ipv4Addr>,
    pub connect_count: u64,
}

/// Longest `ConnectCount` accepted: every `u64` fits in 20 digits.
const MAX_COUNT_DIGITS: usize = 20;

/// Requires `IPv4.InterfaceName` (a `utun` name) and a non-empty
/// `IPv4.Addresses`. Every other `InterfaceName` anywhere in the status must
/// name the same interface; if they disagree, which one carries the tunnel is
/// ambiguous and the status is rejected. `ServerAddress` and `RemoteAddress`
/// are never used: for the official app they are `127.0.0.1`, not a peer.
pub fn tunnel_interface(status: &NcStatus) -> Result<TunnelInterface, Malformed> {
    let extended = status
        .extended
        .as_ref()
        .ok_or(Malformed("no extended status"))?;
    let Some(Node::Dict(ipv4)) = extended.get("IPv4") else {
        return Err(Malformed("no IPv4 dictionary"));
    };
    let Some(Node::Scalar(name)) = ipv4.get("InterfaceName") else {
        return Err(Malformed("no IPv4.InterfaceName"));
    };
    if !is_utun(name) {
        return Err(Malformed("IPv4.InterfaceName is not a utun interface"));
    }
    let Some(Node::Array(items)) = ipv4.get("Addresses") else {
        return Err(Malformed("no IPv4.Addresses"));
    };
    if items.is_empty() {
        return Err(Malformed("IPv4.Addresses is empty"));
    }
    let addresses = items
        .iter()
        .map(|item| match item {
            Node::Scalar(text) => text.parse::<Ipv4Addr>().ok(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Malformed(
            "IPv4.Addresses holds something other than an IPv4 address",
        ))?;
    every_interface_name_is(extended, name)?;
    let connect_count = connect_count(extended)?;
    Ok(TunnelInterface {
        name: name.clone(),
        addresses,
        connect_count,
    })
}

/// The one `ConnectCount` in the extended status, wherever it is nested (the
/// on-device check M1 confirms where the app puts it). Missing, repeated or
/// not a plain decimal number is rejected: a connection whose incarnation
/// cannot be read is never treated as the same connection as before.
fn connect_count(extended: &Dict) -> Result<u64, Malformed> {
    let mut found = Vec::new();
    collect_scalars(extended, "ConnectCount", &mut found);
    let [count] = found.as_slice() else {
        return Err(if found.is_empty() {
            Malformed("no ConnectCount")
        } else {
            Malformed("more than one ConnectCount")
        });
    };
    let Some(Node::Scalar(text)) = count else {
        return Err(Malformed("ConnectCount is not a number"));
    };
    if text.is_empty() || text.len() > MAX_COUNT_DIGITS || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Malformed("ConnectCount is not a number"));
    }
    text.parse()
        .map_err(|_| Malformed("ConnectCount is not a number"))
}

/// Every value stored under `key`, at any depth. Non-scalar values are
/// collected as `None` so that they count as occurrences and are rejected.
fn collect_scalars<'a>(dict: &'a Dict, key: &str, found: &mut Vec<Option<&'a Node>>) {
    for (k, node) in dict.entries() {
        if k == key {
            found.push(matches!(node, Node::Scalar(_)).then_some(node));
        }
        collect_in(node, key, found);
    }
}

fn collect_in<'a>(node: &'a Node, key: &str, found: &mut Vec<Option<&'a Node>>) {
    match node {
        Node::Scalar(_) => {}
        Node::Dict(dict) => collect_scalars(dict, key, found),
        Node::Array(items) => items.iter().for_each(|item| collect_in(item, key, found)),
    }
}

fn every_interface_name_is(dict: &Dict, expected: &str) -> Result<(), Malformed> {
    for (key, node) in dict.entries() {
        if key == "InterfaceName" && *node != Node::Scalar(expected.to_owned()) {
            return Err(Malformed("conflicting InterfaceName values"));
        }
        check_node(node, expected)?;
    }
    Ok(())
}

fn check_node(node: &Node, expected: &str) -> Result<(), Malformed> {
    match node {
        Node::Scalar(_) => Ok(()),
        Node::Dict(dict) => every_interface_name_is(dict, expected),
        Node::Array(items) => items.iter().try_for_each(|item| check_node(item, expected)),
    }
}

/// `utun` followed by 1 to 4 digits without a leading zero (`utun0` aside).
/// Anything accepted here may be passed to `ping -b`.
pub fn is_utun(name: &str) -> bool {
    let Some(digits) = name.strip_prefix("utun") else {
        return false;
    };
    (1..=4).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_state_and_rejects_others() {
        let cases = [
            ("Connected", OsState::Connected),
            ("Disconnected", OsState::Disconnected),
            ("Connecting", OsState::Transitioning),
            ("Disconnecting", OsState::Transitioning),
            ("Reasserting", OsState::Transitioning),
            ("Invalid", OsState::Unknown),
            ("Unknown", OsState::Unknown),
        ];
        for (word, os_state) in cases {
            let status = parse_status(&format!("{word}\n")).unwrap();
            assert_eq!(status.state.os_state(), os_state, "{word}");
        }
        for bad in [
            "connected",
            "Connected ",
            " Connected",
            "Current status = Connected",
            "",
        ] {
            assert!(parse_status(&format!("{bad}\n")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn utun_names() {
        for ok in ["utun0", "utun4", "utun12", "utun9999"] {
            assert!(is_utun(ok), "{ok}");
        }
        for bad in [
            "utun",
            "utun01",
            "utun10000",
            "en0",
            "utun4 ",
            "utun-1",
            "UTUN4",
            "utun4;x",
        ] {
            assert!(!is_utun(bad), "{bad}");
        }
    }
}
