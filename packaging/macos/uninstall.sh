#!/bin/bash
# Removes WG Always Live (WGAL) from macOS for the signed-in user.
#
#   "$HOME/Library/Application Support/WGAL/uninstall.sh" [--remove-data]
#
# (install.sh keeps this copy there; the one in the release folder works too.)
#
# Run it as yourself, not with sudo; it asks for an administrator's password
# only when it cannot remove the app from /Applications by itself. Quit the
# WGAL window first. It stops and removes the two LaunchAgents (the watchdog
# and the window at login) and the app, and enables the watchdog's label
# in launchd again (an install run that did not start the watchdog
# disables it), so it leaves no disabled override behind. Your WGAL data
# (~/Library/Application Support/WGAL), its log (~/Library/Logs/WGAL) and the
# bot token in your login Keychain are kept unless --remove-data is given.
#
# It never connects, disconnects or changes a VPN configuration of the
# official WireGuard app: it records `scutil --nc list` before and after and
# reports any difference.
#
# To stop WGAL without removing it:
#   launchctl bootout gui/$(id -u)/io.github.hsinken.wg-always-live.watchdog
#
# Exit codes: 0 done; 1 the watchdog did not stop (no WGAL file was
# removed, though its LaunchAgent may be unloaded), or the bot token could not
# be removed from the Keychain or checked; 2 a VPN configuration changed
# while the script ran; 64 wrong usage; 65 a check failed before anything
# changed.
set -euo pipefail
umask 022
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

readonly APP="/Applications/WG Always Live.app"
# What install.sh may leave beside the app after an upgrade that did not
# finish: the new app under its hidden name and the previous one.
readonly LABEL=io.github.hsinken.wg-always-live.watchdog
readonly WINDOW_LABEL=io.github.hsinken.wg-always-live.window
# Where the watchdog keeps the bot token (crates/wgal-runtime notify/secret.rs),
# for the message when it cannot be removed.
readonly KEYCHAIN_SERVICE=io.github.hsinken.wg-always-live

die() {
    echo "uninstall.sh: $1" >&2
    exit "${2:-65}"
}

# Removes what install.sh keeps for an upgrade that did not finish: the
# hidden apps beside the app $1 (".<name>.new" and ".<name>.old") and its
# staging folder $2. Left, a hidden previous app with no app beside it
# stops the next install (Codex review of the CLI, second round, N-P2-2).
# A link there is left as it is: it is not WGAL's.
remove_install_leftovers() {
    local app=$1 stage=$2 copy
    for copy in "$(dirname "$app")/.$(basename "$app").new" "$(dirname "$app")/.$(basename "$app").old"; do
        if [ -d "$copy" ] && [ ! -L "$copy" ]; then
            as_needed rm -rf -- "$copy"
        fi
    done
    if [ -d "$stage" ] && [ ! -L "$stage" ]; then
        rm -rf -- "$stage"
    fi
}

as_needed() {
    if [ -w /Applications ]; then
        "$@"
    else
        sudo "$@"
    fi
}

main() {
    local remove_data=0
    case "${1:-}" in
    "") ;;
    --remove-data) remove_data=1 ;;
    *) die "usage: ./uninstall.sh [--remove-data]" 64 ;;
    esac
    [ $# -le 1 ] || die "it takes only --remove-data" 64
    [ "$(uname -s)" = Darwin ] || die "this is the macOS uninstaller"
    local uid
    uid=$(id -u)
    [ "$uid" -ne 0 ] || die "run it as yourself, not with sudo"
    local data="$HOME/Library/Application Support/WGAL"
    local logs="$HOME/Library/Logs/WGAL"
    local agents="$HOME/Library/LaunchAgents"
    if [ -L "$APP" ] || [ -L "$data" ] || [ -L "$logs" ]; then
        die "the app or a WGAL folder is a link; check and remove it yourself"
    fi
    if pgrep -x -u "$uid" wgal-desktop >/dev/null 2>&1; then
        die "a WGAL window is open; quit it first (its menu bar menu)"
    fi

    local before
    before=$(scutil --nc list 2>/dev/null || true)

    if launchctl print "gui/$uid/$LABEL" >/dev/null 2>&1; then
        launchctl bootout "gui/$uid/$LABEL" ||
            die "stopping the watchdog failed; no WGAL file was removed" 1
    fi
    # No file is removed while the watchdog still runs.
    local waited=0
    while pgrep -x -u "$uid" wgal-watchdog >/dev/null 2>&1; do
        [ "$waited" -lt 50 ] ||
            die "the watchdog is still running 50 s after its stop; no WGAL file was removed (its LaunchAgent may already be unloaded)" 1
        sleep 1
        waited=$((waited + 1))
    done
    # The token is removed by the app's own watchdog, before the app goes:
    # the Keychain's status tells "there was none" from a failure, which the
    # `security` tool cannot.
    local status=0 token=kept
    if [ "$remove_data" -eq 1 ]; then
        if [ -x "$APP/Contents/MacOS/wgal-watchdog" ]; then
            if "$APP/Contents/MacOS/wgal-watchdog" --forget-token >/dev/null; then
                token=removed
            else
                token=failed
                status=1
            fi
        else
            token=unchecked
            status=1
        fi
    fi
    if launchctl print "gui/$uid/$WINDOW_LABEL" >/dev/null 2>&1; then
        launchctl bootout "gui/$uid/$WINDOW_LABEL" || true
    fi
    rm -f -- "$agents/$LABEL.plist" "$agents/$WINDOW_LABEL.plist"
    # An install run that did not start the watchdog may have disabled its
    # label (a lasting launchd override); enabled again, so none is left
    # behind that would stop a later install's watchdog from loading.
    launchctl enable "gui/$uid/$LABEL" 2>/dev/null ||
        echo "uninstall.sh: note: launchctl enable gui/$uid/$LABEL failed; a later install enables it again" >&2
    if [ -d "$APP" ]; then
        as_needed rm -rf -- "$APP"
    fi
    remove_install_leftovers "$APP" "$data/upgrade"
    if [ "$remove_data" -eq 1 ]; then
        rm -rf -- "$data" "$logs"
        case "$token" in
        removed) echo "Removed WGAL, its data, its log and the bot token in your Keychain." ;;
        failed)
            echo "uninstall.sh: removed WGAL, its data and its log, but the bot token could not" \
                "be removed from your Keychain. Remove the item \"$KEYCHAIN_SERVICE\" yourself in" \
                "Keychain Access." >&2
            ;;
        *)
            echo "uninstall.sh: removed WGAL, its data and its log; the app's wgal-watchdog was" \
                "missing or could not run, so the bot token in your Keychain could not be" \
                "checked. Look for the item" \
                "\"$KEYCHAIN_SERVICE\" in Keychain Access and remove it yourself." >&2
            ;;
        esac
    else
        rm -f -- "$data/uninstall.sh"
        echo "Removed WGAL; your data ($data), its log and the bot token in your Keychain are" \
            "kept (uninstall.sh --remove-data from the release folder removes them too)."
    fi

    local after
    after=$(scutil --nc list 2>/dev/null || true)
    if [ "$before" != "$after" ]; then
        echo "uninstall.sh: WARNING: a VPN configuration changed while the script ran (the" \
            "script itself never touches them):" >&2
        diff <(echo "$before") <(echo "$after") >&2 || true
        status=2
    fi
    exit "$status"
}

# Sourced (tests/install-recovery.test.sh does this): only the functions
# are defined.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
