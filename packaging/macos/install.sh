#!/bin/bash
# Installs or upgrades WG Always Live (WGAL) on macOS from this folder, for
# the signed-in user.
#
#   ./install.sh
#
# Run it as yourself, not with sudo: the watchdog is your own LaunchAgent
# and runs with your rights only. It asks for an administrator's password
# only when it cannot write to /Applications by itself.
#
# Quit the WGAL window first. Everything is checked before anything
# changes. Then, before anything stops, it copies "WG Always Live.app" (the
# window, with wgal-watchdog, wgal-mcp and wgal, its command line, inside)
# to /Applications under
# the hidden name ".WG Always Live.app.new", checks the copy against a
# record of the release's app (every file's SHA-256, every folder and
# link), keeps that record in its data folder, removes the download
# quarantine from the copy alone (it is not signed by an Apple developer
# ID; see README.zh-TW.md), and writes and checks the two LaunchAgents
# (the watchdog, and the window at login) in its data folder, with copies
# of the ones in place. Then it stops the earlier watchdog, copies its
# stored state (not the bot token, which is in the Keychain) into
# ~/Library/Application Support/WGAL/backup/state-<UTC time>, keeping the
# newest five, renames the app in place to ".WG Always Live.app.old" and
# the new one into its place, moves the LaunchAgents into
# ~/Library/LaunchAgents, starts them and checks the watchdog. The
# watchdog keeps its data in ~/Library/Application Support/WGAL and its
# log in ~/Library/Logs/WGAL/watchdog.log; an upgrade keeps both. A copy
# of uninstall.sh is kept in that folder too.
#
# If anything fails from the stop until the started watchdog has been
# checked (Ctrl-C included), a trap finds out which complete app is in
# place: the previous one, or the new one, judged by the record alone, so
# the release folder is no longer needed. Before the new one was started,
# with the previous one renamed away and the new one not in place, it
# renames the previous one back. Once the start has begun, the new
# version may already have run and changed the stored state, so nothing
# is put back: it starts the new one again. It puts the matching
# LaunchAgents in place, starts the watchdog, checks it, and says which
# version runs. It never copies stored state back. With no complete app
# it leaves the watchdog stopped and says so; once the new version may
# have run, it never starts the previous app, and says to put the complete
# new app in place (or to hand it to the owner), never to rename the
# previous one back. A new version that started and does not run (no
# socket) is not rolled back: it may already have changed the stored
# state.
#
# The staging folder (upgrade/) says in its file "started" whether the new
# watchdog may have been started; it is removed once a complete app has
# been started. A later run that finds it saying anything but "no" (or
# missing) treats the new version as one that may have run: it never
# starts the previous app either. A later run keeps the folder and writes
# that file first (never removing the folder to make it again), and the
# new watchdog is started (the first time, or again by the trap) only
# after the file has been read back saying "yes"; if it cannot be, the
# watchdog is left stopped, everything is kept, and the script says so.
# Whenever it leaves the watchdog stopped like this, it also disables the
# watchdog's LaunchAgent label (launchctl disable, a lasting override in
# launchd, for WGAL's own label only) and checks it, so that launchd does
# not start that app's watchdog at the next login, and says so (or that it
# could not, in which case launchd may start it at the next login: run the
# script again first); the next run that gets past its checks enables it
# again before it loads the watchdog (launchd refuses to load a disabled
# one), as does the trap whenever it starts one.
# A run that finds the previous app an earlier run renamed away (".WG
# Always Live.app.old") changes nothing (65) unless the app in place is
# the complete new one that run recorded: the previous app may be the only
# complete one.
#
# It never connects, disconnects or changes a VPN configuration of the
# official WireGuard app: it records `scutil --nc list` before and after and
# reports any difference.
#
# Exit codes: 0 done; 1 the watchdog is not running with its socket; 2 a VPN
# configuration changed while the script ran; 4 a step of the upgrade
# failed after the watchdog was stopped, and the watchdog runs again with
# the complete app then in place (only when neither 1 nor 2 applies); 64
# wrong usage; 65 a check or a preparation failed before the watchdog was
# stopped: the watchdog and the app in place are as they were, but the
# data and log folders, the uninstaller there, the staging folder
# (upgrade/) and the hidden ".WG Always Live.app.new" may already be new.
set -euo pipefail
umask 022
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

