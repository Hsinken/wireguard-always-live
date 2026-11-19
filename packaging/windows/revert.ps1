#Requires -Version 5.1
<#
.SYNOPSIS
Takes WGAL out of the way without the GUI and brings a tunnel back. Run it
from an elevated PowerShell when anything about WGAL looks wrong.

.DESCRIPTION
It changes only two things: the WGALWatchdog service, and the start of the
tunnel services named with -Tunnel. In order:

  1. sets WGALWatchdog to Disabled, so neither a reboot nor the service
     manager's recovery actions bring it back;
  2. asks it to stop without waiting, then waits up to 45 s for it to stop,
     asking again while it is still starting (a stop already under way may
     first start a tunnel it stopped);
  3. if it is still not stopped, ends the process the service manager
     reports for it, only if the service's registered program is the
     installed wgal-watchdog.exe;
  4. checks that WGALWatchdog is stopped and no wgal-watchdog process is
     left, waiting up to 15 s more; if it cannot tell, it starts no tunnel
     (WGAL could still act on it) and ends with 1;
  5. for each -Tunnel: waits up to 60 s for its service to leave a pending
     state, asks it to start if it is Stopped, and waits up to 60 s for
     Running, never longer;
  6. prints the state of WGALWatchdog and of every WireGuard service.

It never stops or reconfigures a tunnel service, never changes a tunnel's
start type, and never touches WireGuardManager. WGAL's own data, including
whether automatic recovery is on, is left as it is.

To resume WGAL afterwards:
    Set-Service -Name WGALWatchdog -StartupType Automatic
    Start-Service -Name WGALWatchdog
(install.ps1 also sets it back to Automatic on an upgrade.) WGAL acts on
a tunnel as soon as its own settings say a fault is confirmed, which can
be right away (the probe timing is the user's, and the minimum fault time
may be 0): if automatic recovery should not act, turn it off for that
tunnel before resuming WGAL, or keep WGAL stopped.

.PARAMETER Tunnel
The tunnel names (as in the official WireGuard app) to bring back if they
are stopped. For several, give them as a list (-Tunnel a, b) or
comma-separated in one argument (-Tunnel a,b), which is how
`powershell.exe -File` and revert.cmd pass them.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]] $Tunnel
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# A list given as one argument ("a,b"), as `powershell.exe -File` and
# revert.cmd pass it, is split here; a tunnel name never has a comma.
$Tunnel = @($Tunnel | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } |
        Where-Object { $_ -ne '' })
if ($Tunnel.Count -eq 0) {
    throw 'Give at least one -Tunnel <name>; nothing was changed.'
}

$ServiceName = 'WGALWatchdog'
$TunnelPrefix = 'WireGuardTunnel$'
$ProgramDir = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'WGAL'
$Reserved = @('CON', 'PRN', 'AUX', 'NUL') + (1..9 | ForEach-Object { "COM$_"; "LPT$_" })

function Assert-Elevated {
    $principal = New-Object Security.Principal.WindowsPrincipal(
        [Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this as administrator: from an elevated PowerShell, or run the .cmd beside it from an elevated Command Prompt.'
    }
}

function Test-TunnelName([string] $Name) {
    return ($Name -cmatch '^[A-Za-z0-9_=+.-]{1,32}$') -and ($Name -ne '.') -and
        ($Name -ne '..') -and ($Reserved -notcontains $Name)
}

# The tunnel's service, looked up read-only with the name as given.
function Resolve-TunnelService([string] $Name) {
    if (-not (Test-TunnelName $Name)) {
        throw "Not a WireGuard for Windows tunnel name: '$Name'."
    }
    $found = @(Get-Service -Name ($TunnelPrefix + '*') -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ServiceName.StartsWith($TunnelPrefix, [StringComparison]::Ordinal) -and
            ($_.ServiceName.Substring($TunnelPrefix.Length) -ieq $Name)
        })
    if ($found.Count -ne 1) {
        throw "There is no service $TunnelPrefix$Name."
    }
    return $found[0].ServiceName
}

function Wait-NotPending([string] $Name, [int] $Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $status = [string](Get-Service -Name $Name).Status
        if ($status -notlike '*Pending') {
            return $status
        }
        Start-Sleep -Seconds 1
    }
    return [string](Get-Service -Name $Name).Status
}

