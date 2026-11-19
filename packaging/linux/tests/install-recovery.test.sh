#!/usr/bin/env bash
# Tests install.sh's upgrade recovery (FIX-05): every step between stopping
# wgal-watchdog.service and starting it again is made to fail in turn, and
# the test checks that the trap puts a complete set in place, starts it (or
# refuses to, with the right message), never puts back an access list that
# was replaced, never touches the stored state, and ends with the right
# exit code.
#
# Run it as a normal user, never with sudo:
#
#   packaging/linux/tests/install-recovery.test.sh
#
# It sources install.sh (which then runs nothing), points its paths at a
# new scratch folder, and replaces systemctl, pgrep, sleep, journalctl,
# install (without its -o/-g), the wg-quick listing and the socket wait
# with stand-ins defined here; mv is a stand-in that can fail. The
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

root=$(mktemp -d "${TMPDIR:-/tmp}/wgal-fix05.XXXXXX")
trap 'rm -rf -- "$root"' EXIT
release=$root/release
out=$root/out
calls=$root/calls
starts=$root/starts
PREFIX=$root/opt/wgal
UNIT_FILE=$root/etc/systemd/system/wgal-watchdog.service
ACCESS_DIR=$root/etc/wgal
ACCESS_FILE=$ACCESS_DIR/access.conf
STATE_DIR=$root/var/lib/wgal
SOCKET=$root/run/wgal/watchdog.sock
DESKTOP_FILE=$root/usr/share/applications/wgal.desktop
AUTOSTART_FILE=$root/etc/xdg/autostart/wgal.desktop
CLI_LINK=$root/usr/local/bin/wgal

# --- Stand-ins -----------------------------------------------------------------
FAIL=""          # stop, still-running, interrupt, backup, access, unit, daemon-reload, enable,
                 # restart-interrupt, wait-interrupt (Ctrl-C while the new set starts),
                 # restart-breaks (a new program is damaged as the new set starts,
                 # then Ctrl-C), mark-locked (the program folder is made read-only
                 # once the new set is in place: the record can be neither
                 # rewritten nor removed)
FAIL_MV_AT=0     # fail the n-th move of a staged program into place (1-4)
FAIL_MV_HOW=""   # before (nothing written) or partial (half a file left)
FAIL_RESTORE=0
FAIL_STAGE=0     # fail copying a new file to its .new copy
FAIL_MARK_WRITES=0  # fail this many renames of a record saying "started yes"
START_RUNS=1
WG_CHANGES=0
FAIL_DISABLE=""  # fail (systemctl disable fails) or ignored (it changes nothing)
moves=0
# Whether WGAL's unit starts at boot, as the systemctl stand-in keeps it.
enabled=$root/enabled

systemctl() {
    echo "systemctl $*" >>"$calls"
    case "$1" in
    start | restart)
        # Which watchdog starts, and what the record says then.
        echo "$(cat -- "$PREFIX/wgal-watchdog" 2>/dev/null)|$(grep -s '^started ' "$(record_path)" || echo '<none>')" >>"$starts"
        ;;
    esac
    case "$1" in
    stop)
        [ "$FAIL" != stop ] || return 1
        if [ "$FAIL" = interrupt ]; then
            kill -INT "$BASHPID"
        fi
        ;;
    daemon-reload) [ "$FAIL" != daemon-reload ] || return 1 ;;
    enable)
        [ "$FAIL" != enable ] || return 1
        echo enabled >"$enabled"
        ;;
    disable)
        [ "$FAIL_DISABLE" != fail ] || return 1
        [ "$FAIL_DISABLE" = ignored ] || echo disabled >"$enabled"
        ;;
    is-enabled)
        cat -- "$enabled"
        [ "$(cat -- "$enabled")" = enabled ] || return 1
        ;;
    restart)
        if [ "$FAIL" = restart-interrupt ]; then
            kill -INT "$BASHPID"
        fi
        if [ "$FAIL" = restart-breaks ]; then
            echo damaged >"$PREFIX/wgal-mcp"
            kill -INT "$BASHPID"
        fi
        ;;
    esac
    return 0
}
pgrep() { [ "$FAIL" = still-running ]; }
sleep() { :; }
journalctl() { :; }
wait_running() {
    echo "wait_running" >>"$calls"
    if [ "$FAIL" = wait-interrupt ] && [ "$(calls_of wait_running)" -eq 1 ]; then
        kill -INT "$BASHPID"
    fi
    [ "$START_RUNS" -eq 1 ]
}
wireguard_states() {
    if [ "$WG_CHANGES" -eq 1 ] && [ -n "$WG_BEFORE" ]; then
        echo "wg-quick@wg0.service inactive dead enabled"
    else
        echo "wg-quick@wg0.service active exited enabled"
    fi
}
install() {
    local -a args=()
    while [ $# -gt 0 ]; do
        case "$1" in
        -o | -g) shift 2 ;;
        *)
            args+=("$1")
            shift
            ;;
        esac
    done
    local last=${args[${#args[@]}-1]}
    if [ "$FAIL_STAGE" -eq 1 ] && [[ "$last" == *.new ]]; then
        return 1
    fi
    if [ "$FAIL" = unit ] && [ "$last" = "$UNIT_FILE" ] && [[ "${args[${#args[@]}-2]}" == *.new ]]; then
        echo half >"$UNIT_FILE"
        return 1
    fi
    command install "${args[@]}"
}
mv() {
    local source=${*: -2:1} dest=${*: -1}
    if [ "$FAIL" = access ] && [ "$dest" = "$ACCESS_FILE" ]; then
        return 1
    fi
    if [ "$FAIL_RESTORE" -eq 1 ] && [[ "$source" == *.old ]]; then
        return 1
    fi
    if [ "$FAIL" = mark-interrupt ] && [ "$source" = "$(record_path).tmp" ] && grep -qx 'started yes' "$source"; then
        kill -INT "$BASHPID"
    fi
    if [ "$FAIL_MARK_WRITES" -gt 0 ] && [ "$source" = "$(record_path).tmp" ] && grep -qx 'started yes' "$source"; then
        FAIL_MARK_WRITES=$((FAIL_MARK_WRITES - 1))
        return 1
    fi
    if [[ "$source" == *.new ]]; then
        moves=$((moves + 1))
        if [ "$moves" -eq "$FAIL_MV_AT" ]; then
            [ "$FAIL_MV_HOW" != partial ] || echo half >"$dest"
            return 1
        fi
    fi
    command mv "$@"
}
backup_state_real=$(declare -f backup_state)
eval "${backup_state_real/backup_state/backup_state_real}"
backup_state() {
    [ "$FAIL" != backup ] || return 1
    backup_state_real
}
install_new_set_real=$(declare -f install_new_set)
eval "${install_new_set_real/install_new_set/install_new_set_real}"
install_new_set() {
    install_new_set_real
    if [ "$FAIL" = mark-locked ]; then
        chmod 0555 "$PREFIX"
    fi
}

# --- Fixture ---------------------------------------------------------------------
reset_fixture() {
    rm -rf -- "${root:?}/release" "${root:?}/opt" "${root:?}/etc" "${root:?}/var" "${root:?}/run" "${root:?}/usr"
    command mkdir -p "$release" "$PREFIX" "$(dirname "$UNIT_FILE")" "$ACCESS_DIR" \
        "$STATE_DIR/state" "$STATE_DIR/notify"
    local name absent=""
    # A previous version without one of the programs: wgal-mcp (before
    # 2.0.0) or wgal (before 3.0.0).
    case "${1:-}" in
    no-old-mcp) absent=wgal-mcp ;;
    no-old-cli) absent=wgal ;;
    esac
    for name in "${PROGRAMS[@]}"; do
        echo "new $name" >"$release/$name"
        if [ "$name" != "$absent" ]; then
            echo "old $name" >"$PREFIX/$name"
            chmod 0755 "$PREFIX/$name"
        fi
    done
    echo "new unit" >"$release/wgal-watchdog.service"
    echo "old unit" >"$UNIT_FILE"
    echo "old list" >"$ACCESS_FILE"
    echo "stored state" >"$STATE_DIR/state/state.json"
    : >"$calls"
    : >"$starts"
    FAIL=""
    FAIL_MV_AT=0
    FAIL_MV_HOW=""
    FAIL_RESTORE=0
    FAIL_STAGE=0
    FAIL_MARK_WRITES=0
    START_RUNS=1
    WG_CHANGES=0
    FAIL_DISABLE=""
    echo enabled >"$enabled"
    moves=0
    STAGED_ACCESS=""
    WG_BEFORE=""
    UPGRADE_FAILED=0
    RECOVERED_SET=""
    NEW_MAY_HAVE_RUN=0
    WINDOW_RUNNING=0
}

