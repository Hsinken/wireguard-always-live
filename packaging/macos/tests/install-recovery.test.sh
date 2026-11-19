#!/bin/bash
# Tests install.sh's upgrade recovery (FIX-05): every step between stopping
# the earlier watchdog and starting the new one is made to fail in turn,
# and the test checks that the trap puts a complete app and its
# LaunchAgents in place, starts the watchdog (or refuses to, with the right
# message), never touches the stored state, and ends with the right exit
# code. It also checks the state backup an upgrade now makes.
#
#   packaging/macos/tests/install-recovery.test.sh
#
# It is no test of launchd or the real tools: it runs install.sh's
# functions in any bash (written with WSL's bash 5; since 2026-11-13 it
# also runs with macOS's own bash 3.2 and BSD tools), with stand-ins
# defined here for launchctl,
# pgrep, sleep, ditto, xattr, plutil, scutil, sudo (as_needed) and the
# socket wait; mv, rm, readlink, find and shasum are stand-ins that can
# fail. install.sh is sourced (it
# then runs nothing) and its paths point at a new scratch folder. The
# stand-ins exist only in this file: install.sh has no option or variable
# that selects them.
set -uo pipefail

tests=$(cd "$(dirname "$0")" && pwd -P)
[ "$(id -u)" -ne 0 ] || {
    echo "install-recovery.test.sh: run it as a normal user, not as root" >&2
    exit 64
}
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=install.sh
source "$tests/../install.sh"
set +e

root=$(mktemp -d "${TMPDIR:-/tmp}/wgal-fix05-macos.XXXXXX")
trap 'rm -rf -- "$root"' EXIT
release=$root/release
out=$root/out
calls=$root/calls
starts=$root/starts

# --- Stand-ins -----------------------------------------------------------------
FAIL=""          # bootout, still-running, interrupt, backup, app-aside, app-in,
                 # plist-1, plist-2, bad-copy, plist-source-gone (the release's
                 # app is gone, then a LaunchAgent fails), bootstrap-interrupt,
                 # wait-interrupt (Ctrl-C while the new watchdog starts),
                 # start-breaks (the new app is damaged as its watchdog starts,
                 # then Ctrl-C), mark-locked (the staging folder is made
                 # read-only once the apps are swapped: "started" can be
                 # neither rewritten nor removed), stage-cleared-fails and
                 # stage-cleared-interrupt (staging fails, or Ctrl-C, right
                 # after an earlier run's staging contents are removed)
FAIL_RESTORE=0
FIND_FAILS=""    # partial (lists part of an app, then fails) or late (lists
                 # all of it, then fails); only the listing app_record makes
SHASUM_FAILS=0
SHASUM_SHORT=0   # shasum prints a sum that is not 64 characters
READLINK_FAILS=0
FAIL_MARK_WRITES=0  # fail this many renames of "started" saying yes
START_RUNS=1
VPN_CHANGES=0
LOADED=1         # the watchdog's LaunchAgent
WINDOW_LOADED=1  # the window's
FAIL_DISABLE=""  # fail (launchctl disable fails) or ignored (it changes nothing)
OLD_FORMAT=0     # print-disabled answers "=> true" as older macOS does
# Whether the watchdog's label is disabled in launchd (a lasting override),
# as the launchctl stand-in keeps it: enabled or disabled.
override=$root/override

