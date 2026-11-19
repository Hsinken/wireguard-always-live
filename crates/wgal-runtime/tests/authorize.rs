//! Request authorization (plan §3.2, §9.2; T24) over every request kind,
//! against the real core with a fake platform. Simulation only.

#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use wgal_core::{MemoryStore, Startup};
use wgal_protocol::{ErrorCode, ErrorReason, OsState, Request};
use wgal_runtime::ipc::{AccessList, Caller, authorize, named_tunnel};
use wgal_testkit::FakeClock;

use support::*;

/// What a request needs from its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    Allowed,
    /// `allowed`, and `elevated` or the tunnel on the access list.
    ListedOrElevated,
}

/// The policy, written out per request kind. No wildcard: a new request
/// does not compile here until its rule is decided.
fn needs(request: &Request) -> Needs {
    match request {
        Request::RegisterTunnel { .. } => Needs::ListedOrElevated,
        Request::SetAutoRecovery { enabled, .. }
        | Request::SetStartWhenStoppedExternally { enabled, .. } => {
            if *enabled {
                Needs::ListedOrElevated
            } else {
                Needs::Allowed
            }
        }
        Request::GetSnapshot
        | Request::DiscoverTunnels
        | Request::UnregisterTunnel { .. }
        | Request::UpdateMonitorSettings { .. }
        | Request::SetTunnelMonitoring { .. }
        | Request::ProbeNow { .. }
        | Request::Connect { .. }
        | Request::Disconnect { .. }
        | Request::Reconnect { .. }
        | Request::SetTunnelNotifications { .. }
        | Request::SetRecoveryRetry { .. }
        | Request::UpdateNotificationSettings { .. }
        | Request::SetBotToken { .. }
        | Request::ClearBotToken
        | Request::SetMonitoringPaused { .. }
        | Request::SetMcpEnabled { .. }
        | Request::SetMcpTelegramEnabled { .. }
        | Request::SendTestNotification
        | Request::SetNotificationTemplates { .. }
        | Request::SetCliLanguage { .. }
        | Request::UpdateHostSettings { .. }
        | Request::ReadEventLog { .. }
        | Request::ExportSettings
        | Request::ImportSettings { .. } => Needs::Allowed,
    }
}

/// Whether the request names a tunnel, written out per kind.
fn names_a_tunnel(request: &Request) -> bool {
    match request {
        Request::RegisterTunnel { .. }
        | Request::UnregisterTunnel { .. }
        | Request::UpdateMonitorSettings { .. }
        | Request::SetTunnelMonitoring { .. }
        | Request::ProbeNow { .. }
        | Request::Connect { .. }
        | Request::Disconnect { .. }
        | Request::Reconnect { .. }
        | Request::SetAutoRecovery { .. }
        | Request::SetStartWhenStoppedExternally { .. }
        | Request::SetTunnelNotifications { .. }
        | Request::SetRecoveryRetry { .. } => true,
        // The event log is only filtered by the tunnel it names.
        Request::GetSnapshot
        | Request::DiscoverTunnels
        | Request::UpdateNotificationSettings { .. }
        | Request::SetBotToken { .. }
        | Request::ClearBotToken
        | Request::SetMonitoringPaused { .. }
        | Request::SetMcpEnabled { .. }
        | Request::SetMcpTelegramEnabled { .. }
        | Request::SendTestNotification
        | Request::SetNotificationTemplates { .. }
        | Request::SetCliLanguage { .. }
        | Request::UpdateHostSettings { .. }
        | Request::ReadEventLog { .. }
        | Request::ExportSettings
        | Request::ImportSettings { .. } => false,
    }
}

#[test]
fn the_sample_has_every_request_kind() {
    let kinds: BTreeSet<&str> = every_request("alpha").iter().map(kind).collect();
    // Raise this with a new request kind, after adding it to the sample.
    assert_eq!(kinds.len(), 27);
}

fn callers() -> Vec<Caller> {
    vec![stranger(false), stranger(true), user(), admin()]
}