readonly APP_NAME="WG Always Live.app"
readonly LABEL=io.github.hsinken.wg-always-live.watchdog
readonly WINDOW_LABEL=io.github.hsinken.wg-always-live.window
readonly PROGRAMS=(wgal-desktop wgal-watchdog wgal-mcp wgal)
readonly KEEP_BACKUPS=5
# The paths are fixed here and in main and nowhere else; they are not
# readonly only so that tests/install-recovery.test.sh, which sources this
# file, can point them at its own scratch folder. The staged and the kept
# app are hidden names next to the app, so each swap is one rename.
APP="/Applications/$APP_NAME"
NEW_APP="/Applications/.$APP_NAME.new"
OLD_APP="/Applications/.$APP_NAME.old"
# Set by main, read by the functions and the EXIT trap.
HERE=""
USER_ID=""
DATA=""
LOGS=""
AGENTS=""
SOCKET=""
STAGE=""
PLIST=""
WINDOW_PLIST=""
WG_BEFORE=""
WINDOW_STEP=""
UPGRADE_FAILED=0
RECOVERED_SET=""
# The record of the new app (app_record), made before anything stops.
NEW_RECORD=""
# Set once the start of the new watchdog has begun: from then on it may
# have run, and nothing is put back.
NEW_MAY_HAVE_RUN=0
# Set when an earlier run may have started the new watchdog
# (earlier_may_have_run), and when this run has called its start.
EARLIER_MAY_HAVE_RUN=0
NEW_START_BEGUN=0
# 1 when the watchdog started by upgrade_window runs with its socket.
WINDOW_RUNNING=0

die() {
    echo "install.sh: $1" >&2
    exit "${2:-65}"
}

# Runs a command as this user when it can write to /Applications, else with
# sudo, which asks for an administrator's password.
as_needed() {
    if [ -w /Applications ]; then
        "$@"
    else
        sudo "$@"
    fi
}

# Writes the two LaunchAgents into the staging folder.
write_plists() {
    cat >"$STAGE/watchdog.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$LABEL</string>
	<key>ProgramArguments</key>
	<array>
		<string>$APP/Contents/MacOS/wgal-watchdog</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>ExitTimeOut</key>
	<integer>45</integer>
	<key>StandardOutPath</key>
	<string>$LOGS/watchdog.log</string>
	<key>StandardErrorPath</key>
	<string>$LOGS/watchdog.log</string>
</dict>
</plist>
EOF
    cat >"$STAGE/window.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$WINDOW_LABEL</string>
	<key>ProgramArguments</key>
	<array>
		<string>/usr/bin/open</string>
		<string>-g</string>
		<string>-a</string>
		<string>$APP</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
EOF
    chmod 0644 "$STAGE/watchdog.plist" "$STAGE/window.plist"
}

# The contents of the app folder $1, one line per entry in a fixed order:
# "d <path>" for a folder, "l <path> <target>" for a link, "f <SHA-256>
# <path>" for a file. Fails (non-zero, and its output is no record) for a
# missing folder or a link, when the listing, a link or a file cannot be
# read in full, and for anything else in the folder; a caller uses the
# record only when it succeeded, never by set -e alone.
app_record() {
    { [ -d "$1" ] && [ ! -L "$1" ]; } || return 1
    (
        cd -- "$1" || exit 1
        list=$(find . -print) || exit 1
        list=$(printf '%s\n' "$list" | LC_ALL=C sort) || exit 1
        while IFS= read -r path; do
            if [ -L "$path" ]; then
                target=$(readlink "$path") || exit 1
                printf 'l %s %s\n' "$path" "$target"
            elif [ -d "$path" ]; then
                printf 'd %s\n' "$path"
            elif [ -f "$path" ]; then
                sum=$(shasum -a 256 <"$path") || exit 1
                sum=${sum%% *}
                [ "${#sum}" -eq 64 ] || exit 1
                printf 'f %s %s\n' "$sum" "$path"
            else
                exit 1
            fi
        done <<<"$list"
    )
}

# True when the app folder $1 has the contents recorded before the stop
# (NEW_RECORD); the release folder is not read again.
app_is_new() {
    local record
    record=$(app_record "$1") || return 1
    [ -n "$NEW_RECORD" ] && [ "$record" = "$NEW_RECORD" ]
}

