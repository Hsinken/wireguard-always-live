# AGENTS.md

Instructions for humans and coding agents working in this repository.

WGAL (wg-always-live) is a desktop watchdog for existing official WireGuard
tunnels on Windows 11, macOS and Ubuntu. The official WireGuard software stays
the VPN engine. WGAL verifies that traffic actually flows through each tunnel,
shows the result, recovers only the tunnel that failed when it has been
authorized to, and sends a Telegram alert when the first automatic recovery
fails.

## Scope and authority

- The owner keeps the product specification privately; it is not in this
  repository. This file carries every rule from it that work here needs.
  If this file, the code and a task instruction conflict, or a decision
  needs the specification, mark the work BLOCKED and ask the owner; do not
  pick one yourself, and put no private data in the question.
- Comments in the code that cite "plan §n" or "the plan" point to sections
  of that specification. The rule each one names is written in this file or
  in the code beside it.
- Work is delivered in work packages: A bootstrap and contract
  (integration owner), B macOS, C Ubuntu, D Windows, E core state machine,
  F UI, G notifications, H integration review and release, and since the
  owner's decision of 2026-11-04 I MCP (the agents' server, `wgal-mcp`),
  which since the owner's decision of 2026-11-18 also owns the `wgal`
  command line and the client library both share. Do
  only the package you were assigned. Each package has one owner and ends in its own commits.
- Changes to the shared contract (`crates/wgal-protocol`), workspace members,
  lockfiles or toolchain pins go through the integration owner (A), so no two
  packages change them at the same time.
- A missing machine or missing test authorization blocks only the matching
  on-device acceptance. Keep developing everything else.
- Ubuntu without a desktop (a server) is supported too, by the owner's
  decision of 2026-11-18: `install.sh --no-window` installs no menu or autostart
  entry and needs none of the window's packages, and WGAL is used there
  with its English command line, `wgal`. English is WGAL's default
  language (same decision): the watchdog's built-in Telegram templates are
  the GUI's English ones.
- Plans, review notes and on-device records are kept outside this
  repository. They hold real names and addresses.

### File ownership

Each package edits only the paths it owns. Anything else, including a new
dependency, a new feature on an existing one, or a change to a shared trait or
type, is requested from the integration owner with the exact change and the
reason; the requesting package does not make it.

| Package | Owns |
|---|---|
| A integration | `crates/wgal-protocol/`, `apps/desktop/src/protocol/`, every `Cargo.toml`, `Cargo.lock`, `package.json`, `pnpm-lock.yaml`, `apps/desktop/vite.config.ts`, `rust-toolchain.toml`, `.github/`, `.gitattributes`, `.gitignore`, `AGENTS.md`, `README.md`, `README.zh-TW.md`, `crates/wgal-core/src/{lib,platform,clock}.rs`, `crates/wgal-platform/src/{lib,exec,socket,terminal}.rs`, `crates/wgal-runtime/src/lib.rs`, `crates/wgal-runtime/src/bin/`, `crates/wgal-testkit/`, `packaging/INSTALL.md`, `packaging/INSTALL.zh-TW.md`, `docs/*.md`, `SECURITY.md`, `CHANGELOG.md` |
| B macOS | `crates/wgal-platform/src/macos/`, `crates/wgal-runtime/src/macos/`, `tests/fixtures/macos/`, `packaging/macos/` |
| C Ubuntu | `crates/wgal-platform/src/linux/`, `crates/wgal-runtime/src/linux/`, `tests/fixtures/linux/`, `packaging/linux/` |
| D Windows | `crates/wgal-platform/src/windows/`, `crates/wgal-runtime/src/windows/`, `crates/wgal-runtime/tests/windows*.rs`, `tests/fixtures/windows/`, `packaging/windows/` |
| E core | `crates/wgal-core/src/` other than A's files, `crates/wgal-core/tests/`, `crates/wgal-runtime/src/` other than A's, B's, C's, D's and G's files, `crates/wgal-runtime/tests/` other than D's and G's files |
| F UI | `apps/desktop/index.html`, `apps/desktop/src/` other than `src/protocol/`, `apps/desktop/src-tauri/src/`, `apps/desktop/src-tauri/capabilities/`, `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/tauri.conf.json` |
| G notifications | `crates/wgal-runtime/src/notify/`, `crates/wgal-runtime/tests/notify.rs` |
| I MCP and CLI | `crates/wgal-mcp/`, `crates/wgal-client/` and `crates/wgal-cli/`, each other than its `Cargo.toml` |

E adds its modules to `crates/wgal-core/src/lib.rs` and `crates/wgal-runtime/src/lib.rs`,
and G its `notify` module to `crates/wgal-runtime/src/lib.rs`, by appending
`mod`/`pub use` lines only; A keeps the rest of those files. The Windows
secret protection G needs (DPAPI) is a safe wrapper in D's
`crates/wgal-platform/src/windows/sys.rs`, because that is the only file
allowed `unsafe`.

