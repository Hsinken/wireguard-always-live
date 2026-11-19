# WGAL on macOS: install, upgrade and removal

**English** | [繁體中文](README.zh-TW.md)

The steps to install, check and remove WGAL are in `INSTALL.md`. This file
says what the scripts do, where the files are and where to look when
something goes wrong.

> **Status (2026-11-20)**:
> - On 2026-11-13 WGAL was built and tested natively on a Mac for the
>   first time (Apple silicon, macOS 27.2.1): the five commands pass, and
>   the scripts' stand-in tests ran and pass with macOS's own bash 3.2.
> - The same day, a test build (with the version numbers GUI 2.2.1,
>   watchdog 2.3.1) was installed, upgraded (one run with a failure
>   injected after the watchdog stopped) and removed with
>   `uninstall.sh --remove-data` on that Mac: PASS. That is not "accepted
>   on a device".
> - 4.2.1 was built from this repository and installed on that Mac. Its
>   watchdog could not list the tunnels (`manager_query_failed`):
>   macOS 27.2.1 writes the service type as `[VPN:com.wireguard.macos]`,
>   with a colon, and the parser accepted only a slash. 4.2.2 reads both.
>   The list's format and the status's were read on that Mac without
>   changing anything; the status already parses.
> - Two things must be verified on a real Mac as the LaunchAgent, not
>   just tried in Terminal, and are not done yet (NOT RUN):
>   - connecting and disconnecting with `scutil --nc`;
>   - reading the Keychain after the screen locks.
> - Until a probe bound to the tunnel's interface is proven on a Mac, the
>   light is never green and automatic recovery cannot be turned on
>   ([docs/testing.md](../../docs/testing.md)).
> - Also not tested on a real Mac yet (NOT RUN): the
>   recovery that renames the previous app back, the refusal to start once
>   the new version may have run, turning the start at login off and on
>   again, an interruption with Ctrl-C, and an account that needs sudo to
>   write to `/Applications`.

## How it works

- **The watchdog**: the user's own LaunchAgent (plan §3.1). launchd
  starts it as the user after login, with only that user's rights and no
  root helper. It controls the official WireGuard app's VPN configurations
  with `/usr/sbin/scutil --nc`, and probes with `/sbin/ping` and
  `/sbin/route`. It never changes a VPN configuration or an On-Demand
  rule.
- **The window**: talks to the watchdog over
  `~/Library/Application Support/WGAL/watchdog.sock`, and connects only to
  a socket the same user owns.
- **The command line, `wgal`**: WGAL's command line, in English or in the
  language chosen with `wgal language <tag>` (since 4.0.0), in the app at
  `Contents/MacOS/wgal`. Like the window, it connects only to a socket the
  same user owns and only sends requests.
  - The install script does not change the PATH, so run it by its full
    path, as yourself (**not with sudo**):
    `"/Applications/WG Always Live.app/Contents/MacOS/wgal" status`.
    `--help` lists every command.
  - `wgal` counts as you (owner decision of 2026-11-18): any program that
    runs it as your account, an agent with a shell too, can do what you
    can, including reading the chat ID and changing the Telegram settings.
    The MCP switches limit only `wgal-mcp`.
- **Who the watchdog answers**: only the same user; requests from other
  users are refused. The watchdog has the user's own rights, so macOS has
  no access list.
- **Secrets**: the bot token is kept in the user's login Keychain: service
  `io.github.hsinken.wg-always-live`, account `telegram-bot-token`. It is
  set in the window or with `wgal notify token set`; `wgal` never takes a
  token from the command line or the environment. The watchdog reads it
  without showing any dialog. `uninstall.sh --remove-data` deletes it with
  the app's `wgal-watchdog --forget-token`: that counts as done only when
  the token is surely deleted or was not there, and when the Keychain
  refuses, it says plainly that the token is still there.

## Files