# True unless the staging folder an earlier run left says, in its file
# "started", that the new watchdog had not been started ("no"). With no
# staging folder there was no unfinished run. A folder without that file,
# or with anything else in it, cannot tell, so it counts as started: the
# new version may have run and changed the stored state.
earlier_may_have_run() {
    { [ -d "$STAGE" ] && [ ! -L "$STAGE" ]; } || return 1
    if [ -f "$STAGE/started" ] && [ ! -L "$STAGE/started" ] &&
        [ "$(cat -- "$STAGE/started" 2>/dev/null)" = no ]; then
        return 1
    fi
    return 0
}

# Writes the staging folder's file "started" ($1: yes or no) beside it and
# renames it into place, then reads it back: fails unless it now says $1.
write_started() {
    rm -f -- "$STAGE/started.tmp" || return 1
    echo "$1" >"$STAGE/started.tmp" || return 1
    mv -f -- "$STAGE/started.tmp" "$STAGE/started" || return 1
    [ -f "$STAGE/started" ] && [ ! -L "$STAGE/started" ] &&
        [ "$(cat -- "$STAGE/started" 2>/dev/null)" = "$1" ]
}

# An earlier run that failed after renaming the app in place away may have
# left it as $OLD_APP on purpose: it may be the only complete app. Stops
# the script before anything changes unless the app in place is the
# complete new one that run recorded (then the earlier copies are removed
# below), so stage_upgrade never removes the only complete app.
check_earlier_copies() {
    [ -e "$OLD_APP" ] || [ -L "$OLD_APP" ] || return 0
    local record="" now=""
    if [ -d "$STAGE" ] && [ ! -L "$STAGE" ] && [ -f "$STAGE/app.record" ] && [ ! -L "$STAGE/app.record" ]; then
        record=$(cat -- "$STAGE/app.record")
    fi
    if [ -z "$record" ] || ! now=$(app_record "$APP") || [ "$now" != "$record" ]; then
        if earlier_may_have_run; then
            die "an earlier run of this script did not finish: \"$OLD_APP\" (hidden) is the previous app it renamed away, $APP is not the complete new app it recorded (or is missing), and that run may already have started the new watchdog, so the new version may already have run and changed the stored state. Nothing was changed and the watchdog was not stopped. Do not rename the previous app back: put the complete new app in place yourself (move aside whatever is at $APP, then copy \"$APP_NAME\" from that run's release folder there with ditto, with sudo if needed), then run this script again from that release folder; or leave everything as it is and hand it to the owner"
        fi
        die "an earlier run of this script did not finish: \"$OLD_APP\" (hidden) is the previous app it renamed away, and $APP is not the complete new app it recorded (or is missing). The previous app may be the only complete one, so nothing was changed and the watchdog was not stopped. Rename it back yourself (move aside whatever is at $APP, then: mv \"$OLD_APP\" \"$APP\", with sudo if needed), then run this script again"
    fi
    echo "An earlier run left the previous app as $OLD_APP; the complete new app it recorded is in place, so it is removed"
}

