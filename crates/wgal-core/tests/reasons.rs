//! Every refusal of the core carries a reason beside its code (wave three,
//! stage 8): the GUI shows the sentence its language file has for the
//! reason, and the English message stays for the details. One case per
//! reason the core gives. Fakes only: simulation, never a device.

#![allow(clippy::unwrap_used)]

mod support;

use support::{ALL_SUPPORTED, Lab, Rig, T0, binding, id};
use wgal_core::{
    AdapterError, DiscoveredBinding, MemoryStore, PlatformAdapter, ProbeOutcome, ProbeRequest,
    ResolvedInterface, RouteEvidence, Startup, Timing, TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, BotToken, Capability, ChatId, DeviceName, ErrorCode,
    ErrorReason, EventFilter, HostSettings, OsState, Outcome, Request, TargetAddress, codec,
};
use wgal_testkit::{FakeClock, Operation};

/// The code and reason of a refusal.
fn refused(outcome: Outcome) -> (ErrorCode, Option<ErrorReason>) {
    match outcome {
        Outcome::Err(e) => (e.code, e.reason),
        Outcome::Ok(r) => panic!("expected a refusal, got {r:?}"),
    }
}

fn watchdog(lab: Lab, store: &MemoryStore, startup: Startup) -> Watchdog<Lab, FakeClock> {
    Watchdog::new(
        lab,
        FakeClock::new(T0),
        Timing::default(),
        Box::new(store.clone()),
        startup,
    )
}

#[test]
fn an_unknown_tunnel_is_not_registered_or_not_discovered() {
    let mut rig = Rig::new(&["a"]);
    let zzz = id("zzz");
    for request in [
        Request::Connect {
            tunnel_id: zzz.clone(),
        },
        Request::Disconnect {
            tunnel_id: zzz.clone(),
        },
        Request::Reconnect {
            tunnel_id: zzz.clone(),
        },
        Request::ProbeNow {
            tunnel_id: zzz.clone(),
        },
        Request::UnregisterTunnel {
            tunnel_id: zzz.clone(),
        },
        Request::SetTunnelMonitoring {
            tunnel_id: zzz.clone(),
            enabled: false,
        },
        Request::SetTunnelNotifications {
            tunnel_id: zzz.clone(),
            enabled: false,
        },
        Request::SetAutoRecovery {
            tunnel_id: zzz.clone(),
            enabled: false,
        },
    ] {
        assert_eq!(
            refused(rig.request(request.clone())),
            (
                ErrorCode::UnknownTunnel,
                Some(ErrorReason::TunnelNotRegistered)
            ),
            "{request:?}"
        );
    }
    assert_eq!(
        refused(rig.request(Request::RegisterTunnel { tunnel_id: zzz })),
        (
            ErrorCode::UnknownTunnel,
            Some(ErrorReason::TunnelNotDiscovered)
        )
    );
}

#[test]
fn a_tunnel_the_manager_no_longer_has_says_binding_lost() {
    let mut rig = Rig::new(&["a"]);
    rig.lab().fake.fail_next(
        &binding("a"),
        Operation::QueryState,
        AdapterError::new(AdapterErrorKind::NotFound, "service removed"),
    );
    rig.run(20);
    for request in [
        Request::Connect { tunnel_id: id("a") },
        Request::Disconnect { tunnel_id: id("a") },
        Request::Reconnect { tunnel_id: id("a") },
    ] {
        assert_eq!(
            refused(rig.request(request.clone())),
            (ErrorCode::InvalidState, Some(ErrorReason::BindingLost)),
            "{request:?}"
        );
    }
}

#[test]
fn auto_recovery_needs_a_baseline_and_control() {
    let lab = Lab::new(&["a"]);
    lab.kill_target(1);
    let mut rig = Rig::with_lab(lab, &["a"]);
    let enable = Request::SetAutoRecovery {
        tunnel_id: id("a"),
        enabled: true,
    };
    assert_eq!(
        refused(rig.request(enable.clone())),
        (
            ErrorCode::InvalidState,
            Some(ErrorReason::BaselineNotPassed)
        )
    );

    // A baseline passed, but the tunnel cannot be started or stopped here.
    let mut rig = Rig::new(&["a"]);
    rig.lab().fake.set_capabilities(AdapterCapabilities {
        start_stop: Capability::Unsupported,
        ..ALL_SUPPORTED
    });
    assert_eq!(
        refused(rig.request(enable)),
        (ErrorCode::InvalidState, Some(ErrorReason::CannotControl))
    );
}

