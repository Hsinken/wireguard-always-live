//! Adapter tests against anonymized fixtures in `tests/fixtures/macos/`. No
//! process is started: every tool run is answered by a `ScriptedRunner`,
//! which also records the exact argument vectors.

use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::path::PathBuf;

use wgal_protocol::ProbeTarget;

use super::*;
use crate::exec::{ExecError, ScriptedRunner};

const B1: &str = "00000000-0000-4000-8000-0000000000B1";
const B2: &str = "00000000-0000-4000-8000-0000000000B2";
const B3: &str = "00000000-0000-4000-8000-0000000000B3";

fn fixture(name: &str) -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "tests",
        "fixtures",
        "macos",
        name,
    ]
    .iter()
    .collect();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn mac() -> MacosAdapter<ScriptedRunner> {
    MacosAdapter::with_runner(ScriptedRunner::new())
}

fn reply(adapter: &MacosAdapter<ScriptedRunner>, exit_code: i32, stdout: &str) {
    adapter.runner().push_output(exit_code, stdout);
}

fn reply_err(adapter: &MacosAdapter<ScriptedRunner>, exit_code: i32, stderr: &str) {
    adapter.runner().push(Ok(Output {
        exit_code: Some(exit_code),
        stderr: stderr.as_bytes().to_vec(),
        ..Output::default()
    }));
}

fn binding(text: &str) -> TunnelBinding {
    TunnelBinding::new(text).unwrap()
}

fn argv(adapter: &MacosAdapter<ScriptedRunner>) -> Vec<(String, Vec<String>)> {
    adapter
        .runner()
        .calls()
        .into_iter()
        .map(|call| (call.program.path().to_owned(), call.args))
        .collect()
}

fn scutil(args: &[&str]) -> (String, Vec<String>) {
    (
        "/usr/sbin/scutil".to_owned(),
        args.iter().map(|a| (*a).to_owned()).collect(),
    )
}

/// The interface as resolved for the first configuration this adapter saw
/// (slot 0), whose `incarnation` is the plain connect count.
fn utun(name: &str, source: [u8; 4], connect_count: u64) -> ResolvedInterface {
    utun_in(0, name, source, connect_count)
}

/// The interface as resolved for the configuration in `slot`.
fn utun_in(slot: u64, name: &str, source: [u8; 4], connect_count: u64) -> ResolvedInterface {
    ResolvedInterface {
        name: name.to_owned(),
        index: None,
        source: Some(Ipv4Addr::from(source)),
        incarnation: Some((slot << COUNT_BITS) | connect_count),
    }
}

/// Resolves `binding_text` through the status in `status_fixture`, so the
/// adapter knows which configuration owns the interface.
fn resolved(
    adapter: &MacosAdapter<ScriptedRunner>,
    binding_text: &str,
    status_fixture: &str,
) -> ResolvedInterface {
    reply(adapter, 0, &fixture(status_fixture));
    adapter.resolve_interface(&binding(binding_text)).unwrap()
}

/// Queues the status read `check_route` and `probe` use to confirm the
/// interface is still the one they were given.
fn status_now(adapter: &MacosAdapter<ScriptedRunner>, status_fixture: &str) {
    reply(adapter, 0, &fixture(status_fixture));
}

fn target() -> TargetAddress {
    TargetAddress::new(Ipv4Addr::new(192, 0, 2, 1)).unwrap()
}

fn icmp() -> ProbeRequest {
    ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    }
}

fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
    result.expect_err("expected an error").kind
}

// discover

#[test]
fn discovers_wireguard_configurations_with_their_names() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_list_three.txt"));

    let found = adapter.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|d| (d.binding.as_str(), d.display_name.as_str(), d.control))
        .collect();
    assert_eq!(
        summary,
        [
            (B1, "Office", Capability::Supported),
            (B2, "Lab 2", Capability::Supported),
            (B3, "Home", Capability::Supported),
        ]
    );
    assert_eq!(argv(&adapter), [scutil(&["--nc", "list"])]);
}

