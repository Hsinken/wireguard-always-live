#Requires -Version 5.1
# Runs make-release.ps1 itself in a scratch git checkout of its own, with
# fake programs and the files the script packs. Nothing real is built;
# nothing is written outside the scratch folder.
#
# -NoBuild (FIX-06, Codex review): it must refuse, and pack nothing,
# without a build record, with a record of another commit and with a
# program changed since the record; with a record of this commit it packs.
#
# A build (4.3.0), with stand-ins for cargo and pnpm (.ps1 files first on
# PATH) that log where they ran, their arguments and the variables they
# got, and write the programs: the compiler flags are exactly the two path
# rewrites, for Cargo's home however it is given and the repository; the
# window is built with --locked; a refused variable or Cargo configuration
# builds nothing; a program that names the profile folder, Cargo's home or
# the repository, in UTF-8 or UTF-16LE, in either case, with / or with 8.3
# short names, zips nothing, and the message does not print the path; a
# link or junction on the way is refused; the same-name release is deleted
# first; a build that changes Cargo.lock zips nothing; CARGO_HOME and
# CARGO_ENCODED_RUSTFLAGS are as they were after a run, built or refused.
# The scratch paths have spaces and non-ASCII characters; USERPROFILE is a
# scratch folder.
#
#   powershell.exe -NoProfile -File packaging\windows\tests\make-release.entry.test.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$failures = 0
function Check([string] $Name, [bool] $Ok) {
    if ($Ok) { Write-Host "PASS $Name" } else { Write-Host "FAIL $Name"; $script:failures++ }
}

$a = [string][char]0xE4
$top = Join-Path ([IO.Path]::GetTempPath()) ("wgal-make-release-entry-" + [Guid]::NewGuid().ToString('N'))
$work = Join-Path $top "re po $a\repo"
$bin = Join-Path $top 'bin'
$profileDir = Join-Path $top "profile $a"
$log = Join-Path $top 'fake.log'
$junctions = @()
New-Item -ItemType Directory $work, $bin, (Join-Path $profileDir '.cargo') | Out-Null
$utf8 = New-Object Text.UTF8Encoding $false

