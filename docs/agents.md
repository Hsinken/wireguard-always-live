# For AI agents

How an AI agent installs WG Always Live for a person, and then uses it
through MCP or the command line. To change WGAL's own code, read
[AGENTS.md](../AGENTS.md) instead.

WGAL watches tunnels that already exist in the official WireGuard software.
It never reads or changes a WireGuard configuration or key. Probes through
a tunnel tell whether traffic really flows.

## Who does what

The agent does the work. The person makes every decision that grants a
right, each one separately:

| Step | The agent | The person |
|---|---|---|
| Install on Windows or Ubuntu | Lists the tunnels with a command that needs no rights; runs the install | Picks the tunnels from that list; approves administrator rights once (UAC, or the sudo password) |
| Install on macOS | Runs `./install.sh` as the person, not with sudo | Usually nothing: no tunnels to pick. Only when the person cannot write to `/Applications` does the script ask for an administrator password |
| Telegram notifications (optional) | Sets the chat ID if allowed (below), never reads the token | Types the Bot Token in the window or with `wgal notify token set` |
| Let agents use WGAL through MCP (optional) | Adds `wgal-mcp` to its own MCP settings | Turns MCP on in the window's MCP tab, or with `wgal mcp on` in their own terminal (typing yes) |
| Let agents set Telegram (optional) | — | A second switch: the window's MCP tab, or `wgal mcp telegram on` (typing yes) |
| Connect, disconnect, reconnect a tunnel; turn automatic recovery on | Only when the person asks for that, each time | Asks |

- **Approving one does not approve the others.** Installing is not
  permission to start or stop a VPN; turning MCP on is not permission to
  turn automatic recovery on.
- **Install by the guide**: follow [packaging/INSTALL.md](../packaging/INSTALL.md),
  starting with its "Rules for an agent". Never work through the VPN you
  are about to watch. Never read or print a secret.

## Install

1. **Get the release** for the platform from the
   [Releases page](https://github.com/Hsinken/wireguard-always-live/releases),
   and check the archive against the SHA-256 published there.
   - If the Releases page has no archive for the platform, build one on
     that computer: see
     "Build the release from source" in the guide.
2. **Unpack it** and check the files against `RELEASE.txt`. It lists the
   commit the release was built from and the SHA-256 of every file in it.
   This proves the contents match the build record, not that the download
   came from a trusted place: that is what step 1 is for.
3. **Install** as the guide says for the platform, and check every exit
   code: only 0 is success.

## Use WGAL through MCP

`wgal-mcp` is a stdio MCP server: the agent starts it and exchanges one
JSON-RPC message per line. It opens no port and runs no command. Each tool
sends the watchdog the same requests the window sends, marked as coming
from MCP. The watchdog checks the account and the access list as it does
for the window, and logs each change as the agent's.

**Set it up**:

1. The person turns MCP on (above). While it is off, `wgal-mcp` answers
   nothing.
2. Add it to the agent's MCP settings. The window can do this for Claude
   Code, Claude Desktop, Cursor, VS Code, Trae, Kiro, Codex, Antigravity
   and Gemini CLI. By hand, the entry is named `wgal`, with no arguments:

| Platform | `command` |
|---|---|
| Windows | `C:\Program Files\WGAL\wgal-mcp.exe` |
| Ubuntu | `/opt/wgal/wgal-mcp` |
| macOS | `/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp` |

```json
{ "mcpServers": { "wgal": { "command": "/opt/wgal/wgal-mcp", "args": [] } } }
```

**The tools**:

| Tool | Does | Changes anything |
|---|---|---|
| `health` | Everything about WGAL in one call: the watchdog, notifications, every monitored tunnel's light and why, and what to tell the user | No |
| `list_tunnels` | Monitored tunnels, and the official manager's tunnels WGAL does not monitor yet | No |
| `get_tunnel` | One monitored tunnel in full, with every setting | No |
| `register_tunnel` | Starts monitoring a tunnel, observe-only; never starts or stops it; limited by the access list | Yes |
| `unregister_tunnel` | Forgets WGAL's registration; never touches the tunnel | Yes |
| `set_tunnel_monitoring` | Turns one tunnel's monitoring on or off | Yes |
| `set_monitoring_paused` | Pauses or resumes monitoring of every tunnel | Yes |
| `connect_tunnel` | **Starts the real tunnel** and changes routing as its configuration says | Yes |
| `disconnect_tunnel` | **Stops the real tunnel**; connections through it stop | Yes |
| `reconnect_tunnel` | **Stops and starts the real tunnel**, then checks traffic once | Yes |
| `probe_tunnel` | Probes the tunnel's targets now and returns the result | No settings; it sends probes |
| `set_auto_recovery` | Turns automatic recovery of one tunnel on or off; on needs a recovery target that has answered through the tunnel, and the access list | Yes |
| `update_tunnel_settings` | The recovery target, observe-only targets, probe timing and retries of one tunnel | Yes |
| `set_whole_computer_notifications` | Turns Telegram notifications on or off for the computer | Yes |
| `set_one_tunnel_notifications` | The same for one tunnel | Yes |
| `set_telegram_settings` | Changes or clears the device name, chat ID and Bot Token; only while the person lets agents set Telegram; the token is write-only | Yes |
| `send_test_notification` | Sends a test message to Telegram | Sends a message |
| `update_host_settings` | The worker thread count and the time zone | Yes |
| `export_settings` | The settings file (no device name, chat ID or token) | No |
| `import_settings` | Imports a settings file, with a dry run | Yes |
| `read_event_log` | A page of the event log | No |
| `log_locations` | Where the logs are on this computer | No |

`tools/list` gives each tool's full description and schema; read it before
calling a tool.

**What an agent never gets**:

- The Bot Token, ever.
- The chat ID, unless the person lets agents set Telegram.
- Anything outside the access list: an agent acts with the person's
  rights, no more.

Use the tools that start or stop a tunnel, turn automatic recovery on or
send a message only when the person asked for that.

## Use WGAL through the command line

`wgal` does everything the window does with the watchdog. It acts as the
person who runs it, so an agent with a shell can do with it whatever that
person can. Use it only for what the person asked.

| Platform | Run it as |
|---|---|
| Windows | `& 'C:\Program Files\WGAL\wgal.exe' <command>` (PowerShell) |
| Ubuntu | `wgal <command>` |
| macOS | `"/Applications/WG Always Live.app/Contents/MacOS/wgal" <command>` |

- **Commands**: `wgal help` lists them and `wgal help <command>` explains
  one. `wgal status` is a good first call.
- **Output for a program**: add `--json` for one JSON object on standard
  output. Its keys and `kind` values are the same in every language.
- **Language**: add `--lang en` when you need English output. Do not change
  the person's language with `wgal language` unless they ask.
- **Exit codes**:

| Code | Means |
|---|---|
| 0 | Done |
| 1 | Refused, or done with a failed result |
| 2 | A mistake in the arguments or the input; nothing changed |
| 3 | The watchdog could not be reached; nothing changed |
| 4 | It may have been carried out, but that is not confirmed |

Turning MCP on (`wgal mcp on`), and letting agents set Telegram, ask for a
typed yes in an interactive terminal, and refuse without one. That is a
confirmation step, not proof that a person typed it: an agent must never
run these commands, or answer their prompt, for the person.