The second wave (the watchdog loop, IPC server, background services and
packaging) started on 2026-11-03 by the owner's decision, while the review
of the last core fix and the Codex review were still running; their P1 and
P2 findings are fixed before any on-device soak. Windows comes first. E owns
the OS-independent part in `crates/wgal-runtime/src/`: the loop that drives
the core, request authorization and the event log. D owns the Windows
service, the named pipe server and the installation's access list in
`crates/wgal-runtime/src/windows/`, with every Win32 call in `sys.rs`, and
`packaging/windows/`. The `wgal-watchdog` executable in
`crates/wgal-runtime/src/bin/` stays a thin dispatch to the platform's
entry point. The owner assigned the macOS and Ubuntu service paths on
2026-11-05, to be built before their on-device tests: E owns the local
socket server and the hosting both share in `crates/wgal-runtime/src/socket/`
(the server is tested on every OS over loopback TCP), C the Ubuntu entry
(root systemd service, the access list `/etc/wgal/access.conf`) in
`crates/wgal-runtime/src/linux/` and the socket's client side in
`crates/wgal-platform/src/linux/socket.rs`, B the macOS entry (the user's
own LaunchAgent) in `crates/wgal-runtime/src/macos/` and
`crates/wgal-platform/src/macos/socket.rs`, and each its install scripts in
`packaging/linux/` and `packaging/macos/`. A keeps the folder and socket
checks both clients share (`crates/wgal-platform/src/socket.rs`). Releases
are zips installed and removed by these scripts, not installers, and are
not code-signed (owner decisions of 2026-11-05).

Commit subjects start with the package, for example `WGW-001-C: parse systemctl
show output`. A package's commits touch only its own paths.

### `unsafe`

`unsafe_code` is denied workspace-wide. The only exception is
`crates/wgal-platform/src/windows/sys.rs` (D), which opens with
`#![allow(unsafe_code)]`, wraps each Win32 call in a safe function, and puts a
`// SAFETY:` comment on every `unsafe` block (clippy denies undocumented ones).
Any other exception goes through the integration owner. The one accepted so
far: `windows_service::define_windows_service!` in
`crates/wgal-runtime/src/windows/service.rs` expands to the crate's own
`extern "system"` service entry, which the lint does not see; nothing else in
that file may need `unsafe`.

## Safety boundaries

These are hard rules. If a task seems to need an exception, stop and ask the
owner. Do not work around them.

1. **Never touch live VPNs.** Do not start, stop, restart, reconfigure or
   fault-inject any real tunnel, WireGuard service, Network Extension, route,
   DNS, firewall rule or MTU, on a development machine or in CI. Tests use
   `wgal-testkit` fakes. Asking for development is not authorization to
   interrupt a tunnel in use. A live test needs explicit, per-run owner
   authorization and all of the following:
   - local access or a management path that does not depend on the tunnel
     under test; never disable a VPN from the only SSH or remote-desktop
     session that runs through it;
   - the state recorded beforehand, an automatic revert that does not depend
     on the GUI, and a check afterwards that everything was restored;
   - the narrowest fault possible: an isolated test tunnel or a revertible
     per-target fault. Never change the whole machine's firewall, a physical
     interface, the default route, or other peers on a production server.

   If any of these is missing, mark the test BLOCKED and move on.
2. **Preserve official WireGuard settings.** Never edit, export, regenerate,
   rotate, move or delete WireGuard configs, keys, tunnel services or
   boot-start settings. Never reinstall or replace the official manager.
   Stopping, removing or uninstalling WGAL must leave whether each tunnel
   starts at boot unchanged. Do not redo the owner's completed host work
   (PersistentKeepalive, OS upgrades, other maintenance).
3. **Stay inside the product scope.** WGAL never changes
   AllowedIPs, DNS, routes, firewall rules, MTU, On-Demand or VPN boot-start
   settings, and never bypasses an existing VPN or kill-switch policy to reach
   Telegram. On macOS it drives the official app only: do not install
   Homebrew, wireguard-go or wg-quick, rebuild configurations or switch
   managers. On Ubuntu it drives `wg-quick@.service` through systemd only, and
   never takes over or installs NetworkManager. Out of scope: key management,
   cloud back ends, accounts, multi-device management, mobile apps, Telegram
   remote control or webhooks, traffic statistics and auto-update.
4. **Do not inspect secrets.** Do not read, print, copy or log private keys,
   `wg showconf` or `wg show ... dump` output, notification tokens, Keychain
   or Credential Manager entries, or `.env` files. Addresses, UUIDs, tunnel
   and interface names, account and host names seen on a real computer are
   local verification data: never put them in code, defaults, fixtures,
   documents or commit messages. Fixtures use invented identifiers and
   documentation addresses (`192.0.2.0/24`, `198.51.100.0/24`,
   `203.0.113.0/24`).
5. **Handle the Telegram token as a secret.** It never goes into
   `localStorage`, ordinary JSON settings, diagnostics, logs or error text,
   including URL paths in HTTP client errors. Send only to the official
   Telegram Bot API over HTTPS, never to a configurable URL, with automatic
   POST retries and redirects turned off.
6. **Do not install services.** Do not register, install or start OS services,
   daemons, LaunchAgents, scheduled tasks or system packages, unless the owner
   authorizes it for that package. Install, upgrade and uninstall may only add
   or remove WGAL's own service, LaunchAgent, GUI and data. The one exception
   (owner decision of 2026-11-12): `packaging/linux/install.sh` may install,
   with `apt-get`, the Ubuntu packages the window needs at run time and the
   system lacks, from the fixed list `libwebkit2gtk-4.1-0` and
   `libayatana-appindicator3-1`, and only when the user agrees (asked in a
   terminal, by `setup.sh`, or `--install-runtime`). It never removes them:
   other programs may use them.