# Everything the stop and the start enclose, prepared first: WGAL's own
# folders, the new app under its hidden name, the LaunchAgents, and copies
# of the LaunchAgents in place. Nothing that runs is touched.
stage_upgrade() {
    check_earlier_copies
    # The record the trap judges the app in place by, made before the stop
    # and before any copy an earlier run kept is removed.
    if ! NEW_RECORD=$(app_record "$HERE/$APP_NAME") || [ -z "$NEW_RECORD" ]; then
        die "$HERE/$APP_NAME cannot be read completely; the earlier watchdog was not stopped"
    fi
    # An earlier run that may have started the new watchdog: this run, too,
    # never starts the previous one again (recover_from).
    if earlier_may_have_run; then
        NEW_MAY_HAVE_RUN=1
        EARLIER_MAY_HAVE_RUN=1
        echo "install.sh: an earlier run of this script did not finish and may already have started the new watchdog: if this run fails, the previous app is not started again" >&2
    fi
    mkdir -p "$AGENTS" "$LOGS" "$DATA"
    # The log folder and the watchdog's own folder are private (the
    # watchdog refuses a data folder others can enter); the uninstaller is
    # kept in the latter.
    chmod 0700 "$LOGS" "$DATA"
    install -m 0755 "$HERE/uninstall.sh" "$DATA/uninstall.sh"
    # An earlier run's staging folder is kept, never removed and made
    # again: its file "started" may be all that says the new version may
    # have run, and a run that stops here must leave it for the next one.
    if [ -e "$STAGE" ] || [ -L "$STAGE" ]; then
        { [ -d "$STAGE" ] && [ ! -L "$STAGE" ]; } ||
            die "$STAGE is not a folder (or is a link); check and remove it, then run again"
        chmod 0700 "$STAGE"
    else
        mkdir -m 0700 "$STAGE"
    fi
    if [ -e "$STAGE/started" ] || [ -L "$STAGE/started" ]; then
        { [ -f "$STAGE/started" ] && [ ! -L "$STAGE/started" ]; } ||
            die "$STAGE/started is not a plain file (or is a link); check and remove it, then run again"
    fi
    # Whether the new watchdog may have been started, for a later run,
    # written first: "yes" from the start on (upgrade_window), or carried
    # from an earlier run that may have started it.
    local mark=no
    [ "$NEW_MAY_HAVE_RUN" -eq 0 ] || mark=yes
    write_started "$mark" ||
        die "$STAGE/started could not be written; the earlier watchdog was not stopped"
    local path
    for path in "$NEW_APP" "$OLD_APP"; do
        # Left by an earlier run, as check_earlier_copies allowed; removed
        # before the record it was checked against.
        if [ -e "$path" ] || [ -L "$path" ]; then
            { [ -d "$path" ] && [ ! -L "$path" ]; } ||
                die "$path is not a folder (or is a link); check and remove it, then run again"
            as_needed rm -rf -- "$path"
        fi
    done
    # The rest of an earlier run's staging is made anew.
    rm -rf -- "$STAGE/app.record" "$STAGE/watchdog.plist" "$STAGE/window.plist" \
        "$STAGE/watchdog.plist.old" "$STAGE/window.plist.old"
    write_plists
    plutil -lint "$STAGE/watchdog.plist" "$STAGE/window.plist" >/dev/null
    if [ -f "$PLIST" ]; then
        cp -p -- "$PLIST" "$STAGE/watchdog.plist.old"
    fi
    if [ -f "$WINDOW_PLIST" ]; then
        cp -p -- "$WINDOW_PLIST" "$STAGE/window.plist.old"
    fi
    printf '%s\n' "$NEW_RECORD" >"$STAGE/app.record"
    as_needed ditto "$HERE/$APP_NAME" "$NEW_APP"
    # Only this app's own download quarantine; Gatekeeper is left as it is.
    as_needed xattr -dr com.apple.quarantine "$NEW_APP" 2>/dev/null || true
    app_is_new "$NEW_APP" ||
        die "the copy $NEW_APP does not match $HERE/$APP_NAME; the earlier watchdog was not stopped"
}

stop_watchdog() {
    if launchctl print "gui/$USER_ID/$LABEL" >/dev/null 2>&1; then
        launchctl bootout "gui/$USER_ID/$LABEL" ||
            die "stopping the running watchdog failed; no WGAL file was replaced"
    fi
    local waited=0
    while pgrep -x -u "$USER_ID" wgal-watchdog >/dev/null 2>&1; do
        [ "$waited" -lt 50 ] ||
            die "the watchdog is still running 50 s after its stop; no WGAL file was replaced (its LaunchAgent may already be unloaded)"
        sleep 1
        waited=$((waited + 1))
    done
    if launchctl print "gui/$USER_ID/$WINDOW_LABEL" >/dev/null 2>&1; then
        launchctl bootout "gui/$USER_ID/$WINDOW_LABEL" || true
    fi
    # A socket the ended watchdog left behind: removed, so the new one that
    # appears below proves the new watchdog serves it.
    if [ -S "$SOCKET" ]; then
        rm -f -- "$SOCKET"
    fi
}

