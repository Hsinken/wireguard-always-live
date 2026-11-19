# Architecture

How WG Always Live (WGAL) is put together: its programs, the crates, the
local connection, the state machine that decides when a tunnel is healthy
and when to recover it, what it stores, and what each platform does.

The source is the reference; this page is the map. Every test it mentions
runs against fakes (`wgal-testkit`): none of them touches a real VPN.

## The programs

| Program | What it does | Runs as |
|---|---|---|
| `wgal-watchdog` | Probes each registered tunnel, decides its light, recovers it when allowed, keeps the event log and sends the Telegram messages | Windows: a service (LocalSystem). Ubuntu: a systemd service (root). macOS: the user's own LaunchAgent |
| `wgal-desktop` | The window and the tray or menu bar icon | The signed-in user, no privileges |
| `wgal` | The command line: everything the window does with the watchdog | The user who runs it |
| `wgal-mcp` | The MCP server an AI agent starts and talks to over standard input and output | The user whose agent starts it |

```text
 the window ─────────────┐
 wgal (command line) ────┤  requests from a closed list,
 wgal-mcp (AI agents) ───┤  over a local named pipe or Unix socket
                         ▼
                  wgal-watchdog ── probes through each tunnel's interface
                         │
                         ▼
          the official WireGuard manager
   (Windows services, the macOS app, wg-quick units)
```

Only the watchdog acts on tunnels. The window, `wgal` and `wgal-mcp` send
it requests and show its answers; they never start or stop a tunnel
themselves, run no command they are given, and take no path, executable or
service name from the user or the agent.

## Crates

| Crate | Path | Contents |
|---|---|---|
| `wgal-protocol` | `crates/wgal-protocol/` | The shared contract: requests, answers, validated IDs and texts, limits, the frame codec; the TypeScript bindings are generated from it |
| `wgal-core` | `crates/wgal-core/` | The operating-system-independent state machine; the `PlatformAdapter` trait and `Clock` |
| `wgal-platform` | `crates/wgal-platform/` | The bounded tool runner (`exec`) and the Windows, macOS and Ubuntu adapters |
| `wgal-runtime` | `crates/wgal-runtime/` | The watchdog: the loop that drives the core, request authorization, storage, the event log, notifications and each platform's service entry |
| `wgal-client` | `crates/wgal-client/` | The way to the watchdog that `wgal` and `wgal-mcp` share |
| `wgal-cli` | `crates/wgal-cli/` | `wgal` |
| `wgal-mcp` | `crates/wgal-mcp/` | `wgal-mcp` |
| `wgal-testkit` | `crates/wgal-testkit/` | `FakeClock` and `FakePlatform`, for tests only |
| `wgal-desktop` | `apps/desktop/src-tauri/` | The window's Tauri 2 shell; the interface itself is TypeScript in `apps/desktop/src/` |

Dependencies point one way:

- `wgal-core` uses `wgal-protocol`; `wgal-platform` uses both; `wgal-runtime`
  uses all three.
- `wgal-desktop`, `wgal-client`, `wgal-cli` and `wgal-mcp` use the protocol
  and, of `wgal-platform`, only the functions that connect to the watchdog
  (and check that it is the watchdog), that say where a file really is, and,
  for `wgal`, the terminal's hidden input for the Bot Token. They never use
  the runtime or call adapter or service code.

`AGENTS.md` lists the exact functions each may use.

## The local connection

| Platform | Endpoint | Who the server must be |
|---|---|---|
| Windows | the named pipe `\\.\pipe\wgal-watchdog` | owned by LocalSystem |
| Ubuntu | `/run/wgal/watchdog.sock` | root owns the socket and its folder, and is the kernel's peer |
| macOS | `~/Library/Application Support/WGAL/watchdog.sock` | the signed-in user owns the socket and its folder, and is the peer |

- **Frames**: each message is a 4-byte little-endian length, then one
  encoded message of at most 64 KiB, checked before the body is read
  (`wgal_protocol::codec`). `PROTOCOL_VERSION` is 1.
- **Both ends check each other**: a client connects only if the endpoint is
  owned as the table says. The watchdog asks the operating system who the
  caller is; that identity never travels on the wire.
- **The access list** (Windows and Ubuntu, written at install time) names
  the accounts that may use WGAL and the tunnels they may register and hand
  to automatic recovery. On macOS the watchdog is the user's own and admits
  only that user.

| Request | Needs |
|---|---|
| Any request | A caller on the access list, SYSTEM or root, or an elevated administrator; anyone else gets `unauthorized` and changes nothing |
| `RegisterTunnel` | Also elevated, or the tunnel's binding (as the watchdog itself discovered it) on the access list's tunnels |
| Turning on automatic recovery, or "start when stopped externally" | Also elevated, or the registered tunnel's binding on the access list's tunnels |
| Everything else, and turning those two off | Only a caller on the access list |

