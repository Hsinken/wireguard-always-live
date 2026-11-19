#!/usr/bin/env bash
# Installs or upgrades WG Always Live (WGAL) on Ubuntu from this folder.
#
# Run it with sudo from the folder the release was unpacked into:
#
#   sudo ./install.sh --tunnel wg0 [--tunnel wg1 | --tunnel wg0,wg1] [--user NAME]
#   sudo ./install.sh          # keeps the access list there is (an upgrade,
#                              # or a reinstall over kept data)
#   ./install.sh --list-tunnels   # lists the tunnels as JSON, changes nothing
#   sudo ./install.sh --install-runtime ...   # may install the window's packages
#   sudo ./install.sh --no-window ...   # a server without a desktop: no window
#
# --tunnel  a wg-quick interface (/etc/wireguard/<name>.conf) the user may
#           register and hand to automatic recovery; required when there is
#           no access list yet. Given otherwise, it rewrites the whole list.
# --user    the account that uses the WGAL window; default: the one that ran
#           sudo (SUDO_USER).
# --list-tunnels  prints this machine's wg-quick tunnels as JSON and exits,
#           for an agent (or setup.sh) to let the user pick before the one
#           sudo run that installs; takes no other option. Run as root it
#           lists every /etc/wireguard/<name>.conf ("complete": true);
#           without root that folder is not readable, so it lists the
#           wg-quick units systemd knows and the running WireGuard
#           interfaces ("complete": false). One tunnel per line:
#           {"name": ..., "status": "running"|"stopped",
#            "starts_at_boot": true|false, "accepted": true|false}
#           accepted: WGAL takes the name (--tunnel also needs the .conf).
#           Only names, never a configuration's contents, are read.
# --install-runtime  the user agreed that the Ubuntu packages the WGAL
#           window needs and this system lacks (libwebkit2gtk-4.1-0, its
#           window; libayatana-appindicator3-1, its tray icon) may be
#           installed with apt-get (owner decision of 2026-11-12). Without
#           it, a missing package is asked about when the script runs in a
#           terminal, and otherwise stops the script before anything
#           changes, naming the one command that installs them.
# --no-window  for a computer without a desktop, such as a server (owner
#           decision of 2026-11-18): the window's packages are neither
#           checked nor installed, the menu and autostart entries are not
#           installed, and WGAL's own ones from an earlier install are
#           removed. WGAL is then used with its command line, wgal. The
#           window's program is still copied into /opt/wgal (the set of
#           programs stays the same), but nothing opens it. Not kept: each
#           run follows its own options. Not with --install-runtime.
#
# Every install also puts WGAL's command line, wgal, in /opt/wgal and, once
# the new set is in place, links /usr/local/bin/wgal to it, only where
# nothing is there or the link already points to it; anything else there
# is left alone, and the closing lines give the full path instead.
#
# Everything is checked before anything changes. Then, before the service
# stops, it copies the four programs and the unit into /opt/wgal as
# .<name>.new and the ones in place as .<name>.old (each checked by its
# SHA-256, the sums recorded in /opt/wgal/.upgrade.record), installs the
# menu and autostart entries (or removes them, with --no-window), and
# writes the new access list to a new file
# in /etc/wgal. On an upgrade it then stops the service and copies its
# stored state (not the bot token) into
# /var/lib/wgal/backup/state-<UTC time>, keeping the newest five. Then it
# moves the access list into place (one rename: the old one is never
# removed first), moves the programs into /opt/wgal, writes the systemd
# unit wgal-watchdog.service, and starts the service and checks it.
#
# If anything fails from the stop until the started service has been
# checked (Ctrl-C included), a trap finds out which complete set is in
# place: the previous one, or the new one. Before the new one was
# started, with some files replaced and some not, it puts the previous
# ones back from the .old copies. Once the start has begun, the new
# version may already have run and changed the stored state, so nothing
# is put back: it starts the new set again. It starts that set, checks
# it, and says which one runs and which access list is in effect. It
# never copies stored state back and never puts back an access list it
# replaced. With no complete set it leaves the service stopped, keeps the
# copies and the record, and says so; once the new version may have run,
# it never starts the previous set, and says to put the complete new set
# in place (or to hand it to the owner), never to put the previous one
# back. A new version that started and does not run (no socket) is not
# rolled back: it may already have changed the stored state.
#
# A run that finds copies an earlier run left uses its record to check,
# before changing anything of WGAL, that a complete set is in place; if
# not, it stops (65), so as not to lose copies that may be the only
# complete set. The record says whether that run may have started the new
# version ("started yes", or no such line); if so, this run never starts
# the previous set either. The new set is started (the first time, or
# again by the trap) only after the record has been read back saying
# "started yes"; if it cannot be, the service is left stopped, the copies
# and the record are kept, and the script says so. Whenever it leaves the
# service stopped like this, it also disables wgal-watchdog.service (WGAL's
# own unit only) and checks it, so that systemd does not start that set at
# the next boot, and says so (or that it could not, in which case systemd
# may start it at the next boot: run the script again first); the next run
# that gets past its checks enables it again (as does the trap whenever it
# starts a set).
#
# It never starts, stops or changes a wg-quick unit, a WireGuard
# configuration or whether a tunnel starts at boot: it records every
# wg-quick unit's state before and after and reports any difference.
#
# Exit codes: 0 done; 1 the service is not running with its socket (or, on
# a first install, the record could not be marked and nothing was started); 2 a
# wg-quick unit changed while the script ran, or their states could not
# be read again at the end (2 wins over 1); 4 a step of the upgrade failed
# after the service was stopped, and the service runs again with the
# complete set then in place (only when neither 1 nor 2 applies); 64 wrong
# usage; 65 a check or a preparation failed before the service was
# stopped (systemctl not answering included): the service and the WGAL
# files in use are as they were, but the copies in /opt/wgal, the menu and
# autostart entries, the uninstaller, the staged access list in /etc/wgal
# or the window's packages may already be new.
set -euo pipefail
umask 022
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

