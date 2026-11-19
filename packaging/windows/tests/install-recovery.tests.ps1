#Requires -Version 5.1
<#
Tests install.ps1's upgrade recovery (FIX-05): every step between stopping
WGALWatchdog and starting it again is made to fail in turn, and the test
checks that recovery puts a complete set of executables in place, starts
it (or refuses to, with the right message), never puts back an access list
that was replaced, and never touches the stored state.

Run it as a normal user, from any folder:

  powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\install-recovery.tests.ps1

It loads install.ps1's functions (dot-sourcing runs none of the script's
steps) and replaces every service command with a stand-in defined here;
the files it reads and writes are in a new folder under %TEMP%, removed at
the end. The stand-ins exist only in this file: install.ps1 has no
parameter or variable that selects them.
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Service commands: stand-ins first, so nothing can reach the real ones ---
function Stop-Service { throw 'test: the real Stop-Service was reached' }
function Start-Service { throw 'test: the real Start-Service was reached' }
function Set-Service { throw 'test: the real Set-Service was reached' }
function New-Service { throw 'test: the real New-Service was reached' }
function Get-Service { throw 'test: the real Get-Service was reached' }
function Get-CimInstance { throw 'test: the real Get-CimInstance was reached' }
function sc.exe { throw 'test: the real sc.exe was reached' }

. (Join-Path $PSScriptRoot '..\install.ps1')

$script:Root = Join-Path ([IO.Path]::GetTempPath()) ('wgal-fix05-' + [Guid]::NewGuid().ToString('N'))
$script:Source = Join-Path $Root 'release'
$script:ProgramDir = Join-Path $Root 'Program Files\WGAL'
$script:DataDir = Join-Path $Root 'ProgramData\WGAL'

# --- Stand-ins for install.ps1's own steps, and failure points ---------------
$script:Fail = ''           # the step to fail: stop, backup, access, access-owner
$script:FailCopyAt = 0      # fail the n-th copy into a program file (1-4)
$script:FailCopyHow = ''    # 'before' (nothing written), 'partial' (half a file), 'after' (copied)
$script:FailRestore = $false
$script:FailStage = $false  # fail copying a new executable to its .new copy
$script:StartRuns = $true
$script:StartThrows = $false  # the first start throws
$script:StartBreaks = $false  # the first start damages wgal-mcp.exe, then throws
$script:FailMarkWrites = 0  # fail this many writes of upgrade.record's "started yes"
$script:Calls = @()
$script:StartLog = @()      # per start: "<wgal-watchdog.exe in place>|<the record's started line>"
$script:StartType = 'Automatic'  # WGALWatchdog's start type, as the stand-ins keep it
$script:FailDisable = ''    # 'throw' (setting Disabled fails) or 'ignored' (it changes nothing)

# WGAL's own service's start type: read and set on a value kept here, for
# WGALWatchdog only. Replaces the stand-ins above that refuse every call.
function Set-Service {
    param([string] $Name, [string] $StartupType)
    if ($Name -cne $ServiceName) { throw "test: Set-Service was called for another service: $Name" }
    $script:Calls += "starttype $StartupType"
    # What the record said when the service was let start at boot.
    if ($StartupType -eq 'Automatic') { $script:AutomaticWhile = Get-RecordStarted }
    if ($StartupType -eq 'Disabled') {
        if ($script:FailDisable -eq 'throw') { throw 'test: the start type could not be changed' }
        if ($script:FailDisable -eq 'ignored') { return }
    }
    $script:StartType = $StartupType
}

function Get-Service {
    param([string] $Name)
    if ($Name -cne $ServiceName) { throw "test: Get-Service was called for another service: $Name" }
    return [pscustomobject]@{ Name = $Name; StartType = $script:StartType }
}

# install.ps1's sc.exe settings; with $FailRecoveryActions they fail, as
# sc.exe failure or failureflag may.
$script:FailRecoveryActions = $false
$script:RecoveryActionsSet = 0
$script:AutomaticWhile = $null

# The first install's New-Service: the service as created, kept here.
function New-Service {
    param([string] $Name, [string] $BinaryPathName, [string] $DisplayName, [string] $Description, [string] $StartupType)
    if ($Name -cne $ServiceName) { throw "test: New-Service was called for another service: $Name" }
    $script:Calls += "new-service $StartupType"
    $script:StartType = $StartupType
}
function Set-WgalRecoveryActions {
    if ($script:FailRecoveryActions) { throw 'test: sc.exe failure returned 5.' }
    $script:RecoveryActionsSet += 1
}

function Stop-WgalService {
    $script:Calls += 'stop'
    if ($script:Fail -eq 'stop') { throw 'test: the service did not stop' }
}

function Start-WgalChecked {
    $script:Calls += 'start'
    $script:StartLog += "$(Get-ProgramText 'wgal-watchdog.exe')|$(Get-RecordStarted)"
    if ($script:StartThrows -and (@($script:Calls | Where-Object { $_ -eq 'start' }).Count -eq 1)) {
        throw 'test: the first start failed'
    }
    if ($script:StartBreaks -and (@($script:Calls | Where-Object { $_ -eq 'start' }).Count -eq 1)) {
        [IO.File]::WriteAllText((Join-Path $ProgramDir 'wgal-mcp.exe'), 'damaged')
        throw 'test: the first start failed, and wgal-mcp.exe is damaged'
    }
    $owner = if ($script:StartRuns) { $LocalSystem } else { $null }
    return @{ Running = $script:StartRuns; PipeOwner = $owner }
}

function Set-AdministratorsOwner([string] $Path) {
    # A normal user cannot give a file to Administrators; the owner is not
    # what these tests check.
    if (($script:Fail -eq 'access-owner') -and ($Path -eq (Get-AccessPath))) {
        throw 'test: could not set the owner of access.txt'
    }
}

# The temporary data folder is the test user's, not a protected one.
function Assert-ProtectedDirectory([string] $Path) { }

$script:RealBackup = ${function:Backup-State}
function Backup-State {
    if ($script:Fail -eq 'backup') { throw 'test: the backup failed' }
    return & $script:RealBackup @args
}

$script:RealSaveRecord = ${function:Save-ProgramRecord}
function Save-ProgramRecord($Set) {
    if ($Set.MayHaveRun -and ($script:FailMarkWrites -gt 0)) {
        $script:FailMarkWrites -= 1
        throw 'test: upgrade.record could not be written'
    }
    & $script:RealSaveRecord $Set
}

$script:RealInstallAccess = ${function:Install-StagedAccessList}
function Install-StagedAccessList {
    if ($script:Fail -eq 'access') { throw 'test: access.txt could not be replaced' }
    & $script:RealInstallAccess
}

