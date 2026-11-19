#!/usr/bin/env bash
# Runs make-release.sh itself in a scratch git checkout of its own, with
# fake programs and the files the script packs. Nothing real is built;
# nothing is written outside the scratch folder.
#
# --no-build (FIX-06, Codex review): it must refuse, and pack nothing,
# without a build record, with a record of another commit and with a
# program changed since the record; with a record of this commit it packs.
# The one version (since 4.2.1) is the workspace's; the package.json here
# has none, as the repository's has none.
#
# A build (4.3.0), with stand-ins for cargo and pnpm on PATH that log where
# they ran, their arguments and the variables they got, and write the
# programs: the compiler flags are exactly the two path rewrites, for
# Cargo's home however it is given and the repository; the window is built
# with --locked; a refused variable or Cargo configuration builds nothing;
# a program that names the home folder, Cargo's home or the repository, in
# UTF-8 or UTF-16LE, packs nothing, and the message does not print the
# path; the same-name release is deleted first; a build that changes
# Cargo.lock packs nothing. The scratch paths have spaces and non-ASCII
# characters.
#
#   bash packaging/linux/tests/make-release.entry.test.sh
set -euo pipefail

source_root=$(cd "$(dirname "$0")/../../.." && pwd -P)
failures=0
check() { # name, then the command that must succeed
    local check_name=$1
    shift
    if "$@"; then
        echo "PASS $check_name"
    else
        echo "FAIL $check_name"
        failures=$((failures + 1))
    fi
}

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
work=$(cd "$work" && pwd -P)
script_rel=packaging/linux/make-release.sh
archive_ext=tar.gz
for name in $(compgen -e); do
    case "$name" in
    CARGO_TARGET_*_RUSTFLAGS) unset "$name" ;;
    esac
done

# The stand-ins for cargo and pnpm.
mkdir -p "$work/bin"
cat >"$work/bin/fake-content.sh" <<'EOF'
# The content of a fake program: its name, and the path FAKE_EMBED asks for
# ("<program>:<home|homegiven|cargo|repo>:<utf8|utf16>").
content() {
    local p which encoding path
    printf 'fake %s\n' "$1"
    IFS=: read -r p which encoding <<<"${FAKE_EMBED:-::}"
    [ "$p" = "$1" ] || return 0
    case "$which" in
    home) path=$(cd "$HOME" && pwd -P) ;;
    homegiven) path=$HOME ;;
    cargo) path=$CARGO_HOME ;;
    repo) path=$(pwd -P) ;;
    esac
    if [ "$encoding" = utf16 ]; then
        printf '%s/src/lib.rs' "$path" | iconv -f UTF-8 -t UTF-16LE
    else
        printf '%s/src/lib.rs' "$path"
    fi
}
log_run() { # tool args...
    local tool=$1
    shift
    {
        echo "$tool cwd=$(pwd -P)"
        echo "$tool args=$*"
        echo "$tool home=${CARGO_HOME-unset}"
        printf '%s flags=%s\n' "$tool" "${CARGO_ENCODED_RUSTFLAGS-unset}" | tr '\037' '|'
    } >>"$FAKE_LOG"
}
EOF
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/fake-content.sh"
log_run cargo "$@"
package=""
while [ $# -gt 0 ]; do
    [ "$1" != -p ] || package=$2
    shift
done
case "$package" in
wgal-runtime) program=wgal-watchdog ;;
wgal-mcp) program=wgal-mcp ;;
wgal-cli) program=wgal ;;
*) exit 2 ;;
esac
mkdir -p target/release
content "$program" >"target/release/$program"
[ -z "${FAKE_TOUCH:-}" ] || echo "# changed by the build" >>"$FAKE_TOUCH"
[ -z "${FAKE_NEWFILE:-}" ] || echo "// made by the build" >"$FAKE_NEWFILE"
EOF
cat >"$work/bin/pnpm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/fake-content.sh"
log_run pnpm "$@"
mkdir -p target/release
content wgal-desktop >target/release/wgal-desktop
EOF
chmod +x "$work/bin/cargo" "$work/bin/pnpm"
# git that fails `git status` (FAKE_GIT_FAIL=status), or only once cargo has
# run (FAKE_GIT_FAIL=after-build); otherwise the real one.
mkdir -p "$work/gitbin" "$work/hashbin"
real_git=$(command -v git)
cat >"$work/gitbin/git" <<EOF
#!/usr/bin/env bash
if [ "\$1" = -c ]; then sub=\$3; else sub=\$1; fi
if [ "\$sub" = status ]; then
    case "\${FAKE_GIT_FAIL:-}" in
    status) exit 128 ;;
    after-build) grep -q '^cargo args' "\$FAKE_LOG" && exit 128 ;;
    esac
