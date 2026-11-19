//! Parsers for `ip -j` (iproute2 JSON) output.
//!
//! Only the fields WGAL relies on are read; iproute2 adds others between
//! versions. Each answer must be the expected shape exactly once: one entry
//! for the asked-for interface or destination, no second guess.

use std::net::Ipv4Addr;

use serde::Deserialize;
use wgal_core::{AdapterError, ResolvedInterface, RouteEvidence};
use wgal_protocol::{AdapterErrorKind, TargetAddress};

use crate::exec::Output;

use super::name::InterfaceName;

/// Most bytes an `ip -j` answer may have before it is refused.
pub const MAX_IP_BYTES: usize = 64 * 1024;

fn parse_error(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::ParseError, detail)
}

fn route_mismatch(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::RouteMismatch, detail)
}

#[derive(Debug, Deserialize)]
struct Link {
    ifindex: u32,
    ifname: String,
    #[serde(default)]
    addr_info: Vec<AddrInfo>,
}

#[derive(Debug, Deserialize)]
struct AddrInfo {
    family: String,
    local: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Route {
    #[serde(rename = "type")]
    kind: Option<String>,
    dst: String,
    dev: Option<String>,
    prefsrc: Option<String>,
}

fn json<'a, T: Deserialize<'a>>(stdout: &'a [u8], what: &str) -> Result<T, AdapterError> {
    serde_json::from_slice(stdout)
        .map_err(|_| parse_error(&format!("{what} is not the expected JSON")))
}

/// `ip -j address show dev <interface>`: the interface's index and, when it
/// has exactly one IPv4 address, that address. With none or several the
/// source is `None` and route checks fail as ambiguous.
pub fn parse_address(
    stdout: &[u8],
    name: &InterfaceName,
) -> Result<ResolvedInterface, AdapterError> {
    let links: Vec<Link> = json(stdout, "ip address output")?;
    let [link] = links.as_slice() else {
        return Err(parse_error("ip address returned other than one interface"));
    };
    if link.ifname != name.as_str() || link.ifindex == 0 {
        return Err(parse_error("ip address returned a different interface"));
    }
    let mut ipv4 = Vec::new();
    for info in link.addr_info.iter().filter(|i| i.family == "inet") {
        let local = info
            .local
            .as_deref()
            .and_then(|a| a.parse::<Ipv4Addr>().ok())
            .ok_or_else(|| parse_error("ip address has an IPv4 entry without a valid address"))?;
        ipv4.push(local);
    }
    Ok(ResolvedInterface {
        name: link.ifname.clone(),
        index: Some(link.ifindex),
        source: match ipv4.as_slice() {
            [only] => Some(*only),
            _ => None,
        },
        incarnation: Some(u64::from(link.ifindex)),
    })
}

/// `ip -j link show type wireguard`: names of the WireGuard interfaces that
/// exist now. Older iproute2 prints nothing instead of `[]` when there are
/// none.
pub fn parse_wireguard_links(stdout: &[u8]) -> Result<Vec<String>, AdapterError> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let links: Vec<Link> = json(stdout, "ip link output")?;
    if links.iter().any(|l| l.ifindex == 0 || l.ifname.is_empty()) {
        return Err(parse_error(
            "ip link returned an interface without index or name",
        ));
    }
    Ok(links.into_iter().map(|l| l.ifname).collect())
}

