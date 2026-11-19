#Requires -Version 5.1
<#
.SYNOPSIS
Removes WGAL (wg-always-live) from this machine. Run it from an elevated
PowerShell.

.DESCRIPTION
Quit the WGAL window first (tray icon, "exit the interface"); the script
refuses to run while it is open.

Stops the WGALWatchdog service and waits for it and its process, deletes the
service, and removes <Program Files>\WGAL. <ProgramData>\WGAL (state,
events, settings, the protected bot token, the access list, the service log)
is kept unless -RemoveData is given.

It never starts, stops or changes an official WireGuard service, not even
one that WGAL stopped: it records the status and start type of every
WireGuardTunnel$* service and of WireGuardManager before and after, reports
any difference, and for every tunnel service that is stopped at the end it
prints the command to start it again yourself. That comparison and those
warnings are printed even when a step before them fails.

If the service hangs while stopping, run revert.ps1 first; it ends the
service process without the service being restarted.

.PARAMETER RemoveData
Also remove <ProgramData>\WGAL. Refused if anything in it is a link or
junction.
#>
[CmdletBinding()]
param(
    [switch] $RemoveData
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ServiceName = 'WGALWatchdog'
$TunnelPrefix = 'WireGuardTunnel$'
$ProgramDir = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'WGAL'
$DataDir = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'WGAL'

function Assert-Elevated {
    $principal = New-Object Security.Principal.WindowsPrincipal(
        [Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this as administrator: from an elevated PowerShell, or run the .cmd beside it from an elevated Command Prompt.'
    }
}

function Assert-GuiClosed {
    if ($null -ne (Get-Process -Name 'wgal-desktop' -ErrorAction SilentlyContinue)) {
        throw 'The WGAL window (wgal-desktop) is running. Quit it first: tray icon, then the menu item that exits the interface. Then run this script again.'
    }
    if ($null -ne (Get-Process -Name 'wgal-mcp' -ErrorAction SilentlyContinue)) {
        throw 'An agent is running the WGAL MCP server (wgal-mcp). Close that agent (or its MCP connection) first, then run this script again.'
    }
    if ($null -ne (Get-Process -Name 'wgal' -ErrorAction SilentlyContinue)) {
        throw 'The WGAL command line (wgal.exe) is running. Let it finish, or stop it, then run this script again.'
    }
}

function Test-ReparsePoint([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force
    return [bool]($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

function Get-WireGuardServices {
    Get-Service -Name ($TunnelPrefix + '*'), 'WireGuardManager' -ErrorAction SilentlyContinue |
        Sort-Object ServiceName |
        ForEach-Object {
            [pscustomobject]@{
                Name = $_.ServiceName
                Status = [string]$_.Status
                StartType = [string]$_.StartType
            }
        }
}

# Lists every link or junction under $Path without descending into one.
function Find-Links([string] $Path) {
    foreach ($child in @(Get-ChildItem -LiteralPath $Path -Force)) {
        if ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            $child.FullName
        } elseif ($child.PSIsContainer) {
            Find-Links $child.FullName
        }
    }
}

# Removes a folder WGAL created, refusing links anywhere in it: Windows
# PowerShell's recursive delete could follow a junction out of the folder.
function Remove-WgalFolder([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        Write-Host "$Path does not exist."
        return
    }
    if (Test-ReparsePoint $Path) {
        throw "$Path is a link or junction; not removing it. Look at it yourself."
    }
    $links = @(Find-Links $Path)
    if ($links.Count -gt 0) {
        throw "$Path holds links or junctions ($($links.Count)); not removing it. Look at them yourself."
    }
    Remove-Item -LiteralPath $Path -Recurse -Force
    Write-Host "Removed $Path."
}

# Asks WGALWatchdog to stop without waiting (asking again while it is
# still starting, which refuses the request) and waits for Stopped for at
# most $Seconds (Codex review of the CLI, second round, N-P2-4: a plain
# Stop-Service waits with no limit). $true once it is stopped.
function Stop-WgalService([int] $Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ($true) {
        $status = [string](Get-Service -Name $ServiceName).Status
        if ($status -eq 'Stopped') {
            return $true
        }
        if ((Get-Date) -ge $deadline) {
            return $false
        }
        if ($status -ne 'StopPending') {
            try {
                Stop-Service -Name $ServiceName -NoWait
            } catch {
                Write-Host "($ServiceName is $status and did not take the stop request yet; asking again.)"
            }
        }
        Start-Sleep -Seconds 1
    }
}

function Remove-WgalService {
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($null -eq $service) {
        Write-Host "There is no service $ServiceName."
        return
    }
    $processId = [int](Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'").ProcessId
    if ($service.Status -ne 'Stopped') {
        Write-Host "Stopping $ServiceName (it finishes a tunnel restart it already began)..."
        if (-not (Stop-WgalService 90)) {
            throw "$ServiceName did not stop within 90 s; nothing was removed. Run revert.ps1, then this script again."
        }
    }
    if ($processId -ne 0) {
        Wait-Process -Id $processId -Timeout 30 -ErrorAction SilentlyContinue
        if ($null -ne (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
            throw "The $ServiceName process ($processId) did not end within 30 s. Run revert.ps1, then this script again."
        }
    }
    & sc.exe delete $ServiceName | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "sc.exe delete returned $LASTEXITCODE." }
    $deadline = (Get-Date).AddSeconds(30)
    while (($null -ne (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) -and ((Get-Date) -lt $deadline)) {
        Start-Sleep -Milliseconds 500
    }
    if ($null -ne (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) {
        Write-Warning "$ServiceName is marked for deletion; it disappears once nothing holds it open (close the Services console)."
    } else {
        Write-Host "Deleted the service $ServiceName."
    }
}

# Dot-sourced (tests\revert-uninstall.tests.ps1 does this): only the
# functions above are defined; nothing below runs.
if ($MyInvocation.InvocationName -eq '.') {
    return
}

Assert-Elevated
Assert-GuiClosed

$before = @(Get-WireGuardServices)
Write-Host 'WireGuard services before:'
$before | Format-Table -AutoSize | Out-Host

$failed = $false
try {
    Remove-WgalService
    Remove-WgalFolder $ProgramDir
    if ($RemoveData) {
        Remove-WgalFolder $DataDir
    } else {
        Write-Host "Kept $DataDir (give -RemoveData to remove it)."
    }
} catch {
    $failed = $true
    Write-Warning "Uninstall stopped: $($_.Exception.Message)"
} finally {
    $after = @(Get-WireGuardServices)
    Write-Host 'WireGuard services after:'
    $after | Format-Table -AutoSize | Out-Host
    $old = @($before | ForEach-Object { "$($_.Name)|$($_.Status)|$($_.StartType)" })
    $new = @($after | ForEach-Object { "$($_.Name)|$($_.Status)|$($_.StartType)" })
    $differences = @(Compare-Object -ReferenceObject $old -DifferenceObject $new)
    if ($differences.Count -eq 0) {
        Write-Host 'WireGuard services: unchanged (status and start type).'
    } else {
        Write-Warning '!!! A WireGuard service changed while this script ran. The script never touches them; check what did. !!!'
        foreach ($difference in $differences) {
            $side = if ($difference.SideIndicator -eq '<=') { 'before' } else { 'after ' }
            Write-Warning "  $side  $($difference.InputObject)"
        }
    }
    foreach ($tunnel in @($after | Where-Object { $_.Name.StartsWith($TunnelPrefix) -and $_.Status -eq 'Stopped' })) {
        Write-Warning "$($tunnel.Name) is stopped. This script does not start it. If it should run, start it yourself:"
        Write-Warning "    Start-Service '$($tunnel.Name)'"
    }
}
if ($failed) {
    exit 1
}
if ($differences.Count -gt 0) {
    exit 2
}
exit 0
