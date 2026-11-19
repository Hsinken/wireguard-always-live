#Requires -Version 5.1
<#
Tests revert.ps1 and uninstall.ps1 where they wait for WGALWatchdog to stop
(Codex review of the CLI, second round, N-P2-3 and N-P2-4): revert.ps1
starts no tunnel while WGAL could still act on it, and uninstall.ps1 gives
up on a service that does not stop within 90 s instead of waiting with no
limit; and install.ps1's own stop and start, which wait at most 60 s and
30 s (third and fourth rounds, R3-P2-4 and R4-P2-3).

Run it as a normal user, from any folder:

  powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\revert-uninstall.tests.ps1

It loads the two scripts' functions (dot-sourcing runs none of their
steps) and replaces every service, process and clock command with a
stand-in defined here; nothing real is reached. The stand-ins exist only
in this file: the scripts have no parameter or variable that selects them.
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Stand-ins: services, processes and the clock ---------------------------
# Start-Service waits with no limit while a service stays StartPending, so
# none of the scripts may use it; and nothing real may be reached.
function Start-Service { $script:Calls += 'Start-Service'; throw 'test: Start-Service waits with no limit while a service stays StartPending' }
function Restart-Service { throw 'test: the real Restart-Service was reached' }
$script:Now = [datetime]'2026-01-01T00:00:00'
$script:Calls = @()
$script:Wgal = $null          # the WGALWatchdog service, or none
$script:WgalStops = $true     # whether a stop request stops it
$script:WgalPid = 0           # the process the service manager reports
$script:WgalPath = ''         # the service's registered program
$script:KillWorks = $true     # whether ending the process ends it
$script:ProcessLeft = $false  # a wgal-watchdog process is running
$script:Tunnels = @()
$script:ServiceQueryFails = $false  # Get-Service fails with another error
$script:CimFails = $false           # the CIM query fails
$script:ProcessQueryFails = $false  # Get-Process fails with another error
$script:StartTo = 'StartPending'    # what a service's Start() leaves it in
$script:PipeUp = $true              # whether the watchdog's pipe is there while it runs
$script:PendingLooks = 0            # WGALWatchdog runs after this many looks while StartPending

# A query that fails, as the cmdlet does: with -ErrorAction Stop (or none,
# under this file's $ErrorActionPreference) it throws an error that is not
# "not found"; with SilentlyContinue the error is not seen and nothing
# comes back.
function Stop-QueryFailed([string] $Id, $ErrorAction) {
    if (($ErrorAction -eq 'Stop') -or (($null -eq $ErrorAction) -and ($ErrorActionPreference -eq 'Stop'))) {
        throw (New-Object System.Management.Automation.ErrorRecord(
                (New-Object Exception 'test: the query failed'), $Id, 'NotSpecified', $null))
    }
}

# The error a cmdlet gives, with -ErrorAction Stop, when there is no such
# service or process.
function Stop-NotFound([string] $Id) {
    throw (New-Object System.Management.Automation.ErrorRecord(
            (New-Object Exception 'not found'), $Id, 'ObjectNotFound', $null))
}

function Get-Date { $script:Now }
function Start-Sleep {
    param([int] $Seconds = 0, [int] $Milliseconds = 0)
    $script:Now = $script:Now.AddSeconds($Seconds).AddMilliseconds($Milliseconds)
}

function New-FakeService([string] $Name, [string] $Status) {
    $service = [pscustomobject]@{ ServiceName = $Name; Status = $Status; StartType = 'Automatic' }
    $service | Add-Member -MemberType ScriptMethod -Name Start -Value {
        $script:Calls += "start $($this.ServiceName)"
        $this.Status = $script:StartTo
    }
    $service | Add-Member -MemberType ScriptMethod -Name WaitForStatus -Value {
        param($Wanted, $Limit)
        $this.Status = [string]$Wanted
    }
    $service | Add-Member -MemberType ScriptMethod -Name Refresh -Value { }
    return $service
}