# What main does from the staging to the end, on an upgrade; the exit code
# goes to $code and everything said to $out.
run_upgrade() {
    local with_access=${1:-1}
    (
        set -euo pipefail
        WG_BEFORE=$(wireguard_states)
        stage_set "$release"
        if [ "$with_access" -eq 1 ]; then
            stage_access 1000 wg0
        fi
        upgrade_window
        echo "WINDOW DONE"
        running=$WINDOW_RUNNING
        remove_staged
        finish "$running"
    ) >"$out" 2>&1
    code=$?
}

# A rerun's staging alone, as main does it before the stop; the exit code
# goes to $code and everything said to $out.
run_staging() {
    (
        set -euo pipefail
        stage_set "$release"
    ) >"$out" 2>&1
    code=$?
}

# --- Checks ----------------------------------------------------------------------
passed=0
failed=0
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
programs_are() {
    local name
    for name in "${PROGRAMS[@]}"; do
        is_text "$PREFIX/$name" "$1 $name" || return 1
    done
}
no_staged() {
    local name
    for name in "${SET_FILES[@]}"; do
        [ ! -e "$(staged_path "$name" new)" ] && [ ! -e "$(staged_path "$name" old)" ] || return 1
    done
    [ -z "$(find "$ACCESS_DIR" -name '.access.*')" ] && [ ! -e "$(record_path)" ]
}
state_untouched() { is_text "$STATE_DIR/state/state.json" "stored state"; }
# Every start so far was safe: the new set only with the record saying
# "started yes", and the previous set never once the new one had started.
starts_safe() {
    local line new_ran=0
    while IFS= read -r line; do
        case "$line" in
        "new wgal-watchdog|started yes") new_ran=1 ;;
        "new wgal-watchdog|"*) return 1 ;;
        *) [ "$new_ran" -eq 0 ] || return 1 ;;
        esac
    done <"$starts"
}
starts_seen() { tr '\n' ',' <"$starts"; }
is_enabled() { is_text "$enabled" "$1"; }
# Every systemctl disable was for WGAL's own unit, never a wg-quick one.
disables_only_wgal() { ! grep '^systemctl disable' "$calls" | grep -qvxF "systemctl disable $UNIT"; }
# Nothing was started after the last systemctl disable, and there was one.
none_started_after_disable() {
    grep -qxF "systemctl disable $UNIT" "$calls" &&
        ! awk -v d="systemctl disable $UNIT" '$0 == d {seen = 1; bad = 0; next}
            seen && /^systemctl (start|restart) / {bad = 1} END {exit !bad}' "$calls"
}
# The unit was enabled before it was (re)started.
enabled_before_start() {
    awk -v e="systemctl enable $UNIT" '$0 == e {on = 1; next}
        /^systemctl (start|restart) / {started = 1; if (!on) bad = 1} END {exit bad || !started}' "$calls"
}
# A run that left the service stopped turned its start at boot off and said so.
autostart_off() {
    local name=$1
    ok "$name: the unit is disabled" is_enabled disabled
    ok "$name: says its start at boot was turned off" said "Its start at boot was turned off"
    ok "$name: says the next run turns it back on" said "the next run of this script that gets past its checks turns it back on"
    ok "$name: nothing started after the disable" none_started_after_disable
    ok "$name: only WGAL's unit was disabled" disables_only_wgal
}
# A run that started the service left its start at boot on.
autostart_kept() {
    local name=$1
    ok "$name: the unit stays enabled" is_enabled enabled
    ok "$name: never disabled" [ "$(grep -c '^systemctl disable' "$calls")" -eq 0 ]
    ok "$name: start at boot not mentioned" not_said "start at boot"
}
show() { sed 's/^/    | /' "$out" >&2; }