$script:Copies = 0
function Copy-Item {
    param([string] $LiteralPath, [string] $Destination, [switch] $Force)
    $intoPlace = $Destination -match '\.exe$'
    if ($script:FailStage -and $Destination -match '\.exe\.new$') {
        throw 'test: staging a new executable failed'
    }
    if ($intoPlace -and $LiteralPath -match '\.old$' -and $script:FailRestore) {
        throw 'test: restoring a previous executable failed'
    }
    if ($intoPlace -and $LiteralPath -match '\.new$') {
        $script:Copies += 1
        if ($script:Copies -eq $script:FailCopyAt) {
            if ($script:FailCopyHow -eq 'partial') {
                [IO.File]::WriteAllText($Destination, 'half')
            }
            if ($script:FailCopyHow -eq 'after') {
                Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination -Force
            }
            throw "test: copy $($script:Copies) failed ($($script:FailCopyHow))"
        }
    }
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination -Force:$Force
}

# --- Fixture -------------------------------------------------------------------
function Reset-Fixture([switch] $NoOldMcp, [switch] $NoOldCli) {
    if (Test-Path -LiteralPath $Root) { Remove-Item -LiteralPath $Root -Recurse -Force }
    foreach ($dir in @($Source, $ProgramDir, (Join-Path $DataDir 'state'), (Join-Path $DataDir 'notify'))) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    foreach ($exe in $Executables) {
        [IO.File]::WriteAllText((Join-Path $Source $exe), "new $exe")
        # A previous version without wgal-mcp.exe (before 2.0.0) or without
        # wgal.exe (before 3.0.0).
        if (-not ($NoOldMcp -and $exe -eq 'wgal-mcp.exe') -and -not ($NoOldCli -and $exe -eq 'wgal.exe')) {
            [IO.File]::WriteAllText((Join-Path $ProgramDir $exe), "old $exe")
        }
    }
    [IO.File]::WriteAllText((Join-Path $DataDir 'state\state.json'), 'stored state')
    [IO.File]::WriteAllText((Join-Path $DataDir 'access.txt'), "old list`r`n")
    $script:Fail = ''
    $script:FailCopyAt = 0
    $script:FailCopyHow = ''
    $script:FailRestore = $false
    $script:FailStage = $false
    $script:StartRuns = $true
    $script:StartThrows = $false
    $script:StartBreaks = $false
    $script:FailMarkWrites = 0
    $script:Calls = @()
    $script:StartLog = @()
    $script:Copies = 0
    $script:StartType = 'Automatic'
    $script:FailDisable = ''
}