# The paths are fixed here and nowhere else; they are not readonly only so
# that tests/install-recovery.test.sh, which sources this file, can point
# them at its own scratch folder. Nothing outside this file sets them.
PREFIX=/opt/wgal
UNIT_FILE=/etc/systemd/system/wgal-watchdog.service
ACCESS_DIR=/etc/wgal
ACCESS_FILE=/etc/wgal/access.conf
STATE_DIR=/var/lib/wgal
SOCKET=/run/wgal/watchdog.sock
DESKTOP_FILE=/usr/share/applications/wgal.desktop
AUTOSTART_FILE=/etc/xdg/autostart/wgal.desktop
CLI_LINK=/usr/local/bin/wgal
# The release folder: the one this script is in. Empty here, set only by
# tests/install-recovery.test.sh, which sources this file.
RELEASE_DIR=""
readonly UNIT=wgal-watchdog.service
readonly PROGRAMS=(wgal-watchdog wgal-desktop wgal-mcp wgal)
# What makes a complete set: the four programs and WGAL's own unit.
readonly SET_FILES=(wgal-watchdog wgal-desktop wgal-mcp wgal wgal-watchdog.service)
# The programs a set from before 3.0.0 has: no wgal. An earlier run's
# record of such a set is read with wgal absent from both versions.
readonly OLDER_SET_FILES=(wgal-watchdog wgal-desktop wgal-mcp wgal-watchdog.service)
readonly KEEP_BACKUPS=5

die() {
    echo "install.sh: $1" >&2
    exit "${2:-65}"
}

usage() {
    sed -n '4,48p' "$0" >&2
    exit "${1:-64}"
}

# The Ubuntu packages the WGAL window needs at run time: WebKitGTK (its
# window; it does not start without it) and Ayatana AppIndicator (its tray
# icon). The only packages this script ever installs, and only when the
# user agrees (--install-runtime, or yes to the question).
readonly RUNTIME_PACKAGES=(libwebkit2gtk-4.1-0 libayatana-appindicator3-1)

# The runtime packages that are not installed, one per line.
missing_runtime() {
    local package
    for package in "${RUNTIME_PACKAGES[@]}"; do
        # Installed and not in error, whether held or not.
        [ "$(dpkg-query -W -f='${db:Status-Status} ${db:Status-Eflag}' "$package" 2>/dev/null)" = "installed ok" ] ||
            echo "$package"
    done
}

# The window's packages this install needs and the system lacks: none
# with --no-window, which installs no window (owner decision of
# 2026-11-18).
window_packages_missing() {
    [ "$NO_WINDOW" -eq 1 ] || missing_runtime
}

# Whether systemd runs this system.
systemd_running() {
    [ -d /run/systemd/system ]
}

# The programs of the new set an earlier run recorded, in words, for its
# hand-made repair: the ones the record has, and whether wgal belongs to it
# (a record from before 3.0.0 has none; Codex review of the CLI, second
# round, N-P3-1).
recorded_programs() {
    local name listed=()
    for name in "${PROGRAMS[@]}"; do
        [ -z "${NEW_SUM[$name]:-}" ] || listed+=("$name")
    done
    local text="${listed[*]}"
    text=${text// /, }
    if [ -z "${NEW_SUM[wgal]:-}" ]; then
        text="$text (that version has no wgal: remove $PREFIX/wgal if it is there)"
    fi
    echo "$text"
}

# The rules of WGAL's InterfaceName: [A-Za-z0-9_.-]{1,15}, not starting with
# '-', not "all", "interfaces", "." or "..".
valid_tunnel() {
    [[ "$1" =~ ^[A-Za-z0-9_.-]{1,15}$ ]] && [[ "$1" != -* ]] &&
        [[ "$1" != all && "$1" != interfaces && "$1" != . && "$1" != .. ]]
}

# Every wg-quick unit systemd knows, with whether it runs and whether it
# starts at boot, sorted: what this script must leave as it found it.
wireguard_states() {
    # systemctl is asked for every service and the wg-quick ones are picked
    # out here: with a pattern, list-unit-files exits 1 when nothing matches
    # (no wireguard-tools at all), which cannot be told apart from a
    # systemctl that cannot answer. Without one it fails only when it
    # cannot answer, and then so does this function.
    local units files
    units=$(systemctl list-units --all --plain --no-legend --type=service) || return 1
    files=$(systemctl list-unit-files --plain --no-legend --type=service) || return 1
    {
        awk '$1 ~ /^wg-quick@/ {print $1, $3, $4}' <<<"$units"
        awk '$1 ~ /^wg-quick@/ {print $1, $2}' <<<"$files"
    } | sort
}

# The machine's tunnels, read only: what --list-tunnels prints. Names come
# from the configuration files' names (as root), the wg-quick units and the
# WireGuard interfaces; a name outside a plain character set is left out,
# so every printed one is safe in JSON as it is.
list_tunnels() {
    local -A found=()
    local complete=false name
    if [ -d /etc/wireguard ] && [ -r /etc/wireguard ] && [ -x /etc/wireguard ]; then
        complete=true
        local conf
        for conf in /etc/wireguard/*.conf; do
            if [ ! -f "$conf" ] || [ -L "$conf" ]; then
                continue
            fi
            name=${conf##*/}
            found[${name%.conf}]=1
        done
    fi
    local units
    units=$(wireguard_states 2>/dev/null) || units=""
    while read -r name _; do
        name=${name#wg-quick@}
        name=${name%.service}
        [ -n "$name" ] && found[$name]=1
    done <<<"$units"
    while read -r name; do
        [ -n "$name" ] && found[$name]=1
    done < <(ip -o link show type wireguard 2>/dev/null | awk -F': ' '{print $2}' | cut -d@ -f1)
    local first=1 status boot accepted
    printf '{"complete":%s,"tunnels":[' "$complete"
    while read -r name; do
        [[ "$name" =~ ^[A-Za-z0-9_.=+@-]{1,64}$ ]] || continue
        status=stopped
        if systemctl is-active --quiet "wg-quick@$name.service" 2>/dev/null ||
            ip link show dev "$name" type wireguard >/dev/null 2>&1; then
            status=running
        fi
        boot=false
        [ "$(systemctl is-enabled "wg-quick@$name.service" 2>/dev/null)" = enabled ] && boot=true
        accepted=false
        valid_tunnel "$name" && accepted=true
        [ "$first" -eq 1 ] || printf ','
        first=0
        printf '\n{"name":"%s","status":"%s","starts_at_boot":%s,"accepted":%s}' \
            "$name" "$status" "$boot" "$accepted"
    done < <(printf '%s\n' "${!found[@]}" | sort)
    printf '\n]}\n'
}

# --- The set of programs and unit, staged before the service stops ---------
# Globals, so that the EXIT trap sees them: the step between the stop and
# the start that is running (empty outside it), the staged access list
# (empty when none is written), the SHA-256 of each new file and of each
# one in place before (empty when there was none), and what was found.
WINDOW_STEP=""
STAGED_ACCESS=""
declare -gA NEW_SUM=() OLD_SUM=()
WG_BEFORE=""
INSTALL_USER=""
UPGRADE_FAILED=0
RECOVERED_SET=""
# Set once the start of the new set has begun: from then on it may have
# run, and nothing is put back.
NEW_MAY_HAVE_RUN=0
# Set when an earlier run may have started the new set (check_earlier_copies),
# and when this run has called its start.
EARLIER_MAY_HAVE_RUN=0
NEW_START_BEGUN=0
# 1 when the service started by upgrade_window runs with its socket.
WINDOW_RUNNING=0
# 1 when the service first_install started runs with its socket.
FIRST_RUNNING=0
# 1 with --no-window.
NO_WINDOW=0
# How the closing lines tell the user to run the command line.
CLI_COMMAND=

# Where a file of the set is installed.
target_of() {
    case "$1" in
    wgal-watchdog.service) echo "$UNIT_FILE" ;;
    *) echo "$PREFIX/$1" ;;
    esac
}