launchctl() {
    echo "launchctl $*" >>"$calls"
    case "$1" in
    print)
        case "$2" in
        */"$LABEL") [ "$LOADED" -eq 1 ] ;;
        */"$WINDOW_LABEL") [ "$WINDOW_LOADED" -eq 1 ] ;;
        *) return 0 ;;
        esac
        ;;
    bootout)
        if [ "$2" != "gui/$USER_ID/$LABEL" ]; then
            WINDOW_LOADED=0
            return 0
        fi
        [ "$FAIL" != bootout ] || return 1
        if [ "$FAIL" = interrupt ]; then
            kill -INT "$BASHPID"
        fi
        LOADED=0
        ;;
    enable)
        [ "$2" != "gui/$USER_ID/$LABEL" ] || echo enabled >"$override"
        ;;
    disable)
        [ "$FAIL_DISABLE" != fail ] || return 1
        if [ "$FAIL_DISABLE" != ignored ] && [ "$2" = "gui/$USER_ID/$LABEL" ]; then
            echo disabled >"$override"
        fi
        ;;
    print-disabled)
        local word
        word=$(cat -- "$override")
        if [ "$OLD_FORMAT" -eq 1 ]; then
            word=false
            [ "$(cat -- "$override")" != disabled ] || word=true
        fi
        printf 'disabled services = {\n\t"com.example.other" => enabled\n\t"%s" => %s\n}\n' "$LABEL" "$word"
        ;;
    bootstrap)
        if [ "$3" != "$PLIST" ]; then
            WINDOW_LOADED=1
            return 0
        fi
        # launchd refuses to load a disabled service.
        if [ "$(cat -- "$override")" = disabled ]; then
            echo "Bootstrap failed: 5: Input/output error" >&2
            return 5
        fi
        # Which watchdog starts, and what "started" says then.
        echo "$(cat -- "$APP/Contents/MacOS/wgal-watchdog" 2>/dev/null)|$(cat -- "$STAGE/started" 2>/dev/null || echo '<none>')" >>"$starts"
        if [ "$FAIL" = bootstrap-interrupt ] && [ ! -e "$root/failed-once" ]; then
            : >"$root/failed-once"
            kill -INT "$BASHPID"
        fi
        if [ "$FAIL" = start-breaks ] && [ ! -e "$root/failed-once" ]; then
            : >"$root/failed-once"
            echo damaged >"$APP/Contents/MacOS/wgal-mcp"
            kill -INT "$BASHPID"
        fi
        LOADED=1
        ;;
    esac
}
pgrep() { [ "$FAIL" = still-running ]; }
sleep() { :; }
xattr() { :; }
plutil() { :; }
as_needed() { "$@"; }
ditto() {
    command cp -R -- "$1" "$2"
    if [ "$FAIL" = bad-copy ]; then
        echo damaged >"$2/Contents/MacOS/wgal-mcp"
    fi
}
scutil() {
    if [ "$VPN_CHANGES" -eq 1 ] && [ -n "$WG_BEFORE" ]; then
        echo '* (Disconnected) 00000000-0000-0000-0000-000000000001 VPN (com.wireguard.macos) "office" [VPN/com.wireguard.macos]'
    else
        echo '* (Connected) 00000000-0000-0000-0000-000000000001 VPN (com.wireguard.macos) "office" [VPN/com.wireguard.macos]'
    fi
}
find() {
    if [ "$1" = . ] && [ -n "$FIND_FAILS" ]; then
        if [ "$FIND_FAILS" = partial ]; then
            printf '.\n./Contents\n'
        else
            command find "$@"
        fi
        return 1
    fi
    command find "$@"
}
shasum() {
    [ "$SHASUM_FAILS" -eq 0 ] || return 1
    if [ "$SHASUM_SHORT" -eq 1 ]; then
        echo "0123abcd  -"
        return 0
    fi
    command shasum "$@"
}
readlink() {
    [ "$READLINK_FAILS" -eq 0 ] || return 1
    command readlink "$@"
}
rm() {
    local arg cleared=0
    for arg in "$@"; do
        if [ "$arg" = "$STAGE" ] || [ "$arg" = "$STAGE/app.record" ]; then
            cleared=1
        fi
    done
    command rm "$@" || return
    if [ "$cleared" -eq 1 ] && [ "$FAIL" = stage-cleared-fails ]; then
        return 1
    fi
    if [ "$cleared" -eq 1 ] && [ "$FAIL" = stage-cleared-interrupt ]; then
        kill -INT "$BASHPID"
    fi
}
wait_running() {
    echo "wait_running" >>"$calls"
    if [ "$FAIL" = wait-interrupt ] && [ "$(calls_of wait_running)" -eq 1 ]; then
        kill -INT "$BASHPID"
    fi
    [ "$START_RUNS" -eq 1 ] && [ "$LOADED" -eq 1 ]
}
mv() {
    local source=${*: -2:1} dest=${*: -1}
    if [ "$FAIL_MARK_WRITES" -gt 0 ] && [ "$source" = "$STAGE/started.tmp" ] && grep -qx yes "$source"; then
        FAIL_MARK_WRITES=$((FAIL_MARK_WRITES - 1))
        return 1
    fi
    if [ "$FAIL" = app-aside ] && [ "$source" = "$APP" ]; then
        return 1
    fi
    if [ "$FAIL" = app-in ] && [ "$source" = "$NEW_APP" ] && [ ! -e "$root/failed-once" ]; then
        : >"$root/failed-once"
        return 1
    fi
    if [ "$FAIL_RESTORE" -eq 1 ] && [ "$source" = "$OLD_APP" ]; then
        return 1
    fi
    if [ "$FAIL" = plist-source-gone ] && [ "$dest" = "$PLIST" ] && [ ! -e "$root/failed-once" ]; then
        : >"$root/failed-once"
        command rm -rf -- "$release/$APP_NAME"
        return 1
    fi
    if [ "$FAIL" = plist-1 ] && [ "$dest" = "$PLIST" ] && [ ! -e "$root/failed-once" ]; then
        : >"$root/failed-once"
        return 1
    fi
    if [ "$FAIL" = plist-2 ] && [ "$dest" = "$WINDOW_PLIST" ] && [ ! -e "$root/failed-once" ]; then
        : >"$root/failed-once"
        return 1
    fi
    command mv "$@"
}
backup_state_real=$(declare -f backup_state)
eval "${backup_state_real/backup_state/backup_state_real}"
backup_state() {
    [ "$FAIL" != backup ] || return 1
    backup_state_real
}
swap_in_real=$(declare -f swap_in)
eval "${swap_in_real/swap_in/swap_in_real}"
swap_in() {
    swap_in_real
    if [ "$FAIL" = mark-locked ]; then
        chmod 0444 "$STAGE/started"
        chmod 0500 "$STAGE"
    fi
}

# --- Fixture ---------------------------------------------------------------------
HERE=$release
USER_ID=501
DATA="$root/home/Library/Application Support/WGAL"
LOGS=$root/home/Library/Logs/WGAL
AGENTS=$root/home/Library/LaunchAgents
SOCKET=$DATA/watchdog.sock
STAGE=$DATA/upgrade
PLIST=$AGENTS/$LABEL.plist
WINDOW_PLIST=$AGENTS/$WINDOW_LABEL.plist
APP="$root/Applications/$APP_NAME"
NEW_APP="$root/Applications/.$APP_NAME.new"
OLD_APP="$root/Applications/.$APP_NAME.old"

make_app() {
    local where=$1 which=$2 program
    command mkdir -p "$where/Contents/MacOS"
    for program in "${PROGRAMS[@]}"; do
        echo "$which $program" >"$where/Contents/MacOS/$program"
    done
    echo "$which info" >"$where/Contents/Info.plist"
}