# Copies the stored state into a new backup folder, as on Windows and
# Ubuntu; call it only while the watchdog is stopped. Keeps the newest
# $KEEP_BACKUPS: real folders named state-<time>, never a link.
backup_state() {
    [ -d "$DATA/state" ] || return 0
    local stamp backup file folder count=0
    stamp=$(date -u +%Y%m%d-%H%M%SZ)
    backup="$DATA/backup/state-$stamp"
    if [ ! -d "$DATA/backup" ]; then
        mkdir -m 0700 "$DATA/backup"
    fi
    # Fails if it exists: an earlier run in the same second.
    mkdir -m 0700 "$backup" "$backup/state" "$backup/notify"
    for file in state/state.json state/state.prev.json notify/notify.json; do
        if [ -f "$DATA/$file" ] && [ ! -L "$DATA/$file" ]; then
            cp -p -- "$DATA/$file" "$backup/$file"
        fi
    done
    echo "Stored state copied to $backup"
    for folder in "$DATA/backup"/state-*; do
        if [ -d "$folder" ] && [ ! -L "$folder" ]; then
            count=$((count + 1))
        fi
    done
    # The glob lists them oldest first: the names are UTC times.
    for folder in "$DATA/backup"/state-*; do
        [ "$count" -gt "$KEEP_BACKUPS" ] || break
        if [ -d "$folder" ] && [ ! -L "$folder" ]; then
            rm -rf -- "$folder"
            count=$((count - 1))
        fi
    done
}

# The new app and LaunchAgents into place: each one rename.
swap_in() {
    WINDOW_STEP="replacing the app"
    if [ -e "$APP" ]; then
        as_needed mv -- "$APP" "$OLD_APP"
    fi
    as_needed mv -- "$NEW_APP" "$APP"
    WINDOW_STEP="replacing the LaunchAgents"
    mv -f -- "$STAGE/watchdog.plist" "$PLIST"
    mv -f -- "$STAGE/window.plist" "$WINDOW_PLIST"
}

# Turns off the start at login of WGAL's own watchdog: a lasting override
# in launchd for its label, so that launchd does not load it at the next
# login and run the app in place, which this run refused to start; then
# checks it. Starts and stops nothing. Prints the sentence the "NOT
# started" message carries: that it was turned off and how it is turned
# back on, or that it could not be.
disable_autostart() {
    launchctl disable "gui/$USER_ID/$LABEL" >&2 || true
    if launchctl print-disabled "gui/$USER_ID" 2>/dev/null |
        grep -F -e "\"$LABEL\" => disabled" -e "\"$LABEL\" => true" >/dev/null; then
        echo "Its start at login was turned off (launchctl disable gui/$USER_ID/$LABEL), so launchd does not start it when you next log in either; the next run of this script that gets past its checks turns it back on."
    else
        echo "Its start at login could NOT be turned off (launchctl print-disabled gui/$USER_ID does not list $LABEL as disabled): launchd may start the watchdog, from the app now in place, when you next log in. Fix the cause and run this script again (or hand everything to the owner) before you log out or restart this Mac."
    fi
}

# Starts the LaunchAgents in place, each only when it is not loaded yet.
# The watchdog's label is enabled first: a run that did not start the
# watchdog disabled it (disable_autostart), and launchd refuses to load a
# disabled one.
start_agents() {
    launchctl enable "gui/$USER_ID/$LABEL" || echo "install.sh: launchctl enable of the watchdog failed" >&2
    if ! launchctl print "gui/$USER_ID/$LABEL" >/dev/null 2>&1; then
        launchctl bootstrap "gui/$USER_ID" "$PLIST" || echo "install.sh: launchctl bootstrap of the watchdog failed" >&2
    fi
    if ! launchctl print "gui/$USER_ID/$WINDOW_LABEL" >/dev/null 2>&1; then
        launchctl bootstrap "gui/$USER_ID" "$WINDOW_PLIST" || true
    fi
}

# Stops the earlier watchdog, backs up its state, puts the new app and
# LaunchAgents in place, starts them and checks the watchdog, with the
# trap armed until the check is done: any failure, or Ctrl-C, ends in
# on_window_exit. Sets WINDOW_RUNNING.
upgrade_window() {
    trap on_window_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
    WINDOW_STEP="stopping the earlier watchdog"
    stop_watchdog
    WINDOW_STEP="backing up the stored state"
    backup_state
    swap_in
    # From here on the new version may run and change the stored state: a
    # failure no longer puts the previous one back.
    NEW_MAY_HAVE_RUN=1
    WINDOW_STEP="starting the new watchdog"
    # Kept for a rerun until the staging folder is removed; not started
    # unless the file says so.
    write_started yes || {
        echo "install.sh: $STAGE/started could not be written \"yes\"" >&2
        exit 1
    }
    NEW_START_BEGUN=1
    start_agents
    WINDOW_RUNNING=0
    wait_running && WINDOW_RUNNING=1
    WINDOW_STEP=""
    trap - EXIT INT TERM HUP
}

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