# A staged copy: fixed names in WGAL's own program folder.
staged_path() {
    echo "$PREFIX/.$1.$2"
}

# The SHA-256 of a plain file; nothing for one that is missing or a link.
sum_of() {
    if [ -f "$1" ] && [ ! -L "$1" ]; then
        sha256sum -- "$1" | cut -d ' ' -f 1
    fi
}

# The record of a staged set, beside its copies: one line per file,
# "<name> <new SHA-256> <old SHA-256, or - for none>", and "started no",
# rewritten as "started yes" before the new set's start begins (or carried
# from an earlier run that may have started it). Written once every copy
# is made and checked, removed after them; so copies without a record
# were left by a run that stopped while staging, before the service
# stopped. A record without its "started" line cannot tell, so it counts
# as "yes": the new version may have run.
record_path() {
    echo "$PREFIX/.upgrade.record"
}

# Writes the record (record_path) whole beside it, then renames it into
# place; $1 is yes or no, as its "started" line says. Fails when a step
# does, also where set -e does not apply.
write_record() {
    local record name
    record=$(record_path)
    rm -f -- "$record.tmp" || return 1
    {
        for name in "${SET_FILES[@]}"; do
            echo "$name ${NEW_SUM[$name]} ${OLD_SUM[$name]:--}"
        done
        echo "started $1"
    } >"$record.tmp" || return 1
    chmod 0644 "$record.tmp" || return 1
    mv -f -- "$record.tmp" "$record"
}

# Marks the record "started yes" and reads it back. The new set is started
# only once a rerun would read that it may have run: fails when the record
# does not end in that line (nothing may start then).
mark_started() {
    local record
    record=$(record_path)
    write_record yes
    [ -f "$record" ] && [ ! -L "$record" ] && [ "$(tail -n 1 -- "$record" 2>/dev/null)" = "started yes" ]
}

# An earlier run that failed between the stop and the start may have kept
# its copies on purpose: they may be the only complete set. stage_set
# removes them only when the earlier run's record shows a complete set in
# place, or that there was no previous version; otherwise this stops the
# script before anything of WGAL changes. Loads the record into NEW_SUM
# and OLD_SUM, which stage_set then sets anew. A record that does not say
# "started no" sets NEW_MAY_HAVE_RUN: that run may have started the new
# version, so this one never starts the previous version either.
check_earlier_copies() {
    local record name new old in_place count=0 started=""
    local -r sum='^[0-9a-f]{64}$' old_sum='^([0-9a-f]{64}|-)$'
    record=$(record_path)
    [ -e "$record" ] || [ -L "$record" ] || return 0
    { [ -f "$record" ] && [ ! -L "$record" ]; } ||
        die "$record is not a plain file (or is a link); check and remove it, then run again"
    NEW_SUM=()
    OLD_SUM=()
    while read -r name new old; do
        if [ "$name" = started ] && [ -z "$started" ] && [ -z "$old" ] &&
            { [ "$new" = yes ] || [ "$new" = no ]; }; then
            started=$new
            continue
        fi
        if ! [[ " ${SET_FILES[*]} " == *" $name "* && -z "${NEW_SUM[$name]:-}" &&
            "$new" =~ $sum && "$old" =~ $old_sum ]]; then
            die "$record is not a record this script wrote; nothing of WGAL was changed. Check it, then run again"
        fi
        NEW_SUM[$name]=$new
        OLD_SUM[$name]=${old#-}
        count=$((count + 1))
    done <"$record"
    if [ "$count" -eq "$((${#SET_FILES[@]} - 1))" ] && [ -z "${NEW_SUM[wgal]:-}" ] &&
        [ "$count" -eq "${#OLDER_SET_FILES[@]}" ]; then
        # Written by the script of a version before 3.0.0, whose sets have
        # no wgal: absent from both, as in that run.
        NEW_SUM[wgal]=""
        OLD_SUM[wgal]=""
        count=$((count + 1))
    fi
    [ "$count" -eq "${#SET_FILES[@]}" ] ||
        die "$record is not a record this script wrote; nothing of WGAL was changed. Check it, then run again"
    if [ "$started" != no ]; then
        NEW_MAY_HAVE_RUN=1
        EARLIER_MAY_HAVE_RUN=1
    fi
    [ -n "${OLD_SUM[wgal-watchdog]}" ] || return 0
    in_place=$(set_in_place)
    if [ "$in_place" = mixed ] && [ "$NEW_MAY_HAVE_RUN" -eq 1 ]; then
        die "an earlier run of this script did not finish: the files in place are neither the complete previous version nor the complete new one it recorded in $record, and that run may already have started the new version, so the new version may already have run and changed the stored state. Nothing of WGAL was changed and the service was not stopped. Do not put the previous version back: put the complete new version in place yourself (from that run's release folder, $(recorded_programs) into $PREFIX and wgal-watchdog.service over $UNIT_FILE, each matching its new sum in the record), then run this script again from that release folder; or leave everything as it is, with the copies in $PREFIX, and hand it to the owner"
    fi
    if [ "$in_place" = mixed ]; then
        die "an earlier run of this script did not finish: the files in place are neither the complete previous version nor the complete new one it recorded in $record, and the copies it kept in $PREFIX (.<name>.old the previous version, .<name>.new the new one) may be the only complete set. Nothing of WGAL was changed and the service was not stopped. Put the previous version back yourself (move each .<name>.old still in $PREFIX over its file, .wgal-watchdog.service.old over $UNIT_FILE, and remove each program the record lists with - as its old sum), then run this script again"
    fi
    [ "$in_place" = new ] || in_place=previous
    echo "An earlier run left its copies; the complete $in_place set it recorded is in place, so they are replaced"
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ]; then
        echo "install.sh: that run may already have started the new version: if this run fails, the previous version is not started again" >&2
    fi
}