The check always uses the binding the watchdog discovered or registered,
never a value from the request.

**Requests** come from a closed `Request` enum. Requests from `wgal-mcp`
are marked `origin: mcp`. The watchdog answers them only while MCP is on,
and refuses those that change the Telegram settings unless the user has
let agents set Telegram. The window's and `wgal`'s requests carry no
origin: they act as the user.

## The state machine

`wgal-core` never sleeps, opens a socket or runs a program. The runtime
calls it; it plans the work that is due, the work runs through the
`PlatformAdapter`, and the results are applied. Because it only reads a
`Clock`, every behaviour below is tested with a fake clock and a fake
platform.

### What it keeps per tunnel

| Layer | Holds |
|---|---|
| Intent (stored) | Wanted connected or disconnected; monitoring on or off; automatic recovery; start when stopped externally; probe timing; notifications for this tunnel |
| Takeover (stored) | `observe_only` until the baseline passes and the user allows automatic recovery (`managed`); `stopped_externally`, `externally_controlled`, `binding_lost` |
| Operating-system state | Connected, disconnected, in transition, or unknown; a failed read is unknown, never disconnected |
| Data health | `healthy`, `suspect`, `failed`, `unknown`, `stale` |
| Recovery activity | `idle`, `queued`, `stopping`, `starting`, `verifying`, `backoff`, `blocked` |
| Incident (stored) | The incident's ID, the first recovery's outcome, the retries used, the next retry and each notification's state |

A newly registered tunnel is `observe_only`: registering changes nothing
on the tunnel.

### A monitoring round

1. Read the tunnel's state from the manager.
2. If it is connected, resolve its interface. For each target: check the
   route to it, probe it through that interface, then check the route
   again. Resolve the interface once more at the end.
3. Apply the result:
   - an answer through the right interface, with the same route before and
     after, is a success;
   - no answer is a failure;
   - a route that changed or leads to another interface is a
     `route_conflict`, never a success or a failure;
   - an interface that was rebuilt during the round throws the whole round
     away;
   - a probe tool that is missing, times out or prints something unexpected
     is `probe_unavailable`, not a failure;
   - a computer that is offline is `local_network_offline`: nothing is
     counted and nothing is recovered.

A probe counts only if the platform can bind it to the tunnel's interface.
Where that capability is not `Supported`, the path is `path_unverified`: the
light is never green and automatic recovery cannot be turned on. Only the
recovery check target decides the tunnel's health; observe-only targets
are shown and never trigger recovery.

### Timing

Each tunnel has its own probe timing (`ProbeTiming`):

| Setting | Range | Default |
|---|---|---|
| Interval | 10–3600 s | 15 s |
| Timeout | 1–10 s | 2 s |
| Retries | 0–10 | 2 |
| Minimum fault time | 0–3600 s | 60 s |

- **A fault is confirmed** after `retries + 1` failed rounds in a row,
  lasting at least the minimum fault time.
- **A result is fresh** for interval + timeout + 18 s (35 s by default),
  plus timeout + 1 s for each observe-only target. A tunnel without a fresh
  success is not green.
- **Grace**: for 30 s after the watchdog starts, a wake from sleep, a
  network change or monitoring being turned back on, failures are not
  counted. A wake or a network change also discards the earlier evidence,
  so the light stays yellow until a round planned after it answers.
- **Start and stop**: each phase may take 30 s, with the state read every
  second. Only the state counts, not a command's exit code.
- **Verification** after a recovery: up to 45 s, a round every 5 s.
- **Recovery retries**, per tunnel: by default 100 rounds after the first,
  60 s apart (0 to 3600 rounds, or until the fault ends; 10 to 10800 s
  apart).
- **An incident closes** after 120 s of fresh, healthy rounds.

All intervals use a monotonic clock. The wall clock is used for display,
the log, and resuming a backoff after a restart.

### Recovery

Automatic recovery restarts only the failed tunnel, and only when all of
these hold:

- storage works;
- monitoring and automatic recovery are on, and the user wants the tunnel
  connected;
- the baseline passed: the recovery check target has answered through this
  tunnel at least once (a refused TCP connection proves the path but does
  not pass the baseline);
- WGAL does not see another manager in control, and the binding still
  exists;
- not in grace, not offline, the probe available, and the path neither
  unverified nor in conflict;
- the fault is confirmed.