#[test]
fn leaves_out_other_vpn_services_and_marks_disabled_ones_unknown() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_list_mixed.txt"));

    let found = adapter.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|d| (d.binding.as_str(), d.control))
        .collect();
    assert_eq!(
        summary,
        [
            (B1, Capability::Supported),
            ("00000000-0000-4000-8000-0000000000B4", Capability::Unknown),
        ]
    );
}

#[test]
fn a_bad_list_fails_as_a_whole() {
    for name in ["nc_list_duplicate.txt", "nc_list_interleaved.txt"] {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.discover()),
            AdapterErrorKind::ParseError,
            "{name}"
        );
    }

    let adapter = mac();
    let mut cut = fixture("nc_list_three.txt");
    cut.truncate(cut.len() - 20);
    reply(&adapter, 0, &cut);
    assert_eq!(kind(adapter.discover()), AdapterErrorKind::ParseError);

    let adapter = mac();
    adapter.runner().push(Ok(Output {
        exit_code: Some(0),
        stdout: fixture("nc_list_three.txt").into_bytes(),
        truncated: true,
        ..Output::default()
    }));
    assert_eq!(kind(adapter.discover()), AdapterErrorKind::ParseError);

    let adapter = mac();
    reply(&adapter, 0, "\u{1}garbage\n");
    assert_eq!(kind(adapter.discover()), AdapterErrorKind::ParseError);
}

// query_state

#[test]
fn reports_each_connection_state() {
    let cases = [
        ("nc_status_connected.txt", OsState::Connected),
        ("nc_status_disconnected.txt", OsState::Disconnected),
        ("nc_status_connecting.txt", OsState::Transitioning),
        ("nc_status_disconnecting.txt", OsState::Transitioning),
    ];
    for (name, expected) in cases {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            adapter.query_state(&binding(B1)).unwrap(),
            expected,
            "{name}"
        );
        assert_eq!(argv(&adapter), [scutil(&["--nc", "status", B1])]);
    }
}

#[test]
fn unknown_or_malformed_status_is_a_parse_error_never_disconnected() {
    for name in [
        "nc_status_garbage.txt",
        "nc_status_two_blocks.txt",
        "nc_status_interleaved.txt",
        "nc_status_deep.txt",
        "nc_status_duplicate_key.txt",
    ] {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.query_state(&binding(B1))),
            AdapterErrorKind::ParseError,
            "{name}"
        );
    }
    let adapter = mac();
    reply(&adapter, 0, "Offline\n");
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::ParseError
    );
}

#[test]
fn a_removed_configuration_is_not_found() {
    let adapter = mac();
    reply_err(&adapter, 1, "No service\n");
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::NotFound
    );
}

#[test]
fn lower_case_bindings_reach_scutil_in_upper_case() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_disconnected.txt"));
    adapter.query_state(&binding(&B1.to_lowercase())).unwrap();
    assert_eq!(argv(&adapter), [scutil(&["--nc", "status", B1])]);
}

#[test]
fn a_binding_that_is_not_a_uuid_runs_nothing() {
    let adapter = mac();
    for bad in ["wg0", "Office", "00000000-0000-4000-8000-0000000000B1.x"] {
        let bad = binding(bad);
        assert_eq!(kind(adapter.query_state(&bad)), AdapterErrorKind::NotFound);
        assert_eq!(
            kind(adapter.resolve_interface(&bad)),
            AdapterErrorKind::NotFound
        );
        assert_eq!(kind(adapter.start(&bad)), AdapterErrorKind::NotFound);
        assert_eq!(kind(adapter.stop(&bad)), AdapterErrorKind::NotFound);
    }
    assert!(adapter.runner().calls().is_empty());
}

#[test]
fn tool_failures_are_not_tunnel_states() {
    let adapter = mac();
    adapter.runner().push(Err(ExecError::Timeout));
    adapter.runner().push(Err(ExecError::NotFound));
    adapter.runner().push(Err(ExecError::PermissionDenied));
    reply_err(&adapter, 1, "Could not create connection\n");
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::Timeout
    );
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::Unsupported
    );
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::PermissionDenied
    );
    assert_eq!(
        kind(adapter.query_state(&binding(B1))),
        AdapterErrorKind::ParseError
    );
}

