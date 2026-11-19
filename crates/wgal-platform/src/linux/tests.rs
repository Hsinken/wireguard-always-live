//! Adapter tests: the exact argument vectors sent to each fixed tool, and how
//! each answer, failure or refusal surfaces. Nothing here starts a process.

use std::io;
use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::time::Duration;

use wgal_core::{AdapterError, PlatformAdapter, ProbeOutcome, ProbeRequest, TunnelBinding};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, Capability, OsState, ProbeKind, ProbeTarget,
    TargetAddress,
};

use super::files::FileKind;
use super::files::fake::FakeFiles;
use super::test_support::fixture;
use super::{IP, LinuxAdapter, PING, SYSTEMCTL, WG};
use crate::exec::{ExecError, Invocation, Output, Program, ScriptedRunner};

type Adapter = LinuxAdapter<ScriptedRunner, FakeFiles>;

fn adapter(tunnels: &[&str]) -> Adapter {
    LinuxAdapter::with_parts(ScriptedRunner::new(), FakeFiles::ubuntu(tunnels))
}

fn binding(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

fn target() -> TargetAddress {
    TargetAddress::new(Ipv4Addr::new(192, 0, 2, 1)).unwrap()
}

fn reply(stdout: &str) -> Result<Output, ExecError> {
    Ok(Output {
        exit_code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        ..Output::default()
    })
}

fn failure(exit_code: i32, stderr: &str) -> Result<Output, ExecError> {
    Ok(Output {
        exit_code: Some(exit_code),
        stderr: stderr.as_bytes().to_vec(),
        ..Output::default()
    })
}

fn argv(call: &Invocation) -> (Program, Vec<&str>) {
    (call.program, call.args.iter().map(String::as_str).collect())
}

fn calls(adapter: &Adapter) -> Vec<(Program, Vec<String>)> {
    adapter
        .runner
        .calls()
        .into_iter()
        .map(|c| (c.program, c.args))
        .collect()
}

fn kind<T: std::fmt::Debug>(result: Result<T, AdapterError>) -> AdapterErrorKind {
    result.unwrap_err().kind
}

const SHOW: &str = "--property=LoadState,ActiveState,SubState,FragmentPath,DropInPaths";

/// Only what was proven on a device (`docs/testing.md`,
/// 2026-11-19) is `Supported`: the bound ICMP probe. TCP probes cannot be
/// bound yet; the rest stays `Unknown`, and no number of tunnels side by
/// side is claimed.
#[test]
fn only_the_bound_icmp_probe_is_proven_on_a_device() {
    let caps = adapter(&[]).capabilities();
    assert_eq!(
        caps,
        AdapterCapabilities {
            bound_icmp_probe: Capability::Supported,
            bound_tcp_probe: Capability::Unsupported,
            ..AdapterCapabilities::UNKNOWN
        }
    );
    assert_eq!(caps.verified_concurrent_tunnels, 0);
}

#[test]
fn query_state_reads_only_the_given_unit() {
    let a = adapter(&["wg-office"]);
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    assert_eq!(
        a.query_state(&binding("wg-office")).unwrap(),
        OsState::Connected
    );

    let call = &a.runner.calls()[0];
    assert_eq!(
        argv(call),
        (
            SYSTEMCTL,
            vec!["show", "wg-quick@wg-office.service", SHOW, "--no-pager"]
        )
    );
    assert_eq!(call.timeout, Duration::from_secs(10));
    assert!(call.max_output_bytes <= 16 * 1024);
}

#[test]
fn a_stopped_unit_is_disconnected_unless_its_configuration_is_gone() {
    let a = adapter(&["wg-lab"]);
    a.runner
        .push(reply(&fixture("systemctl-show-inactive.txt")));
    assert_eq!(
        a.query_state(&binding("wg-lab")).unwrap(),
        OsState::Disconnected
    );

    a.files.remove("/etc/wireguard/wg-lab.conf");
    a.runner
        .push(reply(&fixture("systemctl-show-inactive.txt")));
    assert_eq!(
        kind(a.query_state(&binding("wg-lab"))),
        AdapterErrorKind::NotFound
    );

    // Without the privileges to look, systemd's answer stands.
    a.files.fail(
        "/etc/wireguard/wg-lab.conf",
        io::ErrorKind::PermissionDenied,
    );
    a.runner.push(reply(&fixture("systemctl-show-failed.txt")));
    assert_eq!(
        a.query_state(&binding("wg-lab")).unwrap(),
        OsState::Disconnected
    );
}

#[test]
fn read_failures_are_errors_never_disconnected() {
    let a = adapter(&["wg-office"]);
    let cases: [(Result<Output, ExecError>, AdapterErrorKind); 6] = [
        (Err(ExecError::Timeout), AdapterErrorKind::Timeout),
        (Err(ExecError::NotFound), AdapterErrorKind::Unsupported),
        (
            Err(ExecError::PermissionDenied),
            AdapterErrorKind::PermissionDenied,
        ),
        (
            failure(1, "Failed to connect to bus: No such file or directory\n"),
            AdapterErrorKind::ManagerConflict,
        ),
        (reply("LoadState=loaded\n"), AdapterErrorKind::ParseError),
        (
            Ok(Output {
                exit_code: Some(0),
                stdout: fixture("systemctl-show-inactive.txt").into_bytes(),
                truncated: true,
                ..Output::default()
            }),
            AdapterErrorKind::ParseError,
        ),
    ];
    for (answer, expected) in cases {
        a.runner.push(answer);
        assert_eq!(kind(a.query_state(&binding("wg-office"))), expected);
    }
}

#[test]
fn bindings_that_are_not_wg_quick_interfaces_never_reach_a_command() {
    let a = adapter(&[]);
    for bad in ["a=b", "a+b", "-x", "all", "interfaces", "abcdefghijklmnop"] {
        let b = binding(bad);
        assert_eq!(kind(a.query_state(&b)), AdapterErrorKind::NotFound, "{bad}");
        assert_eq!(kind(a.start(&b)), AdapterErrorKind::NotFound, "{bad}");
        assert_eq!(kind(a.stop(&b)), AdapterErrorKind::NotFound, "{bad}");
        assert_eq!(
            kind(a.resolve_interface(&b)),
            AdapterErrorKind::NotFound,
            "{bad}"
        );
        assert_eq!(
            kind(a.read_metrics(&b)),
            AdapterErrorKind::NotFound,
            "{bad}"
        );
        assert_eq!(
            kind(a.check_registration(&b)),
            AdapterErrorKind::NotFound,
            "{bad}"
        );
    }
    assert!(a.runner.calls().is_empty());
}

fn show_unit(unit: &str) -> (Program, Vec<String>) {
    (
        SYSTEMCTL,
        ["show", unit, SHOW, "--no-pager"]
            .map(str::to_owned)
            .to_vec(),
    )
}

#[test]
fn start_and_stop_queue_a_job_for_that_unit_only() {
    let a = adapter(&["wg-office", "wg-lab"]);
    for _ in 0..2 {
        a.runner
            .push(reply(&fixture("systemctl-show-active-exited.txt")));
        a.runner.push(reply(""));
    }
    a.start(&binding("wg-lab")).unwrap();
    a.stop(&binding("wg-lab")).unwrap();
    assert_eq!(
        calls(&a),
        [
            show_unit("wg-quick@wg-lab.service"),
            (
                SYSTEMCTL,
                [
                    "--no-block",
                    "--no-ask-password",
                    "start",
                    "wg-quick@wg-lab.service"
                ]
                .map(str::to_owned)
                .to_vec()
            ),
            show_unit("wg-quick@wg-lab.service"),
            (
                SYSTEMCTL,
                [
                    "--no-block",
                    "--no-ask-password",
                    "stop",
                    "wg-quick@wg-lab.service"
                ]
                .map(str::to_owned)
                .to_vec()
            ),
        ]
    );
    // Neither call so much as names another tunnel.
    assert!(
        calls(&a)
            .iter()
            .all(|(_, args)| !args.join(" ").contains("wg-office"))
    );
}

#[test]
fn start_refuses_a_tunnel_whose_configuration_is_gone() {
    let a = adapter(&[]);
    assert_eq!(
        kind(a.start(&binding("wg-gone"))),
        AdapterErrorKind::NotFound
    );
    assert!(a.runner.calls().is_empty());
}

#[test]
fn refused_control_requests_keep_their_reason() {
    let a = adapter(&["wg-office"]);
    let cases = [
        (
            "Failed to start wg-quick@wg-office.service: Access denied\n",
            AdapterErrorKind::PermissionDenied,
        ),
        (
            "Failed to start wg-quick@wg-office.service: Interactive authentication required.\n",
            AdapterErrorKind::PermissionDenied,
        ),
        (
            "Failed to start wg-quick@wg-office.service: Unit wg-quick@wg-office.service is masked.\n",
            AdapterErrorKind::ManagerConflict,
        ),
        (
            "Failed to start wg-quick@wg-office.service: Unit wg-quick@wg-office.service not found.\n",
            AdapterErrorKind::NotFound,
        ),
        ("something unexpected\n", AdapterErrorKind::ManagerConflict),
    ];
    for (stderr, expected) in cases {
        a.runner
            .push(reply(&fixture("systemctl-show-active-exited.txt")));
        a.runner.push(failure(1, stderr));
        let error = a.start(&binding("wg-office")).unwrap_err();
        assert_eq!(error.kind, expected, "{stderr:?}");
        assert!(!error.detail.as_str().contains("Failed to start"));
    }
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.runner.push(Err(ExecError::Timeout));
    assert_eq!(
        kind(a.stop(&binding("wg-office"))),
        AdapterErrorKind::Timeout
    );
}

#[test]
fn resolves_the_interface_with_ip_json() {
    let a = adapter(&["wg-office"]);
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    let resolved = a.resolve_interface(&binding("wg-office")).unwrap();
    assert_eq!(resolved.index, Some(7));
    assert_eq!(resolved.source, Some(Ipv4Addr::new(192, 0, 2, 11)));
    assert_eq!(
        argv(&a.runner.calls()[0]),
        (IP, vec!["-j", "address", "show", "dev", "wg-office"])
    );

    a.runner
        .push(failure(1, "Device \"wg-office\" does not exist.\n"));
    assert_eq!(
        kind(a.resolve_interface(&binding("wg-office"))),
        AdapterErrorKind::NotFound
    );
}

fn office(a: &Adapter) -> wgal_core::ResolvedInterface {
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    a.resolve_interface(&binding("wg-office")).unwrap()
}

#[test]
fn route_evidence_needs_both_the_plain_and_the_bound_lookup() {
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    a.runner
        .push(reply(&fixture("ip-route-via-wg-office.json")));
    a.runner
        .push(reply(&fixture("ip-route-via-wg-office.json")));
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    let evidence = a.check_route(&interface, target()).unwrap();
    assert_eq!(evidence.interface_name, "wg-office");
    assert_eq!(evidence.source, Some(Ipv4Addr::new(192, 0, 2, 11)));
    let calls = a.runner.calls();
    assert_eq!(
        argv(&calls[3]),
        (IP, vec!["-j", "address", "show", "dev", "wg-office"])
    );
    assert_eq!(
        argv(&calls[1]),
        (IP, vec!["-j", "route", "get", "192.0.2.1"])
    );
    assert_eq!(
        argv(&calls[2]),
        (
            IP,
            vec!["-j", "route", "get", "192.0.2.1", "oif", "wg-office"]
        )
    );
}

#[test]
fn a_target_ordinary_traffic_reaches_another_way_is_a_mismatch() {
    // T08: the target answers through another tunnel; that proves nothing here.
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    a.runner.push(reply(&fixture("ip-route-via-wg-lab.json")));
    assert_eq!(
        kind(a.check_route(&interface, target())),
        AdapterErrorKind::RouteMismatch
    );
    assert_eq!(a.runner.pending(), 0);

    a.runner
        .push(reply(&fixture("ip-route-via-wg-office.json")));
    a.runner
        .push(failure(2, "RTNETLINK answers: Network is unreachable\n"));
    assert_eq!(
        kind(a.check_route(&interface, target())),
        AdapterErrorKind::RouteMismatch
    );
}

#[test]
fn icmp_probe_is_bound_to_the_interface_with_fixed_arguments() {
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    let request = |millis| ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_millis(millis),
    };
    a.runner.push(reply(&fixture("ping-reply.txt")));
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    assert_eq!(
        a.probe(&interface, &request(2_000)).unwrap(),
        ProbeOutcome::Reply {
            rtt: Duration::from_micros(56_531)
        }
    );
    let call = &a.runner.calls()[1];
    assert_eq!(
        argv(call),
        (
            PING,
            vec![
                "-4",
                "-n",
                "-I",
                "wg-office",
                "-c",
                "1",
                "-W",
                "2",
                "192.0.2.1"
            ]
        )
    );
    // AR-01: no `-w`. With `-c 1 -w`, iputils keeps sending one echo a
    // second until the deadline, so a silent target printed
    // "3 packets transmitted" and was read as a broken probe. The runner's
    // own deadline still bounds the run: the wait, a second, the grace.
    assert!(!call.args.iter().any(|a| a == "-w"), "{:?}", call.args);
    assert_eq!(call.timeout, Duration::from_secs(5));

    // Partial seconds round up; the wait is capped.
    let waits = |a: &Adapter| -> Vec<String> {
        a.runner
            .calls()
            .into_iter()
            .filter(|c| c.program == PING)
            .map(|c| c.args[7].clone())
            .collect()
    };
    a.runner.push(failure(1, ""));
    let _ = a.probe(&interface, &request(1_500));
    a.runner.push(failure(1, ""));
    let _ = a.probe(&interface, &request(600_000));
    assert_eq!(waits(&a), ["2", "2", "30"]);
}