function Get-Service {
    param([string[]] $Name, $ErrorAction)
    $found = @()
    foreach ($one in $Name) {
        if ($one -eq 'WGALWatchdog') {
            if ($script:ServiceQueryFails) {
                Stop-QueryFailed 'InvalidOperation,Microsoft.PowerShell.Commands.GetServiceCommand' $ErrorAction
                continue
            }
            if ($null -ne $script:Wgal) {
                if (($script:PendingLooks -gt 0) -and ($script:Wgal.Status -eq 'StartPending')) {
                    $script:PendingLooks -= 1
                    if ($script:PendingLooks -eq 0) { $script:Wgal.Status = 'Running' }
                }
                $found += $script:Wgal
            } elseif ($ErrorAction -eq 'Stop') {
                Stop-NotFound 'NoServiceFoundForGivenName,Microsoft.PowerShell.Commands.GetServiceCommand'
            }
        } elseif ($one.EndsWith('*')) {
            $found += $script:Tunnels
        } else {
            $found += @($script:Tunnels | Where-Object { $_.ServiceName -eq $one })
        }
    }
    return $found
}
function Set-Service {
    param([string] $Name, [string] $StartupType)
    $script:Calls += "starttype $Name $StartupType"
}
function Stop-Service {
    param([string] $Name, [switch] $NoWait)
    if (-not $NoWait) { throw 'test: Stop-Service without -NoWait waits with no limit' }
    $script:Calls += "stop $Name"
    if ($script:WgalStops) {
        $script:Wgal.Status = 'Stopped'
        $script:WgalPid = 0
        $script:ProcessLeft = $false
    } else {
        $script:Wgal.Status = 'StopPending'
    }
}
function Get-CimInstance {
    param($ClassName, $Filter, $ErrorAction)
    if ($script:CimFails) {
        Stop-QueryFailed 'HRESULT 0x80041001,Microsoft.Management.Infrastructure.CimCmdlets.GetCimInstanceCommand' $ErrorAction
        return
    }
    return [pscustomobject]@{ ProcessId = $script:WgalPid; PathName = $script:WgalPath }
}
function Stop-Process {
    param([int] $Id, [switch] $Force)
    $script:Calls += "kill $Id"
    if ($script:KillWorks) {
        $script:WgalPid = 0
        $script:ProcessLeft = $false
        $script:Wgal.Status = 'Stopped'
    }
}
function Wait-Process { param($Id, $Timeout, $ErrorAction) }
function Get-Process {
    param([string[]] $Name, [int] $Id, $ErrorAction)
    if ($script:ProcessQueryFails) {
        Stop-QueryFailed 'CouldNotEnumerateProcesses,Microsoft.PowerShell.Commands.GetProcessCommand' $ErrorAction
        return
    }
    if ($script:ProcessLeft -and (($Name -contains 'wgal-watchdog') -or ($Id -ne 0))) {
        return [pscustomobject]@{ Id = 4242; Name = 'wgal-watchdog' }
    }
    # As the cmdlet: nothing when there is no such process, and with
    # -ErrorAction Stop its not-found error.
    if ($ErrorAction -eq 'Stop') {
        Stop-NotFound 'NoProcessFoundForGivenName,Microsoft.PowerShell.Commands.GetProcessCommand'
    }
}
function sc.exe { $script:Calls += "sc $args"; $global:LASTEXITCODE = 0 }
function Format-Table { param([Parameter(ValueFromPipeline = $true)] $InputObject, [switch] $AutoSize) }

# install.ps1 first: its Stop-WgalService is kept before uninstall.ps1's,
# of the same name, replaces it.
. (Join-Path $PSScriptRoot '..\install.ps1')
$script:InstallStop = ${function:Stop-WgalService}
$script:InstallStart = ${function:Start-WgalChecked}
. (Join-Path $PSScriptRoot '..\revert.ps1') -Tunnel 'office'
. (Join-Path $PSScriptRoot '..\uninstall.ps1')

