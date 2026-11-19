# WGAL on Windows: install, upgrade, removal and emergency revert (WGW-001-D)

**English** | [繁體中文](README.zh-TW.md)

You run the PowerShell scripts in this folder yourself, in **a PowerShell
opened as administrator** (`setup.ps1`, started by double-clicking
`setup.cmd`, asks for administrator rights itself). Agents that develop
WGAL do not run them (they install or stop a service); they only check
their syntax. An agent that installs WGAL for you follows `INSTALL.md`. This is an unsigned
development build: it has no code signature and no installer package, and
SmartScreen may warn about it.

| Script | Purpose |
|---|---|
| `install.ps1` | Installs; upgrades when WGAL is already installed |
| `uninstall.ps1` | Removes |
| `revert.ps1` | Emergency revert: disables and stops WGAL without the GUI, and starts the named tunnels again |
| `setup.ps1` | Guided install: lists the tunnels to tick in a small window, then runs `install.ps1` (start it with `setup.cmd`) |

## Requirements

- Windows 11 with the official WireGuard for Windows, and the tunnels WGAL
  is to watch already in the official app (service
  `WireGuardTunnel$<name>`).
- To build: Rust 1.99.1 (`rust-toolchain.toml`), the MSVC build tools,
  WebView2, Node.js 24.16.0 and pnpm 11.10.0.

## Build

Run this at the repository root, on a clean commit, and note the commit
SHA:

```powershell
pnpm install --frozen-lockfile
cargo build --release --locked -p wgal-runtime --bin wgal-watchdog
cargo build --release --locked -p wgal-mcp
cargo build --release --locked -p wgal-cli
pnpm tauri build --no-bundle
```

`pnpm tauri build` is the same as `pnpm --filter @wgal/desktop run tauri
build`; it runs the front end's `pnpm run build` first. The four
executables are in the workspace's `target\release\`:

- `wgal-watchdog.exe`, the background service;
- `wgal-desktop.exe`, the window;
- `wgal-mcp.exe`, the MCP server for agents, which an agent starts over
  stdio. Until MCP is turned on in the window's settings or with `wgal mcp
  on`, the service refuses all its requests;
- `wgal.exe`, WGAL's command line, since 3.0.0 (see "The command line
  `wgal.exe`" below).

`--no-bundle` makes no MSI or NSIS installer. The GUI connects only to a
pipe that LocalSystem owns (`connect_watchdog_pipe`, since `3c7bc97`), and
so do `wgal-mcp.exe` and `wgal.exe`. The release zip is built by
`make-release.ps1`, which builds these four executables with the same
commands and two compiler flags that keep the build computer's folders out
of them, and checks every file before zipping (see "Build the release from
source" in `INSTALL.md`).

## Put them in one folder

```powershell
$stage = Join-Path $HOME 'wgal-stage'
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\wgal-watchdog.exe, target\release\wgal-desktop.exe, target\release\wgal-mcp.exe,
          target\release\wgal.exe, packaging\windows\install.ps1, packaging\windows\uninstall.ps1,
          packaging\windows\revert.ps1, packaging\windows\setup.ps1, packaging\windows\install.cmd,
          packaging\windows\uninstall.cmd, packaging\windows\revert.cmd, packaging\windows\setup.cmd $stage