reset_fixture() {
    [ ! -d "$STAGE" ] || chmod 0700 "$STAGE"
    rm -rf -- "${root:?}/release" "${root:?}/Applications" "${root:?}/home" "${root:?}/failed-once"
    make_app "$release/$APP_NAME" new
    echo "uninstaller" >"$release/uninstall.sh"
    command mkdir -p "$root/Applications" "$AGENTS" "$DATA/state" "$DATA/notify" "$LOGS"
    if [ "${1:-}" != first ]; then
        make_app "$APP" old
        echo "old watchdog plist" >"$PLIST"
        echo "old window plist" >"$WINDOW_PLIST"
        echo "stored state" >"$DATA/state/state.json"
        LOADED=1
        WINDOW_LOADED=1
    else
        LOADED=0
        WINDOW_LOADED=0
    fi
    : >"$calls"
    : >"$starts"
    FAIL=""
    FAIL_RESTORE=0
    FAIL_DISABLE=""
    OLD_FORMAT=0
    echo enabled >"$override"
    FIND_FAILS=""
    SHASUM_FAILS=0
    SHASUM_SHORT=0
    READLINK_FAILS=0
    FAIL_MARK_WRITES=0
    START_RUNS=1
    VPN_CHANGES=0
    WINDOW_STEP=""
    UPGRADE_FAILED=0
    RECOVERED_SET=""
    WG_BEFORE=""
    NEW_RECORD=""
    NEW_MAY_HAVE_RUN=0
    WINDOW_RUNNING=0
}

# What main does after its checks; the exit code goes to $code and
# everything said to $out.
run_install() {
    (
        set -euo pipefail
        WG_BEFORE=$(scutil --nc list 2>/dev/null || true)
        stage_upgrade
        upgrade_window
        echo "WINDOW DONE"
        remove_staged
        finish "$WINDOW_RUNNING"
    ) >"$out" 2>&1
    code=$?
}

# A rerun's staging alone, before anything stops; the exit code goes to
# $code and everything said to $out.
run_staging() {
    (
        set -euo pipefail
        stage_upgrade
    ) >"$out" 2>&1
    code=$?
}

# --- Checks ----------------------------------------------------------------------
passed=0
failed=0
# A file's permission bits in octal, with GNU stat (Linux, where the tests
# usually run) or BSD stat (macOS, the script's own system).
mode_of() {
    stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"
}

ok() {
    local what=$1
    shift
    if "$@"; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        echo "FAIL: $what" >&2
    fi
}
is_text() { [ "$(cat -- "$1" 2>/dev/null || echo '<none>')" = "$2" ]; }
said() { grep -qF -- "$1" "$out"; }
not_said() { ! grep -qF -- "$1" "$out"; }
calls_of() { grep -cxF -- "$1" "$calls"; }
app_is() {
    local program
    for program in "${PROGRAMS[@]}"; do
        is_text "$APP/Contents/MacOS/$program" "$1 $program" || return 1
    done
    is_text "$APP/Contents/Info.plist" "$1 info"
}
new_plists() {
    grep -qF "<string>$APP/Contents/MacOS/wgal-watchdog</string>" "$PLIST" &&
        grep -qF "<string>$WINDOW_LABEL</string>" "$WINDOW_PLIST"
}
old_plists() { is_text "$PLIST" "old watchdog plist" && is_text "$WINDOW_PLIST" "old window plist"; }
no_staged() { [ ! -e "$NEW_APP" ] && [ ! -e "$OLD_APP" ] && [ ! -e "$STAGE" ]; }
state_untouched() { is_text "$DATA/state/state.json" "stored state"; }
started_once() { [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 1 ]; }
# Every start so far was safe: the new watchdog only with "started" saying
# yes, and the previous one never once the new one had started.
starts_safe() {
    local line new_ran=0
    while IFS= read -r line; do
        case "$line" in
        "new wgal-watchdog|yes") new_ran=1 ;;
        "new wgal-watchdog|"*) return 1 ;;
        *) [ "$new_ran" -eq 0 ] || return 1 ;;
        esac
    done <"$starts"
}
starts_seen() { tr '\n' ',' <"$starts"; }
label_is() { is_text "$override" "$1"; }
# Every launchctl disable was for the watchdog's own label.
disables_only_wgal() { ! grep '^launchctl disable' "$calls" | grep -qvxF "launchctl disable gui/$USER_ID/$LABEL"; }
# Nothing was loaded after the last launchctl disable, and there was one.
none_started_after_disable() {
    grep -qxF "launchctl disable gui/$USER_ID/$LABEL" "$calls" &&
        ! awk -v d="launchctl disable gui/$USER_ID/$LABEL" -v b="launchctl bootstrap gui/$USER_ID $PLIST" \
            '$0 == d {seen = 1; bad = 0; next} seen && $0 == b {bad = 1} END {exit !bad}' "$calls"
}
# The label was enabled before the watchdog was loaded, and it was loaded.
enabled_before_start() {
    awk -v e="launchctl enable gui/$USER_ID/$LABEL" -v b="launchctl bootstrap gui/$USER_ID $PLIST" \
        '$0 == e {on = 1; next} $0 == b {started = 1; if (!on) bad = 1} END {exit bad || !started}' "$calls"
}
# A run that left the watchdog stopped disabled its label and said so.
autostart_off() {
    local name=$1
    ok "$name: the label is disabled" label_is disabled
    ok "$name: says its start at login was turned off" said "Its start at login was turned off"
    ok "$name: says the next run turns it back on" said "the next run of this script that gets past its checks turns it back on"
    ok "$name: nothing loaded after the disable" none_started_after_disable
    ok "$name: only the watchdog's label was disabled" disables_only_wgal
}
# A run that started the watchdog left its label enabled.
autostart_kept() {
    local name=$1
    ok "$name: the label stays enabled" label_is enabled
    ok "$name: never disabled" [ "$(grep -c '^launchctl disable' "$calls")" -eq 0 ]
    ok "$name: start at login not mentioned" not_said "start at login"
}
show() { sed 's/^/    | /' "$out" >&2; }