7. **Do not push.** Do not push, add or change remotes, open PRs or publish
   packages without explicit owner authorization. Local commits are fine.
8. **No generic execution.** Never add a code path that runs a caller-supplied
   command, executable path or shell string, reads or writes a caller-supplied
   file path, or acts on a caller-supplied OS service or interface name.
   Subprocesses use a fixed executable and a fixed argument vector, never a
   shell, with capped output and a deadline. Every parameter (tunnel ID, UUID,
   unit, service name, IPv4 address, port) is validated first, and display
   names never reach a command. Settings and diagnostics paths must resist
   symlinks and path traversal.
9. **The GUI holds no power.** The Tauri webview loads only packaged content,
   under a CSP, with least-privilege capabilities. Each command is declared in
   `apps/desktop/src-tauri/build.rs`, granted in `capabilities/`, and still
   validates its input. The GUI renders watchdog state and sends operations
   from the closed `Request` enum. It never controls tunnels or sends
   notifications itself. Since the owner's request of 2026-11-12 it may,
   after the user confirms, add the `wgal` entry (the `wgal-mcp` beside it)
   to the user-level MCP settings of a closed set of AI tools
   (`apps/desktop/src-tauri/src/mcpclients.rs`): the webview names only the
   tool, the shell picks the fixed file, edits only that entry in place,
   refuses links, hard links and files it cannot parse, keeps no copy of
   the file (it may hold other servers' secrets), and never runs the
   tools. The watchdog verifies every IPC peer's OS identity
   and its own authorization rules; capabilities do not replace that.
10. **Diagnostics are redacted.** Exports mask IP addresses,
    UUIDs, account paths, public keys, endpoints and chat IDs, remove secrets
    entirely, and never include whole WireGuard configs, `wg` dumps, the full
    environment, credential stores or whole system logs.

## Reporting results

- Report each check as PASS, FAIL, NOT RUN or BLOCKED. NOT RUN and BLOCKED are
  not passes. For each, record the platform and version, commit SHA, the
  identity it ran as, steps, output, time and side effects. Raw output
  from a real computer holds real names and addresses: keep it on that
  computer, out of this repository.
- A result from fakes or simulation is never reported as a live or on-device
  result, and a Linux cross-compile never stands in for macOS or Windows
  acceptance. Say which one it was.
- Installer status is one of "built", "installed on a device" or "accepted on
  a device". Do not merge them.
- Each work-package report lists: scope, changed files, commit SHA, commands
  run with their results, what was not tested, risks, and the next
  dependency. "Done" or a lit-up screen is not evidence.
- Review findings are fixed by the package's original owner, and the affected
  tests are run again. A finding is never listed as open while the check is
  still marked PASS. A review has not passed until the reviewer has reviewed
  the fixing commit.

## Repository layout

Crate names use the `wgal-` prefix.

| Path | Contents |
|---|---|
| `apps/desktop/` | Vite + TypeScript UI; `src/protocol/generated/` is ts-rs output |
| `apps/desktop/src-tauri/` | Tauri 2 shell, crate `wgal-desktop` |
| `crates/wgal-protocol/` | Shared contract: Rust types, codec, fixtures, TS bindings |
| `crates/wgal-core/` | OS-independent core: `Clock`, `PlatformAdapter`, `TunnelBinding`; the state machine |
| `crates/wgal-platform/` | `exec` (bounded tool runner) and the `windows`, `macos`, `linux` adapters |
| `crates/wgal-runtime/` | The watchdog: the loop, IPC servers, storage, the event log, notifications and service entries |
| `crates/wgal-testkit/` | `FakeClock`, `FakePlatform`; dev-dependency only |
| `crates/wgal-mcp/` | `wgal-mcp`, the agents' MCP server |
| `crates/wgal-client/` | The way to the watchdog that `wgal-mcp` and `wgal` share |
| `crates/wgal-cli/` | `wgal`, the English command line |
| `docs/` | The English guides: architecture, testing, languages, and the guide for agents |
| `.github/workflows/` | `ci.yml`, the CI baseline (Linux, Windows, macOS), on pushes to `main`, pull requests and before a release; `release.yml`, the release build from a version tag |

Dependencies point one way: `wgal-desktop` uses `wgal-protocol`, and of
`wgal-platform` only these functions: on Windows
`windows::connect_watchdog_pipe` (with `is_not_the_watchdog` for its error),
which opens the watchdog's pipe only if LocalSystem owns it, on Linux and
macOS `linux::socket::connect_watchdog_socket` and
`macos::socket::connect_watchdog_socket` (with
`socket::is_not_the_watchdog`), which connect only if the watchdog's user
(root, or the signed-in user) owns the socket and its folder and is the
kernel's peer, and on each OS
its `final_path_of_file` (`windows::`, `linux::`, `macos::`), with which the
log export and the MCP client setup check, from the handle, where the file
they hold open really is, and on Windows `windows::link_count`, with which
that setup refuses a file with more than one name;
`wgal-runtime` uses core, platform and protocol; `wgal-platform` uses core
and protocol; `wgal-core` uses protocol; `wgal-client`, the way to the
watchdog that `wgal-mcp` and the `wgal` command line share (owner
decision of 2026-11-18), uses protocol and, of `wgal-platform`, only the
same connect functions as the window (with their `is_not_the_watchdog`)
and, for where the logs are, `windows::program_data_dir` on Windows and
`macos::socket::{home_dir, data_dir}` on macOS; `wgal-mcp` uses protocol
and `wgal-client`; `wgal-cli` (the `wgal` program) uses protocol,
`wgal-client` and, of `wgal-platform`, only `terminal::HiddenInput`, which
turns the terminal's echo off while the Bot Token is typed. Never make the
desktop crate, `wgal-client`, `wgal-mcp` or `wgal-cli` depend on runtime
code or call adapter, control or service code from them: like the window,
the MCP server and the command line only send `Request`s (the MCP
server's marked `origin: mcp`, the command line's with no origin, as the
window's), and take no command, executable path, file path or service name
from the agent or the user (rule 8); the command line reads a settings
file only from standard input and writes one only to standard output. The
window never runs `wgal-mcp`: it reads the version mark the executable
carries (`wgal_protocol::MCP_VERSION_MARK`, read with
`wgal_protocol::mcp_version_in`) from the `wgal-mcp` file beside its own
binary, a fixed path, not followed through a link, the read capped.

Installing is a choice from a list, not typed names (owner request of
2026-11-12): `install.ps1 -ListTunnels` and `install.sh --list-tunnels`
print the machine's tunnels as JSON without administrator rights and
change nothing, for an agent to let the user pick before the one elevated
run that installs; `setup.cmd` (Windows, a Windows PowerShell 5.1 window
with check boxes) and `setup.sh` (Ubuntu, a whiptail check list) ask for
administrator rights once and run the same install script with the ticked
tunnels. macOS asks for no tunnels.

Every release carries `wgal` (owner decision of 2026-11-18), installed and
upgraded with the other programs as one set: `C:\Program Files\WGAL\wgal.exe`
on Windows and `Contents/MacOS/wgal` inside the app on macOS, run by their
full paths (no script changes the PATH), and `/opt/wgal/wgal` on Ubuntu,
where `install.sh` also links `/usr/local/bin/wgal` to it, only where
nothing is there or the link is already WGAL's, and `uninstall.sh`
removes only that link. `install.sh --no-window` is for an Ubuntu computer
without a desktop: it checks and installs none of the window's packages,
installs no menu or autostart entry and removes WGAL's own; `setup.sh`
asks for it when there is no graphical desktop.

`packaging/{windows,macos,linux}/` holds each platform's scripts, tests and
guides, and `tests/fixtures/` anonymized platform output. `docs/` holds
`architecture.md`, `testing.md`, `languages.md` and `agents.md`;
`CHANGELOG.md` and `SECURITY.md` are at the root.

## Toolchain

All versions are pinned. Do not float to `latest`.

| Tool | Pin | Where |
|---|---|---|
| Rust | 1.99.1 (+ rustfmt, clippy) | `rust-toolchain.toml` |
| Node.js | 24.16.0 (`.node-version`, what CI uses); 24.16.0 to 26.4.0 accepted (owner decision of 2026-11-13: the first native macOS checks and test build ran with 26.4.0) | `.node-version`, `package.json` engines |
| pnpm | 11.10.0 | `package.json` `packageManager` |
| Tauri | crate 2.12.4, tauri-build 2.7.3, `@tauri-apps/api` 2.12.4, `@tauri-apps/cli` 2.12.3 | `Cargo.toml`, `apps/desktop/package.json` |
| Vite / Vitest / TypeScript | 8.3.4 / 5.0.4 / 7.0.2 | `apps/desktop/package.json` |

Commit `Cargo.lock` and `pnpm-lock.yaml` with any dependency change.
`pnpm-workspace.yaml` refuses npm releases younger than 7 days.

### Versions

Since 4.2.1 the window, the watchdog, `wgal-mcp` and `wgal` share one
version (owner decision of 2026-11-19: "全部共用一個版號，反正我們也不可能獨立安裝，要裝也是整套，就把多餘的其他版號都移除"). It is
written in one place, the `[workspace.package]` version in `Cargo.toml`:
every crate inherits it, the GUI shell's crate included
(`version.workspace = true`; its `app_version()` is `CARGO_PKG_VERSION`),
`apps/desktop/src-tauri/tauri.conf.json` has no `version` (Tauri then takes
the crate's, the macOS bundle's `Info.plist` too) and neither has
`apps/desktop/package.json`. So `wgal-watchdog --version`, `wgal
--version`, `wgal-mcp`'s version mark, the snapshot's
`host.watchdog_version`, the window's own version, every release file's
name (`wgal-<version>-<platform>-<arch>`) and its `RELEASE.txt` give the
same number. The 關於 tab still shows the window's and the watchdog's
(with the `wgal-mcp` installed beside the window, as its executable's
version mark says; owner request of 2026-11-04) side by side: they differ
only in an incomplete installation, which is what it should show.

Before that, the parts had versions of their own: the GUI and the
watchdog from 1.0.0 (owner decisions of 2026-11-03), every part again at
2.0.0 before WGAL had users (2026-11-06) and at 3.0.0 with the command line
(2026-11-18); then the watchdog, `wgal-mcp` and `wgal` 4.0.0 (`wgal`'s
shared language), 4.1.0 (the host name, the time zone read before every
message) and 4.2.0 (bound probes proven on an Ubuntu device), with the GUI
at 3.0.1, 3.1.0 and 3.1.1. At 4.2.1 everything was synced (owner decision
of 2026-11-19), and every minimum below starts there. 4.2.1 is also the
first version published (owner decision of 2026-11-19); the earlier ones
were not.

Raise the version for every release handed to the owner (with
`cargo update -w`): the patch number for fixes and wording, the minor number
for new features or changed behaviour, the major number for changes an
older installation cannot take (for example stored state the previous
version rejects). When only one platform needs a change, raise the version
and build and release only that platform; the others stay at their last
release and skip the number, and the release notes say which platforms a
version was released for and which did not change (owner decision of
2026-11-19). A change to an install script alone is a new version too; no
packaging revision such as `-2` is used. `CHANGELOG.md` records each
version and the platforms it was released for. Each release archive's
`RELEASE.txt` gives the commit it was built from and the SHA-256 of every
file in it, and the Releases page gives the SHA-256 of each archive. The
integration owner (A) raises the version in one `WGW-001-A:` commit that
changes `Cargo.toml` and `Cargo.lock` and nothing else;
`apps/desktop/src/protocol/version.test.ts` checks that the version is
written nowhere else.

Compatibility is a minimum, never an exact match, and a minimum is what
makes every function work, not what can merely connect (owner decision of
2026-11-04). Each side states two minimums for the other:

- `MIN_GUI_VERSION` in `crates/wgal-protocol/src/settings.rs` (the oldest
  GUI with every function of this watchdog, sent in every snapshot as
  `host.min_gui_version`) and `MIN_USABLE_GUI_VERSION` (the oldest GUI
  that can still use its tunnel functions, `host.min_usable_gui_version`);
- `MIN_WATCHDOG_VERSION` and `MIN_USABLE_WATCHDOG_VERSION` in
  `apps/desktop/src/protocol/index.ts`, the same for the watchdog this GUI
  needs.

The GUI warns only when one side is below the other's minimums. Below the
full one the 關於 tab says 「版本過舊-無法支援完整功能，建議升級」 after that
side's version; below the usable one, 「版本不支援-請升級」. Raise the full
minimum with every change the other side needs to show or operate all of
it (a new feature, request or field); raise the usable one only when the
older side can no longer use the tunnels at all. Wording, a new language
or a fix never needs either, nor does a release for one platform only. At
4.2.1 all four minimums were set to 4.2.1 (the one-time sync above), so
a side below 4.2.1 is 「版本不支援-請升級」 until a later release raises a
full minimum alone. `apps/desktop/src/protocol/version.test.ts` checks
that every minimum is at most the version.
`PROTOCOL_VERSION` is separate: it guards the wire format and follows
"Changing the shared contract".

Platform prerequisites: Windows needs MSVC build tools and WebView2. Ubuntu
needs `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
libxdo-dev libssl-dev`. macOS needs the Xcode command line tools.

## Commands

Run these from the repository root. They are the same checks CI runs.

```sh
pnpm install --frozen-lockfile
pnpm run check                      # tsc, vitest, vite build
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked     # also regenerates TS bindings
```

To run the desktop shell locally, use `pnpm tauri dev`. It opens a window and
monitors nothing.

Release zips are built on each platform itself
(`packaging/windows/make-release.ps1`, `packaging/linux/make-release.sh`,
`packaging/macos/make-release.sh`); they write only `target/` and `dist/`
and install nothing. An agent working on this repository for the owner
builds a release only when the owner asks. The archives on the Releases
page are built with the same scripts by `.github/workflows/release.yml`
(from 4.3.1): pushing the tag `v<version>` (the workspace version) on a
commit of `main` runs `ci.yml` on that commit, builds the three archives
on GitHub's runners, checks that each `RELEASE.txt` names that version and
commit, and publishes them with their SHA-256 and a `SHA256SUMS` file; a
manual run of the workflow does everything but publish. Pushing a version
tag publishes a release: do it only when the owner asks, after `ci.yml`
has passed on that commit, and never on a commit whose message has
`[skip ci]` (GitHub then runs nothing for the tag). The workflow releases
all three platforms; a version for one platform only is built by hand on
that platform and not tagged. If its `publish` job fails after it made the
draft release, the draft stays, seen only by the repository's writers:
check that its notes name this run and commit, then, with the owner's
approval, delete the draft (never a published release, whose files are
never replaced) and run the failed jobs again. Building one from source on the
computer it will be installed on, as the README says, needs nobody's
request. `packaging/INSTALL.md` (English) and
`packaging/INSTALL.zh-TW.md` are the install and removal guide every zip
carries, written for an agent to follow, and each platform's `README.md`
and `README.zh-TW.md` its details (since 2026-11-19). The two languages say the
same: a change to one is made to the other in the same commit.

On a machine without macOS or Linux, the adapters are type-checked for those
targets with the commands below. This is a cross-check only: report it as
such, never as a native build, test run or on-device result.
`--no-default-features` leaves out the runtime's `telegram` feature (the
HTTPS client), whose TLS stack compiles C code and needs a C cross compiler;
native CI builds it with default features.

```sh
rustup target add x86_64-unknown-linux-gnu aarch64-apple-darwin   # once
cargo clippy --locked --all-targets --no-default-features --target x86_64-unknown-linux-gnu \
  -p wgal-protocol -p wgal-core -p wgal-platform -p wgal-runtime -p wgal-testkit -p wgal-mcp \
  -p wgal-client -p wgal-cli -- -D warnings
cargo clippy --locked --all-targets --no-default-features --target aarch64-apple-darwin \
  -p wgal-protocol -p wgal-core -p wgal-platform -p wgal-runtime -p wgal-testkit -p wgal-mcp \
  -p wgal-client -p wgal-cli -- -D warnings
```

`wgal-desktop` cannot be cross-checked this way: its Linux and macOS
dependencies build C code (GTK and D-Bus, Objective-C helpers). Its Linux
and macOS paths are checked by native CI or on the device; until then
report them as NOT RUN.

## Changing the shared contract

1. Edit the Rust types in `crates/wgal-protocol/src/`. Rust is the source of
   truth. Document exported types at the type level, not on struct fields:
   ts-rs leaves trailing whitespace before a documented field, and a test
   rejects it.
2. Requests are strict: a type a request carries refuses fields it does
   not have (`deny_unknown_fields`), except the extra fields beside a
   unit variant's tag, which are ignored (P3-10, closed by the owner's
   decision of 2026-11-04 for compatibility). Answers are read leniently,
   so a GUI can read a newer watchdog's answers and say it is too old:
   types only in answers carry no `deny_unknown_fields`, and a field added
   to them is `#[serde(default)]` (an `Option` or a value with a safe
   default) so an older watchdog's answers still decode;
   `fixtures/compat/` keeps the 1.1.0 answers that prove it. A new variant
   of an enum in an answer still needs a GUI that knows it: raise the full
   minimum.