# Copies each file of the new set from the release folder $1 to
# .<name>.new, and each one in place to .<name>.old, checking every copy,
# and records their sums (record_path).
stage_set() {
    local from=$1 name src target new old mode
    check_earlier_copies
    install -d -o root -g root -m 0755 "$PREFIX"
    for name in "${SET_FILES[@]}"; do
        src="$from/$name"
        target=$(target_of "$name")
        new=$(staged_path "$name" new)
        old=$(staged_path "$name" old)
        mode=0755
        if [ "$name" = wgal-watchdog.service ]; then
            mode=0644
        fi
        # Left by an earlier run, as check_earlier_copies allowed; rm
        # never follows a link.
        rm -f -- "$new" "$old"
        NEW_SUM[$name]=$(sum_of "$src")
        install -o root -g root -m "$mode" "$src" "$new"
        if [ -z "${NEW_SUM[$name]}" ] || [ "$(sum_of "$new")" != "${NEW_SUM[$name]}" ]; then
            die "the copy $new does not match $src; nothing of WGAL was stopped"
        fi
        OLD_SUM[$name]=""
        if [ -e "$target" ] || [ -L "$target" ]; then
            { [ -f "$target" ] && [ ! -L "$target" ]; } ||
                die "$target is not a plain file (or is a link); check and remove it, then run again"
            OLD_SUM[$name]=$(sum_of "$target")
            cp -p -- "$target" "$old"
            [ "$(sum_of "$old")" = "${OLD_SUM[$name]}" ] ||
                die "the copy $old does not match $target; nothing of WGAL was stopped"
        fi
    done
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ]; then
        write_record yes
    else
        write_record no
    fi
}

# Writes the whole new access list, for user ID $1 and the tunnels after
# it, to a new file in its folder; moved into place later in one rename.
stage_access() {
    local id=$1 name
    shift
    install -d -o root -g root -m 0755 "$ACCESS_DIR"
    STAGED_ACCESS=$(mktemp "$ACCESS_DIR/.access.XXXXXX")
    {
        echo "# Written by install.sh at $(date -u +%Y-%m-%dT%H:%M:%SZ)."
        echo "# Who may use the WGAL window besides root, and the tunnels they may"
        echo "# register and hand to automatic recovery. Read when the service starts."
        echo "user $id"
        for name in "$@"; do
            echo "tunnel $name"
        done
    } >"$STAGED_ACCESS"
    chmod 0644 "$STAGED_ACCESS"
}

# Puts every staged new file that is not in place yet in place: a program
# by one rename in its folder, the unit by a copy; then checks them all.
install_staged() {
    local name target
    for name in "${SET_FILES[@]}"; do
        target=$(target_of "$name")
        [ "$(sum_of "$target")" != "${NEW_SUM[$name]}" ] || continue
        if [ "$name" = wgal-watchdog.service ]; then
            install -o root -g root -m 0644 "$(staged_path "$name" new)" "$target"
        else
            mv -f -- "$(staged_path "$name" new)" "$target"
        fi
    done
    for name in "${SET_FILES[@]}"; do
        target=$(target_of "$name")
        if [ "$(sum_of "$target")" != "${NEW_SUM[$name]}" ]; then
            echo "install.sh: $target does not match the new version" >&2
            return 1
        fi
    done
}

# Which complete set is in place: "new" when every file is the new one,
# "old" when every file is as before (each one that was there is the
# previous one again, and one the previous version did not have is
# absent), else "mixed".
set_in_place() {
    local name now is_new=1 is_old=1
    [ -n "${OLD_SUM[wgal-watchdog]}" ] || is_old=0
    for name in "${SET_FILES[@]}"; do
        now=$(sum_of "$(target_of "$name")")
        [ "$now" = "${NEW_SUM[$name]}" ] || is_new=0
        # An empty sum (none before) matches only a file that is not there.
        [ "$now" = "${OLD_SUM[$name]}" ] || is_old=0
    done
    if [ "$is_new" -eq 1 ]; then
        echo new
    elif [ "$is_old" -eq 1 ]; then
        echo old
    else
        echo mixed
    fi
}

# Puts back, from the .old copies, every previous file that is no longer
# in place, and removes one the previous version did not have.
restore_old() {
    local name target
    for name in "${SET_FILES[@]}"; do
        target=$(target_of "$name")
        if [ -z "${OLD_SUM[$name]}" ]; then
            rm -f -- "$target"
        elif [ "$(sum_of "$target")" != "${OLD_SUM[$name]}" ]; then
            if [ "$name" = wgal-watchdog.service ]; then
                install -o root -g root -m 0644 "$(staged_path "$name" old)" "$target"
            else
                mv -f -- "$(staged_path "$name" old)" "$target"
            fi
        fi
    done
}

# Removes the staged copies, and then their record, once a complete set is
# in place.
remove_staged() {
    local name
    for name in "${SET_FILES[@]}"; do
        rm -f -- "$(staged_path "$name" new)" "$(staged_path "$name" old)"
    done
    rm -f -- "$(record_path)"
}