| Path | Contents |
|---|---|
| `/Applications/WG Always Live.app` | the window; `Contents/MacOS/` also holds `wgal-watchdog`, `wgal-mcp` and the command line `wgal` |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.watchdog.plist` | the watchdog (launchd starts it again when it ends other than normally) |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.window.plist` | opens the window at login |
| `~/Library/Application Support/WGAL/` (0700) | `state`, `notify`, `events`, `watchdog.sock` (0600), `uninstall.sh`; the state backups made before upgrades, `backup/state-<UTC time>` (at most 5); during an install, `upgrade/` (the new LaunchAgents and copies of the old ones, removed when it is done) |
| `~/Library/Logs/WGAL/watchdog.log` | the watchdog's log |
| `/Applications/.WG Always Live.app.new`, `.WG Always Live.app.old` (hidden) | only during an install: the copied new app and the previous app it replaced; removed when it is done. When an upgrade that did not finish leaves them, `uninstall.sh` removes them together with `upgrade/` (a link there is left alone) |

## Upgrades, and failures along the way

`install.sh` always takes the same path (on a first install, "stop" and
"back up" have nothing to do). It first checks that the release's app
holds the four programs (`wgal-desktop`, `wgal-watchdog`, `wgal-mcp`,
`wgal`), and stops before it changes anything when one is missing.

Before it stops anything, it does everything it can:
- the data and log folders, and `uninstall.sh`;
- a **content record** of the release's app (every folder and link, and
  every file's SHA-256), saved as `upgrade/app.record`. If listing,
  reading a link or hashing fails at any point, there is no record: the
  script stops with 65, and stops and removes nothing;
- the new app, copied with `ditto` to the hidden
  `/Applications/.WG Always Live.app.new`, compared item by item with that
  record, and its download quarantine mark removed (an administrator
  password is asked for at this point when needed);
- the two LaunchAgents, written in `upgrade/` and checked with
  `plutil -lint`, and copies of the two in place.

Then it stops the watchdog's and the window's LaunchAgents, and **backs up
the state** (as on Windows and Ubuntu: `state/state.json`,
`state/state.prev.json` and `notify/notify.json` are copied to
`backup/state-<UTC time>`, keeping only the newest 5; the bot token is in
the Keychain, not in the backup). It renames the app in place to
`.WG Always Live.app.old` and the new one into its place (one rename each;
the previous app is no longer removed first), moves the LaunchAgents to
`~/Library/LaunchAgents`, and finally starts the watchdog and checks its
socket. On success it removes `.new`, `.old` and `upgrade/` (when the
previous app cannot be removed, `upgrade/` is kept: the next run needs its
record).

When an earlier run left the hidden `.WG Always Live.app.old` (the
previous app it replaced), it is removed only when the app in place is
exactly what that run's `upgrade/app.record` describes (that run's
complete new app). Otherwise the script stops with 65 before it stops or
changes anything, and says what to do: when that run had not started the
new version yet, how to rename the previous app back (it may be the only
complete app); when it may have started the new version (see
`upgrade/started` below), only how to put the complete new app back, or
to leave everything as it is for the owner.

`upgrade/started` records whether the new watchdog may have started: `no`
while preparing, `yes` just before the new version is started; `upgrade/`
is removed only once the complete app has started. When a later run finds
`upgrade/` and this file does not say `no` (or is missing, or cannot be
read), it takes it that the new version may have run and changed the
stored state: that run, too, **never starts the previous app**, and keeps
recording `yes`. A later run **does not remove the whole `upgrade/` and
make it again**: it keeps the folder, first writes `started` with one
rename (still `yes` if it was `yes`), and only then handles the rest of
what it prepares, so a failure or an interruption in between still tells
the next run that the new version may have run.

