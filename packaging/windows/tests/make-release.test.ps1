#Requires -Version 5.1
# Tests of make-release.ps1's build record (FIX-06): -NoBuild packs the
# programs only when the record says they were built from this commit with
# this version and none changed since. Fake files in a temporary folder;
# nothing is built, packed or installed.
#
#   powershell.exe -NoProfile -File packaging\windows\tests\make-release.test.ps1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Dot-sourced, the script only defines its functions: it builds nothing.
. (Join-Path $PSScriptRoot '..\make-release.ps1')

$failures = 0
function Check([string] $Name, [bool] $Ok) {
    if ($Ok) {
        Write-Host "PASS $Name"
    } else {
        Write-Host "FAIL $Name"
        $script:failures++
    }
}

$folder = Join-Path ([IO.Path]::GetTempPath()) ("wgal-make-release-test-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $folder | Out-Null
try {
    $files = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe')
    foreach ($file in $files) {
        [IO.File]::WriteAllText((Join-Path $folder $file), "fake $file")
    }
    $record = Join-Path $folder 'wgal-build.txt'
    $commit = '0123456789abcdef0123456789abcdef01234567'
    $now = { param($c, $v) Get-BuildRecord $folder $c $v $files }

    Check 'no record is refused' ($null -ne (Test-BuildRecord $record (& $now $commit '4.2.1')))

    Write-BuildRecord $record (& $now $commit '4.2.1')
    Check 'the record holds one version' (@([IO.File]::ReadAllLines($record)) -contains 'version 4.2.1')
    Check 'the same commit, version and programs are accepted' ($null -eq (Test-BuildRecord $record (& $now $commit '4.2.1')))

    $why = Test-BuildRecord $record (& $now 'fedcba9876543210fedcba9876543210fedcba98' '4.2.1')
    Check 'another commit is refused' ($null -ne $why -and $why -like '*another commit*')

    $why = Test-BuildRecord $record (& $now $commit '4.2.2')
    Check 'another version is refused' ($null -ne $why -and $why -like '*version differs*')

    [IO.File]::WriteAllText((Join-Path $folder 'wgal-mcp.exe'), 'another build of the same version')
    $why = Test-BuildRecord $record (& $now $commit '4.2.1')
    Check 'a changed program is refused' ($null -ne $why -and $why -like '*changed*')

    Remove-Item (Join-Path $folder 'wgal-mcp.exe')
    Check 'a missing program is refused' ($null -ne (Test-BuildRecord $record (& $now $commit '4.2.1')))

} finally {
    Remove-Item -Recurse -Force $folder
}

if ($failures -gt 0) {
    Write-Host "$failures failed"
    exit 1
}
Write-Host 'all passed'