# Turns off the start at boot of WGAL's own unit (never a wg-quick one), so
# that systemd does not start, at the next boot, a set this run refused to
# start; then checks it. Starts and stops nothing. Prints the sentence the
# "NOT started" message carries: that it was turned off and how it is
# turned back on, or that it could not be.
disable_autostart() {
    local state
    systemctl disable "$UNIT" >&2 || true
    state=$(systemctl is-enabled "$UNIT" 2>/dev/null) || true
    if [ "$state" = disabled ]; then
        echo "Its start at boot was turned off (systemctl disable $UNIT), so systemd does not start it at the next boot either; the next run of this script that gets past its checks turns it back on."
    else
        echo "Its start at boot could NOT be turned off (systemctl is-enabled $UNIT says \"${state:-nothing}\"): systemd may start $UNIT, with the files now in place, at the next boot. Fix the cause and run this script again (or hand everything to the owner) before you restart this computer."
    fi
}

# --- The upgrade, from stopping the service to starting it again -----------
stop_service() {
    echo "Stopping $UNIT"
    systemctl stop "$UNIT" || die "stopping $UNIT failed; no WGAL file was replaced"
    local waited=0
    while pgrep -x -u 0 wgal-watchdog >/dev/null 2>&1; do
        [ "$waited" -lt 50 ] ||
            die "the watchdog is still running 50 s after its stop; no WGAL file was replaced (the service may already be stopped)"
        sleep 1
        waited=$((waited + 1))
    done
}

# Copies the stored state into a new backup folder; call it only while the
# service is stopped.
backup_state() {
    [ -d "$STATE_DIR/state" ] || return 0
    local stamp backup file
    stamp=$(date -u +%Y%m%d-%H%M%SZ)
    backup="$STATE_DIR/backup/state-$stamp"
    install -d -o root -g root -m 0700 "$STATE_DIR/backup" "$backup" "$backup/state" "$backup/notify"
    for file in state/state.json state/state.prev.json notify/notify.json; do
        if [ -f "$STATE_DIR/$file" ] && [ ! -L "$STATE_DIR/$file" ]; then
            cp -p -- "$STATE_DIR/$file" "$backup/$file"
        fi
    done
    echo "Stored state copied to $backup"
    # Only the newest backups are kept: real folders named
    # state-<time> in the backup folder, never a link.
    find "$STATE_DIR/backup" -mindepth 1 -maxdepth 1 -type d -name 'state-*' -printf '%f\n' |
        sort | head -n "-$KEEP_BACKUPS" | while read -r old; do
            rm -rf -- "${STATE_DIR:?}/backup/$old"
        done
}

# Puts the staged access list and the new set in place and tells systemd.
# Leaves WINDOW_STEP at its last step: the caller says what comes next.
install_new_set() {
    if [ -n "$STAGED_ACCESS" ]; then
        WINDOW_STEP="putting the new access list in place"
        mv -f -- "$STAGED_ACCESS" "$ACCESS_FILE"
    fi
    WINDOW_STEP="replacing the programs and the unit"
    install_staged
    WINDOW_STEP="reloading systemd"
    systemctl daemon-reload
    WINDOW_STEP="enabling $UNIT"
    systemctl enable "$UNIT" >/dev/null
}

# A first install: puts the new set in place and starts it only once the
# record says "started yes" and reads back so, as an upgrade does (Codex
# review of the CLI, second round, N-P2-1): a run interrupted after the
# start leaves a record that says the new version may have run. When a
# step of putting it in place fails, or the record cannot be marked,
# nothing is started, the unit's start at boot is turned off and the
# script ends with 1. Sets FIRST_RUNNING to 1 when the service runs. It
# must be called as a plain command, never in a condition, which would turn
# set -e off inside it (Codex review of the CLI, third round, R3-P2-1);
# its EXIT trap catches a failing step, and Ctrl-C, as upgrade_window's,
# and stays until the record reads back "started yes" (fourth round,
# R4-P2-1): the unit is enabled by then, so a Ctrl-C while the record is
# marked must still turn its start at boot off.
first_install() {
    FIRST_RUNNING=0
    trap on_first_install_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    install_new_set
    WINDOW_STEP="marking $(record_path) to say that the new version may run"
    if ! mark_started; then
        WINDOW_STEP="marking $(record_path): it could not be marked to say that it may run ($PREFIX must be writable: free disk space, or a file system that is not read-only)"
        exit 1
    fi
    trap - EXIT INT TERM HUP
    WINDOW_STEP=""
    NEW_START_BEGUN=1
    systemctl restart "$UNIT" || echo "install.sh: systemctl restart $UNIT failed" >&2
    if wait_running; then
        FIRST_RUNNING=1
    fi
}

# The EXIT trap while first_install puts the new set in place: nothing was
# started; the unit's start at boot is turned off and the script ends
# with 1, the copies and the record kept.
on_first_install_exit() {
    local code=$?
    trap - EXIT
    trap '' INT TERM HUP
    local autostart
    autostart=$(disable_autostart)
    echo "install.sh: !!! $UNIT was NOT started: the first install failed while ${WINDOW_STEP:-putting the new version in place} (exit status $code). The copies and the record are kept: fix the cause and run this script again. $autostart !!!" >&2
    exit 1
}

# Stops the service, backs up its state, puts the new set in place, starts
# it and checks it, with the trap armed until the check is done: any
# failure, or Ctrl-C, ends in on_window_exit. Sets WINDOW_RUNNING.
upgrade_window() {
    trap on_window_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    WINDOW_STEP="stopping the service"
    stop_service
    WINDOW_STEP="backing up the stored state"
    backup_state
    install_new_set
    # From here on the new version may run and change the stored state: a
    # failure no longer puts the previous one back.
    NEW_MAY_HAVE_RUN=1
    WINDOW_STEP="starting the new version"
    # Kept for a rerun until the record is removed; not started unless the
    # record says so.
    mark_started || {
        echo "install.sh: $(record_path) could not be marked \"started yes\"" >&2
        exit 1
    }
    NEW_START_BEGUN=1
    systemctl restart "$UNIT" || echo "install.sh: systemctl restart $UNIT failed" >&2
    WINDOW_RUNNING=0
    wait_running && WINDOW_RUNNING=1
    WINDOW_STEP=""
    trap - EXIT INT TERM HUP
}

