# WG Always Live: install and removal guide

**English** | [繁體中文](INSTALL.zh-TW.md)

This guide is in every release archive, written for people and for an agent
that installs WGAL for them. Follow it to install, upgrade, check and remove
WG Always Live (WGAL) on Windows 11, macOS or Ubuntu, with a desktop or
without one (a server). Each platform's details are in `README.md` in the
same folder.

WGAL is not a VPN and does not replace the official WireGuard software. It
only checks that the tunnels you already have in WireGuard really carry
traffic, and, when you allow it, reconnects the one that failed.

## Rules for an agent (read first)

1. **Do only what this guide says**:
   - change nothing in the official WireGuard: no setting, key, tunnel or
     start at boot;
   - do not connect or disconnect a VPN by hand;
   - install no other package, and change no firewall, route or DNS.
2. **Do not work through the VPN you are about to watch**: an install stops
   and starts WGAL's own service for a moment. The scripts never touch a
   tunnel, but work on the computer itself, or over a connection that does
   not depend on that VPN.
3. **Ask the user first**:
   - which account uses WGAL (its window, or its command line `wgal`);
   - which tunnels that account may register and hand to automatic recovery
     (Windows and Ubuntu; macOS asks for none);
   - on Ubuntu, whether the computer has a desktop. Without one, install
     with `--no-window` (see "Ubuntu without a desktop").

   Do not guess. Do not ask the user to type tunnel names: list the
   computer's tunnels first with a command that needs no administrator
   rights and changes nothing (Windows `.\install.cmd -ListTunnels`,
   Ubuntu `./install.sh --list-tunnels`), let the user pick from that list,
   and then install with administrator rights **once**.
4. **Do not read or print secrets**: not the Telegram Bot Token, a
   WireGuard configuration's contents or a private key. The scripts do not
   read them either. If the user wants Telegram notifications, they type
   the Bot Token themselves, in the window or with `wgal notify token set`
   in their own terminal. Never pass a token on a command line.
5. **Check every exit code**: only 0 is success. On anything else, stop and
   give the user the script's messages word for word; do not "fix" things
   and try again.
   - If a message says the new version may already have run, do not put the
     old version back or start it. Only put the complete new version back as
     the message says, or leave everything as it is for the owner.
   - If a message says the record could not be written and so nothing was
     started (could not be marked, could not be written ... NOT started),
     leave the complete new version where it is and do not put the old one
     back. Remove the cause the message names (disk space, a read-only file
     system, another program holding the file open) and run the script
     again, or leave everything as it is for the owner.
   - If a message says the script did not start the watchdog (NOT started),
     it has also turned off WGAL's own start (Windows: the service set to
     Disabled; Ubuntu: `systemctl disable wgal-watchdog.service`; macOS:
     `launchctl disable` of the watchdog's label), so the operating system
     does not start that set at the next boot or login. Do what the message
     says and run the script again; a run that passes its checks turns the
     start back on.
   - If a message says that start could NOT be turned off, deal with it as
     the message says and run the script again before any restart or
     logout, or hand it to the owner as it is. Do not change the WGAL
     service's start setting yourself, and never touch any WireGuard
     tunnel's start at boot.
6. **Administrator rights: the user approves, once**: Windows' UAC prompt
   and the sudo password on Ubuntu and macOS are the user's to enter.
   Listing the tunnels needs no administrator rights, so the whole install
   needs the user's approval only once.
7. **When the user wants to click through it themselves**: on Windows, ask
   them to double-click `setup.cmd` in the unpacked folder; on Ubuntu, to
   run `./setup.sh`. Each asks for administrator rights once, lists the
   tunnels to tick and runs the same install script.
8. **The command line `wgal` acts as the user**: anything run as the
   account on the access list, an agent with a shell too, can do with
   `wgal` what the user can, including reading the Telegram chat ID and
   changing the Telegram settings. Use it only for what the user asked.
   Do not send a test message unless the user asks for one. `wgal` writes
   in the language the user chose for it; add `--lang en` to any command
   when you need its output in English, and do not change the user's
   language (`wgal language`) unless they ask.