# --- Staging, before the stop ----------------------------------------------------
reset_fixture
(
    set -euo pipefail
    stage_set "$release"
    for name in "${SET_FILES[@]}"; do
        [ -n "${NEW_SUM[$name]}" ] && [ -n "${OLD_SUM[$name]}" ]
    done
    [ "$(set_in_place)" = old ]
) >"$out" 2>&1
ok "staging: every file has a new and an old sum, the old set is in place" [ $? -eq 0 ]
ok "staging: .wgal-watchdog.new holds the new one" is_text "$PREFIX/.wgal-watchdog.new" "new wgal-watchdog"
ok "staging: .wgal-watchdog.old holds the old one" is_text "$PREFIX/.wgal-watchdog.old" "old wgal-watchdog"
ok "staging: the unit is staged" is_text "$PREFIX/.wgal-watchdog.service.new" "new unit"
ok "staging: nothing in place changed" programs_are old
ok "staging: the unit in place is unchanged" is_text "$UNIT_FILE" "old unit"

# --- The whole window works ------------------------------------------------------
reset_fixture
run_upgrade
ok "success: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "success: the window finished" said "WINDOW DONE"
ok "success: the new programs are in place" programs_are new
ok "success: the new unit is in place" is_text "$UNIT_FILE" "new unit"
ok "success: the new access list is in place" grep -qx "tunnel wg0" "$ACCESS_FILE"
ok "success: nothing staged is left" no_staged
ok "success: the state was backed up" [ -f "$(find "$STATE_DIR/backup" -path '*/state/state.json' | head -n 1)" ]
ok "success: never started by the recovery" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
ok "success: started once, inside the window" [ "$(calls_of "systemctl restart $UNIT")" -eq 1 ]
ok "success: no failure said" not_said "failed while"
autostart_kept success
[ "$failed" -eq 0 ] || show

# --- A step fails before any file is replaced ------------------------------------
for case in "stop|stopping the service" "still-running|stopping the service" \
    "interrupt|stopping the service" "backup|backing up the stored state" \
    "access|putting the new access list in place"; do
    reset_fixture
    FAIL=${case%%|*}
    step=${case#*|}
    run_upgrade
    name="fail at $FAIL"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the step is named" said "the upgrade failed while $step"
    ok "$name: the old programs are in place" programs_are old
    ok "$name: the old unit is in place" is_text "$UNIT_FILE" "old unit"
    ok "$name: the previous access list is unchanged" is_text "$ACCESS_FILE" "old list"
    ok "$name: says the previous list is in effect" said "the previous access list is still in effect"
    ok "$name: says the previous version runs" said "the previous version is in place, complete"
    ok "$name: started once" [ "$(calls_of "systemctl start $UNIT")" -eq 1 ]
    ok "$name: the closing line" said "runs the old version"
    ok "$name: the window did not go on" not_said "WINDOW DONE"
    ok "$name: the state is untouched" state_untouched
    ok "$name: nothing staged is left" no_staged
    autostart_kept "$name"
    [ "$failed" -eq "$before" ] || show
done

# --- Moving the programs fails at each one -----------------------------------
for at in 1 2 3 4; do
    for how in before partial; do
        reset_fixture
        FAIL_MV_AT=$at
        FAIL_MV_HOW=$how
        run_upgrade
        name="move $at fails ($how)"
        before=$failed
        ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
        ok "$name: the step is named" said "the upgrade failed while replacing the programs and the unit"
        ok "$name: the old programs are back" programs_are old
        ok "$name: the old unit is in place" is_text "$UNIT_FILE" "old unit"
        ok "$name: the new access list is kept" grep -qx "tunnel wg0" "$ACCESS_FILE"
        ok "$name: says the new list is kept" said "it is kept (an access list is never put back)"
        if [ "$at" -gt 1 ] || [ "$how" = partial ]; then
            ok "$name: says the previous ones are put back" said "putting the previous ones back"
        fi
        ok "$name: started once" [ "$(calls_of "systemctl start $UNIT")" -eq 1 ]
        ok "$name: the state is untouched" state_untouched
        ok "$name: nothing staged is left" no_staged
        [ "$failed" -eq "$before" ] || show
    done
done

# --- The unit is written halfway: programs new, unit neither -> old set ------
reset_fixture
FAIL=unit
run_upgrade
before=$failed
ok "unit fails: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "unit fails: the old programs are back" programs_are old
ok "unit fails: the old unit is back" is_text "$UNIT_FILE" "old unit"
ok "unit fails: systemd reloaded before the start" grep -qx "systemctl daemon-reload" "$calls"
[ "$failed" -eq "$before" ] || show

# --- systemd fails after the new set is in place: the new set, not yet run ---
for step in daemon-reload enable; do
    reset_fixture
    FAIL=$step
    run_upgrade
    name="$step fails"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the new programs are in place" programs_are new
    ok "$name: the new unit is in place" is_text "$UNIT_FILE" "new unit"
    ok "$name: says the new version has not run" said "the new version is in place, complete; it has not run yet"
    ok "$name: started once" [ "$(calls_of "systemctl start $UNIT")" -eq 1 ]
    ok "$name: the closing line" said "runs the new version"
    [ "$failed" -eq "$before" ] || show
done

# --- Ctrl-C while the new set starts (P2-1): it may have run, so it is
# started again and nothing is put back -----------------------------------------
for FAIL_AT in restart-interrupt wait-interrupt; do
    reset_fixture
    FAIL=$FAIL_AT
    run_upgrade
    name="$FAIL_AT"
    before=$failed
    ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
    ok "$name: the step is named" said "the upgrade failed while starting the new version"
    ok "$name: says the new version may have run" said "it may already have run"
    ok "$name: says nothing is put back" said "the previous version is not put back"
    ok "$name: nothing is put back" not_said "putting the previous ones back"
    ok "$name: the new programs are in place" programs_are new
    ok "$name: the new unit is in place" is_text "$UNIT_FILE" "new unit"
    ok "$name: the new access list is kept" grep -qx "tunnel wg0" "$ACCESS_FILE"
    ok "$name: started again by the recovery" [ "$(calls_of "systemctl start $UNIT")" -eq 1 ]
    autostart_kept "$name"
    ok "$name: the closing line" said "runs the new version"
    ok "$name: the window did not go on" not_said "WINDOW DONE"
    ok "$name: the state is untouched" state_untouched
    ok "$name: nothing staged is left" no_staged
    [ "$failed" -eq "$before" ] || show
done

# --- The new set starts but does not run: reported, not rolled back -----------
reset_fixture
START_RUNS=0
run_upgrade
before=$failed
ok "new set does not run: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "new set does not run: the new programs stay" programs_are new
ok "new set does not run: no recovery" not_said "failed while"
ok "new set does not run: not started again" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
[ "$failed" -eq "$before" ] || show

# --- Neither set can be made: not started -------------------------------------
reset_fixture
FAIL_MV_AT=2
FAIL_MV_HOW=partial
FAIL_RESTORE=1
run_upgrade
before=$failed
ok "no complete set: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "no complete set: says it was not started" said "was NOT started"
ok "no complete set: never started" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
ok "no complete set: says nothing monitors" said "nothing monitors the tunnels now"
ok "no complete set: the .old copies are kept" [ -f "$PREFIX/.wgal-watchdog.old" ]
ok "no complete set: the .new copies are kept" [ -f "$PREFIX/.wgal-mcp.new" ]
ok "no complete set: the record is kept" [ -f "$(record_path)" ]
ok "no complete set: says a rerun refuses until a set is put back" said "until then it refuses to run"
autostart_off "no complete set"
[ "$failed" -eq "$before" ] || show

# --- ... then a rerun, whose staging would fail again (P2-2): refused before
# a copy is removed ----------------------------------------------------------------
in_place_before=$(for name in "${PROGRAMS[@]}"; do cat -- "$PREFIX/$name"; done; cat -- "$UNIT_FILE")
FAIL_MV_AT=0
FAIL_RESTORE=0
FAIL_STAGE=1
: >"$calls"
run_staging
before=$failed
ok "rerun after a failed recovery: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "rerun after a failed recovery: says the earlier run did not finish" said "an earlier run of this script did not finish"
ok "rerun after a failed recovery: says nothing was changed" said "Nothing of WGAL was changed"
for name in "${SET_FILES[@]}"; do
    case "$name" in
    wgal-watchdog.service) text="old unit" ;;
    *) text="old $name" ;;
    esac
    ok "rerun after a failed recovery: .$name.old still holds the previous one" is_text "$(staged_path "$name" old)" "$text"