A round takes the computer-wide control slot, stops the tunnel, waits for
it to be disconnected, starts it, waits for it to be connected, releases
the slot, and then verifies traffic. Before the first command, the
incident and an unfinished-operation record (`resume`) are stored, so a
restart of the watchdog finishes the round instead of leaving the tunnel
down. Before every start or stop, the adapter checks the registration
again.

### What the user does, and what others do

| Event | What WGAL does |
|---|---|
| The user disconnects | Stores the intent first; cancels the pending retry and notification; stops the tunnel and keeps it down, retrying the stop until it is down |
| The user connects or reconnects | Stores the intent; starts (or stops and starts) and verifies |
| A managed tunnel is stopped by someone else | Marks it `stopped_externally` and does not start it, unless "start when stopped externally" is on |
| A tunnel the user disconnected is started by someone else (On-Demand, for example) | Marks it `externally_controlled`, shows the real state and does not fight it |
| The recovery check target changes | The baseline is reset and automatic recovery is turned off |
| Monitoring is paused (one tunnel, or all) | No probes and no automatic actions; the light is gray. A command already sent finishes (a stop is always followed by its start) |
| Unregister | Forgets WGAL's registration only; never touches the tunnel |

### Lights

| Light | Main reasons |
|---|---|
| Green | `verified`: a fresh success through the tunnel's interface |
| Yellow | `grace_period`, `confirming` (failures not yet confirmed), `starting`, `reconnecting`, `verifying` |
| Red | `fault_confirmed`, `recovery_failed` (with the next retry), `retries_exhausted`, `route_conflict`, `path_unverified`, `probe_unavailable`, `baseline_failed`, `status_unknown`, `stopped_externally`, `externally_started`, `control_failed`, `local_network_offline`, `binding_lost` |
| Gray | `monitoring_off`, `disconnected_by_user`, `not_connected`, `not_configured` |

Each light carries its reason, the time it last changed and, for a yellow
light, the time it will end.

### Incidents and notifications

- A confirmed fault opens an incident. It stays open across a manual
  reconnect, the window quitting and a watchdog restart, and closes after
  120 s of healthy rounds.
- **Three messages**: an alert when the first automatic recovery of an
  incident fails or cannot be carried out; `retries_exhausted` when a
  limited number of retries is used up; `recovered` when an incident that
  automatic recovery worked on closes.
- A message is marked `reserved` in storage before it is sent. A message
  whose result is not known (for example, the watchdog stopped while
  sending) becomes `unknown` and is never sent again automatically.
- Notifications can be turned off for the whole computer or for one tunnel.

## Storage

- **State** (`state.json`): written to a temporary file, flushed, the
  current file kept as `state.prev.json`, then renamed into place and made
  durable before the save returns. A lock file keeps a single writer. A
  newer or unreadable file is never overwritten: the watchdog opens it read
  only and reports `storage: failed`.
- **Restored from the previous file**: automatic recovery is turned off,
  unfinished operations and retries are dropped, and pending notifications
  become `unknown`; the user decides again.
- **When a save fails**: no new automatic recovery or notification starts,
  and requests that need a save are refused. Monitoring goes on, and the
  save is retried every 15 s.
- **The event log** (`events.jsonl`): one small JSON object per line. The
  file rotates at 1 MiB or after 24 hours, and lines are kept for about 72
  hours (at most 8 files). Tunnels appear only as a random label chosen at
  registration. Target addresses, names, the Chat ID, the token and text
  from the operating system are never written.

## The runtime loop

`wgal-runtime` drives the core from one thread and runs the platform work
on others.

### Threads

| Thread | How many | Does |
|---|---|---|
| Owner | 1 | Holds the core and the notifier, plans, applies results, answers requests and writes the event log |
| Workers | Up to the effective count, started only when there is work | Run planned probes, state reads, starts and stops, and hand the results back |
| Notifications | 1 | Sends one message at a time over HTTPS |

- **Stack**: each worker has a 512 KiB stack, a quarter of Rust's default.
  A worker runs only the core's handling of one item and the adapter's
  checks around a platform call. Win32 calls run on a thread of their own;
  on Ubuntu and macOS the tools are child processes, read by the runner's
  own threads. The largest local buffer on a worker is the runner's 4 KiB
  read buffer. The size is an estimate from the code with a large margin,
  not a measurement.
- **The control slot**: one worker is always kept free for the tunnel that
  holds it, so a stop or start never waits behind probes.
- **Requests about a tunnel** wait while that tunnel's stop or start is
  out, and are answered right after.
- **A deadline is not a "no"**: a request whose deadline passes may still be
  applied, so a `timeout` means the result is unknown.

### Worker count and the suggested value