Every time before it starts the new watchdog (its first start, or the
trap's start during recovery), the script writes `yes` into
`upgrade/started` and reads it back. When that cannot be confirmed (a full
disk, say), it does **not** start it, keeps the hidden
`.WG Always Live.app.old` and `upgrade/`, ends with 1, and says truthfully
whether this run started the new watchdog: leave the complete new app in
place, do not rename the previous app back, fix the cause and run the
script again from the release folder; or leave everything as it is for
the owner to decide.

**If any step fails from the watchdog's stop until the started watchdog
has been checked (Ctrl-C included)**, the `trap` takes over:

1. It finds out whether what is in place is the **complete previous app**
   (never renamed) or the **complete new app** (the same, item by item, as
   the content record made before the stop; the release is not read
   again, so it does not matter if the disk it is on has gone by then).
   When the previous app was renamed and the new one is not in place yet,
   it renames the previous one back.
2. It puts the matching LaunchAgents in place (the newly written ones for
   the new version, the copies of the original ones for the previous
   version), starts the watchdog (a LaunchAgent already loaded is not
   loaded again) and checks its socket, and says which step failed and
   whether the previous or the new version runs now.
3. It does not touch WGAL's stored state, and never copies a backup over
   it.
4. It compares `scutil --nc list` as usual. It ends with 4 when the
   watchdog runs (1 and 2 come first), and with 1 when it does not,
   saying that nothing monitors the tunnels now.

Once the new watchdog's start has begun, the new version may already have
run and changed the stored state: a failure after that (Ctrl-C while it
loads or waits for the socket, say) **does not rename the previous app
back**; the `trap` starts the complete new app in place again, and says
that the new version may have run.

With neither app complete, nothing is started; the hidden `.old`/`.new`
and `upgrade/` are kept, and it ends with 1 and says so. When the new
version had not started yet, a later run refuses with 65 (and does not
remove the previous app): fix the cause, rename
`.WG Always Live.app.old` back to `WG Always Live.app` yourself (with sudo
when needed), and run it again.

When the new version **may already have run** (this run or an earlier
one began starting it), the `trap` neither starts the previous app nor
renames it back, even when what is in place is the complete previous app.
It ends with 1 and says: **do not put the previous app back and do not
start it**; fix the cause, put the complete new app in place (move
`/Applications/WG Always Live.app` aside and copy `WG Always Live.app`
from the same release there with `ditto`, with sudo when needed), and run
the script again from that release; or leave everything as it is for the
owner to decide. When the previous app was renamed to
`.WG Always Live.app.old` and what is in place is not the complete new
app, a later run also refuses with 65 and gives the same advice. A new
version that started and then shows no socket is not rolled back
automatically: it may already have changed the stored state.

In the three cases above where it does **not** start (`started` cannot be
written, neither app is complete, or the new version may have run and
what is in place is not the complete new app), the script first runs
`launchctl disable gui/<uid>/io.github.hsinken.wg-always-live.watchdog`
(a setting launchd keeps, for WGAL's own watchdog only) and checks it with
`launchctl print-disabled gui/<uid>`, so that at the next login launchd
does not start, through the LaunchAgent's `RunAtLoad`, the watchdog of the
app the script just refused to start. The window's LaunchAgent is left
alone (it only opens the window, and never starts the watchdog).
- The `!!!` message says the start at login was turned off; the next run
  that passes its checks runs `launchctl enable` before it loads the
  watchdog (launchd does not load a disabled service; the trap also
  enables whichever one it starts during recovery).
- A run that refuses with 65 before the stop leaves it disabled.
- When it cannot be turned off, the message says so plainly, and that
  launchd may start it at the next login: fix the cause and run the script
  again before you sign out or restart, or leave it as it is for the
  owner.
- After it removes the LaunchAgents, `uninstall.sh` also runs
  `launchctl enable` for this label, so no disabled setting is left
  behind.

The exit codes of `install.sh`:
- 0: done;
- 1: the watchdog does not run with its socket;
- 2: a VPN configuration changed;
- 4: an upgrade step failed after the stop; the watchdog was started again
  with the complete app in place (only when neither 1 nor 2 applies);
- 64: wrong usage;
- 65: a check or a preparation before the watchdog stops did not pass. The
  watchdog and the app in place are as they were, but the data and log
  folders, the `uninstall.sh` in them, `upgrade/` and the hidden
  `.WG Always Live.app.new` may already have been replaced.

The test of this flow is `tests/install-recovery.test.sh`. It loads
`install.sh` as a library (it then runs nothing), points its paths at a
scratch folder of its own, replaces `launchctl`, `pgrep`, `sleep`,
`ditto`, `xattr`, `plutil`, `scutil`, `sudo` and the wait for the socket
with stand-ins, and makes each step fail in turn; `mv`, `rm`, `readlink`,
`find` and `shasum` are stand-ins that can be made to fail and otherwise
run the system's own tools. It runs in any bash (it was written with
WSL's bash 5); since 2026-11-13 it also runs on a Mac with macOS's own
bash 3.2 and the BSD tools. The real launchd, `ditto`, `scutil` and `sudo` are not tested by
it; it is not an upgrade test on a real machine. The stand-ins exist only
in the test: `install.sh` has no option or variable that selects them.

## Going back to a version before 4.0.0

Once `wgal language <tag>` has been used (owner decision of 2026-11-19),
`~/Library/Application Support/WGAL/notify/notify.json` holds the language `wgal` writes in, and a watchdog before
4.0.0 cannot read that file: it keeps notifications off and refuses
changes to them until the file is fixed.

- Before installing an older version, run `wgal language --clear` in 4.0.0
  or later (the Telegram messages keep their language) and check that
  `wgal language` shows English as the default.
- If an older version already runs with notifications off for this
  reason, install 4.0.0 or later again, run `wgal language --clear`, and
  then install the older version again.

The backups made before upgrades (`backup/state-<UTC time>`) are no way around this: one
made by an upgrade after `wgal language` was used holds the language too.
This README gives no steps to put a backup back on macOS.

## Signing and Gatekeeper

- **Signing**:
  - The app is signed ad hoc only (`codesign --sign -`). Apple silicon
    runs only signed code; after the release builder puts the three
    programs (`wgal-watchdog`, `wgal-mcp`, `wgal`) into the app, it must
    sign it again.
  - It has no Apple developer ID signature and is not notarized by Apple
    (owner decision of 2026-11-05).
- **What the install script does**: it removes the download quarantine
  mark from `/Applications/WG Always Live.app` only
  (`xattr -dr com.apple.quarantine`).
- **What it does not do**: it does not change the system's Gatekeeper
  settings, and never asks you to turn any security feature off.
- **To move to developer ID signing and notarization later**: the release
  builder changes, the way to install does not; a notarized DMG is
  another option.

## Stopping, and the logs

- To stop the watchdog for now:
  `launchctl bootout gui/$(id -u)/io.github.hsinken.wg-always-live.watchdog`
- To start it again: run `./install.sh` again, or sign out and in.
- The log: `tail -n 50 ~/Library/Logs/WGAL/watchdog.log`. The event log is
  shown by the window's log page and by `wgal log`.

## Exit codes (`exited with` in the log)

| Code | Meaning |
|---|---|
| 1 | no home folder, a path too long (the socket's is at most 103 bytes), or a folder that is not your own 0700 folder |
| 2 | `state`, `notify` or `events` is not your own 0700 folder |
| 3 | the state file cannot be opened |
| 4 | the monitoring loop cannot start |
| 5 | run as root (the watchdog must run as the user) |
| 11 | the monitoring loop stopped answering; launchd restarts it |

## Differences from the Windows version (not implemented yet)

- After sleep or a network change, the grace period does not start again;
  the failed probes alone still decide whether there is a fault.
- The watchdog runs only while the user is signed in: it does not monitor
  after the user signs out, or before the first login (plan §3.1).
