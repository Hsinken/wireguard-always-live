#!/usr/bin/env bash
# Runs make-release.sh --no-build itself (FIX-06, Codex review): in a scratch
# git checkout of its own, with fake programs and the files the script packs,
# it must refuse, and pack nothing, without a build record, with a record of
# another commit and with a program changed since the record; with a record
# of this commit it packs. Nothing is built; nothing is written outside the
# scratch folder. The one version (since 4.2.1) is the workspace's; the
# package.json here has none, as the repository's has none.
#
#   bash packaging/linux/tests/make-release.entry.test.sh
set -euo pipefail

source_root=$(cd "$(dirname "$0")/../../.." && pwd -P)
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

repo="$work/repo"
mkdir -p "$repo/packaging/linux" "$repo/apps/desktop/src-tauri/icons" "$repo/target/release"
cp "$source_root/packaging/linux/make-release.sh" "$repo/packaging/linux/"
for file in install.sh uninstall.sh setup.sh wgal-watchdog.service wgal.desktop README.md README.zh-TW.md; do
    echo "fake $file" >"$repo/packaging/linux/$file"
done
echo "fake guide" >"$repo/packaging/INSTALL.md"
echo "fake zh-TW guide" >"$repo/packaging/INSTALL.zh-TW.md"
echo "fake icon" >"$repo/apps/desktop/src-tauri/icons/128x128.png"
printf '[workspace.package]\nversion = "9.8.7"\n' >"$repo/Cargo.toml"
echo '{ "name": "@wgal/desktop", "private": true }' >"$repo/apps/desktop/package.json"
printf 'target/\ndist/\n' >"$repo/.gitignore"
cd "$repo"
git init -q .
git -c user.name=test -c user.email=test@example.invalid add -A
git -c user.name=test -c user.email=test@example.invalid commit -q -m scratch
head=$(git rev-parse HEAD)
programs=(target/release/wgal-watchdog target/release/wgal-desktop target/release/wgal-mcp target/release/wgal)
for program in "${programs[@]}"; do
    echo "fake $program" >"$program"
done
record() { # commit [version]
    {
        echo "commit $1"
        echo "version ${2:-9.8.7}"
        sha256sum -- "${programs[@]}"
    } >target/release/wgal-build.txt
}
out=""
code=0
run() {
    code=0
    out=$(bash packaging/linux/make-release.sh --no-build 2>&1) || code=$?
}
refused_with() { [ "$code" -eq 65 ] && [[ "$out" == *"$1"* ]]; }
nothing_packed() { [ ! -e dist ]; }

run
check "no record: refused" refused_with "no build record"
check "no record: nothing packed" nothing_packed

record fedcba9876543210fedcba9876543210fedcba98
run
check "another commit: refused" refused_with "another commit"
check "another commit: nothing packed" nothing_packed

record "$head"
echo "another build of the same version" >target/release/wgal-mcp
run
check "a changed program: refused" refused_with "changed"
check "a changed program: nothing packed" nothing_packed

# Only the command line replaced, the commit and versions the same.
record "$head"
echo "another wgal" >target/release/wgal
run
check "only the command line changed: refused" refused_with "changed"
check "only the command line changed: nothing packed" nothing_packed
echo "fake target/release/wgal" >target/release/wgal

# A record of a build without the command line (before 3.0.0).
record "$head"
grep -v 'target/release/wgal$' target/release/wgal-build.txt >"$work/older"
cp "$work/older" target/release/wgal-build.txt
run
check "a record without the command line: refused" refused_with "changed"
check "a record without the command line: nothing packed" nothing_packed

# The same commit and programs, built as another version.
record "$head" 9.8.6
run
check "another version: refused" refused_with "version differs"
check "another version: nothing packed" nothing_packed

record "$head"
run
check "this commit: packed" test "$code" -eq 0 -a -f "dist/wgal-9.8.7-linux-$(uname -m).tar.gz"
check "this commit: RELEASE.txt names it" grep -q "Commit $head" "dist/wgal-9.8.7-linux-$(uname -m)/RELEASE.txt"
check "this commit: the command line is packed" test -x "dist/wgal-9.8.7-linux-$(uname -m)/wgal"
check "this commit: RELEASE.txt has its sum" grep -q "  wgal$" "dist/wgal-9.8.7-linux-$(uname -m)/RELEASE.txt"
for doc in README.md README.zh-TW.md INSTALL.md INSTALL.zh-TW.md; do
    check "this commit: $doc is packed" test -f "dist/wgal-9.8.7-linux-$(uname -m)/$doc"
    check "this commit: RELEASE.txt has the sum of $doc" grep -q "  $doc$" "dist/wgal-9.8.7-linux-$(uname -m)/RELEASE.txt"
done
check "this commit: RELEASE.txt opens with the one version" test "$(head -2 "dist/wgal-9.8.7-linux-$(uname -m)/RELEASE.txt")" = "WG Always Live 9.8.7 for Ubuntu ($(uname -m))
Version 9.8.7: the window, the watchdog, wgal-mcp and wgal"
check "this commit: RELEASE.txt points to the English guide" grep -q "see INSTALL.md" "dist/wgal-9.8.7-linux-$(uname -m)/RELEASE.txt"

if [ "$failures" -gt 0 ]; then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