/// `ip -j route get <target> [oif <interface>]`: the route must be an
/// ordinary unicast route to `target` that leaves through `interface` from
/// its own address. Anything else is a `RouteMismatch`, including a route
/// through another interface, another source address, or a local route.
pub fn parse_route(
    stdout: &[u8],
    target: TargetAddress,
    interface: &ResolvedInterface,
) -> Result<RouteEvidence, AdapterError> {
    let routes: Vec<Route> = json(stdout, "ip route output")?;
    let [route] = routes.as_slice() else {
        return Err(parse_error("ip route returned other than one route"));
    };
    if route.dst.parse::<Ipv4Addr>().ok() != Some(target.get()) {
        return Err(parse_error("ip route answered for a different destination"));
    }
    if !matches!(route.kind.as_deref(), None | Some("unicast")) {
        return Err(route_mismatch("target has no unicast route"));
    }
    if route.dev.as_deref() != Some(interface.name.as_str()) {
        return Err(route_mismatch(
            "target does not route through this interface",
        ));
    }
    let Some(expected) = interface.source else {
        return Err(route_mismatch(
            "interface has no single IPv4 address to verify",
        ));
    };
    let prefsrc = route
        .prefsrc
        .as_deref()
        .map(|a| {
            a.parse::<Ipv4Addr>()
                .map_err(|_| parse_error("ip route has an invalid prefsrc"))
        })
        .transpose()?;
    if prefsrc != Some(expected) {
        return Err(route_mismatch("route uses a different source address"));
    }
    Ok(RouteEvidence {
        target,
        interface_name: interface.name.clone(),
        interface_index: interface.index,
        source: prefsrc,
    })
}