# --- An upgrade from an app without the command line (before 3.0.0): the
# new app, with it, is in place ------------------------------------------------
reset_fixture
rm -f -- "$APP/Contents/MacOS/wgal"
run_install
before=$failed
ok "no old wgal: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "no old wgal: the new app is in place" app_is new
ok "no old wgal: it has the command line" is_text "$APP/Contents/MacOS/wgal" "new wgal"
[ "$failed" -eq "$before" ] || show

# --- ... and when the new app is not moved in, the previous one is renamed
# back as it was, without the command line ------------------------------------
reset_fixture
rm -f -- "$APP/Contents/MacOS/wgal"
FAIL=app-in
run_install
before=$failed
ok "no old wgal, new app not moved in: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "no old wgal, new app not moved in: the old watchdog is back" is_text "$APP/Contents/MacOS/wgal-watchdog" "old wgal-watchdog"
ok "no old wgal, new app not moved in: the old wgal-mcp is back" is_text "$APP/Contents/MacOS/wgal-mcp" "old wgal-mcp"
ok "no old wgal, new app not moved in: no command line, as before" [ ! -e "$APP/Contents/MacOS/wgal" ]
ok "no old wgal, new app not moved in: started once" started_once
[ "$failed" -eq "$before" ] || show

# --- The whole run works ---------------------------------------------------------
reset_fixture
run_install
before=$failed
ok "success: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "success: the window finished" said "WINDOW DONE"
ok "success: the new app is in place" app_is new
ok "success: the new LaunchAgents are in place" new_plists
ok "success: nothing staged is left" no_staged
ok "success: the state was backed up" [ -f "$(find "$DATA/backup" -path '*/state/state.json' | head -n 1)" ]
ok "success: the backup folder is private" [ "$(mode_of "$DATA/backup")" = 700 ]
ok "success: the state copy matches" is_text "$(find "$DATA/backup" -path '*/state/state.json' | head -n 1)" "stored state"
ok "success: the uninstaller is kept" is_text "$DATA/uninstall.sh" "uninstaller"
ok "success: started once" started_once
ok "success: no failure said" not_said "failed while"
autostart_kept success
ok "success: enabled before it was loaded" enabled_before_start
[ "$failed" -eq "$before" ] || show

# --- First install ---------------------------------------------------------------
reset_fixture first
run_install
ok "first install: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "first install: the new app is in place" app_is new
ok "first install: the LaunchAgents" new_plists
ok "first install: nothing staged is left" no_staged

# --- The staged copy is wrong: refused before anything stops ---------------------
reset_fixture
FAIL=bad-copy
run_install
ok "bad copy: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "bad copy: says so" said "does not match"
ok "bad copy: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
ok "bad copy: the old app is untouched" app_is old

# --- A step fails before the app is replaced -------------------------------------
for case in "bootout|stopping the earlier watchdog" "still-running|stopping the earlier watchdog" \
    "interrupt|stopping the earlier watchdog" "backup|backing up the stored state" \
    "app-aside|replacing the app"; do
    reset_fixture
    FAIL=${case%%|*}
    step=${case#*|}
    run_install
    name="fail at $FAIL"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the step is named" said "the upgrade failed while $step"
    ok "$name: the old app is in place" app_is old
    ok "$name: the old LaunchAgents are in place" old_plists
    ok "$name: says the previous app runs" said "the previous app is in place, complete"
    if [ "$FAIL" = bootout ] || [ "$FAIL" = interrupt ]; then
        # The bootout did not happen: still loaded, so not loaded again.
        ok "$name: still loaded, not loaded again" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 0 ]
        ok "$name: checked" [ "$(calls_of wait_running)" -eq 1 ]
    else
        ok "$name: started once" started_once
    fi
    ok "$name: the closing line" said "runs the old version"
    ok "$name: the window did not go on" not_said "WINDOW DONE"
    ok "$name: the state is untouched" state_untouched
    ok "$name: nothing staged is left" no_staged
    autostart_kept "$name"
    [ "$failed" -eq "$before" ] || show
done

# --- The old app renamed away, the new one not in: renamed back --------------
reset_fixture
FAIL=app-in
run_install
before=$failed
ok "new app not moved in: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "new app not moved in: says it renames back" said "renaming the previous one back"
ok "new app not moved in: the old app is back" app_is old
ok "new app not moved in: the old LaunchAgents" old_plists
ok "new app not moved in: started once" started_once
ok "new app not moved in: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- First install, the new app not moved in: moved in by the recovery --------
reset_fixture first
FAIL=app-in
run_install
ok "first install, app not moved in: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "first install, app not moved in: the new app is in place" app_is new
ok "first install, app not moved in: the LaunchAgents" new_plists

# --- The app is new, a LaunchAgent is not: the new set, not yet run -----------
for FAIL_AT in plist-1 plist-2; do
    reset_fixture
    FAIL=$FAIL_AT
    run_install
    name="$FAIL_AT fails"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the step is named" said "the upgrade failed while replacing the LaunchAgents"
    ok "$name: the new app is in place" app_is new
    ok "$name: the new LaunchAgents are in place" new_plists
    ok "$name: says the new app has not run" said "the new app is in place, complete; it has not run yet"
    ok "$name: started once" started_once
    ok "$name: the closing line" said "runs the new version"
    ok "$name: nothing staged is left" no_staged
    [ "$failed" -eq "$before" ] || show
done