/// The list as macOS 27.2.1 writes it, with a colon in the type
/// (`nc_list_macos27.txt`: the layout of a real list, invented IDs and names).
#[test]
fn discovers_the_list_macos_27_writes() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_list_macos27.txt"));

    let found = adapter.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|d| (d.binding.as_str(), d.display_name.as_str(), d.control))
        .collect();
    assert_eq!(
        summary,
        [
            (B1, "Office", Capability::Supported),
            (B2, "Home", Capability::Supported),
        ]
    );
}

/// The status as macOS 27.2.1 writes it (`nc_status_macos27.txt`: the
/// structure of a real one, with `ConnectCount` under
/// `ConnectionStatistics`, dictionaries inside an array, an empty array and
/// an empty value; every value invented). Parsed from a copy, not on a
/// device.
#[test]
fn reads_the_status_macos_27_writes() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_macos27.txt"));
    assert_eq!(
        adapter.query_state(&binding(B2)).unwrap(),
        OsState::Connected
    );
    assert_eq!(
        resolved(&adapter, B2, "nc_status_macos27.txt"),
        utun("utun5", [192, 0, 2, 10], 4)
    );
}

// resolve_interface

#[test]
fn resolves_the_interface_from_the_current_ipv4_status() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    assert_eq!(
        adapter.resolve_interface(&binding(B1)).unwrap(),
        utun("utun4", [192, 0, 2, 10], 2)
    );
}

#[test]
fn resolves_again_on_every_call_so_a_reconnect_is_seen() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    reply(&adapter, 0, &fixture("nc_status_reconnected.txt"));
    let before = adapter.resolve_interface(&binding(B1)).unwrap();
    let after = adapter.resolve_interface(&binding(B1)).unwrap();
    assert_eq!(
        (before.name.as_str(), after.name.as_str()),
        ("utun4", "utun7")
    );
    assert_eq!(adapter.runner().calls().len(), 2);
}

#[test]
fn a_tunnel_that_is_not_connected_has_no_interface() {
    for name in [
        "nc_status_disconnected.txt",
        "nc_status_connecting.txt",
        "nc_status_disconnecting.txt",
    ] {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.resolve_interface(&binding(B1))),
            AdapterErrorKind::NotFound,
            "{name}"
        );
    }
}

#[test]
fn rejects_missing_ambiguous_or_unsafe_interface_data() {
    for name in [
        "nc_status_missing_interface.txt",
        "nc_status_duplicate_key.txt",
        "nc_status_nested_conflict.txt",
        "nc_status_bad_interface.txt",
        "nc_status_ipv6_address.txt",
        "nc_status_two_blocks.txt",
        "nc_status_interleaved.txt",
        "nc_status_deep.txt",
    ] {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.resolve_interface(&binding(B1))),
            AdapterErrorKind::ParseError,
            "{name}"
        );
    }
}

#[test]
fn agreeing_nested_interface_names_are_accepted() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_nested_consistent.txt"));
    let interface = adapter.resolve_interface(&binding(B1)).unwrap();
    assert_eq!(interface, utun("utun4", [192, 0, 2, 10], 2));
}

#[test]
fn truncated_status_is_rejected() {
    let full = fixture("nc_status_connected.txt");
    for cut in [full.len() - 1, full.len() - 2, full.len() / 2] {
        let adapter = mac();
        reply(&adapter, 0, &full[..cut]);
        assert_eq!(
            kind(adapter.resolve_interface(&binding(B1))),
            AdapterErrorKind::ParseError,
            "cut at {cut}"
        );
    }
    let adapter = mac();
    adapter.runner().push(Ok(Output {
        exit_code: Some(0),
        stdout: full.into_bytes(),
        truncated: true,
        ..Output::default()
    }));
    assert_eq!(
        kind(adapter.resolve_interface(&binding(B1))),
        AdapterErrorKind::ParseError
    );
}