# install.ps1's pipe checks, on the fake service.
function Test-PipeExists { return ($script:PipeUp -and ($null -ne $script:Wgal) -and ($script:Wgal.Status -eq 'Running')) }
function Get-PipeOwnerSid([string] $Name) { return 'S-1-5-18' }

# revert.ps1's final check, where a failing query is injected
# ($FailAtCheck: service, cim or process); the stop before it goes through
# as usual.
$script:FailAtCheck = ''
$script:RevertWait = ${function:Wait-WgalStopped}
function Wait-WgalStopped([int] $Seconds) {
    switch ($script:FailAtCheck) {
        'service' { $script:ServiceQueryFails = $true }
        'cim' { $script:CimFails = $true }
        'process' { $script:ProcessQueryFails = $true }
    }
    return & $script:RevertWait $Seconds
}

$script:Passed = 0
$script:Failed = 0
function Assert([bool] $Ok, [string] $What) {
    if ($Ok) { $script:Passed++ } else { $script:Failed++; Write-Host "FAIL: $What" }
}

$expected = '"' + (Join-Path $ProgramDir 'wgal-watchdog.exe') + '"'
function Reset-Fixture {
    $script:Now = [datetime]'2026-01-01T00:00:00'
    $script:Calls = @()
    $script:Wgal = New-FakeService 'WGALWatchdog' 'Running'
    $script:WgalStops = $true
    $script:WgalPid = 4242
    $script:WgalPath = $expected
    $script:KillWorks = $true
    $script:ProcessLeft = $true
    $script:Tunnels = @(New-FakeService 'WireGuardTunnel$office' 'Stopped')
    $script:ServiceQueryFails = $false
    $script:CimFails = $false
    $script:ProcessQueryFails = $false
    $script:StartTo = 'StartPending'
    $script:PipeUp = $true
    $script:PendingLooks = 0
    $script:FailAtCheck = ''
}

function Invoke-Quietly([scriptblock] $Step) {
    $out = @(& $Step 3>&1 6>$null)
    $warnings = @($out | Where-Object { $_ -is [System.Management.Automation.WarningRecord] } |
            ForEach-Object { $_.Message })
    $result = @($out | Where-Object { $_ -isnot [System.Management.Automation.WarningRecord] })
    return @{ Result = $(if ($result.Count -gt 0) { $result[-1] } else { $null }); Count = $result.Count; Warnings = $warnings }
}

# --- revert: WGAL stops; then the tunnel is started --------------------------
Reset-Fixture
$run = Invoke-Quietly { Invoke-Revert }
Assert ($run.Result -eq 0) "revert, WGAL stops: exit 0 (got $($run.Result))"
Assert ($run.Count -eq 1) "revert, WGAL stops: one exit code and nothing else (got $($run.Count))"
Assert ($script:Calls -contains 'start WireGuardTunnel$office') 'revert, WGAL stops: the tunnel is started'
Assert ($script:Calls -contains 'starttype WGALWatchdog Disabled') 'revert, WGAL stops: WGAL is disabled first'

# --- revert: WGAL never stops and runs another program: not ended, and no
# tunnel is started ---------------------------------------------------------
Reset-Fixture
$script:WgalStops = $false
$script:WgalPath = '"C:\elsewhere\other.exe"'
$run = Invoke-Quietly { Invoke-Revert }
Assert ($run.Result -eq 1) "revert, WGAL not stopped: exit 1 (got $($run.Result))"
Assert ($run.Count -eq 1) "revert, WGAL not stopped: one exit code and nothing else (got $($run.Count))"
Assert (-not ($script:Calls -match '^start ')) "revert, WGAL not stopped: no tunnel started ($($script:Calls -join ', '))"
Assert (-not ($script:Calls -match '^kill ')) 'revert, WGAL not stopped: another program is not ended'
Assert (@($run.Warnings | Where-Object { $_ -like '*no tunnel was started*' }).Count -eq 1) 'revert, WGAL not stopped: says no tunnel was started'