# --- The release's app is gone after staging and a LaunchAgent fails (P2-4):
# the app in place is judged by the record, not the release ------------------------
reset_fixture
FAIL=plist-source-gone
run_install
before=$failed
ok "source gone, LaunchAgent fails: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "source gone, LaunchAgent fails: the release's app is gone" [ ! -e "$release/$APP_NAME" ]
ok "source gone, LaunchAgent fails: the new app is in place" app_is new
ok "source gone, LaunchAgent fails: the new LaunchAgents are in place" new_plists
ok "source gone, LaunchAgent fails: says the new app has not run" said "the new app is in place, complete; it has not run yet"
ok "source gone, LaunchAgent fails: started once" started_once
ok "source gone, LaunchAgent fails: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- Ctrl-C while the new watchdog starts (P2-1): it may have run, so it is
# started again and the previous app is not put back ------------------------------
for FAIL_AT in bootstrap-interrupt wait-interrupt; do
    reset_fixture
    FAIL=$FAIL_AT
    run_install
    name="$FAIL_AT"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the step is named" said "the upgrade failed while starting the new watchdog"
    ok "$name: says the new app may have run" said "it may already have run"
    ok "$name: says nothing is put back" said "the previous app is not put back"
    ok "$name: nothing is renamed back" not_said "renaming the previous one back"
    ok "$name: the new app is in place" app_is new
    ok "$name: the new LaunchAgents are in place" new_plists
    ok "$name: checked again by the recovery" [ "$(calls_of wait_running)" -eq "$([ "$FAIL_AT" = wait-interrupt ] && echo 2 || echo 1)" ]
    autostart_kept "$name"
    ok "$name: the closing line" said "runs the new version"
    ok "$name: the window did not go on" not_said "WINDOW DONE"
    ok "$name: the state is untouched" state_untouched
    ok "$name: nothing staged is left" no_staged
    [ "$failed" -eq "$before" ] || show
done
reset_fixture
FAIL=bootstrap-interrupt
run_install
ok "bootstrap-interrupt: loaded again by the recovery" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 2 ]
reset_fixture
FAIL=wait-interrupt
run_install
ok "wait-interrupt: still loaded, not loaded twice" started_once

# --- The new watchdog starts but does not run: reported, not rolled back -----
reset_fixture
START_RUNS=0
run_install
before=$failed
ok "new watchdog does not run: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "new watchdog does not run: the new app stays" app_is new
ok "new watchdog does not run: no recovery" not_said "failed while"
[ "$failed" -eq "$before" ] || show

# --- No complete app can be made: not started -----------------------------------
reset_fixture
FAIL=app-in
FAIL_RESTORE=1
run_install
before=$failed
ok "no complete app: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "no complete app: says it was not started" said "the watchdog was NOT started"
ok "no complete app: never started" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 0 ]
ok "no complete app: says nothing monitors" said "nothing monitors the tunnels now"
ok "no complete app: the previous app is kept" [ -d "$OLD_APP" ]
ok "no complete app: the new app is kept" [ -d "$NEW_APP" ]
ok "no complete app: says a rerun refuses until it is renamed back" said "until then it refuses to run"
autostart_off "no complete app"
[ "$failed" -eq "$before" ] || show

# --- ... then a rerun, whose staging fails again (P2-2): refused before the
# only complete app is removed ---------------------------------------------------------
FAIL=bad-copy
FAIL_RESTORE=0
: >"$calls"
run_staging
before=$failed
ok "rerun after a failed recovery: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "rerun after a failed recovery: says the earlier run did not finish" said "an earlier run of this script did not finish"
ok "rerun after a failed recovery: says how to rename it back" said "Rename it back yourself"
for program in "${PROGRAMS[@]}"; do
    ok "rerun after a failed recovery: the previous app keeps its $program" is_text "$OLD_APP/Contents/MacOS/$program" "old $program"
done
ok "rerun after a failed recovery: the new app copy is kept" [ -d "$NEW_APP" ]
ok "rerun after a failed recovery: never stopped anything" [ "$(grep -c bootout "$calls")" -eq 0 ]
ok "rerun after a failed recovery: its start at login stays off" label_is disabled
[ "$failed" -eq "$before" ] || show

# --- ... and once it is renamed back, a rerun goes on ---------------------------
command mv -- "$OLD_APP" "$APP"
FAIL=""
rm -f -- "$root/failed-once"
# The first run backed up the state in this same second.
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
before=$failed
ok "rerun after renaming back: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "rerun after renaming back: its start at login is on again" label_is enabled
ok "rerun after renaming back: enabled before it was loaded" enabled_before_start
ok "rerun after renaming back: the new app is in place" app_is new
ok "rerun after renaming back: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- An earlier run swapped the apps and stopped there: the recorded new app
# is in place, so its leftovers are removed; any other app there is refused --
for variant in recorded changed; do
    reset_fixture
    run_staging
    command mv -- "$APP" "$OLD_APP"
    command mv -- "$NEW_APP" "$APP"
    if [ "$variant" = changed ]; then
        echo "changed" >"$APP/Contents/MacOS/wgal-mcp"
    fi
    run_install
    name="earlier run stopped after the swap, app $variant"
    before=$failed
    if [ "$variant" = recorded ]; then
        ok "$name: exit 0 (got $code)" [ "$code" -eq 0 ]
        ok "$name: says the recorded app is in place" said "the complete new app it recorded is in place"
        ok "$name: the new app is in place" app_is new
        ok "$name: nothing staged is left" no_staged
    else
        ok "$name: exit 65 (got $code)" [ "$code" -eq 65 ]
        ok "$name: the previous app is kept" is_text "$OLD_APP/Contents/MacOS/wgal-watchdog" "old wgal-watchdog"
        ok "$name: never stopped anything" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
    fi
    [ "$failed" -eq "$before" ] || show
done

# --- A listing that fails partway (P2-5): never a record, so staging
# refuses before anything stops, and nothing is copied -----------------------
for how in partial late; do
    reset_fixture
    FIND_FAILS=$how
    run_install
    name="the app's listing fails ($how)"
    before=$failed
    ok "$name: exit 65 (got $code)" [ "$code" -eq 65 ]
    ok "$name: says the app cannot be read" said "cannot be read completely"
    ok "$name: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
    ok "$name: the old app is untouched" app_is old
    ok "$name: nothing was copied" [ ! -e "$NEW_APP" ]
    [ "$failed" -eq "$before" ] || show