## Where to get it

- **A release**: download the archive for the platform from the
  [Releases page](https://github.com/Hsinken/wireguard-always-live/releases),
  and check it against the SHA-256 published there. Then, after unpacking,
  check its files against `RELEASE.txt` (below). The first check is what
  tells you the download is genuine; `RELEASE.txt` only tells you the files
  match the build record.
- **From source**: build the archive on the computer it will be installed
  on, as "Build the release from source" at the end of this guide says,
  and install from it the same way.

## What the archive holds

| Platform | File | Contents |
|---|---|---|
| Windows 11 (x64) | `wgal-<version>-windows-x64.zip` | `wgal-watchdog.exe`, `wgal-desktop.exe`, `wgal-mcp.exe`, `wgal.exe`; `install`, `uninstall`, `revert` and `setup`, each as `.ps1` and `.cmd`; the guides; `RELEASE.txt` |
| macOS | `wgal-<version>-macos-<arm64 or x86_64>.zip` | `WG Always Live.app` (with `wgal-watchdog`, `wgal-mcp` and `wgal` inside); `install.sh`, `uninstall.sh`; the guides; `RELEASE.txt` |
| Ubuntu | `wgal-<version>-linux-x86_64.tar.gz` | `wgal-watchdog`, `wgal-desktop`, `wgal-mcp`, `wgal`, `wgal-watchdog.service`, `wgal.desktop`, `wgal.png`; `install.sh`, `uninstall.sh`, `setup.sh`; the guides; `RELEASE.txt` |

The programs:
- `wgal-watchdog`: the background service that probes and recovers the
  tunnels and sends the Telegram messages;
- `wgal-desktop`: the window (the user's own account, no privileges);
- `wgal-mcp`: the MCP server AI agents use; off until the user turns it on;
- `wgal`: the command line, for everything the window does with the
  watchdog (from 3.0.0); English by default, in 13 languages from 4.0.0.

`RELEASE.txt` gives:
- the version: since 4.2.1 one for the whole set (the window, the watchdog,
  `wgal-mcp` and `wgal`), the one in the file's name;
- the commit it was built from;
- the SHA-256 of every file (on macOS: of the scripts, the guides and the
  programs in the app's `Contents/MacOS`).

After unpacking you can check the sums: `Get-FileHash` on Windows,
`shasum -a 256` on macOS, `sha256sum` on Ubuntu.

The files are not code-signed (owner decision of 2026-11-05):
- Windows may show a SmartScreen warning;
- on macOS the install script removes the download quarantine flag from
  this app only; see the macOS section.

## Before you start

- **The official WireGuard is installed and has the tunnels to watch**:
  - Windows: WireGuard for Windows. Only an activated tunnel has a service
    to register.
  - macOS: WireGuard from the Mac App Store.
  - Ubuntu: `wireguard-tools`, with each configuration in
    `/etc/wireguard/<name>.conf`. The WGAL window also needs the Ubuntu
    packages `libwebkit2gtk-4.1-0` (without it the window does not open)
    and `libayatana-appindicator3-1` (the icon at the top right); when they
    are missing the install script asks whether to install them with
    `apt-get`, see the Ubuntu section. Without a desktop, none of this is
    needed: see "Ubuntu without a desktop".
- **Quit the WGAL window first** (to upgrade or remove): the tray or menu
  bar icon's menu → "Quit interface (watchdog keeps running)". Let any
  `wgal` command finish, and close an agent that uses `wgal-mcp`.

---

## Windows 11

### Install or upgrade

**The user clicks through it (recommended)**: unpack the zip into a local
folder and double-click `setup.cmd`. Windows asks once for administrator
rights; then a small window lists the **activated** tunnels. Tick the ones
to watch and press "Install" (on an installed computer the button is
"Upgrade", with the current list kept by default). The last, black window
shows how the install went; press Enter to close it. `setup.cmd -DryRun`
only shows the window and the command it would run, and changes nothing.

**An agent installs it**:

1. Unpack the zip into a local folder, for example
   `C:\Users\<you>\wgal-<version>-windows-x64`.
2. In that folder, list the tunnels from an ordinary PowerShell (no
   administrator rights needed):

```powershell
.\install.cmd -ListTunnels
```

   It prints one line of JSON and changes nothing:
   `{"complete":false,"tunnels":[{"name":"office","status":"running","starts_at_boot":true,"accepted":true}]}`.
   It lists only activated tunnels (a tunnel that was never activated has
   no service, so `complete` is false). A name with `accepted` false is
   one WGAL does not take and cannot be picked. Let the user pick from the
   list.
3. Run one of these **as administrator** (for example started with
   `Start-Process -Verb RunAs`, which the user approves once in UAC):

```bat
.\install.cmd -Tunnel <tunnel name>[,<tunnel name>...]
```

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Tunnel <tunnel name>[,<tunnel name>...]
```

4. In PowerShell always write `.\install.cmd`, with the `.\`: PowerShell
   does not run a file from the current folder, and a bare `install.cmd`
   is looked up on the PATH, where another tool's script of that name may
   run instead (nvm-windows' `install.cmd`, for one, asks for a path and
   changes the system's PATH). If a question appears that is not WGAL's,
   press Ctrl+C and type nothing. `.\` works in Command Prompt too. To
   change folders, use `cd <folder>` in PowerShell and `cd /d <folder>` in
   Command Prompt.

Options:
- Several tunnels are separated by commas: `-Tunnel office,home`.
- When the elevation uses another administrator account, add
  `-User <SID of the account that uses WGAL>`. That account sees its SID
  with `whoami /user` in its own window.
- On an upgrade, leave `-Tunnel` out to keep the access list as it is.

Exit codes:
- 0: done;
- 1: the service does not run with its pipe; or a check before the service
  was stopped failed, and the script stopped with a message. If the
  message says the copies an earlier upgrade left (`*.exe.old`,
  `*.exe.new`, `upgrade.record`) cannot be confirmed, the script stopped
  no service and changed no file; give the user the message word for word
  (rule 5);
- 2: a WireGuard service changed while the script ran;
- 3: another process holds the pipe's name;
- 4: an upgrade step after the service was stopped failed; the script
  started the service again with the complete set of programs that was in
  place, and lines starting with `!!!` say whether that is the old or the
  new version and which access list is in effect (only when 1, 2 and 3 do
  not apply). Give the user the message word for word (rule 5) and run
  the script again once the cause is fixed.

### Check

- `sc.exe query WGALWatchdog` shows `RUNNING`.
- From an **ordinary (not elevated)** session, open
  `C:\Program Files\WGAL\wgal-desktop.exe`; the top of the window shows
  "Watchdog OK".
- Or, from an ordinary PowerShell as that account:
  `& 'C:\Program Files\WGAL\wgal.exe' --lang en status`. Its first line
  starts with `WGAL watchdog <version>` (`--lang en`: in English, whatever
  language was chosen for `wgal`). No script changes the PATH, so `wgal.exe` is
  run by its full path.

### Remove

Run as administrator:

```bat
.\uninstall.cmd
```

- Add `-RemoveData` to delete WGAL's data too.
- If the service hangs and does not stop, ask the user first, then run
  `.\revert.cmd -Tunnel <registered tunnel>[,...]`: it ends the service
  and starts again those of the listed tunnels that are stopped. Then run
  `.\uninstall.cmd` again.

---

## Ubuntu

This section is for a computer with a desktop. Without one, follow
"Ubuntu without a desktop" below.

### Install or upgrade

**The user clicks through it (recommended)**: in the unpacked folder, run
`./setup.sh` (without sudo: it asks for the sudo password once itself). It
lists the tunnels in a check list (space ticks, Enter confirms) and then
installs; on an installed computer it first asks whether to keep the
current list. On a computer without a graphical desktop it asks whether to
install without the window. `./setup.sh --dry-run` only shows the choices
and the command it would run, and changes nothing.

**An agent installs it**:

1. Unpack: `tar -xzf wgal-<version>-linux-x86_64.tar.gz && cd wgal-<version>-linux-x86_64`
2. List the tunnels: `./install.sh --list-tunnels`. It prints JSON, one
   tunnel per line, and changes nothing. An ordinary user cannot read
   `/etc/wireguard`, so it lists only the tunnels systemd knows and the
   running ones (`complete` is false); for the full list, run
   `sudo ./install.sh --list-tunnels` with the same sudo session as the
   install that follows. Let the user pick from the list.
3. Run:

```bash
sudo ./install.sh --tunnel <interface>[,<interface>...]
```

Options:
- The account that uses WGAL is by default the one that ran sudo; add
  `--user <account>` for another one. It cannot be root.
- When there is an access list already (an upgrade, or a reinstall after a
  removal that kept the data), leaving `--tunnel` and `--user` out keeps it
  as it is.
- When the window's packages (`libwebkit2gtk-4.1-0`,
  `libayatana-appindicator3-1`) are missing: run in a terminal, the script
  first asks whether to install them with `apt-get`. An agent asks the
  user, and adds `--install-runtime` only when the user agrees. Without
  agreement or the option, the script stops before it changes anything and
  prints the `sudo apt install ...` to run. It installs only those of the
  two packages that are missing, and removing WGAL does not remove them.

Exit codes:
- 0: done;
- 1: the service does not run with its socket;
- 2: a wg-quick unit changed while the script ran, or their states could
  not be read at the end (2 wins over 1);
- 4: an upgrade step after the service was stopped failed; the script
  started the service again with the complete set that was in place (old
  or new, as the lines starting with `!!!` say), only when 1 and 2 do not
  apply;
- 64: wrong usage;
- 65: a check or preparation before the service was stopped failed
  (systemctl not answering included, or copies an unfinished earlier
  upgrade left that cannot be confirmed to leave a complete set in place).
  The service and the WGAL files in use are as they were, but the prepared
  copies in `/opt/wgal` (`.<name>.new`, `.<name>.old`, `.upgrade.record`),
  the menu and login entries, `/opt/wgal/uninstall.sh`, the prepared
  access list in `/etc/wgal`, or the window's packages the user agreed to,
  may already have been made or replaced. Give the user the message word
  for word (rule 5).

What it installs:

| Where | What |
|---|---|
| `/opt/wgal/` | the four programs, the icon, `uninstall.sh`, the READMEs |
| `/usr/local/bin/wgal` | a link to `/opt/wgal/wgal`, made only where nothing is there or the link is already WGAL's; otherwise the closing lines give the full path |
| `/etc/systemd/system/wgal-watchdog.service` | the watchdog service, run as root, started at boot |
| `/etc/wgal/access.conf` | the access list |
| `/var/lib/wgal/` | the data |
| `/run/wgal/watchdog.sock` | the local socket |
| the applications menu and start at login | the window |

### Check

- `systemctl is-active wgal-watchdog` shows `active`.
- `journalctl -u wgal-watchdog -n 20` has a `running` line.
- As the user (**not with sudo**), open WG Always Live from the
  applications menu; the top of the window shows "Watchdog OK". Or run `wgal --lang en status`: its first
  line starts with `WGAL watchdog <version>`.

### Remove

```bash
sudo /opt/wgal/uninstall.sh
```

- Add `--remove-data` to delete `/var/lib/wgal` and `/etc/wgal` too.
- It removes `/usr/local/bin/wgal` only while it is WGAL's link.
- To stop WGAL without removing it: `sudo systemctl disable --now wgal-watchdog`.

---

## Ubuntu without a desktop (a server)

Without a desktop, WGAL is installed without its window and used with its
command line, `wgal` (supported from 3.0.0). The watchdog is the same: it
probes, recovers and sends Telegram messages on its own.

### Install or upgrade

**The user**: in the unpacked folder, run `./setup.sh` (without sudo). On
a computer without a graphical desktop it asks whether to install without
the window; the answer is yes by default, and it then passes `--no-window`.

**An agent**:

1. Unpack, and list the tunnels, as in the Ubuntu section (steps 1 and 2).
2. Run:

```bash
sudo ./install.sh --no-window --tunnel <interface>[,<interface>...]
```

`--no-window`:
- does not check or install the window's packages;
- installs no menu or start-at-login entry, and removes WGAL's own ones
  from an earlier install;
- still copies the window's program into `/opt/wgal`, so the set of
  programs stays the same; nothing opens it;
- is **not kept**: every run follows its own options. Give `--no-window`
  again on every upgrade: `sudo ./install.sh --no-window`. Without it, the
  run takes the desktop path: it checks for the window's packages and, when
  they are missing, asks in a terminal or stops with 65 (nothing changed);
- does not go with `--install-runtime` (exit 64).

The account that uses `wgal` is the one that ran sudo, or `--user
<account>`; it cannot be root. Log in to the server as that account, or
give `--user`. `--tunnel`, `--user`, the exit codes and what it installs
are as in the Ubuntu section, without the menu and login entries.

### Check

- `systemctl is-active wgal-watchdog` shows `active`.
- `journalctl -u wgal-watchdog -n 20` has a `running` line.
- As the account on the access list (**not with sudo**): `wgal --lang en
  status`. Its first line starts with `WGAL watchdog <version>`. When `/usr/local/bin/wgal`
  could not be linked, the install's closing lines say so; use
  `/opt/wgal/wgal` instead.

Then set WGAL up as in "Set WGAL up with the command line" below.

### Remove

As in the Ubuntu section: `sudo /opt/wgal/uninstall.sh`, with
`--remove-data` to delete the data too.

---

## macOS

### Install or upgrade

1. Unpack: double-click the zip in Finder, or run
   `ditto -x -k wgal-<version>-macos-arm64.zip .`
2. Logged in to the Mac as yourself, open Terminal in that folder and run,
   **as yourself** (**not with sudo**):

```bash
./install.sh
```

- The script copies the app into `/Applications`. When it cannot write
  there, it asks for an administrator password with sudo.
- The app has no Apple Developer signature. The script removes the
  download quarantine flag from this app only; it does not change the
  system's Gatekeeper settings.

Exit codes:
- 0: done;
- 1: the watchdog does not run with its socket;
- 2: a VPN configuration changed while the script ran;
- 4: an upgrade step after the watchdog was stopped failed; the script
  started the watchdog again with the complete app that was in place (as
  the lines starting with `!!!` say), only when 1 and 2 do not apply;
- 64: wrong usage;
- 65: a check or preparation before the watchdog was stopped failed
  (including an old app `.WG Always Live.app.old`, left by an unfinished
  earlier upgrade, that cannot be confirmed safe to delete). The watchdog
  and the app in place are as they were, but
  `~/Library/Application Support/WGAL` (with its `uninstall.sh` and
  `upgrade/`), `~/Library/Logs/WGAL` and the hidden
  `/Applications/.WG Always Live.app.new` may already have been made or
  replaced. Give the user the message word for word (rule 5).

On an upgrade the script also backs up the state to
`~/Library/Application Support/WGAL/backup/state-<time>` (the newest five
are kept).

What it installs:

| Where | What |
|---|---|
| `/Applications/WG Always Live.app` | the window, with `wgal-watchdog`, `wgal-mcp` and `wgal` in `Contents/MacOS/` |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.watchdog.plist` | the watchdog, started at login as you |
| `~/Library/LaunchAgents/io.github.hsinken.wg-always-live.window.plist` | opens the window at login |
| `~/Library/Application Support/WGAL/` | the data, the socket and `uninstall.sh` |
| `~/Library/Logs/WGAL/watchdog.log` | the log |
| the login keychain | the Bot Token |

### Check

- `launchctl print gui/$(id -u)/io.github.hsinken.wg-always-live.watchdog | grep state` shows `running`.
- The top of the window shows "Watchdog OK".
- Or, as yourself: `"/Applications/WG Always Live.app/Contents/MacOS/wgal" --lang en status`
  (with the quotes). Its first line starts with `WGAL watchdog <version>`. No script
  changes the PATH.

### Remove

As yourself (**not with sudo**). The install copied the removal script
into WGAL's folder:

```bash
"$HOME/Library/Application Support/WGAL/uninstall.sh"
```

Add `--remove-data` to delete the data, the log and the Bot Token in the
keychain too.

---

## Set WGAL up with the command line

On a desktop the window does all of this. Without one, or when you prefer
a terminal, `wgal` does the same: it sends the watchdog the same requests
the window sends, and nothing else. Run it as the account that uses WGAL,
not elevated and not with sudo (an elevated administrator or root is not
held to the access list's tunnels). It writes in English unless a
language is chosen (see "Languages" below).

| Platform | Run it as |
|---|---|
| Windows | `& 'C:\Program Files\WGAL\wgal.exe' <command>` in PowerShell, `"C:\Program Files\WGAL\wgal.exe" <command>` in Command Prompt |
| Ubuntu | `wgal <command>`, or `/opt/wgal/wgal <command>` |
| macOS | `"/Applications/WG Always Live.app/Contents/MacOS/wgal" <command>` |

`wgal help` lists the commands and `wgal help <command>` explains one. The
examples below write `wgal`.

### Watch a tunnel

1. `wgal discover`: the tunnels the official WireGuard manager has, with
   the ID WGAL gives each (the first column), and which are registered.
2. `wgal register <tunnel>`: start watching it. On Windows and Ubuntu only
   a tunnel on the access list can be registered (an administrator can
   register any); on macOS, which has no access list, any of yours.
3. `wgal set <tunnel> --recovery-target <address>`: the target the probes
   must reach through that tunnel. It is an IPv4 address (ICMP), or
   `address:port` (a TCP connect), that answers only through the tunnel,
   such as a host on the other side. For now only an address (ICMP) works,
   on every system: WGAL cannot yet bind a TCP probe to a tunnel, so a TCP
   target is probed but never shows that traffic goes through the tunnel.
   `--observe <address>` (up to 8 times;
   it replaces the list) sets targets that are only probed and shown: they
   never decide a fault or a recovery. `--interval`, `--timeout`,
   `--retries` and `--min-fault` tune the probes; `wgal help set` gives the
   ranges.
4. `wgal probe <tunnel>`: probe the targets now and see what answered.
5. `wgal auto-recovery <tunnel> on`: let WGAL restart the tunnel when
   traffic stops. As with registering, on Windows and Ubuntu only a tunnel
   on the access list can be handed over. It turns on once the recovery target
   has answered a probe through the tunnel (within ten minutes; otherwise
   it stays off), and `wgal` says so.
6. Optional: `wgal start-when-stopped <tunnel> on` also starts the tunnel
   again when something other than WGAL stopped it; `wgal retry <tunnel>
   --limit <N>|forever --interval <S>` sets how often recovery tries again.

`wgal status` shows the watchdog and every tunnel, one line each;
`wgal show <tunnel>` one tunnel in full; `wgal log` the event log.
`wgal monitor <tunnel> off` and `wgal pause` stop the probes (one tunnel,
or all) without forgetting anything; `wgal unregister <tunnel> --yes`
makes WGAL forget a tunnel, and never touches the tunnel itself.

### Telegram notifications

The user does these, in their own terminal (rule 4 for agents):

1. `wgal notify device "<name>"`: the name the messages give this
   computer. While none is set (or only spaces), since 4.1.0 the messages
   give the computer's host name, which `wgal notify` shows; `wgal notify
   device ""` goes back to it.
2. `wgal notify chat <chat ID>`: the Telegram chat to write to.
3. `wgal notify token set`: asks for the Bot Token without showing it
   (or reads one line of standard input when that is not a terminal). It
   never takes the token from the command line or the environment. If
   `wgal` is killed while it asks and the terminal stops showing what you
   type, `stty echo` brings it back on Linux and macOS.
4. `wgal notify on`, then `wgal notify test` to send a test message.

The messages are in English unless a language is chosen: `wgal language
<tag>` chooses one for `wgal` and the messages together (see "Languages"),
and `wgal notify language <tag>` for the messages only. On a computer with
the window, the window hands over its own language while the watchdog
still uses its built-in English texts; a language chosen with `wgal` stays
until someone chooses another, in the window or with `wgal`.

Times in the messages are in the computer's time zone, which since 4.1.0
the watchdog reads again right before each message: a change of the
system's zone shows in the next message on Ubuntu and macOS, and within
five minutes on Windows. `wgal host --time-zone <name>` (an IANA name such
as `Asia/Taipei`) makes WGAL use that zone whatever the system's is, and
`wgal host --time-zone system` follows the system's again.

### Languages

`wgal` and the Telegram messages come in 13 languages: `zh-TW`, `en`,
`zh-CN`, `ja`, `ko`, `de`, `es`, `pt-BR`, `tr`, `fr`, `ru`, `id` and `vi`.
Both are English until a language is chosen.

- `wgal language` shows the language `wgal` writes in, the one the
  messages use, and the 13 to choose from.
- `wgal language <tag>` (for example `wgal language zh-TW`) makes `wgal`
  write in that language from then on, and writes the messages in it too.
  The watchdog keeps it with the notification settings, so it holds for
  every account that uses that watchdog: on Windows and Ubuntu, every
  account on the computer; on macOS, where each user has their own
  watchdog, only that user. The window never changes it; choosing the
  window's language there changes only the messages'.
- `wgal --lang <tag> <command>` writes in another language for that one
  command; `--lang en` for English.
- `wgal notify language <tag>` changes only the messages' language.
- `wgal language --clear` goes back to English for `wgal` (the messages
  keep theirs). Do this before going back to a version before 4.0.0: an
  older watchdog cannot read the notification settings while they hold a
  language for `wgal`, and keeps notifications off until they are fixed.

Commands, options and values (`on`, `off`, `--yes`, the tunnel IDs) are
the same in every language; type them as shown here.

### Move the settings to another computer

`wgal export > wgal-settings.json` writes the settings file (without the
device name, chat ID or Bot Token). On the other computer,
`wgal import --dry-run < wgal-settings.json` shows what would change and
`wgal import --yes < wgal-settings.json` applies it; `wgal help import`
gives the options.

On Windows, run these two in Command Prompt (`cmd.exe`), not PowerShell:
PowerShell has no `<`, and Windows PowerShell's `>` writes the file as
UTF-16, which `wgal import` does not read.

```bat
"C:\Program Files\WGAL\wgal.exe" export > wgal-settings.json
"C:\Program Files\WGAL\wgal.exe" import --dry-run < wgal-settings.json
"C:\Program Files\WGAL\wgal.exe" import --yes < wgal-settings.json
```

### Exit codes

0 done; 1 refused, or done with a failed result; 2 a mistake in the
arguments or the input (nothing changed); 3 the watchdog could not be
reached (nothing changed); 4 it may have been carried out, but that is not
confirmed. To say even a mistake in its language, `wgal` reads the
watchdog's state once first unless `--lang` is given; that changes
nothing. Add `--json` for one JSON object on standard output instead of
text; its keys and `kind` values are the same in every language.

## Let an agent use WGAL (MCP)

`wgal-mcp` lets an AI agent use WGAL with the same rights as the window, on
this installation's access list. It is a stdio MCP server: the agent starts
it. It is off until the user turns it on.

1. **The user turns MCP on**, in the window's MCP tab or with `wgal mcp on`
   in their own terminal, typing yes. An agent must never do this, or
   answer that prompt, for them. (The command also refuses, changing
   nothing, when it is not run in an interactive terminal; that is a
   confirmation step, not proof that a person typed it.) While MCP is
   off, `wgal-mcp` answers nothing.
2. **Add `wgal-mcp` to the agent's MCP settings.** The window's MCP tab can
   do it, after the user confirms, for Claude Code, Claude Desktop, Cursor,
   VS Code, Trae, Kiro, Codex, Antigravity and Gemini CLI. By hand, add a
   server named `wgal` that runs `wgal-mcp` by its full path, with no
   arguments:

   | Platform | Path |
   |---|---|
   | Windows | `C:\Program Files\WGAL\wgal-mcp.exe` |
   | Ubuntu | `/opt/wgal/wgal-mcp` |
   | macOS | `/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp` |

3. **What the agent can and cannot do**:
   - It can do what the user can do in the window, within the access list.
   - It never gets the Bot Token.
   - It cannot change the device name, the chat ID or the Bot Token, or
     read the chat ID, unless the user also turns on letting agents set
     Telegram (`wgal mcp telegram on`, or the same tab).
   - Turning MCP on is not permission to switch tunnels: connect,
     disconnect, reconnect and turning automatic recovery on are done only
     when the user asks.

The tools are listed in `docs/agents.md` in the source repository, and by
the server itself (`tools/list`).

---

## Build the release from source (on each platform)

Each release is built on its own platform. Windows cannot build the window
for macOS or Ubuntu.

To build you need:
- the versions in AGENTS.md "Toolchain": Rust 1.99.1, Node.js 24.16.0,
  pnpm 11.10.0;
- the platform's packages: Ubuntu needs `libwebkit2gtk-4.1-dev` and the
  others listed there, macOS the Xcode Command Line Tools.

The working tree must be clean. From the repository root:

| Platform | Command | Result |
|---|---|---|
| Windows | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\make-release.ps1` | `dist\wgal-<version>-windows-x64.zip` |
| Ubuntu | `packaging/linux/make-release.sh` | `dist/wgal-<version>-linux-<arch>.tar.gz` |
| macOS | `packaging/macos/make-release.sh` | `dist/wgal-<version>-macos-<arch>.zip` |

The build scripts write only `target/` and `dist/` and install nothing.
Once built, install as above.

## What the scripts never do

- Start, stop, change or delete any official WireGuard tunnel, service,
  configuration or start at boot. The one exception is Windows' emergency
  revert: `revert.cmd` or `revert.ps1 -Tunnel <name>` ends the WGAL
  service and starts again those of the listed tunnels that are stopped
  (see "Emergency revert" in the Windows `README.md`). An agent never runs
  it on its own: it first needs the user's clear agreement for those
  tunnels.
- Read a WireGuard configuration's contents, a private key or the Bot
  Token.
- Change the firewall, routes, DNS or MTU, or install or remove other
  packages. The one exception: Ubuntu's `install.sh`, when the user agrees,
  installs `libwebkit2gtk-4.1-0` and `libayatana-appindicator3-1`, which
  the WGAL window needs and the system lacks (it never removes them).
- Change the PATH. Ubuntu's `install.sh` only links `/usr/local/bin/wgal`
  where nothing is there (or keeps WGAL's own link), and `uninstall.sh`
  removes only that link.
- Add or remove anything but WGAL's own files, service or LaunchAgents and
  WGAL's data. The data is kept by default; it is deleted only when removal
  is explicitly asked to.