# --- revert: its process ended, but one is still there: no tunnel ------------
Reset-Fixture
$script:WgalStops = $false
$script:KillWorks = $false
$run = Invoke-Quietly { Invoke-Revert }
Assert ($run.Result -eq 1) "revert, process not ended: exit 1 (got $($run.Result))"
Assert ($script:Calls -contains 'kill 4242') 'revert, process not ended: it was asked to end'
Assert (-not ($script:Calls -match '^start ')) 'revert, process not ended: no tunnel started'

# --- revert: its process ended: then the tunnel ------------------------------
Reset-Fixture
$script:WgalStops = $false
$run = Invoke-Quietly { Invoke-Revert }
Assert ($run.Result -eq 0) "revert, process ended: exit 0 (got $($run.Result))"
Assert ($script:Calls -contains 'start WireGuardTunnel$office') 'revert, process ended: the tunnel is started'

# --- uninstall: a service that never stops is given up on in time ------------
Reset-Fixture
$script:WgalStops = $false
$started = $script:Now
$thrown = ''
try { [void](Invoke-Quietly { Remove-WgalService }) } catch { $thrown = $_.Exception.Message }
Assert ($thrown -like '*did not stop within 90 s; nothing was removed*') "uninstall, not stopped: refused ($thrown)"
Assert ((($script:Now - $started).TotalSeconds) -le 92) "uninstall, not stopped: within 90 s ($((($script:Now - $started).TotalSeconds)) s)"
Assert (-not ($script:Calls -match '^sc ')) 'uninstall, not stopped: the service is not deleted'

# --- uninstall: a service that stops is deleted ------------------------------
Reset-Fixture
$script:ProcessLeft = $false
$thrown = ''
try { [void](Invoke-Quietly { Remove-WgalService }) } catch { $thrown = $_.Exception.Message }
Assert ($thrown -eq '') "uninstall, stopped: no error ($thrown)"
Assert ($script:Calls -contains 'stop WGALWatchdog') 'uninstall, stopped: asked without waiting'
Assert (@($script:Calls -match '^sc delete').Count -eq 1) "uninstall, stopped: deleted ($($script:Calls -join ', '))"

# --- revert: a query that fails is not an answer (Codex review of the CLI,
# third round, R3-P2-3, and its test, fourth round, R4-P2-2). WGAL stops;
# then, in the final check only, the service, CIM or process query fails
# as the cmdlet does (Stop-QueryFailed). The check that asked with
# SilentlyContinue read that as "none" and started the tunnel; this one
# must end with 1, starting none, and say why ----------------------------
foreach ($what in 'service', 'cim', 'process') {
    Reset-Fixture
    $script:FailAtCheck = $what
    $thrown = ''
    $run = $null
    try { $run = Invoke-Quietly { Invoke-Revert } } catch { $thrown = $_.Exception.Message }
    $script:FailAtCheck = ''
    $code = if ($null -ne $run) { $run.Result } else { '-' }
    Assert ($thrown -eq '') "revert, the $what query fails: nothing thrown ($thrown)"
    Assert ($code -eq 1) "revert, the $what query fails: exit 1 (got $code)"
    Assert ($script:Calls -contains 'stop WGALWatchdog') "revert, the $what query fails: WGAL was asked to stop first"
    Assert (-not ($script:Calls -match '^start ')) "revert, the $what query fails: no tunnel started ($($script:Calls -join ', '))"
    Assert (($null -ne $run) -and (@($run.Warnings | Where-Object { $_ -like '*could not be confirmed stopped*' }).Count -eq 1)) "revert, the $what query fails: says WGAL could not be confirmed stopped"
}

# --- revert: no WGAL service at all, and no process: the tunnel ------------
Reset-Fixture
$script:Wgal = $null
$script:WgalPid = 0
$script:ProcessLeft = $false
$run = Invoke-Quietly { Invoke-Revert }
Assert ($run.Result -eq 0) "revert, no WGAL at all: exit 0 (got $($run.Result))"
Assert ($script:Calls -contains 'start WireGuardTunnel$office') 'revert, no WGAL at all: the tunnel is started'

