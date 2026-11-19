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
# The programs must not name the computer that built them (4.3.0):
#
# - It builds only in the expected environment. It refuses, building
#   nothing, when compiler flags or Cargo's output folder are set from
#   outside: the variables RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS,
#   CARGO_BUILD_RUSTFLAGS, CARGO_TARGET_<triple>_RUSTFLAGS,
#   CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR or CARGO_BUILD_TARGET, or a
#   Cargo configuration file (.cargo/config.toml or .cargo/config in a
#   folder cargo runs in or any folder above it, or in Cargo's home) that
#   mentions rustflags or target-dir or has a key named target.
# - It resolves Cargo's home (CARGO_HOME, relative to where the script was
#   started, or ~/.cargo), the repository and the home folder to absolute
#   paths without symbolic links, exports that CARGO_HOME, and builds with
#   only two flags, which write those two folders in the programs as /cargo
#   and /wgal (CARGO_ENCODED_RUSTFLAGS, --remap-path-prefix).
# - The window is built with --locked too, and the checkout must be
#   unchanged after the builds (Cargo.lock included).
# - Before packing, every file of the release, RELEASE.txt included, is
#   searched for the three folders' paths, as UTF-8 and as UTF-16LE. A hit,
#   or any error, packs nothing; the message names the file inside the
#   release, which folder and which encoding, never the path itself.
# - The same-name folder and archive in dist/ are deleted first; the
#   release is assembled in a temporary folder and its archive renamed into
#   place only when everything passed, so a failed run leaves neither.
#
# A build writes target/release/wgal-build.txt beside the programs: the
# commit, the version and each program's SHA-256. --no-build packs the
# programs already there only when that record says they were built from
# this very commit with this version and none has changed since (FIX-06:
# a version mark alone does not tell one commit's build from another's).
# It checks them for the three paths too.
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

# The folder as an absolute path without symbolic links, a relative path
# taken from the first folder; fails if it does not exist.
real_dir() { # from folder
    (cd -- "$1" 2>/dev/null && cd -- "$2" 2>/dev/null && pwd -P)
}

has_control() { # text
    case "$1" in
    *[[:cntrl:]]*) return 0 ;;
    esac
    return 1
}

# The exported variables that would add compiler flags or move Cargo's
# output, as themselves or as Cargo's own configuration keys. Prints each.
refused_variables() {
    local name
    for name in $(compgen -e); do
        case "$name" in
        RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_RUSTFLAGS | CARGO_TARGET_*_RUSTFLAGS | \
            CARGO_TARGET_DIR | CARGO_BUILD_TARGET_DIR | CARGO_BUILD_TARGET)
            echo "$name"
            ;;
        esac
    done
}

# Cargo's configuration files that apply to cargo run in any of the
# folders: .cargo/config.toml and .cargo/config there and in every folder
# above it, and config.toml and config in Cargo's home. Prints each one
# that exists, once.
cargo_configs() { # cargo_home folder...
    local home=$1 dir file
    shift
    {
        for dir in "$@"; do
            while :; do
                for file in "${dir%/}/.cargo/config.toml" "${dir%/}/.cargo/config"; do
                    if [ -e "$file" ] || [ -L "$file" ]; then
                        echo "$file"
                    fi
                done
                [ "$dir" = / ] && break
                dir=$(dirname -- "$dir")
            done
        done
        for file in "$home/config.toml" "$home/config"; do
            if [ -e "$file" ] || [ -L "$file" ]; then
                echo "$file"
            fi
        done
    } | LC_ALL=C sort -u
}

# What a configuration file has that a release build refuses, whatever its
# TOML spelling: any mention of rustflags or target-dir (in a comment too),
# or a key named target (build.target moves the output to target/<triple>/).
# Prints it and succeeds; fails when the file has none. A file that cannot
# be read is refused.
config_refusal() { # file
    local key rc
    if [ ! -f "$1" ] || [ ! -r "$1" ]; then
        echo "it is not a readable file"
        return 0
    fi
    for key in rustflags target-dir; do
        rc=0
        grep -qF -- "$key" "$1" || rc=$?
        case "$rc" in
        0)
            echo "$key"
            return 0
            ;;
        1) ;;
        *)
            echo "it could not be read"
            return 0
            ;;
        esac
    done
    rc=0
    grep -qE -- "(^|[^A-Za-z0-9_-])target[\"']?[[:space:]]*=" "$1" || rc=$?
    case "$rc" in
    0) echo "a key named target" ;;
    1) return 1 ;;
    *) echo "it could not be read" ;;
    esac
}

# Where a configuration file is, without the account's folders.
config_place() { # file root cargo_home
    case "$1" in
    "$2"/*) echo "${1#"$2"/}" ;;
    "$3"/*) echo "Cargo's home: ${1#"$3"/}" ;;
    *) echo "a folder above the repository: .cargo/${1##*/}" ;;
    esac
}