done
ok "rerun after a failed recovery: .wgal-mcp.new is kept" is_text "$PREFIX/.wgal-mcp.new" "new wgal-mcp"
ok "rerun after a failed recovery: the record is kept" [ -f "$(record_path)" ]
ok "rerun after a failed recovery: nothing in place changed" \
    [ "$(for name in "${PROGRAMS[@]}"; do cat -- "$PREFIX/$name"; done; cat -- "$UNIT_FILE")" = "$in_place_before" ]
ok "rerun after a failed recovery: systemd not called" [ ! -s "$calls" ]
ok "rerun after a failed recovery: its start at boot stays off" is_enabled disabled
[ "$failed" -eq "$before" ] || show

# --- ... and once the previous version is put back by hand, a rerun goes on --
for name in wgal-watchdog wgal-desktop; do
    command mv -f -- "$(staged_path "$name" old)" "$PREFIX/$name"
done
FAIL_STAGE=0
run_staging
before=$failed
ok "rerun with the previous version put back: staged (got $code)" [ "$code" -eq 0 ]
ok "rerun with the previous version put back: says so" said "the complete previous set it recorded is in place"
ok "rerun with the previous version put back: new copies made again" is_text "$PREFIX/.wgal-watchdog.new" "new wgal-watchdog"
ok "rerun with the previous version put back: old copies made again" is_text "$PREFIX/.wgal-watchdog.old" "old wgal-watchdog"
[ "$failed" -eq "$before" ] || show

# --- A record this script did not write: refused -------------------------------
echo "wgal-watchdog something else" >"$(record_path)"
run_staging
ok "a foreign record: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "a foreign record: says so" said "is not a record this script wrote"
ok "a foreign record: the copies are kept" is_text "$PREFIX/.wgal-watchdog.old" "old wgal-watchdog"

# --- Staging fails: copies without a record, which a rerun replaces ------------
reset_fixture
FAIL_STAGE=1
run_staging
ok "staging fails: not 0 (got $code)" [ "$code" -ne 0 ]
ok "staging fails: no record" [ ! -e "$(record_path)" ]
FAIL_STAGE=0
run_staging
ok "staging failed before: a rerun stages again (got $code)" [ "$code" -eq 0 ]
ok "staging failed before: the rerun writes the record" [ -f "$(record_path)" ]