# The stand-ins for cargo and pnpm.
[IO.File]::WriteAllText((Join-Path $bin 'fake-content.ps1'), @'
# The content of a fake program: its name, and the path FAKE_EMBED asks for
# ("<program>,<profile|cargo|repo|repoupper|reposlash|reposhort|repomixed>,<utf8|utf16>").
function Get-FakeBytes([string] $Program) {
    $utf8 = New-Object Text.UTF8Encoding $false
    $bytes = New-Object 'System.Collections.Generic.List[byte]'
    $bytes.AddRange($utf8.GetBytes("fake $Program`n"))
    $spec = "$env:FAKE_EMBED".Split(',')
    if ($spec.Count -eq 3 -and $spec[0] -eq $Program) {
        $here = (Get-Location).ProviderPath
        $fso = New-Object -ComObject Scripting.FileSystemObject
        $path = switch ($spec[1]) {
            'profile' { $env:USERPROFILE }
            'cargo' { $env:CARGO_HOME }
            'repo' { $here }
            'repoupper' { [regex]::Replace($here, '[a-z]', { param($m) $m.Value.ToUpperInvariant() }) }
            'reposlash' { $here.Replace('\', '/') }
            'reposhort' { $fso.GetFolder($here).ShortPath }
            'repomixed' { Join-Path (Join-Path (Split-Path (Split-Path $here)) $fso.GetFolder((Split-Path $here)).ShortName) (Split-Path $here -Leaf) }
        }
        $embedded = "$path\src\lib.rs"
        if ($spec[2] -eq 'utf16') {
            $bytes.AddRange([Text.Encoding]::Unicode.GetBytes($embedded))
        } else {
            $bytes.AddRange($utf8.GetBytes($embedded))
        }
    }
    return , $bytes.ToArray()
}
function Write-FakeLog([string] $Tool, [object[]] $Arguments) {
    $flags = "$env:CARGO_ENCODED_RUSTFLAGS".Replace([string][char]0x1f, '|')
    $lines = @("$Tool cwd=$((Get-Location).ProviderPath)", "$Tool args=$($Arguments -join ' ')", "$Tool home=$env:CARGO_HOME", "$Tool flags=$flags")
    [IO.File]::AppendAllText($env:FAKE_LOG, ($lines -join "`n") + "`n", (New-Object Text.UTF8Encoding $false))
}
'@)
[IO.File]::WriteAllText((Join-Path $bin 'cargo.ps1'), @'
. (Join-Path $PSScriptRoot 'fake-content.ps1')
Write-FakeLog 'cargo' $args
$package = $null
for ($i = 0; $i -lt $args.Count - 1; $i++) {
    if ($args[$i] -eq '-p') { $package = $args[$i + 1] }
}
if (-not $package) { exit 2 }
$program = @{ 'wgal-runtime' = 'wgal-watchdog.exe'; 'wgal-mcp' = 'wgal-mcp.exe'; 'wgal-cli' = 'wgal.exe' }[$package]
$out = Join-Path (Get-Location).ProviderPath 'target\release'
New-Item -ItemType Directory -Force $out | Out-Null
[IO.File]::WriteAllBytes((Join-Path $out $program), (Get-FakeBytes $program))
if ($env:FAKE_TOUCH) { [IO.File]::AppendAllText($env:FAKE_TOUCH, "# changed by the build`n") }
if ($env:FAKE_NEWFILE) { [IO.File]::WriteAllText($env:FAKE_NEWFILE, "// made by the build`n") }
exit 0
'@)
[IO.File]::WriteAllText((Join-Path $bin 'pnpm.ps1'), @'
. (Join-Path $PSScriptRoot 'fake-content.ps1')
Write-FakeLog 'pnpm' $args
$out = Join-Path (Get-Location).ProviderPath 'target\release'
New-Item -ItemType Directory -Force $out | Out-Null
[IO.File]::WriteAllBytes((Join-Path $out 'wgal-desktop.exe'), (Get-FakeBytes 'wgal-desktop.exe'))
exit 0
'@)

# git that fails `git status` (FAKE_GIT_FAIL=status), or only once cargo
# has run (FAKE_GIT_FAIL=after-build); otherwise the real one.
$gitbin = Join-Path $top 'gitbin'
New-Item -ItemType Directory $gitbin | Out-Null
$realGit = (Get-Command git -CommandType Application | Select-Object -First 1).Source
[IO.File]::WriteAllText((Join-Path $gitbin 'git.ps1'), @'
$sub = if ($args.Count -gt 2 -and $args[0] -eq '-c') { $args[2] } else { $args[0] }
if ($sub -eq 'status') {
    if ($env:FAKE_GIT_FAIL -eq 'status') { exit 128 }
    if ($env:FAKE_GIT_FAIL -eq 'after-build' -and (Select-String -LiteralPath $env:FAKE_LOG -Pattern '^cargo args' -Quiet)) { exit 128 }
}
& $env:FAKE_REAL_GIT @args
exit $LASTEXITCODE
'@)

# Runs the script in its own Windows PowerShell with the scratch profile,
# no CARGO_HOME, no refused variable and the stand-ins first on PATH, plus
# $Vars; from $From.
$refusedNames = @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET')
foreach ($item in @(Get-ChildItem Env:)) {
    if ($item.Name -like 'CARGO_TARGET_*_RUSTFLAGS') { $refusedNames += $item.Name }
}
$basePath = $env:PATH
function Invoke-Release([hashtable] $Vars = @{}, [switch] $NoBuild, [string] $Script = $script:script, [string] $From = $work) {
    $set = @{ USERPROFILE = $profileDir; CARGO_HOME = $null; FAKE_LOG = $log; FAKE_EMBED = $null; FAKE_TOUCH = $null; PATH = "$bin;$basePath" }
    foreach ($name in $refusedNames) { $set[$name] = $null }
    foreach ($name in $Vars.Keys) { $set[$name] = $Vars[$name] }
    $before = @{}
    foreach ($name in @($set.Keys)) {
        $before[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $set[$name], 'Process')
    }
    [IO.File]::WriteAllText($log, '')
    Push-Location $From
    try {
        # Its error goes to stderr; Windows PowerShell 5.1 would make that
        # an exception here, and Out-String would wrap it at the console's
        # width, so each line is taken as it is.
        $ErrorActionPreference = 'Continue'
        $arguments = @('-NoProfile', '-File', $Script)
        if ($NoBuild) { $arguments += '-NoBuild' }
        $out = (& powershell.exe @arguments 2>&1 | ForEach-Object { "$_" }) -join "`n"
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
        foreach ($name in @($before.Keys)) {
            [Environment]::SetEnvironmentVariable($name, $before[$name], 'Process')
        }
    }
    [pscustomobject]@{ Code = $code; Out = $out }
}

try {
    Push-Location $work
    try {
        # A checkout of its own: the manifests, the packaged files, the script.
        foreach ($dir in @('packaging\windows', 'apps\desktop', 'target\release')) {
            New-Item -ItemType Directory -Force $dir | Out-Null
        }
        Copy-Item (Join-Path $source 'packaging\windows\make-release.ps1') 'packaging\windows\'
        foreach ($file in @('install.ps1', 'uninstall.ps1', 'revert.ps1', 'setup.ps1', 'install.cmd', 'uninstall.cmd', 'revert.cmd', 'setup.cmd', 'README.md', 'README.zh-TW.md')) {
            [IO.File]::WriteAllText((Join-Path $work "packaging\windows\$file"), "fake $file")
        }
        $guides = @('INSTALL.md', 'INSTALL.zh-TW.md')
        foreach ($file in $guides) {
            [IO.File]::WriteAllText((Join-Path $work "packaging\$file"), "fake $file")
        }
        [IO.File]::WriteAllText((Join-Path $work 'Cargo.toml'), "[workspace.package]`nversion = `"9.8.7`"`n")
        [IO.File]::WriteAllText((Join-Path $work 'Cargo.lock'), "# fake lock file`n")
        # The one version (since 4.2.1) is the workspace's; package.json has none.
        [IO.File]::WriteAllText((Join-Path $work 'apps\desktop\package.json'), '{ "name": "@wgal/desktop", "private": true }')
        [IO.File]::WriteAllText((Join-Path $work '.gitignore'), "target/`ndist/`n")
        git init -q . 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid add -A 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid commit -q -m 'scratch' 2>&1 | Out-Null
        $head = (git rev-parse HEAD).Trim()
        $exes = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe', 'wgal.exe')
        foreach ($exe in $exes) {
            [IO.File]::WriteAllText((Join-Path $work "target\release\$exe"), "fake $exe")
        }
        $script = Join-Path $work 'packaging\windows\make-release.ps1'
        $record = Join-Path $work 'target\release\wgal-build.txt'
        $name = 'wgal-9.8.7-windows-x64'
        $dist = Join-Path $work 'dist'
        $stage = Join-Path $dist $name
        $zip = Join-Path $dist "$name.zip"
        $lines = {
            param($commit, $listed = $exes, $version = '9.8.7')
            $l = @("commit $commit", "version $version")
            foreach ($exe in $listed) {
                $l += "$((Get-FileHash -Algorithm SHA256 (Join-Path $work "target\release\$exe")).Hash.ToLower())  $exe"
            }
            [IO.File]::WriteAllLines($record, $l, (New-Object Text.UTF8Encoding $false))
        }
        $nothingPacked = {
            -not (Test-Path -LiteralPath $stage) -and -not (Test-Path -LiteralPath $zip) -and
            -not ((Test-Path -LiteralPath $dist) -and @(Get-ChildItem -LiteralPath $dist -Force -Filter '.wgal-*').Count -gt 0)
        }
        $logged = { @([IO.File]::ReadAllLines($log)) }
        $timesLogged = { param([string] $Line) @(& $logged | Where-Object { $_ -ceq $Line }).Count }
        $nothingBuilt = { @(& $logged).Count -eq 0 }
        $noPath = { param($r) -not $r.Out.Contains($top.TrimEnd('\')) }
        $filesIn = { param([string] $Folder) @(Get-ChildItem -LiteralPath $Folder -Recurse -File -Force).Count }

        $r = Invoke-Release -NoBuild
        Check 'no record: refused' ($r.Code -ne 0 -and $r.Out -like '*no build record*')
        Check 'no record: nothing packed' (-not (Test-Path $dist))

        & $lines 'fedcba9876543210fedcba9876543210fedcba98'
        $r = Invoke-Release -NoBuild
        Check 'another commit: refused' ($r.Code -ne 0 -and $r.Out -like '*another commit*')
        Check 'another commit: nothing packed' (-not (Test-Path $dist))

        & $lines $head
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal-mcp.exe'), 'another build of the same version')
        $r = Invoke-Release -NoBuild
        Check 'a changed program: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'a changed program: nothing packed' (-not (Test-Path $dist))

        # Only the command line replaced, the commit and versions the same.
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal-mcp.exe'), 'fake wgal-mcp.exe')
        & $lines $head
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal.exe'), 'another wgal')
        $r = Invoke-Release -NoBuild
        Check 'only the command line changed: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'only the command line changed: nothing packed' (-not (Test-Path $dist))
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal.exe'), 'fake wgal.exe')

        # A record of a build without the command line (before 3.0.0).
        & $lines $head @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe')
        $r = Invoke-Release -NoBuild
        Check 'a record without the command line: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'a record without the command line: nothing packed' (-not (Test-Path $dist))

        # The same commit and programs, built as another version.
        & $lines $head $exes '9.8.6'
        $r = Invoke-Release -NoBuild
        Check 'another version: refused' ($r.Code -ne 0 -and $r.Out -like '*version differs*')
        Check 'another version: nothing packed' (-not (Test-Path $dist))

        # A commit without the English guide: nothing is zipped.
        git rm -q 'packaging/INSTALL.md' 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid commit -q -m 'no guide' 2>&1 | Out-Null
        & $lines (git rev-parse HEAD).Trim()
        $r = Invoke-Release -NoBuild
        Check 'no English guide: refused' ($r.Code -ne 0 -and $r.Out -like '*INSTALL.md*')
        Check 'no English guide: no zip' (& $nothingPacked)
        [IO.File]::WriteAllText((Join-Path $work 'packaging\INSTALL.md'), 'fake INSTALL.md')
        git -c user.name=test -c user.email=test@example.invalid add -A 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid commit -q -m 'the guide again' 2>&1 | Out-Null
        $head = (git rev-parse HEAD).Trim()

        & $lines $head
        $r = Invoke-Release -NoBuild
        Check 'this commit: packed' ($r.Code -eq 0 -and (Test-Path $zip))
        foreach ($file in @('README.md', 'README.zh-TW.md') + $guides) {
            Check "this commit: $file is packed" ((Test-Path (Join-Path $stage $file)) -and
                ((Get-Content -Raw (Join-Path $stage $file)) -eq "fake $file"))
        }
        Check 'this commit: the command line is packed' (Test-Path (Join-Path $stage 'wgal.exe'))
        $release = Join-Path $stage 'RELEASE.txt'
        Check 'this commit: RELEASE.txt names it' ((Test-Path $release) -and ((Get-Content -Raw $release) -like "*Commit $head*"))
        $first = @(Get-Content $release | Select-Object -First 2)
        Check 'this commit: RELEASE.txt opens with the one version' (($first -join "`n") -ceq "WG Always Live 9.8.7 for Windows (x64)`nVersion 9.8.7: the window, the watchdog, wgal-mcp and wgal")
        Check 'this commit: RELEASE.txt points to the English guide' ((Get-Content -Raw $release) -like '*see INSTALL.md*')
        foreach ($file in @('README.md', 'README.zh-TW.md') + $guides) {
            $copy = Join-Path $stage $file
            $hash = if (Test-Path $copy) { (Get-FileHash -Algorithm SHA256 $copy).Hash.ToLower() } else { 'not packed' }
            Check "this commit: RELEASE.txt lists $file" ((Get-Content -Raw $release) -like "*$hash  $file*")
        }
        Check 'this commit: every packed file was checked, RELEASE.txt too' ($r.Out -like "*Checked $(& $filesIn $stage) files: no build paths*")
        Check 'this commit: no temporary folder is left' (@(Get-ChildItem -LiteralPath $dist -Force -Filter '.wgal-*').Count -eq 0)

        # -NoBuild checks the programs too.
        [IO.File]::AppendAllText((Join-Path $work 'target\release\wgal-mcp.exe'), $work)
        & $lines $head
        $r = Invoke-Release -NoBuild
        Check '-NoBuild with the repository path: refused' ($r.Code -ne 0 -and $r.Out -like '*  wgal-mcp.exe: the path of the repository, UTF-8, 1*')
        Check '-NoBuild with the repository path: nothing packed' (& $nothingPacked)
        Check '-NoBuild with the repository path: the path is not printed' (& $noPath $r)

        # A build with the stand-ins.
        $cargoHome = Join-Path $profileDir '.cargo'
        $flags = { param([string] $CargoHome) "--remap-path-prefix=$CargoHome=/cargo|--remap-path-prefix=$work=/wgal" }
        $r = Invoke-Release
        Check 'a build: packed' ($r.Code -eq 0 -and (Test-Path $zip))
        foreach ($package in @('wgal-runtime --bin wgal-watchdog', 'wgal-mcp', 'wgal-cli')) {
            Check "a build: cargo builds $package locked" ((& $timesLogged "cargo args=build --release --locked -p $package") -eq 1)
        }
        Check 'a build: cargo runs in the repository' ((& $timesLogged "cargo cwd=$work") -eq 3)
        Check 'a build: the window is built locked' ((& $timesLogged 'pnpm args=--filter @wgal/desktop run tauri build --no-bundle -- --locked') -eq 1)
        Check 'a build: cargo gets exactly the two rewrites' ((& $timesLogged "cargo flags=$(& $flags $cargoHome)") -eq 3)
        Check "a build: the window's build gets them" ((& $timesLogged "pnpm flags=$(& $flags $cargoHome)") -eq 1)
        Check 'a build: CARGO_HOME is the profile folder''s .cargo' ((& $timesLogged "cargo home=$cargoHome") -eq 3)
        Check "a build: the window's build gets that CARGO_HOME" ((& $timesLogged "pnpm home=$cargoHome") -eq 1)
        Check 'a build: every packed file was checked' ($r.Out -like "*Checked $(& $filesIn $stage) files: no build paths*")
        Check 'a build: no temporary folder is left' (@(Get-ChildItem -LiteralPath $dist -Force -Filter '.wgal-*').Count -eq 0)
        Check 'a build: the record names this commit' (@([IO.File]::ReadAllLines($record)) -contains "commit $head")

        # CARGO_HOME outside the profile folder, and relative to where the
        # script was started (not to the repository, where it does not exist).
        $other = Join-Path $top ("other cargo " + [char]::ConvertFromUtf32(0x1D11E))
        $elsewhere = Join-Path $top 'elsewhere'
        New-Item -ItemType Directory $other, (Join-Path $elsewhere 'cargo rel') | Out-Null
        $r = Invoke-Release @{ CARGO_HOME = $other }
        Check 'CARGO_HOME elsewhere: packed' ($r.Code -eq 0)
        Check 'CARGO_HOME elsewhere: the rewrite names it' ((& $timesLogged "cargo flags=$(& $flags $other)") -eq 3)
        Check 'CARGO_HOME elsewhere: cargo gets it' ((& $timesLogged "cargo home=$other") -eq 3)
        $r = Invoke-Release @{ CARGO_HOME = 'cargo rel' } -From $elsewhere
        Check 'a relative CARGO_HOME: packed' ($r.Code -eq 0)
        Check 'a relative CARGO_HOME: taken from where the script started' ((& $timesLogged "cargo flags=$(& $flags (Join-Path $elsewhere 'cargo rel'))") -eq 3)
        Check 'a relative CARGO_HOME: cargo gets it absolute' ((& $timesLogged "cargo home=$(Join-Path $elsewhere 'cargo rel')") -eq 3)
        Check 'before a refusal: the release is there' (Test-Path -LiteralPath $zip)
        $r = Invoke-Release @{ CARGO_HOME = (Join-Path $top 'missing') }
        Check 'a missing Cargo home: refused' ($r.Code -ne 0 -and $r.Out -like "*Cargo's home folder*")
        Check 'a missing Cargo home: nothing built' (& $nothingBuilt)
        Check 'a missing Cargo home: the same-name release was deleted' (& $nothingPacked)
        Check 'a missing Cargo home: the path is not printed' (& $noPath $r)

        # dist\ as a junction is refused before anything is deleted.
        $savedDist = Join-Path $top 'dist.saved'
        Move-Item -LiteralPath $dist -Destination $savedDist
        $outside = Join-Path $top 'outside'
        New-Item -ItemType Directory (Join-Path $outside $name) | Out-Null
        [IO.File]::WriteAllText((Join-Path $outside "$name\sentinel"), 'keep')
        [IO.File]::WriteAllText((Join-Path $outside "$name.zip"), 'keep')
        New-Item -ItemType Junction -Path $dist -Target $outside | Out-Null
        $r = Invoke-Release
        Check 'dist\ as a junction: refused' ($r.Code -ne 0 -and $r.Out -like '*link or a junction*')
        Check 'dist\ as a junction: nothing built' (& $nothingBuilt)
        Check 'dist\ as a junction: the folder it points to is untouched' ((Test-Path (Join-Path $outside "$name\sentinel")) -and (Test-Path (Join-Path $outside "$name.zip")))
        Check 'dist\ as a junction: the path is not printed' (& $noPath $r)
        [IO.Directory]::Delete($dist, $false)
        Move-Item -LiteralPath $savedDist -Destination $dist

        # A version that is not a plain name is refused before anything is deleted.
        $cargoToml = Join-Path $work 'Cargo.toml'
        $savedToml = [IO.File]::ReadAllText($cargoToml)
        [IO.File]::WriteAllText($cargoToml, "[workspace.package]`nversion = `"9.8.7\..\..\x`"`n")
        $r = Invoke-Release
        Check 'a version that is not a plain name: refused' ($r.Code -ne 0 -and $r.Out -like '*not a plain name*')
        Check 'a version that is not a plain name: nothing built' (& $nothingBuilt)
        [IO.File]::WriteAllText($cargoToml, $savedToml)

        # git failing is never taken for a clean checkout.
        $r = Invoke-Release @{ PATH = "$gitbin;$bin;$basePath"; FAKE_REAL_GIT = $realGit; FAKE_GIT_FAIL = 'status' }
        Check 'git status failing: refused' ($r.Code -ne 0 -and $r.Out -like '*git could not read the checkout*')
        Check 'git status failing: nothing built' (& $nothingBuilt)
        Check 'git status failing: nothing packed' (& $nothingPacked)
        $r = Invoke-Release @{ PATH = "$gitbin;$bin;$basePath"; FAKE_REAL_GIT = $realGit; FAKE_GIT_FAIL = 'after-build' }
        Check 'git status failing after the build: refused' ($r.Code -ne 0 -and $r.Out -like '*git could not read the checkout*')
        Check 'git status failing after the build: nothing packed' (& $nothingPacked)

        # A file the build adds to the checkout zips nothing.
        $newFile = Join-Path $work 'made-by-the-build.rs'
        $r = Invoke-Release @{ FAKE_NEWFILE = $newFile }
        Check 'a new file from the build: refused' ($r.Code -ne 0 -and $r.Out -like '*the build changed the checkout*')
        Check 'a new file from the build: nothing packed' (& $nothingPacked)
        Remove-Item -LiteralPath $newFile

        # Links and junctions on the way are refused.
        $profileLink = Join-Path $top 'profile link'
        New-Item -ItemType Junction -Path $profileLink -Target $profileDir | Out-Null
        $junctions += $profileLink
        $r = Invoke-Release
        Check 'before the junction refusal: the release is there' ($r.Code -eq 0 -and (Test-Path -LiteralPath $zip))
        $r = Invoke-Release @{ USERPROFILE = $profileLink }
        Check 'a profile folder through a junction: refused' ($r.Code -ne 0 -and $r.Out -like '*link or a junction*')
        Check 'a profile folder through a junction: nothing built' (& $nothingBuilt)
        Check 'a profile folder through a junction: the same-name release was deleted' (& $nothingPacked)
        Check 'a profile folder through a junction: the path is not printed' (& $noPath $r)
        $r = Invoke-Release @{ CARGO_HOME = (Join-Path $profileLink '.cargo') }
        Check "Cargo's home through a junction: refused" ($r.Code -ne 0 -and $r.Out -like '*link or a junction*')
        $repoLink = Join-Path $top 'repo link'
        New-Item -ItemType Junction -Path $repoLink -Target $work | Out-Null
        $junctions += $repoLink
        $r = Invoke-Release -Script (Join-Path $repoLink 'packaging\windows\make-release.ps1') -From $repoLink
        Check 'the repository through a junction: refused' ($r.Code -ne 0 -and $r.Out -like '*link or a junction*')
        Check 'the repository through a junction: nothing built' (& $nothingBuilt)

        # Refused variables build nothing, and the same-name release goes
        # first; other files in dist\ stay.
        New-Item -ItemType Directory -Force $stage, (Join-Path $dist 'wgal-9.8.6-windows-x64') | Out-Null
        [IO.File]::WriteAllText((Join-Path $stage 'old'), 'old')
        [IO.File]::WriteAllText($zip, 'old')
        [IO.File]::WriteAllText((Join-Path $dist 'keep.txt'), 'keep')
        foreach ($variable in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS',
                'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_TARGET')) {
            $r = Invoke-Release @{ $variable = 'x' }
            Check "${variable}: refused" ($r.Code -ne 0 -and $r.Out -like "*$variable is set*")
            Check "${variable}: nothing built" (& $nothingBuilt)
            Check "${variable}: nothing packed" (& $nothingPacked)
        }
        Check 'the old same-name release was deleted' (-not (Test-Path (Join-Path $stage 'old')))
        Check 'other files in dist\ stay' ((Test-Path (Join-Path $dist 'keep.txt')) -and (Test-Path (Join-Path $dist 'wgal-9.8.6-windows-x64')))

        # Refused Cargo configurations, in each folder cargo runs in or above
        # it, and in Cargo's home.
        foreach ($place in @((Join-Path $work '.cargo\config.toml'), (Join-Path $work 'apps\desktop\src-tauri\.cargo\config.toml'),
                (Join-Path $work 'apps\.cargo\config'), (Join-Path (Split-Path $work) '.cargo\config.toml'), (Join-Path $cargoHome 'config.toml'))) {
            New-Item -ItemType Directory -Force (Split-Path $place) | Out-Null
            [IO.File]::WriteAllText($place, "[build]`ntarget-dir = `"elsewhere`"`n")
            $r = Invoke-Release
            $shown = $place.Substring($top.Length + 1)
            Check "a configuration in ${shown}: refused" ($r.Code -ne 0 -and $r.Out -like '*has target-dir*')
            Check "a configuration in ${shown}: nothing built" (& $nothingBuilt)
            Check "a configuration in ${shown}: nothing packed" (& $nothingPacked)
            Check "a configuration in ${shown}: the path is not printed" (& $noPath $r)
            Remove-Item -LiteralPath $place
        }
        [IO.File]::WriteAllText((Join-Path $work '.cargo\config.toml'), "[net]`nretry = 3`n")
        $r = Invoke-Release
        Check 'a harmless configuration: packed' ($r.Code -eq 0)
        Remove-Item -LiteralPath (Join-Path $work '.cargo\config.toml')

        # A program that names a forbidden folder, in either encoding, zips
        # nothing; the message names the file, the folder and the encoding only.
        foreach ($program in $exes) {
            foreach ($which in @(@('profile', 'profile folder'), @('cargo', 'Cargo home'), @('repo', 'repository'))) {
                foreach ($encoding in @(@('utf8', 'UTF-8'), @('utf16', 'UTF-16LE'))) {
                    $r = Invoke-Release @{ FAKE_EMBED = "$program,$($which[0]),$($encoding[0])" }
                    $what = "$program with the $($which[1]) path in $($encoding[1])"
                    Check "${what}: refused" ($r.Code -ne 0 -and $r.Out -like "*  ${program}: the path of the $($which[1]), $($encoding[1]), 1*")
                    Check "${what}: nothing packed" (& $nothingPacked)
                    Check "${what}: the path is not printed" (& $noPath $r)
                }
            }
        }
        $fso = New-Object -ComObject Scripting.FileSystemObject
        $variants = @(@('repoupper', 'in upper case'), @('reposlash', 'with /'))
        $repoParent = Split-Path $work
        if ($fso.GetFolder($repoParent).ShortName -ne (Split-Path $repoParent -Leaf)) {
            $variants += @(@('reposhort', 'in 8.3 short names'), @('repomixed', 'with one part in its 8.3 short name'))
        } else {
            Write-Host 'SKIP the repository in 8.3 short names: this volume makes none'
        }
        foreach ($variant in $variants) {
            foreach ($encoding in @(@('utf8', 'UTF-8'), @('utf16', 'UTF-16LE'))) {
                $r = Invoke-Release @{ FAKE_EMBED = "wgal.exe,$($variant[0]),$($encoding[0])" }
                Check "the repository $($variant[1]) in $($encoding[1]): refused" ($r.Code -ne 0 -and $r.Out -like "*  wgal.exe: the path of the repository, $($encoding[1]), 1*")
                Check "the repository $($variant[1]) in $($encoding[1]): nothing packed" (& $nothingPacked)
            }
        }

        # A build that changes Cargo.lock zips nothing.
        $r = Invoke-Release @{ FAKE_TOUCH = (Join-Path $work 'Cargo.lock') }
        Check 'a changed Cargo.lock: refused' ($r.Code -ne 0 -and $r.Out -like '*the build changed the checkout*')
        Check 'a changed Cargo.lock: nothing packed' (& $nothingPacked)
        git checkout -q -- Cargo.lock 2>&1 | Out-Null

        # In this very process: CARGO_HOME and CARGO_ENCODED_RUSTFLAGS are
        # put back after a build and after a refusal.
        $outcomes = @()
        foreach ($embed in @($null, 'wgal.exe,repo,utf8')) {
            $before = @{}
            $set = @{ USERPROFILE = $profileDir; CARGO_HOME = $other; FAKE_LOG = $log; FAKE_EMBED = $embed; PATH = "$bin;$basePath" }
            foreach ($key in @($set.Keys)) {
                $before[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
                [Environment]::SetEnvironmentVariable($key, $set[$key], 'Process')
            }
            try {
                $threw = $false
                try {
                    & { . $script; Invoke-Main } 6>&1 | Out-Null
                } catch {
                    $threw = $true
                }
                $outcomes += $threw
                $label = if ($embed) { 'after a refusal' } else { 'after a build' }
                Check "${label}: CARGO_HOME is as it was" ([Environment]::GetEnvironmentVariable('CARGO_HOME', 'Process') -eq $other)
                Check "${label}: CARGO_ENCODED_RUSTFLAGS is as it was" ($null -eq [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process'))
                Check "${label}: the location is as it was" ((Get-Location).ProviderPath -eq $work)
            } finally {
                foreach ($key in @($before.Keys)) {
                    [Environment]::SetEnvironmentVariable($key, $before[$key], 'Process')
                }
            }
        }
        Check 'in this process: the build passed and the refusal refused' (($outcomes -join ',') -eq 'False,True')

        # The last step failing (the zip's hash) leaves nothing either.
        $before = @{}
        $set = @{ USERPROFILE = $profileDir; CARGO_HOME = $null; FAKE_LOG = $log; FAKE_EMBED = $null; PATH = "$bin;$basePath" }
        foreach ($key in @($set.Keys)) {
            $before[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, $set[$key], 'Process')
        }
        try {
            $message = try {
                & {
                    . $script
                    function Get-FileHash([string] $Algorithm, [string] $LiteralPath) {
                        if ($LiteralPath.EndsWith('.zip')) { throw 'the hash failed' }
                        Microsoft.PowerShell.Utility\Get-FileHash -Algorithm $Algorithm -LiteralPath $LiteralPath
                    }
                    Invoke-Main
                } 6>&1 | Out-Null
                'none'
            } catch {
                $_.Exception.Message
            }
            Check "the zip's hash failing: it stops" ($message -eq 'the hash failed')
            Check "the zip's hash failing: nothing packed" (& $nothingPacked)
            Check "the zip's hash failing: CARGO_ENCODED_RUSTFLAGS is as it was" ($null -eq [Environment]::GetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', 'Process'))
        } finally {
            foreach ($key in @($before.Keys)) {
                [Environment]::SetEnvironmentVariable($key, $before[$key], 'Process')
            }
        }
    } finally {
        Pop-Location
    }
} finally {
    foreach ($junction in $junctions) {
        if (Test-Path -LiteralPath $junction) { [IO.Directory]::Delete($junction, $false) }
    }
    Remove-Item -Recurse -Force $top
}

if ($failures -gt 0) { Write-Host "$failures failed"; exit 1 }
Write-Host 'all passed'