#[test]
fn an_operation_under_way_says_so() {
    let mut rig = Rig::new(&["a"]);
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    let busy = (ErrorCode::Busy, Some(ErrorReason::OperationInProgress));
    assert_eq!(
        refused(rig.request(Request::Reconnect { tunnel_id: id("a") })),
        busy
    );
    assert_eq!(
        refused(rig.request(Request::UnregisterTunnel { tunnel_id: id("a") })),
        busy
    );
}

#[test]
fn a_change_that_cannot_be_saved_says_save_failed() {
    let lab = Lab::new(&["a", "b"]);
    let mut rig = Rig::with_lab(lab, &["a"]);
    rig.store.set_failing(true);
    let failed = (
        ErrorCode::StorageUnavailable,
        Some(ErrorReason::StorageSaveFailed),
    );
    for request in [
        // `change`
        Request::Disconnect { tunnel_id: id("a") },
        // `change_setting`
        Request::SetTunnelNotifications {
            tunnel_id: id("a"),
            enabled: false,
        },
        Request::SetMonitoringPaused { paused: true },
        Request::UnregisterTunnel { tunnel_id: id("a") },
        // A new registration.
        Request::RegisterTunnel { tunnel_id: id("b") },
    ] {
        assert_eq!(refused(rig.request(request.clone())), failed, "{request:?}");
    }
    // What the runtime gives for its own saves through the core.
    assert!(rig.wd.set_host_settings(settings_with_workers()).is_err());
    assert_eq!(rig.wd.storage_reason(), ErrorReason::StorageSaveFailed);
    assert_eq!(rig.wd.host_settings(), &HostSettings::default());
}

fn settings_with_workers() -> HostSettings {
    HostSettings {
        workers: Some(wgal_protocol::WorkerCount::new(3).unwrap()),
        time_zone: None,
    }
}

#[test]
fn state_that_could_not_be_loaded_says_load_failed() {
    // A tunnel stored next to a record that does not validate: the valid one
    // is restored, but nothing may be written over the files.
    let rig = Rig::new(&["a"]);
    let mut state = rig.store.saved().unwrap();
    let mut damaged = state.tunnels[0].clone();
    damaged.tunnel_id = id("b");
    damaged.binding = "not a binding".to_owned();
    state.tunnels.push(damaged);
    let store = MemoryStore::new();
    let mut wd = watchdog(Lab::new(&["a", "c"]), &store, Startup::Restored(state));
    let not_loaded = (
        ErrorCode::StorageUnavailable,
        Some(ErrorReason::StorageLoadFailed),
    );
    for request in [
        Request::Disconnect { tunnel_id: id("a") },
        Request::SetTunnelNotifications {
            tunnel_id: id("a"),
            enabled: false,
        },
        Request::SetMonitoringPaused { paused: true },
        Request::UnregisterTunnel { tunnel_id: id("a") },
    ] {
        assert_eq!(refused(wd.handle(&request)), not_loaded, "{request:?}");
    }
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        Outcome::Ok(_)
    ));
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel { tunnel_id: id("c") })),
        not_loaded
    );
    assert_eq!(wd.storage_reason(), ErrorReason::StorageLoadFailed);
    assert!(wd.set_host_settings(settings_with_workers()).is_err());
    assert_eq!(wd.storage_reason(), ErrorReason::StorageLoadFailed);
    assert_eq!(store.saved(), None, "nothing was written");

    // Storage that failed at startup.
    let store = MemoryStore::new();
    let mut wd = watchdog(Lab::new(&["a"]), &store, Startup::StorageFailed);
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        Outcome::Ok(_)
    ));
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel { tunnel_id: id("a") })),
        not_loaded
    );
    assert_eq!(wd.storage_reason(), ErrorReason::StorageLoadFailed);
}

#[test]
fn registration_says_why_it_was_refused() {
    let lab = Lab::new(&["other", "open", "a"]);
    lab.fake
        .set_control(&binding("other"), Capability::Unsupported);
    lab.fake.set_registration(
        &binding("open"),
        Err(AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            "writable by others",
        )),
    );
    let mut wd = watchdog(lab, &MemoryStore::new(), Startup::Empty);
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        Outcome::Ok(_)
    ));
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel {
            tunnel_id: id("other")
        })),
        (
            ErrorCode::InvalidState,
            Some(ErrorReason::ManagedByAnotherTool)
        )
    );
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel {
            tunnel_id: id("open")
        })),
        (
            ErrorCode::InvalidState,
            Some(ErrorReason::ConfigNotProtected)
        )
    );
}