# The EXIT trap while upgrade_window runs: recovers, checks WireGuard and
# ends the script with its exit code.
on_window_exit() {
    local code=$?
    [ -n "$WINDOW_STEP" ] || return 0
    set +e
    trap - EXIT
    trap '' INT TERM HUP
    local running=0
    recover_from "$code" && running=1
    finish "$running"
}

# After a failed step: makes sure a complete set is in place, starts it
# and checks it, and says what happened. Never touches the stored state,
# and never puts back an access list that was replaced. Once the new set's
# start had begun, puts nothing back: it starts the new set again. Returns
# 0 when the service runs.
recover_from() {
    local step=$WINDOW_STEP in_place
    WINDOW_STEP=""
    UPGRADE_FAILED=1
    echo "install.sh: !!! the upgrade failed while $step (exit status $1) !!!" >&2
    in_place=$(set_in_place)
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ]; then
        : # The new version may have run: the previous one is not put back.
    elif [ "$in_place" = mixed ] && [ -n "${OLD_SUM[wgal-watchdog]}" ]; then
        echo "install.sh: only some files were replaced; putting the previous ones back from the copies kept in $PREFIX (.<name>.old)" >&2
        restore_old
        in_place=$(set_in_place)
    elif [ "$in_place" = mixed ]; then
        echo "install.sh: only some files were replaced and there is no previous wgal-watchdog; putting the rest of the new ones in place" >&2
        install_staged
        in_place=$(set_in_place)
    fi
    if [ -n "$STAGED_ACCESS" ]; then
        if [ -e "$STAGED_ACCESS" ]; then
            echo "install.sh: $ACCESS_FILE was not replaced: the previous access list is still in effect, not the one given to this run" >&2
            rm -f -- "$STAGED_ACCESS"
        else
            echo "install.sh: the new $ACCESS_FILE is in place; it is kept (an access list is never put back)" >&2
        fi
    fi
    RECOVERED_SET=$in_place
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ] && [ "$in_place" != new ]; then
        # The previous version is never started on a state the new one may
        # have changed; the record keeps saying so for a rerun.
        local what="the files in place are not the complete new version"
        [ "$in_place" = mixed ] || what="the previous version is in place, not the new one"
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! $UNIT was NOT started: $what, and the new version's start had begun (in this run or in an earlier one), so the new version may already have run and changed the stored state: do not put the previous version back, and do not start it. The copies in $PREFIX and their record $(record_path) are kept. Fix the cause and put the complete new version in place: run this script again from the release folder (if the files in place are not a complete set, it refuses until you put $(recorded_programs) from that folder into $PREFIX and wgal-watchdog.service over $UNIT_FILE, each matching its new sum in the record); or leave everything as it is and hand it to the owner. $autostart !!!" >&2
        return 1
    fi
    if [ "$in_place" = mixed ]; then
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! $UNIT was NOT started: the files in place are neither the complete previous version nor the complete new one. The previous ones are kept in $PREFIX as .<name>.old, the new ones as .<name>.new, their sums in $(record_path). Fix the cause, put the previous version back yourself (move each .<name>.old still in $PREFIX over its file, .wgal-watchdog.service.old over $UNIT_FILE, and remove each program the record lists with - as its old sum), and run this script again: until then it refuses to run, so as not to lose these copies. $autostart !!!" >&2
        return 1
    fi
    if [ "$in_place" = new ] && ! mark_started; then
        # From here on it is started: a rerun must know, so it is not
        # started unless the record says so (removed with the copies).
        local ran="This run did not start the new version."
        [ "$NEW_START_BEGUN" -eq 0 ] ||
            ran="This run had started the new version, so it may already have run and changed the stored state."
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! $UNIT was NOT started: the new version is in place, complete, but $(record_path) could not be marked to say that the new version may have run. A later run reads that mark to know that the new version may have changed the stored state, so the new version is not started without it. $ran The copies in $PREFIX and their record are kept. Leave the new version in place (do not put the .<name>.old copies back): fix the cause ($PREFIX must be writable: free disk space, or a file system that is not read-only), then run this script again from the release folder; or leave everything as it is and hand it to the owner. $autostart !!!" >&2
        return 1
    fi
    if [ "$in_place" = old ]; then
        echo "install.sh: the previous version is in place, complete. Starting $UNIT again with it; its stored state is as it left it (nothing was copied back)." >&2
    elif [ "$NEW_START_BEGUN" -eq 1 ] || [ "$EARLIER_MAY_HAVE_RUN" -eq 1 ]; then
        echo "install.sh: the new version is in place, complete. The failure came after its start had begun (in this run or in an earlier one), so it may already have run and changed the stored state: the previous version is not put back. Starting $UNIT with the new version again." >&2
    else
        echo "install.sh: the new version is in place, complete; it has not run yet. Starting $UNIT with it." >&2
    fi
    systemctl daemon-reload || echo "install.sh: systemctl daemon-reload failed; systemd may still use the unit as it read it before" >&2
    # A set this script starts also starts at boot: an earlier run that did
    # not start a set turned that off (disable_autostart).
    systemctl enable "$UNIT" >/dev/null ||
        echo "install.sh: systemctl enable $UNIT failed; it may not start at the next boot" >&2
    systemctl start "$UNIT" || echo "install.sh: systemctl start $UNIT failed" >&2
    remove_staged
    wait_running
}

# The window's menu entry and its start at login, from the release folder
# $1.
install_window_entries() {
    install -d -o root -g root -m 0755 "$(dirname "$DESKTOP_FILE")" "$(dirname "$AUTOSTART_FILE")"
    install -o root -g root -m 0644 "$1/wgal.desktop" "$DESKTOP_FILE"
    install -o root -g root -m 0644 "$1/wgal.desktop" "$AUTOSTART_FILE"
}

# --no-window: WGAL's own menu entry and start at login, from an earlier
# install, would open a window this computer cannot show. rm never follows
# a link.
remove_window_entries() {
    rm -f -- "$DESKTOP_FILE" "$AUTOSTART_FILE"
}

