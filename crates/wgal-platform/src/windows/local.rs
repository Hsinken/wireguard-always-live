//! Whether this host itself is offline (open finding D-3, plan §4.1, §6.3),
//! and which interface changes are changes of the host's network (plan
//! §6.2). Everything here is a pure function over plain data from the IP
//! Helper API, so it is unit-tested on every OS; the adapter gathers the data
//! only after a probe failed, and `sys.rs` reads it from Windows.
//!
//! # The way out
//!
//! The host is judged offline only on positive evidence that it has no way
//! out (Codex review of wave three, P1-4). A missing IPv4 default route is
//! not such evidence: a WireGuard endpoint may sit on the local network, be
//! reached by a specific route, or over IPv6, none of which needs
//! `0.0.0.0/0`.
//!
//! An *uplink* is an adapter that is operationally up, is not of a tunnel
//! type, and holds a usable unicast address ([`is_usable`]: IPv4 or IPv6,
//! not unspecified, loopback, link-local, multicast or broadcast). Adapters
//! of these types never carry the host's own way out, whatever addresses
//! and routes they hold, and are left out by their type alone
//! ([`is_tunnel_type`]):
//!
//! - `IF_TYPE_SOFTWARE_LOOPBACK` (24);
//! - `IF_TYPE_PROP_VIRTUAL` (53): the adapters of WireGuard for Windows
//!   (WireGuardNT and Wintun, confirmed as 53 on a device in W1) and of many
//!   other VPNs. A full-tunnel WireGuard configuration puts its own default
//!   route on this adapter; it must not count as a way out, or a host whose
//!   Wi-Fi is gone would look online through the tunnel that depends on it;
//! - `IF_TYPE_TUNNEL` (131): Teredo, IP-HTTPS, 6to4, ISATAP and some VPNs.
//!
//! Every other type counts, so an uplink of a type not listed here is never
//! mistaken for a tunnel: the check can then only fail to see an outage,
//! never invent one. That includes adapters that are always up, such as the
//! host side of a virtual switch (Hyper-V, WSL, VMware, VirtualBox): with
//! one of them up, a host whose Wi-Fi is gone still has an uplink, and is
//! judged by its gateways alone.
//!
//! The host is offline in exactly two cases:
//!
//! 1. There is no uplink at all ([`Offline::NoUplink`]): Wi-Fi that lost
//!    its access point and a cable pulled out leave the adapter down, and an
//!    adapter whose DHCP failed has only link-local addresses.
//! 2. There are default gateways on the uplinks, and every one of them
//!    stayed silent ([`Offline::GatewaysSilent`]). A default route
//!    (`0.0.0.0/0` or `::/0`) on an uplink names its gateway, which is asked
//!    for its link-layer address (ARP for IPv4, neighbor discovery for IPv6)
//!    within a short deadline. A wired link, or Wi-Fi through an access
//!    point that stays up, to a router that is rebooting keeps its routes,
//!    but the router answers neither until it is back.
//!
//! Anything else counts as online, and the probe's own result stands, as
//! before this check existed (plan §6.3: never conclude that the whole host
//! is offline from too little):
//!
//! - uplinks without any default gateway (a LAN-only network, specific
//!   routes, an IPv6-only network without a router);
//! - a default route on a point-to-point link ([`is_point_to_point`]: PPP
//!   23, mobile broadband 243 and 244), which has no ARP, or with no next
//!   hop (on-link), which cannot be checked at the link layer either;
//! - more gateways than are checked, and anything the check cannot read or
//!   cannot decide.
//!
//! What it cannot see: a router that answers while its own upstream (the
//! ISP) is still down. The probe failure is then reported as it is, and the
//! fault confirmation thresholds and the grace period after a network change
//! are what keep a short ISP outage from counting as a tunnel fault.

use std::net::IpAddr;

use serde::Deserialize;

use super::net::{AdapterInfo, IF_TYPE_PROP_VIRTUAL};

