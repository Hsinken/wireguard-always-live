# Testing

How WG Always Live is tested, what each kind of result may claim, and what
has been proven on a real computer so far.

## Default and CI tests do not touch a real VPN

Every test that runs by default, and in CI, runs against fakes and
fixtures:

- `wgal-testkit` gives the core a fake clock (`FakeClock`) and a fake
  platform (`FakePlatform`), so a test can stop a tunnel, let a probe time
  out or jump an hour ahead without a network.
- The adapters' parsers are tested against anonymized output in
  `tests/fixtures/` (documentation addresses and invented names only).
- The install scripts are tested with stand-ins for `systemctl`, `launchctl`,
  `sc.exe` and the other system tools, in temporary folders.

None of them starts, stops or probes a real tunnel, installs a service or
needs a secret. The only exceptions are the on-device checks described
below: they are ignored by default and refuse to run without the owner's
authorization for that run. Do not add any other test that touches a real
system: see the safety boundaries in [AGENTS.md](../AGENTS.md).

## The checks

From the repository root, the same as CI:

```sh
pnpm install --frozen-lockfile
pnpm run check                      # TypeScript, vitest, vite build
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked     # also regenerates the TypeScript bindings
```

After `cargo test`, `git status` must be clean. If
`apps/desktop/src/protocol/generated/` changed, the contract changed:
follow "Changing the shared contract" in `AGENTS.md`.

CI (`.github/workflows/ci.yml`) runs these on Linux, Windows and macOS for
every pull request.

### Checking the other platforms' code

On a computer without macOS or Linux, type-check those adapters with:

```sh
rustup target add x86_64-unknown-linux-gnu aarch64-apple-darwin   # once
cargo clippy --locked --all-targets --no-default-features --target x86_64-unknown-linux-gnu \
  -p wgal-protocol -p wgal-core -p wgal-platform -p wgal-runtime -p wgal-testkit -p wgal-mcp \
  -p wgal-client -p wgal-cli -- -D warnings
cargo clippy --locked --all-targets --no-default-features --target aarch64-apple-darwin \
  -p wgal-protocol -p wgal-core -p wgal-platform -p wgal-runtime -p wgal-testkit -p wgal-mcp \
  -p wgal-client -p wgal-cli -- -D warnings
```

This is a cross-check only. It is never a native build, a test run or an
on-device result. `wgal-desktop` cannot be cross-checked this way.

### The install scripts