#[test]
fn three_tunnels_resolve_to_their_own_interfaces() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    reply(&adapter, 0, &fixture("nc_status_connected_b.txt"));
    reply(&adapter, 0, &fixture("nc_status_connected_c.txt"));
    let resolved: Vec<_> = [B1, B2, B3]
        .iter()
        .map(|b| adapter.resolve_interface(&binding(b)).unwrap())
        .collect();
    assert_eq!(
        resolved,
        [
            utun_in(0, "utun4", [192, 0, 2, 10], 2),
            utun_in(1, "utun5", [198, 51, 100, 20], 4),
            utun_in(2, "utun6", [203, 0, 113, 30], 6),
        ]
    );
    assert_eq!(
        argv(&adapter),
        [
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B2]),
            scutil(&["--nc", "status", B3]),
        ]
    );
}

#[test]
fn f05_same_utun_and_address_with_a_new_connect_count_is_a_different_interface() {
    let adapter = mac();
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    reply(&adapter, 0, &fixture("nc_status_reconnected_same_utun.txt"));
    let before = adapter.resolve_interface(&binding(B1)).unwrap();
    let after = adapter.resolve_interface(&binding(B1)).unwrap();
    assert_eq!(
        (&before.name, before.source),
        (&after.name, after.source),
        "the fixtures differ only in the connect count"
    );
    assert_eq!((before.incarnation, after.incarnation), (Some(2), Some(3)));
    assert_ne!(before, after);
}

#[test]
fn f05_connect_count_must_be_exactly_one_plain_number() {
    for name in [
        "nc_status_connect_count_missing.txt",
        "nc_status_connect_count_duplicate.txt",
        "nc_status_connect_count_not_numeric.txt",
    ] {
        let adapter = mac();
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.resolve_interface(&binding(B1))),
            AdapterErrorKind::ParseError,
            "{name}"
        );
    }
}

