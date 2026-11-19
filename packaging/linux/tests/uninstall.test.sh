#!/usr/bin/env bash
# Tests uninstall.sh's removal of the command line's link: only install.sh's
# own link (to /opt/wgal/wgal) is removed; a file, another link or nothing
# there is left as it is. It sources uninstall.sh (which then runs nothing)
# and works only in a scratch folder of its own.
#
#   bash packaging/linux/tests/uninstall.test.sh
set -euo pipefail

tests=$(cd "$(dirname "$0")" && pwd -P)
# shellcheck source=SCRIPTDIR/../uninstall.sh
source "$tests/../uninstall.sh"

failures=0
check() { # name, then the command that must succeed
    local name=$1
    shift
    if "$@"; then
        echo "PASS $name"
    else
        echo "FAIL $name"
        failures=$((failures + 1))
    fi
}

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
link=$work/bin/wgal
target=$work/opt/wgal/wgal
mkdir -p "$work/bin" "$work/opt/wgal"
echo "wgal" >"$target"

ln -s -- "$target" "$link"
remove_cli_link "$link" "$target"
check "WGAL's link: removed" test ! -e "$link" -a ! -L "$link"
check "WGAL's link: the program itself is not touched" test -f "$target"

ln -s -- /usr/bin/true "$link"
remove_cli_link "$link" "$target"
check "another link: left" test "$(readlink -- "$link")" = /usr/bin/true
rm -f -- "$link"

echo "someone's own" >"$link"
remove_cli_link "$link" "$target"
check "a file: left" test "$(cat -- "$link")" = "someone's own"
rm -f -- "$link"

remove_cli_link "$link" "$target"
check "nothing there: still nothing" test ! -e "$link"

if [ "$failures" -gt 0 ]; then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