| Platform | Tests |
|---|---|
| Ubuntu | `bash packaging/linux/tests/install-recovery.test.sh`, `uninstall.test.sh`, `make-release.test.sh`, `make-release.entry.test.sh`; `shellcheck -x packaging/linux/*.sh packaging/linux/tests/*.sh` |
| macOS | `bash packaging/macos/tests/install-recovery.test.sh`, `make-release.test.sh`, `make-release.entry.test.sh` (run them with macOS's own bash 3.2 and BSD tools) |
| Windows | `packaging/windows/tests/install-recovery.tests.ps1`, `revert-uninstall.tests.ps1`, `make-release.test.ps1`, `make-release.entry.test.ps1` |

They install nothing: every system tool they would call is a stand-in.

## What a result may claim

- Each check is **PASS**, **FAIL**, **NOT RUN** or **BLOCKED**. NOT RUN and
  BLOCKED are not passes.
- Record each one with:
  - the platform and version, and the commit;
  - the account it ran as, and the steps;
  - the output, the time and any side effects.
- A result from fakes or a simulation is never reported as an on-device
  result. A cross-check never stands in for a native build.
- An installer is "built", "installed on a device" or "accepted on a
  device". These are three different states.

## On-device tests

An on-device test touches a real tunnel, so it needs, every time:

- **The owner's authorization** for that run.
- **A way in that does not depend on the tunnel under test**: local access,
  or a connection that does not go through it. Never disable a VPN from
  the only remote session that runs through it.
- **A record and an automatic revert**: the state recorded first, a revert
  that does not depend on the window, and a check afterwards that
  everything is back.
- **The narrowest fault possible**: never the whole computer's firewall, a
  physical interface, the default route or other peers on a production
  server.

If any of these is missing, the test is BLOCKED. Output from a real computer
holds real tunnel names and addresses: keep it on that computer and never
commit it.

**Windows**: `crates/wgal-platform/src/windows/live.rs` has the on-device
checks as tests that are ignored by default. They also refuse to run unless
these are set for that run:

- `WGAL_LIVE_AUTHORIZED=yes-for-this-run`;
- `WGAL_LIVE_TUNNEL`, the tunnel's name;
- `WGAL_LIVE_CONTROL=yes-for-this-run` as well, for the test that stops and
  starts a tunnel;
- for the probe checks, the targets they need: `WGAL_LIVE_TARGET` (one
  that answers through the tunnel), `WGAL_LIVE_OFF_TUNNEL_TARGET` (one on
  another route) and `WGAL_LIVE_SILENT_TARGET` (an address inside the
  tunnel's routes that nothing answers);
- `WGAL_LIVE_W9_SECONDS`, how long the W9 check probes through an outage
  (300 s by default).

Run one check at a time, from a clean commit, for example the read-only
listing:

```powershell
$env:WGAL_LIVE_AUTHORIZED = 'yes-for-this-run'; $env:WGAL_LIVE_TUNNEL = '<tunnel>'
cargo test -p wgal-platform --locked windows::live::w1 -- --ignored --nocapture
```

**Ubuntu and macOS**: there is no such harness. On-device checks run through
the installed watchdog with `wgal`: `wgal show`, `wgal probe`,
`wgal disconnect`, `wgal connect` and `wgal reconnect`, with a revert started
in the background first.

## What has been proven on a device

As of 2026-11-19. Anything not listed is not proven.

### Ubuntu

On a server running Ubuntu Server 26.04 with one `wg-quick` tunnel, through
the installed watchdog (root), with WGAL 4.1.0, then 4.2.1:

- **PASS**:
  - the tools' output matches what the parsers expect;
  - a stop and a start of the unit finished within 30 s, and the interface
    came back with a new ifindex;
  - probes bound to the tunnel's interface: the target answered through
    the tunnel before and after a stop and start; a silent address inside
    the tunnel's routes got no reply; the LAN gateway and the computer's
    own addresses were refused before anything was sent;
  - the service runs as root, with files an ordinary user cannot write;
  - discovery of `wg-quick` tunnels;
  - after the 4.2.1 upgrade: a green light, a verified connect and a
    verified reconnect, automatic recovery could be turned on, and every
    setting was kept across the watchdog's restart and across a reboot.
- **Not proven**:
  - two tunnels, and overlapping routes;
  - that only the registered unit is ever controlled;
  - a real fault that automatic recovery fails to fix (the alert, retries
    and the "recovered" message);
  - a probe through an interface that vanishes during it (fakes only);
  - a desktop: these results come from a server without one.
- **By reading the code** (not tested): if the tunnel itself fails to come
  up at boot, WGAL does not start it. It acts only on a tunnel it has seen
  connected.

### Windows 11

On the developer's computer:

- **PASS**:
  - listing the tunnel services and mapping each to its interface, as an
    ordinary user and as an administrator;
  - a bound probe with one tunnel, including probes that must be refused.
- **In use**: the window 2.2.1 and the watchdog 2.3.1, from before the
  versions were unified, run on real tunnels there. 4.2.1 is not released
  for Windows yet.
- **Not proven**:
  - two tunnels;
  - controlling one tunnel without touching another;
  - a tunnel removed in the official app;
  - telling the computer's own network outage from a tunnel fault on a
    real network.

### macOS

- **PASS**: a native build and every test on a Mac (macOS 27, Apple
  silicon), and installing, upgrading (with a failure injected on purpose)
  and removing a 2.2.1 test build.
- **Found on a device**: 4.2.1, installed on that Mac (macOS 27.2.1), could
  not list the tunnels: that macOS writes the service type with a colon
  (`[VPN:com.wireguard.macos]`), and the parser accepted only a slash. Fixed
  in 4.2.2; the fixtures `nc_list_macos27.txt` and `nc_status_macos27.txt`
  copy the real layout and structure with invented values.
- **PASS with 4.2.2** (installed on that Mac on 2026-11-20, checked read
  only): the watchdog runs, lists both of the official app's tunnels, and
  reads a registered tunnel's state as connected, as it is. The light was
  gray (`not_configured`) until a recovery check target was set, then red
  (`path_unverified`): 4.2.2 does not count a bound probe on macOS.
- **On-device checks with 4.2.2** (2026-11-20, that Mac, one connected
  tunnel, through the installed watchdog, the user's own LaunchAgent; the
  owner authorized stopping and starting that tunnel and a few probes to
  addresses beside the target):
  - **PASS**: the target answered a probe bound to the tunnel (M3); the
    tunnel stopped and started through the watchdog within 30 s, without a
    prompt, its interface read again and its `ConnectCount` one higher
    (M2, M6); while it was stopped, nothing routed through its old
    interface and nothing answered there, and it did not connect again by
    itself (M4; M5 observed only); after the SSH session that stopped it
    had ended, a request through the watchdog connected it again; a silent
    address inside the tunnel's routes got `no_reply`; the LAN gateway and
    the Mac's LAN address were refused before sending (`route_mismatch`);
    the watchdog's folder and socket are the user's alone.
  - **FAIL**: the tunnel's own address was not refused. Its route leaves
    through the tunnel with the `LOCAL` flag, which 4.2.2 did not read, so
    one echo was sent to it; it got no reply, not a false one. Fixed in
    4.3.0: a route that is not up, or has `LOCAL`, `BROADCAST`,
    `MULTICAST`, `BLACKHOLE` or `REJECT`, is refused.
  - **Found**: on macOS 27.2.1, `ping -b` with an interface that does not
    exist exits with 1 and `ping: bad interface name`, not 68; 4.3.0 reads
    both as `NotFound`.
- **4.3.0**: the bound ICMP probe is `Supported`, by the owner's decision
  of 2026-11-20, although the check above failed on 4.2.2.
- **Checked again with 4.3.0** (installed on that Mac by the owner on
  2026-11-20, the same tunnel, through the installed watchdog):
  - **PASS**: the window, the watchdog, `wgal` and `wgal-mcp` are 4.3.0,
    and the watchdog reports the bound ICMP probe `Supported`; the light
    turned green once the recovery check target answered through the
    tunnel; the LAN gateway, the Mac's LAN address and the tunnel's own
    address were all refused before sending (`route_mismatch`), and the
    same silent address got `no_reply`; a disconnect, a connect and a
    reconnect through `wgal` each exited 0; after the disconnect the tunnel
    stayed disconnected until the connect, and nothing connected it again;
    the connect and the reconnect were each followed by a new verified
    probe and a green light; turning automatic recovery on was
    accepted; the end state was the state before, the tunnel's
    `ConnectCount` two higher.
  - The owner had turned automatic recovery on after the upgrade; the
    check turned it off and on again and left it on.
  - The bound ICMP probe stays `Supported`, now on the device, one tunnel.
- **Not proven**: two tunnels; the Keychain while the screen is locked;
  monitoring after the window quits or the screen locks; TCP probes; a
  fault that automatic recovery has to repair.

### The on-device checks by number

Comments in the code cite these numbers. Each check's steps are written
for the computer it runs on; the table gives what each one proves and
where it stands.

| Check | What it proves | Status |
|---|---|---|
| W0 | Windows: the tools' versions and the environment, recorded read-only | PASS |
| W1 | Windows: the tunnel services are listed, one is picked, and its interface is found | PASS, as an ordinary user and as an administrator; "disconnected while stopped" NOT RUN |
| W2 | Windows: the same reads and controls work as the background service's account | BLOCKED |
| W3 | Windows: probes leave from the tunnel's own address and interface; targets on other routes are refused | PASS with one tunnel; the two-tunnel item NOT RUN |
| W4 | Windows: controlling one tunnel leaves the others alone | BLOCKED (needs two authorized tunnels) |
| W5 | Windows: a tunnel removed in the official app | BLOCKED |
| W6 | Windows: the named pipe's access list and the caller's identity | NOT RUN on a device (local Win32 tests only) |
| W7 | Windows: monitoring goes on with the window closed, quit or signed out | NOT RUN as a check |
| W8 | Windows: install, upgrade, removal and revert | NOT RUN as an acceptance check (the service is installed and in use on the developer's computer) |
| W9 | Windows: telling the computer's own network outage from a tunnel fault, and network change notices | BLOCKED |
| U0 | Ubuntu: the tools' versions and output formats match the parsers | PASS |
| U1 | Ubuntu: a controlled stop and start of one unit, waiting for its state | PASS |
| U2 | Ubuntu: the interface is rebuilt with a new ifindex after a restart | PASS |
| U3 | Ubuntu: bound probes through a stop and start, and probes that must be refused | Items 1, 3, 3a, 5 and the refusals PASS; item 2 as root BLOCKED; item 4 (a second path to the same target) NOT RUN |
| U4 | Ubuntu: the service's fixed rights | Partly PASS (root service, file permissions, no prompts); "only the registered unit is controlled" NOT RUN |
| U5 | Ubuntu: controlling one tunnel leaves the others alone | NOT RUN (one tunnel only) |
| U6 | Ubuntu: the ownership checks at registration | PASS (indirect inside `/etc/wireguard`) |
| U7 | Ubuntu: discovery tells `wg-quick` tunnels from other managers | PASS |
| M1 | macOS: `scutil --nc list` and `status` match the parser | PASS (4.2.2's installed watchdog on macOS 27.2.1, 2026-11-20): it lists both tunnels; it reads a registered tunnel's state, and, in its probe rounds, the interface name and `ConnectCount` |
| M2 | macOS: the interface is found from the configuration each time; the connection count tells reconnects apart | PASS (4.2.2, 2026-11-20, one tunnel): read again after a reconnect, `ConnectCount` one higher |
| M3 | macOS: `ping -b` is bound to the interface | 4.2.2, 2026-11-20: a reply from the target and `no_reply` from a silent address in the tunnel's routes, PASS; the LAN gateway and the Mac's LAN address refused before sending, PASS; the tunnel's own address not refused (no reply), FAIL. 4.3.0 installed, 2026-11-20: all three refused before sending, the same silent address `no_reply`, PASS |
| M4 | macOS: routes, state and probes during a stop, with timestamps | PASS (4.2.2, 2026-11-20): over a 15 s stop, the state, the route and a probe on the old interface, each sample with its own times |
| M5 | macOS: On-Demand behaviour | Observed only: no reconnect the test did not cause, during one 15 s stop (4.2.2, 2026-11-20); On-Demand itself NOT RUN |
| M6 | macOS: queries, starts and stops as the LaunchAgent | PASS (4.2.2, 2026-11-20): a stop and a start through the watchdog, each within 30 s, without a prompt, one of them after the SSH session had ended. 4.3.0 installed, 2026-11-20: a disconnect, a connect and a reconnect through `wgal`, each exit 0; the tunnel stayed disconnected until the connect; the connect and the reconnect each followed by a new verified probe, PASS |
| M7 | macOS: monitoring goes on after the window quits | NOT RUN (not in the run the owner authorized on 2026-11-20) |
| M8 | macOS: probes and reconnects after the screen locks | NOT RUN (not in the run the owner authorized on 2026-11-20) |
| M9 | macOS: the Keychain while the screen is locked | NOT RUN (not in the run the owner authorized on 2026-11-20) |
| M10 | macOS: two, then three tunnels, each probed on its own; restarting one leaves the others alone | NOT RUN (one tunnel only) |
| M11 | macOS: whether a TCP probe can be bound to the interface | NOT RUN (not in the run the owner authorized on 2026-11-20) |

### What the adapters report

The core shows a green light, passes a baseline and allows automatic
recovery only where the bound ICMP probe is `Supported`.

| Capability | Windows | macOS | Ubuntu |
|---|---|---|---|
| Bound ICMP probe | Supported (on device, one tunnel) | Supported from 4.3.0 (on device, one tunnel) | Supported (on device, one tunnel) |
| Bound TCP probe | Unsupported | Unsupported | Unsupported |
| Reading metrics | Unsupported | Unsupported | Unknown |
| State queries, start and stop, background operation | Unknown | Unknown | Unknown |

Before 4.3.0 the light is never green on macOS and automatic recovery cannot
be turned on there; that was deliberate, not a fault. From 4.3.0 the
owner's decision of 2026-11-20 makes the bound ICMP probe `Supported` (see
"macOS" above).

A capability changes only after its on-device checks pass, or by the
owner's decision (macOS in 4.3.0), in a commit by the platform's package
that cites them.