#[test]
fn probe_failures_are_never_no_reply() {
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    };
    let cases: [(Result<Output, ExecError>, AdapterErrorKind); 4] = [
        (Err(ExecError::Timeout), AdapterErrorKind::Timeout),
        (Err(ExecError::NotFound), AdapterErrorKind::Unsupported),
        (
            failure(2, &fixture("ping-permission-denied.stderr.txt")),
            AdapterErrorKind::PermissionDenied,
        ),
        (
            failure(2, &fixture("ping-no-such-device.stderr.txt")),
            AdapterErrorKind::NotFound,
        ),
    ];
    for (answer, expected) in cases {
        a.runner.push(answer);
        assert_eq!(kind(a.probe(&interface, &request)), expected);
    }
    a.runner.push(Ok(Output {
        exit_code: Some(1),
        stdout: fixture("ping-no-reply.txt").into_bytes(),
        ..Output::default()
    }));
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    assert_eq!(
        a.probe(&interface, &request).unwrap(),
        ProbeOutcome::NoReply
    );

    // AR-01, a negative check kept so the parser is never loosened: the
    // shape `-c 1 -w` printed for a silent target (more than one echo) is
    // not read as one silent echo. It does not prove the argv; the
    // assertion in `icmp_probe_is_bound_to_the_interface_with_fixed_arguments` does.
    a.runner.push(Ok(Output {
        exit_code: Some(1),
        stdout: fixture("ping-no-reply-three-sent.txt").into_bytes(),
        ..Output::default()
    }));
    assert_eq!(
        kind(a.probe(&interface, &request)),
        AdapterErrorKind::ParseError
    );
}