3. Keep every collection a `BoundedVec` and every free-text field a bounded
   text type (`DisplayName`, `Detail`, `DeviceName`). Messages are encoded and decoded only
   through `wgal_protocol::codec`, which enforces the 64 KiB limit.
   `tests/limits.rs` builds the largest valid messages; extend it with any new
   field. The largest snapshot is currently 55,843 of 65,536 bytes, the
   largest event log page 35,065, the largest template request 18,775, the
   largest settings import 15,254, its largest result 8,640 and the
   largest error response 724 (`cargo test -p wgal-protocol --test limits
   largest -- --nocapture` prints them).
4. Run `cargo test -p wgal-protocol`. This rewrites
   `apps/desktop/src/protocol/generated/`.
5. If the wire format changed on purpose, run
   `WGAL_UPDATE_FIXTURES=1 cargo test -p wgal-protocol`. Then update the typed
   samples in `apps/desktop/src/protocol/contract.test.ts`; every fixture
   needs one. The limits the GUI uses (`MAX_TUNNELS`, the `ProbeTiming`
   ranges and so on) are written to `fixtures/limits.json` and mirrored as
   constants in `apps/desktop/src/protocol/index.ts`, which the TypeScript
   test checks against it.
6. `PROTOCOL_VERSION` stays 1 until a peer is released. After that, bump it in
   Rust and in `apps/desktop/src/protocol/index.ts` for any change an older
   peer would misread.