done
reset_fixture
SHASUM_FAILS=1
run_install
ok "a file's hash fails: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "a file's hash fails: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]

# --- The record's other failures (P3-1): a link that cannot be read, a sum
# that is not 64 characters, and an entry that is neither a folder, a file
# nor a link (a named pipe): never a record, so staging refuses before
# anything stops -------------------------------------------------------------------
for how in readlink short-sum fifo; do
    reset_fixture
    case "$how" in
    readlink)
        ln -s Info.plist "$release/$APP_NAME/Contents/Link"
        READLINK_FAILS=1
        ;;
    short-sum) SHASUM_SHORT=1 ;;
    fifo) mkfifo "$release/$APP_NAME/Contents/pipe" ;;
    esac
    ( app_record "$release/$APP_NAME" >/dev/null )
    ok "the record fails ($how): app_record fails" [ $? -ne 0 ]
    run_install
    name="the record fails ($how)"
    before=$failed
    ok "$name: exit 65 (got $code)" [ "$code" -eq 65 ]
    ok "$name: says the app cannot be read" said "cannot be read completely"
    ok "$name: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
    ok "$name: the old app is untouched" app_is old
    ok "$name: nothing was copied" [ ! -e "$NEW_APP" ]
    [ "$failed" -eq "$before" ] || show
done
# ... and the same link, read: a record with it
reset_fixture
ln -s Info.plist "$release/$APP_NAME/Contents/Link"
records_link() { [ "$(app_record "$release/$APP_NAME" | grep -c '^l ./Contents/Link Info.plist$')" -eq 1 ]; }
ok "a readable link: recorded" records_link

# --- ... and with an earlier run's kept app beside the recorded new app, a
# listing that fails after it listed everything: refused, the kept app stays -
reset_fixture
run_staging
command mv -- "$APP" "$OLD_APP"
command mv -- "$NEW_APP" "$APP"
FIND_FAILS=late
: >"$calls"
run_install
before=$failed
ok "kept app, listing fails: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "kept app, listing fails: the kept app stays" is_text "$OLD_APP/Contents/MacOS/wgal-watchdog" "old wgal-watchdog"
ok "kept app, listing fails: the app in place stays" app_is new
ok "kept app, listing fails: never stopped" [ "$(grep -c bootout "$calls")" -eq 0 ]
[ "$failed" -eq "$before" ] || show

# --- The new watchdog started, then the new app is found damaged (P2-6): the
# previous app is not put back, and the message says to restore the new one -
start_breaks() {
    reset_fixture
    FAIL=start-breaks
    run_install
}
start_breaks
name="new app damaged after its start"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: says the watchdog was not started" said "the watchdog was NOT started"
ok "$name: says the new version may have run" said "may already have run"
ok "$name: says not to put the previous app back" said "do not put the previous app back"
ok "$name: does not say to rename the previous app back" not_said "rename \".$APP_NAME.old\" back"
ok "$name: says how to restore the new app" said "put the complete new app in place"
ok "$name: not started again" started_once
ok "$name: the previous app is kept" [ -d "$OLD_APP" ]
ok "$name: the state is untouched" state_untouched
ok "$name: the staging folder says it started" is_text "$STAGE/started" yes
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... then a rerun: refused, and it does not say to rename the previous
# app back ----------------------------------------------------------------------
FAIL=""
: >"$calls"
run_staging
name="rerun after the damaged new app"
before=$failed
ok "$name: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "$name: says the earlier run may have started the new version" said "may already have run"
ok "$name: does not say to rename it back" not_said "Rename it back yourself"
ok "$name: says how to restore the new app" said "put the complete new app in place"
ok "$name: the previous app is kept" [ -d "$OLD_APP" ]
ok "$name: never stopped anything" [ "$(grep -c bootout "$calls")" -eq 0 ]
ok "$name: its start at login stays off" label_is disabled
[ "$failed" -eq "$before" ] || show

# --- ... the previous app put back anyway and started, then a rerun that
# fails before the swap: the previous watchdog is not started on the state
# the new one may have changed ---------------------------------------------------
command rm -rf -- "$APP"
command mv -- "$OLD_APP" "$APP"
LOADED=1
FAIL=backup
rm -f -- "$root/failed-once"
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
name="previous app put back, rerun fails"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the previous watchdog is never started" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 0 ]
ok "$name: says the watchdog was not started" said "the watchdog was NOT started"
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the previous app is untouched" app_is old
ok "$name: the state is untouched" state_untouched
ok "$name: the staging folder still says it started" is_text "$STAGE/started" yes
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... and a further rerun that fails the same way still does not start it
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
ok "previous app put back, second rerun fails: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "previous app put back, second rerun fails: never started" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 0 ]
autostart_off "previous app put back, second rerun fails"