# Which complete app is in place: "new", "old" (the previous one, never
# renamed away), or "none".
app_in_place() {
    if app_is_new "$APP"; then
        echo new
    elif [ -d "$APP" ] && [ ! -L "$APP" ] && [ ! -e "$OLD_APP" ]; then
        echo old
    else
        echo none
    fi
}

# Puts the LaunchAgents of version $1 (new or old) in place: the staged
# ones not moved yet, or the copies of the previous ones.
plists_for() {
    local kind=$1 name dest
    for name in watchdog window; do
        dest=$PLIST
        [ "$name" = watchdog ] || dest=$WINDOW_PLIST
        if [ "$kind" = new ] && [ -f "$STAGE/$name.plist" ]; then
            mv -f -- "$STAGE/$name.plist" "$dest"
        elif [ "$kind" = old ] && [ ! -f "$STAGE/$name.plist" ] && [ -f "$STAGE/$name.plist.old" ]; then
            mv -f -- "$STAGE/$name.plist.old" "$dest"
        fi
    done
}

# After a failed step: makes sure a complete app is in place, starts its
# watchdog and checks it, and says what happened. Never touches the
# stored state. Once the new watchdog's start had begun, puts nothing
# back: it starts the new one again. Returns 0 when the watchdog runs.
recover_from() {
    local step=$WINDOW_STEP in_place
    WINDOW_STEP=""
    UPGRADE_FAILED=1
    echo "install.sh: !!! the upgrade failed while $step (exit status $1) !!!" >&2
    in_place=$(app_in_place)
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ]; then
        : # The new version may have run: the previous one is not put back.
    elif [ "$in_place" = none ] && [ ! -e "$APP" ] && [ -d "$OLD_APP" ]; then
        echo "install.sh: the previous app was renamed away and the new one is not in place; renaming the previous one back" >&2
        as_needed mv -- "$OLD_APP" "$APP"
        in_place=$(app_in_place)
    elif [ "$in_place" = none ] && [ ! -e "$APP" ] && [ -d "$NEW_APP" ]; then
        echo "install.sh: there is no previous app; putting the new one in place" >&2
        as_needed mv -- "$NEW_APP" "$APP"
        in_place=$(app_in_place)
    fi
    RECOVERED_SET=$in_place
    if [ "$NEW_MAY_HAVE_RUN" -eq 1 ] && [ "$in_place" != new ]; then
        # The previous app is never started on a state the new one may have
        # changed; the staging folder keeps saying so for a rerun.
        local what="$APP is not the complete new app"
        [ "$in_place" = none ] || what="the previous app is in place, not the new one"
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! the watchdog was NOT started: $what, and the new watchdog's start had begun (in this run or in an earlier one), so the new version may already have run and changed the stored state: do not put the previous app back, and do not start it. Whatever is left is kept: in /Applications \".$APP_NAME.old\" (hidden; the previous app, if this run renamed it away) and the staging folder $STAGE. Fix the cause and put the complete new app in place: run this script again from the release folder (if $APP is not the complete new app and \".$APP_NAME.old\" is there, it refuses until you move aside whatever is at $APP and copy the release's \"$APP_NAME\" there with ditto, with sudo if needed); or leave everything as it is and hand it to the owner. $autostart !!!" >&2
        return 1
    fi
    if [ "$in_place" = none ]; then
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! the watchdog was NOT started: $APP is neither the complete previous version nor the complete new one. Whatever is left is in /Applications as \".$APP_NAME.old\" (the previous app) and \".$APP_NAME.new\" (hidden). Fix the cause, rename \".$APP_NAME.old\" back to \"$APP_NAME\" yourself (with sudo if needed), and run this script again: until then it refuses to run, so as not to lose the previous app. $autostart !!!" >&2
        return 1
    fi
    if [ "$in_place" = new ] && ! write_started yes; then
        # From here on it is started: a rerun must know, so it is not
        # started unless the file says so (removed with the copies).
        local ran="This run did not start the new watchdog."
        [ "$NEW_START_BEGUN" -eq 0 ] ||
            ran="This run had started the new watchdog, so it may already have run and changed the stored state."
        local autostart
        autostart=$(disable_autostart)
        echo "install.sh: !!! the watchdog was NOT started: the new app is in place, complete, but $STAGE/started could not be written to say that the new version may have run. A later run reads it to know that the new version may have changed the stored state, so the new watchdog is not started without it. $ran Whatever is left is kept: in /Applications \".$APP_NAME.old\" (hidden; the previous app, if this run renamed it away) and the staging folder $STAGE. Leave the new app in place (do not rename the previous app back): fix the cause (the staging folder must be writable: free disk space), then run this script again from the release folder; or leave everything as it is and hand it to the owner. $autostart !!!" >&2
        return 1
    fi
    plists_for "$in_place"
    if [ "$in_place" = old ]; then
        echo "install.sh: the previous app is in place, complete. Starting its watchdog again; its stored state is as it left it (nothing was copied back)." >&2
    elif [ "$NEW_START_BEGUN" -eq 1 ] || [ "$EARLIER_MAY_HAVE_RUN" -eq 1 ]; then
        echo "install.sh: the new app is in place, complete. The failure came after its start had begun (in this run or in an earlier one), so it may already have run and changed the stored state: the previous app is not put back. Starting its watchdog again." >&2
    else
        echo "install.sh: the new app is in place, complete; it has not run yet. Starting its watchdog." >&2
    fi
    start_agents
    remove_staged
    wait_running
}