#[test]
fn f05_probe_through_an_older_connection_is_rejected_without_pinging() {
    let adapter = mac();
    let old = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_reconnected_same_utun.txt");
    assert_eq!(
        kind(adapter.probe(&old, &icmp())),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        argv(&adapter),
        [
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
        ]
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn f05_a_reconnect_during_the_probe_discards_its_reply() {
    let adapter = mac();
    let old = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("ping_reply.txt"));
    status_now(&adapter, "nc_status_reconnected_same_utun.txt");
    assert_eq!(
        kind(adapter.probe(&old, &icmp())),
        AdapterErrorKind::NotFound
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn f05_route_evidence_across_a_reconnect_is_rejected() {
    let adapter = mac();
    let old = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("route_utun4.txt"));
    status_now(&adapter, "nc_status_reconnected_same_utun.txt");
    assert_eq!(
        kind(adapter.check_route(&old, target())),
        AdapterErrorKind::NotFound
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn f05_an_interface_this_adapter_never_resolved_runs_nothing() {
    let adapter = mac();
    let unknown = utun("utun4", [192, 0, 2, 10], 2);
    assert_eq!(
        kind(adapter.probe(&unknown, &icmp())),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        kind(adapter.check_route(&unknown, target())),
        AdapterErrorKind::NotFound
    );
    assert!(adapter.runner().calls().is_empty());
}

// Integrated review of 121b42a, finding 3: `check_route` and `probe` are given
// only the interface, so they must re-read the configuration that resolved
// it. Looking that up by `utun` name let another configuration that later got
// the same `utun`, address and connect count vouch for an interface it never
// resolved.

#[test]
fn r3_two_configurations_never_resolve_to_the_same_interface() {
    let adapter = mac();
    let first = resolved(&adapter, B1, "nc_status_connected.txt");
    // B1 went down; B2 came up on the same utun, address and connect count.
    let second = resolved(&adapter, B2, "nc_status_connected.txt");
    assert_eq!(
        (&first.name, first.source),
        (&second.name, second.source),
        "the statuses differ only in which configuration they belong to"
    );
    assert_ne!(first, second);
}

#[test]
fn r3_an_interface_is_checked_against_the_configuration_that_resolved_it() {
    let adapter = mac();
    let old = resolved(&adapter, B1, "nc_status_connected.txt");
    let other = resolved(&adapter, B2, "nc_status_connected.txt");

    // B1 is disconnected now: its old interface proves nothing, whatever B2 has.
    status_now(&adapter, "nc_status_disconnected.txt");
    assert_eq!(
        kind(adapter.probe(&old, &icmp())),
        AdapterErrorKind::NotFound
    );
    status_now(&adapter, "nc_status_disconnected.txt");
    assert_eq!(
        kind(adapter.check_route(&old, target())),
        AdapterErrorKind::NotFound
    );

    // B2's own interface is still probed, confirmed through B2.
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("ping_reply.txt"));
    status_now(&adapter, "nc_status_connected.txt");
    assert!(matches!(
        adapter.probe(&other, &icmp()),
        Ok(ProbeOutcome::Reply { .. })
    ));

    assert_eq!(
        argv(&adapter)[2..],
        [
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B2]),
            (
                "/sbin/ping".to_owned(),
                ping::args("utun4", target(), Duration::from_secs(2))
            ),
            scutil(&["--nc", "status", B2]),
        ]
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn r3_a_connect_count_too_large_to_tag_is_rejected() {
    let at = |count: &str| {
        let adapter = mac();
        let text = fixture("nc_status_connected.txt")
            .replace("ConnectCount : 2\n", &format!("ConnectCount : {count}\n"));
        reply(&adapter, 0, &text);
        adapter.resolve_interface(&binding(B1))
    };
    assert_eq!(
        at("281474976710655").unwrap().incarnation,
        Some(281_474_976_710_655)
    );
    assert_eq!(kind(at("281474976710656")), AdapterErrorKind::ParseError);
}

#[test]
fn r3_an_interface_without_a_known_configuration_runs_nothing() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    let forged = [
        ResolvedInterface {
            incarnation: None,
            ..tunnel.clone()
        },
        ResolvedInterface {
            incarnation: Some((1 << 48) | 2),
            ..tunnel.clone()
        },
        ResolvedInterface {
            incarnation: Some(u64::MAX),
            ..tunnel
        },
    ];
    for interface in forged {
        assert_eq!(
            kind(adapter.probe(&interface, &icmp())),
            AdapterErrorKind::NotFound,
            "{interface:?}"
        );
        assert_eq!(
            kind(adapter.check_route(&interface, target())),
            AdapterErrorKind::NotFound,
            "{interface:?}"
        );
    }
    assert_eq!(adapter.runner().calls().len(), 1);
}

#[test]
fn r3_the_configurations_one_adapter_tracks_are_bounded() {
    let adapter = mac();
    let uuid = |i: usize| format!("00000000-0000-4000-8000-{i:012X}");
    for i in 0..MAX_CONFIGURATIONS {
        reply(&adapter, 0, &fixture("nc_status_connected.txt"));
        adapter.resolve_interface(&binding(&uuid(i))).unwrap();
    }
    // A configuration seen before still resolves; a new one is refused.
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    adapter.resolve_interface(&binding(&uuid(0))).unwrap();
    reply(&adapter, 0, &fixture("nc_status_connected.txt"));
    assert_eq!(
        kind(adapter.resolve_interface(&binding(&uuid(MAX_CONFIGURATIONS)))),
        AdapterErrorKind::Unsupported
    );
}

#[test]
fn registration_check_validates_the_uuid_and_runs_nothing() {
    let adapter = mac();
    assert_eq!(adapter.check_registration(&binding(B1)), Ok(()));
    assert_eq!(
        adapter.check_registration(&binding(&B2.to_ascii_lowercase())),
        Ok(())
    );
    assert_eq!(
        kind(adapter.check_registration(&binding("wg0"))),
        AdapterErrorKind::NotFound
    );
    assert!(adapter.runner().calls().is_empty());
}

// start and stop

#[test]
fn reconnect_sequence_is_observable_through_query_state() {
    let adapter = mac();
    let b1 = binding(B1);
    reply(&adapter, 0, "");
    reply(&adapter, 0, &fixture("nc_status_disconnecting.txt"));
    reply(&adapter, 0, &fixture("nc_status_disconnected.txt"));
    reply(&adapter, 0, "");
    reply(&adapter, 0, &fixture("nc_status_connecting.txt"));
    reply(&adapter, 0, &fixture("nc_status_reconnected.txt"));
    reply(&adapter, 0, &fixture("nc_status_reconnected.txt"));

    adapter.stop(&b1).unwrap();
    assert_eq!(adapter.query_state(&b1).unwrap(), OsState::Transitioning);
    assert_eq!(adapter.query_state(&b1).unwrap(), OsState::Disconnected);
    adapter.start(&b1).unwrap();
    assert_eq!(adapter.query_state(&b1).unwrap(), OsState::Transitioning);
    assert_eq!(adapter.query_state(&b1).unwrap(), OsState::Connected);
    assert_eq!(adapter.resolve_interface(&b1).unwrap().name, "utun7");

    assert_eq!(
        argv(&adapter),
        [
            scutil(&["--nc", "stop", B1]),
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "start", B1]),
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
        ]
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn control_acts_only_on_the_given_configuration() {
    let adapter = mac();
    reply(&adapter, 0, "");
    adapter.stop(&binding(B2)).unwrap();
    let calls = adapter.runner().calls();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].args.iter().any(|a| a == B1 || a == B3));
    assert_eq!(argv(&adapter), [scutil(&["--nc", "stop", B2])]);
}