# --- The complete new app put in place as the message says, then a rerun that
# fails before the swap: the new one is started, said to have maybe run -------
start_breaks
command rm -rf -- "$APP"
command cp -R -- "$release/$APP_NAME" "$APP"
FAIL=backup
rm -f -- "$root/failed-once"
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
name="new app put back, rerun fails"
before=$failed
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: says the earlier copies are removed" said "the complete new app it recorded is in place"
ok "$name: the new app is in place" app_is new
ok "$name: started once" started_once
ok "$name: its start at login is on again" label_is enabled
ok "$name: enabled before it was loaded" enabled_before_start
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the closing line" said "runs the new version"
ok "$name: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- A staging folder without its "started" file cannot tell: counted as
# started, so a rerun that fails before the swap does not start the previous
# app --------------------------------------------------------------------------------
reset_fixture
run_staging
command rm -f -- "$STAGE/started"
FAIL=backup
: >"$calls"
run_install
name="staging folder without its started file"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the previous watchdog is never started" [ "$(calls_of "launchctl bootstrap gui/$USER_ID $PLIST")" -eq 0 ]
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the previous app is untouched" app_is old
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- An earlier run's new watchdog may have run (started yes, the previous
# app kept), the complete new app put back as the message says, and a rerun
# that fails, or is stopped, right after it removed the earlier staging
# contents (P2-8): the next run still knows the new version may have run ---
for how in fails interrupt; do
    start_breaks
    command rm -rf -- "$APP"
    command cp -R -- "$release/$APP_NAME" "$APP"
    FAIL=stage-cleared-$how
    rm -f -- "$root/failed-once"
    : >"$calls"
    run_install
    name="restaging $how after the staging contents are removed"
    before=$failed
    ok "$name: not 0 (got $code)" [ "$code" -ne 0 ]
    ok "$name: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
    ok "$name: the staging folder still says it started" is_text "$STAGE/started" yes
    ok "$name: its start at login stays off" label_is disabled
    [ "$failed" -eq "$before" ] || show
    FAIL=backup
    rm -rf -- "$DATA/backup"
    : >"$calls"
    run_install
    name="restaging $how, then a rerun fails"
    before=$failed
    ok "$name: does not say to rename the previous app back" not_said "Rename it back yourself"
    ok "$name: says the new version may have run" said "may already have run"
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the new app is in place" app_is new
    ok "$name: the closing line" said "runs the new version"
    ok "$name: no start without the mark, none of the previous app (starts: $(starts_seen))" starts_safe
    ok "$name: its start at login is on again" label_is enabled
    ok "$name: enabled before it was loaded" enabled_before_start
    ok "$name: the state is untouched" state_untouched
    [ "$failed" -eq "$before" ] || show
done

# --- "started" cannot be written "yes" (P2-7), in the window and again in the
# trap, and the staging folder cannot be removed: the new watchdog is not
# started, everything is kept, and the message says this run did not start it
reset_fixture
FAIL="mark-locked"
run_install
chmod 0700 "$STAGE"
chmod 0644 "$STAGE/started"
name="started cannot be marked"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the step is named" said "the upgrade failed while starting the new watchdog"
ok "$name: never started (starts: $(starts_seen))" [ ! -s "$starts" ]
ok "$name: no start without the mark" starts_safe
ok "$name: says it was not started" said "the watchdog was NOT started"
ok "$name: says the file could not be written" said "could not be written to say that the new version may have run"
ok "$name: says this run did not start it" said "This run did not start the new watchdog"
ok "$name: says to leave the new app in place" said "Leave the new app in place"
ok "$name: does not say it starts the watchdog" not_said "Starting its watchdog"
ok "$name: does not say to rename the previous app back" not_said "rename \".$APP_NAME.old\" back"
ok "$name: says nothing monitors" said "nothing monitors the tunnels now"
ok "$name: the new app is in place" app_is new
ok "$name: the previous app is kept" [ -d "$OLD_APP" ]
ok "$name: the staging folder still says no" is_text "$STAGE/started" no
ok "$name: the state is untouched" state_untouched
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... then the previous app put back by hand (the new one never ran) and
# a rerun that fails before the swap: the previous one may start, as the new
# one was never started ------------------------------------------------------------
command rm -rf -- "$APP"
if [ -d "$OLD_APP" ]; then
    command mv -- "$OLD_APP" "$APP"
else
    make_app "$APP" old
fi
FAIL=backup
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
name="started could not be marked, previous app put back, rerun fails"
before=$failed
ok "$name: no start of the previous app after one of the new (starts: $(starts_seen))" starts_safe
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: the closing line" said "runs the old version"
ok "$name: the state is untouched" state_untouched
ok "$name: its start at login is on again" label_is enabled
ok "$name: enabled before it was loaded" enabled_before_start
[ "$failed" -eq "$before" ] || show

# --- ... and the same failure, then a rerun that works: started only with the
# mark ----------------------------------------------------------------------------
reset_fixture
FAIL="mark-locked"
run_install
chmod 0700 "$STAGE"
chmod 0644 "$STAGE/started"
ok "started could not be marked: its start at login is off before the rerun" label_is disabled
FAIL=""
rm -rf -- "$DATA/backup"
: >"$calls"
run_install
name="started could not be marked, then a rerun"
before=$failed
ok "$name: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "$name: its start at login is on again" label_is enabled
ok "$name: enabled before it was loaded" enabled_before_start
ok "$name: started once (starts: $(starts_seen))" [ "$(wc -l <"$starts")" -eq 1 ]
ok "$name: no start without the mark" starts_safe
ok "$name: the new app is in place" app_is new
ok "$name: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- Disabling the label fails: said plainly, with what to do before the
# next login; nothing is loaded ---------------------------------------------------
for how in fail ignored; do
    reset_fixture
    FAIL="mark-locked"
    FAIL_DISABLE=$how
    run_install
    chmod 0700 "$STAGE"
    chmod 0644 "$STAGE/started"
    name="disabling the label fails ($how)"
    before=$failed
    ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
    ok "$name: never started" [ ! -s "$starts" ]
    ok "$name: it was tried" grep -qxF "launchctl disable gui/$USER_ID/$LABEL" "$calls"
    ok "$name: says the watchdog was not started" said "the watchdog was NOT started"
    ok "$name: says it could not be turned off" said "Its start at login could NOT be turned off"
    ok "$name: says launchd may start it" said "launchd may start the watchdog"
    ok "$name: says to act before the next login" said "before you log out or restart this Mac"
    ok "$name: does not say it was turned off" not_said "start at login was turned off"
    [ "$failed" -eq "$before" ] || show