// Integrated review of 121b42a, finding 1: when the host refuses to send the
// echo (for example a netfilter OUTPUT drop), iputils prints `sendmsg: <error>`
// on stderr, counts the packet as transmitted and exits 1 with the same
// summary as a target that stayed silent. That is never `NoReply`.

#[test]
fn f1_a_probe_the_host_refused_to_send_is_an_error_never_no_reply() {
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    };
    let cases = [
        (
            "ping-sendmsg-not-permitted.stderr.txt",
            AdapterErrorKind::PermissionDenied,
        ),
        (
            "ping-sendmsg-no-route.stderr.txt",
            AdapterErrorKind::RouteMismatch,
        ),
    ];
    for (file, expected) in cases {
        let a = adapter(&["wg-office"]);
        let interface = office(&a);
        a.runner.push(Ok(Output {
            exit_code: Some(1),
            stdout: fixture("ping-no-reply.txt").into_bytes(),
            stderr: fixture(file).into_bytes(),
            truncated: false,
        }));
        assert_eq!(kind(a.probe(&interface, &request)), expected, "{file}");
        assert_eq!(a.runner.pending(), 0, "{file}");
    }

    // The same run without a send error is a target that did not answer.
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    a.runner.push(Ok(Output {
        exit_code: Some(1),
        stdout: fixture("ping-no-reply.txt").into_bytes(),
        ..Output::default()
    }));
    a.runner.push(reply(&fixture("ip-address-wg-office.json")));
    assert_eq!(
        a.probe(&interface, &request).unwrap(),
        ProbeOutcome::NoReply
    );
}

