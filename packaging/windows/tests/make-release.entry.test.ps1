#Requires -Version 5.1
# Runs make-release.ps1 -NoBuild itself (FIX-06, Codex review): in a scratch
# git checkout of its own, with fake programs and the files the script packs,
# it must refuse, and pack nothing, without a build record, with a record of
# another commit and with a program changed since the record; with a record
# of this commit it packs. Nothing is built; nothing is written outside the
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

$work = Join-Path ([IO.Path]::GetTempPath()) ("wgal-make-release-entry-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
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
        $zip = Join-Path $work 'dist\wgal-9.8.7-windows-x64.zip'
        $run = {
            # Its error goes to stderr; Windows PowerShell 5.1 would make that
            # an exception here.
            $ErrorActionPreference = 'Continue'
            $out = powershell.exe -NoProfile -File $script -NoBuild 2>&1 | Out-String
            [pscustomobject]@{ Code = $LASTEXITCODE; Out = $out }
        }
        $lines = {
            param($commit, $listed = $exes, $version = '9.8.7')
            $l = @("commit $commit", "version $version")
            foreach ($exe in $listed) {
                $l += "$((Get-FileHash -Algorithm SHA256 (Join-Path $work "target\release\$exe")).Hash.ToLower())  $exe"
            }
            [IO.File]::WriteAllLines($record, $l, (New-Object Text.UTF8Encoding $false))
        }

        $r = & $run
        Check 'no record: refused' ($r.Code -ne 0 -and $r.Out -like '*no build record*')
        Check 'no record: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))

        & $lines 'fedcba9876543210fedcba9876543210fedcba98'
        $r = & $run
        Check 'another commit: refused' ($r.Code -ne 0 -and $r.Out -like '*another commit*')
        Check 'another commit: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))

        & $lines $head
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal-mcp.exe'), 'another build of the same version')
        $r = & $run
        Check 'a changed program: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'a changed program: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))

        # Only the command line replaced, the commit and versions the same.
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal-mcp.exe'), 'fake wgal-mcp.exe')
        & $lines $head
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal.exe'), 'another wgal')
        $r = & $run
        Check 'only the command line changed: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'only the command line changed: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))
        [IO.File]::WriteAllText((Join-Path $work 'target\release\wgal.exe'), 'fake wgal.exe')

        # A record of a build without the command line (before 3.0.0).
        & $lines $head @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe')
        $r = & $run
        Check 'a record without the command line: refused' ($r.Code -ne 0 -and $r.Out -like '*changed*')
        Check 'a record without the command line: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))

        # The same commit and programs, built as another version.
        & $lines $head $exes '9.8.6'
        $r = & $run
        Check 'another version: refused' ($r.Code -ne 0 -and $r.Out -like '*version differs*')
        Check 'another version: nothing packed' (-not (Test-Path (Join-Path $work 'dist')))

        # A commit without the English guide: nothing is zipped.
        git rm -q 'packaging/INSTALL.md' 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid commit -q -m 'no guide' 2>&1 | Out-Null
        & $lines (git rev-parse HEAD).Trim()
        $r = & $run
        Check 'no English guide: refused' ($r.Code -ne 0 -and $r.Out -like '*INSTALL.md*')
        Check 'no English guide: no zip' (-not (Test-Path $zip))
        [IO.File]::WriteAllText((Join-Path $work 'packaging\INSTALL.md'), 'fake INSTALL.md')
        git -c user.name=test -c user.email=test@example.invalid add -A 2>&1 | Out-Null
        git -c user.name=test -c user.email=test@example.invalid commit -q -m 'the guide again' 2>&1 | Out-Null
        $head = (git rev-parse HEAD).Trim()

        & $lines $head
        $r = & $run
        Check 'this commit: packed' ($r.Code -eq 0 -and (Test-Path $zip))
        $packed = Join-Path $work 'dist\wgal-9.8.7-windows-x64'
        foreach ($file in @('README.md', 'README.zh-TW.md') + $guides) {
            Check "this commit: $file is packed" ((Test-Path (Join-Path $packed $file)) -and
                ((Get-Content -Raw (Join-Path $packed $file)) -eq "fake $file"))
        }
        Check 'this commit: the command line is packed' (Test-Path (Join-Path $work 'dist\wgal-9.8.7-windows-x64\wgal.exe'))
        $release = Join-Path $work 'dist\wgal-9.8.7-windows-x64\RELEASE.txt'
        Check 'this commit: RELEASE.txt names it' ((Test-Path $release) -and ((Get-Content -Raw $release) -like "*Commit $head*"))
        $first = @(Get-Content $release | Select-Object -First 2)
        Check 'this commit: RELEASE.txt opens with the one version' (($first -join "`n") -ceq "WG Always Live 9.8.7 for Windows (x64)`nVersion 9.8.7: the window, the watchdog, wgal-mcp and wgal")
        Check 'this commit: RELEASE.txt points to the English guide' ((Get-Content -Raw $release) -like '*see INSTALL.md*')
        foreach ($file in @('README.md', 'README.zh-TW.md') + $guides) {
            $copy = Join-Path $packed $file
            $hash = if (Test-Path $copy) { (Get-FileHash -Algorithm SHA256 $copy).Hash.ToLower() } else { 'not packed' }
            Check "this commit: RELEASE.txt lists $file" ((Get-Content -Raw $release) -like "*$hash  $file*")
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item -Recurse -Force $work
}

if ($failures -gt 0) { Write-Host "$failures failed"; exit 1 }
Write-Host 'all passed'