# Whether WGAL is out of the way: its service is gone or Stopped, the
# service manager reports no process for it, and no wgal-watchdog process
# is left. Only "there is none" counts as none: a query that fails is not
# an answer, so WGAL is then not confirmed stopped (Codex review of the
# CLI, third round, R3-P2-3).
function Test-WgalStopped {
    try {
        $service = Get-Service -Name $ServiceName -ErrorAction Stop
        if (($null -ne $service) -and ([string]$service.Status -ne 'Stopped')) {
            return $false
        }
    } catch {
        if ($_.FullyQualifiedErrorId -notlike 'NoServiceFoundForGivenName*') {
            return $false
        }
    }
    try {
        $cim = @(Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'" -ErrorAction Stop)
        if (@($cim | Where-Object { [int]$_.ProcessId -ne 0 }).Count -gt 0) {
            return $false
        }
    } catch {
        return $false
    }
    try {
        if (@(Get-Process -Name 'wgal-watchdog' -ErrorAction Stop).Count -gt 0) {
            return $false
        }
    } catch {
        if ($_.FullyQualifiedErrorId -notlike 'NoProcessFoundForGivenName*') {
            return $false
        }
    }
    return $true
}

# Waits up to $Seconds for Test-WgalStopped.
function Wait-WgalStopped([int] $Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (-not (Test-WgalStopped)) {
        if ((Get-Date) -ge $deadline) {
            return $false
        }
        Start-Sleep -Seconds 1
    }
    return $true
}

function Show-State {
    Write-Host 'Now:'
    Get-Service -Name $ServiceName, ($TunnelPrefix + '*'), 'WireGuardManager' -ErrorAction SilentlyContinue |
        Sort-Object ServiceName |
        Select-Object @{ Name = 'Name'; Expression = { $_.ServiceName } }, Status, StartType |
        Format-Table -AutoSize | Out-Host
}

# The steps above, in order; returns the exit code.
function Invoke-Revert {
    # Checked before anything changes.
    $services = @($Tunnel | ForEach-Object { Resolve-TunnelService $_ })

# --- WGAL ---------------------------------------------------------------------
$wgal = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($null -eq $wgal) {
    Write-Host "There is no service $ServiceName."
} else {
    Set-Service -Name $ServiceName -StartupType Disabled
    Write-Host "$ServiceName is now Disabled: no reboot or recovery action starts it."
    if ($wgal.Status -ne 'Stopped') {
        Write-Host "Asking $ServiceName to stop..."
        $status = $null
        $deadline = (Get-Date).AddSeconds(45)
        while ((Get-Date) -lt $deadline) {
            $status = [string](Get-Service -Name $ServiceName).Status
            if ($status -eq 'Stopped') { break }
            if ($status -ne 'StopPending') {
                # Asked again on every pass until it is stopping: a service
                # that is still starting refuses the request.
                try {
                    Stop-Service -Name $ServiceName -NoWait
                } catch {
                    Write-Host "($ServiceName is $status and did not take the stop request yet; asking again.)"
                }
            }
            Start-Sleep -Seconds 1
        }
        if ($status -ne 'Stopped') {
            # The process the service manager reports for the service, ended
            # only if the service's registered program is WGAL's. (The path
            # of a LocalSystem process may not be readable even elevated.)
            $service = Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'"
            $processId = [int]$service.ProcessId
            $expected = '"' + (Join-Path $ProgramDir 'wgal-watchdog.exe') + '"'
            if ($processId -eq 0) {
                Write-Warning "$ServiceName is $status but has no process."
            } elseif ($service.PathName -ne $expected) {
                Write-Warning "$ServiceName runs '$($service.PathName)', not $expected; not ending it. Look at it yourself."
            } else {
                Write-Warning "$ServiceName did not stop within 45 s; ending its process ($processId)."
                Stop-Process -Id $processId -Force
                Wait-Process -Id $processId -Timeout 30 -ErrorAction SilentlyContinue
            }
        }
    }
}

# --- No tunnel while WGAL may still act on it (Codex review of the CLI,
# second round, N-P2-3) -------------------------------------------------------
if (-not (Wait-WgalStopped 15)) {
    Write-Warning "$ServiceName could not be confirmed stopped (its service, the process the service manager reports for it, or a wgal-watchdog process is still there), so no tunnel was started: WGAL could still act on it. Look at it yourself (Services, Task Manager), then run this again."
    Show-State
    return 1
}

# --- Tunnels ------------------------------------------------------------------
foreach ($name in $services) {
    $status = Wait-NotPending $name 60
    if ($status -eq 'Stopped') {
        Write-Host "Starting $name..."
        # Start() only asks; Start-Service would wait without a limit while
        # the service stays START_PENDING.
        $controller = Get-Service -Name $name
        try {
            $controller.Start()
            $controller.WaitForStatus('Running', [TimeSpan]::FromSeconds(60))
        } catch [System.ServiceProcess.TimeoutException] {
            $controller.Refresh()
            Write-Warning "$name is still $($controller.Status) after 60 s. Look at it in the official WireGuard app."
        } catch {
            Write-Warning "Starting $name failed: $($_.Exception.Message)"
        }
    } elseif ($status -ne 'Running') {
        Write-Warning "$name is $status; not starting it. Look at it in the official WireGuard app."
    }
}

# --- Result -------------------------------------------------------------------
Show-State
$stillUp = @(Get-Process -Name 'wgal-watchdog' -ErrorAction SilentlyContinue)
if ($stillUp.Count -gt 0) {
    Write-Warning "A wgal-watchdog process is still running ($(@($stillUp | ForEach-Object Id) -join ', '))."
    return 1
}
foreach ($name in $services) {
    if ([string](Get-Service -Name $name).Status -ne 'Running') {
        Write-Warning "$name is not running."
        return 1
    }
}
Write-Host 'WGAL is stopped and disabled; the named tunnels are running.'
Write-Host 'To resume WGAL: Set-Service -Name WGALWatchdog -StartupType Automatic; Start-Service -Name WGALWatchdog'
return 0
}

# Dot-sourced (tests\revert-uninstall.tests.ps1 does this): only the
# functions above are defined; nothing below runs.
if ($MyInvocation.InvocationName -eq '.') {
    return
}

Assert-Elevated
exit (Invoke-Revert)
