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

# 4.3.0: the build environment. Every variable that would add compiler
# flags or move Cargo's output is refused once exported, and only those.
for name in $(refused_variables); do
    unset "$name"
done
check "no refused variable is left here" test -z "$(refused_variables)"
for name in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS \
    CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_TARGET; do
    check "$name is refused, even empty" test "$(export "$name="; refused_variables)" = "$name"
done
check "other Cargo variables are not refused" test -z "$(export CARGO_HOME=/x CARGO_TERM_COLOR=always RUSTDOCFLAGS=-x CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=cc; refused_variables)"
# shellcheck disable=SC2034 # set without export, on purpose
check "a variable that is not exported does not reach cargo" test -z "$(RUSTFLAGS=x; refused_variables)"

# Cargo's configuration files: in the folders cargo runs in, every folder
# above them, and Cargo's home.
cfg="$work/cfg dir ä"
crepo="$cfg/above/repo"
chome="$cfg/cargo home"
mkdir -p "$crepo/apps/desktop/src-tauri/.cargo" "$crepo/apps/desktop/.cargo" "$crepo/apps/.cargo" \
    "$crepo/.cargo" "$cfg/above/.cargo" "$chome"
for file in "$crepo/apps/desktop/src-tauri/.cargo/config.toml" "$crepo/apps/desktop/.cargo/config" \
    "$crepo/apps/.cargo/config.toml" "$crepo/.cargo/config" "$cfg/above/.cargo/config.toml" \
    "$chome/config.toml" "$chome/config"; do
    echo '[net]' >"$file"
done
listed=$(cargo_configs "$chome" "$crepo" "$crepo/apps/desktop/src-tauri")
for file in "$crepo/apps/desktop/src-tauri/.cargo/config.toml" "$crepo/apps/desktop/.cargo/config" \
    "$crepo/apps/.cargo/config.toml" "$crepo/.cargo/config" "$cfg/above/.cargo/config.toml" \
    "$chome/config.toml" "$chome/config"; do
    check "the configuration ${file#"$cfg"/} is looked at" grep -qxF -- "$file" <<<"$listed"
done
check "each configuration is looked at once" test "$(sort <<<"$listed" | uniq -d)" = ""
check "harmless configurations are not refused" test -z "$(environment_refusals "$crepo" "$chome")"

has_refusal() { # expected file content...
    local expected=$1 file="$work/one-config.toml" got
    shift
    printf '%s\n' "$@" >"$file"
    got=$(config_refusal "$file") && [ "$got" = "$expected" ]
}
not_refused() { # content...
    local file="$work/one-config.toml"
    printf '%s\n' "$@" >"$file"
    ! config_refusal "$file" >/dev/null
}
check "build.rustflags is refused" has_refusal rustflags '[build]' 'rustflags = ["-Cdebuginfo=2"]'
check "target.<triple>.rustflags is refused" has_refusal rustflags '[target.aarch64-apple-darwin]' 'rustflags = []'
check "a dotted build.rustflags is refused" has_refusal rustflags 'build.rustflags = "-Cx"'
check "an inline rustflags is refused" has_refusal rustflags 'build = { rustflags = [] }'
check "rustflags in a comment is refused" has_refusal rustflags '# rustflags are set elsewhere'
check "target-dir is refused" has_refusal target-dir '[build]' 'target-dir = "out"'
check "a dotted target-dir is refused" has_refusal target-dir 'build.target-dir = "out"'
check "build.target is refused" has_refusal "a key named target" '[build]' 'target = "x86_64-apple-darwin"'
check "a dotted build.target is refused" has_refusal "a key named target" 'build.target="x86_64-apple-darwin"'
check "an inline build.target is refused" has_refusal "a key named target" 'build = { target = "x" }'
check "a quoted target key is refused" has_refusal "a key named target" '[build]' '"target" = "x"'
check "linker settings are not refused" not_refused '[target.aarch64-apple-darwin]' 'linker = "cc"' \
    "[target.'cfg(target_os = \"macos\")']" 'runner = "x"' '[net]' 'retry = 3'
mkdir -p "$work/a-folder.toml"
check "a configuration that is a folder is refused" test "$(config_refusal "$work/a-folder.toml")" = "it is not a readable file"
if [ "$(id -u)" -ne 0 ]; then
    echo '[net]' >"$work/unreadable.toml"
    chmod 000 "$work/unreadable.toml"
    check "an unreadable configuration is refused" test "$(config_refusal "$work/unreadable.toml")" = "it is not a readable file"
    chmod 600 "$work/unreadable.toml"
fi

refusals_name() { # file content -> the refusal, which must not name the account's folders
    local out
    printf '%s\n' "$2" >"$1"
    out=$(environment_refusals "$crepo" "$chome") || return 1
    echo '[net]' >"$1"
    [[ "$out" != *"$cfg"* ]] && echo "$out"
}
check "a refused configuration where the window is built" test \
    "$(refusals_name "$crepo/apps/desktop/src-tauri/.cargo/config.toml" 'target-dir = "x"')" = \
    "the Cargo configuration in apps/desktop/src-tauri/.cargo/config.toml has target-dir"
