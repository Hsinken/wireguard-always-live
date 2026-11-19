#!/usr/bin/env bash
# Tests of the macOS make-release.sh build record (FIX-06): --no-build packs
# the programs only when the record says they were built from this commit with
# this version and none changed since. Fake files in a temporary folder;
# nothing is built, packed or installed.
#
#   bash packaging/macos/tests/make-release.test.sh
#
# Runs with any bash that has shasum (on Linux too); it is not a macOS run.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd -P)
# Sourced, the script only defines its functions: it builds nothing.
# shellcheck source=SCRIPTDIR/../make-release.sh
source "$here/../make-release.sh"

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
refused() { ! check_record "$@" >/dev/null; }
accepted() { check_record "$@" >/dev/null; }
says() { # text, then check_record's arguments
    local text=$1
    shift
    local why
    why=$(check_record "$@") && return 1
    [[ "$why" == *"$text"* ]]
}

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
cd "$work"
app="target/release/bundle/macos/WG Always Live.app"
mkdir -p "$app/Contents/MacOS"
programs=(target/release/wgal-watchdog target/release/wgal-mcp
    "$app/Contents/Info.plist" "$app/Contents/MacOS/wgal-desktop")
for program in "${programs[@]}"; do
    echo "fake $program" >"$program"
done
commit=0123456789abcdef0123456789abcdef01234567

check "no record is refused" says "no build record" "$commit" 4.2.1 "${programs[@]}"
build_record "$commit" 4.2.1 "${programs[@]}" >"$record"
check "the record holds one version" grep -qx "version 4.2.1" "$record"
check "the same commit, version and programs are accepted" accepted "$commit" 4.2.1 "${programs[@]}"
check "another commit is refused" says "another commit" fedcba9876543210fedcba9876543210fedcba98 4.2.1 "${programs[@]}"
check "another version is refused" says "version differs" "$commit" 4.2.2 "${programs[@]}"
echo "another build of the same version" >target/release/wgal-mcp
check "a changed program is refused" says "changed" "$commit" 4.2.1 "${programs[@]}"
echo "fake target/release/wgal-mcp" >target/release/wgal-mcp
check "the program restored is accepted again" accepted "$commit" 4.2.1 "${programs[@]}"
echo "signed in place" >>"$app/Contents/MacOS/wgal-desktop"
check "a changed file in the app is refused" says "changed" "$commit" 4.2.1 "${programs[@]}"
rm target/release/wgal-mcp
check "a missing program is refused" refused "$commit" 4.2.1 "${programs[@]}"

if [ "$failures" -gt 0 ]; then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