done

# --- The label left disabled by an earlier run (launchd refuses to load it):
# a run that gets past its checks enables it and loads the watchdog ----------
reset_fixture
echo disabled >"$override"
run_install
name="label left disabled, then a run"
before=$failed
ok "$name: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "$name: started once" started_once
ok "$name: its start at login is on again" label_is enabled
ok "$name: enabled before it was loaded" enabled_before_start
ok "$name: no bootstrap refused" not_said "Bootstrap failed"
[ "$failed" -eq "$before" ] || show

# --- Older macOS lists a disabled label as "=> true": read as disabled ---------
reset_fixture
FAIL="mark-locked"
OLD_FORMAT=1
run_install
chmod 0700 "$STAGE"
chmod 0644 "$STAGE/started"
name="print-disabled in the older format"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- The window's mark fails once, the trap's works: started, with the mark,
# and said not to have run ------------------------------------------------------------
reset_fixture
FAIL_MARK_WRITES=1
run_install
name="first mark fails, the trap marks it"
before=$failed
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: started once (starts: $(starts_seen))" [ "$(wc -l <"$starts")" -eq 1 ]
ok "$name: no start without the mark" starts_safe
ok "$name: says the new app has not run" said "the new app is in place, complete; it has not run yet"
ok "$name: the closing line" said "runs the new version"
ok "$name: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- The recovered watchdog does not run ------------------------------------------
reset_fixture
FAIL=backup
START_RUNS=0
run_install
ok "recovered watchdog does not run: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "recovered watchdog does not run: says nothing monitors" said "nothing monitors the tunnels now"

# --- A VPN configuration changed meanwhile: 2 over 4 -----------------------------
reset_fixture
FAIL=backup
VPN_CHANGES=1
run_install
ok "VPN changed: exit 2 (got $code)" [ "$code" -eq 2 ]
ok "VPN changed: the warning" said "a VPN configuration changed while the script ran"
ok "VPN changed: the failure is still said" said "the upgrade did not complete"

# --- The backup keeps the newest five ---------------------------------------------
reset_fixture
for stamp in 20260101-000000Z 20260102-000000Z 20260103-000000Z 20260104-000000Z \
    20260105-000000Z 20260106-000000Z; do
    command mkdir -p "$DATA/backup/state-$stamp"
done
(
    set -euo pipefail
    backup_state_real
) >"$out" 2>&1
ok "backup: done" [ $? -eq 0 ]
ok "backup: five kept" [ "$(find "$DATA/backup" -mindepth 1 -maxdepth 1 -name 'state-*' | wc -l)" -eq 5 ]
ok "backup: the oldest ones are gone" [ ! -e "$DATA/backup/state-20260102-000000Z" ]
ok "backup: the newest old one is kept" [ -d "$DATA/backup/state-20260106-000000Z" ]
ok "backup: the new one holds the state" [ -f "$(find "$DATA/backup" -path '*/state/state.json' | head -n 1)" ]
newest=$(find "$DATA/backup" -path '*/state/state.json' | head -n 1)
ok "backup: the new one is private" [ "$(mode_of "$(dirname "$(dirname "$newest")")")" = 700 ]

# --- A link where the staging folder goes: refused before anything stops ----
reset_fixture
command mkdir -p "$root/elsewhere"
ln -s "$root/elsewhere" "$STAGE"
run_install
ok "staging folder is a link: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "staging folder is a link: never stopped" [ "$(calls_of "launchctl bootout gui/$USER_ID/$LABEL")" -eq 0 ]
ok "staging folder is a link: the target is untouched" [ -d "$root/elsewhere" ]

# --- A failed upgrade, then an uninstall that keeps the data, then a new
# install (Codex review of the CLI, second round, N-P2-2) -------------------
uninstall_after_failed_upgrade() { # with or without uninstall.sh's leftover removal
    reset_fixture
    FAIL="mark-locked"
    run_install
    chmod 0700 "$STAGE"
    chmod 0644 "$STAGE/started"
    FAIL=""
    # What uninstall.sh removes, keeping the data.
    if [ "$1" = with ]; then
        # In a bash of its own: uninstall.sh's readonly names would clash
        # with install.sh's, sourced here.
        # shellcheck disable=SC2016 # expanded by that bash
        bash -c 'source "$1" && as_needed() { "$@"; } && remove_install_leftovers "$2" "$3"'             _ "$tests/../uninstall.sh" "$APP" "$STAGE"
    fi
    rm -rf -- "$APP"
    rm -f -- "$PLIST" "$WINDOW_PLIST"
    # A real install comes later; here it may come within the same second,
    # whose backup folder (state-<UTC time to the second>) the failed
    # upgrade already made.
    rm -rf -- "$DATA/backup"
    LOADED=0
    WINDOW_LOADED=0
    : >"$starts"
    run_install
}
uninstall_after_failed_upgrade with
before=$failed
ok "uninstall after a failed upgrade: the hidden previous app is gone" [ ! -e "$OLD_APP" ]
ok "uninstall after a failed upgrade: then an install works (got $code)" [ "$code" -eq 0 ]
ok "uninstall after a failed upgrade: the new app is in place" app_is new
ok "uninstall after a failed upgrade: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show
uninstall_after_failed_upgrade without
ok "without removing the leftovers, that install is refused (got $code)" [ "$code" -ne 0 ]
ok "without removing the leftovers: it names the hidden previous app" said "is the previous app it renamed away"

echo "install-recovery.test.sh (macOS script, with stand-ins; bash $BASH_VERSION on $(uname -s)): $passed passed, $failed failed"
[ "$failed" -eq 0 ]