7. Commit the generated files and fixtures. CI fails if they drift.

### Contract status

Version 1 covers the IPC envelopes and codec, validated IDs, text and probe
targets, the request allowlist for snapshot, discovery, registration, monitor
settings, connect, disconnect, probe, reconnect, auto-recovery and the
"start when stopped externally" option, the layered tunnel
state with light reason, start time, deadline and next retry, takeover
state, the incident with its first-recovery outcome and notification state,
storage health, recovery and observe targets,
adapter error kinds and three-state capabilities, and in `wgal-core`
the `PlatformAdapter` trait, which addresses tunnels by `TunnelBinding`, the
manager's own name for them. It also has the host-wide monitoring pause,
per-tunnel `auto_recovery_allowed` and `last_verified_unix_ms`, and for
notifications the write-only `BotToken`, `ChatId` kept as a
string, the settings, set/clear token and test-notification requests, and
`NotificationStatus` in the snapshot, which never carries the token. For
the transport it has the frame format (`codec::write_frame`/`read_frame`: a
4-byte little-endian length, then one encoded message, checked against the
limit before the body is read) and the fixed Windows pipe name
`WINDOWS_PIPE_NAME`.

Wave three (owner decisions of 2026-11-03, which replace a fixed fault
confirmation of three failed rounds over 60 s) added per-tunnel probe timing (`ProbeTiming`: interval
10–3600 s, timeout 1–10 s, retries 0–10, minimum fault time 0–3600 s; in
`MonitorSettings`, which no longer carries the monitoring switch, and in
`TunnelStatus` with `freshness_secs` and `consecutive_failures`), the
per-tunnel monitoring pause (`SetTunnelMonitoring`) and notification switch
(`SetTunnelNotifications`, `notifications_enabled`), unregistering a tunnel
(`UnregisterTunnel`, which only forgets WGAL's registration), host settings
with the worker thread count (`WorkerCount`, 2–17) and time zone
(`TimeZoneName`) in `UpdateHostSettings`, `HostStatus` in the snapshot
(with the watchdog's `ProductVersion`), the `invalid_setting` error, and
the event log page (`ReadEventLog` with an `EventFilter` for every line,
one tunnel's, the watchdog's own start and stop lines, or, from watchdog
1.1.0 (owner request of 2026-11-04), every line about no tunnel
(`system`);
`Response::EventLog` with `EventEntry` and `LogEvent`, at most
`MAX_EVENT_PAGE` entries; `removed_tunnel` marks lines about a tunnel that
is no longer registered).

For language files (owner requests of 2026-11-03) it added notification
message templates (`NotificationTemplates`: `formal`, `result_failed`,
`result_not_executed`, `test` and `unnamed_device`, each a
`MessageTemplate` of at most 1,024 bytes that must use each placeholder of
its fixed set exactly once and no other, so a language file cannot drop what
a message must carry, and a `TemplatesId` naming the
set),
which the GUI hands over with `SetNotificationTemplates` and the snapshot
names in `NotificationStatus::templates_id` (`null` for the watchdog's
built-in set; `fixtures/request_set_notification_templates.json` holds the
built-in Traditional Chinese texts), and `ErrorReason`, a machine reason
beside the coarse `ErrorCode` in every `ProtocolError` (`null` when the code
says it all), built with `ProtocolError::new(code, reason, message)`.

Watchdog 1.2.0 (owner decisions of 2026-11-04) added the per-tunnel
recovery retry (`RecoveryRetry`: 0 to 3600 rounds after the first or
`null` to keep trying, one every 10 to 10800 s, default 100 a minute
apart, replacing a fixed backoff of 1, 2 and 5 minutes, then every 15;
`SetRecoveryRetry`,
`TunnelStatus::retry`, `IncidentStatus::retries_used` and
`retries_exhausted`, the `retries_exhausted` light reason and log line),
two notifications beside the one alert (`NoticeKind`: `alert`,
`retries_exhausted` when a limited count is used up, `recovered` when an
incident automatic recovery worked on, and no manual connect ended, has
closed; `IncidentStatus::exhausted_notification`,
`TunnelStatus::recovered_notification`, and `notice` on the notification
log lines), and the templates they need: `formal` gained `{retry}`, filled
with `retry_limited`, `retry_forever` or `retry_none`, and the set gained
`exhausted` and `recovered` (`TemplateTexts`). The snapshot also names the
oldest GUI that can still use the tunnels (`min_usable_gui_version`).
It also added the settings file (`SettingsDocument`, version 1, read
strictly, without the device name, Bot Token or Chat ID):
`ExportSettings` answers `SettingsExport`, and `ImportSettings` with
`ImportOptions` (match by name or ID, register missing tunnels, monitor
new ones, dry run) answers `SettingsImport` with an `ImportResult` per
entry; automatic recovery from a file waits for the baseline
(`TunnelStatus::auto_recovery_pending`).

Watchdog 1.3.0 (owner request of 2026-11-05) added `{time_zone}` to the
`formal`, `exhausted`, `recovered` and `test` templates: a time is written
without its zone (`2026-11-05 00:26:01`), and the message names the zone
and its offset once (`Asia/Taipei, UTC+08:00`, at most
`NotificationTemplates::MAX_TIME_ZONE_BYTES`); a later time in the same
message whose offset differs (daylight saving time) keeps its own offset.
A set without `{time_zone}` is refused, and one stored by an older
watchdog is read as none, so the built-in set is used until the GUI sends
its own again.

Watchdog 1.5.0 (owner decision of 2026-11-06) changes what `ProbeNow`
does for a tunnel that is not monitored, by its own switch or the
host-wide pause: it probes the targets once, and the answers only update
each target's `last_probe`; no streak, fault, light or operation follows,
and monitoring stays off. The wire format is unchanged; an older watchdog
accepts the request and probes nothing, which `wgal-mcp` tells apart by
the snapshot's `watchdog_version`.

Watchdog 2.0.0 (owner decisions of 2026-11-06) adds the switch that lets
agents set Telegram (`SetMcpTelegramEnabled`,
`HostStatus::mcp_telegram_enabled`, kept in the stored state). It is off
by default and only the user turns it on: in the window, or, since the
owner's decision of 2026-11-18, with `wgal mcp telegram on` (as MCP itself
with `wgal mcp on`), which asks for a typed yes in an interactive
terminal. While it is on, an `mcp`
request may change or clear the device name, chat ID and bot token, and
`wgal-mcp` gives the chat ID out; the bot token is never given out. While
it is off, the runtime refuses, with `mcp_telegram_disabled`, an `mcp`
request that changes any of the three or turns the switch on. It is kept,
without effect, while MCP is off. These switches limit only `wgal-mcp`:
`wgal` sends no origin, as the window, and whoever runs it as an account
the access list allows, an agent with a shell too, acts as the user, reads
the chat ID and may change all three (owner decision of 2026-11-18). It also narrows `BotToken` to a bot ID
of 3 to 15 digits and a secret of 30 to 40 characters, and `ChatId` to 1
to 19 digits not starting with 0 or an `@username` of 1 to 37
characters: Telegram's own
lengths with 5 to spare either way (`limits.json`, mirrored in the GUI).
Every minimum, on both sides, is 2.0.0.

Watchdog 2.1.0 (owner decision of 2026-11-06) gives the device name its
own type, `DeviceName`: any text the user likes, of at most 64 characters
(`limits.json`), with no control characters, as in every contract text.
`wgal-mcp` and the GUI take whatever name they are given: control
characters become spaces and a longer name is cut to 61 characters and
`...`; a stored name is read the same way. Names are not otherwise
limited, by the owner's decision: they are only shown, never run, and
are not trimmed. Every minimum, on both sides, is 2.1.0: a 2.0.0 GUI
cannot decode a longer name, and 2.0.0 was never installed.

Watchdog 4.0.0 (owner decisions of 2026-11-19) keeps `wgal`'s shared
language: the language the command line writes in when a run does not
name one with `--lang`, English until it is set. `LanguageTag` is a
language's tag (2 to 16 bytes of ASCII letters, digits and `-`, starting
with a letter, kept and compared as given; only its shape is checked).
`SetCliLanguage` sets it, or clears it with `null` (the field must be
given), and is answered `accepted`; the snapshot reports it as
`NotificationStatus::cli_language` (`null` when never set, and from an
older watchdog). It is kept in `notify.json` with the notification
settings, written only when set: clearing always writes the file again
without it, so a 3.x watchdog, which refuses the key, reads the file
again (`wgal language --clear` before a rollback). Only the user sets it:
an `mcp` request is refused, whatever MCP's switches. `MIN_GUI_VERSION`
is 3.0.1, because `RequestOp` gained `set_cli_language` and appears in
the event log's `agent_request` lines; the other minimums stay 2.1.0.