/// `IF_TYPE_SOFTWARE_LOOPBACK`.
pub const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
/// `IF_TYPE_TUNNEL`.
pub const IF_TYPE_TUNNEL: u32 = 131;
/// `IF_TYPE_PPP`.
pub const IF_TYPE_PPP: u32 = 23;
/// `IF_TYPE_WWANPP` and `IF_TYPE_WWANPP2`: mobile broadband.
pub const IF_TYPE_WWANPP: u32 = 243;
pub const IF_TYPE_WWANPP2: u32 = 244;

/// Most default routes one reading may return; more fails instead of
/// truncating.
pub const MAX_DEFAULT_ROUTES: usize = 64;

/// Most gateways checked at once. A host with more distinct default
/// gateways than this is not judged offline at all.
pub const MAX_GATEWAYS: usize = 8;

/// Whether an interface of this type is loopback, a tunnel or a virtual
/// adapter, which never carries the host's own way out.
pub fn is_tunnel_type(if_type: u32) -> bool {
    matches!(
        if_type,
        IF_TYPE_SOFTWARE_LOOPBACK | IF_TYPE_PROP_VIRTUAL | IF_TYPE_TUNNEL
    )
}

/// Whether an interface of this type is a point-to-point link without ARP.
pub fn is_point_to_point(if_type: u32) -> bool {
    matches!(if_type, IF_TYPE_PPP | IF_TYPE_WWANPP | IF_TYPE_WWANPP2)
}

/// Whether a unicast address can carry the host's traffic off its own link:
/// not unspecified, loopback, link-local (`169.254.0.0/16`, `fe80::/10`),
/// multicast or broadcast.
pub fn is_usable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(a) => {
            !(a.is_unspecified()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_multicast()
                || a.is_broadcast())
        }
        IpAddr::V6(a) => {
            !(a.is_unspecified()
                || a.is_loopback()
                || a.is_unicast_link_local()
                || a.is_multicast())
        }
    }
}

/// Whether the adapter is an uplink: up, not a tunnel type, and holding a
/// usable unicast address of either family.
pub fn is_uplink(adapter: &AdapterInfo) -> bool {
    adapter.up
        && !is_tunnel_type(adapter.if_type)
        && (adapter.ipv4.iter().any(|a| is_usable((*a).into()))
            || adapter.ipv6.iter().any(|a| is_usable((*a).into())))
}

/// The interface type Windows keeps in the top 16 bits of every LUID
/// (`NET_LUID_LH.Info.IfType`), so a change notice can be judged without
/// looking the interface up.
pub fn luid_if_type(luid: u64) -> u32 {
    u32::from((luid >> 48) as u16)
}

/// One default route, IPv4 (`0.0.0.0/0`) or IPv6 (`::/0`), from
/// `GetIpForwardTable2`. `next_hop` is unspecified (`0.0.0.0` or `::`) for
/// an on-link route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultRoute {
    pub if_index: u32,
    pub luid: u64,
    pub next_hop: IpAddr,
}

/// A default gateway on one interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gateway {
    pub luid: u64,
    pub address: IpAddr,
}

/// What the adapters and the routing table offer as a way off this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WayOut {
    /// No uplink: no adapter that is up and not a tunnel holds a usable
    /// address.
    None,
    /// Nothing that could prove the host offline: uplinks without any
    /// default gateway, a default route that cannot be checked at the link
    /// layer (a point-to-point link or an on-link route), or more gateways
    /// than are checked. Taken as a way out.
    Unchecked,
    /// Default routes on uplinks through these gateways, each listed once.
    Gateways(Vec<Gateway>),
}