#[test]
fn tcp_probes_are_unsupported_and_run_nothing() {
    let a = adapter(&["wg-office"]);
    let interface = office(&a);
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
        kind(a.probe(&interface, &request)),
        AdapterErrorKind::Unsupported
    );
    assert_eq!(a.runner.calls().len(), 1);
}

#[test]
fn interfaces_handed_back_are_validated_again() {
    let a = adapter(&[]);
    let forged = wgal_core::ResolvedInterface {
        name: "eth0; reboot".to_owned(),
        index: Some(2),
        source: None,
        incarnation: Some(2),
    };
    assert_eq!(
        kind(a.check_route(&forged, target())),
        AdapterErrorKind::NotFound
    );
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    };
    assert_eq!(kind(a.probe(&forged, &request)), AdapterErrorKind::NotFound);
    assert!(a.runner.calls().is_empty());
}

#[test]
fn discovery_lists_wg_quick_units_and_marks_other_managers_unsupported() {
    let a = adapter(&[]);
    a.runner.push(reply(&fixture("systemctl-list-units.txt")));
    a.runner
        .push(reply(&fixture("systemctl-list-unit-files.txt")));
    a.runner.push(reply(&fixture("ip-link-wireguard.json")));
    let found = a.discover().unwrap();
    let summary: Vec<_> = found
        .iter()
        .map(|t| (t.binding.as_str(), t.display_name.as_str(), t.control))
        .collect();
    assert_eq!(
        summary,
        [
            ("nm-guest", "nm-guest", Capability::Unsupported),
            ("wg-home", "wg-home", Capability::Supported),
            ("wg-lab", "wg-lab", Capability::Supported),
            ("wg-office", "wg-office", Capability::Supported),
            ("wg-spare", "wg-spare", Capability::Supported),
        ]
    );
    let calls = a.runner.calls();
    assert_eq!(
        argv(&calls[0]),
        (
            SYSTEMCTL,
            vec![
                "list-units",
                "--type=service",
                "--all",
                "--plain",
                "--no-legend",
                "--no-pager",
                "--full",
                "wg-quick@*"
            ]
        )
    );
    assert_eq!(
        argv(&calls[1]),
        (
            SYSTEMCTL,
            vec![
                "list-unit-files",
                "--type=service",
                "--plain",
                "--no-legend",
                "--no-pager",
                "wg-quick@*"
            ]
        )
    );
    assert_eq!(
        argv(&calls[2]),
        (IP, vec!["-j", "link", "show", "type", "wireguard"])
    );
}