# Removes the staged and the kept app and the staging folder, once a
# complete app is in place. The staging folder, with the record a rerun
# checks the app in place against, stays while a kept app does.
remove_staged() {
    local path kept=0
    for path in "$NEW_APP" "$OLD_APP"; do
        if [ -d "$path" ] && [ ! -L "$path" ]; then
            if ! as_needed rm -rf -- "$path"; then
                echo "install.sh: could not remove $path; remove it yourself" >&2
                kept=1
            fi
        fi
    done
    [ "$kept" -eq 1 ] || rm -rf -- "$STAGE"
}

# Waits up to 20 s for the watchdog to run with its socket.
wait_running() {
    local waited=0
    until [ -S "$SOCKET" ] && [ "$(stat -f %u "$SOCKET")" -eq "$USER_ID" ] &&
        launchctl print "gui/$USER_ID/$LABEL" 2>/dev/null | grep 'state = running' >/dev/null; do
        if [ "$waited" -ge 20 ]; then
            echo "install.sh: the watchdog is not running with its socket after 20 s:" >&2
            launchctl print "gui/$USER_ID/$LABEL" 2>&1 | head -n 20 >&2 || true
            tail -n 20 "$LOGS/watchdog.log" >&2 2>/dev/null || true
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    echo "The watchdog is running; its socket is $SOCKET"
}

# Compares the VPN configurations with WG_BEFORE, says how it went and ends
# the script: $1 is 1 when the watchdog runs.
finish() {
    local status=0 after
    [ "$1" -eq 1 ] || status=1
    after=$(scutil --nc list 2>/dev/null || true)
    if [ "$WG_BEFORE" != "$after" ]; then
        echo "install.sh: WARNING: a VPN configuration changed while the script ran (the script" \
            "itself never touches them):" >&2
        diff <(echo "$WG_BEFORE") <(echo "$after") >&2 || true
        if [ "$status" -eq 0 ]; then
            status=2
        fi
    fi
    if [ "$UPGRADE_FAILED" -eq 1 ]; then
        if [ "$1" -eq 1 ]; then
            echo "install.sh: !!! the upgrade did not complete; the watchdog runs the $RECOVERED_SET version. Fix the cause shown above and run this script again. !!!" >&2
            [ "$status" -ne 0 ] || status=4
        else
            echo "install.sh: !!! the upgrade did not complete and the watchdog is not running: nothing monitors the tunnels now. Fix the cause shown above and run this script again. !!!" >&2
        fi
    elif [ "$status" -eq 0 ]; then
        echo "Done. WG Always Live opens now and at every login. The watchdog's log:"
        echo "  $LOGS/watchdog.log"
        echo "Its command line (the PATH is not changed): \"$APP/Contents/MacOS/wgal\" --help"
    fi
    exit "$status"
}