# --- The new set started, then a file of it is found damaged (P2-6): the
# previous version is not put back, and the message says to restore the new
# one ---------------------------------------------------------------------------
record_says() { grep -qsx "started $1" "$(record_path)"; }
put_set_back() {
    local name
    for name in "${SET_FILES[@]}"; do
        if [ "$1" = old ]; then
            command cp -p -- "$(staged_path "$name" old)" "$(target_of "$name")"
        else
            command cp -p -- "$release/$name" "$(target_of "$name")"
        fi
    done
}
restart_breaks() {
    reset_fixture
    FAIL=restart-breaks
    run_upgrade
}
restart_breaks
name="new set damaged after its start"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: says it was not started" said "was NOT started"
ok "$name: says the new version may have run" said "may already have run"
ok "$name: says not to put the previous version back" said "do not put the previous version back"
ok "$name: does not say to put the previous version back" not_said "put the previous version back yourself"
ok "$name: says how to restore the new version" said "put the complete new version in place"
ok "$name: not started again" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
ok "$name: the .old copies are kept" [ -f "$PREFIX/.wgal-watchdog.old" ]
ok "$name: the record is kept" [ -f "$(record_path)" ]
ok "$name: the record says it started" record_says yes
ok "$name: the state is untouched" state_untouched
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... then a rerun: refused, and it does not say to put the previous
# version back ---------------------------------------------------------------------
FAIL=""
: >"$calls"
run_staging
name="rerun after the damaged new set"
before=$failed
ok "$name: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "$name: says the earlier run may have started the new version" said "may already have run"
ok "$name: does not say to put the previous version back" not_said "Put the previous version back yourself"
ok "$name: says how to restore the new version" said "put the complete new version in place"
ok "$name: the .old copies are kept" is_text "$PREFIX/.wgal-watchdog.old" "old wgal-watchdog"
ok "$name: systemd not called" [ ! -s "$calls" ]
ok "$name: its start at boot stays off" is_enabled disabled
[ "$failed" -eq "$before" ] || show

# --- ... the previous version put back anyway, then a rerun that fails
# before the new set is in place: the previous version is not started on the
# state the new one may have changed -------------------------------------------------
put_set_back old
FAIL=backup
: >"$calls"
run_upgrade
name="previous version put back, rerun fails"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the previous version is never started" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
ok "$name: says it was not started" said "was NOT started"
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the previous programs are untouched" programs_are old
ok "$name: the state is untouched" state_untouched
ok "$name: the record still says it started" record_says yes
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... and a further rerun that fails the same way still does not start it
: >"$calls"
run_upgrade
ok "previous version put back, second rerun fails: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "previous version put back, second rerun fails: never started" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
autostart_off "previous version put back, second rerun fails"

# --- The complete new set put in place as the message says, then a rerun that
# fails before the new set is in place: the new one is started --------------------
restart_breaks
put_set_back new
FAIL=backup
: >"$calls"
run_upgrade
name="new version put back, rerun fails"
before=$failed
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: the new programs are in place" programs_are new
ok "$name: the new unit is in place" is_text "$UNIT_FILE" "new unit"
ok "$name: started once" [ "$(calls_of "systemctl start $UNIT")" -eq 1 ]
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the closing line" said "runs the new version"
ok "$name: nothing staged is left" no_staged
ok "$name: its start at boot is on again" is_enabled enabled
ok "$name: enabled before it was started" enabled_before_start
[ "$failed" -eq "$before" ] || show

# --- A record without its "started" line cannot tell: counted as started,
# so a rerun that fails before the new set is in place does not start the
# previous one -----------------------------------------------------------------------
reset_fixture
run_staging
command sed -i '/^started /d' "$(record_path)"
FAIL=backup
: >"$calls"
run_upgrade
name="record without its started line"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the previous version is never started" [ "$(calls_of "systemctl start $UNIT")" -eq 0 ]
ok "$name: says the new version may have run" said "may already have run"
ok "$name: the previous programs are untouched" programs_are old
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- The record cannot be marked "started yes" (P2-7), in the window and
# again in the trap, and the copies cannot be removed: the new set is not
# started, everything is kept, and the message says this run did not start it
reset_fixture
FAIL=mark-locked
run_upgrade
chmod 0755 "$PREFIX"
name="record cannot be marked"
before=$failed
ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "$name: the step is named" said "the upgrade failed while starting the new version"
ok "$name: never started (starts: $(starts_seen))" [ ! -s "$starts" ]
ok "$name: no start without the mark" starts_safe
ok "$name: says it was not started" said "was NOT started"
ok "$name: says the record could not be marked" said "could not be marked to say that the new version may have run"
ok "$name: says this run did not start it" said "This run did not start the new version"
ok "$name: says to leave the new version in place" said "Leave the new version in place"
ok "$name: does not say it starts the service" not_said "Starting $UNIT"
ok "$name: does not say to put the previous version back" not_said "put the previous version back yourself"
ok "$name: says nothing monitors" said "nothing monitors the tunnels now"
ok "$name: the new programs are in place" programs_are new
ok "$name: the .old copies are kept" is_text "$PREFIX/.wgal-watchdog.old" "old wgal-watchdog"
ok "$name: the record is kept" [ -f "$(record_path)" ]
ok "$name: the record still says no" record_says no
ok "$name: the state is untouched" state_untouched
autostart_off "$name"
[ "$failed" -eq "$before" ] || show

# --- ... then the previous version put back by hand (the new one never ran)
# and a rerun that fails before the new set is in place: the previous one
# may start, as the new one was never started --------------------------------
put_set_back old
FAIL=backup
: >"$calls"
run_upgrade
name="record could not be marked, previous version put back, rerun fails"
before=$failed
ok "$name: no start of the previous version after one of the new (starts: $(starts_seen))" starts_safe
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: the closing line" said "runs the old version"
ok "$name: the state is untouched" state_untouched
ok "$name: its start at boot is on again" is_enabled enabled
ok "$name: enabled before it was started" enabled_before_start
[ "$failed" -eq "$before" ] || show