#[test]
fn control_failures_are_classified() {
    let adapter = mac();
    reply_err(&adapter, 1, "No service\n");
    reply_err(&adapter, 1, "Could not start connection: failed\n");
    adapter.runner().push(Err(ExecError::Timeout));
    assert_eq!(
        kind(adapter.start(&binding(B1))),
        AdapterErrorKind::NotFound
    );
    assert_eq!(
        kind(adapter.start(&binding(B1))),
        AdapterErrorKind::ManagerConflict
    );
    assert_eq!(kind(adapter.stop(&binding(B1))), AdapterErrorKind::Timeout);
}

// check_route

#[test]
fn a_route_through_the_tunnel_interface_is_evidence() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("route_utun4.txt"));
    status_now(&adapter, "nc_status_connected.txt");
    let evidence = adapter.check_route(&tunnel, target()).unwrap();
    assert_eq!(
        evidence,
        RouteEvidence {
            target: target(),
            interface_name: "utun4".into(),
            interface_index: None,
            source: None,
        }
    );
    assert_eq!(
        argv(&adapter),
        [
            scutil(&["--nc", "status", B1]),
            scutil(&["--nc", "status", B1]),
            (
                "/sbin/route".to_owned(),
                vec!["-n".to_owned(), "get".into(), "192.0.2.1".into()]
            ),
            scutil(&["--nc", "status", B1]),
        ]
    );
}

#[test]
fn a_route_elsewhere_or_none_is_a_mismatch() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    let other = resolved(&adapter, B2, "nc_status_connected_b.txt");

    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("route_en0.txt"));
    assert_eq!(
        kind(adapter.check_route(&tunnel, target())),
        AdapterErrorKind::RouteMismatch
    );
    status_now(&adapter, "nc_status_connected_b.txt");
    reply(&adapter, 0, &fixture("route_utun4.txt"));
    assert_eq!(
        kind(adapter.check_route(&other, target())),
        AdapterErrorKind::RouteMismatch
    );
    status_now(&adapter, "nc_status_connected.txt");
    reply_err(
        &adapter,
        1,
        "route: writing to routing socket: not in table\n",
    );
    assert_eq!(
        kind(adapter.check_route(&tunnel, target())),
        AdapterErrorKind::RouteMismatch
    );
    assert_eq!(adapter.runner().pending(), 0);
}

#[test]
fn malformed_route_output_is_a_parse_error() {
    for name in [
        "route_duplicate_interface.txt",
        "route_no_interface.txt",
        "route_other_target.txt",
    ] {
        let adapter = mac();
        let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
        status_now(&adapter, "nc_status_connected.txt");
        reply(&adapter, 0, &fixture(name));
        assert_eq!(
            kind(adapter.check_route(&tunnel, target())),
            AdapterErrorKind::ParseError,
            "{name}"
        );
    }
}

// probe