#[test]
fn discovery_fails_as_a_whole_rather_than_guessing() {
    let a = adapter(&[]);
    a.runner.push(reply(&fixture("systemctl-list-units.txt")));
    a.runner.push(reply("not a unit list\n"));
    assert_eq!(kind(a.discover()), AdapterErrorKind::ParseError);
}

#[test]
fn metrics_come_from_two_wg_show_subcommands_only() {
    let a = adapter(&["wg-office"]);
    a.runner.push(reply(&fixture("wg-latest-handshakes.txt")));
    a.runner.push(reply(&fixture("wg-transfer.txt")));
    let metrics = a.read_metrics(&binding("wg-office")).unwrap();
    assert_eq!(metrics.latest_handshake_unix_ms, Some(1_767_225_590_000));
    assert_eq!(
        calls(&a),
        [
            (
                WG,
                ["show", "wg-office", "latest-handshakes"]
                    .map(str::to_owned)
                    .to_vec()
            ),
            (
                WG,
                ["show", "wg-office", "transfer"]
                    .map(str::to_owned)
                    .to_vec()
            ),
        ]
    );

    a.runner.push(failure(
        1,
        "Unable to access interface: Operation not permitted\n",
    ));
    assert_eq!(
        kind(a.read_metrics(&binding("wg-office"))),
        AdapterErrorKind::PermissionDenied
    );
}

#[test]
fn registration_accepts_a_root_owned_packaged_setup() {
    let a = adapter(&["wg-office"]);
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.check_registration(&binding("wg-office")).unwrap();
}

#[test]
fn registration_checks_drop_ins_too() {
    let setup = || {
        let a = adapter(&["wg-lab"]);
        for dir in [
            "/etc/systemd/system/wg-quick@.service.d",
            "/etc/systemd/system/wg-quick@wg-lab.service.d",
        ] {
            a.files.set(dir, FileKind::Directory, 0, 0o755);
        }
        a.files.set(
            "/etc/systemd/system/wg-quick@.service.d/10-order.conf",
            FileKind::File,
            0,
            0o644,
        );
        a.files.set(
            "/etc/systemd/system/wg-quick@wg-lab.service.d/20-local.conf",
            FileKind::File,
            0,
            0o644,
        );
        a.runner
            .push(reply(&fixture("systemctl-show-drop-ins.txt")));
        a
    };
    setup().check_registration(&binding("wg-lab")).unwrap();

    let a = setup();
    a.files.set(
        "/etc/systemd/system/wg-quick@wg-lab.service.d/20-local.conf",
        FileKind::File,
        1000,
        0o644,
    );
    assert_eq!(
        kind(a.check_registration(&binding("wg-lab"))),
        AdapterErrorKind::ManagerConflict
    );
}

