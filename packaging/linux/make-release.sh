#!/usr/bin/env bash
# Builds the Ubuntu release, dist/wgal-<version>-linux-<arch>.tar.gz, where
# the version is the one the whole set shares since 4.2.1 (the Cargo
# workspace's; owner decision of 2026-11-19).
# Run it only when the owner asks for a release, on Ubuntu with the build
# prerequisites of AGENTS.md ("Toolchain"); it needs no root and installs
# nothing.
#
#   packaging/linux/make-release.sh [--no-build]
#
# From a clean checkout it builds wgal-watchdog, wgal-mcp, wgal and wgal-desktop
# in release mode and puts them in dist/wgal-<version>-linux-<arch>/
# with install.sh, uninstall.sh, the systemd unit, the menu entry and its
# icon, README.md and README.zh-TW.md, INSTALL.md and INSTALL.zh-TW.md
# (the guide an agent can follow, in English and in Traditional Chinese)
# and RELEASE.txt (the version, the commit and every file's SHA-256), then
# packs that folder. Not signed (owner decision of 2026-11-05).
#
# A build writes target/release/wgal-build.txt beside the programs: the
# commit, the version and each program's SHA-256. --no-build packs the
# programs already there only when that record says they were built from
# this very commit with this version and none has changed since (FIX-06:
# a version mark alone does not tell one commit's build from another's).
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
            sha256sum -- "$file"
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
    echo "usage: packaging/linux/make-release.sh [--no-build]" >&2
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

programs=(target/release/wgal-watchdog target/release/wgal-desktop target/release/wgal-mcp target/release/wgal)
if [ "$build" -eq 1 ]; then
    cargo build --release --locked -p wgal-runtime --bin wgal-watchdog
    cargo build --release --locked -p wgal-mcp
    cargo build --release --locked -p wgal-cli
    pnpm --filter @wgal/desktop run tauri build --no-bundle
    build_record "$commit" "$version" "${programs[@]}" >"$record"
elif ! why=$(check_record "$commit" "$version" "${programs[@]}"); then
    echo "make-release.sh: --no-build: $why; nothing was packed" >&2
    exit 65
fi

name="wgal-$version-linux-$arch"
stage="dist/$name"
rm -rf -- "$stage" "dist/$name.tar.gz"
mkdir -p "$stage"
for program in wgal-watchdog wgal-desktop wgal-mcp wgal; do
    install -m 0755 "target/release/$program" "$stage/$program"
done
install -m 0755 packaging/linux/install.sh packaging/linux/uninstall.sh packaging/linux/setup.sh "$stage/"
install -m 0644 packaging/linux/wgal-watchdog.service packaging/linux/wgal.desktop \
    packaging/linux/README.md packaging/linux/README.zh-TW.md \
    packaging/INSTALL.md packaging/INSTALL.zh-TW.md "$stage/"
install -m 0644 apps/desktop/src-tauri/icons/128x128.png "$stage/wgal.png"
# Every file but RELEASE.txt itself, hashed before it is written.
hashes=$(cd "$stage" && sha256sum -- * | sort -k 2)
{
    echo "WG Always Live $version for Ubuntu ($arch)"
    echo "Version $version: the window, the watchdog, wgal-mcp and wgal"
    echo "Commit $commit"
    echo "Not signed. Install and remove with the scripts: see INSTALL.md (INSTALL.zh-TW.md in Traditional Chinese)."
    echo
    echo "SHA-256"
    echo "$hashes"
} >"$stage/RELEASE.txt"
tar -C dist -czf "dist/$name.tar.gz" "$name"
echo "Built dist/$name.tar.gz"
sha256sum "dist/$name.tar.gz"
}

# Sourced (by the tests) it only defines the functions.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
