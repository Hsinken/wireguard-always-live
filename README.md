# WG Always Live

**English** | [繁體中文](README.zh-TW.md)

WG Always Live (WGAL) is a watchdog for the official WireGuard tunnels on
Windows 11, macOS and Ubuntu, with a desktop or on a server. It checks that
traffic really flows through each tunnel and shows the result. Only when
you allow it, it reconnects just the tunnel that failed. When automatic
recovery fails, it can tell you on Telegram.

> **Full MCP support; installing with an AI agent is recommended.**
> The agent does the install and you authorize it. Paste this to your agent:
>
> ```text
> Install WG Always Live for me by following the README at
> https://github.com/Hsinken/wireguard-always-live
> ```
>
> Agents: read [Install with an AI agent](#install-with-an-ai-agent) and
> the [rules for an agent](packaging/INSTALL.md#rules-for-an-agent-read-first)
> before you run anything.

> **Status: pre-release.** 4.2.1 is the first public version. No release
> has been published on the Releases page yet: for now, build the archive
> on the computer it will be installed on. See
> [Platform status](#platform-status).

## What WGAL is for

WGAL was designed to make up for the official WireGuard software's weak
detection of dead connections.

- **The problem**: after running a long time over an unstable link, a
  tunnel can show "connected" while traffic does not get through.
- **The check**: WGAL sends probes through the tunnel's own interface to a
  target you choose. A tunnel is healthy only when that probe answers.
- **The repair**: when you allow it, WGAL reconnects only the tunnel that
  failed. The other tunnels are never touched.

Three things to know first:

- **WGAL is not a VPN.** It does not replace WireGuard. The official
  WireGuard app (or `wg-quick` on Ubuntu) stays your VPN.
- **WireGuard comes first.** Install the official WireGuard and set up its
  tunnels before you install WGAL.
- **WGAL never touches the secret files.** It never reads, changes or
  exports a WireGuard configuration or key.

## Features

- **Health per tunnel**: probes bound to the tunnel's interface, sent to an
  IPv4 *recovery check target* you choose, with a green, yellow, red or
  gray light and the reason for it.
- **Fault judgment you can tune** per tunnel: probe interval, timeout,
  retries and minimum fault time.
- **Automatic recovery, opt-in per tunnel**: only the failed tunnel is
  restarted, with configurable retries.
- **Telegram notifications**: when the first automatic recovery fails,
  when the retries run out, and when traffic is back.
- **Event log** (about 72 hours) and a log export with IP addresses,
  endpoints, UUIDs, account paths and Chat IDs masked, and no Bot Token.
- **Pause** monitoring of every tunnel or of one.
- **Settings file**: export and import, to back up or move to another
  computer.
- **Command line** (`wgal`): everything the window does with the watchdog,
  from a terminal. On an Ubuntu server without a desktop it is the way to
  use WGAL.
- **MCP server** (`wgal-mcp`): AI agents can use WGAL with the same
  permissions as the window. It never gets the Bot Token, and it is off
  until you turn it on.
- **13 languages** for the window, the command line and the Telegram
  messages.
- Tray or menu bar icon.

## What WGAL never does

- Read, edit, export or delete WireGuard configurations or keys.
- Change routes, DNS, firewall rules, MTU, AllowedIPs or On-Demand rules.
- Change whether a tunnel starts at boot.
- Start or stop a tunnel you have not handed to it. Registering a tunnel
  and turning on its automatic recovery are separate, explicit choices.
- Send data anywhere on its own: no telemetry, no update checks. The only
  traffic it starts is the probes you set up, and, once you set
  notifications up, messages to the official Telegram Bot API.

## Install with an AI agent

The agent runs the steps. The human gives each permission.

### What the human does

Each row is a separate permission.

| Step | The human does |
|---|---|
| Install on Windows or Ubuntu | Approves administrator rights once (UAC, or the sudo password), and picks the tunnels from a list. The agent gets that list with a command that needs no rights |
| Install on macOS | Usually nothing: it installs as the user, not with sudo, and asks for no tunnels. Only when the user cannot write to `/Applications` does it ask for an administrator password |
| Telegram notifications (optional) | Types the Bot Token themselves, in the window or with `wgal notify token set` |
| MCP (optional) | Turns MCP on themselves: in the window's MCP tab, or with `wgal mcp on` in their own terminal, typing yes |
| Letting agents set Telegram (optional) | A second switch, turned on the same way (`wgal mcp telegram on`) |

**Installing is not permission to switch a VPN.** The agent connects,
disconnects or reconnects a tunnel, and turns automatic recovery on, only
when the human asks.

### Rules for the agent

The full rulebook is
[Rules for an agent](packaging/INSTALL.md#rules-for-an-agent-read-first).
Read it first. In short:

| Rule | What it means |
|---|---|
| Never work through the VPN you are about to watch | Work on the computer itself, or over a connection that does not depend on that VPN. This always applies |
| Never read or print a secret | Not a WireGuard configuration, a private key or the Bot Token. Never pass a token on a command line. This always applies |
| Change nothing in WireGuard | No setting, key, tunnel or start at boot. Do not connect or disconnect a VPN by hand |
| Install nothing else | No other package, and no change to a firewall, route or DNS |
| Ask, do not guess | Ask which account uses WGAL, which tunnels it may hand to WGAL, and on Ubuntu whether the computer has a desktop |
| Elevate once | List the tunnels without rights first. Then run the install with administrator rights one time |
| Check every exit code | Only 0 is success. On anything else, stop and give the human the script's messages word for word. Do not "fix" things and try again |

### Steps for the agent

1. **Check that WireGuard is ready.** The official WireGuard must be
   installed and must have the tunnels to watch:

   | Platform | Needs |
   |---|---|
   | Windows 11 | WireGuard for Windows. Only an activated tunnel can be registered |
   | macOS | WireGuard from the Mac App Store |
   | Ubuntu | `wireguard-tools`, with each configuration in `/etc/wireguard/<name>.conf` |

   If it is not, stop and tell the human. Do not install or set up
   WireGuard yourself.
2. **Get the archive.** Download the platform's archive from the
   [Releases page](https://github.com/Hsinken/wireguard-always-live/releases)
   and check it against the SHA-256 published there. **No release is
   published yet**: until one is, follow
   [Build the release archive yourself](#build-the-release-archive-yourself).

   | Platform | Archive |
   |---|---|
   | Windows 11 (x64) | `wgal-<version>-windows-x64.zip` |
   | macOS | `wgal-<version>-macos-<arm64 or x86_64>.zip` |
   | Ubuntu | `wgal-<version>-linux-x86_64.tar.gz` |

3. **Unpack it and check the files against `RELEASE.txt`.** `RELEASE.txt`
   gives the version, the commit the archive was built from and the SHA-256
   of every file in it. Use `Get-FileHash` on Windows, `shasum -a 256` on
   macOS and `sha256sum` on Ubuntu. This proves the contents match the
   build record. It does not prove the download is genuine.
4. **Install** as your platform's section below says, and as
   [packaging/INSTALL.md](packaging/INSTALL.md) says in full. The archive
   holds the same guide. Check every exit code.
5. **Check** that the watchdog runs, as the same section says.
6. **Set up MCP if the human wants it.** See
   [Let the agent use WGAL](#let-the-agent-use-wgal-mcp) and
   [docs/agents.md](docs/agents.md).

Before an upgrade or a removal, the WGAL window must be quit, no `wgal`
command may be running, and any agent that uses `wgal-mcp` must be closed.

#### Windows 11

Run everything from the unpacked folder. In PowerShell always keep the
`.\`: a bare `install.cmd` may run another program's script found on the
PATH.

1. List the tunnels from an ordinary PowerShell. It needs no administrator
   rights, prints one line of JSON and changes nothing:

   ```powershell
   .\install.cmd -ListTunnels
   ```

   Only activated tunnels are listed. A tunnel with `"accepted": false`
   cannot be picked. Let the human pick from the list.
2. Install as administrator, for example started with
   `Start-Process -Verb RunAs`. The human approves UAC once:

   ```bat
   .\install.cmd -Tunnel <tunnel name>[,<tunnel name>...]
   ```

   When the elevation uses another administrator account, add
   `-User <SID of the account that uses WGAL>`. On an upgrade, leave
   `-Tunnel` out to keep the access list as it is.
3. Check, from an ordinary (not elevated) PowerShell as the account that
   uses WGAL:

   ```powershell
   sc.exe query WGALWatchdog
   & 'C:\Program Files\WGAL\wgal.exe' --lang en status
   ```

   The service shows `RUNNING`, and the first line of the status starts
   with `WGAL watchdog <version>`.

Details: [packaging/windows/README.md](packaging/windows/README.md).

#### Ubuntu

1. Ask the human whether the computer has a desktop.
2. List the tunnels. It prints JSON and changes nothing:

   ```bash
   ./install.sh --list-tunnels
   ```

   An ordinary user cannot read `/etc/wireguard`, so this lists only the
   tunnels systemd knows and the running ones (`"complete": false`). For
   the full list, run `sudo ./install.sh --list-tunnels` in the same sudo
   session as the install that follows. Let the human pick from the list.
3. Install. The human enters the sudo password.

   With a desktop:

   ```bash
   sudo ./install.sh --tunnel <interface>[,<interface>...]
   ```

   Without a desktop (a server). Give `--no-window` again on every upgrade:

   ```bash
   sudo ./install.sh --no-window --tunnel <interface>[,<interface>...]
   ```

   The account that uses WGAL is the one that ran sudo. Add
   `--user <account>` for another one. It cannot be root.

   With a desktop, the window needs the Ubuntu packages
   `libwebkit2gtk-4.1-0` and `libayatana-appindicator3-1`. When they are
   missing, ask the human, and add `--install-runtime` only when the human
   agrees. Without it, the script stops before it changes anything.
4. Check, as the account that uses WGAL, not with sudo:

   ```bash
   systemctl is-active wgal-watchdog
   wgal --lang en status
   ```

   The service shows `active`, and the first line of the status starts
   with `WGAL watchdog <version>`. If `/usr/local/bin/wgal` could not be
   linked, use `/opt/wgal/wgal`.

Details: [packaging/linux/README.md](packaging/linux/README.md).

#### macOS

1. Install as the user, **not with sudo**. It asks for no tunnels:

   ```bash
   ./install.sh
   ```

   The script copies the app into `/Applications`. Only when it cannot
   write there does it ask for an administrator password.
2. Check, as the user:

   ```bash
   "/Applications/WG Always Live.app/Contents/MacOS/wgal" --lang en status
   ```

   The first line starts with `WGAL watchdog <version>`.

Read [Platform status](#platform-status) first: on macOS, WGAL cannot yet
show a green light or turn automatic recovery on.

Details: [packaging/macos/README.md](packaging/macos/README.md).

### After the install

The human sets WGAL up in the window, or with `wgal`. An agent runs these
only when the human asks. Run `wgal` as the account that uses WGAL, not
elevated and not with sudo.

| Platform | Run `wgal` as |
|---|---|
| Windows | `& 'C:\Program Files\WGAL\wgal.exe' <command>` in PowerShell |
| Ubuntu | `wgal <command>`, or `/opt/wgal/wgal <command>` |
| macOS | `"/Applications/WG Always Live.app/Contents/MacOS/wgal" <command>` |

| Command | What it does |
|---|---|
| `wgal discover` | Lists the tunnels WGAL can watch, with the ID WGAL gives each |
| `wgal register <tunnel>` | Starts watching a tunnel |
| `wgal set <tunnel> --recovery-target <address>` | Sets the target the probes must reach through that tunnel. Use an IPv4 address that answers only through the tunnel, such as a host on the other side (`192.0.2.1` is a placeholder) |
| `wgal probe <tunnel>` | Probes the targets now |
| `wgal auto-recovery <tunnel> on` | Lets WGAL restart the tunnel when traffic stops. Only when the human asks |
| `wgal status` | Shows the watchdog and every tunnel |
| `wgal help` | Lists every command |

`wgal` writes in the language the human chose. Add `--lang en` when you
need English output, and `--json` for one JSON object. Do not change the
human's language, and do not send a test message, unless they ask.

### Let the agent use WGAL (MCP)

1. **The human turns MCP on.** In the window's MCP tab, or with
   `wgal mcp on` in their own terminal, typing yes. An agent must never
   do this, or answer that prompt, for the human. (The command also
   refuses when it is not run in an interactive terminal; that is a
   confirmation step, not proof that a person typed it.)
2. **Add `wgal-mcp` to the agent's MCP settings.** The window's MCP tab
   can add it for the AI tools it supports, after the human confirms.
   Otherwise add a stdio server that runs `wgal-mcp` by its full path:

   | Platform | `wgal-mcp` |
   |---|---|
   | Windows | `C:\Program Files\WGAL\wgal-mcp.exe` |
   | Ubuntu | `/opt/wgal/wgal-mcp` |
   | macOS | `/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp` |

3. **Know the limits.** Through MCP an agent can do what the human can do
   in the window, within this installation's access list. It never gets
   the Bot Token. It cannot change the Telegram settings unless the human
   turns the second switch on.

Turning MCP on is not permission to switch a tunnel either. The tools and
their limits are in [docs/agents.md](docs/agents.md).

## Install it yourself

Releases are archives (`.zip`, `.tar.gz`) installed and removed by scripts.
There is no installer, and the programs are not code-signed yet. Windows
may show a SmartScreen warning. On macOS the install script removes the
download quarantine flag from this app only.

Install the official WireGuard and add your tunnels first. Then, from the
unpacked folder:

| Platform | The easy way | Or |
|---|---|---|
| Windows 11 | Double-click `setup.cmd`. It asks for administrator rights once and lets you tick the tunnels | `.\install.cmd -Tunnel <tunnel name>[,...]` as administrator |
| Ubuntu | `./setup.sh` (without sudo). It asks for the sudo password once and lets you tick the tunnels | `sudo ./install.sh --tunnel <interface>[,...]` |
| Ubuntu without a desktop | `./setup.sh`, and answer yes to installing without the window | `sudo ./install.sh --no-window --tunnel <interface>[,...]` |
| macOS | `./install.sh` as yourself, not with sudo | |

To remove WGAL:

| Platform | Command |
|---|---|
| Windows 11 | `.\uninstall.cmd` as administrator, from the unpacked folder |
| Ubuntu | `sudo /opt/wgal/uninstall.sh` |
| macOS | `"$HOME/Library/Application Support/WGAL/uninstall.sh"` (with the quotes, not with sudo) |

Your WGAL data is kept unless you ask for it to be removed. None of the
scripts changes a WireGuard tunnel or configuration. The full guide is
[packaging/INSTALL.md](packaging/INSTALL.md).

## Platform status

As of 2026-11-20.

- **One version**: 4.2.1 is the first public version. It is also the first
  in which the window, the watchdog, `wgal-mcp` and `wgal` share one version
  number. Earlier versions were not published.
- **No release yet**: nothing has been published on the Releases page.
  Build the archive on the computer it will be installed on, then install
  from it.

| Platform | Status |
|---|---|
| Windows 11 | The window 2.2.1 and the watchdog 2.3.1, from before the versions were unified, are in use on real tunnels on the developer's computer. 4.2.1 is not released for Windows yet |
| Ubuntu without a desktop (`--no-window`) | 4.2.1 is installed on a test server. That build was made before the source moved to this repository. See the table below |
| Ubuntu with a desktop | 2.2.1 was installed on a test computer with a desktop (GNOME). 4.2.1 is not installed on a desktop yet. The server's results do not stand for it |
| macOS | A native build passes all tests on a Mac (macOS 27, Apple silicon). A 2.2.1 test build was installed, upgraded with a failure injected on purpose, and removed there. 4.2.1 was installed on that Mac and could not list the tunnels: macOS 27.2.1 writes the service type with a colon, which its parser refused. 4.2.2 (macOS only) reads it: installed on that Mac, its watchdog lists the tunnels and reads a registered tunnel's state. Probes and controlling the official app's VPN are not tested yet |

On the Ubuntu test server, with 4.2.1:

| Proven | Not proven |
|---|---|
| Probes bound to the tunnel's interface (ICMP; TCP probes are not supported on Ubuntu) | A real fault that automatic recovery fails to fix. Turning automatic recovery on is not proof that it repairs a fault |
| A verified disconnect, connect and reconnect | Two tunnels with overlapping routes |
| Turning automatic recovery on | |
| The settings kept across a restart of the watchdog and across a reboot | |

**macOS limit**: until a probe bound to the tunnel's interface is proven on
a Mac, the macOS version never shows a green light and cannot turn
automatic recovery on. The capability is unknown, not unsupported.

What each platform has proven, and how, is in
[docs/testing.md](docs/testing.md). A result from tests with fakes is never
reported as one from a real computer.

## How it works

```text
 the window (your account, no privileges) ──┐
 wgal (the command line, your account) ─────┤
 wgal-mcp (AI agents; off by default) ──────┤
                                            │ local named pipe / Unix socket
                                            ▼
                                     wgal-watchdog (background service)
                                     probes · recovery · event log · Telegram
                                            │
                                            ▼
                              the official WireGuard manager
```

| Platform | The watchdog runs as | It controls tunnels through | Local connection |
|---|---|---|---|
| Windows 11 | a Windows service (LocalSystem) | the `WireGuardTunnel$<name>` services | a named pipe with an access control list |
| macOS | your own LaunchAgent | the official app's VPN configurations, with `scutil --nc` | a Unix socket in `~/Library/Application Support/WGAL` |
| Ubuntu | a systemd service (root) | `wg-quick@<interface>` units | a Unix socket at `/run/wgal/watchdog.sock` |

The watchdog checks every client's operating-system identity before it
answers. On Windows and Ubuntu, an access list written at install time
says which accounts may use it and which tunnels they may register.

More: [docs/architecture.md](docs/architecture.md).

## Build from source

Toolchain (pinned): Rust 1.99.1, Node.js 24.16.0, pnpm 11.10.0.

| Platform | Also needs |
|---|---|
| Windows | The MSVC build tools and WebView2 |
| Ubuntu | `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev` |
| macOS | The Xcode Command Line Tools |

The checks CI runs, from the repository root:

```bash
pnpm install --frozen-lockfile
pnpm run check                      # TypeScript, tests, build
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`pnpm tauri dev` runs the window locally. Without a watchdog it monitors
nothing.

### Build the release archive yourself

Each archive is built on its own platform, on the computer it will be
installed on. The build writes only `target/` and `dist/` and installs
nothing.

Agents: the build tools above are not part of WGAL. If any is missing,
tell the human and ask before you install it.

1. Get the source. The checkout must have no uncommitted changes:

   ```bash
   git clone https://github.com/Hsinken/wireguard-always-live.git
   cd wireguard-always-live
   pnpm install --frozen-lockfile
   ```

2. Build, from the repository root:

   | Platform | Command | Result |
   |---|---|---|
   | Windows | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\make-release.ps1` | `dist\wgal-<version>-windows-x64.zip` |
   | Ubuntu | `packaging/linux/make-release.sh` | `dist/wgal-<version>-linux-<arch>.tar.gz` |
   | macOS | `packaging/macos/make-release.sh` | `dist/wgal-<version>-macos-<arch>.zip` |

3. Unpack the archive from `dist/` into a local folder, and install from
   it as [Steps for the agent](#steps-for-the-agent) says, from step 3.

## Repository layout

| Path | Contents |
|---|---|
| `apps/desktop/` | The window: a Vite + TypeScript interface, its Tauri 2 shell (`src-tauri/`) and its language files (`src/lang/`) |
| `crates/wgal-protocol/` | The shared contract: messages, limits and the TypeScript bindings |
| `crates/wgal-core/` | The operating-system-independent state machine |
| `crates/wgal-platform/` | The Windows, macOS and Ubuntu adapters |
| `crates/wgal-runtime/` | The watchdog: service entry points, local servers, storage and notifications |
| `crates/wgal-client/` | The way to the watchdog that the command line and the MCP server share |
| `crates/wgal-cli/` | The command line, `wgal` |
| `crates/wgal-mcp/` | The MCP server |
| `crates/wgal-testkit/` | The fakes the tests run against |
| `packaging/` | Install and uninstall scripts and release builders, per platform |
| `docs/` | Architecture, testing, languages and notes for agents |

## Documentation

| File | For |
|---|---|
| [packaging/INSTALL.md](packaging/INSTALL.md) | Installing, upgrading, checking and removing WGAL, for people and agents |
| [packaging/windows/README.md](packaging/windows/README.md), [packaging/linux/README.md](packaging/linux/README.md), [packaging/macos/README.md](packaging/macos/README.md) | Each platform's details |
| [docs/agents.md](docs/agents.md) | AI agents that install WGAL or use it through MCP or `wgal` |
| [docs/architecture.md](docs/architecture.md) | How WGAL is built |
| [docs/testing.md](docs/testing.md) | How WGAL is tested and what each platform has proven |
| [docs/languages.md](docs/languages.md) | Adding a language |
| [AGENTS.md](AGENTS.md) | The rules for changing the code |
| [CHANGELOG.md](CHANGELOG.md) | What each version changed, and for which platforms |
| [SECURITY.md](SECURITY.md) | Reporting a vulnerability |

## Languages

繁體中文, English, 简体中文, 日本語, 한국어, Deutsch, Español,
Português (Brasil), Türkçe, Français, Русский, Bahasa Indonesia and
Tiếng Việt. Translations other than Traditional Chinese and English are
first drafts.

| Part | Language |
|---|---|
| The window | Follows the system language, unless you pick one |
| `wgal` | English until you choose: `wgal language <tag>` for `wgal` and the messages, `wgal --lang <tag>` for one command |
| Telegram messages | English until you choose: `wgal language <tag>`, `wgal notify language <tag>` for the messages only, or the window's language |

Adding a language takes no code change in the window, and one line in the
command line's list of files: see [docs/languages.md](docs/languages.md).

## Contributing

The development rules, including the safety boundaries every change must
keep, are in [AGENTS.md](AGENTS.md). The tests that run by default, and in
CI, never touch a real VPN: they run against fakes and fixtures. The few
on-device checks are ignored unless the owner authorizes them for a run
([docs/testing.md](docs/testing.md)). Pull requests run CI
(`.github/workflows/ci.yml`) on Linux, Windows and macOS.

To report a vulnerability, see [SECURITY.md](SECURITY.md). Never put a
WireGuard configuration, a key or a Bot Token in an issue.

## License

[MIT](LICENSE).

WireGuard is a registered trademark of Jason A. Donenfeld. WG Always Live is
an independent project; it is not affiliated with or endorsed by WireGuard
or its authors.