Watchdog 4.1.0 (owner decisions of 2026-11-19) gives messages the
computer's host name when no device name is set (empty or only white space),
and reports it as `NotificationStatus::host_name`, a `DeviceName` (control
characters become spaces, a longer name is fitted; `null` when it cannot
be read or is blank, and from an older watchdog; never stored). The host
name and the system's time zone are read again before each message is
written, when a snapshot or the event log is asked for, and every ten
minutes. On Linux and macOS a change of `/etc/localtime`, or of a file
`TZ` names, takes effect at once (the file is read each time, at most 256
KiB, past jiff's five-minute cache); on Windows within five minutes
(through jiff; accepted by the owner). A zone read from a file's TZif data
is unnamed: messages give its offset alone. `MIN_GUI_VERSION` is 3.1.0,
`MIN_WATCHDOG_VERSION` 4.1.0.

4.2.1 (owner decision of 2026-11-19), the one version the whole set shares
from then on ("Versions"), sets all four minimums to 4.2.1:
`MIN_GUI_VERSION` and `MIN_USABLE_GUI_VERSION` here (and in `limits.json`),
`MIN_WATCHDOG_VERSION` and `MIN_USABLE_WATCHDOG_VERSION` in the GUI. The
wire format did not change; `PROTOCOL_VERSION` stays 1.

Still to be added, through the integration owner, by the package that needs
them:

- exporting redacted diagnostics: WGW-001-E with WGW-001-F;
- nothing for the caller's identity: it never travels on the wire. The IPC
  server establishes it from the OS and the runtime (E) authorizes each
  request with it; registration and enabling automatic actions are limited
  by the installation's access list.

### Language files

Every sentence the user reads in the window is the GUI's (F), in its language files in
`apps/desktop/src/lang/`, one per language, listed with the system
languages each stands for in `languages.json` there (owner requests of
2026-11-05; its `README.md` says how to add one). Thirteen exist:
Traditional Chinese, English, Simplified Chinese, Japanese, Korean,
German, Spanish, Brazilian Portuguese, Turkish, French, Russian,
Indonesian and Vietnamese. English is the fallback, key by key and when
none of the system's languages has a file; the window follows the
system's language unless the user picks one, and the GUI's tests read its
words in Traditional Chinese (`src/test-setup.ts`). The watchdog
never sends a user-facing sentence: it reports codes, reasons and states,
and writes Telegram messages only from the templates the GUI or `wgal`
gave it, or from its built-in set, which equals the GUI's en file text for
text since English is the default language (owner decision of 2026-11-18;
until 3.0.0 it was the zh-TW text;
`fixtures/request_set_notification_templates.json` is the reference both
are tested against). `wgal notify language <tag>` hands over one of the
13 languages' sets from `crates/wgal-cli/src/languages.json`, which a test
writes from the GUI's language files (`WGAL_UPDATE_FIXTURES=1 cargo test
-p wgal-cli`), so a change to a file's `notification_templates` is
regenerated there; each set is named as the GUI names it, so a window in
the same language sees it as its own. `wgal`'s own words are I's, not
in the GUI's language files: since the owner's decision of 2026-11-19
they are in the 13 files of `crates/wgal-cli/src/lang/`, one per GUI
language with the GUI's tags, `en.json` the source and the fallback, key
by key. A run writes in the language its `--lang` names, otherwise in
the shared language the watchdog keeps (`SetCliLanguage`), otherwise in
English; `wgal language <tag>` sets the shared language and hands over
that language's Telegram set too. The English `message` of a
`ProtocolError` and the `Detail` of a fault are diagnostics, shown only
among the details. A new language needs no contract change; a new reason,
placeholder or template does.
