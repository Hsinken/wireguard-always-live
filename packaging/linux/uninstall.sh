#!/usr/bin/env bash
# Removes WG Always Live (WGAL) from Ubuntu.
#
#   sudo /opt/wgal/uninstall.sh [--remove-data]
#
# Quit the WGAL window first. It stops and disables wgal-watchdog.service,
# removes the unit, /opt/wgal and the menu and autostart entries.
# /var/lib/wgal (state, events, settings, the bot token) and /etc/wgal (the
# access list) are kept unless --remove-data is given.
#
# It never starts, stops or changes a wg-quick unit, not even one WGAL
# stopped: it records every wg-quick unit's state before and after, reports
# any difference, and for every wg-quick unit that is not running at the end
# prints the command to start it again yourself.
#
# To stop WGAL without removing it: sudo systemctl disable --now wgal-watchdog
#
# Exit codes: 0 done; 1 stopping the service reported an error (the rest was
# still removed once the watchdog had ended), or it did not end within 50 s
# (then no WGAL file was removed, though the service may already be
# disabled); 2 a wg-quick unit changed while the script ran, or their
# states could not be read again at the end (2 wins over 1); 64 wrong usage; 65 a check
# failed before anything changed (systemctl not answering included).
set -euo pipefail
umask 022
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

readonly PREFIX=/opt/wgal
readonly UNIT=wgal-watchdog.service
readonly UNIT_FILE=/etc/systemd/system/wgal-watchdog.service
readonly DESKTOP_FILE=/usr/share/applications/wgal.desktop
readonly AUTOSTART_FILE=/etc/xdg/autostart/wgal.desktop
# install.sh's link to the command line; removed only while it is WGAL's.
readonly CLI_LINK=/usr/local/bin/wgal
readonly DATA=(/var/lib/wgal /etc/wgal /run/wgal)

die() {
    echo "uninstall.sh: $1" >&2
    exit "${2:-65}"
}

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

# The body is one function, read whole before it runs: the script may be
# /opt/wgal/uninstall.sh, which it removes.
# Removes the link $1 only while it is install.sh's: a link to $2.
# Anything else there is not WGAL's and is left as it is.
remove_cli_link() {
    if [ -L "$1" ] && [ "$(readlink -- "$1")" = "$2" ]; then
        rm -f -- "$1"
    fi
}

main() {
    local remove_data=0
    case "${1:-}" in
    "") ;;
    --remove-data) remove_data=1 ;;
    *)
        sed -n '4,4p' "$0" >&2
        exit 64
        ;;
    esac
    [ $# -le 1 ] || die "it takes only --remove-data" 64
    [ "$(id -u)" -eq 0 ] || die "run it with sudo (as root)"
    local path
    for path in "$PREFIX" "${DATA[@]}"; do
        if [ -L "$path" ]; then
            die "$path is a link, not WGAL's folder; check and remove it yourself"
        fi
    done
    if pgrep -x wgal-desktop >/dev/null 2>&1; then
        die "a WGAL window is open; quit it first (its tray menu)"
    fi

    local before
    before=$(wireguard_states) ||
        die "systemctl could not list the wg-quick units; nothing was changed"
    local status=0

    if [ -e "$UNIT_FILE" ]; then
        echo "Stopping and disabling $UNIT"
        systemctl disable --now "$UNIT" || status=1
        # No file is removed while the watchdog still runs: its data and
        # its files are in use.
        local waited=0
        while pgrep -x -u 0 wgal-watchdog >/dev/null 2>&1; do
            [ "$waited" -lt 50 ] ||
                die "the watchdog is still running 50 s after its stop; no WGAL file was removed, though the service may already be disabled (try: sudo systemctl kill $UNIT, then run this again)" 1
            sleep 1
            waited=$((waited + 1))
        done
        rm -f -- "$UNIT_FILE"
        systemctl daemon-reload
        systemctl reset-failed "$UNIT" 2>/dev/null || true
    fi
    rm -f -- "$DESKTOP_FILE" "$AUTOSTART_FILE"
    remove_cli_link "$CLI_LINK" "$PREFIX/wgal"
    [ -d "$PREFIX" ] && rm -rf -- "$PREFIX"
    if [ "$remove_data" -eq 1 ]; then
        for path in "${DATA[@]}"; do
            [ -d "$path" ] && rm -rf -- "$path"
        done
        echo "Removed WGAL and its data."
    else
        echo "Removed WGAL; its data is kept in /var/lib/wgal and /etc/wgal" \
            "(sudo $0 --remove-data removes it too)."
    fi
    [ "$status" -eq 0 ] || echo "uninstall.sh: stopping $UNIT reported an error" >&2

    local after
    if ! after=$(wireguard_states); then
        echo "uninstall.sh: WARNING: systemctl could not list the wg-quick units at the end," \
            "so the script cannot confirm they are as it found them" >&2
        status=2
    elif [ "$before" != "$after" ]; then
        echo "uninstall.sh: WARNING: a wg-quick unit changed while the script ran (the script" \
            "itself never touches them):" >&2
        diff <(echo "$before") <(echo "$after") >&2 || true
        status=2
    fi
    local unit
    while read -r unit _; do
        case "$unit" in
        wg-quick@?*.service)
            if ! systemctl is-active --quiet "$unit"; then
                echo "Not running: $unit. To start it yourself: sudo systemctl start $unit"
            fi
            ;;
        esac
    done < <(systemctl list-units --all --plain --no-legend --type=service 'wg-quick@*' 2>/dev/null)
    exit "$status"
}

# Sourced (tests/uninstall.test.sh does this): only the functions are
# defined.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