# --- install.ps1's stop has a limit too (R3-P2-4) ----------------------------
Reset-Fixture
$script:WgalStops = $false
$started = $script:Now
$thrown = ''
try { [void](& $script:InstallStop 6>$null) } catch { $thrown = $_.Exception.Message }
Assert ($thrown -like '*did not stop within 60 s*') "install's stop, not stopped: refused ($thrown)"
Assert ((($script:Now - $started).TotalSeconds) -le 62) "install's stop, not stopped: within 60 s ($((($script:Now - $started).TotalSeconds)) s)"
Reset-Fixture
$script:ProcessLeft = $false
$thrown = ''
try { [void](& $script:InstallStop 6>$null) } catch { $thrown = $_.Exception.Message }
Assert ($thrown -eq '') "install's stop, stopped: no error ($thrown)"
Assert ($script:Calls -contains 'stop WGALWatchdog') "install's stop: asked without waiting"

# --- install.ps1's start has a limit too (R4-P2-3): asked with Start(),
# never Start-Service, and a service that stays StartPending is given up
# on within 30 s ------------------------------------------------------------
Reset-Fixture
$script:Wgal.Status = 'Stopped'
$started = $script:Now
$run = Invoke-Quietly { & $script:InstallStart }
Assert (($null -ne $run.Result) -and (-not $run.Result.Running)) "install's start, stays StartPending: not running"
Assert ($script:Calls -contains 'start WGALWatchdog') "install's start: asked with Start() ($($script:Calls -join ', '))"
Assert ($script:Calls -notcontains 'Start-Service') "install's start: never with Start-Service ($($script:Calls -join ', '))"
Assert ($run.Warnings.Count -eq 0) "install's start, stays StartPending: no failure reported ($($run.Warnings -join ' | '))"
Assert ((($script:Now - $started).TotalSeconds) -le 31) "install's start, stays StartPending: within 30 s ($((($script:Now - $started).TotalSeconds)) s)"
# ... one that comes up with its pipe runs, and the pipe's owner is read.
Reset-Fixture
$script:Wgal.Status = 'Stopped'
$script:StartTo = 'Running'
$run = Invoke-Quietly { & $script:InstallStart }
Assert ($run.Result.Running) "install's start, comes up: running"
Assert ($run.Result.PipeOwner -eq 'S-1-5-18') "install's start, comes up: the pipe's owner is read ($($run.Result.PipeOwner))"
# ... one that stays StartPending for 10 looks (about 5 s) and then runs
# is waited for, not given up on at the first look (Codex review of the
# CLI, fifth round, P3).
Reset-Fixture
$script:Wgal.Status = 'Stopped'
$script:PendingLooks = 10
$started = $script:Now
$run = Invoke-Quietly { & $script:InstallStart }
$took = ($script:Now - $started).TotalSeconds
Assert ($run.Result.Running) "install's start, runs after a while: running"
Assert (($took -ge 4) -and ($took -le 31)) "install's start, runs after a while: waited for it ($took s)"
Assert ($run.Warnings.Count -eq 0) "install's start, runs after a while: no failure reported ($($run.Warnings -join ' | '))"
# ... one already running is not asked again; its pipe is checked.
Reset-Fixture
$run = Invoke-Quietly { & $script:InstallStart }
Assert ($run.Result.Running) "install's start, already running: running"
Assert ($script:Calls -notcontains 'start WGALWatchdog') "install's start, already running: not asked again ($($script:Calls -join ', '))"
# ... and one that runs without its pipe is not taken as running.
Reset-Fixture
$script:Wgal.Status = 'Stopped'
$script:StartTo = 'Running'
$script:PipeUp = $false
$run = Invoke-Quietly { & $script:InstallStart }
Assert (-not $run.Result.Running) "install's start, no pipe: not running"

Write-Host "revert-uninstall: $Passed passed, $Failed failed"
if ($Failed -ne 0) { exit 1 }
exit 0