# Links CLI_LINK to the command line in PREFIX, once the new set is in
# place: only where nothing is there, or kept when the link already points
# to it. Anything else there is not WGAL's and is left alone. Prints the
# command to run the command line with: wgal, or its full path.
link_cli() {
    local target="$PREFIX/wgal"
    if [ -L "$CLI_LINK" ] && [ "$(readlink -- "$CLI_LINK")" = "$target" ]; then
        echo wgal
        return 0
    fi
    if [ -e "$CLI_LINK" ] || [ -L "$CLI_LINK" ]; then
        echo "install.sh: note: $CLI_LINK is there and is not WGAL's link; it is left as it is. Run WGAL's command line as $target" >&2
        echo "$target"
        return 0
    fi
    if [ -d "$(dirname "$CLI_LINK")" ] && ln -s -- "$target" "$CLI_LINK" 2>/dev/null; then
        echo wgal
    else
        echo "install.sh: note: $CLI_LINK could not be made; run WGAL's command line as $target" >&2
        echo "$target"
    fi
}

# Waits up to 20 s for the service to run with its socket.
wait_running() {
    local waited=0
    until [ -S "$SOCKET" ] && [ "$(stat -c %u "$SOCKET")" -eq 0 ] && systemctl is-active --quiet "$UNIT"; do
        if [ "$waited" -ge 20 ]; then
            echo "install.sh: the service is not running with its socket after 20 s:" >&2
            systemctl --no-pager status "$UNIT" >&2 || true
            journalctl --no-pager -n 20 -u "$UNIT" >&2 || true
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    echo "$UNIT is running; its socket is $SOCKET"
}

# Compares the wg-quick units with WG_BEFORE, says how it went and ends the
# script: $1 is 1 when the service runs.
finish() {
    local status=0 after
    [ "$1" -eq 1 ] || status=1
    if ! after=$(wireguard_states); then
        echo "install.sh: WARNING: systemctl could not list the wg-quick units at the end," \
            "so the script cannot confirm they are as it found them" >&2
        status=2
    elif [ "$WG_BEFORE" != "$after" ]; then
        echo "install.sh: WARNING: a wg-quick unit changed while the script ran (the script" \
            "itself never touches them):" >&2
        diff <(echo "$WG_BEFORE") <(echo "$after") >&2 || true
        status=2
    fi
    if [ "$UPGRADE_FAILED" -eq 1 ]; then
        if [ "$1" -eq 1 ]; then
            echo "install.sh: !!! the upgrade did not complete; $UNIT runs the $RECOVERED_SET version. Fix the cause shown above and run this script again. !!!" >&2
            [ "$status" -ne 0 ] || status=4
        else
            echo "install.sh: !!! the upgrade did not complete and $UNIT is not running: nothing monitors the tunnels now. Fix the cause shown above and run this script again. !!!" >&2
        fi
    elif [ "$status" -eq 0 ]; then
        local who=${INSTALL_USER:-the account on the access list}
        local cli=${CLI_COMMAND:-$PREFIX/wgal}
        if [ "$NO_WINDOW" -eq 1 ]; then
            echo "Done, without the window. As $who (not with sudo): \"$cli status\" shows WGAL,"
            echo "\"$cli --help\" the rest. The service's log: journalctl -u $UNIT"
        else
            echo "Done. Open WG Always Live from the applications menu as $who (not with sudo);"
            echo "it also starts at login. Its command line: \"$cli --help\"."
            echo "The service's log: journalctl -u $UNIT"
        fi
    fi
    exit "$status"
}

