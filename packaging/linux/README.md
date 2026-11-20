# WGAL on Ubuntu: install, upgrade and removal

**English** | [繁體中文](README.zh-TW.md)

The steps to install, check and remove WGAL are in `INSTALL.md`. This file
says what the scripts do, where the files are and where to look when
something goes wrong.

> **Status (2026-11-20)**:
> - Built natively on Ubuntu 24.04 in WSL; all tests and shellcheck pass.
>   `install.sh`, `uninstall.sh` and the systemd service were installed,
>   upgraded and removed for real in WSL.
> - On 2026-11-12, 2.2.0 was installed with `setup.sh` on an Ubuntu 26.04
>   test computer with a desktop. Once the window's packages were added,
>   the window started in the desktop session without errors and its tray
>   item registered; opening it from the applications menu was not
>   confirmed. It was then reinstalled as 2.2.1, and on 2026-11-13 an
>   upgrade test ran as root there, one run with a failure injected after
>   the service stopped: PASS.
> - On 2026-11-19, 4.0.0, 4.1.0 and then 4.2.1 were installed with
>   `--no-window` on an Ubuntu Server 26.04 with one `wg-quick` tunnel,
>   and tested there with 4.1.0 and 4.2.1 through the watchdog: bound
>   probes, a verified disconnect, connect and reconnect, turning automatic
>   recovery on, and the settings kept across a restart of the watchdog and
>   a reboot ([docs/testing.md](../../docs/testing.md)).
> - On 2026-11-20, 4.3.1 from the Releases page (built by GitHub Actions)
>   was installed as an upgrade on an Ubuntu Server 26.04 (`--no-window`,
>   from 4.2.1: every setting kept, green, a verified disconnect, connect
>   and reconnect) and on Ubuntu 26.04 with a desktop (`setup.sh`, from
>   2.3.1: the watchdog runs, the window runs in the desktop session, the
>   menu and autostart entries are installed, a tunnel is verified; checked
>   read only).
> - These are "installed on a device", not "accepted on a device".
> - **Not done yet** (NOT RUN): with a desktop, opening the window from the
>   menu and its tray item; two tunnels; a real fault that automatic
>   recovery fails to fix.

## How it works

- **The watchdog**: `wgal-watchdog.service`, run by systemd as root and
  started at boot. It controls tunnels with
  `systemctl start/stop wg-quick@<interface>` and probes through each
  tunnel. It never changes a WireGuard configuration, and never changes
  whether a `wg-quick@` unit starts at boot (`enable`/`disable`).