- **The limit** is the number of monitored tunnels plus one, at least 2.
- **The user's setting** (2 to 17) is kept within that limit.
- **Without a setting**, the watchdog uses the suggested value
  (`driver::suggested_workers`): the fewest workers that keep every round on
  time even if every probe times out, plus one for the control slot.

For a load of m monitored tunnels that probe something, the suggested value
is found like this:

- **D**, the longest round, is the largest of targets × (timeout + 1 s)
  over the tunnels. A probe that times out takes up to timeout + 1 s.
- **W(k)**, the longest wait for a worker with k workers for rounds, is
  ⌈(m − k) / k⌉ × D, and 0 when k ≥ m.
- **S**, the slack, is the smallest over the tunnels of
  timeout + 18 − (targets − 1) × (timeout + 1), at least 0.
- **k** is the first value from 1 to m − 1 with W(k) ≤ S, or m when none
  works. The suggestion is k + 1, kept between 2 and the limit.

| Monitored tunnels | D | S | k | Suggested |
|---|---|---|---|---|
| 2, defaults (1 target, timeout 2 s) | 3 s | 20 s | 1 | 2 |
| 10, defaults | 3 s | 20 s | 2 | 3 |
| 16, defaults | 3 s | 20 s | 3 | 4 |
| 2, 5 targets each | 15 s | 8 s | 2 | 3 |
| 16, 5 targets each | 15 s | 8 s | 16 | 17 |
| 16, timeout 10 s | 11 s | 28 s | 6 | 7 |

`crates/wgal-runtime/tests/capacity.rs` simulates these loads with a fake
clock and checks that rounds start on time and every fault is confirmed.

### Host settings and the time zone

- **Settings**: the worker count and the time zone are stored with the
  state.
- **The time zone**: without a configured one, the system's is used. It is
  read again right before each message is written, when a snapshot or the
  event log is asked for, and every ten minutes. On Linux and macOS a
  change of `/etc/localtime` takes effect at once; on Windows within five
  minutes.
- **The host name** is read the same way. Messages use it when no device
  name is set.

## Notifications

- **Telegram only**: messages go to the official Telegram Bot API over
  HTTPS, never to a configurable address. POST is not retried
  automatically and redirects are not followed.
- **The Bot Token** is write-only: no answer, log, export or error message
  contains it. Where it is kept:
  - Windows: a file protected with DPAPI;
  - macOS: the user's login Keychain;
  - Ubuntu: a file readable only by root.
- **Templates**: messages are written from the templates the window or
  `wgal` gave the watchdog, or from the built-in English set. Each template
  must use its placeholders exactly once (see [languages.md](languages.md)).

## Platforms

| | Windows 11 | macOS | Ubuntu |
|---|---|---|---|
| Tunnels | The official app's `WireGuardTunnel$<name>` services, through the service manager (query, start, stop only) | The official app's VPN configurations, with `/usr/sbin/scutil --nc list/status/start/stop <UUID>` | `wg-quick@<interface>.service`, through `systemctl` |
| Bound probe | `IcmpSendEcho2Ex` from the tunnel's own address, with `GetBestRoute2` checking the interface | `/sbin/ping -b <utun>`, the interface resolved from the configuration each time | `ping -4 -I <interface>` with `ip -j route get … oif <interface>` |
| Interface identity | LUID | The connection count and the configuration that resolved it | ifindex |
| Bound ICMP probe | Supported | Unknown (not yet proven on a Mac) | Supported |
| Bound TCP probe | Unsupported | Unsupported | Unsupported |
| Token storage | DPAPI-protected file | Keychain | root-only file |

Every external tool runs from a fixed path with a fixed argument list, never
through a shell, with its output capped and a deadline. Every parameter is
validated before it reaches a command, and a display name never does.
[testing.md](testing.md) says what each capability's value rests on.

## Safety boundaries in the code

- **No generic execution**: no code path runs a command, executable, path
  or service name it is given.
- **Only WGAL's own settings**: WGAL never edits WireGuard configurations,
  keys, routes, DNS, firewall rules, MTU, On-Demand rules or whether a tunnel
  starts at boot.
- **The window holds no power**:
  - it loads only packaged content, under a content security policy;
  - each Tauri command is declared and granted with the least privilege;
  - it sends requests from the closed list only.
- **Exports are redacted**: addresses, UUIDs, account paths, public keys,
  endpoints and Chat IDs are masked, secrets removed, and no whole
  configuration or system log is included.
- **One file may use `unsafe`**: `unsafe_code` is denied everywhere except
  the Windows wrapper `crates/wgal-platform/src/windows/sys.rs`, where every
  `unsafe` block is documented.

The full rules are in [AGENTS.md](../AGENTS.md).