```

## Before you start

- **Quit the WGAL window first**: the tray icon's menu, then "Quit
  interface (watchdog keeps running)". `install.ps1` and `uninstall.ps1`
  refuse to run while `wgal-desktop` (the window), `wgal-mcp` (an agent
  using the WGAL MCP server) or `wgal` (the command line) runs: an
  executable in use cannot be replaced or deleted. Close that agent or its
  MCP connection, let the `wgal` command finish or stop it, then run the
  script again.
- Open PowerShell or the Command Prompt as administrator, at the local
  console.
- Run every script as `powershell.exe -NoProfile -ExecutionPolicy Bypass
  -File .\<script>.ps1 ...`. `-ExecutionPolicy Bypass` applies to that one
  PowerShell process only; it does not change any system setting.
- In the Command Prompt you can also run the `.cmd` of the same name
  (`.\install.cmd`, `.\uninstall.cmd`, `.\revert.cmd`; owner request of
  2026-11-05). In PowerShell, keep the `.\` before a `.cmd`: without it,
  PowerShell looks for a file of that name on the PATH and may run another
  tool's script. Each `.cmd` runs the `.ps1` of the same name as above,
  with the Windows PowerShell that comes with Windows
  (`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`, not
  looked up on the PATH), passes the arguments on unchanged, and ends with
  the same exit code.
- The install and removal steps for an agent to follow are in
  `packaging\INSTALL.md` (English) and `packaging\INSTALL.zh-TW.md`
  (`INSTALL.md` and `INSTALL.zh-TW.md` in the release zip).

## Install

### With the check-box window (recommended for people)

In the unzipped folder, double-click `setup.cmd` (owner request of
2026-11-12):

1. Windows asks once for administrator rights (UAC). The later steps run
   in the same elevated process and do not ask again.
2. A small "WGAL setup" window lists this computer's **active** WireGuard
   tunnels (the result of `install.ps1 -ListTunnels`). Tick the ones to
   hand to WGAL and press "Install". A tunnel that is not active has no
   service and is not listed: activate it in the official WireGuard app
   first. A name WGAL does not accept is listed but cannot be ticked.
3. When WGAL is already installed, the button is "Upgrade" and "Keep the
   current list" is ticked: left as it is, the upgrade keeps the list (no
   `-Tunnel`); untick it and tick tunnels again to replace the whole access
   list.
4. Once the window closes, the same black window runs `install.ps1`, with
   the same output and exit code as when it is run directly. Press Enter
   at the end to close it.

On a first install, and whenever you tick the tunnels again, the account
that double-clicks `setup.cmd` becomes the WGAL window's user: before
asking for administrator rights, setup notes that account's SID and passes
it to `install.ps1` as `-User` with the ticked tunnels (also when another
administrator account approves the UAC prompt). An upgrade that keeps the
current list passes neither, so the access list keeps its own account. While the WGAL window or an agent using
the WGAL MCP server is open, or a `wgal` command runs, "Install" asks you
to close it or let it finish first.

`setup.cmd -DryRun` needs no administrator rights: it shows the same
window and only prints the `install.ps1` command it would run. It runs
nothing and changes nothing.

The window speaks Traditional Chinese when the system's display language
is Chinese, and English otherwise.

### For an agent: list the tunnels, then install with administrator rights once

```powershell
.\install.cmd -ListTunnels
```

It needs no administrator rights, changes nothing and prints one line of
JSON:
`{"complete":false,"tunnels":[{"name":"office","status":"running","starts_at_boot":true,"accepted":true}]}`.
`status` is the state of the tunnel's service, `starts_at_boot` whether it
starts at boot, and `accepted` whether WGAL accepts the name. `complete` is
false because a tunnel that is not active has no service and is not
listed. The agent lets the user tick from the list, then runs the install
below with administrator rights **once** (for example started with
`Start-Process -Verb RunAs`, so the user approves one UAC prompt).

### Run it directly

```powershell
cd $HOME\wgal-stage
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Tunnel '<tunnel name>'
```

Or in a Command Prompt run as administrator:

```bat
cd /d %USERPROFILE%\wgal-stage
.\install.cmd -Tunnel <tunnel name>
```

Parameters:

- `-Tunnel`: the tunnels the user may register and hand to automatic
  recovery. Separate several with commas in one argument: `-Tunnel a,b`.
  `-File` and the `.cmd` both pass it as one string, which the script
  splits at the commas (a tunnel name has no comma). In PowerShell a list
  works too: `-Command "& .\install.ps1 -Tunnel 'a','b'"`.
  For a tunnel listed here, the GUI of an ordinary account can also turn
  automatic recovery on. A tunnel WGAL should only watch, never start or
  stop on its own, can be registered with automatic recovery left off.
  Names must follow WireGuard for Windows' rules (`[A-Za-z0-9_=+.-]`, at
  most 32 characters, not `CON`, `NUL` and so on), and the
  `WireGuardTunnel$<name>` service must already exist (a read-only query).
  The case need not match the official app: once the script finds the
  service, it writes the name to the access list as the service manager
  spells it (WGAL compares names case-sensitively). Required on the first
  install.
- `-User`: the SID of the account that uses the WGAL window; default: the
  account that runs the script. If another administrator account was used
  to elevate, give the SID of the account that actually uses the window
  (run `whoami /user` in that account's own, ordinary window).
- `-From`: the folder with the four executables (`wgal-watchdog.exe`,
  `wgal-desktop.exe`, `wgal-mcp.exe`, `wgal.exe`); default: the script's
  folder. It refuses if any of them is missing.

The script **checks first, then acts**. If any check fails, it stops and
changes nothing:

1. It runs as administrator; the WGAL window, `wgal-mcp` and `wgal` are not
   running; the parameters are valid.
2. If `C:\ProgramData\WGAL\` exists, neither it nor anything in it is a link
   or junction, and SYSTEM or Administrators owns each of them (an item
   another account owns may have been planted there: check it and remove
   it yourself, then run again).
3. On an upgrade, the service's executable path must be the one this
   script installs.

Then:

4. It records the Status and StartType of every `WireGuardTunnel$*` service
   and of `WireGuardManager`.
5. It handles the folder first, to keep the gap between the check and the
   creation short. On a first install it creates `C:\ProgramData\WGAL\`
   with `O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`: it creates a new folder
   under ProgramData with a random name that **already has this security
   descriptor**, checks it, and renames it to `WGAL`. If a folder, file or
   junction appeared at `WGAL` meanwhile, the rename fails; the script
   deletes its own temporary folder and stops, and never writes into what
   someone else put there. After the rename it checks again: not a link,
   owned by Administrators, a DACL that does not inherit and gives full
   control only to SYSTEM and Administrators. So the folder never has the
   moment where ProgramData's default lets Users add files. When it already
   exists (an upgrade), the script repeats check 2 above and checks its
   permissions against the same conditions; if they were changed, it stops
   for you to check (the script never rewrites the permissions of a folder
   that exists).
6. It creates `C:\Program Files\WGAL\` and first copies the four new
   executables beside the ones there, with `.new` added to their names (for
   example `wgal-watchdog.exe.new`), and copies the ones in place with
   `.old` added. It checks every copy by its SHA-256. Once all are checked,
   it records their hashes in `upgrade.record` beside them (one line per
   executable: the name, the new version's hash, and the old version's hash
   or `-`; the last line is `started no`, see "Upgrade" below). These names
   and `upgrade.record` are checked not to be links, like the executables.
   Files of these names that an earlier run left are deleted and copied
   again only after their `upgrade.record` confirms that what is in place is
   the complete old or complete new set that run recorded (or that nothing
   was installed before that run). Copies without an `upgrade.record` were
   left by a run that stopped while preparing (the files in place were not
   touched then), and are deleted. When it cannot confirm this, the script
   stops before the service stops, changes nothing, and says what to do:
   those copies may be the only complete set. When that run may already
   have started the new version (`upgrade.record` is not `started no`), it
   only says how to put the complete new set back, or to leave it all as it
   is for the owner; it never suggests putting the old set back.
7. It checks the folder's permissions again and writes the new access list
   as a **new file**, `access.txt.new` (UTF-8 without BOM): a comment saying
   who wrote it and when, then a line `user <SID>` and a line
   `tunnel <name>` for each tunnel. The file's owner is set to
   Administrators (the service accepts only an access list owned by SYSTEM
   or Administrators). Then one replacement (`ReplaceFile`, or a rename
   when there is no old file yet) makes it `access.txt`. The old file is
   never deleted first, so there is always an old or a new list, and a
   handle anyone holds on the old file never reaches the new one.
8. It creates the service `WGALWatchdog`:
   `"C:\Program Files\WGAL\wgal-watchdog.exe"`, LocalSystem. A first
   install creates it **Disabled** and sets it to Automatic only right
   before its first start (step 9), so a failure or Ctrl+C before then
   leaves a service that neither a reboot nor the service manager starts.
   On an upgrade, if `revert.ps1`, or a run that did not start it (see
   below), set it to Disabled, it is set back to Automatic before the
   service stops, and the script says so. Its recovery actions (also set before the stop on an upgrade):
   restart after 1 minute, 1 minute, then every 5 minutes, with the count
   reset after one day (`sc.exe failure`); and
   `sc.exe failureflag WGALWatchdog 1`, so that the recovery actions also
   run when the service stops itself with an error code. On a first
   install the service is created only after the access list and the
   executables are in place (each `.new` copied to its name and checked).
9. On a first install it first writes `started yes` into
   `upgrade.record`, reads it back and sets the service to Automatic; when
   any of that fails it starts nothing, sets the service to Disabled again
   (read back), keeps the copies and `upgrade.record`, and ends with 1. It
   starts the service and waits at most 30 seconds until it is Running and
   `\\.\pipe\wgal-watchdog` exists. Then, like the WGAL window, it checks
   that SYSTEM owns the pipe: it connects at identification level (the
   other side can only identify this administrator, never impersonate it),
   reads the owner and closes without writing anything. An owner other than
   SYSTEM means another process holds the name: the service keeps
   monitoring and recovering and keeps trying, and the script warns loudly
   and ends with 3, also saying so if `service.log` has `the pipe could not
   be served`. A failed start does not interrupt it: it still prints
   `sc.exe query`, what the exit code means and the last lines of
   `service.log`.
10. It records the WireGuard services again and compares; it warns loudly
    about any difference (the script itself never touches them).

Exit codes: 0 done; 1 the service does not run with its pipe; 2 a
WireGuard service changed while the script ran; 3 the service runs, but
another process holds its pipe name; 4 a step of the upgrade failed after
the service stopped, and the script started the service again with the
complete set of executables then in place (it says whether that is the old
or the new version; see "Upgrade"). It returns 4 only when 1, 3 and 2 do
not apply.

After installing, **from an ordinary (not elevated) session**, open
`C:\Program Files\WGAL\wgal-desktop.exe` with Explorer or the Start menu.
Do not start it from an elevated window: it would then run as an
administrator, and the service would take it for an elevated administrator
and bypass the access list (the GUI must never run as an
administrator). The script creates no shortcut, sets nothing to open at
sign-in and does not change the PATH, because it writes nothing outside
those two folders.

### The command line `wgal.exe`

Since 3.0.0 every release carries WGAL's command line, `wgal.exe` (owner
decision of 2026-11-18); it writes in English, or since 4.0.0 in any of
the 13 languages chosen with `wgal language <tag>` or for one command with
`--lang <tag>` (owner decision of 2026-11-19; `INSTALL.md`, "Languages"). It is installed, upgraded and removed with
the other three executables, at `C:\Program Files\WGAL\wgal.exe`. No script
changes the PATH, so run it by its full path, for example in PowerShell:

```powershell
& 'C:\Program Files\WGAL\wgal.exe' status
& 'C:\Program Files\WGAL\wgal.exe' --help
```

- Like the window, run it from an **ordinary (not elevated)** terminal, as
  the account on the access list. From an elevated terminal it makes its
  requests as an administrator, which the access list's limit on tunnels
  does not bind, so do not use it that way day to day.
- `wgal` counts as the user (owner decision of 2026-11-18): anything that
  runs as this account, an agent with a shell too, can do what this user
  can, including reading the Chat ID and changing the Telegram settings.
  The two MCP switches limit only `wgal-mcp`.
- It sends only the requests the window sends, and never runs a command,
  path or service name it is given. It reads a settings file only from
  standard input and writes one only to standard output. It never takes
  the Bot Token from the command line or the environment:
  `wgal notify token set` asks for it without showing what you type.
- Since 4.1.0 the watchdog reads the system's time zone again before each
  message, but Windows gives it the zone as it was up to five minutes
  earlier (owner decision of 2026-11-19): a message right after the zone
  changes may still use the old one. `wgal host --time-zone <name>` takes
  effect at once.

## Upgrade

On an installed machine, the same command upgrades (quit the window
first):

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

It checks `ProgramData\WGAL` and everything in it first. Before the
service stops, it does what it can in advance: the new executables as
`*.exe.new` and copies of the ones in place as `*.exe.old` (step 6 above),
the new list as `access.txt.new` when `-Tunnel` is given (step 7), and the
service's start type and recovery actions (step 8). Then it stops
`WGALWatchdog` and waits for it to stop (at most 60 seconds) and for its
process to end (at most 30 seconds more). Next, before the new version first runs and while nothing
writes, it **backs up the state WGAL stores**: it copies `state\state.json`,
`state\state.prev.json` and `notify\notify.json` (those that exist), in
their subfolders, to a new folder
`C:\ProgramData\WGAL\backup\state-<UTC time of the upgrade, yyyyMMdd-HHmmss>Z\`
and prints its full path. The folder and the copies are owned by
Administrators and inherit the `WGAL` folder's permissions (SYSTEM and
Administrators only). Only the newest 5 are kept and older ones are
deleted (only folders of this name holding only these files; anything else
is left alone, with a warning). The backup never reads or copies
`events\`, the protected Bot Token (the `.dpapi` file in `notify\`),
`access.txt` or `state.lock`. The backup fails when a state file or folder
is a link, or when a backup of the same second exists. Then it puts
`access.txt.new` in place (one replacement) and copies the four `.new`
files to their names, checking each. It then starts the service and checks
the pipe's owner (this is inside the protected part too, see below). Once
the new set is in place, it deletes the `.new` and `.old` copies and
`upgrade.record`. `access.txt` is rewritten only when `-Tunnel` or `-User`
is given, and then as a whole (`-User` needs `-Tunnel` too); all other
data (state, events, notification settings, the protected Bot Token,
`service.log`) is kept. If `revert.ps1`, or a run that did not start the
service, set it to Disabled, the upgrade sets it back to Automatic (WGAL's
own setting) and says so on screen.

**If any step fails from the stop until the started service has been
checked (Ctrl+C included)**, the script does not leave WGAL stopped there.
It:

1. Uses the SHA-256s to find out whether the **complete old set** or the
   **complete new set** is in place (the new one has not run yet at this
   point). When only some were replaced, it copies the old ones back from
   `*.exe.old`, which makes the complete old set (an executable the old set
   did not have is deleted).
2. Starts that set and checks it as usual (Running, the pipe, its owner).
   A warning starting with `!!!` says which step failed and why, whether the
   old or the new version runs now, and which access list is in effect: not
   yet replaced, "the old list is still in effect; the one given this time
   was not applied"; already replaced, the new one is kept, and **the old
   list is not put back** (an authorization you just withdrew is not
   restored).
3. Does not touch the state WGAL stores: the old version goes on with state
   the new version never touched, and no backup is copied over it.
4. Deletes the `.new` and `.old` copies and compares the WireGuard services
   as usual. It ends with 4 when the service runs (1, 3 and 2 come first),
   and with 1 when it does not, saying that nothing monitors the tunnels
   now.

Once the new version's start has begun, the new version may already have
run and changed the state. A failure after that point (Ctrl+C during the
start or the check, for example) **never puts the old version back**: the
script starts the complete new set in place again and says the new version
may already have run.

The `started` line of `upgrade.record` records whether the new version may
already have started: `no` while preparing, changed to `yes` before the new
version starts. The record is deleted with the copies only once the
complete set has started. A later run that reads a record that is not
`started no` (or has no such line) takes it that the new version may
already have run and changed the state: that run, too, **never starts the
old version**, and keeps recording `started yes`.

Every time before it starts the new version (the first start, or the start
again during recovery), the script first writes `started yes` into
`upgrade.record` and reads it back. If it cannot confirm it (a full disk,
for example, or another program holding the file open against writes), it
**does not start** the service. It keeps `*.exe.old`, `*.exe.new` and
`upgrade.record`, ends with 1, and says truthfully whether this run started
the new version: leave the complete new set in place and do not copy
`*.exe.old` back; fix the cause and run the script again; or leave it all as
it is for the owner to decide.

If neither set can be made complete (the old files cannot be copied back
either, for example), the script **does not start** the service. It keeps
`*.exe.old`, `*.exe.new` and `upgrade.record`, ends with 1 and says so.
While the new version has not started, a later run of the script refuses
(and does not delete these copies): fix the cause, put a complete set back
yourself (copy each `*.exe.new` to its name; or copy each `*.exe.old` to its
name and delete the executables that have no `.old`), then run again. When
the new version had started and something failed after that (no pipe
appeared, for example), the script does not roll back to the old version on
its own: the new version may already have changed the state. To roll back,
follow "Roll back to an older version".

When the new version **may already have run** (this run or an earlier one
began to start it), the script does not start the old version and does not
copy it back, even if the complete old set is in place. It keeps the copies
and `upgrade.record`, ends with 1, and says: **do not put the old version
back (do not copy `*.exe.old` back) and do not start it**. Fix the cause
and run the script again, so it puts the complete new set in place; when
what is in place is not a complete set, a run again refuses, and you must
first copy each `*.exe.new` to its name (each must match the new version's
hash in `upgrade.record`); or leave it all as it is for the owner to decide.

In the three cases above where it **does not start** the service (the
record cannot be written, neither set can be made complete, the new
version may have run and what is in place is not the complete new set), the
script first sets `WGALWatchdog`'s start type to **Disabled** and reads it
back, before it explains, so that Windows does not start the set the script
just refused to start at the next boot. It changes only WGAL's own service,
never a WireGuard service or whether one starts at boot. The `!!!` message
says automatic start was turned off; the next run that passes its checks
sets it back to Automatic before it stops the service (a run that refuses
before the stop, such as the "a run again refuses" above, leaves it
Disabled). When it cannot set Disabled, the message says plainly that it
could not, and that Windows may start the set in place at the next boot:
fix the cause and run the script again before you restart, or leave it all
as it is for the owner.

The test of this flow is `tests\install-recovery.tests.ps1`. Run it as an
ordinary user
(`powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\install-recovery.tests.ps1`).
It loads only `install.ps1`'s functions, replaces every service command
with a stand-in, writes files only in its own folder under `%TEMP%`, and
injects failures step by step. The stand-ins exist only in the test;
`install.ps1` has no parameter that selects them. It is not an upgrade test
on a real machine.

**Do not install a version older than the one installed directly**; to go
back, follow the next section. A newer version may store fields in the
state file or the notification settings that an older version cannot read
(the versions before `9fe8123` cannot read the state files of later ones,
for example). The older version then takes the state as damaged and runs
read-only: it shows a storage failure, makes no automatic recovery and
sends no notification, with no other clear sign.

## Roll back to an older version

**Going back to a version before 4.0.0** (owner decision of 2026-11-19):
once `wgal language <tag>` has been used, `notify\notify.json` holds the
language `wgal` writes in, and a watchdog before 4.0.0 cannot read that
file: it keeps notifications off and refuses changes to them until the
file is fixed. Before rolling back, run `wgal language --clear` in 4.0.0 or
later (the Telegram messages keep their language) and check that `wgal
language` shows English as the default. If an older version already runs
with notifications off for this reason (rolled back without step 3a),
install 4.0.0 or later again, run `wgal language --clear`, and roll back
again with the steps below. Step 3 puts `notify.json` back from the backup, which may
hold that language too (a backup made by an upgrade from 4.0.0 or later),
so when you go back to a version before 4.0.0, step 3a checks for it and,
when it is there, deletes the file: the older version then starts with
its default notification settings. Either way the notification settings
are no newer than the backup; the protected Bot Token is not touched.

What you put back is the backup made **when you upgraded to the version you
want to undo** (the `backup\state-…Z` folder the upgrade printed). It is
not necessarily the newest one: a later upgrade or reinstall makes newer
backups. A rollback loses what WGAL stored after that upgrade (new
registrations, monitoring and notification settings, event and
notification states, the choices to disconnect and to recover
automatically). The official WireGuard settings and services are not
affected, and WGAL's event log and the protected Bot Token are left as they
are.

**The older version must take the state you put back as "restored from a
backup".** The backup stops at the moment of the upgrade. A notification
not yet sent then may have been sent by the newer version since; an
automatic recovery that was on then may have been turned off by you since,
and the tunnel may have been disconnected by you. Put back as the current
state file, the backup is trusted by the older version as usual: it may
send the same notification again, or start a tunnel you disconnected,
following settings that are out of date. So step 3 puts the backup in
place as the **previous** file (`state\state.prev.json`) and rewrites the
**current state file** (`state\state.json`) as one line of text that is on
purpose not a state file. When WGAL's state store finds "the current file
is damaged and the previous one readable", it loads the previous one
conservatively, as "restored from a backup": each tunnel's automatic
recovery and "start the tunnel when it is stopped from outside" are turned
off, unfinished operations are cancelled, notifications not yet settled
become "Outcome unknown (not resent automatically)", and this conservative
state is written at once as the new current file, the previous one kept as
the backup. (This logic is in `crates/wgal-runtime/src/store.rs` and
`distrust` in `crates/wgal-core/src/watchdog.rs`; these two parts are the
same in `00ad525`, `7339e8e` and 1.0.0.) Putting back only the previous
file, **without** a current file, does not work: that is taken for a save
interrupted halfway, and the previous file is trusted as usual.

The order is: put the backup back first, then the old executables, so that
the older version reads state it understands on its first start. The other
way round, the older version first reads a file it cannot read: it runs
read-only, or, when only the previous file is readable, restores
conservatively and writes back, and you would have to stop the service
once more before putting the backup back.

Quit the WGAL window first, then, in a PowerShell opened as administrator:

1. Find that backup:

   ```powershell
   $data = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'WGAL'
   Get-ChildItem -LiteralPath (Join-Path $data 'backup') -Directory | Sort-Object Name
   $from = Join-Path $data 'backup\state-<time of that upgrade>Z'
   if (-not (Test-Path -LiteralPath $from -PathType Container)) { throw "There is no such backup: $from" }
   ```

2. Stop the service. `-NoWait` only asks: the second line waits at most
   60 seconds and ends with a timeout error when the service has not
   stopped by then. Then do not go on: run `revert.ps1` (Emergency revert),
   which ends the service's process, and come back to step 3 once
   `WGALWatchdog` is stopped (step 3 checks that again).

   ```powershell
   Stop-Service -Name WGALWatchdog -NoWait
   (Get-Service -Name WGALWatchdog).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
   ```

3. Put the backup back. It checks first and, if any check fails, stops
   and changes nothing: the service is stopped; the `state` folder exists;
   neither the folders, the backup nor the files it touches are links or
   junctions; what it writes over (`state\state.json`,
   `state\state.prev.json`, `notify\notify.json`) is a file or not there,
   never a folder, and has no other name (a hard link), so writing it
   changes no other file; if the backup has state files, at least one is
   readable. It runs as one block that stops at the first error (`&` with
   `$ErrorActionPreference = 'Stop'`), so nothing after a failed write is
   done; then do not go on to step 4.
   Then:
   - It first rewrites `state\state.json` as the marker text. From this
     moment, an interruption leaves only the conservative "restored from a
     backup", or read-only, never trusted old state.
   - It copies the backup's usable state (its `state\state.json`; if that is
     not readable, its `state\state.prev.json`) to `state\state.prev.json`.
     When the backup has no state file (WGAL had stored no state yet at the
     upgrade), it deletes both state files, and the older version starts
     empty.
   - It puts the notification settings `notify\notify.json` back from the
     backup; when the backup has none (a newer version wrote it), it
     deletes it.
   - It sets these files' owner back to Administrators.

   ```powershell
   & {
   $ErrorActionPreference = 'Stop'
   $state = Join-Path $data 'state'
   $current = Join-Path $state 'state.json'
   $previous = Join-Path $state 'state.prev.json'
   $settings = Join-Path $data 'notify\notify.json'
   $marker = 'WGAL rollback: not a state file. The watchdog loads state.prev.json as restored from a backup.'
   if ((Get-Service -Name WGALWatchdog).Status -ne 'Stopped') { throw 'WGALWatchdog has not stopped yet; do step 2 first.' }
   if (-not (Test-Path -LiteralPath $state -PathType Container)) { throw "There is no $state; this is not a folder WGAL used." }
   $paths = @($data, $state, (Join-Path $data 'notify'), $current, $previous, $settings, $from) +
       @('state', 'notify', 'state\state.json', 'state\state.prev.json', 'notify\notify.json' |
           ForEach-Object { Join-Path $from $_ })
   foreach ($path in $paths) {
       if ((Test-Path -LiteralPath $path) -and
           ((Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
           throw "$path is a link or junction; check it yourself before you go on."
       }
   }
   foreach ($path in @($current, $previous, $settings)) {
       if (Test-Path -LiteralPath $path) {
           $item = Get-Item -LiteralPath $path -Force
           if ($item.PSIsContainer -or ($item.LinkType -eq 'HardLink')) {
               throw "$path is a folder or has another name (a hard link); check it yourself before you go on."
           }
       }
   }
   $found = @('state\state.json', 'state\state.prev.json' | ForEach-Object { Join-Path $from $_ } |
       Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
   $saved = $found | Where-Object {
       try {
           $json = Get-Content -LiteralPath $_ -Raw -Encoding UTF8 | ConvertFrom-Json
           $null -ne $json -and ($json.PSObject.Properties.Name -contains 'schema_version')
       } catch { $false }
   } | Select-Object -First 1
   if (($found.Count -gt 0) -and ($null -eq $saved)) { throw "None of the backup's state files is readable: $from" }
   [IO.File]::WriteAllText($current, $marker)
   if ($null -ne $saved) {
       Copy-Item -LiteralPath $saved -Destination $previous -Force
   } else {
       if (Test-Path -LiteralPath $previous) { Remove-Item -LiteralPath $previous }
       Remove-Item -LiteralPath $current
   }
   $backupSettings = Join-Path $from 'notify\notify.json'
   if (Test-Path -LiteralPath $backupSettings -PathType Leaf) {
       Copy-Item -LiteralPath $backupSettings -Destination $settings -Force
   } elseif (Test-Path -LiteralPath $settings -PathType Leaf) {
       Remove-Item -LiteralPath $settings
   }
   foreach ($path in @($current, $previous, $settings)) {
       if (Test-Path -LiteralPath $path -PathType Leaf) {
           $acl = Get-Acl -LiteralPath $path
           $acl.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))
           Set-Acl -LiteralPath $path -AclObject $acl
       }
   }
   }
   ```

3a. **Only when you go back to a version before 4.0.0**, right after step 3
   and with the service still stopped: check the notification settings you
   put back for `wgal`'s language (owner decision of 2026-11-19). A backup
   made by an upgrade from 4.0.0 or later can hold it (once `wgal language
   <tag>` was used), and a version before 4.0.0 cannot read such a file: it
   would keep notifications off. The backup made at the upgrade from the
   older version itself never holds it, so with that one this step finds
   nothing.
   - It only reads the file, as JSON. When it holds `cli_language` at its
     top level, whatever the value, it deletes the file, as step 3 does
     when the backup has none: the older version then starts with
     notifications off and its own defaults, and you set the device name,
     chat ID and message language again in its window. The protected Bot
     Token is not touched.
   - It stops with an error, changing nothing, when the file is a link or
     junction or not a JSON object, or when it cannot be read or deleted: it
     runs as one block that stops at the first error, as step 3 does, so
     after a failed read it never goes on to judge or delete with what an
     earlier run had read. Then do not go on to step 4.

   ```powershell
   & {
   $ErrorActionPreference = 'Stop'
   $settings = Join-Path $data 'notify\notify.json'
   if (Test-Path -LiteralPath $settings -PathType Leaf) {
       if ((Get-Item -LiteralPath $settings -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
           throw "$settings is a link or junction; check it yourself before you go on."
       }
       Add-Type -AssemblyName System.Web.Extensions
       $object = (New-Object Web.Script.Serialization.JavaScriptSerializer).DeserializeObject([IO.File]::ReadAllText($settings))
       if ($object -isnot [Collections.Generic.IDictionary[string, object]]) { throw "$settings is not a JSON object; check it yourself before you go on." }
       if ($object.ContainsKey('cli_language')) {
           Remove-Item -LiteralPath $settings -ErrorAction Stop
           "Removed $settings, which held wgal's language: set the notifications again in the older version."
       }
   }
   }
   ```

4. In **the older version's folder** (the older version's executables
   and its own scripts), run its `install.ps1` without `-Tunnel`: it puts
   the old executables back and starts the service (it goes ahead with the
   service stopped too). An older `install.ps1` that makes backups also
   saves the result of step 3 as a backup now (its `state.json` is the
   marker text); for a later rollback, still choose the one made at the
   upgrade. When you roll back to a version before 3.0.0, the older
   `install.ps1` knows only its own three executables: 3.0.0's `wgal.exe`
   stays in `C:\Program Files\WGAL\` and is neither replaced nor deleted
   (2.2.2's `uninstall.ps1` deletes it with the whole folder).

   ```powershell
   cd <the older version's folder>
   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
   ```

5. Check: the last start in `C:\ProgramData\WGAL\service.log` has `the
   current state file was damaged; the previous copy was loaded` (this is
   expected: step 3 makes the older version load the backup this way on
   purpose; with no state in the backup it is `no stored state; starting
   empty`) and `running`, and no `stored state could not be loaded`;
   `state\state.json` is no longer the marker text (the older version wrote
   the conservative state). The WGAL window, opened as an ordinary user,
   shows the tunnels and settings as they were at the upgrade, with no
   storage failure. The notification settings are as before the upgrade;
   but when step 3a removed `notify\notify.json`, or the backup had none,
   notifications are off with the older version's defaults: turn them on
   and set the device name, chat ID and message language again.

6. **What you decide again after the rollback** (WGAL does not restore
   it):
   - Each tunnel's "Automatic recovery" and "Also start the tunnel when it
     is stopped from outside" are off; turn them on again on the tunnel's
     card if you want them. While they are off, WGAL keeps monitoring and
     showing the lights, but never starts or stops a tunnel on its own.
   - The connection intents are those of the upgrade. For a tunnel you disconnected after the upgrade, the older version sees it stopped but cannot tell who stopped it (automatic recovery is off, and it never saw it connected before it started): the card shows the gray light "Inactive" with the reason "Not connected", never as stopped externally, and WGAL does not start it. To keep it disconnected, press disconnect once in WGAL; to connect it, press connect.
   - Incidents that were not closed at the upgrade, whose notification was
     neither surely sent nor cancelled, get the notification state
     "Outcome unknown (not resent automatically)": WGAL does not send them
     again; check in Telegram yourself whether they arrived, if it
     matters.
   - Registrations, monitoring and probe settings, each tunnel's
     notification switch and monitoring pauses made after the upgrade are
     lost; set them again if you want them.

Without a backup of that upgrade (an upgrade from an older `install.ps1`
that made none, for example), the only clean rollback is to remove WGAL
first with `uninstall.ps1 -RemoveData` (which deletes WGAL's state, events,
settings and token), then install the older version.

## Removal

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall.ps1             # keeps the data
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\uninstall.ps1 -RemoveData # also removes ProgramData\WGAL
```

In order (quit the window first): it records the WireGuard services; stops
`WGALWatchdog` and waits for it and its process to end (if it is restarting
a tunnel, it first starts that tunnel again, then stops); deletes the
service with `sc.exe delete`; removes `C:\Program Files\WGAL\`; and removes
`C:\ProgramData\WGAL\` (including the upgrades' state backups in `backup\`)
only when `-RemoveData` is given. It refuses to delete a folder that is, or
holds, a link or junction, so that Windows PowerShell's recursive delete
never follows a link out of the folder. **Even when an earlier step
failed**, it compares the WireGuard services at the end. For any
`WireGuardTunnel$*` still Stopped, it only prints a warning and the command
you can run yourself, `Start-Service 'WireGuardTunnel$<name>'`; the script
does not start it. If the service hangs while stopping, run `revert.ps1`
first, then `uninstall.ps1` again. Exit codes: 0 done; 1 a step failed; 2 a
WireGuard service changed.

## Emergency revert (without the GUI)

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\revert.ps1 -Tunnel '<tunnel name>'
```

It touches only `WGALWatchdog` and the "start" of the tunnels named:

1. Sets `WGALWatchdog` to Disabled (neither a restart nor the service
   manager's recovery actions start it after that).
2. Asks it to stop without waiting, and waits at most 45 seconds (if it is
   still starting and did not accept the request, it asks again while it
   waits). If it still has not stopped, it ends the process the service
   manager reports for this service, provided the program the service is
   registered with is indeed `"C:\Program Files\WGAL\wgal-watchdog.exe"`.
3. For each `-Tunnel`: waits for its service to leave a pending state (at
   most 60 seconds); if it is Stopped, asks it to start and waits at most 60
   seconds for Running. When that takes longer, it warns plainly; it never
   waits without end.
4. Prints the state and start type of `WGALWatchdog` and of every WireGuard
   service. The exit code is 0 when WGAL has stopped and the tunnels are
   Running, and 1 otherwise.

It never stops or changes a tunnel's service, never changes a tunnel's
start type, never touches `WireGuardManager`, and does not touch the data
WGAL stores (the automatic recovery settings stay as they are). To bring
WGAL back:

```powershell
Set-Service -Name WGALWatchdog -StartupType Automatic
Start-Service -Name WGALWatchdog
```

An upgrade with `install.ps1` also sets it back to Automatic. Automatic
recovery keeps the settings it had: once WGAL runs again, it may stop and
start a registered tunnel that has automatic recovery on as soon as that
tunnel's own fault confirmation is met (its probe interval, retries and
minimum fault time; with 0 retries and a minimum fault time of 0, one failed
round is enough). `wgal show <tunnel>` shows them. If WGAL must not act
right away: before you run `revert.ps1`, while the window or `wgal` still
reaches the watchdog, pause all monitoring (the window's Settings page, or
`wgal pause`). The pause is stored, so WGAL comes back paused: no probes and
no automatic actions until you resume it. Without that, automatic recovery
cannot be turned off while WGAL is stopped.

## Troubleshooting

- **The window says "The signed-in account is not on this installation's
  access list…"**, or that the watchdog refused this account (the details,
  where shown, start with `unauthorized (caller_not_allowed)`): this
  account is not on the access list, or the service refused `access.txt`.
  Look in `C:\ProgramData\WGAL\service.log` for `access list refused`
  (followed by the line number and the reason), and run
  `install.ps1 -Tunnel <name> -User <SID>` again with the right account.
  Also look for `the pipe could not be served`: if a process holding the
  pipe name refuses this account, the window shows the same "not on the
  access list" message.
- **The window shows the watchdog as unreachable, but the service is
  Running**: look in `service.log` for `the pipe could not be served`.
  Another process holds `\\.\pipe\wgal-watchdog`. The service keeps
  monitoring and recovering, and tries again every minute until the name
  is free. The window connects only to a pipe LocalSystem owns, so it never
  sends a request or the token to the holder.
- **The service did not start**: see the `SERVICE_EXIT_CODE` of
  `sc.exe query WGALWatchdog` in the table below.

## What the scripts change, and what they never change

| Created or changed | Never changed |
|---|---|
| `C:\Program Files\WGAL\` (four executables; during install and upgrade also their `*.exe.new` and `*.exe.old`, deleted when done) | The settings, ACL and start type of any `WireGuardTunnel$*` service (`revert.ps1` only starts the named tunnels that are stopped) |
| `C:\ProgramData\WGAL\` (owner, DACL, `access.txt` and, before it is written, `access.txt.new`; on an upgrade, a new state backup in `backup\` and the deletion of those older than the newest 5) | `WireGuardManager` and the official WireGuard app |
| The service `WGALWatchdog` (created, recovery actions, started, stopped, disabled, deleted) | WireGuard configurations, keys, routes, DNS, firewall, MTU, network interfaces |
| | Registry startup entries, scheduled tasks, shortcuts, other programs |

## The service's files and exit codes

In `C:\ProgramData\WGAL\`: `access.txt` (written by the installer),
`state\` (the state), `notify\` (the notification settings and the
DPAPI-protected token, which only LocalSystem can unprotect), `events\` (the
event log `events.jsonl`), `backup\` (the upgrades' state backups, written
by the installer, at most 5, never read by the service) and `service.log`
(the service's own log of starts, stops, pipe retries, network changes and
errors; at most 256 KiB, with one older file kept).

The `SERVICE_EXIT_CODE` of `sc.exe query WGALWatchdog` (with
`WIN32_EXIT_CODE` 1066):

| Code | Meaning | Log |
|---|---|---|
| 1 | ProgramData was not found, or the `WGAL` folder does not exist, is a link, or has an unsafe owner or DACL | None (nothing is written to a folder that is not trusted) |
| 2 | The `state`, `notify` or `events` subfolder cannot be created or is not safe | `service.log` |
| 3 | The state store cannot be opened (another watchdog holds the lock, for example) | `service.log` |
| 4 | The watchdog loop cannot start | `service.log` |
| 11 | The running watchdog loop did not answer three times in a row (about 90 seconds) | `service.log` |

Each of these exit codes triggers the service manager's recovery actions.
A pipe problem does not stop the service (see Troubleshooting). A stop that
you (or a shutdown) ask for always ends with success, so the service is
not restarted on its own after `Stop-Service WGALWatchdog`. If the loop
hangs during the stop, the service reports stopped after at most about 45
seconds and ends its process.