/// Reads the way out from the host's adapters (both families) and its
/// default routes (both families). A route counts only on an uplink,
/// matched by LUID; a route whose adapter is not listed is left out. With
/// no uplink the routes are not needed, and may be empty.
pub fn way_out(adapters: &[AdapterInfo], routes: &[DefaultRoute]) -> WayOut {
    if !adapters.iter().any(is_uplink) {
        return WayOut::None;
    }
    let mut gateways: Vec<Gateway> = Vec::new();
    for route in routes {
        let Some(adapter) = adapters.iter().find(|a| a.luid == route.luid) else {
            continue;
        };
        if !is_uplink(adapter) {
            continue;
        }
        if is_point_to_point(adapter.if_type) || route.next_hop.is_unspecified() {
            return WayOut::Unchecked;
        }
        let gateway = Gateway {
            luid: route.luid,
            address: route.next_hop,
        };
        if !gateways.contains(&gateway) {
            gateways.push(gateway);
        }
    }
    if gateways.is_empty() || gateways.len() > MAX_GATEWAYS {
        WayOut::Unchecked
    } else {
        WayOut::Gateways(gateways)
    }
}

/// What one gateway's link-layer check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// It answered ARP.
    Answered,
    /// It did not answer within the deadline.
    Silent,
    /// The check could not be made or could not tell (a static neighbor
    /// entry, which is never resolved, or any other error).
    Unknown,
}

/// Why the host is judged offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offline {
    /// No uplink: no adapter that is up and not a tunnel holds a usable
    /// address.
    NoUplink,
    /// Every default gateway on the uplinks stayed silent.
    GatewaysSilent,
}

impl Offline {
    /// The adapter error detail. It names no address or interface.
    pub fn detail(self) -> &'static str {
        match self {
            Self::NoUplink => {
                "this computer is offline: no network adapter that is not a tunnel is up with a \
                 usable address"
            }
            Self::GatewaysSilent => {
                "this computer is offline: its default gateway does not answer on the local \
                 network"
            }
        }
    }
}

/// The verdict on the host's own network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalNetwork {
    /// There is a way out, or nothing proves there is none.
    Online,
    Offline(Offline),
}

/// The verdict once the gateways of [`WayOut::Gateways`] have been checked:
/// offline only if every one of them stayed silent.
pub fn judge_gateways(resolutions: &[Resolution]) -> LocalNetwork {
    if !resolutions.is_empty() && resolutions.iter().all(|r| *r == Resolution::Silent) {
        LocalNetwork::Offline(Offline::GatewaysSilent)
    } else {
        LocalNetwork::Online
    }
}

/// One notice of a change to an IPv4 interface (`NotifyIpInterfaceChange`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceChange {
    pub luid: u64,
}

impl InterfaceChange {
    /// Whether the change is on an interface that can carry the host's way
    /// out. Changes on loopback, tunnel and virtual adapters are not: WGAL's
    /// own tunnel starts and stops are not network changes.
    pub fn is_network_change(self) -> bool {
        !is_tunnel_type(luid_if_type(self.luid))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    const ETHERNET: u32 = 6;
    const WIFI: u32 = 71;

    /// A LUID as Windows builds it: type in the top 16 bits, then the
    /// 24-bit index, then 24 reserved bits.
    fn luid(if_type: u32, index: u64) -> u64 {
        (u64::from(if_type) << 48) | (index << 24)
    }

    /// An adapter with one usable IPv4 address of its own.
    fn adapter(name: &str, if_type: u32, index: u32, up: bool) -> AdapterInfo {
        AdapterInfo {
            friendly_name: name.to_owned(),
            if_type,
            if_index: index,
            luid: luid(if_type, u64::from(index)),
            up,
            ipv4: vec![Ipv4Addr::new(192, 0, 2, u8::try_from(index).unwrap())],
            ipv6: Vec::new(),
        }
    }

    const ROUTER_V6: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);

    fn route_v6(adapter: &AdapterInfo, next_hop: Ipv6Addr) -> DefaultRoute {
        DefaultRoute {
            if_index: adapter.if_index,
            luid: adapter.luid,
            next_hop: next_hop.into(),
        }
    }