# --- ... and the same failure, then a rerun that works: started only with the
# mark ----------------------------------------------------------------------------
reset_fixture
FAIL=mark-locked
run_upgrade
chmod 0755 "$PREFIX"
ok "record could not be marked: its start at boot is off before the rerun" is_enabled disabled
FAIL=""
: >"$calls"
run_upgrade
name="record could not be marked, then a rerun"
before=$failed
ok "$name: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "$name: its start at boot is on again" is_enabled enabled
ok "$name: enabled before it was started" enabled_before_start
ok "$name: started once (starts: $(starts_seen))" [ "$(wc -l <"$starts")" -eq 1 ]
ok "$name: no start without the mark" starts_safe
ok "$name: the new programs are in place" programs_are new
ok "$name: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- Turning the start at boot off fails: said plainly, with what to do
# before a restart; nothing is started -----------------------------------------
for how in fail ignored; do
    reset_fixture
    FAIL=mark-locked
    FAIL_DISABLE=$how
    run_upgrade
    chmod 0755 "$PREFIX"
    name="disabling the unit fails ($how)"
    before=$failed
    ok "$name: exit 1 (got $code)" [ "$code" -eq 1 ]
    ok "$name: never started" [ ! -s "$starts" ]
    ok "$name: it was tried" grep -qxF "systemctl disable $UNIT" "$calls"
    ok "$name: says it was not started" said "was NOT started"
    ok "$name: says it could not be turned off" said "Its start at boot could NOT be turned off"
    ok "$name: says what is-enabled says" said "is-enabled $UNIT says \"enabled\""
    ok "$name: says systemd may start it" said "systemd may start $UNIT"
    ok "$name: says to act before a restart" said "before you restart this computer"
    ok "$name: does not say it was turned off" not_said "start at boot was turned off"
    [ "$failed" -eq "$before" ] || show
done

# --- The window's mark fails once, the trap's works: started, with the mark,
# and said not to have run ------------------------------------------------------------
reset_fixture
FAIL_MARK_WRITES=1
run_upgrade
name="first mark fails, the trap marks it"
before=$failed
ok "$name: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "$name: started once (starts: $(starts_seen))" [ "$(wc -l <"$starts")" -eq 1 ]
ok "$name: no start without the mark" starts_safe
ok "$name: says the new version has not run" said "the new version is in place, complete; it has not run yet"
ok "$name: the closing line" said "runs the new version"
ok "$name: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- The recovered set does not run ---------------------------------------------
reset_fixture
FAIL=backup
START_RUNS=0
run_upgrade
ok "recovered set does not run: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "recovered set does not run: says nothing monitors" said "nothing monitors the tunnels now"

# --- A wg-quick unit changed meanwhile: 2 wins over 4 ---------------------------
reset_fixture
FAIL=backup
WG_CHANGES=1
run_upgrade
ok "wg-quick changed: exit 2 (got $code)" [ "$code" -eq 2 ]
ok "wg-quick changed: the warning" said "a wg-quick unit changed while the script ran"
ok "wg-quick changed: the failure is still said" said "the upgrade did not complete"

# --- No access list given: none mentioned or touched ---------------------------
reset_fixture
FAIL=backup
run_upgrade 0
ok "no access list given: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "no access list given: unchanged" is_text "$ACCESS_FILE" "old list"
ok "no access list given: not mentioned" not_said "access list"

# --- The previous version had no wgal-mcp ---------------------------------------
reset_fixture no-old-mcp
FAIL_MV_AT=3
FAIL_MV_HOW=partial
run_upgrade
ok "no old wgal-mcp: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "no old wgal-mcp: the old watchdog is back" is_text "$PREFIX/wgal-watchdog" "old wgal-watchdog"
ok "no old wgal-mcp: the half-moved one is removed" [ ! -e "$PREFIX/wgal-mcp" ]

# --- ... and the others are the same in both versions: the half-moved one
# still makes it not the old set ----------------------------------------------------
reset_fixture no-old-mcp
echo "new wgal-watchdog" >"$PREFIX/wgal-watchdog"
echo "new wgal-desktop" >"$PREFIX/wgal-desktop"
echo "new unit" >"$UNIT_FILE"
FAIL_MV_AT=1
FAIL_MV_HOW=partial
run_upgrade
before=$failed
ok "no old wgal-mcp, same others: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "no old wgal-mcp, same others: says the previous ones are put back" said "putting the previous ones back"
ok "no old wgal-mcp, same others: the half-moved one is removed" [ ! -e "$PREFIX/wgal-mcp" ]
ok "no old wgal-mcp, same others: the closing line" said "runs the old version"
[ "$failed" -eq "$before" ] || show

# --- First install: no trap, the set and the list are put in place ----------
reset_fixture
rm -f -- "$PREFIX"/wgal-* "$UNIT_FILE" "$ACCESS_FILE"
(
    set -euo pipefail
    stage_set "$release"
    stage_access 1000 wg0
    install_new_set
    [ "$(set_in_place)" = new ]
) >"$out" 2>&1
ok "first install: the new set is in place" [ $? -eq 0 ]
ok "first install: the programs" programs_are new
ok "first install: the access list" grep -qx "tunnel wg0" "$ACCESS_FILE"


# --- An upgrade from a version without the command line (before 3.0.0) -------
reset_fixture no-old-cli
run_upgrade
before=$failed
ok "no old wgal: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "no old wgal: the new programs, wgal included" programs_are new
ok "no old wgal: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show

# --- ... whose move of wgal into place fails halfway: the previous set is
# put back, and wgal, which it did not have, is removed -----------------------
reset_fixture no-old-cli
FAIL_MV_AT=4
FAIL_MV_HOW=partial
run_upgrade
before=$failed
ok "no old wgal, its move fails: exit 4 (got $code)" [ "$code" -eq 4 ]
ok "no old wgal, its move fails: the old watchdog is back" is_text "$PREFIX/wgal-watchdog" "old wgal-watchdog"
ok "no old wgal, its move fails: the old wgal-mcp is back" is_text "$PREFIX/wgal-mcp" "old wgal-mcp"
ok "no old wgal, its move fails: the half-moved wgal is removed" [ ! -e "$PREFIX/wgal" ]
ok "no old wgal, its move fails: the closing line" said "runs the old version"
[ "$failed" -eq "$before" ] || show