#[test]
fn registration_refuses_writable_configs_binaries_and_masked_units() {
    // Configuration and binaries are checked before any command runs.
    let user_config = adapter(&["wg-office"]);
    user_config
        .files
        .set("/etc/wireguard/wg-office.conf", FileKind::File, 1000, 0o600);
    let writable_binary = adapter(&["wg-office"]);
    writable_binary
        .files
        .set("/usr/bin/wg-quick", FileKind::File, 0, 0o777);
    let linked_config = adapter(&["wg-office"]);
    linked_config
        .files
        .set("/etc/wireguard/wg-office.conf", FileKind::Symlink, 0, 0o777);
    for a in [user_config, writable_binary, linked_config] {
        assert_eq!(
            kind(a.check_registration(&binding("wg-office"))),
            AdapterErrorKind::ManagerConflict
        );
        assert!(a.runner.calls().is_empty());
    }

    let masked = adapter(&["wg-office"]);
    let text =
        fixture("systemctl-show-inactive.txt").replace("LoadState=loaded", "LoadState=masked");
    masked.runner.push(reply(&text));
    assert_eq!(
        kind(masked.check_registration(&binding("wg-office"))),
        AdapterErrorKind::ManagerConflict
    );

    let gone = adapter(&["wg-office"]);
    gone.runner
        .push(reply(&fixture("systemctl-show-not-found.txt")));
    assert_eq!(
        kind(gone.check_registration(&binding("wg-office"))),
        AdapterErrorKind::NotFound
    );
}

#[test]
fn three_tunnels_are_addressed_independently() {
    // T07 at the adapter level: every call for one tunnel names only that tunnel.
    let a = adapter(&["wg-office", "wg-lab", "wg-home"]);
    for name in ["wg-office", "wg-lab", "wg-home"] {
        for answer in ["show", "show", "", "show", ""] {
            a.runner.push(reply(&if answer.is_empty() {
                String::new()
            } else {
                fixture("systemctl-show-active-exited.txt")
            }));
        }
        a.query_state(&binding(name)).unwrap();
        a.stop(&binding(name)).unwrap();
        a.start(&binding(name)).unwrap();
    }
    // Per tunnel: show; show + stop; show + start.
    for (i, name) in ["wg-office", "wg-lab", "wg-home"].iter().enumerate() {
        let unit = format!("wg-quick@{name}.service");
        for (_, args) in &calls(&a)[i * 5..i * 5 + 5] {
            assert!(args.contains(&unit), "{args:?}");
            for other in ["wg-office", "wg-lab", "wg-home"]
                .iter()
                .filter(|o| *o != name)
            {
                assert!(!args.join(" ").contains(other), "{args:?} names {other}");
            }
        }
    }
}

// Review finding 1: the privileged controller must never start or stop a unit
// whose configuration, hooks or binaries an ordinary user could change.

const CONF: &str = "/etc/wireguard/wg-office.conf";