fi
exec "$real_git" "\$@"
EOF
# tar without the owner options (FAKE_TAR_OWNER), or adding one more file
# to the archive than the folder holds (FAKE_TAR_EXTRA, its content).
mkdir -p "$work/tarbin"
real_tar=$(command -v tar)
cat >"$work/tarbin/tar" <<EOF
#!/usr/bin/env bash
args=()
create=""
dir=""
prev=""
for a in "\$@"; do
    [ "\$prev" != -C ] || dir=\$a
    prev=\$a
    case "\$a" in
    --owner=* | --group=* | --numeric-owner) [ -z "\${FAKE_TAR_OWNER:-}" ] || continue ;;
    -czf) create=1 ;;
    esac
    args+=("\$a")
done
if [ -n "\$create" ] && [ -n "\${FAKE_TAR_EXTRA:-}" ]; then
    last=\${!#}
    printf '%s' "\$FAKE_TAR_EXTRA" >"\$dir/\$last/extra"
    rc=0
    "$real_tar" "\${args[@]}" || rc=\$?
    rm -f -- "\$dir/\$last/extra"
    exit "\$rc"
fi
if [ -n "\$create" ] && [ -n "\${FAKE_TAR_EXTRA_ROOT:-}" ]; then
    printf '%s' "\$FAKE_TAR_EXTRA_ROOT" >"\$dir/extra-root.txt"
    exec "$real_tar" "\${args[@]}" extra-root.txt
fi
exec "$real_tar" "\${args[@]}"
EOF
chmod +x "$work/tarbin/tar"
# The hash tool, failing for the archive itself when FAKE_HASH_FAIL is set.
real_hash=$(command -v sha256sum)
cat >"$work/hashbin/sha256sum" <<EOF
#!/usr/bin/env bash
# FAKE_HASH_BAD: a file whose hash is printed right, then failed.
for a in "\$@"; do
    if [ -n "\${FAKE_HASH_BAD:-}" ] && [ "\$a" = "\$FAKE_HASH_BAD" ]; then
        "$real_hash" "\$@"
        exit 1
    fi
done
if [ -n "\${FAKE_HASH_FAIL:-}" ]; then
    for a in "\$@"; do case "\$a" in *.tar.gz) exit 1 ;; esac; done
fi
exec "$real_hash" "\$@"
EOF
chmod +x "$work/gitbin/git" "$work/hashbin/sha256sum"

home="$work/home ä"
mkdir -p "$home/.cargo"
repo="$work/re po ä/repo"
mkdir -p "$repo/packaging/linux" "$repo/apps/desktop/src-tauri/icons" "$repo/target/release"
cp "$source_root/packaging/linux/make-release.sh" "$repo/packaging/linux/"
for file in install.sh uninstall.sh setup.sh wgal-watchdog.service wgal.desktop README.md README.zh-TW.md; do
    echo "fake $file" >"$repo/packaging/linux/$file"
done
echo "fake guide" >"$repo/packaging/INSTALL.md"
echo "fake zh-TW guide" >"$repo/packaging/INSTALL.zh-TW.md"
echo "fake icon" >"$repo/apps/desktop/src-tauri/icons/128x128.png"
printf '[workspace.package]\nversion = "9.8.7"\n' >"$repo/Cargo.toml"
echo "# fake lock file" >"$repo/Cargo.lock"
echo '{ "name": "@wgal/desktop", "private": true }' >"$repo/apps/desktop/package.json"
# The tests' Cargo configurations, in the checkout, are ignored by git.
printf 'target/\ndist/\n.cargo/\n' >"$repo/.gitignore"
cd "$repo"
git init -q .
git -c user.name=test -c user.email=test@example.invalid add -A
git -c user.name=test -c user.email=test@example.invalid commit -q -m scratch
head=$(git rev-parse HEAD)
arch=$(uname -m)
name="wgal-9.8.7-linux-$arch"
programs=(target/release/wgal-watchdog target/release/wgal-desktop target/release/wgal-mcp target/release/wgal)
for program in "${programs[@]}"; do
    echo "fake $program" >"$program"