/// An `ip` run that exited non-zero. Classified from its (C locale) error
/// text; the text itself is never copied into the error. `route get` answers
/// an unreachable route with "Network is unreachable" and a prohibit route
/// with "Permission denied": both mean the target is not reached through the
/// tunnel, not that `ip` lacked privileges ("Operation not permitted").
pub fn classify_failure(output: &Output) -> AdapterError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("does not exist") || stderr.contains("Cannot find device") {
        AdapterError::new(AdapterErrorKind::NotFound, "interface no longer exists")
    } else if stderr.contains("Network is unreachable")
        || stderr.contains("No route to host")
        || stderr.contains("Permission denied")
    {
        route_mismatch("no route to the target through this interface")
    } else if stderr.contains("Operation not permitted") {
        AdapterError::new(AdapterErrorKind::PermissionDenied, "ip was not permitted")
    } else {
        parse_error("ip failed with an unrecognised error")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    fn name(n: &str) -> InterfaceName {
        InterfaceName::new(n).unwrap()
    }

    fn target(a: u8, b: u8, c: u8, d: u8) -> TargetAddress {
        TargetAddress::new(Ipv4Addr::new(a, b, c, d)).unwrap()
    }

    fn office() -> ResolvedInterface {
        parse_address(
            fixture("ip-address-wg-office.json").as_bytes(),
            &name("wg-office"),
        )
        .unwrap()
    }

    fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
        result.unwrap_err().kind
    }

    #[test]
    fn reads_index_and_the_single_ipv4_address() {
        assert_eq!(
            office(),
            ResolvedInterface {
                name: "wg-office".to_owned(),
                index: Some(7),
                source: Some(Ipv4Addr::new(192, 0, 2, 11)),
                incarnation: Some(7),
            }
        );
    }

    #[test]
    fn several_ipv4_addresses_leave_the_source_unknown() {
        let resolved = parse_address(
            fixture("ip-address-two-ipv4.json").as_bytes(),
            &name("wg-lab"),
        )
        .unwrap();
        assert_eq!(resolved.index, Some(8));
        assert_eq!(resolved.source, None);
    }

    #[test]
    fn rejects_other_interfaces_missing_fields_and_garbage() {
        let cases = [
            ("ip-address-wg-office.json", "wg-lab"),
            ("ip-address-missing-ifindex.json", "wg-office"),
            ("ip-address-bad-ipv4.json", "wg-office"),
            ("ip-garbage.txt", "wg-office"),
        ];
        for (file, n) in cases {
            assert_eq!(
                kind(parse_address(fixture(file).as_bytes(), &name(n))),
                AdapterErrorKind::ParseError,
                "{file}"
            );
        }
        for raw in ["[]", "{}", "", "[{\"ifindex\":0,\"ifname\":\"wg-office\"}]"] {
            assert_eq!(
                kind(parse_address(raw.as_bytes(), &name("wg-office"))),
                AdapterErrorKind::ParseError,
                "{raw:?}"
            );
        }
        let two = format!(
            "[{0},{0}]",
            r#"{"ifindex":7,"ifname":"wg-office","addr_info":[]}"#
        );
        assert_eq!(
            kind(parse_address(two.as_bytes(), &name("wg-office"))),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn every_truncation_of_json_is_rejected() {
        let full = fixture("ip-address-wg-office.json");
        let full = full.trim_end();
        for cut in 0..full.len() {
            assert!(
                parse_address(&full.as_bytes()[..cut], &name("wg-office")).is_err(),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn route_through_the_tunnel_with_its_own_source_is_evidence() {
        let evidence = parse_route(
            fixture("ip-route-via-wg-office.json").as_bytes(),
            target(192, 0, 2, 1),
            &office(),
        )
        .unwrap();
        assert_eq!(evidence.interface_name, "wg-office");
        assert_eq!(evidence.interface_index, Some(7));
        assert_eq!(evidence.source, Some(Ipv4Addr::new(192, 0, 2, 11)));
    }

    #[test]
    fn any_other_path_is_a_route_mismatch() {
        for file in [
            "ip-route-via-eth.json",
            "ip-route-via-wg-lab.json",
            "ip-route-prefsrc-mismatch.json",
            "ip-route-no-prefsrc.json",
            "ip-route-local.json",
        ] {
            assert_eq!(
                kind(parse_route(
                    fixture(file).as_bytes(),
                    target(192, 0, 2, 1),
                    &office()
                )),
                AdapterErrorKind::RouteMismatch,
                "{file}"
            );
        }
        let mut no_source = office();
        no_source.source = None;
        assert_eq!(
            kind(parse_route(
                fixture("ip-route-via-wg-office.json").as_bytes(),
                target(192, 0, 2, 1),
                &no_source
            )),
            AdapterErrorKind::RouteMismatch
        );
    }

    #[test]
    fn malformed_route_answers_are_parse_errors() {
        for file in ["ip-route-two-routes.json", "ip-garbage.txt"] {
            assert_eq!(
                kind(parse_route(
                    fixture(file).as_bytes(),
                    target(192, 0, 2, 1),
                    &office()
                )),
                AdapterErrorKind::ParseError,
                "{file}"
            );
        }
        // An answer for another destination does not prove anything about this one.
        assert_eq!(
            kind(parse_route(
                fixture("ip-route-via-wg-office.json").as_bytes(),
                target(192, 0, 2, 2),
                &office()
            )),
            AdapterErrorKind::ParseError
        );
    }

    #[test]
    fn lists_wireguard_links() {
        let links = parse_wireguard_links(fixture("ip-link-wireguard.json").as_bytes()).unwrap();
        assert_eq!(links, ["wg-office", "nm-guest", "wg-home"]);
        assert!(parse_wireguard_links(b"").unwrap().is_empty());
        assert!(parse_wireguard_links(b"[]\n").unwrap().is_empty());
        assert!(parse_wireguard_links(b"[{\"ifname\":\"wg0\"}]").is_err());
    }

    #[test]
    fn classifies_ip_failures_without_copying_them() {
        let failed = |stderr: &str| Output {
            exit_code: Some(1),
            stderr: stderr.as_bytes().to_vec(),
            ..Output::default()
        };
        let cases = [
            (
                "Device \"wg-gone\" does not exist.\n",
                AdapterErrorKind::NotFound,
            ),
            (
                "Cannot find device \"wg-gone\"\n",
                AdapterErrorKind::NotFound,
            ),
            (
                "RTNETLINK answers: Network is unreachable\n",
                AdapterErrorKind::RouteMismatch,
            ),
            (
                "RTNETLINK answers: Permission denied\n",
                AdapterErrorKind::RouteMismatch,
            ),
            (
                "RTNETLINK answers: Operation not permitted\n",
                AdapterErrorKind::PermissionDenied,
            ),
            ("something new\n", AdapterErrorKind::ParseError),
        ];
        for (stderr, expected) in cases {
            let error = classify_failure(&failed(stderr));
            assert_eq!(error.kind, expected, "{stderr:?}");
            assert!(!error.detail.as_str().contains("wg-gone"));
        }
    }
}