/// `alpha` registered and discovered, `beta` discovered only.
fn rig() -> Wd {
    let lab = Lab::new(&["alpha"]);
    lab.fake()
        .add_tunnel(binding("beta"), "Tunnel beta", OsState::Connected);
    let clock = Arc::new(FakeClock::new(T0));
    let mut wd = prepared(&lab, &clock, &MemoryStore::new(), &["alpha"]);
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        wgal_protocol::Outcome::Ok(_)
    ));
    wd
}

fn list(names: &[&str]) -> AccessList {
    AccessList {
        tunnels: names.iter().map(|n| binding(n)).collect(),
    }
}

#[test]
fn every_request_against_every_caller_and_list() {
    let wd = rig();
    for name in ["alpha", "beta"] {
        for access in [list(&[]), list(&[name]), list(&["gamma"])] {
            let listed = access.tunnels.contains(&binding(name));
            for caller in callers() {
                for request in every_request(name) {
                    let result = authorize(&caller, &access, &wd, &request);
                    // `beta` is not registered, so the automatic options have
                    // no binding to find on the list.
                    let lookup = match request {
                        Request::SetAutoRecovery { .. }
                        | Request::SetStartWhenStoppedExternally { .. } => {
                            listed && name == "alpha"
                        }
                        _ => listed,
                    };
                    let want = caller.allowed
                        && match needs(&request) {
                            Needs::Allowed => true,
                            Needs::ListedOrElevated => caller.elevated || lookup,
                        };
                    assert_eq!(
                        result.is_ok(),
                        want,
                        "{caller:?} {} {name} listed={listed}",
                        kind(&request)
                    );
                    if let Err(e) = result {
                        assert_eq!(e.code, ErrorCode::Unauthorized);
                        assert_eq!(
                            e.reason,
                            Some(if caller.allowed {
                                ErrorReason::NeedsAdministratorOrListedTunnel
                            } else {
                                ErrorReason::CallerNotAllowed
                            })
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn requests_name_their_tunnel_and_nothing_else() {
    for request in every_request("alpha") {
        assert_eq!(
            named_tunnel(&request),
            names_a_tunnel(&request).then(|| id("alpha")).as_ref(),
            "{}",
            kind(&request)
        );
    }
}

#[test]
fn an_undiscovered_tunnel_cannot_be_registered_without_rights() {
    let wd = rig();
    let request = Request::RegisterTunnel {
        tunnel_id: id("gamma"),
    };
    let access = list(&["gamma"]);
    assert_eq!(
        authorize(&user(), &access, &wd, &request).map_err(|e| (e.code, e.reason)),
        Err((
            ErrorCode::Unauthorized,
            Some(ErrorReason::NeedsAdministratorOrListedTunnel)
        ))
    );
    assert!(authorize(&admin(), &access, &wd, &request).is_ok());
}

#[test]
fn the_list_is_checked_against_the_binding_not_the_id() {
    // `office.vpn` is a valid binding but not a valid ID, so its ID is a
    // sanitized form with a hash suffix.
    let lab = Lab::new(&[]);
    lab.fake()
        .add_tunnel(binding("office.vpn"), "Office", OsState::Connected);
    let clock = Arc::new(FakeClock::new(T0));
    let mut wd = watchdog(&lab, &clock, &MemoryStore::new(), Startup::Empty);
    let discovered = match wd.handle(&Request::DiscoverTunnels) {
        wgal_protocol::Outcome::Ok(wgal_protocol::Response::Discovered { tunnels }) => tunnels,
        other => panic!("{other:?}"),
    };
    let tunnel_id = discovered.iter().next().unwrap().tunnel_id.clone();
    assert_ne!(tunnel_id.as_str(), "office.vpn");
    let request = Request::RegisterTunnel { tunnel_id };
    assert!(authorize(&user(), &list(&["office.vpn"]), &wd, &request).is_ok());
    let by_id_like = AccessList {
        tunnels: [binding("office_vpn")].into_iter().collect(),
    };
    assert!(authorize(&user(), &by_id_like, &wd, &request).is_err());
}

#[test]
fn a_caller_never_prints_its_account() {
    let caller = user();
    let text = format!("{caller:?}");
    assert!(!text.contains(&caller.account), "{text}");
    assert!(text.contains("allowed"));
}