    fn route(adapter: &AdapterInfo, next_hop: [u8; 4]) -> DefaultRoute {
        DefaultRoute {
            if_index: adapter.if_index,
            luid: adapter.luid,
            next_hop: Ipv4Addr::from(next_hop).into(),
        }
    }

    fn gateway(adapter: &AdapterInfo, address: [u8; 4]) -> Gateway {
        Gateway {
            luid: adapter.luid,
            address: Ipv4Addr::from(address).into(),
        }
    }

    #[test]
    fn the_luid_carries_the_interface_type() {
        for if_type in [ETHERNET, WIFI, IF_TYPE_PROP_VIRTUAL, IF_TYPE_TUNNEL, 0xffff] {
            assert_eq!(luid_if_type(luid(if_type, 0xab_cdef)), if_type);
        }
        // The fixture's adapters, built the same way.
        assert_eq!(luid_if_type(1_688_849_877_467_136), ETHERNET);
        assert_eq!(
            luid_if_type(6_755_399_441_055_744),
            IF_TYPE_SOFTWARE_LOOPBACK
        );
        assert_eq!(luid_if_type(14_918_173_849_550_848), IF_TYPE_PROP_VIRTUAL);
    }

    #[test]
    fn a_default_route_through_a_physical_adapter_is_checked_at_its_gateway() {
        for if_type in [ETHERNET, WIFI] {
            let lan = adapter("uplink", if_type, 7, true);
            assert_eq!(
                way_out(std::slice::from_ref(&lan), &[route(&lan, [192, 0, 2, 1])]),
                WayOut::Gateways(vec![gateway(&lan, [192, 0, 2, 1])])
            );
        }
    }

    #[test]
    fn no_uplink_is_no_way_out() {
        assert_eq!(way_out(&[], &[]), WayOut::None);
        let mut lan = adapter("Ethernet", ETHERNET, 7, true);
        lan.ipv4.clear();
        assert_eq!(
            way_out(std::slice::from_ref(&lan), &[route(&lan, [192, 0, 2, 1])]),
            WayOut::None
        );
    }