fn start_argv(verb: &str) -> (Program, Vec<String>) {
    (
        SYSTEMCTL,
        [
            "--no-block",
            "--no-ask-password",
            verb,
            "wg-quick@wg-office.service",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
}

fn show_argv() -> (Program, Vec<String>) {
    (
        SYSTEMCTL,
        ["show", "wg-quick@wg-office.service", SHOW, "--no-pager"]
            .map(str::to_owned)
            .to_vec(),
    )
}

#[test]
fn f01_user_controlled_configuration_refuses_registration_and_control_before_any_command() {
    let cases: [(&str, FileKind, u32, u32); 5] = [
        (CONF, FileKind::File, 1000, 0o600),
        (CONF, FileKind::File, 0, 0o620),
        (CONF, FileKind::File, 0, 0o602),
        (CONF, FileKind::Symlink, 0, 0o777),
        ("/etc/wireguard", FileKind::Directory, 1000, 0o700),
    ];
    for (path, file_kind, uid, mode) in cases {
        let a = adapter(&["wg-office"]);
        a.files.set(path, file_kind, uid, mode);
        let tunnel = binding("wg-office");
        let adapter: &dyn PlatformAdapter = &a;
        assert_eq!(
            kind(adapter.check_registration(&tunnel)),
            AdapterErrorKind::ManagerConflict,
            "{path} {uid} {mode:o}"
        );
        assert_eq!(
            kind(adapter.start(&tunnel)),
            AdapterErrorKind::ManagerConflict,
            "{path}"
        );
        assert_eq!(
            kind(adapter.stop(&tunnel)),
            AdapterErrorKind::ManagerConflict,
            "{path}"
        );
        assert!(a.runner.calls().is_empty(), "{path}: {:?}", calls(&a));
    }
}

#[test]
fn f01_root_owned_setup_passes_and_start_issues_exactly_the_expected_argv() {
    let a = adapter(&["wg-office"]);
    let tunnel = binding("wg-office");
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.check_registration(&tunnel).unwrap();
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.runner.push(reply(""));
    a.start(&tunnel).unwrap();
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.runner.push(reply(""));
    a.stop(&tunnel).unwrap();
    assert_eq!(
        calls(&a),
        [
            show_argv(),
            show_argv(),
            start_argv("start"),
            show_argv(),
            start_argv("stop"),
        ]
    );
}

#[test]
fn f01_a_file_made_writable_after_registration_blocks_the_next_start() {
    let a = adapter(&["wg-office"]);
    let tunnel = binding("wg-office");
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    a.check_registration(&tunnel).unwrap();

    a.files.set(CONF, FileKind::File, 0, 0o666);
    assert_eq!(kind(a.start(&tunnel)), AdapterErrorKind::ManagerConflict);
    a.files.set(CONF, FileKind::File, 0, 0o600);
    a.files
        .set("/usr/bin/wg-quick", FileKind::File, 1000, 0o755);
    assert_eq!(kind(a.start(&tunnel)), AdapterErrorKind::ManagerConflict);
    assert_eq!(calls(&a), [show_argv()]);
}

#[test]
fn f01_a_unit_file_or_drop_in_made_writable_blocks_start_after_only_a_read() {
    let a = adapter(&["wg-office"]);
    let tunnel = binding("wg-office");
    a.files.set(
        "/usr/lib/systemd/system/wg-quick@.service",
        FileKind::File,
        1000,
        0o644,
    );
    a.runner
        .push(reply(&fixture("systemctl-show-active-exited.txt")));
    assert_eq!(kind(a.start(&tunnel)), AdapterErrorKind::ManagerConflict);
    // Only the read-only `systemctl show` that names the unit file ran.
    assert_eq!(calls(&a), [show_argv()]);
}

// Integrated review of 121b42a, finding 2: `NotFound` from the ownership
// guard tells the core the binding is lost and must be registered again. Only
// the tunnel's own configuration being gone means that. A missing wg-quick
// binary is a missing tool (`Unsupported`, as `exec` reports a tool it cannot
// run), and a unit file or drop-in that systemd names but the disk does not
// have is systemd and the disk disagreeing (`ManagerConflict`).

#[test]
fn f2_a_missing_wg_quick_binary_is_unsupported_not_a_lost_binding() {
    for missing in ["/usr/bin/wg-quick", "/usr/bin/wg", "/usr/bin"] {
        let a = adapter(&["wg-office"]);
        a.files.remove(missing);
        let tunnel = binding("wg-office");
        assert_eq!(
            kind(a.check_registration(&tunnel)),
            AdapterErrorKind::Unsupported,
            "{missing}"
        );
        assert_eq!(
            kind(a.start(&tunnel)),
            AdapterErrorKind::Unsupported,
            "{missing}"
        );
        assert_eq!(
            kind(a.stop(&tunnel)),
            AdapterErrorKind::Unsupported,
            "{missing}"
        );
        assert!(a.runner.calls().is_empty(), "{missing}: {:?}", calls(&a));
    }
}

#[test]
fn f2_a_unit_file_or_drop_in_systemd_names_but_the_disk_lacks_is_a_manager_conflict() {
    let lab = |missing: &str| {
        let a = adapter(&["wg-lab"]);
        for dir in [
            "/etc/systemd/system/wg-quick@.service.d",
            "/etc/systemd/system/wg-quick@wg-lab.service.d",
        ] {
            a.files.set(dir, FileKind::Directory, 0, 0o755);
        }
        for file in [
            "/etc/systemd/system/wg-quick@.service.d/10-order.conf",
            "/etc/systemd/system/wg-quick@wg-lab.service.d/20-local.conf",
        ] {
            a.files.set(file, FileKind::File, 0, 0o644);
        }
        a.files.remove(missing);
        a.runner
            .push(reply(&fixture("systemctl-show-drop-ins.txt")));
        a
    };
    for missing in [
        "/etc/systemd/system/wg-quick@wg-lab.service.d/20-local.conf",
        "/etc/systemd/system/wg-quick@.service.d",
        "/usr/lib/systemd/system/wg-quick@.service",
    ] {
        let a = lab(missing);
        assert_eq!(
            kind(a.check_registration(&binding("wg-lab"))),
            AdapterErrorKind::ManagerConflict,
            "{missing}"
        );
        let a = lab(missing);
        assert_eq!(
            kind(a.start(&binding("wg-lab"))),
            AdapterErrorKind::ManagerConflict,
            "{missing}"
        );
        // Only the read-only `systemctl show` that names the files ran.
        assert_eq!(calls(&a), [show_unit("wg-quick@wg-lab.service")]);
    }
}

#[test]
fn f2_only_a_configuration_that_is_gone_is_a_lost_binding() {
    for missing in [CONF, "/etc/wireguard"] {
        let a = adapter(&["wg-office"]);
        a.files.remove(missing);
        let tunnel = binding("wg-office");
        assert_eq!(
            kind(a.check_registration(&tunnel)),
            AdapterErrorKind::NotFound,
            "{missing}"
        );
        assert_eq!(kind(a.stop(&tunnel)), AdapterErrorKind::NotFound);
        assert!(a.runner.calls().is_empty(), "{missing}");
    }
}

// Review finding 5: an interface rebuilt under the same name is a different
// interface, and nothing measured through it is credited to the old one.

fn office_with_index(index: u32) -> String {
    fixture("ip-address-wg-office.json").replace("\"ifindex\":7", &format!("\"ifindex\":{index}"))
}

#[test]
fn f05_same_name_and_source_with_a_new_ifindex_is_a_different_interface() {
    let a = adapter(&["wg-office"]);
    let before = office(&a);
    a.runner.push(reply(&office_with_index(9)));
    let after = a.resolve_interface(&binding("wg-office")).unwrap();
    assert_eq!((&before.name, before.source), (&after.name, after.source));
    assert_eq!(before.incarnation, Some(7));
    assert_eq!(after.incarnation, Some(9));
    assert_ne!(before, after);
}

#[test]
fn f05_a_probe_or_route_check_across_a_rebuild_is_not_credited_to_the_old_interface() {
    let request = ProbeRequest {
        target: ProbeTarget {
            address: target(),
            probe: ProbeKind::Icmp,
        },
        timeout: Duration::from_secs(2),
    };
    let ping_answers = [
        reply(&fixture("ping-reply.txt")),
        Ok(Output {
            exit_code: Some(1),
            stdout: fixture("ping-no-reply.txt").into_bytes(),
            ..Output::default()
        }),
    ];
    for answer in ping_answers {
        let a = adapter(&["wg-office"]);
        let interface = office(&a);
        a.runner.push(answer);
        a.runner.push(reply(&office_with_index(9)));
        assert_eq!(
            kind(a.probe(&interface, &request)),
            AdapterErrorKind::NotFound
        );
        assert_eq!(
            argv(&a.runner.calls()[2]),
            (IP, vec!["-j", "address", "show", "dev", "wg-office"])
        );
    }

    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    a.runner
        .push(reply(&fixture("ip-route-via-wg-office.json")));
    a.runner
        .push(reply(&fixture("ip-route-via-wg-office.json")));
    a.runner.push(reply(&office_with_index(9)));
    assert_eq!(
        kind(a.check_route(&interface, target())),
        AdapterErrorKind::NotFound
    );

    let a = adapter(&["wg-office"]);
    let interface = office(&a);
    a.runner.push(reply(&fixture("ping-reply.txt")));
    a.runner
        .push(failure(1, "Device \"wg-office\" does not exist.\n"));
    assert_eq!(
        kind(a.probe(&interface, &request)),
        AdapterErrorKind::NotFound
    );
}