#[test]
fn a_bound_ping_reply_reports_its_round_trip_time() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 0, &fixture("ping_reply.txt"));
    status_now(&adapter, "nc_status_connected.txt");
    let outcome = adapter.probe(&tunnel, &icmp()).unwrap();
    assert_eq!(
        outcome,
        ProbeOutcome::Reply {
            rtt: Duration::from_micros(12_345)
        }
    );
    let calls = adapter.runner().calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[2].program.path(), "/sbin/ping");
    assert_eq!(
        calls[2].args,
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
    assert_eq!(calls[2].timeout, Duration::from_secs(5));
}

#[test]
fn no_reply_is_reported_only_when_ping_says_so() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    reply(&adapter, 2, &fixture("ping_no_reply.txt"));
    status_now(&adapter, "nc_status_connected.txt");
    assert_eq!(adapter.probe(&tunnel, &icmp()), Ok(ProbeOutcome::NoReply));
}

#[test]
fn probe_failures_are_errors_not_unreachable_targets() {
    let cases: Vec<(Result<Output, ExecError>, AdapterErrorKind)> = vec![
        (
            Ok(Output {
                exit_code: Some(68),
                stderr: b"ping: bad interface name\n".to_vec(),
                ..Output::default()
            }),
            AdapterErrorKind::NotFound,
        ),
        (
            Ok(Output {
                exit_code: Some(77),
                ..Output::default()
            }),
            AdapterErrorKind::PermissionDenied,
        ),
        (
            Ok(Output {
                exit_code: Some(71),
                stderr: b"ping: socket: Operation not permitted\n".to_vec(),
                ..Output::default()
            }),
            AdapterErrorKind::PermissionDenied,
        ),
        (
            Ok(Output {
                exit_code: Some(64),
                stderr: b"usage: ping ...\n".to_vec(),
                ..Output::default()
            }),
            AdapterErrorKind::Unsupported,
        ),
        (
            Ok(Output {
                exit_code: Some(0),
                stdout: fixture("ping_reply_other_source.txt").into_bytes(),
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
        (
            Ok(Output {
                exit_code: Some(0),
                stdout: fixture("ping_summary_without_reply.txt").into_bytes(),
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
        (
            Ok(Output {
                exit_code: Some(2),
                stdout: fixture("ping_reply.txt").into_bytes(),
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
        (
            Ok(Output {
                exit_code: Some(0),
                stdout: fixture("ping_reply.txt").into_bytes(),
                truncated: true,
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
        (
            Ok(Output {
                exit_code: None,
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
        (Err(ExecError::Timeout), AdapterErrorKind::Timeout),
        (Err(ExecError::NotFound), AdapterErrorKind::Unsupported),
    ];
    for (output, expected) in cases {
        let adapter = mac();
        let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
        status_now(&adapter, "nc_status_connected.txt");
        adapter.runner().push(output.clone());
        assert_eq!(
            kind(adapter.probe(&tunnel, &icmp())),
            expected,
            "{output:?}"
        );
        assert_eq!(adapter.runner().pending(), 0, "{output:?}");
    }
}

// Integrated review of 121b42a, finding 1: a probe the local host refused to
// send says nothing about the target (plan §4.1, T23). Apple's ping warns
// `ping: sendto: <error>`, still counts the packet as transmitted and exits 2
// exactly as when nothing answered, so its summary alone cannot tell.

fn ping_run(exit_code: i32, stdout: &str, stderr: &str) -> Result<Output, ExecError> {
    Ok(Output {
        exit_code: Some(exit_code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        truncated: false,
    })
}

#[test]
fn f1_a_probe_the_host_refused_to_send_is_an_error_never_no_reply() {
    let cases = [
        (
            "ping: sendto: Operation not permitted\n",
            AdapterErrorKind::PermissionDenied,
        ),
        (
            "ping: sendto: Permission denied\n",
            AdapterErrorKind::PermissionDenied,
        ),
        (
            "ping: sendto: No route to host\n",
            AdapterErrorKind::RouteMismatch,
        ),
        (
            "ping: sendto: Network is unreachable\n",
            AdapterErrorKind::RouteMismatch,
        ),
        (
            "ping: sendto: Can't assign requested address\n",
            AdapterErrorKind::RouteMismatch,
        ),
        (
            "ping: sendto: No buffer space available\n",
            AdapterErrorKind::ParseError,
        ),
        (
            "ping: sendto: Operation not permitted (extra)\n",
            AdapterErrorKind::ParseError,
        ),
        (
            "ping: 192.0.2.1: partial write: 8 of 64 bytes\n",
            AdapterErrorKind::ParseError,
        ),
        ("something unexpected\n", AdapterErrorKind::ParseError),
    ];
    for (stderr, expected) in cases {
        let adapter = mac();
        let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
        status_now(&adapter, "nc_status_connected.txt");
        adapter
            .runner()
            .push(ping_run(2, &fixture("ping_no_reply_single.txt"), stderr));
        assert_eq!(
            kind(adapter.probe(&tunnel, &icmp())),
            expected,
            "{stderr:?}"
        );
        assert_eq!(adapter.runner().pending(), 0, "{stderr:?}");
    }
}

#[test]
fn f1_the_same_summary_without_a_send_error_is_still_no_reply() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    adapter
        .runner()
        .push(ping_run(2, &fixture("ping_no_reply_single.txt"), ""));
    status_now(&adapter, "nc_status_connected.txt");
    assert_eq!(adapter.probe(&tunnel, &icmp()), Ok(ProbeOutcome::NoReply));
}

#[test]
fn f1_a_send_error_beside_a_reply_is_not_a_reply() {
    let adapter = mac();
    let tunnel = resolved(&adapter, B1, "nc_status_connected.txt");
    status_now(&adapter, "nc_status_connected.txt");
    adapter.runner().push(ping_run(
        0,
        &fixture("ping_reply.txt"),
        "ping: sendto: No route to host\n",
    ));
    assert_eq!(
        kind(adapter.probe(&tunnel, &icmp())),
        AdapterErrorKind::RouteMismatch
    );
}

#[test]
fn never_runs_ping_with_an_unvalidated_interface() {
    let adapter = mac();
    for bad in ["en0", "utun4 -c 1000", "-b", "utun01", ""] {
        let interface = ResolvedInterface {
            name: bad.to_owned(),
            index: None,
            source: None,
            incarnation: None,
        };
        assert_eq!(
            kind(adapter.probe(&interface, &icmp())),
            AdapterErrorKind::NotFound
        );
    }
    assert!(adapter.runner().calls().is_empty());
}

#[test]
fn tcp_probes_are_unsupported_and_run_nothing() {
    let adapter = mac();
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::TcpConnect {
                port: NonZeroU16::new(443).unwrap(),
            },
        },
        timeout: Duration::from_secs(2),
    };
    assert_eq!(
        kind(adapter.probe(&utun("utun4", [192, 0, 2, 10], 2), &request)),
        AdapterErrorKind::Unsupported
    );
    assert!(adapter.runner().calls().is_empty());
}

// capabilities and metrics

#[test]
fn claims_nothing_that_was_not_proven_on_a_mac() {
    let capabilities = mac().capabilities();
    assert_eq!(capabilities.query_state, Capability::Unknown);
    assert_eq!(capabilities.start_stop, Capability::Unknown);
    assert_eq!(capabilities.bound_icmp_probe, Capability::Unknown);
    assert_eq!(capabilities.background_operation, Capability::Unknown);
    assert_eq!(capabilities.bound_tcp_probe, Capability::Unsupported);
    assert_eq!(capabilities.read_metrics, Capability::Unsupported);
    assert_eq!(capabilities.verified_concurrent_tunnels, 0);
}

#[test]
fn metrics_are_not_provided_by_the_official_app() {
    let adapter = mac();
    assert_eq!(
        kind(adapter.read_metrics(&binding(B1))),
        AdapterErrorKind::Unsupported
    );
    assert!(adapter.runner().calls().is_empty());
}

#[test]
fn programs_are_the_fixed_system_paths() {
    assert_eq!(SCUTIL.path(), "/usr/sbin/scutil");
    assert_eq!(ping::PING.path(), "/sbin/ping");
    assert_eq!(route::ROUTE.path(), "/sbin/route");
}