#[test]
fn an_id_that_names_another_tunnel_now_is_a_conflict() {
    // Stored for another binding than the one discovery now finds under
    // the same ID.
    let rig = Rig::new(&["a"]);
    let mut state = rig.store.saved().unwrap();
    state.tunnels[0].binding = "elsewhere".to_owned();
    let mut wd = watchdog(
        Lab::new(&["a"]),
        &MemoryStore::new(),
        Startup::Restored(state),
    );
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        Outcome::Ok(_)
    ));
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel { tunnel_id: id("a") })),
        (ErrorCode::InvalidState, Some(ErrorReason::IdConflict))
    );
}

#[test]
fn no_more_than_the_most_tunnels_are_registered() {
    let names: Vec<String> = (0..=wgal_protocol::MAX_TUNNELS)
        .map(|i| format!("t{i:02}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut wd = watchdog(Lab::new(&refs), &MemoryStore::new(), Startup::Empty);
    assert!(matches!(
        wd.handle(&Request::DiscoverTunnels),
        Outcome::Ok(_)
    ));
    let (last, first) = refs.split_last().unwrap();
    for name in first {
        assert!(matches!(
            wd.handle(&Request::RegisterTunnel {
                tunnel_id: id(name)
            }),
            Outcome::Ok(_)
        ));
    }
    assert_eq!(
        refused(wd.handle(&Request::RegisterTunnel {
            tunnel_id: id(last)
        })),
        (ErrorCode::InvalidState, Some(ErrorReason::TooManyTunnels))
    );
}

/// A platform whose official manager cannot be asked anything.
struct ManagerDown;

fn down() -> AdapterError {
    AdapterError::new(AdapterErrorKind::Timeout, "the manager did not answer")
}

impl PlatformAdapter for ManagerDown {
    fn capabilities(&self) -> AdapterCapabilities {
        ALL_SUPPORTED
    }

    fn discover(&self) -> Result<Vec<DiscoveredBinding>, AdapterError> {
        Err(down())
    }

    fn check_registration(&self, _tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        Err(down())
    }

    fn query_state(&self, _tunnel: &TunnelBinding) -> Result<OsState, AdapterError> {
        Err(down())
    }

    fn resolve_interface(
        &self,
        _tunnel: &TunnelBinding,
    ) -> Result<ResolvedInterface, AdapterError> {
        Err(down())
    }

    fn check_route(
        &self,
        _interface: &ResolvedInterface,
        _target: TargetAddress,
    ) -> Result<RouteEvidence, AdapterError> {
        Err(down())
    }

    fn probe(
        &self,
        _interface: &ResolvedInterface,
        _request: &ProbeRequest,
    ) -> Result<ProbeOutcome, AdapterError> {
        Err(down())
    }

    fn start(&self, _tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        Err(down())
    }

    fn stop(&self, _tunnel: &TunnelBinding) -> Result<(), AdapterError> {
        Err(down())
    }
}

#[test]
fn a_manager_that_cannot_be_asked_says_so() {
    let mut wd = Watchdog::new(
        ManagerDown,
        FakeClock::new(T0),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Empty,
    );
    assert_eq!(
        refused(wd.handle(&Request::DiscoverTunnels)),
        (ErrorCode::Internal, Some(ErrorReason::ManagerQueryFailed))
    );
}

/// The built-in templates' request, from the contract's fixture.
fn set_templates() -> Request {
    let fixture =
        include_str!("../../wgal-protocol/fixtures/request_set_notification_templates.json");
    codec::decode_request(fixture.as_bytes()).unwrap().request
}

/// Requests the runtime answers before they reach the core.
#[test]
fn requests_for_the_runtime_are_not_available_here() {
    let mut rig = Rig::new(&["a"]);
    let set_templates = set_templates();
    assert!(matches!(
        set_templates,
        Request::SetNotificationTemplates { .. }
    ));
    for request in [
        Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("Lab laptop").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        },
        Request::SetBotToken {
            token: BotToken::new("123456789:EXAMPLE-not-a-real-token_0000000000").unwrap(),
        },
        Request::ClearBotToken,
        Request::SendTestNotification,
        set_templates,
        Request::UpdateHostSettings {
            settings: settings_with_workers(),
        },
        Request::ReadEventLog {
            filter: EventFilter::All,
            before_seq: None,
        },
    ] {
        assert_eq!(
            refused(rig.request(request.clone())),
            (ErrorCode::Internal, Some(ErrorReason::NotAvailableHere)),
            "{request:?}"
        );
    }
}