check "a refused configuration in Cargo's home" test \
    "$(refusals_name "$chome/config" 'rustflags = []')" = "the Cargo configuration in Cargo's home: config has rustflags"
check "a refused configuration above the repository" test \
    "$(refusals_name "$cfg/above/.cargo/config.toml" 'build.target = "x"')" = \
    "the Cargo configuration in a folder above the repository: .cargo/config.toml has a key named target"
check "a refused variable is named" test "$(export CARGO_TARGET_DIR=/x; environment_refusals "$crepo" "$chome")" = \
    "CARGO_TARGET_DIR is set; a release is built with its own compiler flags and output folder only"

not() { ! "$@"; }

# Paths: absolute, without symbolic links.
mkdir -p "$work/real one"
ln -s "real one" "$work/link"
check "a link is resolved" test "$(real_dir "$work" link)" = "$(cd "$work/real one" && pwd -P)"
check "a relative path is taken from the given folder" test "$(real_dir "$work/real one" ..)" = "$(cd "$work" && pwd -P)"
check "a missing folder fails" not real_dir / "$work/missing"
check "a control character is found" has_control "$(printf 'a\037b')"
check "a newline is found" has_control "a
b"
check "plain text has none" not has_control "/home/a b/ä"
forbidden=""
forbid "home folder" "/x/y"
forbid "Cargo home" "/x/y"
forbid "repository" "/x/y z"
check "a path is forbidden once" test "$forbidden" = "home folder	/x/y
repository	/x/y z
"

# The scan before packing: every file, UTF-8 and UTF-16LE, and the message
# never names the path.
s_home="$work/scan home ä"
s_cargo="$work/scan cargo 𝄞"
s_repo="$work/scan repo"
list="home folder	$s_home
Cargo home	$s_cargo
repository	$s_repo
"
release_like() { # folder: a clean release-like folder
    rm -rf -- "$1"
    mkdir -p "$1/sub"
    printf 'program\0bytes\n' >"$1/wgal"
    echo "plain" >"$1/RELEASE.txt"
    echo "nested" >"$1/sub/file"
}
utf16() { printf '%s' "$1" | iconv -f UTF-8 -t UTF-16LE; }
refused_scan() { ! scan_release "$@" >/dev/null 2>&1; }
refused_without_perl() { ! PATH="$work/no-perl" scan_release "$@" >/dev/null 2>&1; }
scan_out=""
scanned() { # folder: succeeds when the scan passes; keeps its output
    local rc=0
    scan_out=$(scan_release "$1" "$list" 2>&1) || rc=$?
    return "$rc"
}
release_like "$work/rel"
check "a clean release passes" scanned "$work/rel"
check "a clean release: every file counted" test "$scan_out" = "Checked 3 files: no build paths"
for which in home cargo repo; do
    case "$which" in
    home) path=$s_home label="home folder" ;;
    cargo) path=$s_cargo label="Cargo home" ;;
    repo) path=$s_repo label="repository" ;;
    esac
    for encoding in UTF-8 UTF-16LE; do
        release_like "$work/rel"
        {
            printf 'before'
            if [ "$encoding" = UTF-8 ]; then printf '%s' "$path/x.rs"; else utf16 "$path/x.rs"; fi
            printf 'after'
        } >"$work/rel/wgal"
        check "the $label path in $encoding is refused" refused_scan "$work/rel" "$list"
        scanned "$work/rel" || true
        check "the $label path in $encoding: the file, folder and encoding are named" \
            grep -qxF "  wgal: the path of the $label, $encoding, 1" <<<"$scan_out"
        check "the $label path in $encoding: the path is not printed" test "${scan_out/"$work"/}" = "$scan_out"
    done
done
release_like "$work/rel"
printf 'Commit 1\n%s\n' "$s_repo" >"$work/rel/RELEASE.txt"
check "a path in RELEASE.txt is refused" refused_scan "$work/rel" "$list"
scanned "$work/rel" || true
check "a path in RELEASE.txt: named" grep -qxF "  RELEASE.txt: the path of the repository, UTF-8, 1" <<<"$scan_out"
release_like "$work/rel"
printf '%s%s' "$s_home" "$s_home" >"$work/rel/sub/file"
scanned "$work/rel" || true
check "a path in a nested file is counted" grep -qxF "  sub/file: the path of the home folder, UTF-8, 2" <<<"$scan_out"
release_like "$work/rel"
ln -s wgal "$work/rel/link"
check "a symbolic link is refused" refused_scan "$work/rel" "$list"
release_like "$work/rel"
mkfifo "$work/rel/fifo"
check "a FIFO is refused" refused_scan "$work/rel" "$list"
if [ "$(id -u)" -ne 0 ]; then
    release_like "$work/rel"
    chmod 000 "$work/rel/wgal"
    check "an unreadable file is refused" refused_scan "$work/rel" "$list"
    chmod 600 "$work/rel/wgal"
fi
release_like "$work/rel"
check "no forbidden paths is refused" refused_scan "$work/rel" ""
check "the root as a forbidden path is refused" refused_scan "$work/rel" "home folder	/"
mkdir -p "$work/no-perl"
check "without perl nothing passes" refused_without_perl "$work/rel" "$list"

if [ "$failures" -gt 0 ]; then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