# Prints why the build environment is refused, one reason per line, and
# succeeds when there is one; fails when there is none. Cargo runs in the
# repository and, for the window, in apps/desktop/src-tauri.
environment_refusals() { # root cargo_home
    local root=$1 home=$2 found=1 name file why
    for name in $(refused_variables); do
        echo "$name is set; a release is built with its own compiler flags and output folder only"
        found=0
    done
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        if why=$(config_refusal "$file"); then
            echo "the Cargo configuration in $(config_place "$file" "$root" "$home") has $why"
            found=0
        fi
    done < <(cargo_configs "$home" "$root" "$root/apps/desktop/src-tauri")
    return "$found"
}

# The perl program that searches a folder for the forbidden paths (perl-base
# on Ubuntu: built-in functions only, no module but strict and warnings).
read -r -d '' path_scanner <<'PERL' || true
use strict;
use warnings;
my $root = shift @ARGV;
defined $root or die "make-release: no folder to check\n";
my @needles;
for my $line (split /\n/, $ENV{WGAL_FORBIDDEN_PATHS} // '') {
    my ($label, $path) = split /\t/, $line, 2;
    die "make-release: a forbidden path is malformed\n"
        unless defined $path && $path =~ m{^/.};
    my $text = $path;
    utf8::decode($text) or die "make-release: a forbidden path is not UTF-8\n";
    my $utf16 = '';
    for my $c (map { ord } split //, $text) {
        if ($c < 0x10000) {
            $utf16 .= pack 'v', $c;
        } else {
            $c -= 0x10000;
            $utf16 .= pack 'vv', 0xD800 + ($c >> 10), 0xDC00 + ($c & 0x3FF);
        }
    }
    push @needles, [$label, 'UTF-8', $path], [$label, 'UTF-16LE', $utf16];
}
@needles or die "make-release: no forbidden paths were given\n";
my ($files, $hits) = (0, 0);
sub scan {
    my ($rel, $path) = @_;
    open(my $fh, '<:raw', $path) or die "make-release: cannot read $rel\n";
    my $data = do { local $/; <$fh> };
    close $fh or die "make-release: cannot read $rel\n";
    $data = '' unless defined $data;
    $files++;
    for my $needle (@needles) {
        my ($label, $encoding, $bytes) = @$needle;
        my ($count, $at) = (0, 0);
        while (($at = index($data, $bytes, $at)) >= 0) {
            $count++;
            $at++;
        }
        next unless $count;
        $hits += $count;
        print "  $rel: the path of the $label, $encoding, $count\n";
    }
}
sub walk {
    my ($rel) = @_;
    my $dir = $rel eq '' ? $root : "$root/$rel";
    opendir(my $dh, $dir) or die "make-release: cannot list a folder of the release\n";
    my @names = sort grep { $_ ne '.' && $_ ne '..' } readdir $dh;
    closedir $dh or die "make-release: cannot list a folder of the release\n";
    for my $name (@names) {
        my $r = $rel eq '' ? $name : "$rel/$name";
        my $p = "$root/$r";
        lstat $p or die "make-release: cannot inspect $r\n";
        if (-l _) {
            die "make-release: $r is a symbolic link; a release holds only files and folders\n";
        } elsif (-d _) {
            walk($r);
        } elsif (-f _) {
            scan($r, $p);
        } else {
            die "make-release: $r is not a file or a folder\n";
        }
    }
}
walk('');
if ($hits) {
    print "make-release: the release names the computer that built it ($hits places); nothing was packed\n";
    exit 1;
}
print "Checked $files files: no build paths\n";
PERL

# Searches every file under the folder for the forbidden paths, given as
# "<label><TAB><path>" lines. Fails on a hit and on any error.
scan_release() { # folder forbidden
    if ! command -v perl >/dev/null 2>&1; then
        echo "make-release.sh: perl is missing; the release cannot be checked, so nothing was packed" >&2
        return 1
    fi
    local rc=0
    WGAL_FORBIDDEN_PATHS=$2 perl -e "$path_scanner" -- "$1" || rc=$?
    case "$rc" in
    0) ;;
    1) return 1 ;;
    *)
        echo "make-release.sh: the release could not be checked (exit $rc); nothing was packed" >&2
        return 1
        ;;
    esac
}

# Adds the path to the forbidden list (the global $forbidden) under the
# label, once.
forbid() { # label path
    case "$forbidden" in
    *"	$2"$'\n'*) return 0 ;;
    esac
    forbidden+="$1	$2"$'\n'
}

refuse() { # message
    echo "make-release.sh: $1" >&2
    exit 65
}

main() {
invoked=$PWD
root=$(cd "$(dirname "$0")/../.." && pwd -P)
build=1
case "${1:-}" in
"") ;;
--no-build) build=0 ;;
*)
    echo "usage: packaging/linux/make-release.sh [--no-build]" >&2
    exit 64
    ;;
esac