- **Probes that count**: a probe is a ping bound to the tunnel's
  interface (`ping -I`), sent only when both route lookups name that
  interface and its address. Before 4.2.0 Ubuntu did not count such a
  probe as passing through the tunnel (it was not yet proven on a
  device), so the light never turned green, a connect or reconnect ended
  as `control_failed`, and automatic recovery could not be turned on.
  Since 4.2.0 it counts (on-device tests of 2026-11-19 on one tunnel,
  [docs/testing.md](../../docs/testing.md); after the 4.2.1 upgrade the
  light turned green, a connect and a reconnect were verified, and the
  settings were kept across the watchdog's restart and across a reboot).
  Not part of those tests: two tunnels with overlapping routes, and a real
  fault that automatic recovery fails to fix (the alert, retries and the
  "recovered" message); they are still unproven on Ubuntu. If the tunnel
  itself fails to come up at boot, WGAL does not start it: it acts only on
  a tunnel it has seen connected. Only an address (ICMP) works as a target: a TCP
  probe cannot be bound to a tunnel yet.
- **The window**: runs as a normal user and talks to the watchdog over
  `/run/wgal/watchdog.sock`. It holds no privileges itself, and connects
  only to a socket root owns.
- **The command line, `wgal`**: WGAL's command line, in English or in the
  language chosen with `wgal language <tag>` (since 4.0.0),
  `/opt/wgal/wgal`. Like the window, it connects only to the socket root
  owns and only sends requests; it holds no privileges itself.
  - Run it as the account on the access list, **not with sudo**: root is
    an administrator and is not held to the access list's tunnels.
  - On a computer without a desktop (`--no-window`) it is the only way to
    use WGAL. `wgal --help` lists every command.
  - `wgal` counts as the user (owner decision of 2026-11-18): any program
    that runs it as that account, an agent with a shell too, can do what
    the user can, including reading the chat ID and changing the Telegram
    settings. The MCP switches limit only `wgal-mcp`.
- **Who the watchdog answers**: root, and the accounts on the access list.
  A request from any other account is refused as "not on this computer's
  access list", and the window shows that.
- **Secrets**: the bot token is kept in
  `/var/lib/wgal/notify/telegram-bot-token`, mode 0600, in a 0700 folder.
  That is protection by permissions, not encryption. The token
  is set in the window or with `wgal notify token set`; `wgal` never takes
  a token from the command line or the environment.

## Files

| Path | Contents | Written by |
|---|---|---|
| `/opt/wgal/` | `wgal-watchdog`, `wgal-desktop`, `wgal-mcp`, `wgal`, `wgal.png`, `uninstall.sh`, `README.md`, `README.zh-TW.md`; during an install or upgrade also `.<name>.new`, `.<name>.old` and `.upgrade.record`, removed when it is done | the install script |
| `/usr/local/bin/wgal` | a link to `/opt/wgal/wgal`, so that `wgal` can be typed as it is. Made only where nothing is there, or kept when it already is this link; anything else there is left alone, and the closing lines give the full path `/opt/wgal/wgal` instead | the install script; `uninstall.sh` removes it only while it still points to `/opt/wgal/wgal` |
| `/etc/systemd/system/wgal-watchdog.service` | the service unit | the install script |
| `/etc/wgal/access.conf` | the access list: `user <UID>` and `tunnel <interface>`, one per line | the install script |
| `/var/lib/wgal/` (0700) | `state`, `notify`, `events`, and the backups made before upgrades, `backup/state-<time>` | the watchdog (`StateDirectory`) |
| `/run/wgal/` (0755) | `watchdog.sock` (0666) | the watchdog (`RuntimeDirectory`) |
| `/usr/share/applications/wgal.desktop`, `/etc/xdg/autostart/wgal.desktop` | the applications menu entry and the start at login. `--no-window` does not install them, and removes the ones WGAL installed before | the install script |

## The access list

- Only root can change `access.conf`. The watchdog reads it only when it
  starts; after a change, run `sudo systemctl restart wgal-watchdog`.
- When the file or `/etc/wgal` is not root's alone, is a link or is not in
  the right format, none of it is used: only root can use WGAL, and the
  reason is written to the journal (only the line number and the problem,
  never the contents).
- To allow another tunnel, run `sudo ./install.sh --tunnel <every interface
  to allow>` again (on a computer without a desktop, add `--no-window`).
  This option rewrites the whole list.

## Installing from a check list (owner request of 2026-11-12)

- **For people**: in the unpacked folder, run `./setup.sh` from your own
  account.
  - sudo asks for your password once. It then lists this computer's
    wg-quick tunnels (what `install.sh --list-tunnels` prints) in a
    whiptail check list (Space ticks, Enter confirms). You tick the
    tunnels to hand to WGAL, and it runs `install.sh` with them and with
    the account that ran it (`SUDO_USER`). Without whiptail it shows a
    numbered list instead.
  - When WGAL is already installed, it first asks whether to keep the
    current list: keeping it upgrades as it is (no `--tunnel`); choosing
    again replaces the whole list. Names WGAL does not accept are listed
    but cannot be ticked.
  - Without a graphical desktop (see "A computer without a desktop"
    below), it first asks whether to install without the window.
  - `./setup.sh --dry-run` needs no sudo: it only prints the command it
    would run. It speaks Traditional Chinese when the system language is
    Chinese, and English otherwise.
- **For agents**: `./install.sh --list-tunnels` prints JSON and changes
  nothing:
  `{"complete":true,"tunnels":[{"name":"wg0","status":"running","starts_at_boot":true,"accepted":true}]}`,
  one tunnel per line.
  - Run as root, it lists every `/etc/wireguard/<name>.conf` (`complete`
    is true). A normal user cannot read that folder, so it lists the
    wg-quick units systemd knows and the WireGuard interfaces that are
    running (`complete` is false). It reads only names, never a
    configuration's contents.
  - The agent lets the user choose from the list, then runs, with one
    sudo, `sudo ./install.sh --tunnel <the chosen interfaces>` (add
    `--no-window` on a computer without a desktop). `accepted` looks only
    at the name; `--tunnel` also needs the matching `.conf`.
- Tested on Ubuntu 24.04 in WSL: `--list-tunnels` as a normal user; the
  full listing with a scratch folder standing in for `/etc/wireguard`
  (links and names WGAL does not accept are left out); the numbered list
  of `setup.sh --dry-run`, in Chinese and in English.
- The Ubuntu packages the window needs: `libwebkit2gtk-4.1-0` (the window;
  it does not open without it) and `libayatana-appindicator3-1` (the icon
  at the top right).
  - `install.sh` checks for them before it changes anything (not with
    `--no-window`).
  - When some are missing, it asks in a terminal, or follows
    `--install-runtime` (which `setup.sh` adds after asking in whiptail),
    and installs only the missing ones with `apt-get`. Without a yes it
    stops with 65, prints `sudo apt install …`, and says that a computer
    without a desktop can use `--no-window` (owner decision of 2026-11-12:
    on the Ubuntu 26.04 test machine the window did not open because
    WebKitGTK was missing).
  - Removing WGAL does not remove them.
- Tested on 2026-11-12 on a real Ubuntu 26.04 machine, as a normal user
  over SSH (PASS, no sudo needed): `--list-tunnels` (`complete` false,
  listing the running tunnel); extra arguments refused (64); the whiptail
  check list of `setup.sh --dry-run`: "install" after ticking prints the
  command it would run (0), "install" with nothing ticked is refused (10),
  "cancel" (10), in Chinese and in English. Listing as root and a real
  install with `setup.sh`: see the status above.

## A computer without a desktop (`--no-window`, owner decision of 2026-11-18)

Ubuntu without a desktop, such as a server, is supported too. WGAL is
then used only with its command line, `wgal`:

```bash
sudo ./install.sh --no-window --tunnel <interface>[,<interface>...] [--user <account>]
```

- It neither checks nor installs the window's packages. It installs no
  applications menu entry and no start at login, and removes the ones
  WGAL installed before (they would open a window this computer cannot
  show).
- `wgal-desktop` is still copied into `/opt/wgal` (the set of programs
  stays the same), but nothing opens it.
- It does not go with `--install-runtime` (64, nothing changed).
- **It is not remembered**: each run follows its own options. On a
  computer without a desktop, give `--no-window` again on every upgrade.
  Without it the run takes the window's path: it checks the window's
  packages (when some are missing it asks in a terminal, and outside one
  stops with 65), and it puts the menu entry and the start at login back.
- `setup.sh` asks about it when there is no graphical desktop:
  `graphical.target` is not active, and neither `DISPLAY` nor
  `WAYLAND_DISPLAY` is set. The default is no window; a yes passes
  `--no-window`, and the window's packages are not asked about.
- On success the closing lines say:
  `Done, without the window. As <account> (not with sudo): "wgal status" shows WGAL, "wgal --help" the rest.`
  (When `/usr/local/bin/wgal` could not be made, `wgal` is replaced by
  the full path.)
- Then, as the account on the access list (**not with sudo**), run
  `wgal status` to check, and set WGAL up with `wgal discover`,
  `wgal register`, `wgal set <tunnel> --recovery-target <address>`,
  `wgal auto-recovery` and `wgal notify`. The steps are in `INSTALL.md`.

## First install

When there is no `/etc/systemd/system/wgal-watchdog.service` yet, it is a
first install. It prepares as an upgrade does (below: the copies and
`.upgrade.record`). It then puts the access list, the programs and the
unit in place, runs `daemon-reload` and `enable`, and writes
`started yes` into `.upgrade.record` and reads it back. Only then does it
start the service and check its socket.

If any step before that fails (Ctrl-C included), the script does **not**
start the service: it first turns WGAL's own start at boot off
(`systemctl disable wgal-watchdog.service`, then checks), keeps the copies
and the record, and ends with 1; the `!!!` message names the step that
failed. Fix the cause and run it again.

## Upgrades, and failures along the way

When `/etc/systemd/system/wgal-watchdog.service` exists, `install.sh`
upgrades.

Before the service stops, it does everything it can:
- The four programs (`wgal-watchdog`, `wgal-desktop`, `wgal-mcp`, `wgal`)
  and the unit are copied to `/opt/wgal/.<name>.new`, and the ones in
  place to `/opt/wgal/.<name>.old`. Each copy is checked by its SHA-256,
  and the sums are recorded in `/opt/wgal/.upgrade.record`, whose last
  line is `started no`.
- When upgrading from a version before 3.0.0, the set in place has no
  `wgal`: its previous sum in the record is `-`. A record an earlier run
  left without `wgal` is read the same way.
- The menu entry and the start at login (removed instead, with
  `--no-window`), the icon, `uninstall.sh` and both READMEs are replaced.
- With `--tunnel`/`--user`, the new access list is first written whole as
  a new file in `/etc/wgal`.

Then it stops the service, backs up the stored state, moves the access
list into place with one rename (the old one is never removed first),
moves the four programs into place, writes the unit, runs `daemon-reload`
and `enable`, and finally starts the service and checks its socket. On
success it removes `.new`, `.old` and `.upgrade.record`. Only once the new
set is in place and its start has begun does it make the
`/usr/local/bin/wgal` link (see "Files"; a first install too); a run that
ends in the trap below makes no link.

Copies `.new`/`.old` that an earlier run left are removed and made again
only after that run's `.upgrade.record` confirms that what is in place is
the complete previous set or the complete new set it recorded (or that
nothing was installed before it). Copies without an `.upgrade.record`
come from a run that stopped while preparing (the files in place had not
been touched then), and are simply removed. When this cannot be
confirmed, the script stops with 65 before it stops the service and
before it changes any of WGAL's files, and says what to do: when that run
had not yet started the new version, how to put the previous set back
(those copies may be the only complete set); when it may have started the
new version, only how to put the complete new set back, or to leave
everything as it is for the owner.

The `started` line of `.upgrade.record` records whether the new version
may have started: `no` while preparing, `yes` just before the new version
is started. The record is removed, with the copies, only once a complete
set has started. When a later run reads a record that does not say
`started no` (or has no such line), it takes it that the new version may
have run and changed the stored state: that run, too, **never starts the
previous version**, and keeps recording `started yes`.

Every time before it starts the new version (the first install's start,
an upgrade's first start, or the trap's start during recovery), the
script writes `started yes` into `.upgrade.record` and reads it back. When
that cannot be confirmed (a full disk, or a file system that became
read-only), it does **not** start the service, keeps the copies in
`/opt/wgal` and `.upgrade.record`, ends with 1, and says truthfully
whether this run started the new version: leave the complete new set in
place, do not put `.<name>.old` back, fix the cause and run the script
again from the release folder; or leave everything as it is for the owner
to decide.

**If any step fails from the stop until the started service has been
checked (Ctrl-C included)**, the `trap` takes over:

1. It finds out by SHA-256 whether what is in place is the **complete
   previous set** or the **complete new set** (the new one has not run
   yet). With some files replaced and some not, it puts the previous ones
   back from `.old` (files the previous set did not have are removed,
   such as `wgal` when upgrading from a version before 3.0.0).
2. It runs `daemon-reload`, starts that set and checks its socket, and
   says which step failed, whether the previous or the new version runs
   now, and which access list is in effect: when the new one was not yet
   in place, "the old list is still in effect; the one given this time was
   not applied"; when it was, the new one is kept, and **the old list is
   never put back** (an authorization just revoked is not restored).
3. It does not touch WGAL's stored state, and never copies a backup over
   it.
4. It compares the wg-quick units as usual. It ends with 4 when the
   service runs (1 and 2 come first), and with 1 when it does not, saying
   that nothing monitors the tunnels now.

Once the new version's start has begun, the new version may already have
run and changed the stored state: a failure after that (Ctrl-C while it
starts or while it waits for the socket, say) **does not put the previous
version back**; the `trap` starts the complete new set in place again,
and says that the new version may have run.

With no complete set, nothing is started; `.old`, `.new` and
`.upgrade.record` are kept, and it ends with 1 and says so. When the new
version had not started yet, a later run refuses with 65 (and keeps those
copies): fix the cause, put the previous set back yourself (move each
`.<name>.old` still in `/opt/wgal` back to its name, the unit's to
`/etc/systemd/system/wgal-watchdog.service`; remove each program whose
previous sum in `.upgrade.record` is `-`), and run it again. A new
version that started and then shows no socket is not rolled back
automatically: it may already have changed the stored state.

When the new version **may already have run** (this run or an earlier
one began starting it), the `trap` neither starts the previous set nor
puts it back, even when what is in place is the complete previous set. It
keeps the copies and `.upgrade.record`, ends with 1, and says: **do not
put the previous version back and do not start it**; fix the cause and
run it again from the release, so that it puts the complete new set in
place. When what is in place is not a complete set, a later run refuses
with 65: first put `wgal-watchdog`, `wgal-desktop`, `wgal-mcp` and `wgal`
from the same release into `/opt/wgal`, and `wgal-watchdog.service` at
`/etc/systemd/system/wgal-watchdog.service` (each matching the new sum in
the record); or leave everything as it is for the owner to decide.

In the three cases above where it does **not** start (the record cannot
be written, no complete set, or the new version may have run and what is
in place is not the complete new set), the script first runs
`systemctl disable wgal-watchdog.service` and checks it with
`systemctl is-enabled`, so that systemd does not start at the next boot
the set the script just refused to start. It touches only WGAL's own
unit, never a `wg-quick@` unit or whether one starts at boot.
- The `!!!` message says the start at boot was turned off; the next run
  that passes its checks runs `enable` again (putting a new set in place
  always does; the trap also runs `enable` before it starts a set during
  recovery).
- A run that refuses with 65 before the stop leaves it disabled.
- When it cannot be turned off, the message says so plainly, and that
  systemd may start the set in place at the next boot: fix the cause and
  run the script again before restarting the computer, or leave it as it
  is for the owner.

The exit codes of `install.sh`:
- 0: done;
- 1: the service does not run with its socket;
- 2: a wg-quick unit changed, or their states could not be read at the
  end (2 wins over 1);
- 4: an upgrade step failed after the stop; the service was started again
  with the complete set in place (only when neither 1 nor 2 applies);
- 64: wrong usage;
- 65: a check or a preparation before the service stops did not pass. The
  service and the WGAL files in use are as they were, but the copies in
  `/opt/wgal`, the menu entry and the start at login (their removal, with
  `--no-window`), `uninstall.sh`, the access list prepared in `/etc/wgal`,
  or the window's packages, may already have been replaced.

The test of this flow is `tests/install-recovery.test.sh`. Run it as a
normal user (not with sudo). It loads `install.sh` as a library (it then
runs nothing), points its paths at a scratch folder of its own, replaces
`systemctl`, `pgrep` and file operations with stand-ins, and makes each
step fail in turn. The stand-ins exist only in the test: `install.sh` has
no option or variable that selects them. It is not an upgrade test on a
real machine.

## Going back to a version before 4.0.0

Once `wgal language <tag>` has been used (owner decision of 2026-11-19),
`/var/lib/wgal/notify/notify.json` holds the language `wgal` writes in, and a watchdog before
4.0.0 cannot read that file: it keeps notifications off and refuses
changes to them until the file is fixed.

- Before installing an older version, run `wgal language --clear` in 4.0.0
  or later (the Telegram messages keep their language) and check that
  `wgal language` shows English as the default.
- If an older version already runs with notifications off for this
  reason, install 4.0.0 or later again, run `wgal language --clear`, and
  then install the older version again.

The backups made before upgrades (`/var/lib/wgal/backup/state-<time>`) are no way around this: one
made by an upgrade after `wgal language` was used holds the language too.
This README gives no steps to put a backup back on Ubuntu.

## Logs

- The watchdog's own log: `journalctl -u wgal-watchdog`.
- The event log: `/var/lib/wgal/events/events.jsonl`, readable only by
  root; the window's log page and `wgal log` show it.

## Exit codes (`status=` in `systemctl status`)

| Code | Meaning |
|---|---|
| 1 | `/var/lib/wgal` is not root's 0700 folder, or `/run/wgal` is not root's alone |
| 2 | `state`, `notify` or `events` is not root's 0700 folder |
| 3 | the state file cannot be opened |
| 4 | the monitoring loop cannot start |
| 5 | not run as root (not restarted automatically) |
| 11 | the monitoring loop stopped answering; systemd restarts it |

## Differences from the Windows version (not implemented yet)

- After sleep or a network change, the grace period does not start again;
  the failed probes alone still decide whether there is a fault.
- There is no package (`.deb`); WGAL is installed with scripts (owner
  decision of 2026-11-05).