main() {
    [ $# -eq 0 ] || die "it takes no arguments" 64
    [ "$(uname -s)" = Darwin ] || die "this is the macOS installer"
    local uid
    uid=$(id -u)
    [ "$uid" -ne 0 ] || die "run it as yourself, not with sudo: the watchdog is your own LaunchAgent"
    local home=$HOME
    case "$home" in
    /*) ;;
    *) die "your home folder is not an absolute path" ;;
    esac
    case "$home" in
    *[\<\>\&\"\']*) die "your home folder's path has a character a LaunchAgent file cannot hold" ;;
    esac
    local data="$home/Library/Application Support/WGAL"
    local socket="$data/watchdog.sock"
    local logs="$home/Library/Logs/WGAL"
    local agents="$home/Library/LaunchAgents"
    # macOS keeps at most 103 bytes of a socket's path (bytes, not
    # characters).
    local socket_bytes
    socket_bytes=$(printf %s "$socket" | LC_ALL=C wc -c | tr -d ' ')
    [ "$socket_bytes" -le 103 ] || die "your home folder's path is too long for the watchdog's socket"
    # LaunchAgents live in the signed-in user's desktop session.
    launchctl print "gui/$uid" >/dev/null 2>&1 ||
        die "your desktop session is not available: sign in on the Mac and run it in Terminal"

    # --- Checks; nothing changes before they pass ----------------------------
    local here
    here=$(cd "$(dirname "$0")" && pwd -P)
    local program
    for program in "${PROGRAMS[@]}"; do
        [ -f "$here/$APP_NAME/Contents/MacOS/$program" ] ||
            die "$here/$APP_NAME/Contents/MacOS/$program is missing (run the script from the unpacked release folder)"
    done
    [ -f "$here/uninstall.sh" ] || die "$here/uninstall.sh is missing"
    if [ -e "$APP" ] || [ -L "$APP" ]; then
        if [ ! -d "$APP" ] || [ -L "$APP" ]; then
            die "$APP is not an app folder (or is a link); check and remove it, then run again"
        fi
    fi
    # The data and log folders, made private below, must be the user's own
    # real folders: refused unless proven, a stat that fails refuses too.
    local folder
    for folder in "$data" "$logs"; do
        if [ -e "$folder" ] || [ -L "$folder" ]; then
            local owner=""
            owner=$(stat -f %u "$folder" 2>/dev/null) || owner=""
            if ! { [ -d "$folder" ] && [ ! -L "$folder" ] && [ "$owner" = "$uid" ]; }; then
                die "$folder is not your own folder (or is a link); check and remove it, then run again"
            fi
        fi
    done
    # The folders the backup reads or writes must be real folders: a link
    # there could make it copy into, or prune, something outside WGAL.
    for folder in "$data/state" "$data/notify" "$data/backup"; do
        if [ -e "$folder" ] || [ -L "$folder" ]; then
            if [ ! -d "$folder" ] || [ -L "$folder" ]; then
                die "$folder is not a folder (or is a link); check and remove it, then run again"
            fi
        fi
    done
    if pgrep -x -u "$uid" wgal-desktop >/dev/null 2>&1; then
        die "a WGAL window is open; quit it first (its menu bar menu)"
    fi
    if [ ! -d /Applications/WireGuard.app ]; then
        echo "install.sh: warning: the official WireGuard app is not in /Applications; WGAL watches" \
            "its VPN configurations and will find none until it is installed (App Store)" >&2
    fi

    HERE=$here
    USER_ID=$uid
    DATA=$data
    LOGS=$logs
    AGENTS=$agents
    SOCKET=$socket
    STAGE="$data/upgrade"
    PLIST="$agents/$LABEL.plist"
    WINDOW_PLIST="$agents/$WINDOW_LABEL.plist"
    WG_BEFORE=$(scutil --nc list 2>/dev/null || true)

    # --- Prepared before the earlier watchdog stops ------------------------------
    stage_upgrade
    echo "Staged the new app as $NEW_APP"

    # --- Stop, back up, replace, start; recovery on any failure until the
    # started watchdog is checked ----------------------------------------------
    upgrade_window
    echo "Installed $APP"
    remove_staged
    finish "$WINDOW_RUNNING"
}

# Sourced (the tests in tests/ do this): only the functions are defined.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