    /// Codex review of wave three, P1-4: an uplink without any default route
    /// may still reach the endpoint on the local network, by a specific
    /// route, or over IPv6. Nothing proves it offline.
    #[test]
    fn p1_4_an_uplink_without_a_default_gateway_is_not_judged() {
        let lan = adapter("Ethernet", ETHERNET, 7, true);
        assert_eq!(way_out(std::slice::from_ref(&lan), &[]), WayOut::Unchecked);
        let mut ipv6_only = adapter("Ethernet", ETHERNET, 7, true);
        ipv6_only.ipv4 = vec![Ipv4Addr::new(169, 254, 7, 7)];
        ipv6_only.ipv6 = vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)];
        assert_eq!(way_out(&[ipv6_only], &[]), WayOut::Unchecked);
        // A default route on an adapter that is no uplink does not count.
        let mut dhcp_failed = adapter("Wi-Fi", WIFI, 9, true);
        dhcp_failed.ipv4 = vec![Ipv4Addr::new(169, 254, 9, 9)];
        assert_eq!(
            way_out(
                &[lan, dhcp_failed.clone()],
                &[route(&dhcp_failed, [192, 0, 2, 1])]
            ),
            WayOut::Unchecked
        );
    }

    /// P1-4: an IPv6 default route names its router, which is checked like
    /// an IPv4 gateway, and alongside it.
    #[test]
    fn p1_4_ipv6_default_routes_are_checked_at_their_router() {
        let mut lan = adapter("Ethernet", ETHERNET, 7, true);
        lan.ipv6 = vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)];
        assert_eq!(
            way_out(
                std::slice::from_ref(&lan),
                &[route(&lan, [192, 0, 2, 1]), route_v6(&lan, ROUTER_V6)]
            ),
            WayOut::Gateways(vec![
                gateway(&lan, [192, 0, 2, 1]),
                Gateway {
                    luid: lan.luid,
                    address: ROUTER_V6.into(),
                },
            ])
        );
        assert_eq!(
            way_out(
                std::slice::from_ref(&lan),
                &[route_v6(&lan, Ipv6Addr::UNSPECIFIED)]
            ),
            WayOut::Unchecked
        );
    }

    #[test]
    fn only_addresses_that_leave_the_link_are_usable() {
        for usable in [
            IpAddr::from([192, 0, 2, 10]),
            IpAddr::from([10, 0, 0, 2]),
            IpAddr::from(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 10)),
            // Unique local.
            IpAddr::from(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 10)),
        ] {
            assert!(is_usable(usable), "{usable}");
        }
        for unusable in [
            IpAddr::from([0, 0, 0, 0]),
            IpAddr::from([127, 0, 0, 1]),
            IpAddr::from([169, 254, 3, 4]),
            IpAddr::from([224, 0, 0, 1]),
            IpAddr::from([255, 255, 255, 255]),
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
            IpAddr::from(Ipv6Addr::LOCALHOST),
            IpAddr::from(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 10)),
            IpAddr::from(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)),
        ] {
            assert!(!is_usable(unusable), "{unusable}");
        }
    }

    #[test]
    fn an_uplink_is_up_not_a_tunnel_and_has_a_usable_address() {
        let lan = adapter("Ethernet", ETHERNET, 7, true);
        assert!(is_uplink(&lan));
        assert!(!is_uplink(&AdapterInfo {
            up: false,
            ..lan.clone()
        }));
        for if_type in [
            IF_TYPE_PROP_VIRTUAL,
            IF_TYPE_TUNNEL,
            IF_TYPE_SOFTWARE_LOOPBACK,
        ] {
            assert!(!is_uplink(&AdapterInfo {
                if_type,
                ..lan.clone()
            }));
        }
        let link_local_only = AdapterInfo {
            ipv4: vec![Ipv4Addr::new(169, 254, 7, 7)],
            ipv6: vec![Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 7)],
            ..lan.clone()
        };
        assert!(!is_uplink(&link_local_only));
        assert!(is_uplink(&AdapterInfo {
            ipv6: vec![Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)],
            ..link_local_only
        }));
    }

    #[test]
    fn a_route_on_an_adapter_that_is_down_or_unlisted_is_no_way_out() {
        let down = adapter("Wi-Fi", WIFI, 9, false);
        let gone = adapter("gone", ETHERNET, 12, true);
        assert_eq!(
            way_out(
                std::slice::from_ref(&down),
                &[route(&down, [192, 0, 2, 1]), route(&gone, [192, 0, 2, 1])]
            ),
            WayOut::None
        );
    }

    /// A full-tunnel WireGuard configuration routes 0.0.0.0/0 through its
    /// own adapter; with Wi-Fi gone that must not look like a way out.
    #[test]
    fn default_routes_on_tunnels_loopback_and_virtual_adapters_never_count() {
        let wireguard = adapter("office", IF_TYPE_PROP_VIRTUAL, 21, true);
        let teredo = adapter("Teredo", IF_TYPE_TUNNEL, 14, true);
        let loopback = adapter("Loopback", IF_TYPE_SOFTWARE_LOOPBACK, 1, true);
        let wifi_down = adapter("Wi-Fi", WIFI, 9, false);
        let adapters = [
            wireguard.clone(),
            teredo.clone(),
            loopback.clone(),
            wifi_down,
        ];
        let routes = [
            route(&wireguard, [0, 0, 0, 0]),
            route(&teredo, [0, 0, 0, 0]),
            route(&loopback, [127, 0, 0, 1]),
        ];
        assert_eq!(way_out(&adapters, &routes), WayOut::None);
    }

    #[test]
    fn point_to_point_and_on_link_routes_count_without_arp() {
        for if_type in [IF_TYPE_PPP, IF_TYPE_WWANPP, IF_TYPE_WWANPP2] {
            let link = adapter("cellular", if_type, 30, true);
            assert_eq!(
                way_out(std::slice::from_ref(&link), &[route(&link, [0, 0, 0, 0])]),
                WayOut::Unchecked
            );
            assert_eq!(
                way_out(
                    std::slice::from_ref(&link),
                    &[route(&link, [198, 51, 100, 1])]
                ),
                WayOut::Unchecked
            );
        }
        let lan = adapter("Ethernet", ETHERNET, 7, true);
        assert_eq!(
            way_out(std::slice::from_ref(&lan), &[route(&lan, [0, 0, 0, 0])]),
            WayOut::Unchecked
        );
    }

    #[test]
    fn every_gateway_is_listed_once() {
        let lan = adapter("Ethernet", ETHERNET, 7, true);
        let wifi = adapter("Wi-Fi", WIFI, 9, true);
        let adapters = [lan.clone(), wifi.clone()];
        let routes = [
            route(&lan, [192, 0, 2, 1]),
            route(&lan, [192, 0, 2, 1]),
            route(&wifi, [198, 51, 100, 1]),
            route(&wifi, [192, 0, 2, 1]),
        ];
        assert_eq!(
            way_out(&adapters, &routes),
            WayOut::Gateways(vec![
                gateway(&lan, [192, 0, 2, 1]),
                gateway(&wifi, [198, 51, 100, 1]),
                gateway(&wifi, [192, 0, 2, 1]),
            ])
        );
    }

    #[test]
    fn too_many_gateways_are_not_judged() {
        let lan = adapter("Ethernet", ETHERNET, 7, true);
        let routes: Vec<_> = (1..=u8::try_from(MAX_GATEWAYS + 1).unwrap())
            .map(|last| route(&lan, [192, 0, 2, last]))
            .collect();
        assert_eq!(
            way_out(std::slice::from_ref(&lan), &routes),
            WayOut::Unchecked
        );
        assert!(matches!(
            way_out(std::slice::from_ref(&lan), &routes[..MAX_GATEWAYS]),
            WayOut::Gateways(g) if g.len() == MAX_GATEWAYS
        ));
    }

    #[test]
    fn offline_only_when_every_gateway_is_silent() {
        use Resolution::*;
        assert_eq!(
            judge_gateways(&[Silent]),
            LocalNetwork::Offline(Offline::GatewaysSilent)
        );
        assert_eq!(
            judge_gateways(&[Silent, Silent]),
            LocalNetwork::Offline(Offline::GatewaysSilent)
        );
        for online in [
            &[Answered][..],
            &[Silent, Answered],
            &[Unknown],
            &[Silent, Unknown],
            &[],
        ] {
            assert_eq!(judge_gateways(online), LocalNetwork::Online, "{online:?}");
        }
    }

    #[test]
    fn details_name_no_address() {
        for why in [Offline::NoUplink, Offline::GatewaysSilent] {
            let detail = why.detail();
            assert!(detail.starts_with("this computer is offline"));
            assert!(!detail.chars().any(|c| c.is_ascii_digit()), "{detail}");
        }
    }

    #[test]
    fn only_changes_off_tunnels_are_network_changes() {
        for if_type in [ETHERNET, WIFI, IF_TYPE_PPP, IF_TYPE_WWANPP] {
            let change = InterfaceChange {
                luid: luid(if_type, 3),
            };
            assert!(change.is_network_change(), "{if_type}");
        }
        for if_type in [
            IF_TYPE_PROP_VIRTUAL,
            IF_TYPE_TUNNEL,
            IF_TYPE_SOFTWARE_LOOPBACK,
        ] {
            let change = InterfaceChange {
                luid: luid(if_type, 3),
            };
            assert!(!change.is_network_change(), "{if_type}");
        }
    }
}
