#Requires -Version 5.1
<#
.SYNOPSIS
Builds the Windows release zip, dist\wgal-<version>-windows-x64.zip, where
the version is the one the whole set shares since 4.2.1 (the Cargo
workspace's; owner decision of 2026-11-19).
Run it only when the owner asks for a release.

.DESCRIPTION
From a clean checkout: builds wgal-watchdog, wgal-mcp, wgal and wgal-desktop in
release mode, puts them in dist\wgal-<version>-windows-x64\ with
install, uninstall, revert and setup (each as .ps1 and .cmd), README.md
and README.zh-TW.md, INSTALL.md and INSTALL.zh-TW.md (the guide an agent
can follow, in English and in Traditional Chinese) and RELEASE.txt (the
version, the commit and every file's SHA-256), and zips that folder. A
missing document stops it before the zip is made. It
needs no administrator rights and writes nothing outside target\ and dist\;
it installs nothing. The zip is not code-signed (owner decision of
2026-11-05).

A build writes target\release\wgal-build.txt beside the programs: the
commit, the version and each program's SHA-256.

.PARAMETER NoBuild
Use the programs already in target\release instead of building them. Only
when wgal-build.txt says they were built from this very commit with this
version and none has changed since; otherwise nothing is packed (FIX-06:
a version mark alone does not tell one commit's build from another's).
#>
[CmdletBinding()]
param(
    [switch] $NoBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Programs = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe', 'wgal.exe')
$Scripts = @('install.ps1', 'uninstall.ps1', 'revert.ps1', 'setup.ps1', 'install.cmd', 'uninstall.cmd', 'revert.cmd', 'setup.cmd')
# This folder's README in English and in Traditional Chinese, and the
# install guide every release carries (packaging\), in both.
$Readmes = @('README.md', 'README.zh-TW.md')
$Guides = @('INSTALL.md', 'INSTALL.zh-TW.md')

$RecordFile = 'target\release\wgal-build.txt'

# The build record's lines for the programs in $Folder as they are now.
function Get-BuildRecord([string] $Folder, [string] $Commit, [string] $Version, [string[]] $Files) {
    $lines = @("commit $Commit", "version $Version")
    foreach ($file in $Files) {
        $path = Join-Path $Folder $file
        $hash = if (Test-Path -LiteralPath $path -PathType Leaf) {
            (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower()
        } else {
            'missing'
        }
        $lines += "$hash  $file"
    }
    return , $lines
}

function Write-BuildRecord([string] $Record, [string[]] $Lines) {
    [IO.File]::WriteAllLines($Record, $Lines, (New-Object Text.UTF8Encoding $false))
}

# Why the programs may not be packed as built from $Commit, or $null when
# the record says they were and none has changed.
function Test-BuildRecord([string] $Record, [string[]] $Expected) {
    if (-not (Test-Path -LiteralPath $Record -PathType Leaf)) {
        return "there is no build record ($RecordFile); build once without -NoBuild"
    }
    $recorded = @([IO.File]::ReadAllLines($Record))
    $commit = ($recorded | Where-Object { $_ -like 'commit *' } | Select-Object -First 1)
    if ($commit -ne $Expected[0]) {
        return "the programs were built from another commit ($commit), not $($Expected[0])"
    }
    if (($recorded -join "`n") -cne ($Expected -join "`n")) {
        return 'a program changed since it was built, or the version differs from the record'
    }
    return $null
}

function Invoke-Checked([string] $What, [scriptblock] $Command) {
    Write-Host "== $What"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed (exit $LASTEXITCODE)."
    }
}

function Invoke-Main {
Push-Location $Root
try {
    $dirty = git status --porcelain --untracked-files=no
    if ($dirty) {
        throw 'The checkout has uncommitted changes; a release is built from a commit.'
    }
    $commit = (git rev-parse HEAD).Trim()
    $workspace = Get-Content -Raw -Encoding UTF8 'Cargo.toml'
    if ($workspace -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
        throw 'The version is not in Cargo.toml.'
    }
    $version = $Matches[1]

    $built = Join-Path $Root 'target\release'
    $record = Join-Path $Root $RecordFile
    if ($NoBuild) {
        $why = Test-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
        if ($null -ne $why) {
            throw "-NoBuild: $why. Nothing was packed."
        }
    } else {
        Invoke-Checked 'wgal-watchdog' { cargo build --release --locked -p wgal-runtime --bin wgal-watchdog }
        Invoke-Checked 'wgal-mcp' { cargo build --release --locked -p wgal-mcp }
        Invoke-Checked 'wgal' { cargo build --release --locked -p wgal-cli }
        Invoke-Checked 'wgal-desktop' { pnpm --filter '@wgal/desktop' run tauri build --no-bundle }
        Write-BuildRecord $record (Get-BuildRecord $built $commit $version $Programs)
    }

    $name = "wgal-$version-windows-x64"
    $dist = Join-Path $Root 'dist'
    $stage = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    if (Test-Path $zip) { Remove-Item -Force $zip }
    New-Item -ItemType Directory -Force $stage | Out-Null

    foreach ($file in $Programs) {
        Copy-Item (Join-Path $Root "target\release\$file") $stage
    }
    foreach ($file in $Scripts + $Readmes) {
        Copy-Item (Join-Path $Root "packaging\windows\$file") $stage
    }
    foreach ($file in $Guides) {
        Copy-Item (Join-Path $Root "packaging\$file") $stage
    }

    $lines = @(
        "WG Always Live $version for Windows (x64)",
        "Version ${version}: the window, the watchdog, wgal-mcp and wgal",
        "Commit $commit",
        'Not code-signed. Install and remove with the scripts: see INSTALL.md (in Traditional Chinese: INSTALL.zh-TW.md).',
        '',
        'SHA-256'
    )
    foreach ($item in Get-ChildItem $stage -File | Sort-Object Name) {
        $hash = (Get-FileHash -Algorithm SHA256 $item.FullName).Hash.ToLower()
        $lines += "$hash  $($item.Name)"
    }
    [IO.File]::WriteAllLines((Join-Path $stage 'RELEASE.txt'), $lines, (New-Object Text.UTF8Encoding $false))

    Compress-Archive -Path $stage -DestinationPath $zip
    $zipHash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
    Write-Host "Built $zip"
    Write-Host "SHA-256 $zipHash"
} finally {
    Pop-Location
}
}

# Dot-sourced (by the tests) it only defines the functions.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Main
}