[ -n "${HOME:-}" ] || refuse "HOME is not set"
home_dir=$(real_dir "$invoked" "$HOME") || refuse "the home folder does not exist"
cargo_given=${CARGO_HOME:-$HOME/.cargo}
cargo_home=$(real_dir "$invoked" "$cargo_given") || refuse "Cargo's home folder was not found (CARGO_HOME, or ~/.cargo)"
cd "$root"
top=$(git rev-parse --show-toplevel)
[ "$(real_dir / "$top")" = "$root" ] || refuse "the script is not in the repository it runs in"
for path in "$root" "$home_dir" "$cargo_home" "$HOME" "$cargo_given"; do
    if has_control "$path"; then
        refuse "a folder's path has a control character; it cannot be written out of the programs"
    fi
done
forbidden=""
forbid "home folder" "$home_dir"
case "$HOME" in /?*) forbid "home folder" "${HOME%/}" ;; esac
forbid "Cargo home" "$cargo_home"
case "$cargo_given" in /?*) forbid "Cargo home" "${cargo_given%/}" ;; esac
forbid "repository" "$root"

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "make-release.sh: the checkout has uncommitted changes; a release is built from a commit" >&2
    exit 65
fi
commit=$(git rev-parse HEAD)
version=$(awk '/^\[workspace.package\]/{p=1; next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/, "", $0); sub(/version=/, "", $0); print; exit}' Cargo.toml)
arch=$(uname -m)
name="wgal-$version-linux-$arch"
stage="dist/$name"
archive="dist/$name.tar.gz"
rm -rf -- "$stage" "$archive"

programs=(target/release/wgal-watchdog target/release/wgal-desktop target/release/wgal-mcp target/release/wgal)
if [ "$build" -eq 1 ]; then
    if why=$(environment_refusals "$root" "$cargo_home"); then
        while IFS= read -r line; do
            echo "make-release.sh: $line" >&2
        done <<<"$why"
        echo "make-release.sh: nothing was built" >&2
        exit 65
    fi
    export CARGO_HOME="$cargo_home"
    export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$cargo_home=/cargo"$'\x1f'"--remap-path-prefix=$root=/wgal"
    cargo build --release --locked -p wgal-runtime --bin wgal-watchdog
    cargo build --release --locked -p wgal-mcp
    cargo build --release --locked -p wgal-cli
    pnpm --filter @wgal/desktop run tauri build --no-bundle -- --locked
    if [ -n "$(git status --porcelain --untracked-files=no)" ] || [ "$(git rev-parse HEAD)" != "$commit" ]; then
        refuse "the build changed the checkout (Cargo.lock?); nothing was packed"
    fi
    build_record "$commit" "$version" "${programs[@]}" >"$record"
elif ! why=$(check_record "$commit" "$version" "${programs[@]}"); then
    echo "make-release.sh: --no-build: $why; nothing was packed" >&2
    exit 65
fi

mkdir -p dist
work=$(mktemp -d "dist/.$name.XXXXXX")
finished=0
# Removes what this run made unless it finished: the temporary folder,
# and the release folder and archive, deleted above before the build.
cleanup() {
    if [ "$finished" -ne 1 ]; then
        rm -rf -- "$work" "$stage" "$archive"
    fi
}
trap cleanup EXIT
temp="$work/$name"
mkdir "$temp"
for program in wgal-watchdog wgal-desktop wgal-mcp wgal; do
    install -m 0755 "target/release/$program" "$temp/$program"
done
install -m 0755 packaging/linux/install.sh packaging/linux/uninstall.sh packaging/linux/setup.sh "$temp/"
install -m 0644 packaging/linux/wgal-watchdog.service packaging/linux/wgal.desktop \
    packaging/linux/README.md packaging/linux/README.zh-TW.md \
    packaging/INSTALL.md packaging/INSTALL.zh-TW.md "$temp/"
install -m 0644 apps/desktop/src-tauri/icons/128x128.png "$temp/wgal.png"
# Every file but RELEASE.txt itself, hashed before it is written.
hashes=$(cd "$temp" && sha256sum -- * | sort -k 2)
{
    echo "WG Always Live $version for Ubuntu ($arch)"
    echo "Version $version: the window, the watchdog, wgal-mcp and wgal"
    echo "Commit $commit"
    echo "Not signed. Install and remove with the scripts: see INSTALL.md (INSTALL.zh-TW.md in Traditional Chinese)."
    echo
    echo "SHA-256"
    echo "$hashes"
} >"$temp/RELEASE.txt"
scan_release "$temp" "$forbidden" || exit 65
tar -C "$work" -czf "$work/$name.tar.gz" "$name"
mv -- "$temp" "$stage"
mv -- "$work/$name.tar.gz" "$archive"
finished=1
rm -rf -- "$work"
echo "Built $archive"
sha256sum "$archive"
}

# Sourced (by the tests) it only defines the functions.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