done
record() { # commit [version]
    {
        echo "commit $1"
        echo "version ${2:-9.8.7}"
        echo "paths rewritten as /cargo and /wgal"
        sha256sum -- "${programs[@]}"
    } >target/release/wgal-build.txt
}
out=""
code=0
log="$work/fake.log"
# Runs the script: [VAR=value...] [--no-build]. Every refused variable and
# CARGO_HOME are cleared first; HOME is the scratch home.
run() {
    local vars=()
    while [ $# -gt 0 ] && [[ "$1" == *=* ]]; do
        vars+=("$1")
        shift
    done
    code=0
    : >"$log"
    out=$(env -u CARGO_HOME -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_BUILD_RUSTFLAGS \
        -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET_DIR -u CARGO_BUILD_TARGET \
        HOME="$home" PATH="$work/bin:$PATH" FAKE_LOG="$log" ${vars[@]+"${vars[@]}"} \
        bash packaging/linux/make-release.sh "$@" 2>&1) || code=$?
}
refused_with() { [ "$code" -eq 65 ] && [[ "$out" == *"$1"* ]]; }
nothing_packed() {
    [ ! -e "dist/$name" ] && [ ! -e "dist/$name.tar.gz" ] && [ -z "$(find dist -maxdepth 1 -name '.wgal-*' 2>/dev/null)" ]
}
nothing_built() { ! grep -q . "$log"; }
no_path_printed() { [[ "$out" != *"$work"* ]]; }
files_in() { find "$1" -type f | wc -l | tr -d ' '; }

run --no-build
check "no record: refused" refused_with "no build record"
check "no record: nothing packed" nothing_packed

record fedcba9876543210fedcba9876543210fedcba98
run --no-build
check "another commit: refused" refused_with "another commit"
check "another commit: nothing packed" nothing_packed

record "$head"
echo "another build of the same version" >target/release/wgal-mcp
run --no-build
check "a changed program: refused" refused_with "changed"
check "a changed program: nothing packed" nothing_packed

# Only the command line replaced, the commit and versions the same.
record "$head"
echo "another wgal" >target/release/wgal
run --no-build
check "only the command line changed: refused" refused_with "changed"
check "only the command line changed: nothing packed" nothing_packed
echo "fake target/release/wgal" >target/release/wgal

# A record of a build without the command line (before 3.0.0).
record "$head"
grep -v 'target/release/wgal$' target/release/wgal-build.txt >"$work/older"
cp "$work/older" target/release/wgal-build.txt
run --no-build
check "a record without the command line: refused" refused_with "changed"
check "a record without the command line: nothing packed" nothing_packed

# A record this script did not write: what it says is not repeated.
record "$work/secret"
run --no-build
check "a record this script did not write: refused" refused_with "is not one this script wrote"
check "a record this script did not write: the path is not printed" no_path_printed

# The same commit and programs, built as another version.
record "$head" 9.8.6
run --no-build
check "another version: refused" refused_with "version differs"
check "another version: nothing packed" nothing_packed

record "$head"
run --no-build
check "this commit: packed" test "$code" -eq 0 -a -f "dist/$name.tar.gz"
check "this commit: RELEASE.txt names it" grep -q "Commit $head" "dist/$name/RELEASE.txt"
check "this commit: the command line is packed" test -x "dist/$name/wgal"
check "this commit: RELEASE.txt has its sum" grep -q "  wgal$" "dist/$name/RELEASE.txt"
for doc in README.md README.zh-TW.md INSTALL.md INSTALL.zh-TW.md; do
    check "this commit: $doc is packed" test -f "dist/$name/$doc"
    check "this commit: RELEASE.txt has the sum of $doc" grep -q "  $doc$" "dist/$name/RELEASE.txt"
done
check "this commit: RELEASE.txt opens with the one version" test "$(head -2 "dist/$name/RELEASE.txt")" = "WG Always Live 9.8.7 for Ubuntu ($arch)
Version 9.8.7: the window, the watchdog, wgal-mcp and wgal"
check "this commit: RELEASE.txt points to the English guide" grep -q "see INSTALL.md" "dist/$name/RELEASE.txt"
check "this commit: every packed file was checked, RELEASE.txt too" \
    grep -qxF "Checked $(files_in "dist/$name") files: no build paths" <<<"$out"
check "this commit: no temporary folder is left" test -z "$(find dist -maxdepth 1 -name '.wgal-*')"

# --no-build checks the programs too.
printf '%s' "$repo" >>target/release/wgal-mcp
record "$head"
run --no-build
check "--no-build with the repository's path: refused" refused_with "  wgal-mcp: the path of the repository, UTF-8, 1"
check "--no-build with the repository's path: nothing packed" nothing_packed
check "--no-build with the repository's path: the path is not printed" no_path_printed

# A build with the stand-ins.
cargo_home=$(cd "$home/.cargo" && pwd -P)
flags() { # cargo_home -> the flags every build must get, 0x1f shown as |
    echo "--remap-path-prefix=$1=/cargo|--remap-path-prefix=$repo=/wgal"
}
logged() { grep -qxF -- "$1" "$log"; }
times_logged() { grep -cxF -- "$1" "$log" || true; }
run
check "a build: packed" test "$code" -eq 0 -a -f "dist/$name.tar.gz"
for package in "wgal-runtime --bin wgal-watchdog" wgal-mcp wgal-cli; do
    check "a build: cargo builds $package locked" logged "cargo args=build --release --locked -p $package"
done
check "a build: cargo runs in the repository" test "$(times_logged "cargo cwd=$repo")" -eq 3
check "a build: the window is built locked" logged "pnpm args=--filter @wgal/desktop run tauri build --no-bundle -- --locked"
check "a build: cargo gets exactly the two rewrites" test "$(times_logged "cargo flags=$(flags "$cargo_home")")" -eq 3
check "a build: the window's build gets them" logged "pnpm flags=$(flags "$cargo_home")"
check "a build: CARGO_HOME is the resolved ~/.cargo" test "$(times_logged "cargo home=$cargo_home")" -eq 3
check "a build: the window's build gets that CARGO_HOME" logged "pnpm home=$cargo_home"
check "a build: every packed file was checked" \
    grep -qxF "Checked $(files_in "dist/$name") files: no build paths" <<<"$out"
check "a build: no temporary folder is left" test -z "$(find dist -maxdepth 1 -name '.wgal-*')"
check "a build: the record names this commit" grep -qx "commit $head" target/release/wgal-build.txt

# CARGO_HOME outside the home folder, and relative to where the script was
# started (not to the repository, where it does not exist).
other="$work/other cargo 𝄞"
mkdir -p "$other" "$work/elsewhere/cargo rel"
run "CARGO_HOME=$other"
check "CARGO_HOME elsewhere: packed" test "$code" -eq 0
check "CARGO_HOME elsewhere: the rewrite names it" test "$(times_logged "cargo flags=$(flags "$other")")" -eq 3
check "CARGO_HOME elsewhere: cargo gets it" test "$(times_logged "cargo home=$other")" -eq 3
code=0
: >"$log"
out=$(cd "$work/elsewhere" && env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_TARGET_DIR \
    HOME="$home" PATH="$work/bin:$PATH" FAKE_LOG="$log" CARGO_HOME="cargo rel" \
    bash "$repo/packaging/linux/make-release.sh" 2>&1) || code=$?
check "a relative CARGO_HOME: packed" test "$code" -eq 0
check "a relative CARGO_HOME: taken from where the script started" \
    test "$(times_logged "cargo flags=$(flags "$work/elsewhere/cargo rel")")" -eq 3
check "a relative CARGO_HOME: cargo gets it absolute" test "$(times_logged "cargo home=$work/elsewhere/cargo rel")" -eq 3
check "before a refusal: the release is there" test -f "dist/$name.tar.gz"
run "CARGO_HOME=$work/missing"
check "a missing Cargo home: refused" refused_with "Cargo's home folder was not found"
check "a missing Cargo home: nothing built" nothing_built
check "a missing Cargo home: the same-name release was deleted" nothing_packed

# dist/ as a link is refused before anything is deleted.
mv dist "$work/dist.saved"
mkdir -p "$work/outside/$name"
echo keep >"$work/outside/$name/sentinel"
echo keep >"$work/outside/$name.tar.gz"
ln -s "$work/outside" dist
run
check "dist/ as a link: refused" refused_with "dist/ is a link or not a folder"
check "dist/ as a link: nothing built" nothing_built
check "dist/ as a link: the folder it points to is untouched" test -f "$work/outside/$name/sentinel" -a -f "$work/outside/$name.tar.gz"
rm dist
mv "$work/dist.saved" dist

# A version that is not a plain name is refused before anything is deleted.
cp Cargo.toml "$work/Cargo.toml.saved"
printf '[workspace.package]\nversion = "9.8.7/../../x"\n' >Cargo.toml
run
check "a version that is not a plain name: refused" refused_with "not a plain name"
check "a version that is not a plain name: nothing built" nothing_built
cp "$work/Cargo.toml.saved" Cargo.toml

# git failing is never taken for a clean checkout.
run "PATH=$work/gitbin:$work/bin:$PATH" FAKE_GIT_FAIL=status
check "git status failing: refused" refused_with "git could not read the checkout"
check "git status failing: nothing built" nothing_built
check "git status failing: nothing packed" nothing_packed
run "PATH=$work/gitbin:$work/bin:$PATH" FAKE_GIT_FAIL=after-build
check "git status failing after the build: refused" refused_with "git could not read the checkout"
check "git status failing after the build: nothing packed" nothing_packed

# A file the build adds to the checkout packs nothing.
run "FAKE_NEWFILE=$repo/made-by-the-build.rs"
check "a new file from the build: refused" refused_with "the build changed the checkout"
check "a new file from the build: nothing packed" nothing_packed
rm -f "$repo/made-by-the-build.rs"

# A file that is not in git, there before the build: refused, as the
# checkout is not clean.
echo "notes" >"$repo/notes.txt"
run
check "an untracked file before the build: refused" refused_with "untracked files"
check "an untracked file before the build: nothing built" nothing_built
check "an untracked file before the build: nothing packed" nothing_packed
rm -f "$repo/notes.txt"

# --- The archive (Ubuntu): no owner names, exactly the release's files.
run
check "the archive names no owner" test "$(tar -tvzf "dist/$name.tar.gz" | awk '{print $2}' | LC_ALL=C sort -u)" = "0/0"
check "the archive is checked again after packing" test "$(grep -c '^Checked [0-9]* files: no build paths$' <<<"$out")" -eq 2
run "PATH=$work/tarbin:$work/bin:$PATH" FAKE_TAR_OWNER=1
check "an archive with owner names: refused" refused_with "the archive names an owner"
check "an archive with owner names: nothing packed" nothing_packed
run "PATH=$work/tarbin:$work/bin:$PATH" "FAKE_TAR_EXTRA=$repo"
check "an archive with another file: refused" refused_with "the archive holds other files than the release"
check "an archive with another file: nothing packed" nothing_packed
check "an archive with another file: the path is not printed" no_path_printed
run "PATH=$work/tarbin:$work/bin:$PATH" "FAKE_TAR_EXTRA_ROOT=$repo"
check "an archive with a file beside the release's folder: refused" refused_with "the archive holds other files than the release"
check "an archive with a file beside the release's folder: nothing packed" nothing_packed
check "an archive with a file beside the release's folder: the path is not printed" no_path_printed
# --- End of the archive checks.

# A hash that fails, after printing the right sum, packs nothing.
run "PATH=$work/hashbin:$work/bin:$PATH" FAKE_HASH_BAD=target/release/wgal-watchdog
check "a failing hash in a build: refused" test "$code" -ne 0
check "a failing hash in a build: nothing packed" nothing_packed
run
record "$head"
run "PATH=$work/hashbin:$work/bin:$PATH" FAKE_HASH_BAD=target/release/wgal-watchdog --no-build
check "a failing hash with --no-build: refused" refused_with "the programs could not be hashed"
check "a failing hash with --no-build: nothing packed" nothing_packed

# A repository folder whose name ends with a newline, beside one without:
# refused before anything is deleted, in either.
twin="$work/twin"
nl_repo="$twin"$'\n'
mkdir -p "$twin/dist" "$nl_repo/${script_rel%/*}"
cp "$repo/$script_rel" "$nl_repo/$script_rel"
printf '[workspace.package]\nversion = "9.8.7"\n' >"$twin/Cargo.toml"
echo keep >"$twin/dist/$name.$archive_ext"
code=0
out=$(cd "$work" && env -u CARGO_HOME HOME="$home" PATH="$work/bin:$PATH" FAKE_LOG="$log" \
    bash "$nl_repo/$script_rel" 2>&1) || code=$?
check "a repository name ending with a newline: refused" refused_with "control character"
check "a repository name ending with a newline: the other repository's release stays" test -f "$twin/dist/$name.$archive_ext"

# The last step failing leaves nothing either.
run "PATH=$work/hashbin:$work/bin:$PATH" FAKE_HASH_FAIL=1
check "the archive's hash failing: refused" test "$code" -ne 0
check "the archive's hash failing: nothing packed" nothing_packed

# Refused variables build nothing, and the same-name release goes first;
# other files in dist/ stay.
mkdir -p "dist/$name" "dist/wgal-9.8.6-linux-$arch"
echo old >"dist/$name/old"
echo old >"dist/$name.tar.gz"
echo keep >dist/keep.txt
for variable in RUSTFLAGS=-Cdebuginfo=2 CARGO_ENCODED_RUSTFLAGS= CARGO_BUILD_RUSTFLAGS=-Cx \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS=-Cx CARGO_TARGET_DIR=/x CARGO_BUILD_TARGET_DIR=/x \
    CARGO_BUILD_TARGET=x86_64-unknown-linux-musl; do
    run "$variable"
    check "${variable%%=*}: refused" refused_with "${variable%%=*} is set"
    check "${variable%%=*}: nothing built" nothing_built
    check "${variable%%=*}: nothing packed" nothing_packed
done
check "the old same-name release was deleted" test ! -e "dist/$name/old"
check "other files in dist/ stay" test -f dist/keep.txt -a -d "dist/wgal-9.8.6-linux-$arch"

# Refused Cargo configurations, in each folder cargo runs in or above it,
# and in Cargo's home.
for place in "$repo/.cargo/config.toml" "$repo/apps/desktop/src-tauri/.cargo/config.toml" \
    "$repo/apps/.cargo/config" "$work/re po ä/.cargo/config.toml" "$home/.cargo/config.toml"; do
    mkdir -p "$(dirname "$place")"
    printf '[build]\ntarget-dir = "elsewhere"\n' >"$place"
    run
    check "a configuration in ${place#"$work"/}: refused" refused_with "has target-dir"
    check "a configuration in ${place#"$work"/}: nothing built" nothing_built
    check "a configuration in ${place#"$work"/}: nothing packed" nothing_packed
    check "a configuration in ${place#"$work"/}: the path is not printed" no_path_printed
    rm -f "$place"
done
mkdir -p "$repo/.cargo"
printf '[net]\nretry = 3\n' >"$repo/.cargo/config.toml"
run
check "a harmless configuration: packed" test "$code" -eq 0
rm -f "$repo/.cargo/config.toml"

# A program that names a forbidden folder, in either encoding, packs
# nothing; the message names the file, the folder and the encoding only.
for program in wgal-watchdog wgal-mcp wgal wgal-desktop; do
    for which in home cargo repo; do
        case "$which" in
        home) label="home folder" ;;
        cargo) label="Cargo home" ;;
        repo) label="repository" ;;
        esac
        for encoding in utf8 utf16; do
            shown=UTF-8
            [ "$encoding" = utf8 ] || shown=UTF-16LE
            run "FAKE_EMBED=$program:$which:$encoding"
            check "$program with the $label path in $shown: refused" \
                refused_with "  $program: the path of the $label, $shown, 1"
            check "$program with the $label path in $shown: nothing packed" nothing_packed
            check "$program with the $label path in $shown: the path is not printed" no_path_printed
        done
    done
done

# The home folder as it was given, when it is a link to another folder.
ln -s "home ä" "$work/home link"
code=0
: >"$log"
out=$(env -u CARGO_HOME -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_TARGET_DIR HOME="$work/home link" \
    PATH="$work/bin:$PATH" FAKE_LOG="$log" FAKE_EMBED=wgal:homegiven:utf8 bash packaging/linux/make-release.sh 2>&1) || code=$?
check "the home folder's path as given: refused" refused_with "  wgal: the path of the home folder, UTF-8, 1"
check "the home folder's path as given: nothing packed" nothing_packed

# A build that changes Cargo.lock packs nothing.
run "FAKE_TOUCH=$repo/Cargo.lock"
check "a changed Cargo.lock: refused" refused_with "the build changed the checkout"
check "a changed Cargo.lock: nothing packed" nothing_packed
git checkout -q -- Cargo.lock

if [ "$failures" -gt 0 ]; then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