# --- A record the script of a version before 3.0.0 left (no wgal line),
# with that run's complete previous set in place: read, and replaced ---------
reset_fixture no-old-cli
{
    for name in wgal-watchdog wgal-desktop wgal-mcp wgal-watchdog.service; do
        target=$(target_of "$name")
        echo "$name $(printf '%064d' 1) $(sum_of "$target")"
    done
    echo "started no"
} >"$(record_path)"
run_staging
before=$failed
ok "an older script's record: staging goes on (got $code)" [ "$code" -eq 0 ]
ok "an older script's record: says the previous set is in place" said "the complete previous set it recorded is in place"
ok "an older script's record: wgal is staged" [ -e "$(staged_path wgal new)" ]
[ "$failed" -eq "$before" ] || show

# --- ... but one that lacks another program is not this script's -------------
reset_fixture no-old-cli
{
    for name in wgal-watchdog wgal-desktop wgal wgal-watchdog.service; do
        echo "$name $(printf '%064d' 1) -"
    done
    echo "started no"
} >"$(record_path)"
run_staging
ok "a record without wgal-mcp: refused (got $code)" [ "$code" -eq 65 ]
ok "a record without wgal-mcp: says so" said "is not a record this script wrote"

# --- The command line's link ---------------------------------------------------
link_case() { # what is there: none, ours, file, other
    rm -rf -- "${root:?}/usr/local"
    command mkdir -p "$(dirname "$CLI_LINK")"
    case "$1" in
    ours) ln -s -- "$PREFIX/wgal" "$CLI_LINK" ;;
    file) echo "someone's own" >"$CLI_LINK" ;;
    other) ln -s -- /usr/bin/true "$CLI_LINK" ;;
    esac
    said_cli=$(link_cli 2>"$out")
}
reset_fixture
link_case none
ok "link, none there: made" [ "$(readlink -- "$CLI_LINK")" = "$PREFIX/wgal" ]
ok "link, none there: run as wgal" [ "$said_cli" = wgal ]
link_case ours
ok "link, ours: kept" [ "$(readlink -- "$CLI_LINK")" = "$PREFIX/wgal" ]
ok "link, ours: run as wgal" [ "$said_cli" = wgal ]
link_case file
ok "link, a file there: left alone" is_text "$CLI_LINK" "someone's own"
ok "link, a file there: run by its full path" [ "$said_cli" = "$PREFIX/wgal" ]
ok "link, a file there: says so" said "is not WGAL's link"
link_case other
ok "link, another link: left alone" [ "$(readlink -- "$CLI_LINK")" = /usr/bin/true ]
ok "link, another link: run by its full path" [ "$said_cli" = "$PREFIX/wgal" ]
rm -rf -- "${root:?}/usr/local"
said_cli=$(link_cli 2>"$out")
ok "link, no folder for it: run by its full path" [ "$said_cli" = "$PREFIX/wgal" ]
ok "link, no folder for it: made nothing" [ ! -e "$CLI_LINK" ] && [ ! -L "$CLI_LINK" ]

# --- The window's entries: installed, or removed with --no-window -------------
reset_fixture
echo "entry" >"$release/wgal.desktop"
install_window_entries "$release"
ok "window entries: the menu entry" is_text "$DESKTOP_FILE" entry
ok "window entries: the start at login" is_text "$AUTOSTART_FILE" entry
remove_window_entries
ok "--no-window: the menu entry is removed" [ ! -e "$DESKTOP_FILE" ]
ok "--no-window: the start at login is removed" [ ! -e "$AUTOSTART_FILE" ]

