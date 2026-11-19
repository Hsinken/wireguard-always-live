#!/bin/bash
# Builds the macOS release, dist/wgal-<version>-macos-<arch>.zip, where the
# version is the one the whole set shares since 4.2.1 (the Cargo
# workspace's; owner decision of 2026-11-19); the app's Info.plist takes it
# too, as Tauri does with no version in its configuration. Run it
# only when the owner asks for a release, on a Mac with the build
# prerequisites of AGENTS.md ("Toolchain"); it installs nothing.
#
#   packaging/macos/make-release.sh [--no-build]
#
# From a clean checkout it builds wgal-watchdog, wgal-mcp and wgal (the
# command line), and the window as "WG Always Live.app" (`tauri build
# --bundles app`), puts the three programs inside the app beside the window
# (Contents/MacOS, where the window looks for wgal-mcp and the LaunchAgent
# starts wgal-watchdog), and
# signs the app ad hoc (`codesign --sign -`): no developer ID, but Apple
# silicon runs only signed code, and adding the programs broke the bundle's
# own ad hoc signature. Then it puts the app in
# dist/wgal-<version>-macos-<arch>/ with install.sh, uninstall.sh, README.md and
# README.zh-TW.md, INSTALL.md and INSTALL.zh-TW.md (the guide an agent can
# follow, in English and in Traditional Chinese) and RELEASE.txt (the
# version, the commit and the SHA-256 of the scripts, the documents and
# every program), and zips that folder.
# Not signed with a developer ID nor notarized (owner decision of
# 2026-11-05).
#
# A build writes target/release/wgal-build.txt: the commit, the version
# and the SHA-256 of the three programs and of every file in the built app.
# The programs are put into, and the app signed as, the copy in dist/, so
# what was built stays as it was. --no-build packs what is already there
# only when that record says it was built from this very commit with this
# version and nothing has changed since (FIX-06: neither a version mark
# nor a valid signature tells one commit's build from another's).
set -euo pipefail

readonly record=target/release/wgal-build.txt

# The build record for the given files as they are now: the commit, the
# version and each file's SHA-256 (a missing file hashes as "missing").
build_record() { # commit version file...
    local file
    echo "commit $1"
    echo "version $2"
    shift 2
    for file in "$@"; do
        if [ -f "$file" ]; then
            shasum -a 256 -- "$file"
        else
            echo "missing  $file"
        fi
    done
}

# Prints why the files may not be packed as built from the commit, and
# fails, unless the record says they were built from it with this version
# and none has changed since (FIX-06).
check_record() { # commit version file...
    if [ ! -f "$record" ]; then
        echo "there is no build record ($record); build once without --no-build"
        return 1
    fi
    local recorded
    recorded=$(sed -n 's/^commit //p' "$record")
    if [ "$recorded" != "$1" ]; then
        echo "the programs were built from another commit (${recorded:-none}), not $1"
        return 1
    fi
    if [ "$(build_record "$@")" != "$(cat "$record")" ]; then
        echo "a program changed since it was built, or the version differs from the record"
        return 1
    fi
}

main() {
root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$root"
build=1
case "${1:-}" in
"") ;;
--no-build) build=0 ;;
*)
    echo "usage: packaging/macos/make-release.sh [--no-build]" >&2
    exit 64
    ;;
esac

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "make-release.sh: the checkout has uncommitted changes; a release is built from a commit" >&2
    exit 65
fi
commit=$(git rev-parse HEAD)
version=$(awk '/^\[workspace.package\]/{p=1; next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/, "", $0); sub(/version=/, "", $0); print; exit}' Cargo.toml)
arch=$(uname -m)
app="target/release/bundle/macos/WG Always Live.app"

if [ "$build" -eq 1 ]; then
    cargo build --release --locked -p wgal-runtime --bin wgal-watchdog
    cargo build --release --locked -p wgal-mcp
    cargo build --release --locked -p wgal-cli
    pnpm --filter @wgal/desktop run tauri build --bundles app
fi
[ -d "$app" ] || {
    echo "make-release.sh: $app is missing" >&2
    exit 65
}
# The three programs and every file of the app as built (bash 3.2: no
# mapfile).
built=(target/release/wgal-watchdog target/release/wgal-mcp target/release/wgal)
while IFS= read -r file; do
    built+=("$file")
done < <(find "$app" -type f | LC_ALL=C sort)
if [ "$build" -eq 1 ]; then
    build_record "$commit" "$version" "${built[@]}" >"$record"
elif ! why=$(check_record "$commit" "$version" "${built[@]}"); then
    echo "make-release.sh: --no-build: $why; nothing was packed" >&2
    exit 65
fi

name="wgal-$version-macos-$arch"
stage="dist/$name"
packed="$stage/WG Always Live.app"
rm -rf -- "$stage" "dist/$name.zip"
mkdir -p "$stage"
ditto "$app" "$packed"
install -m 0755 target/release/wgal-watchdog target/release/wgal-mcp target/release/wgal "$packed/Contents/MacOS/"
codesign --force --deep --sign - "$packed"
codesign --verify --deep --strict "$packed"
install -m 0755 packaging/macos/install.sh packaging/macos/uninstall.sh "$stage/"
install -m 0644 packaging/macos/README.md packaging/macos/README.zh-TW.md \
    packaging/INSTALL.md packaging/INSTALL.zh-TW.md "$stage/"
{
    echo "WG Always Live $version for macOS ($arch)"
    echo "Version $version: the window, the watchdog, wgal-mcp and wgal"
    echo "Commit $commit"
    echo "Signed ad hoc only, not notarized. Install and remove with the scripts: see INSTALL.md (INSTALL.zh-TW.md in Traditional Chinese)."
    echo
    echo "SHA-256"
    (cd "$stage" && shasum -a 256 install.sh uninstall.sh README.md README.zh-TW.md INSTALL.md INSTALL.zh-TW.md \
        "WG Always Live.app/Contents/MacOS/"*)
} >"$stage/RELEASE.txt"
(cd dist && ditto -c -k --keepParent "$name" "$name.zip")
echo "Built dist/$name.zip"
shasum -a 256 "dist/$name.zip"
}

# Sourced (by the tests) it only defines the functions.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