main() {
    if [ "${1:-}" = --list-tunnels ]; then
        [ $# -eq 1 ] || die "--list-tunnels takes no other option; nothing was changed" 64
        list_tunnels
        exit 0
    fi
    local -a tunnels=()
    local user=""
    local install_runtime=0
    while [ $# -gt 0 ]; do
        case "$1" in
        --tunnel)
            [ $# -ge 2 ] || usage
            # Split on commas without pathname expansion.
            local -a parts=()
            IFS=, read -r -a parts <<<"$2"
            local name
            for name in "${parts[@]}"; do
                [ -n "$name" ] && tunnels+=("$name")
            done
            shift 2
            ;;
        --user)
            [ $# -ge 2 ] || usage
            user=$2
            shift 2
            ;;
        --install-runtime)
            install_runtime=1
            shift
            ;;
        --no-window)
            NO_WINDOW=1
            shift
            ;;
        -h | --help) usage 0 ;;
        *) usage ;;
        esac
    done
    if [ "$NO_WINDOW" -eq 1 ] && [ "$install_runtime" -eq 1 ]; then
        die "--no-window installs no window, so it takes no --install-runtime; nothing was changed" 64
    fi

    # --- Checks; nothing changes before they pass ----------------------------
    [ "$(id -u)" -eq 0 ] || die "run it with sudo (as root)"
    systemd_running || die "systemd is not running this system"
    local here
    here=${RELEASE_DIR:-$(cd "$(dirname "$0")" && pwd -P)}
    local program
    for program in "${PROGRAMS[@]}"; do
        if [ ! -f "$here/$program" ] || [ -L "$here/$program" ]; then
            die "$here/$program is missing (run the script from the unpacked release folder)"
        fi
        [ "$(head -c 4 "$here/$program" | od -An -tx1 | tr -d ' \n')" = 7f454c46 ] ||
            die "$here/$program is not a Linux program"
    done
    for program in wgal-watchdog.service wgal.desktop wgal.png uninstall.sh; do
        [ -f "$here/$program" ] || die "$here/$program is missing"
    done
    local tool
    for tool in /usr/bin/systemctl /usr/sbin/ip /usr/bin/ping; do
        [ -x "$tool" ] || die "$tool is missing; WGAL needs it"
    done
    local -a missing=()
    mapfile -t missing < <(window_packages_missing)
    if [ "${#missing[@]}" -gt 0 ] && [ "$install_runtime" -eq 0 ]; then
        echo "install.sh: the WGAL window needs these Ubuntu packages, which are not installed:" \
            "${missing[*]}" >&2
        if [ -t 0 ] && [ -t 1 ]; then
            local answer=""
            # End of input (Ctrl-D) is a no.
            read -r -p "Install them now with apt-get? [y/N] " answer || answer=
            [[ "$answer" =~ ^[yY] ]] && install_runtime=1
        fi
        [ "$install_runtime" -eq 1 ] ||
            die "nothing was changed. Install them first (sudo apt install ${missing[*]}), or run again with --install-runtime, or with --no-window on a computer without a desktop" 65
    fi
    if [ ! -x /usr/bin/wg ] || [ ! -x /usr/bin/wg-quick ]; then
        echo "install.sh: warning: wireguard-tools (wg, wg-quick) is not installed; WGAL watches" \
            "wg-quick tunnels and will find none until it is" >&2
    fi

    local upgrade=0
    [ -e "$UNIT_FILE" ] && upgrade=1
    # An access list already there (an upgrade, or a reinstall over data
    # an earlier uninstall kept) is kept unless --tunnel or --user is given.
    local write_access=1
    if [ -e "$ACCESS_FILE" ] && [ "${#tunnels[@]}" -eq 0 ] && [ -z "$user" ]; then
        write_access=0
    fi
    local uid=""
    if [ "$write_access" -eq 1 ]; then
        # Only an access list being written names an account; a kept one
        # keeps its own, so the closing line does not guess one.
        [ -z "$user" ] && user=${SUDO_USER:-}
        [ "${#tunnels[@]}" -gt 0 ] ||
            die "give at least one --tunnel <name>: the tunnels the WGAL user may register (on an upgrade, --tunnel rewrites the whole access list)" 64
        [ -n "$user" ] || die "give --user <name>: the account that uses the WGAL window" 64
        uid=$(id -u -- "$user" 2>/dev/null) || die "there is no user $user"
        [ "$uid" -ne 0 ] || die "the WGAL window must not run as root; give --user <your account>" 64
        local -A seen=()
        for name in "${tunnels[@]}"; do
            valid_tunnel "$name" || die "'$name' is not a wg-quick interface name WGAL accepts" 64
            [ -z "${seen[$name]:-}" ] || die "tunnel '$name' is given twice" 64
            seen[$name]=1
            [ -f "/etc/wireguard/$name.conf" ] ||
                die "there is no /etc/wireguard/$name.conf; add the tunnel to WireGuard first"
        done
    fi
    # The data folder and every folder the upgrade's backup reads or writes
    # must be root's real folders: a link there could make the backup copy
    # into, or prune, something outside WGAL.
    local folder
    for folder in "$STATE_DIR" "$STATE_DIR/state" "$STATE_DIR/notify" "$STATE_DIR/backup"; do
        # Refused unless every part is proven: an owner or a mode that
        # stat cannot read refuses too.
        if [ -e "$folder" ] || [ -L "$folder" ]; then
            local owner="" mode=""
            owner=$(stat -c %u "$folder" 2>/dev/null) || owner=""
            mode=$(stat -c %a "$folder" 2>/dev/null) || mode=""
            if ! { [ -d "$folder" ] && [ ! -L "$folder" ] && [ "$owner" = 0 ] &&
                [[ "$mode" =~ ^[0-7]{3,4}$ ]] && [ $((8#$mode & 8#022)) -eq 0 ]; }; then
                die "$folder is not root's folder (or is a link, or others can write to it); check and remove it, then run again"
            fi
        fi
    done
    for program in "$PREFIX" "$ACCESS_DIR"; do
        if [ -e "$program" ] || [ -L "$program" ]; then
            local owner=""
            owner=$(stat -c %u "$program" 2>/dev/null) || owner=""
            if ! { [ -d "$program" ] && [ ! -L "$program" ] && [ "$owner" = 0 ]; }; then
                die "$program is not root's folder (or is a link); check and remove it, then run again"
            fi
        fi
    done
    if pgrep -x wgal-desktop >/dev/null 2>&1; then
        echo "install.sh: note: a WGAL window is open; it keeps the old version until it is" \
            "quit and opened again" >&2
    fi

    WG_BEFORE=$(wireguard_states) ||
        die "systemctl could not list the wg-quick units; nothing was changed"
    INSTALL_USER=$user

    # --- The window's packages, when missing and the user agreed ------------
    if [ "${#missing[@]}" -gt 0 ]; then
        echo "Installing ${missing[*]} with apt-get ..."
        # A fixed list of packages, never one given to the script.
        # --no-remove: a transaction that would remove any installed package
        # is refused rather than carried out.
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-remove -- "${missing[@]}" ||
            die "apt-get could not install ${missing[*]}; nothing of WGAL was changed (try: sudo apt update)"
        mapfile -t missing < <(missing_runtime)
        [ "${#missing[@]}" -eq 0 ] ||
            die "still not installed after apt-get: ${missing[*]}; nothing of WGAL was changed"
    fi

    # --- Prepared before the service stops ------------------------------------
    # The new set beside the old one, and copies of the old one; the menu
    # entry, autostart and the files only the window reads; the new access
    # list as its own file.
    stage_set "$here"
    echo "Staged the new programs and unit in $PREFIX (.<name>.new; copies of those in place: .<name>.old)"
    install -o root -g root -m 0644 "$here/wgal.png" "$PREFIX/wgal.png"
    install -o root -g root -m 0755 "$here/uninstall.sh" "$PREFIX/uninstall.sh"
    local readme
    for readme in README.md README.zh-TW.md; do
        if [ -f "$here/$readme" ]; then
            install -o root -g root -m 0644 "$here/$readme" "$PREFIX/$readme"
        fi
    done
    if [ "$NO_WINDOW" -eq 1 ]; then
        remove_window_entries
    else
        install_window_entries "$here"
    fi
    if [ "$write_access" -eq 1 ]; then
        stage_access "$uid" "${tunnels[@]}"
    fi

    # --- Upgrade: stop, back up, replace, start; recovery on any failure
    # until the started service is checked ------------------------------------
    local running=0
    if [ "$upgrade" -eq 1 ]; then
        upgrade_window
        running=$WINDOW_RUNNING
    else
        # A plain command, so set -e stays on inside it.
        first_install
        running=$FIRST_RUNNING
    fi
    # The new set is in place (a failure before this ended the script in
    # the trap, which makes no link).
    CLI_COMMAND=$(link_cli)
    if [ "$write_access" -eq 1 ]; then
        echo "Access list written: user $user ($uid), tunnels ${tunnels[*]}"
    fi
    remove_staged
    finish "$running"
}

# Sourced (the tests in tests/ do this): only the functions are defined.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