# --- The window's packages: checked with the window, never without it ------
dpkg-query() { echo "not-installed ok"; }
NO_WINDOW=0
ok "with the window: both packages are needed" [ "$(window_packages_missing | tr '
' ' ')" = "libwebkit2gtk-4.1-0 libayatana-appindicator3-1 " ]
NO_WINDOW=1
ok "--no-window: no package is checked" [ -z "$(window_packages_missing)" ]
NO_WINDOW=0
unset -f dpkg-query

# --- setup.sh's closing line gives the full path, which is WGAL's whatever
# /usr/local/bin/wgal is (P2-7) ---------------------------------------------------
ok "setup.sh: the closing line names /opt/wgal/wgal" grep -qF 'Done. \"/opt/wgal/wgal status' "$tests/../setup.sh"
# shellcheck disable=SC2016 # $1 is bash -c's own argument
ok "setup.sh: no closing line sends the user to a bare wgal" bash -c '! grep -qF "\\\"wgal status\\\"" "$1"' _ "$tests/../setup.sh"

# --- --no-window with --install-runtime: refused before anything ---------------
(main --no-window --install-runtime) >"$out" 2>&1
code=$?
ok "--no-window --install-runtime: usage (got $code)" [ "$code" -eq 64 ]
ok "--no-window --install-runtime: says why" said "takes no --install-runtime"

# --- First install: started only once the record says "started yes"
# (Codex review of the CLI, second round, N-P2-1) ----------------------------
reset_fixture
rm -f -- "$PREFIX"/wgal-* "$PREFIX/wgal" "$UNIT_FILE" "$ACCESS_FILE"
(
    set -euo pipefail
    stage_set "$release"
    stage_access 1000 wg0
    first_install
) >"$out" 2>&1
code=$?
before=$failed
ok "first install: runs (got $code)" [ "$code" -eq 0 ]
ok "first install: started with the record saying yes" [ "$(starts_seen)" = "new wgal-watchdog|started yes," ]
ok "first install: the programs" programs_are new
[ "$failed" -eq "$before" ] || show

# --- ... and when the record cannot be marked: not started, start at boot
# off, the copies and the record kept, exit 1 --------------------------------
reset_fixture
rm -f -- "$PREFIX"/wgal-* "$PREFIX/wgal" "$UNIT_FILE" "$ACCESS_FILE"
FAIL_MARK_WRITES=5
(
    set -euo pipefail
    stage_set "$release"
    first_install
) >"$out" 2>&1
code=$?
before=$failed
ok "first install, record not marked: exit 1 (got $code)" [ "$code" -eq 1 ]
ok "first install, record not marked: never started" [ ! -s "$starts" ]
ok "first install, record not marked: says so" said "could not be marked to say that it may run"
ok "first install, record not marked: the record is kept" [ -f "$(record_path)" ]
autostart_off "first install, record not marked"
[ "$failed" -eq "$before" ] || show

# --- The repair a record asks for names its own programs (N-P3-1) -----------
reset_fixture
NEW_SUM=([wgal-watchdog]=a [wgal-desktop]=b [wgal-mcp]=c [wgal]="" [wgal-watchdog.service]=d)
ok "an older record's repair: lists its three programs" [ "$(recorded_programs)" = "wgal-watchdog, wgal-desktop, wgal-mcp (that version has no wgal: remove $PREFIX/wgal if it is there)" ]
NEW_SUM[wgal]=e
ok "this version's repair: lists all four" [ "$(recorded_programs)" = "wgal-watchdog, wgal-desktop, wgal-mcp, wgal" ]
NEW_SUM=()

# --- main --no-window, an upgrade, end to end (P2-9): no package is looked
# at or installed, the window's entries go, the closing lines say how to use
# wgal. The checks that need root and a real system are stand-ins here. ------
elf_release() {
    local name
    for name in "${PROGRAMS[@]}"; do
        printf '\177ELF new %s' "$name" >"$release/$name"
    done
    echo "unit" >"$release/wgal-watchdog.service"
    echo "entry" >"$release/wgal.desktop"
    echo "icon" >"$release/wgal.png"
    echo "uninstaller" >"$release/uninstall.sh"
}
run_main() {
    (
        # As install.sh runs it (this file turned it off after sourcing).
        set -euo pipefail
        id() { if [ "$*" = -u ]; then echo 0; else command id "$@"; fi; }
        stat() {
            case "$*" in
            *%u*) echo 0 ;;
            *%a*) echo 755 ;;
            *) command stat "$@" ;;
            esac
        }
        systemd_running() { :; }
        dpkg-query() { echo "dpkg-query $*" >>"$calls"; echo "not-installed ok"; }
        apt-get() { echo "apt-get $*" >>"$calls"; return 1; }
        RELEASE_DIR=$release
        main "$@"
    ) >"$out" 2>&1 </dev/null
    code=$?
}
reset_fixture
elf_release
command mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$AUTOSTART_FILE")" "$(dirname "$CLI_LINK")"
echo "old entry" >"$DESKTOP_FILE"
echo "old entry" >"$AUTOSTART_FILE"
run_main --no-window
before=$failed
ok "main --no-window: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "main --no-window: no package was looked at" [ "$(grep -c '^dpkg-query\|^apt-get' "$calls")" -eq 0 ]
ok "main --no-window: the menu entry is removed" [ ! -e "$DESKTOP_FILE" ]
ok "main --no-window: the start at login is removed" [ ! -e "$AUTOSTART_FILE" ]
ok "main --no-window: the new programs are in place" cmp -s "$release/wgal" "$PREFIX/wgal"
ok "main --no-window: wgal is linked" [ "$(readlink -- "$CLI_LINK")" = "$PREFIX/wgal" ]
ok "main --no-window: the closing lines" said 'Done, without the window. As the account on the access list (not with sudo): "wgal status" shows WGAL'
ok "main --no-window: the access list kept" is_text "$ACCESS_FILE" "old list"
[ "$failed" -eq "$before" ] || show

# --- ... while the same run with the window looks at the packages and,
# without them and without a terminal to ask, stops before anything ---------
reset_fixture
elf_release
run_main
before=$failed
ok "main with the window, packages missing: exit 65 (got $code)" [ "$code" -eq 65 ]
ok "main with the window, packages missing: they were looked at" grep -q '^dpkg-query' "$calls"
ok "main with the window, packages missing: nothing installed" [ "$(grep -c '^apt-get' "$calls")" -eq 0 ]
ok "main with the window, packages missing: says --no-window" said "or with --no-window on a computer without a desktop"
ok "main with the window, packages missing: nothing staged" [ ! -e "$(staged_path wgal new)" ]
[ "$failed" -eq "$before" ] || show

# --- main, a first install over kept data, end to end (Codex review of the
# CLI, third round, R3-P2-1): started only with the record marked, and a
# failed step of putting it in place starts nothing; nor does a Ctrl-C
# while the record is marked, once the unit is enabled (fourth round,
# R4-P2-1) ---------------------------------------------------------------------
first_fixture() {
    reset_fixture
    elf_release
    rm -f -- "$PREFIX"/wgal-* "$PREFIX/wgal" "$UNIT_FILE"
    command mkdir -p "$(dirname "$CLI_LINK")"
    echo disabled >"$enabled"
}
first_fixture
run_main --no-window
before=$failed
ok "main, first install: exit 0 (got $code)" [ "$code" -eq 0 ]
ok "main, first install: started once, with the record saying yes" [ "$(starts_seen)" = "$(printf '\177ELF new wgal-watchdog')|started yes," ]
ok "main, first install: its start at boot is on" is_enabled enabled
ok "main, first install: nothing staged is left" no_staged
[ "$failed" -eq "$before" ] || show
for fail in enable daemon-reload mark-interrupt; do
    first_fixture
    FAIL=$fail
    run_main --no-window
    before=$failed
    ok "main, first install, $fail fails: exit 1 (got $code)" [ "$code" -eq 1 ]
    ok "main, first install, $fail fails: never started" [ ! -s "$starts" ]
    # shellcheck disable=SC2016 # $1 is bash -c's own argument
    ok "main, first install, $fail fails: the record does not say started yes" bash -c '! grep -qx "started yes" "$1"' _ "$(record_path)"
    ok "main, first install, $fail fails: says it was not started" said "was NOT started: the first install failed while"
    ok "main, first install, $fail fails: its start at boot is off" is_enabled disabled
    [ "$failed" -eq "$before" ] || show
done
FAIL=""

echo "install-recovery.test.sh: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