# Runs $Body in a child PowerShell that has loaded install.ps1 with its paths
# here and the given stand-ins, as Ctrl+C ends the whole script; returns
# what it wrote.
function Invoke-Child([string] $Functions, [string] $Body) {
    $child = Join-Path $Root 'child.ps1'
    [IO.File]::WriteAllText($child, @"
Set-StrictMode -Version Latest
`$ErrorActionPreference = 'Stop'
function Get-Service { throw 'test: the real Get-Service was reached' }
function Start-Service { throw 'test: the real Start-Service was reached' }
function Stop-Service { throw 'test: the real Stop-Service was reached' }
function sc.exe { throw 'test: the real sc.exe was reached' }
. '$(Join-Path $PSScriptRoot '..\install.ps1')'
`$ProgramDir = '$ProgramDir'
`$DataDir = '$DataDir'
function Set-AdministratorsOwner([string] `$Path) { }
function Assert-ProtectedDirectory([string] `$Path) { }
$Functions
$Body
"@)
    return @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $child 2>&1 3>&1 6>&1 |
            ForEach-Object { [string]$_ })
}

# Stages as install.ps1 does before the stop, then runs the window. Returns
# the window's result and the warnings it wrote. With -LockRecord a reader
# holds upgrade.record open during the window, sharing it for reading only:
# it can then be read, but neither rewritten nor removed. With -AsMain it
# also runs install.ps1's own Initialize-UpgradeService (the recovery
# actions, then the start type set back), after the staging and before the
# window; an error there is kept in $EnableError.
function Invoke-Window([bool] $WriteAccess = $true, [switch] $LockRecord, [switch] $AsMain) {
    $set = Save-ProgramSet $Source 6>$null
    if ($WriteAccess) {
        [void](New-StagedAccessList 'S-1-5-21-1000000000-2000000000-3000000000-1001' @('office'))
    }
    $script:EnableError = ''
    if ($AsMain) {
        try { Initialize-UpgradeService 6>$null } catch { $script:EnableError = $_.Exception.Message }
        if ($script:EnableError -ne '') {
            # install.ps1 stops there, before the window.
            return @{ Result = $null; Warnings = @() }
        }
    }
    $lock = $null
    if ($LockRecord) {
        $lock = New-Object IO.FileStream((Get-RecordPath), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    }
    try {
        $out = @(Invoke-UpgradeWindow $set $WriteAccess 3>&1 6>$null)
    } finally {
        if ($null -ne $lock) { $lock.Dispose() }
    }
    $result = @($out | Where-Object { $_ -is [hashtable] })
    $warnings = @($out | Where-Object { $_ -is [System.Management.Automation.WarningRecord] } |
            ForEach-Object { $_.Message })
    return @{ Result = $(if ($result.Count -gt 0) { $result[0] } else { $null }); Warnings = $warnings }
}

function Get-ProgramText([string] $Exe) {
    $path = Join-Path $ProgramDir $Exe
    if (-not (Test-Path -LiteralPath $path)) { return '<none>' }
    return [IO.File]::ReadAllText($path)
}

$script:Passed = 0
$script:Failed = 0
function Assert([bool] $Condition, [string] $What) {
    if ($Condition) {
        $script:Passed += 1
    } else {
        $script:Failed += 1
        Write-Host "FAIL: $What" -ForegroundColor Red
    }
}

function Assert-Programs([string] $Which, [string] $Case) {
    foreach ($exe in $Executables) {
        Assert ((Get-ProgramText $exe) -eq "$Which $exe") "$Case : $exe is the $Which one (it is '$(Get-ProgramText $exe)')"
    }
}

function Assert-Warned([string[]] $Warnings, [string] $Text, [string] $Case) {
    Assert (@($Warnings | Where-Object { $_ -like "*$Text*" }).Count -gt 0) "$Case : a warning says '$Text'"
}

# A run that left the service stopped turned its automatic start off, said
# so, and started nothing after that.
function Assert-AutoStartOff([string[]] $Warnings, [string] $Case) {
    Assert ($script:StartType -eq 'Disabled') "$Case : the start type is Disabled (it is $script:StartType)"
    Assert-Warned $Warnings 'Its automatic start was turned off' $Case
    Assert-Warned $Warnings 'the next run of this script that gets past its checks sets it back to Automatic' $Case
    $at = [array]::LastIndexOf([object[]]$script:Calls, 'starttype Disabled')
    Assert (($at -ge 0) -and (@($script:Calls | Select-Object -Skip ($at + 1)) -notcontains 'start')) "$Case : nothing started after the start type was set to Disabled (calls: $($script:Calls -join ','))"
}

# A run that started the service left its automatic start as it was.
function Assert-AutoStartKept([string[]] $Warnings, [string] $Case) {
    Assert ($script:StartType -eq 'Automatic') "$Case : the start type is still Automatic (it is $script:StartType)"
    Assert ($script:Calls -notcontains 'starttype Disabled') "$Case : the start type was not changed"
    Assert (@($Warnings | Where-Object { $_ -like '*automatic start*' }).Count -eq 0) "$Case : the automatic start is not mentioned"
}

function Assert-StateUntouched([string] $Case) {
    Assert ([IO.File]::ReadAllText((Join-Path $DataDir 'state\state.json')) -eq 'stored state') "$Case : the stored state is untouched"
}

function Assert-NoStaged([string] $Case) {
    foreach ($exe in $Executables) {
        Assert (-not (Test-Path -LiteralPath (Get-StagedPath $exe 'new'))) "$Case : $exe.new is removed"
        Assert (-not (Test-Path -LiteralPath (Get-StagedPath $exe 'old'))) "$Case : $exe.old is removed"
    }
    Assert (-not (Test-Path -LiteralPath (Get-StagedAccessPath))) "$Case : access.txt.new is removed"
    Assert (-not (Test-Path -LiteralPath (Get-RecordPath))) "$Case : upgrade.record is removed"
}

# A field of a result, $null when it or what holds it is missing: a check
# then fails instead of ending the tests.
function Get-Field($Table, [string] $Key) {
    if (($Table -is [hashtable]) -and $Table.ContainsKey($Key)) { return $Table[$Key] }
    return $null
}

function Get-StagedText([string] $Exe, [string] $Kind) {
    $path = Get-StagedPath $Exe $Kind
    if (-not (Test-Path -LiteralPath $path)) { return '<none>' }
    return [IO.File]::ReadAllText($path)
}

# The record's "started" line, '<none>' without one.
function Get-RecordStarted {
    $path = Get-RecordPath
    if (-not (Test-Path -LiteralPath $path)) { return '<none>' }
    $lines = @([IO.File]::ReadAllLines($path) | Where-Object { $_ -like 'started *' })
    if ($lines.Count -ne 1) { return '<none>' }
    return $lines[0]
}

# Every start so far was safe: the new set only with upgrade.record saying
# "started yes", and the previous set never once the new one had started.
function Test-StartsSafe {
    $newRan = $false
    foreach ($entry in $script:StartLog) {
        $watchdog, $started = $entry -split '\|', 2
        if ($watchdog -eq 'new wgal-watchdog.exe') {
            if ($started -ne 'started yes') { return $false }
            $newRan = $true
        } elseif ($newRan) {
            return $false
        }
    }
    return $true
}

try {
    # --- Before the stop ---------------------------------------------------------
    Reset-Fixture
    $set = Save-ProgramSet $Source
    foreach ($exe in $Executables) {
        Assert ([IO.File]::ReadAllText((Get-StagedPath $exe 'new')) -eq "new $exe") "staging: $exe.new holds the new one"
        Assert ([IO.File]::ReadAllText((Get-StagedPath $exe 'old')) -eq "old $exe") "staging: $exe.old holds the old one"
        Assert ($null -ne $set.Old[$exe]) "staging: $exe has an old hash"
    }
    Assert-Programs 'old' 'staging'
    Assert ((Get-ProgramSetInPlace $set) -eq 'old') 'staging: the set in place is the old one'
    # A second run replaces what an interrupted one left.
    [IO.File]::WriteAllText((Get-StagedPath 'wgal-mcp.exe' 'new'), 'left over')
    $set = Save-ProgramSet $Source 6>$null
    Assert ([IO.File]::ReadAllText((Get-StagedPath 'wgal-mcp.exe' 'new')) -eq 'new wgal-mcp.exe') 'staging: a left-over .new is replaced'
    $staged = New-StagedAccessList 'S-1-5-21-1000000000-2000000000-3000000000-1001' @('office', 'home')
    $text = [IO.File]::ReadAllText($staged)
    Assert ($text -match "user S-1-5-21-1000000000-2000000000-3000000000-1001`r`ntunnel office`r`ntunnel home`r`n$") 'staging: access.txt.new holds the whole list'
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -eq "old list`r`n") 'staging: access.txt is not touched before the stop'

    # --- The whole window works --------------------------------------------------
    Reset-Fixture
    $run = Invoke-Window
    Assert $run.Result.Done 'success: done'
    Assert ($null -eq $run.Result.Outcome) 'success: no recovery'
    Assert (($script:Calls -join ',') -eq 'stop,start') "success: stopped once, then started once inside the window (calls: $($script:Calls -join ','))"
    Assert ((Get-Field (Get-Field $run.Result 'Started') 'Running') -eq $true) 'success: the window reports the started service'
    Assert-Programs 'new' 'success'
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') 'success: the new access list is in place'
    Assert (-not (Test-Path -LiteralPath (Get-StagedAccessPath))) 'success: access.txt.new was moved'
    Assert (@(Get-ChildItem -LiteralPath (Join-Path $DataDir 'backup') -Directory).Count -eq 1) 'success: the state was backed up'
    Assert ($run.Warnings.Count -eq 0) 'success: no warning'
    Assert-AutoStartKept $run.Warnings 'success'
    $script:Calls = @()
    try { Enable-WgalAutoStart 6>$null } catch { $script:Calls += "error: $($_.Exception.Message)" }
    Assert ($script:Calls.Count -eq 0) "success: an Automatic start type is left as it is (calls: $($script:Calls -join ','))"

    # --- Each step fails in turn, before any executable is replaced ---------------
    foreach ($case in @(
            @{ Fail = 'stop'; Step = 'stopping the service' },
            @{ Fail = 'backup'; Step = 'backing up the stored state' },
            @{ Fail = 'access'; Step = 'putting the new access list in place' })) {
        Reset-Fixture
        $script:Fail = $case.Fail
        $run = Invoke-Window
        $name = "fail at $($case.Fail)"
        Assert (-not $run.Result.Done) "$name : not done"
        Assert ($run.Result.Step -eq $case.Step) "$name : the step is '$($case.Step)' (it is '$($run.Result.Step)')"
        Assert ($run.Result.Outcome.Set -eq 'old') "$name : the old set is in place"
        Assert $run.Result.Outcome.Running "$name : it runs again"
        Assert ((@($script:Calls | Where-Object { $_ -eq 'start' })).Count -eq 1) "$name : started once"
        Assert-Programs 'old' $name
        Assert ([IO.File]::ReadAllText((Get-AccessPath)) -eq "old list`r`n") "$name : the previous access list is unchanged"
        Assert-Warned $run.Warnings "The upgrade failed while $($case.Step)" $name
        Assert-Warned $run.Warnings 'previous access list is still in effect' $name
        Assert-Warned $run.Warnings "previous version's executables are in place" $name
        Assert-AutoStartKept $run.Warnings $name
        Assert-StateUntouched $name
        Assert-NoStaged $name
    }

    # --- access.txt replaced, then a failure: the new list stays -----------------
    Reset-Fixture
    $script:Fail = 'access-owner'
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'old') 'access replaced, then failure: the old executables run'
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') 'access replaced, then failure: the new list is kept'
    Assert-Warned $run.Warnings 'The new access.txt is in place; it is kept' 'access replaced, then failure'
    Assert-Programs 'old' 'access replaced, then failure'

    # --- Replacing the executables fails at each one -----------------------------
    foreach ($at in 1..$Executables.Count) {
        foreach ($how in @('before', 'partial')) {
            Reset-Fixture
            $script:FailCopyAt = $at
            $script:FailCopyHow = $how
            $run = Invoke-Window
            $name = "copy $at fails ($how)"
            Assert ($run.Result.Step -eq 'replacing the executables') "$name : the step"
            Assert ($run.Result.Outcome.Set -eq 'old') "$name : the old set is back in place"
            Assert $run.Result.Outcome.Running "$name : it runs again"
            Assert-Programs 'old' $name
            Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') "$name : the new list is kept"
            if (($at -gt 1) -or ($how -eq 'partial')) {
                Assert-Warned $run.Warnings 'copying the previous ones back' $name
            }
            Assert-StateUntouched $name
            Assert-NoStaged $name
        }
    }

    # --- All copied, then a failure: the new set, not yet run, starts ------------
    Reset-Fixture
    $script:FailCopyAt = $Executables.Count
    $script:FailCopyHow = 'after'
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'new') 'all copied, then failure: the new set is in place'
    Assert $run.Result.Outcome.Running 'all copied, then failure: it runs'
    Assert-Programs 'new' 'all copied, then failure'
    Assert-Warned $run.Warnings "new version's executables are in place, complete; it has not run yet" 'all copied, then failure'

    # --- The start itself fails (P2-1): the new set may have run, so it is
    # started again, never replaced by the previous one ---------------------------
    Reset-Fixture
    $script:StartThrows = $true
    $run = Invoke-Window
    $name = 'start fails'
    Assert (-not $run.Result.Done) "$name : not done"
    Assert ($run.Result.Step -eq 'starting the new version') "$name : the step is 'starting the new version' (it is '$($run.Result.Step)')"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Set') -eq 'new') "$name : the new set is in place"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $true) "$name : it runs"
    Assert (($script:Calls -join ',') -eq 'stop,start,start') "$name : started again by the recovery (calls: $($script:Calls -join ','))"
    Assert-Programs 'new' $name
    Assert-Warned $run.Warnings 'it may already have run' $name
    Assert-Warned $run.Warnings 'the previous version is not put back' $name
    Assert (@($run.Warnings | Where-Object { $_ -like '*copying the previous ones back*' }).Count -eq 0) "$name : nothing is copied back"
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') "$name : the new list is kept"
    Assert-AutoStartKept $run.Warnings $name
    Assert-StateUntouched $name
    Assert-NoStaged $name

    # --- The new set starts but does not run: reported, not rolled back --------
    Reset-Fixture
    $script:StartRuns = $false
    $run = Invoke-Window
    Assert $run.Result.Done 'new set does not run: the window is done'
    Assert ($null -eq $run.Result.Outcome) 'new set does not run: no recovery'
    Assert ((Get-Field (Get-Field $run.Result 'Started') 'Running') -eq $false) 'new set does not run: reported as not running'
    Assert-Programs 'new' 'new set does not run'

    # --- The previous version had no wgal-mcp.exe, the other two are the same
    # in both, and the new one is half copied (P2-3): not the old set ----------
    Reset-Fixture -NoOldMcp
    foreach ($exe in @('wgal-watchdog.exe', 'wgal-desktop.exe')) {
        [IO.File]::WriteAllText((Join-Path $ProgramDir $exe), "new $exe")
    }
    $script:FailCopyAt = 3
    $script:FailCopyHow = 'partial'
    $run = Invoke-Window
    $name = 'no old wgal-mcp.exe, same others, half-copied one'
    Assert ($run.Result.Outcome.Set -eq 'old') "$name : the old set is in place (it is '$($run.Result.Outcome.Set)')"
    Assert ((Get-ProgramText 'wgal-mcp.exe') -eq '<none>') "$name : the half-copied wgal-mcp.exe is removed (it is '$(Get-ProgramText 'wgal-mcp.exe')')"
    Assert-Warned $run.Warnings 'copying the previous ones back' $name
    Assert $run.Result.Outcome.Running "$name : it runs"

    # --- Neither set can be made: not started --------------------------------------
    Reset-Fixture
    $script:FailCopyAt = 2
    $script:FailCopyHow = 'partial'
    $script:FailRestore = $true
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'mixed') 'no complete set: mixed'
    Assert (-not $run.Result.Outcome.Running) 'no complete set: not running'
    Assert ((@($script:Calls | Where-Object { $_ -eq 'start' })).Count -eq 0) 'no complete set: never started'
    Assert-Warned $run.Warnings 'was NOT started' 'no complete set'
    foreach ($exe in $Executables) {
        Assert (Test-Path -LiteralPath (Get-StagedPath $exe 'old')) "no complete set: $exe.old is kept"
        Assert (Test-Path -LiteralPath (Get-StagedPath $exe 'new')) "no complete set: $exe.new is kept"
    }
    Assert (Test-Path -LiteralPath (Get-RecordPath)) 'no complete set: upgrade.record is kept'
    Assert-Warned $run.Warnings 'until then it refuses to run' 'no complete set'
    Assert-AutoStartOff $run.Warnings 'no complete set'

    # --- ... then a rerun, whose staging would fail again: it refuses before
    # removing a copy (P2-2), and never stops the service ------------------------
    $mixed = @{}
    foreach ($exe in $Executables) { $mixed[$exe] = Get-ProgramText $exe }
    $script:FailRestore = $false
    $script:FailStage = $true
    $script:Calls = @()
    $rerunError = ''
    try { [void](Save-ProgramSet $Source 6>$null) } catch { $rerunError = $_.Exception.Message }
    Assert ($rerunError -like '*An earlier run of this script did not finish*') "rerun after a failed recovery: refused (error: '$rerunError')"
    Assert ($rerunError -like '*Nothing was changed*') 'rerun after a failed recovery: says nothing was changed'
    foreach ($exe in $Executables) {
        Assert ((Get-StagedText $exe 'old') -eq "old $exe") "rerun after a failed recovery: $exe.old still holds the previous one"
        Assert ((Get-StagedText $exe 'new') -eq "new $exe") "rerun after a failed recovery: $exe.new still holds the new one"
        Assert ((Get-ProgramText $exe) -eq $mixed[$exe]) "rerun after a failed recovery: $exe in place is untouched"
    }
    Assert (Test-Path -LiteralPath (Get-RecordPath)) 'rerun after a failed recovery: upgrade.record is kept'
    Assert ($script:Calls.Count -eq 0) 'rerun after a failed recovery: the service was not touched'
    Assert ($script:StartType -eq 'Disabled') 'rerun after a failed recovery: its automatic start stays off'

    # --- ... and once a complete set is put back by hand, a rerun goes on ------
    $script:FailStage = $false
    foreach ($exe in $Executables) {
        Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Get-StagedPath $exe 'old') -Destination (Join-Path $ProgramDir $exe) -Force
    }
    $set = Save-ProgramSet $Source 6>$null
    Assert ((Get-ProgramSetInPlace $set) -eq 'old') 'rerun with the previous set put back: staged, the set in place is the old one'
    Assert ((Get-StagedText 'wgal-mcp.exe' 'new') -eq 'new wgal-mcp.exe') 'rerun with the previous set put back: the new copies are made again'

    # --- A record this script did not write: refused ---------------------------
    [IO.File]::WriteAllText((Get-RecordPath), "wgal-watchdog.exe something else`r`n")
    $rerunError = ''
    try { [void](Save-ProgramSet $Source 6>$null) } catch { $rerunError = $_.Exception.Message }
    Assert ($rerunError -like '*is not a record this script wrote*') "a foreign upgrade.record: refused (error: '$rerunError')"
    Assert ((Get-StagedText 'wgal-watchdog.exe' 'old') -eq 'old wgal-watchdog.exe') 'a foreign upgrade.record: the copies are kept'

    # --- Staging fails: copies without a record, which a rerun replaces --------
    Reset-Fixture
    $script:FailStage = $true
    $stageError = ''
    try { [void](Save-ProgramSet $Source) } catch { $stageError = $_.Exception.Message }
    Assert ($stageError -like '*staging a new executable failed*') 'staging fails: the error is passed on'
    Assert (-not (Test-Path -LiteralPath (Get-RecordPath))) 'staging fails: no upgrade.record'
    $script:FailStage = $false
    $set = Save-ProgramSet $Source
    Assert ((Get-ProgramSetInPlace $set) -eq 'old') 'staging failed before: a rerun stages again'
    Assert (Test-Path -LiteralPath (Get-RecordPath)) 'staging failed before: the rerun writes upgrade.record'

    # --- The new set started, then an executable of it is found damaged
    # (P2-6): the previous version is not put back, and the message says to
    # restore the new one -------------------------------------------------------
    function Get-StartCount { return @($script:Calls | Where-Object { $_ -eq 'start' }).Count }
    function Copy-SetBack([string] $Kind) {
        foreach ($exe in $Executables) {
            Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Get-StagedPath $exe $Kind) -Destination (Join-Path $ProgramDir $exe) -Force
        }
    }
    function Invoke-StartBreaks {
        Reset-Fixture
        $script:StartBreaks = $true
        $result = Invoke-Window
        $script:StartBreaks = $false
        return $result
    }
    $oldOption = [regex]::Escape('copy each *.exe.old over its executable')

    $run = Invoke-StartBreaks
    $name = 'new set damaged after its start'
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Set') -eq 'mixed') "$name : mixed"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $false) "$name : not running"
    Assert ((Get-StartCount) -eq 1) "$name : not started again (calls: $($script:Calls -join ','))"
    Assert-Warned $run.Warnings 'was NOT started' $name
    Assert-Warned $run.Warnings 'may already have run' $name
    Assert-Warned $run.Warnings 'do not put the previous version back' $name
    Assert-Warned $run.Warnings 'put the complete new version in place' $name
    Assert (@($run.Warnings | Where-Object { $_ -match $oldOption }).Count -eq 0) "$name : does not say to copy the *.exe.old copies back"
    foreach ($exe in $Executables) {
        Assert ((Get-StagedText $exe 'old') -eq "old $exe") "$name : $exe.old is kept"
    }
    Assert ((Get-RecordStarted) -eq 'started yes') "$name : upgrade.record says it started (it says '$(Get-RecordStarted)')"
    Assert-AutoStartOff $run.Warnings $name
    Assert-StateUntouched $name

    # --- ... then a rerun: refused, and it does not say to copy the previous
    # version back --------------------------------------------------------------
    $script:Calls = @()
    $rerunError = ''
    try { [void](Save-ProgramSet $Source 6>$null) } catch { $rerunError = $_.Exception.Message }
    $name = 'rerun after the damaged new set'
    Assert ($rerunError -like '*An earlier run of this script did not finish*') "$name : refused (error: '$rerunError')"
    Assert ($rerunError -like '*may already have run*') "$name : says the new version may have run"
    Assert ($rerunError -like '*put the complete new version in place*') "$name : says how to restore the new version"
    Assert ($rerunError -notmatch $oldOption) "$name : does not say to copy the *.exe.old copies back"
    foreach ($exe in $Executables) {
        Assert ((Get-StagedText $exe 'old') -eq "old $exe") "$name : $exe.old is kept"
    }
    Assert ($script:Calls.Count -eq 0) "$name : the service was not touched"
    Assert ($script:StartType -eq 'Disabled') "$name : its automatic start stays off"

    # --- ... the previous version copied back anyway, then a rerun that fails
    # before the new set is in place: the previous version is not started on
    # the state the new one may have changed --------------------------------------
    Copy-SetBack 'old'
    foreach ($attempt in @('rerun', 'second rerun')) {
        $script:Fail = 'backup'
        $script:Calls = @()
        $run = Invoke-Window
        $name = "previous version put back, $attempt fails"
        Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $false) "$name : not running"
        Assert ((Get-StartCount) -eq 0) "$name : the previous version is never started (calls: $($script:Calls -join ','))"
        Assert-Warned $run.Warnings 'was NOT started' $name
        Assert-Warned $run.Warnings 'may already have run' $name
        Assert-Programs 'old' $name
        Assert ((Get-RecordStarted) -eq 'started yes') "$name : upgrade.record still says it started"
        Assert-AutoStartOff $run.Warnings $name
        Assert-StateUntouched $name
    }

    # --- The complete new set put in place as the message says, then a rerun
    # that fails before the new set is in place: the new one is started ------
    [void](Invoke-StartBreaks)
    Copy-SetBack 'new'
    $script:Fail = 'backup'
    $script:Calls = @()
    $run = Invoke-Window -AsMain
    $name = 'new version put back, rerun fails'
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Set') -eq 'new') "$name : the new set is in place"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $true) "$name : it runs"
    Assert ((Get-StartCount) -eq 1) "$name : started once"
    Assert ($script:EnableError -eq '') "$name : the start type is set back without an error ($script:EnableError)"
    Assert ($script:StartType -eq 'Automatic') "$name : its automatic start is on again (it is $script:StartType)"
    Assert (($script:Calls -join ',') -eq 'starttype Automatic,stop,start') "$name : set back to Automatic before the stop (calls: $($script:Calls -join ','))"
    Assert-Programs 'new' $name
    Assert-Warned $run.Warnings 'may already have run' $name
    Assert-NoStaged $name

    # --- A record without its "started" line cannot tell: counted as started,
    # so a rerun that fails before the new set is in place does not start the
    # previous one -------------------------------------------------------------
    Reset-Fixture
    [void](Save-ProgramSet $Source)
    [IO.File]::WriteAllLines((Get-RecordPath), [string[]]@([IO.File]::ReadAllLines((Get-RecordPath)) | Where-Object { $_ -notlike 'started *' }))
    $script:Fail = 'backup'
    $run = Invoke-Window
    $name = 'record without its started line'
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $false) "$name : not running"
    Assert ((Get-StartCount) -eq 0) "$name : the previous version is never started"
    Assert-Warned $run.Warnings 'may already have run' $name
    Assert-Programs 'old' $name
    Assert-AutoStartOff $run.Warnings $name

    # --- upgrade.record cannot be marked "started yes" (P2-7), in the window
    # and again in the recovery: the new set is not started, everything is
    # kept, and the message says this run did not start it ------------------
    Reset-Fixture
    $run = Invoke-Window -LockRecord
    $name = 'record cannot be marked'
    Assert (-not $run.Result.Done) "$name : not done"
    Assert ($run.Result.Step -eq 'starting the new version') "$name : the step is 'starting the new version' (it is '$($run.Result.Step)')"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Set') -eq 'new') "$name : the new set is in place"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $false) "$name : not running"
    Assert ((Get-StartCount) -eq 0) "$name : never started (starts: $($script:StartLog -join ', '))"
    Assert (Test-StartsSafe) "$name : no start without the mark (starts: $($script:StartLog -join ', '))"
    Assert-Warned $run.Warnings 'was NOT started' $name
    Assert-Warned $run.Warnings 'could not be marked' $name
    Assert-Warned $run.Warnings 'This run did not start the new version' $name
    Assert-Warned $run.Warnings 'Leave the new version in place' $name
    Assert (@($run.Warnings | Where-Object { $_ -like '*Starting WGALWatchdog*' }).Count -eq 0) "$name : does not say it starts the service"
    Assert (@($run.Warnings | Where-Object { $_ -match $oldOption }).Count -eq 0) "$name : does not say to copy the *.exe.old copies back"
    Assert-Programs 'new' $name
    foreach ($exe in $Executables) {
        Assert ((Get-StagedText $exe 'old') -eq "old $exe") "$name : $exe.old is kept"
        Assert ((Get-StagedText $exe 'new') -eq "new $exe") "$name : $exe.new is kept"
    }
    Assert ((Get-RecordStarted) -eq 'started no') "$name : upgrade.record still says 'started no' (it says '$(Get-RecordStarted)')"
    Assert-AutoStartOff $run.Warnings $name
    Assert-StateUntouched $name

    # --- ... then the previous version put back by hand (the new one never
    # ran) and a rerun that fails before the new set is in place: the
    # previous one may start, as the new one was never started ---------------
    foreach ($exe in $Executables) {
        [IO.File]::WriteAllText((Join-Path $ProgramDir $exe), "old $exe")
    }
    $script:Fail = 'backup'
    $script:Calls = @()
    $run = Invoke-Window -AsMain
    $name = 'record could not be marked, previous version put back, rerun fails'
    Assert ($script:StartType -eq 'Automatic') "$name : its automatic start is on again (it is $script:StartType)"
    Assert (($script:Calls -join ',') -eq 'starttype Automatic,stop,start') "$name : set back to Automatic before the stop (calls: $($script:Calls -join ','))"
    Assert (Test-StartsSafe) "$name : no start of the previous version after one of the new (starts: $($script:StartLog -join ', '))"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Set') -eq 'old') "$name : the previous set is in place"
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $true) "$name : it runs"
    Assert-StateUntouched $name

    # --- ... and the same failure, then a rerun that works: started only with
    # the mark ----------------------------------------------------------------
    Reset-Fixture
    [void](Invoke-Window -LockRecord)
    # The first run backed up the state in this same second.
    Remove-Item -LiteralPath (Join-Path $DataDir 'backup') -Recurse -Force
    Assert ($script:StartType -eq 'Disabled') 'record could not be marked: its automatic start is off before the rerun'
    $script:Calls = @()
    $script:StartLog = @()
    $run = Invoke-Window -AsMain
    $name = 'record could not be marked, then a rerun'
    Assert $run.Result.Done "$name : done"
    Assert ($script:EnableError -eq '') "$name : the start type is set back without an error ($script:EnableError)"
    Assert ($script:StartType -eq 'Automatic') "$name : its automatic start is on again (it is $script:StartType)"
    Assert (($script:Calls -join ',') -eq 'starttype Automatic,stop,start') "$name : set back to Automatic before the stop, then started (calls: $($script:Calls -join ','))"
    Assert ((Get-StartCount) -eq 1) "$name : started once (starts: $($script:StartLog -join ', '))"
    Assert (Test-StartsSafe) "$name : no start without the mark (starts: $($script:StartLog -join ', '))"
    Assert-Programs 'new' $name

    # --- Codex review of FIX-05, autostart, P2-01: the rerun accepts the set
    # in place, but the recovery actions fail before the window. The service
    # a run refused to start stays Disabled: it is neither stopped nor
    # started, and Windows does not start it at the next boot ----------------
    Reset-Fixture
    [void](Invoke-Window -LockRecord)
    Remove-Item -LiteralPath (Join-Path $DataDir 'backup') -Recurse -Force
    Assert ($script:StartType -eq 'Disabled') 'recovery actions fail: its automatic start is off before the rerun'
    $script:Calls = @()
    $script:StartLog = @()
    $script:FailRecoveryActions = $true
    $run = Invoke-Window -AsMain
    $script:FailRecoveryActions = $false
    $name = 'recovery actions fail on a rerun'
    Assert ($script:EnableError -like '*sc.exe failure*') "$name : the run fails before the window ($script:EnableError)"
    Assert ($script:StartType -eq 'Disabled') "$name : its automatic start stays off (it is $script:StartType)"
    Assert ($script:Calls.Count -eq 0) "$name : nothing set, stopped or started (calls: $($script:Calls -join ','))"
    Assert ((Get-StartCount) -eq 0) "$name : never started"
    # Once they can be set, the rerun sets them first, then the start type.
    $script:RecoveryActionsSet = 0
    $run = Invoke-Window -AsMain
    Assert ($script:RecoveryActionsSet -eq 1) "$name, then fixed : the recovery actions are set"
    Assert ($script:StartType -eq 'Automatic') "$name, then fixed : its automatic start is on again"
    Assert $run.Result.Done "$name, then fixed : done"

    # --- The automatic start cannot be turned off: said plainly, with what to
    # do before a restart; nothing is started -------------------------------------
    foreach ($how in @('throw', 'ignored')) {
        Reset-Fixture
        $script:FailDisable = $how
        if ($how -eq 'throw') {
            $run = Invoke-Window -LockRecord
        } else {
            $script:FailCopyAt = 2
            $script:FailCopyHow = 'partial'
            $script:FailRestore = $true
            $run = Invoke-Window
        }
        $name = "turning the automatic start off fails ($how)"
        Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $false) "$name : not running"
        Assert ((Get-StartCount) -eq 0) "$name : never started"
        Assert ($script:Calls -contains 'starttype Disabled') "$name : it was tried (calls: $($script:Calls -join ','))"
        Assert-Warned $run.Warnings 'was NOT started' $name
        Assert-Warned $run.Warnings 'Its automatic start could NOT be turned off' $name
        Assert-Warned $run.Warnings 'Windows may start WGALWatchdog' $name
        Assert-Warned $run.Warnings 'before you restart this computer' $name
        Assert (@($run.Warnings | Where-Object { $_ -like '*automatic start was turned off*' }).Count -eq 0) "$name : does not say it was turned off"
        if ($how -eq 'ignored') {
            Assert-Warned $run.Warnings 'its start type reads back as Automatic' $name
        }
    }

    # --- The window's mark fails once, the recovery's works: started, with
    # the mark, and said not to have run ----------------------------------------
    Reset-Fixture
    $script:FailMarkWrites = 1
    $run = Invoke-Window
    $name = 'first mark fails, the recovery marks it'
    Assert ((Get-Field (Get-Field $run.Result 'Outcome') 'Running') -eq $true) "$name : it runs"
    Assert ((Get-StartCount) -eq 1) "$name : started once (starts: $($script:StartLog -join ', '))"
    Assert (Test-StartsSafe) "$name : no start without the mark (starts: $($script:StartLog -join ', '))"
    Assert-Warned $run.Warnings "new version's executables are in place, complete; it has not run yet" $name
    Assert-NoStaged $name

    # --- The recovered set does not start: reported, not hidden ----------------
    Reset-Fixture
    $script:Fail = 'backup'
    $script:StartRuns = $false
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'old') 'recovered set does not start: old set'
    Assert (-not $run.Result.Outcome.Running) 'recovered set does not start: not running'

    # --- Stopped as Ctrl+C stops a script (PipelineStoppedException, which no
    # catch sees): in a child PowerShell, since it ends the whole script ----
    $marker = Join-Path $Root 'started.txt'
    $window = @"
`$set = Save-ProgramSet '$Source'
[void](New-StagedAccessList 'S-1-5-21-1000000000-2000000000-3000000000-1001' @('office'))
[void](Invoke-UpgradeWindow `$set `$true)
[IO.File]::AppendAllText('$marker', 'went on after the interruption')
"@
    Reset-Fixture
    $output = Invoke-Child @"
function Stop-WgalService { throw (New-Object System.Management.Automation.PipelineStoppedException) }
function Start-WgalChecked {
    [IO.File]::AppendAllText('$marker', 'started')
    return @{ Running = `$true; PipeOwner = `$LocalSystem }
}
"@ $window
    Assert ((Test-Path -LiteralPath $marker) -and ([IO.File]::ReadAllText($marker) -eq 'started')) 'interrupted: the finally block started the service, and the script ended there'
    Assert (@($output | Where-Object { $_ -like '*The upgrade failed while stopping the service: interrupted*' }).Count -gt 0) 'interrupted: the warning says so'
    Assert-Programs 'old' 'interrupted'
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -eq "old list`r`n") 'interrupted: the previous access list is unchanged'

    # --- Ctrl+C while the new set starts (P2-1): the finally block starts the
    # new set again and the script ends there; nothing is put back ---------------
    Reset-Fixture
    $output = Invoke-Child @"
function Stop-WgalService { }
`$script:Starts = 0
function Start-WgalChecked {
    `$script:Starts += 1
    if (`$script:Starts -eq 1) { throw (New-Object System.Management.Automation.PipelineStoppedException) }
    [IO.File]::AppendAllText('$marker', 'started again')
    return @{ Running = `$true; PipeOwner = `$LocalSystem }
}
"@ $window
    $name = 'interrupted while starting'
    # The child's console wraps long lines, keeping the spaces: joined as written.
    $said = $output -join ''
    Assert ((Test-Path -LiteralPath $marker) -and ([IO.File]::ReadAllText($marker) -eq 'started again')) "$name : the finally block started the service again, and the script ended there"
    Assert ($said -like '*The upgrade failed while starting the new version: interrupted*') "$name : the warning names the step"
    Assert ($said -like '*it may already have run*') "$name : says the new version may have run"
    Assert ($said -notlike '*copying the previous ones back*') "$name : nothing is copied back"
    Assert-Programs 'new' $name
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') "$name : the new list is kept"
    Assert-StateUntouched $name

    # --- Without -Tunnel/-User: the access list is not mentioned or touched ----
    Reset-Fixture
    $script:Fail = 'backup'
    $run = Invoke-Window $false
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -eq "old list`r`n") 'no access list given: unchanged'
    Assert (@($run.Warnings | Where-Object { $_ -like '*access*' }).Count -eq 0) 'no access list given: no access warning'

    # --- The previous version had no wgal-mcp.exe ----------------------------
    Reset-Fixture -NoOldMcp
    $script:FailCopyAt = 3
    $script:FailCopyHow = 'partial'
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'old') 'no old wgal-mcp.exe: the old set is back'
    Assert ((Get-ProgramText 'wgal-mcp.exe') -eq '<none>') 'no old wgal-mcp.exe: the half-copied one is removed'
    Assert ((Get-ProgramText 'wgal-watchdog.exe') -eq 'old wgal-watchdog.exe') 'no old wgal-mcp.exe: the old watchdog is in place'

    # --- The previous version had no wgal.exe (before 3.0.0) -----------------
    Reset-Fixture -NoOldCli
    $run = Invoke-Window
    Assert $run.Result.Done 'no old wgal.exe: the window is done'
    Assert-Programs 'new' 'no old wgal.exe'
    Assert ((Get-ProgramText 'wgal.exe') -eq 'new wgal.exe') 'no old wgal.exe: it is in place'
    # ... and its copy into place fails halfway: the old set is back, without it.
    Reset-Fixture -NoOldCli
    $script:FailCopyAt = 4
    $script:FailCopyHow = 'partial'
    $run = Invoke-Window
    Assert ($run.Result.Outcome.Set -eq 'old') 'no old wgal.exe, its copy fails: the old set is back'
    Assert ((Get-ProgramText 'wgal.exe') -eq '<none>') 'no old wgal.exe, its copy fails: the half-copied one is removed'
    Assert ((Get-ProgramText 'wgal-mcp.exe') -eq 'old wgal-mcp.exe') 'no old wgal.exe, its copy fails: the old wgal-mcp.exe is in place'

    # --- A record the script of a version before 3.0.0 left (no wgal.exe line),
    # with that run's complete previous set in place: read, and replaced --------
    Reset-Fixture -NoOldCli
    $older = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe' | ForEach-Object {
            "$_ $('A' * 64) $(Get-FileSha256 (Join-Path $ProgramDir $_))"
        })
    [IO.File]::WriteAllLines((Get-RecordPath), [string[]]($older + 'started no'))
    $stageError = ''
    try { $set = Save-ProgramSet $Source 6>$null } catch { $stageError = $_.Exception.Message }
    Assert ($stageError -eq '') "an older script's record: staging goes on ($stageError)"
    Assert ([IO.File]::ReadAllText((Get-StagedPath 'wgal.exe' 'new')) -eq 'new wgal.exe') "an older script's record: wgal.exe is staged"
    # ... but one that lacks another executable is not this script's.
    Reset-Fixture -NoOldCli
    $other = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal.exe' | ForEach-Object { "$_ $('A' * 64) -" })
    [IO.File]::WriteAllLines((Get-RecordPath), [string[]]($other + 'started no'))
    $stageError = ''
    try { [void](Save-ProgramSet $Source 6>$null) } catch { $stageError = $_.Exception.Message }
    Assert ($stageError -like '*is not a record this script wrote*') "a record without wgal-mcp.exe: refused ($stageError)"

    # --- First install: the service is created Disabled, and a failure
    # before the record is marked leaves it so (Codex review of the CLI,
    # third round, R3-P2-2) ---------------------------------------------------
    Reset-Fixture
    foreach ($exe in $Executables) { Remove-Item -LiteralPath (Join-Path $ProgramDir $exe) }
    $set = Save-ProgramSet $Source 6>$null
    $script:FailRecoveryActions = $true
    $thrown = ''
    try { Install-FirstSet $set 'C:\x\wgal-watchdog.exe' $false 6>$null } catch { $thrown = $_.Exception.Message }
    $script:FailRecoveryActions = $false
    Assert ($thrown -like '*sc.exe failure*') "first install, recovery actions fail: the failure is passed on ($thrown)"
    Assert ($script:Calls -contains 'new-service Disabled') "first install: the service is created Disabled ($($script:Calls -join ', '))"
    Assert ($script:StartType -eq 'Disabled') "first install, recovery actions fail: it stays Disabled (it is $($script:StartType))"
    Assert (@($script:Calls | Where-Object { $_ -eq 'start' }).Count -eq 0) 'first install, recovery actions fail: never started'
    # ... and made Automatic only once the record says "started yes".
    Reset-Fixture
    foreach ($exe in $Executables) { Remove-Item -LiteralPath (Join-Path $ProgramDir $exe) }
    $set = Save-ProgramSet $Source 6>$null
    $script:AutomaticWhile = $null
    Install-FirstSet $set 'C:\x\wgal-watchdog.exe' $false 6>$null
    $started = Start-FirstInstall $set 3>$null 6>$null
    Assert $started.Running 'first install: it runs'
    Assert ($script:StartType -eq 'Automatic') "first install: it starts at boot once started (it is $($script:StartType))"
    Assert ($script:AutomaticWhile -eq 'started yes') "first install: made Automatic only with the record saying yes (it said $($script:AutomaticWhile))"

    # --- First install: started only once upgrade.record says "started yes"
    # (Codex review of the CLI, second round, N-P2-1) -------------------------
    Reset-Fixture
    foreach ($exe in $Executables) { Remove-Item -LiteralPath (Join-Path $ProgramDir $exe) }
    $set = Save-ProgramSet $Source 6>$null
    Install-StagedPrograms $set
    $started = Start-FirstInstall $set 3>$null 6>$null
    Assert $started.Running 'first install: it runs'
    Assert ((@($script:StartLog) -join ',') -ceq 'new wgal-watchdog.exe|started yes') "first install: started with the record saying yes ($($script:StartLog -join ', '))"
    # ... and when the record cannot be marked: not started, start type off.
    Reset-Fixture
    foreach ($exe in $Executables) { Remove-Item -LiteralPath (Join-Path $ProgramDir $exe) }
    $set = Save-ProgramSet $Source 6>$null
    Install-StagedPrograms $set
    $script:FailMarkWrites = 5
    $warned = @(Start-FirstInstall $set 3>&1 6>$null)
    $started = @($warned | Where-Object { $_ -is [hashtable] })[0]
    Assert (-not $started.Running) 'first install, record not marked: not running'
    Assert $started.ContainsKey('Unmarked') 'first install, record not marked: said so to the caller'
    Assert (@($script:Calls | Where-Object { $_ -eq 'start' }).Count -eq 0) 'first install, record not marked: never started'
    Assert ($script:StartType -eq 'Disabled') "first install, record not marked: its start type is Disabled (it is $($script:StartType))"
    Assert (@($warned | Where-Object { $_ -is [System.Management.Automation.WarningRecord] -and $_.Message -like '*was NOT started*' }).Count -eq 1) 'first install, record not marked: says it was not started'
    Assert (Test-Path -LiteralPath (Get-RecordPath)) 'first install, record not marked: the record is kept'

    # --- First install: access.txt is moved into place --------------------------
    Reset-Fixture
    Remove-Item -LiteralPath (Get-AccessPath)
    [void](New-StagedAccessList 'S-1-5-21-1000000000-2000000000-3000000000-1001' @('office'))
    Install-StagedAccessList
    Assert ([IO.File]::ReadAllText((Get-AccessPath)) -match 'tunnel office') 'first install: access.txt is in place'
    Assert (-not (Test-Path -LiteralPath (Get-StagedAccessPath))) 'first install: access.txt.new is gone'
} finally {
    if (Test-Path -LiteralPath $Root) { Remove-Item -LiteralPath $Root -Recurse -Force }
}

Write-Host "install-recovery: $Passed passed, $Failed failed"
if ($Failed -ne 0) { exit 1 }
exit 0
