#Requires -Version 5.1
<#
.SYNOPSIS
Installs or upgrades WGAL (wg-always-live) on this machine. Run it from an
elevated PowerShell.

.DESCRIPTION
A first run installs; a run on a machine that already has the WGALWatchdog
service upgrades it. Quit the WGAL window first (tray icon, "exit the
interface"); the script refuses to run while it is open.

Everything is checked before anything changes: the inputs, that the WGAL
window is closed, and that <ProgramData>\WGAL, if it exists, and everything
in it is owned by SYSTEM or Administrators and holds no link or junction.

Install:
  - creates <ProgramData>\WGAL first, as a new folder that already has its
    security descriptor (owned by Administrators, a protected DACL that
    gives SYSTEM and Administrators full control, inherited by everything
    in it): made under a random name and renamed into place, which fails if
    anything appeared at the path meanwhile, then checked again;
  - copies wgal-watchdog.exe, wgal-desktop.exe, wgal-mcp.exe (the MCP
    server agents start; it does nothing until MCP is turned on in the
    window or with "wgal mcp on") and wgal.exe (WGAL's command line, owner
    decision of 2026-11-18; the PATH is not changed, so it is run by its
    full path) to <Program Files>\WGAL;
  - writes <ProgramData>\WGAL\access.txt as a new file (UTF-8 without BOM):
    the account that may use the WGAL window (-User) and the tunnels it may
    register (-Tunnel);
  - creates the service WGALWatchdog (LocalSystem, automatic start),
    restarted by the service manager when it fails, starts it, and checks
    that \\.\pipe\wgal-watchdog exists and is owned by SYSTEM (served by
    the service, not by another process holding the name).

Upgrade: checks the data folder's security; before the service stops,
copies the new executables into the program folder as <name>.exe.new and
the ones in place as <name>.exe.old (each checked by its SHA-256), records
their hashes in upgrade.record beside them, writes
the new access list as access.txt.new (only if -Tunnel or -User is
given), sets the service back to Automatic if revert.ps1, or an earlier
run that did not start it, left it Disabled and applies its recovery
actions. Then it stops WGALWatchdog and
waits for its process to end, copies the watchdog's stored state
(state\state.json, state\state.prev.json and notify\notify.json) to a new
folder backup\state-<UTC time of the upgrade> in <ProgramData>\WGAL and
keeps only the newest five such folders, replaces access.txt with the new
one in one step (the old one is never deleted first), copies the staged
executables into place, and starts the service again and checks it. A
newer build may store fields an older one refuses, so the backup, taken
before the new build first runs, is what a rollback restores
(README.zh-TW.md).

If anything fails from stopping the service until the started service has
been checked (Ctrl+C included), the script finds out which complete set
of executables is in place: the previous one, or the new one. Before the
new one was started, with some replaced and some not, it copies the
previous ones back from the .old copies. Once the start has begun, the new
version may already have run and changed the stored state, so nothing is
put back: it starts the new set again. It starts that set, checks it, and
says which one runs and which access list is in effect. It never copies
stored state back and never puts back an access list it replaced. If no
complete set can be made, it leaves the service stopped, keeps the copies
and upgrade.record, and says so; once the new version may have run, it
never starts the previous set, and says to put the complete new set in
place (or to hand it to the owner), never to copy the previous one back.
A new version that started and does not run (no pipe) is not rolled back:
it may already have changed the stored state.

A run that finds copies an earlier run left uses upgrade.record to check,
before changing anything, that a complete set is in place; if not, it
stops and changes nothing, so as not to lose copies that may be the only
complete set. The record says whether that run may have started the new
version ("started yes", or no such line); if so, this run never starts
the previous set either. The new set is started (the first time, or again
by the recovery) only after upgrade.record has been read back saying
"started yes"; if it cannot be, the service is left stopped, the copies
and the record are kept, and the script says so. Whenever it leaves the
service stopped like this, it also sets the service's start type to
Disabled and reads it back, so that Windows does not start that set at
the next boot, and says so (or that it could not, in which case Windows
may start it at the next boot: run the script again first); the next run
that gets past its checks sets it back to Automatic.
Everything else in <ProgramData>\WGAL is kept; the backup never reads or
copies the events, the protected bot token or access.txt.

The script writes nothing outside those two folders and the WGALWatchdog
service entry. It only reads the official WireGuard services: it records the
status and start type of every WireGuardTunnel$* service and of
WireGuardManager before and after, and reports any difference, but never
starts, stops or changes one.

Exit codes: 0 done; 1 the service is not running with its pipe; 2 a
WireGuard service changed while the script ran; 3 the service runs but
another process holds its pipe's name; 4 a step of the upgrade failed
after the service was stopped, and the service runs again with the
complete set of executables then in place (the messages say which). 4 is
given only when none of 1, 3 or 2 applies.

.PARAMETER From
The folder that holds wgal-watchdog.exe, wgal-desktop.exe, wgal-mcp.exe and wgal.exe. Default: the
folder this script is in.

.PARAMETER Tunnel
A WireGuard for Windows tunnel name that the user may register and hand to
automatic recovery. For several, give them as a list (-Tunnel a, b) or
comma-separated in one argument (-Tunnel a,b), which is how
`powershell.exe -File` and install.cmd pass them. It must have an
installed WireGuardTunnel$<name> service; the list is written with the
service's own spelling. Required on a first install.

.PARAMETER User
The SID of the account that uses the WGAL window. Default: the account
running this script. Give it when you elevate with another administrator
account than the one that will use the window (whoami /user in that
account's normal window shows it).

.PARAMETER ListTunnels
Lists this machine's WireGuard for Windows tunnels as JSON and exits,
changing nothing; it needs no administrator rights and takes no other
parameter. For an agent (or setup.ps1) to let the user pick the tunnels
before the one elevated run that installs:
  {"complete": false, "tunnels": [{"name": "office", "status": "running",
   "starts_at_boot": true, "accepted": true}]}
status is the tunnel service's status in lower case (running, stopped,
...); starts_at_boot is whether its start type is Automatic; accepted is
whether WGAL takes the name (a name it does not take cannot be given to
-Tunnel). complete is false: WireGuard for Windows has a tunnel's service
only while the tunnel is active, so a tunnel that is not active is not
listed (and cannot be given to -Tunnel either); activate it in the
official WireGuard app first.
#>
[CmdletBinding()]
param(
    [string] $From = '',
    [string[]] $Tunnel = @(),
    [string] $User = '',
    [switch] $ListTunnels
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# A list given as one argument ("a,b"), as `powershell.exe -File` and
# install.cmd pass it, is split here; a tunnel name never has a comma.
$Tunnel = @($Tunnel | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } |
        Where-Object { $_ -ne '' })
if ($PSBoundParameters.ContainsKey('Tunnel') -and $Tunnel.Count -eq 0) {
    throw 'Give at least one -Tunnel <name> (or leave -Tunnel out on an upgrade); nothing was changed.'
}

# Not a parameter default: Windows PowerShell 5.1 leaves $PSScriptRoot empty
# there when a [CmdletBinding()] script is run with -File.
if ($From -eq '') {
    $From = $PSScriptRoot
}

$ServiceName = 'WGALWatchdog'
$TunnelPrefix = 'WireGuardTunnel$'
$PipeName = 'wgal-watchdog'
$PipePath = '\\.\pipe\' + $PipeName
$Executables = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe', 'wgal.exe')
# The executables of a version before 3.0.0, which has no wgal.exe: an
# earlier run's record of such a set is read with wgal.exe absent from both
# versions.
$OlderExecutables = @('wgal-watchdog.exe', 'wgal-desktop.exe', 'wgal-mcp.exe')
# Known folders through .NET (SHGetFolderPath), not environment variables.
$ProgramDir = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'WGAL'
$DataDir = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'WGAL'
$DataSddl = 'O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'
# What a newer watchdog may store in a form an older one refuses, relative
# to the data folder: the state store (wgal-runtime store.rs: the current
# file and the one before; not state.lock) and the notification settings
# (notify.rs settings.rs; never the protected bot token next to it).
$StateFiles = @('state\state.json', 'state\state.prev.json', 'notify\notify.json')
$KeepStateBackups = 5
$Administrators = 'S-1-5-32-544'
$LocalSystem = 'S-1-5-18'
$Reserved = @('CON', 'PRN', 'AUX', 'NUL') + (1..9 | ForEach-Object { "COM$_"; "LPT$_" })

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

function Get-OwnerSid([string] $Path) {
    return (Get-Acl -LiteralPath $Path).GetOwner([Security.Principal.SecurityIdentifier]).Value
}

# Refuses a data folder, or anything in it, that is a link or junction or
# owned by another account than SYSTEM or Administrators: something the
# service would refuse too, or that someone planted. Never descends into a
# link.
function Assert-DataTrusted([string] $Path) {
    if (Test-ReparsePoint $Path) {
        throw "$Path is a link or junction. Look at it and remove it yourself, then run again."
    }
    $owner = Get-OwnerSid $Path
    if (($owner -ne $Administrators) -and ($owner -ne $LocalSystem)) {
        throw "$Path is owned by another account ($owner). Look at it and remove it yourself, then run again."
    }
    if ((Get-Item -LiteralPath $Path -Force).PSIsContainer) {
        foreach ($child in @(Get-ChildItem -LiteralPath $Path -Force)) {
            Assert-DataTrusted $child.FullName
        }
    }
}

# The rules of WGAL's TunnelName: [A-Za-z0-9_=+.-]{1,32}, not a reserved
# device name, not "." or "..".
function Test-TunnelName([string] $Name) {
    return ($Name -cmatch '^[A-Za-z0-9_=+.-]{1,32}$') -and ($Name -ne '.') -and
        ($Name -ne '..') -and ($Reserved -notcontains $Name)
}

# The tunnel's name as the service manager spells it. WGAL compares names
# exactly, so the access list must use this spelling, not the one typed.
function Resolve-TunnelName([string] $Name) {
    # The prefix exactly as the official manager writes it, as the adapter
    # requires; the name itself compared ignoring case, as the SCM does.
    $found = @(Get-Service -Name ($TunnelPrefix + '*') -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ServiceName.StartsWith($TunnelPrefix, [StringComparison]::Ordinal) -and
            ($_.ServiceName.Substring($TunnelPrefix.Length) -ieq $Name)
        })
    if ($found.Count -ne 1) {
        throw "There is no service $TunnelPrefix$Name. Add the tunnel in the official WireGuard app first."
    }
    $spelled = $found[0].ServiceName.Substring($TunnelPrefix.Length)
    if (-not (Test-TunnelName $spelled)) {
        throw "The service for '$Name' has a name WGAL does not accept: '$spelled'."
    }
    if ($spelled -cne $Name) {
        Write-Host "Using the service's spelling '$spelled' for -Tunnel '$Name'."
    }
    return $spelled
}

# A user account SID in canonical form, as access.txt accepts it.
function Test-UserSid([string] $Sid) {
    if ($Sid -cnotmatch '^S-1-(5-21|12-1)-\d+-\d+-\d+-\d+$') {
        return $false
    }
    try {
        return (New-Object Security.Principal.SecurityIdentifier($Sid)).Value -ceq $Sid
    } catch {
        return $false
    }
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

# Prints the list and every difference. Returns $true when they match.
function Compare-WireGuardServices($Before, $After) {
    $old = @($Before | ForEach-Object { "$($_.Name)|$($_.Status)|$($_.StartType)" })
    $new = @($After | ForEach-Object { "$($_.Name)|$($_.Status)|$($_.StartType)" })
    $differences = @(Compare-Object -ReferenceObject $old -DifferenceObject $new)
    Write-Host 'WireGuard services after:'
    $After | Format-Table -AutoSize | Out-Host
    if ($differences.Count -eq 0) {
        Write-Host 'WireGuard services: unchanged (status and start type).'
        return $true
    }
    Write-Warning '!!! A WireGuard service changed while this script ran. The script never touches them; check what did. !!!'
    foreach ($difference in $differences) {
        $side = if ($difference.SideIndicator -eq '<=') { 'before' } else { 'after ' }
        Write-Warning "  $side  $($difference.InputObject)"
    }
    return $false
}

# Throws unless $Path is a plain folder (not a link or junction) owned by
# Administrators whose DACL is protected and allows exactly SYSTEM and
# Administrators full control, nothing else.
function Assert-ProtectedDirectory([string] $Path) {
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Path is not a plain folder. Look at it and remove it yourself, then run again."
    }
    $acl = Get-Acl -LiteralPath $Path
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -ne $Administrators) {
        throw "$Path is owned by $owner, not Administrators. Look at it and remove it yourself, then run again."
    }
    if (-not $acl.AreAccessRulesProtected) {
        throw "$Path inherits permissions; it should not."
    }
    $full = @{}
    foreach ($rule in @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))) {
        $who = $rule.IdentityReference.Value
        if (($rule.AccessControlType -ne 'Allow') -or (($who -ne $Administrators) -and ($who -ne $LocalSystem))) {
            throw "$Path grants or denies access to $who; only SYSTEM and Administrators may be listed."
        }
        if (($rule.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq
            [Security.AccessControl.FileSystemRights]::FullControl) {
            $full[$who] = $true
        }
    }
    if (-not ($full.ContainsKey($Administrators) -and $full.ContainsKey($LocalSystem))) {
        throw "$Path does not give SYSTEM and Administrators full control."
    }
}

# Creates the folder $Path with its security descriptor, and fails if
# anything is already there. The folder is made under a random name next to
# it, already protected (so it never has the inherited ProgramData DACL,
# which lets Users add files), checked, and then renamed to $Path; the
# rename fails if a folder, file or junction has appeared at $Path, and the
# result is checked again.
function New-ProtectedDirectory([string] $Path, [string] $Sddl) {
    $parent = Split-Path -Parent $Path
    $temporary = Join-Path $parent ('WGAL.install-' + [Guid]::NewGuid().ToString('N'))
    if (Test-Path -LiteralPath $temporary) {
        throw "$temporary already exists; run again."
    }
    $security = New-Object Security.AccessControl.DirectorySecurity
    $security.SetSecurityDescriptorSddlForm($Sddl)
    $withSecurity = [IO.Directory].GetMethod('CreateDirectory',
        [Type[]]@([string], [Security.AccessControl.DirectorySecurity]))
    if ($null -ne $withSecurity) {
        # Windows PowerShell 5.1 (.NET Framework).
        [void][IO.Directory]::CreateDirectory($temporary, $security)
    } else {
        # PowerShell 7 (.NET).
        [void][System.IO.FileSystemAclExtensions]::CreateDirectory($security, $temporary)
    }
    try {
        Assert-ProtectedDirectory $temporary
        [IO.Directory]::Move($temporary, $Path)
    } catch {
        if (Test-Path -LiteralPath $temporary) {
            # Still empty: nobody else may add anything to it.
            [IO.Directory]::Delete($temporary)
        }
        throw "Could not create $Path as a new protected folder (something may have appeared there): $($_.Exception.Message)"
    }
    Assert-ProtectedDirectory $Path
}

function Set-AdministratorsOwner([string] $Path) {
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetOwner((New-Object Security.Principal.SecurityIdentifier($Administrators)))
    Set-Acl -LiteralPath $Path -AclObject $acl
}

# Removes one old backup folder made by Backup-State: the plain files in its
# plain subfolders, those subfolders, then the folder. A folder holding
# anything else, or with a link anywhere, is left alone with a warning.
function Remove-StateBackup([string] $Path) {
    $plain = {
        param($item)
        -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
    }
    $folders = @(Get-ChildItem -LiteralPath $Path -Force)
    $usual = (& $plain (Get-Item -LiteralPath $Path -Force)) -and
        (@($folders | Where-Object { $_.PSIsContainer -and (& $plain $_) }).Count -eq $folders.Count)
    $files = @()
    if ($usual) {
        foreach ($folder in $folders) {
            $inside = @(Get-ChildItem -LiteralPath $folder.FullName -Force)
            if (@($inside | Where-Object { -not $_.PSIsContainer -and (& $plain $_) }).Count -ne $inside.Count) {
                $usual = $false
            }
            $files += $inside
        }
    }
    if (-not $usual) {
        Write-Warning "Left the old backup $Path as it is: it holds something Backup-State does not make."
        return
    }
    foreach ($file in $files) {
        [IO.File]::Delete($file.FullName)
    }
    foreach ($folder in $folders) {
        [IO.Directory]::Delete($folder.FullName)
    }
    [IO.Directory]::Delete($Path)
}

# Copies the watchdog's stored state ($StateFiles, relative to the data
# folder) into a new folder <data>\backup\state-<UTC yyyyMMdd-HHmmss>Z, in
# the same subfolders, everything owned by Administrators like the rest of
# the data folder; then removes all but the newest $Keep such folders. Call
# it only while the service is stopped, so the files are not being written.
# Reads nothing else in the data folder. Returns the new folder, or $null
# when none of the files exists yet. Throws, before copying anything, if a
# file, its folder or the backup folder is a link, or if a backup with the
# same time already exists.
function Backup-State([string] $Data, [datetime] $At, [int] $Keep) {
    $files = @($StateFiles | Where-Object {
            Test-Path -LiteralPath (Join-Path $Data $_) -PathType Leaf
        })
    if ($files.Count -eq 0) {
        return $null
    }
    foreach ($relative in $files) {
        $file = Join-Path $Data $relative
        foreach ($path in @((Split-Path -Parent $file), $file)) {
            if (Test-ReparsePoint $path) {
                throw "$path is a link or junction. Look at it and remove it yourself, then run again."
            }
        }
    }
    $backups = Join-Path $Data 'backup'
    if (Test-Path -LiteralPath $backups) {
        if ((Test-ReparsePoint $backups) -or -not (Get-Item -LiteralPath $backups -Force).PSIsContainer) {
            throw "$backups is not a plain folder. Look at it and remove it yourself, then run again."
        }
    } else {
        New-Item -ItemType Directory -Path $backups | Out-Null
        Set-AdministratorsOwner $backups
    }
    $target = Join-Path $backups ('state-' + $At.ToUniversalTime().ToString('yyyyMMdd-HHmmss') + 'Z')
    # Fails if it exists: an earlier run in the same second.
    New-Item -ItemType Directory -Path $target | Out-Null
    Set-AdministratorsOwner $target
    foreach ($relative in $files) {
        $copy = Join-Path $target $relative
        $folder = Split-Path -Parent $copy
        if (-not (Test-Path -LiteralPath $folder)) {
            New-Item -ItemType Directory -Path $folder | Out-Null
            Set-AdministratorsOwner $folder
        }
        Copy-Item -LiteralPath (Join-Path $Data $relative) -Destination $copy
        Set-AdministratorsOwner $copy
    }
    $older = @(Get-ChildItem -LiteralPath $backups -Directory -Force |
            Where-Object { $_.Name -cmatch '^state-\d{8}-\d{6}Z$' } |
            Sort-Object Name -Descending |
            Select-Object -Skip $Keep)
    foreach ($folder in $older) {
        Remove-StateBackup $folder.FullName
    }
    return $target
}

function Test-PipeExists {
    return @([IO.Directory]::GetFiles('\\.\pipe\')) -contains $PipePath
}

# The owner SID of the pipe \\.\pipe\$Name, read the way the WGAL window
# checks it: connect as a client at identification level (so whoever serves
# the pipe can identify this administrator but never act as it), read the
# handle's owner, close without writing anything. $null if no connection
# could be made within 2 s.
function Get-PipeOwnerSid([string] $Name) {
    $client = New-Object IO.Pipes.NamedPipeClientStream('.', $Name,
        [IO.Pipes.PipeDirection]::In, [IO.Pipes.PipeOptions]::None,
        [Security.Principal.TokenImpersonationLevel]::Identification)
    try {
        try {
            $client.Connect(2000)
        } catch {
            return $null
        }
        if ($null -ne $client.GetType().GetMethod('GetAccessControl', [Type[]]@())) {
            # Windows PowerShell 5.1 (.NET Framework).
            $security = $client.GetAccessControl()
        } else {
            # PowerShell 7 (.NET).
            $security = [System.IO.Pipes.PipesAclExtensions]::GetAccessControl($client)
        }
        return $security.GetOwner([Security.Principal.SecurityIdentifier]).Value
    } finally {
        $client.Dispose()
    }
}

function Get-ServiceProcessId {
    $service = Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'"
    if ($null -eq $service) { return 0 }
    return [int]$service.ProcessId
}

# Stops the service and waits for its process to end, so its executable can
# be replaced. The stop is asked for without waiting (again while the
# service is still starting, which refuses it) and waited for at most 60 s
# (Codex review of the CLI, third round, R3-P2-4: a plain Stop-Service
# waits with no limit); then it throws, and the upgrade window's recovery
# deals with the set in place.
function Stop-WgalService {
    $processId = Get-ServiceProcessId
    Write-Host "Stopping $ServiceName..."
    $deadline = (Get-Date).AddSeconds(60)
    while ($true) {
        $status = [string](Get-Service -Name $ServiceName).Status
        if ($status -eq 'Stopped') {
            break
        }
        if ((Get-Date) -ge $deadline) {
            throw "$ServiceName did not stop within 60 s (it is $status)."
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
    if ($processId -ne 0) {
        Wait-Process -Id $processId -Timeout 30 -ErrorAction SilentlyContinue
        if ($null -ne (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
            throw "The $ServiceName process ($processId) did not end within 30 s after the service stopped."
        }
    }
}

# Starts the service and waits up to 30 s for it to run with its pipe; then
# reads the pipe's owner. Never throws: a failure to start is reported and
# shows as Running = $false. The start is asked with the controller's
# Start(), which only asks: Start-Service would wait with no limit while
# the service stays StartPending, before the 30 s even began (Codex review
# of the CLI, fourth round, R4-P2-3). Only a stopped service is asked; one
# already starting or running is waited for.
function Start-WgalChecked {
    $result = @{ Running = $false; PipeOwner = $null }
    try {
        $controller = Get-Service -Name $ServiceName
        if ([string]$controller.Status -eq 'Stopped') {
            $controller.Start()
        }
        $deadline = (Get-Date).AddSeconds(30)
        while ((Get-Date) -lt $deadline) {
            $status = (Get-Service -Name $ServiceName).Status
            if (($status -eq 'Running') -and (Test-PipeExists)) {
                $result.Running = $true
                break
            }
            if ($status -eq 'Stopped') {
                break
            }
            Start-Sleep -Milliseconds 500
        }
        if ($result.Running) {
            # The pipe must be the service's own: another process could hold
            # the name while the service runs without it (and retries).
            $result.PipeOwner = Get-PipeOwnerSid $PipeName
        }
    } catch {
        Write-Warning "Starting $ServiceName failed: $($_.Exception.Message)"
    }
    return $result
}

# A first install's start, only once upgrade.record says "started yes" and
# reads back so, as an upgrade's (Codex review of the CLI, second round,
# N-P2-1): a run stopped after the start leaves a record saying the new
# version may have run. When it cannot be marked, the service is not
# started and its start type is set to Disabled, so neither a reboot nor
# the service manager starts it. Returns Start-WgalChecked's result.
function Start-FirstInstall($Set) {
    $why = Confirm-StartedMark $Set
    if ($null -eq $why) {
        # Created Disabled (Install-FirstSet): it starts at boot only from
        # here on, with the record saying it may have run.
        try {
            Set-Service -Name $ServiceName -StartupType Automatic
        } catch {
            $why = "its start type could not be set to Automatic: $($_.Exception.Message)"
        }
    }
    if ($null -ne $why) {
        $autoStart = Disable-WgalAutoStart
        Write-Warning ("!!! $ServiceName was NOT started: the new version is in place, but upgrade.record could " +
            "not be marked to say that it may run ($why), so it is not started. The copies and upgrade.record " +
            "are kept: fix the cause (upgrade.record in $ProgramDir must be writable: free disk space, or end the " +
            "program that holds it open), then run this script again. $autoStart !!!")
        return @{ Running = $false; PipeOwner = $null; Unmarked = $true }
    }
    Write-Host "Starting $ServiceName..."
    return Start-WgalChecked
}

# A first install's set: the access list, the executables and the service,
# which is created Disabled (Codex review of the CLI, third round,
# R3-P2-2): a failure or Ctrl-C before Start-FirstInstall has marked the
# record leaves a service that neither a reboot nor the service manager
# starts. Start-FirstInstall sets it to Automatic once the record is marked.
function Install-FirstSet($Set, [string] $Binary, [bool] $WriteAccess) {
    if ($WriteAccess) {
        Install-StagedAccessList
    }
    Install-StagedPrograms $Set
    New-Service -Name $ServiceName -BinaryPathName $Binary `
        -DisplayName 'WG Always Live watchdog' `
        -Description 'Checks that traffic flows through the official WireGuard tunnels and recovers the one that failed, when authorized. Local only.' `
        -StartupType Disabled | Out-Null
    Write-Host "Created the service $ServiceName (Disabled until its first start)."
    Set-WgalRecoveryActions
}

# The service's recovery actions, WGAL's own settings: restart after 1 min,
# 1 min, then every 5 min, counts reset after a day; also when the service
# stops itself with an error code, not only on a crash.
function Set-WgalRecoveryActions {
    & sc.exe failure $ServiceName reset= 86400 actions= restart/60000/restart/60000/restart/300000 | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "sc.exe failure returned $LASTEXITCODE." }
    & sc.exe failureflag $ServiceName 1 | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "sc.exe failureflag returned $LASTEXITCODE." }
}

# Turns off the automatic start of WGAL's own service (start type Disabled)
# and reads it back, so that Windows does not start, at the next boot, a set
# this run refused to start. Starts and stops nothing, and touches no other
# service. Returns the sentence the "NOT started" message carries: that it
# was turned off and how it is turned back on, or that it could not be.
function Disable-WgalAutoStart {
    $why = $null
    try {
        Set-Service -Name $ServiceName -StartupType Disabled
        $now = [string](Get-Service -Name $ServiceName).StartType
        if ($now -ne 'Disabled') {
            $why = "its start type reads back as $now"
        }
    } catch {
        $why = $_.Exception.Message
    }
    if ($null -eq $why) {
        return ("Its automatic start was turned off (start type Disabled), so Windows does not start it at the " +
            'next boot either; the next run of this script that gets past its checks sets it back to Automatic.')
    }
    return ("Its automatic start could NOT be turned off ($why): Windows may start $ServiceName, with the " +
        'executables now in place, at the next boot. Fix the cause and run this script again (or hand ' +
        'everything to the owner) before you restart this computer.')
}

# Sets WGAL's own service back to automatic start when it is Disabled:
# revert.ps1 leaves it so, and so does a run of this script that refused to
# start a set (Disable-WgalAutoStart). Called on an upgrade once every check
# has passed, before the service stops.
function Enable-WgalAutoStart {
    if ([string](Get-Service -Name $ServiceName).StartType -eq 'Disabled') {
        # The start type is WGAL's own setting.
        Set-Service -Name $ServiceName -StartupType Automatic
        Write-Host ("$ServiceName was Disabled (revert.ps1 does that, and so does a run of this script that " +
            'did not start it); it is set back to Automatic.')
    }
}

# The service's own settings on an upgrade, the last step before the window
# that stops it: the recovery actions first, then the automatic start set
# back. So a failure here leaves a service that a run refused to start still
# Disabled: nothing after the start type is set back can fail before the
# window, whose recovery turns it off again when it starts nothing (Codex
# review of FIX-05, autostart, P2-01).
function Initialize-UpgradeService {
    Set-WgalRecoveryActions
    Enable-WgalAutoStart
}

# --- The executables, staged before the service stops ------------------------
# Each one's new copy (<name>.exe.new) and the copy of the one in place
# (<name>.exe.old) live beside it in the program folder: fixed names, on the
# same volume, checked like the executables themselves.
function Get-StagedPath([string] $Exe, [string] $Kind) {
    return Join-Path $ProgramDir ($Exe + '.' + $Kind)
}

function Get-FileSha256([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

# Removes $Path if it is there; throws if it is a link or not a file.
function Remove-PlainFile([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Path is not a plain file. Look at it and remove it yourself, then run again."
    }
    [IO.File]::Delete($Path)
}

# The record of a staged set, beside its copies: one line per executable,
# "<name> <new SHA-256> <old SHA-256, or - for none>", and "started no",
# rewritten as "started yes" before the new set's start begins (or carried
# from an earlier run that may have started it: $Set.MayHaveRun). Written
# once every copy is made and checked, removed after them; so copies
# without a record were left by a run that stopped while staging, before
# the service stopped. A record without its "started" line cannot tell, so
# it counts as "yes": the new version may have run.
function Get-RecordPath {
    return Join-Path $ProgramDir 'upgrade.record'
}

function Save-ProgramRecord($Set) {
    $lines = @($Executables | ForEach-Object {
            $old = if ($null -eq $Set.Old[$_]) { '-' } else { $Set.Old[$_] }
            "$_ $($Set.New[$_]) $old"
        })
    $lines += $(if ($Set.MayHaveRun) { 'started yes' } else { 'started no' })
    [IO.File]::WriteAllLines((Get-RecordPath), [string[]]$lines)
}

# The set an earlier run recorded, as Save-ProgramSet returns it; $null when
# there is no record. Throws for anything this script does not write.
function Read-ProgramRecord {
    $path = Get-RecordPath
    if (-not (Test-Path -LiteralPath $path)) {
        return $null
    }
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$path is not a plain file. Look at it and remove it yourself, then run again."
    }
    $set = @{ New = @{}; Old = @{}; MayHaveRun = $true }
    $started = $null
    foreach ($line in [IO.File]::ReadAllLines($path)) {
        $parts = @($line -split ' ')
        if (($parts.Count -eq 2) -and ($parts[0] -ceq 'started') -and ($null -eq $started) -and
            (@('yes', 'no') -ccontains $parts[1])) {
            $started = $parts[1]
            continue
        }
        if (($parts.Count -ne 3) -or ($Executables -cnotcontains $parts[0]) -or $set.New.ContainsKey($parts[0]) -or
            ($parts[1] -cnotmatch '^[0-9A-F]{64}$') -or ($parts[2] -cnotmatch '^([0-9A-F]{64}|-)$')) {
            throw "$path is not a record this script wrote. Nothing was changed; look at it, then run again."
        }
        $set.New[$parts[0]] = $parts[1]
        $set.Old[$parts[0]] = if ($parts[2] -eq '-') { $null } else { $parts[2] }
    }
    if (($set.New.Count -eq $OlderExecutables.Count) -and -not $set.New.ContainsKey('wgal.exe')) {
        # Written by the script of a version before 3.0.0, whose sets have
        # no wgal.exe: absent from both, as in that run.
        $set.New['wgal.exe'] = $null
        $set.Old['wgal.exe'] = $null
    }
    if ($set.New.Count -ne $Executables.Count) {
        throw "$path is not a record this script wrote. Nothing was changed; look at it, then run again."
    }
    $set.MayHaveRun = $started -cne 'no'
    return $set
}

# Marks upgrade.record "started yes" and reads it back. The new set is
# started only once a rerun would read that it may have run: returns $null
# when the record says so, else why it does not (nothing may start then).
function Confirm-StartedMark($Set) {
    $Set.MayHaveRun = $true
    $why = $null
    try { Save-ProgramRecord $Set } catch { $why = $_.Exception.Message }
    try {
        $record = Read-ProgramRecord
        if (($null -ne $record) -and $record.MayHaveRun) {
            return $null
        }
    } catch {
        if ($null -eq $why) { $why = $_.Exception.Message }
    }
    if ($null -eq $why) { $why = 'upgrade.record does not say "started yes" after it was written' }
    return $why
}

# An earlier run that failed between the stop and the start may have kept
# its copies on purpose: they may be the only complete set. They are
# removed (by Save-ProgramSet) only when its record shows a complete set in
# place, or that there was no previous version; otherwise this throws and
# nothing is touched, before the service is stopped. Returns $true when
# that run may have started the new version (its record does not say
# "started no"): this run then never starts the previous version either.
function Assert-EarlierCopiesSafe {
    $record = Read-ProgramRecord
    if ($null -eq $record) {
        return $false
    }
    if ($null -eq $record.Old['wgal-watchdog.exe']) {
        return $record.MayHaveRun
    }
    $inPlace = Get-ProgramSetInPlace $record
    if (($inPlace -eq 'mixed') -and $record.MayHaveRun) {
        throw ("An earlier run of this script did not finish: the executables in $ProgramDir are neither " +
            'the complete previous version nor the complete new one it recorded in upgrade.record, and that run ' +
            'may already have started the new version, so the new version may already have run and changed the ' +
            'stored state. Nothing was changed, and the service was not stopped. Do not put the previous version ' +
            'back (do not copy the *.exe.old copies back): put the complete new version in place yourself (copy ' +
            'each *.exe.new over its executable; each must match its new hash in upgrade.record), then run this ' +
            'script again; or leave everything as it is and hand it to the owner.')
    }
    if ($inPlace -eq 'mixed') {
        throw ("An earlier run of this script did not finish: the executables in $ProgramDir are neither " +
            'the complete previous version nor the complete new one it recorded in upgrade.record, and the copies ' +
            'it kept (*.exe.old the previous version, *.exe.new the new one) may be the only complete set. ' +
            'Nothing was changed, and the service was not stopped. Put one complete set in place yourself (copy ' +
            'each *.exe.new over its executable; or copy each *.exe.old over its executable and remove an ' +
            'executable that has no .old copy), then run this script again.')
    }
    $which = if ($inPlace -eq 'old') { 'previous' } else { 'new' }
    Write-Host "An earlier run left its copies; the complete $which set it recorded is in place, so they are replaced."
    if ($record.MayHaveRun) {
        Write-Warning 'That run may already have started the new version: if this run fails, the previous version is not started again.'
    }
    return $record.MayHaveRun
}

# Copies the new executables from $Source to <name>.exe.new and the ones in
# place to <name>.exe.old, checks every copy by its SHA-256, and records
# the hashes (upgrade.record). Runs while the service still runs (reading
# its executable is allowed). Returns the hashes: New[exe], and Old[exe]
# ($null for one that is not installed), and MayHaveRun (an earlier run
# may have started the new version: Assert-EarlierCopiesSafe).
function Save-ProgramSet([string] $Source) {
    if (-not (Test-Path -LiteralPath $ProgramDir)) {
        New-Item -ItemType Directory -Path $ProgramDir | Out-Null
    }
    if (Test-ReparsePoint $ProgramDir) {
        throw "$ProgramDir is a link or junction. Remove it yourself and run again."
    }
    $mayHaveRun = Assert-EarlierCopiesSafe
    $set = @{ New = @{}; Old = @{}; MayHaveRun = [bool]$mayHaveRun }
    foreach ($exe in $Executables) {
        $target = Join-Path $ProgramDir $exe
        $new = Get-StagedPath $exe 'new'
        $old = Get-StagedPath $exe 'old'
        if ((Test-Path -LiteralPath $target) -and (Test-ReparsePoint $target)) {
            throw "$target is a link. Remove it yourself and run again."
        }
        # Left by an earlier run; Assert-EarlierCopiesSafe allowed it.
        Remove-PlainFile $new
        Remove-PlainFile $old
        $set.New[$exe] = Get-FileSha256 (Join-Path $Source $exe)
        Copy-Item -LiteralPath (Join-Path $Source $exe) -Destination $new
        if ((Get-FileSha256 $new) -ne $set.New[$exe]) {
            throw "The copy $new does not match $(Join-Path $Source $exe)."
        }
        $set.Old[$exe] = Get-FileSha256 $target
        if ($null -ne $set.Old[$exe]) {
            Copy-Item -LiteralPath $target -Destination $old
            if ((Get-FileSha256 $old) -ne $set.Old[$exe]) {
                throw "The copy $old does not match $target."
            }
        }
    }
    Save-ProgramRecord $set
    return $set
}

# Puts the staged executables in place and checks each by its SHA-256.
function Install-StagedPrograms($Set) {
    foreach ($exe in $Executables) {
        $target = Join-Path $ProgramDir $exe
        Copy-Item -LiteralPath (Get-StagedPath $exe 'new') -Destination $target -Force
        if ((Get-FileSha256 $target) -ne $Set.New[$exe]) {
            throw "$target does not match the new version after copying."
        }
    }
}

# Which complete set of executables is in place: 'new' when all of them are
# the new ones, 'old' when all of them are as before (each one that was
# installed is the previous one again, and one the previous version did not
# have is absent), else 'mixed'.
function Get-ProgramSetInPlace($Set) {
    $isNew = $true
    $isOld = $null -ne $Set.Old['wgal-watchdog.exe']
    foreach ($exe in $Executables) {
        $now = Get-FileSha256 (Join-Path $ProgramDir $exe)
        if ($now -ne $Set.New[$exe]) {
            $isNew = $false
        }
        # $null (none before) matches only $null (none now).
        if ($now -ne $Set.Old[$exe]) {
            $isOld = $false
        }
    }
    if ($isNew) { return 'new' }
    if ($isOld) { return 'old' }
    return 'mixed'
}

# Copies back, from the .old copies, every previous executable that is no
# longer in place, and removes one the previous version did not have.
# Returns Get-ProgramSetInPlace afterwards.
function Restore-OldPrograms($Set) {
    foreach ($exe in $Executables) {
        $target = Join-Path $ProgramDir $exe
        if ($null -eq $Set.Old[$exe]) {
            Remove-PlainFile $target
        } elseif ((Get-FileSha256 $target) -ne $Set.Old[$exe]) {
            Copy-Item -LiteralPath (Get-StagedPath $exe 'old') -Destination $target -Force
        }
    }
    return Get-ProgramSetInPlace $Set
}

# Removes the .new and .old copies, and then their record, once a complete
# set is in place.
function Remove-StagedPrograms {
    foreach ($exe in $Executables) {
        Remove-PlainFile (Get-StagedPath $exe 'new')
        Remove-PlainFile (Get-StagedPath $exe 'old')
    }
    Remove-PlainFile (Get-RecordPath)
}

# --- The access list, written before the service stops ----------------------
function Get-AccessPath {
    return Join-Path $DataDir 'access.txt'
}

function Get-StagedAccessPath {
    return Join-Path $DataDir 'access.txt.new'
}

# Writes the whole new access list as access.txt.new, a new file owned by
# Administrators, in the protected data folder.
function New-StagedAccessList([string] $Sid, [string[]] $Names) {
    # The folder is protected; checked once more right before writing into it.
    Assert-ProtectedDirectory $DataDir
    $staged = Get-StagedAccessPath
    Remove-PlainFile $staged
    $who = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $lines = @(
        '# WGAL access list, written by packaging\windows\install.ps1',
        "# by $who on $((Get-Date).ToString('yyyy-MM-dd HH:mm:ss zzz'))",
        '# user: the account that may use the WGAL window',
        '# tunnel: a tunnel that account may register and hand to automatic recovery',
        "user $Sid"
    ) + @($Names | ForEach-Object { "tunnel $_" })
    $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes(($lines -join "`r`n") + "`r`n")
    # A new file, never an old one written over: whatever handle anyone held
    # on the old one does not reach the new one.
    $stream = New-Object IO.FileStream($staged, [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $stream.Write($bytes, 0, $bytes.Length)
    } finally {
        $stream.Dispose()
    }
    # Whatever the "default owner" policy says, the service accepts only
    # SYSTEM or Administrators as owner.
    Set-AdministratorsOwner $staged
    return $staged
}

# Replaces access.txt with access.txt.new in one step (ReplaceFile; a plain
# rename when there is none yet): there is always either the old list or
# the new one, never neither. The service reads it only when it starts.
function Install-StagedAccessList {
    $access = Get-AccessPath
    $staged = Get-StagedAccessPath
    if (Test-Path -LiteralPath $access) {
        [IO.File]::Replace($staged, $access, [NullString]::Value)
    } else {
        [IO.File]::Move($staged, $access)
    }
    Set-AdministratorsOwner $access
}

# --- The upgrade, from stopping the service to starting it again -------------
# Every step from the stop until the started service has been checked runs
# inside one try/finally: a failure, or Ctrl+C, ends in
# Resume-AfterFailedUpgrade, which starts the complete set in place.
# Returns Done and, when done, Started (Start-WgalChecked's result); when
# not done, the Step that failed, its Error and the Outcome of the
# recovery. NewMayHaveRun says the start of the new set had begun (or was
# about to: the mark before it), StartBegun that this run called it,
# EarlierMayHaveRun that an earlier run may have started it.
function Invoke-UpgradeWindow($Set, [bool] $WriteAccess) {
    $progress = @{
        Done = $false; Step = 'stopping the service'; Error = 'interrupted'; Outcome = $null
        Started = $null; NewMayHaveRun = [bool]$Set.MayHaveRun; StartBegun = $false
        EarlierMayHaveRun = [bool]$Set.MayHaveRun
    }
    try {
        Stop-WgalService
        # Before the new build first runs, while nothing writes the state.
        $progress.Step = 'backing up the stored state'
        $backup = Backup-State $DataDir (Get-Date) $KeepStateBackups
        if ($null -eq $backup) {
            Write-Host 'No watchdog state file yet; nothing to back up.'
        } else {
            Write-Host "Backed up the watchdog's state to $backup (kept: the newest $KeepStateBackups)."
        }
        if ($WriteAccess) {
            $progress.Step = 'putting the new access list in place'
            Install-StagedAccessList
        }
        $progress.Step = 'replacing the executables'
        Install-StagedPrograms $Set
        # From here on the new version may run and change the stored state:
        # a failure no longer puts the previous one back.
        $progress.Step = 'starting the new version'
        $progress.NewMayHaveRun = $true
        # Kept for a rerun until the record is removed; not started unless
        # the record says so.
        $why = Confirm-StartedMark $Set
        if ($null -ne $why) {
            throw "upgrade.record could not be marked ""started yes"": $why"
        }
        Write-Host "Starting $ServiceName..."
        $progress.StartBegun = $true
        $progress.Started = Start-WgalChecked
        $progress.Done = $true
    } catch {
        $progress.Error = $_.Exception.Message
    } finally {
        if (-not $progress.Done) {
            $progress.Outcome = Resume-AfterFailedUpgrade $Set $progress $WriteAccess
        }
    }
    return $progress
}

# After a failed step: makes sure a complete set of executables is in place,
# starts it and checks it, and says what happened. Never touches the stored
# state, and never puts back an access list that was replaced. Once the new
# set's start had begun, puts nothing back: it starts the new set again.
# Returns Set ('old', 'new' or 'mixed'), Running and PipeOwner.
function Resume-AfterFailedUpgrade($Set, $Progress, [bool] $WriteAccess) {
    Write-Warning "!!! The upgrade failed while $($Progress.Step): $($Progress.Error) !!!"
    $inPlace = 'mixed'
    try {
        $inPlace = Get-ProgramSetInPlace $Set
        if ($Progress.NewMayHaveRun) {
            # The new version may have run: the previous one is not put back.
        } elseif (($inPlace -eq 'mixed') -and ($null -ne $Set.Old['wgal-watchdog.exe'])) {
            Write-Warning "Only some executables were replaced; copying the previous ones back from the copies kept beside them (*.exe.old)."
            $inPlace = Restore-OldPrograms $Set
        } elseif ($inPlace -eq 'mixed') {
            # There was no previous wgal-watchdog.exe to go back to.
            Write-Warning 'Only some executables were replaced and there is no previous wgal-watchdog.exe; copying the rest of the new ones into place.'
            Install-StagedPrograms $Set
            $inPlace = Get-ProgramSetInPlace $Set
        }
    } catch {
        Write-Warning "Checking or restoring the executables failed: $($_.Exception.Message)"
        try { $inPlace = Get-ProgramSetInPlace $Set } catch { $inPlace = 'mixed' }
    }
    if ($WriteAccess) {
        if (Test-Path -LiteralPath (Get-StagedAccessPath)) {
            Write-Warning 'access.txt was not replaced: the previous access list is still in effect, not the one given to this run.'
            try { Remove-PlainFile (Get-StagedAccessPath) } catch { Write-Warning "Could not remove access.txt.new: $($_.Exception.Message)" }
        } else {
            Write-Warning 'The new access.txt is in place; it is kept (an access list is never put back).'
        }
    }
    if ($Progress.NewMayHaveRun -and ($inPlace -ne 'new')) {
        # The previous version is never started on a state the new one may
        # have changed; upgrade.record keeps saying so for a rerun.
        $what = if ($inPlace -eq 'mixed') { "the executables in $ProgramDir are not the complete new version" } else { "the previous version's executables are in place, not the new ones" }
        $autoStart = Disable-WgalAutoStart
        Write-Warning ("!!! $ServiceName was NOT started: $what, and the new version's start had begun (in this " +
            'run or in an earlier one), so the new version may already have run and changed the stored state: do ' +
            'not put the previous version back (do not copy the *.exe.old copies back), and do not start it. The ' +
            'copies beside the executables (*.exe.new the new version, *.exe.old the previous one) and ' +
            'upgrade.record are kept. Fix the cause and put the complete new version in place: run this script ' +
            'again (if the executables in place are not a complete set, it refuses until you copy each *.exe.new ' +
            'over its executable; each must match its new hash in upgrade.record); or leave everything as it is ' +
            "and hand it to the owner. $autoStart !!!")
        return @{ Set = $inPlace; Running = $false; PipeOwner = $null }
    }
    if ($inPlace -eq 'mixed') {
        $autoStart = Disable-WgalAutoStart
        Write-Warning ("!!! $ServiceName was NOT started: the executables in $ProgramDir are neither the complete " +
            'previous version nor the complete new one. The previous ones are kept beside them as *.exe.old, ' +
            'the new ones as *.exe.new, their hashes in upgrade.record. Fix the cause, put one complete set in ' +
            'place yourself (copy each *.exe.new over its executable; or copy each *.exe.old over its executable ' +
            'and remove an executable that has no .old copy), and run this script again: until then it refuses ' +
            "to run, so as not to lose these copies. $autoStart !!!")
        return @{ Set = 'mixed'; Running = $false; PipeOwner = $null }
    }
    if ($inPlace -eq 'new') {
        # From here on it is started: a rerun must know, so it is not
        # started unless upgrade.record says so (removed with the copies).
        $why = Confirm-StartedMark $Set
        if ($null -ne $why) {
            $ran = if ($Progress.StartBegun) {
                'This run had started the new version, so it may already have run and changed the stored state.'
            } else {
                'This run did not start the new version.'
            }
            $autoStart = Disable-WgalAutoStart
            Write-Warning ("!!! $ServiceName was NOT started: the new version's executables are in place, complete, " +
                "but upgrade.record could not be marked to say that the new version may have run ($why). A later " +
                'run reads that mark to know that the new version may have changed the stored state, so the ' +
                "new version is not started without it. $ran The copies beside the executables (*.exe.new the new " +
                'version, *.exe.old the previous one) and upgrade.record are kept. Leave the new version in place ' +
                '(do not copy the *.exe.old copies back): fix the cause (upgrade.record in ' + $ProgramDir +
                ' must be writable: free disk space, or end the program that holds it open), then run this script ' +
                "again; or leave everything as it is and hand it to the owner. $autoStart !!!")
            return @{ Set = $inPlace; Running = $false; PipeOwner = $null }
        }
    }
    if ($inPlace -eq 'old') {
        Write-Warning ("The previous version's executables are in place, complete. Starting $ServiceName again with " +
            'them; the stored state is as the previous version left it (nothing was copied back).')
    } elseif ($Progress.StartBegun -or $Progress.EarlierMayHaveRun) {
        Write-Warning ("The new version's executables are in place, complete. The failure came after its start had " +
            'begun (in this run or in an earlier one), so it may already have run and changed the stored state: ' +
            "the previous version is not put back. Starting $ServiceName with the new version again.")
    } else {
        Write-Warning ("The new version's executables are in place, complete; it has not run yet. Starting " +
            "$ServiceName with them.")
    }
    $started = Start-WgalChecked
    try {
        Remove-StagedPrograms
    } catch {
        Write-Warning "Could not remove the staged copies (*.exe.new, *.exe.old): $($_.Exception.Message)"
    }
    return @{ Set = $inPlace; Running = $started.Running; PipeOwner = $started.PipeOwner }
}

function Show-ServiceFailure {
    Write-Warning "$ServiceName is not running with its pipe. What the service manager says:"
    & sc.exe query $ServiceName | Out-Host
    Write-Warning ('SERVICE_EXIT_CODE: 1 = ProgramData\WGAL missing or not protected; ' +
        '2 = a subfolder of it is not usable; 3 = the state store is locked or unusable; ' +
        '4 = the watchdog loop could not start; 11 = the watchdog loop stopped answering.')
    Write-Warning ('If the service runs but the pipe is missing, another process may hold ' +
        '\\.\pipe\wgal-watchdog; the service keeps monitoring and tries again (see service.log).')
    $log = Join-Path $DataDir 'service.log'
    if ((Test-Path -LiteralPath $log -PathType Leaf) -and -not (Test-ReparsePoint $log)) {
        Write-Warning 'Last lines of ProgramData\WGAL\service.log:'
        Get-Content -LiteralPath $log -Tail 15 | Out-Host
    }
}

# The machine's tunnels, read only: what -ListTunnels prints.
function Get-TunnelList {
    $list = @(Get-Service -Name ($TunnelPrefix + '*') -ErrorAction SilentlyContinue |
        Where-Object { $_.ServiceName.StartsWith($TunnelPrefix, [StringComparison]::Ordinal) } |
        Sort-Object ServiceName |
        ForEach-Object {
            $name = $_.ServiceName.Substring($TunnelPrefix.Length)
            [ordered]@{
                name = $name
                status = ([string]$_.Status).ToLowerInvariant()
                starts_at_boot = ([string]$_.StartType -eq 'Automatic')
                accepted = (Test-TunnelName $name)
            }
        })
    # Only active tunnels have a service (see -ListTunnels).
    return [ordered]@{ complete = $false; tunnels = $list }
}

# Dot-sourced (the tests in tests\ do this): only the functions above are
# defined; nothing below runs.
if ($MyInvocation.InvocationName -eq '.') {
    return
}

if ($ListTunnels) {
    if ($PSBoundParameters.Count -ne 1) {
        throw '-ListTunnels takes no other parameter; nothing was changed.'
    }
    # -InputObject keeps a one-tunnel list a list (the pipeline would unroll it).
    ConvertTo-Json -InputObject (Get-TunnelList) -Depth 4 -Compress
    exit 0
}

Assert-Elevated

# --- Checks; nothing changes before they pass --------------------------------
Assert-GuiClosed
$From = (Resolve-Path -LiteralPath $From).ProviderPath
foreach ($exe in $Executables) {
    if (-not (Test-Path -LiteralPath (Join-Path $From $exe) -PathType Leaf)) {
        throw "Missing $exe in $From. Build and stage it first (see README.zh-TW.md)."
    }
}

$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
$upgrade = $null -ne $existing
$writeAccess = (-not $upgrade) -or ($Tunnel.Count -gt 0) -or ($User -ne '')
if ($writeAccess -and $Tunnel.Count -eq 0) {
    throw 'Give at least one -Tunnel <name>: the tunnels the WGAL user may register. (On an upgrade, -Tunnel rewrites the whole access list.)'
}
if ($User -eq '') {
    $User = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
}
if ($writeAccess -and -not (Test-UserSid $User)) {
    throw '-User must be the SID of a user account (S-1-5-21-... or S-1-12-1-...), in canonical form.'
}
$tunnels = @()
$seen = @{}
foreach ($name in $Tunnel) {
    if (-not (Test-TunnelName $name)) {
        throw "Not a WireGuard for Windows tunnel name: '$name'."
    }
    $spelled = Resolve-TunnelName $name
    if ($seen.ContainsKey($spelled.ToLowerInvariant())) {
        throw "Tunnel '$name' is given twice."
    }
    $seen[$spelled.ToLowerInvariant()] = $true
    $tunnels += $spelled
}

$binary = '"' + (Join-Path $ProgramDir 'wgal-watchdog.exe') + '"'
if ($upgrade) {
    $registered = (Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'").PathName
    if ($registered -ne $binary) {
        throw "$ServiceName runs '$registered', not $binary. Not upgrading a service this script did not install."
    }
}
if ((Test-Path -LiteralPath $ProgramDir) -and (Test-ReparsePoint $ProgramDir)) {
    throw "$ProgramDir is a link or junction. Remove it yourself and run again."
}
foreach ($exe in $Executables) {
    foreach ($target in @((Join-Path $ProgramDir $exe), (Get-StagedPath $exe 'new'), (Get-StagedPath $exe 'old'), (Get-RecordPath))) {
        if ((Test-Path -LiteralPath $target) -and (Test-ReparsePoint $target)) {
            throw "$target is a link. Remove it yourself and run again."
        }
    }
}
if (Test-Path -LiteralPath $DataDir) {
    Assert-DataTrusted $DataDir
}

$before = @(Get-WireGuardServices)
Write-Host 'WireGuard services before:'
$before | Format-Table -AutoSize | Out-Host

# --- Data folder, first, so the time between its check and its creation is
# as short as it can be ---------------------------------------------------------
if (Test-Path -LiteralPath $DataDir) {
    # Checked again: it may have appeared since the checks above. Its
    # security is checked, not rewritten: a folder whose permissions changed
    # since it was created is refused, for the owner to look at.
    Assert-DataTrusted $DataDir
    Assert-ProtectedDirectory $DataDir
} else {
    # Fails if anything appears at the path meanwhile.
    New-ProtectedDirectory $DataDir $DataSddl
}

# --- Everything the service's stop and start enclose, prepared first ----------
# The new executables beside the old ones, and copies of the old ones; the
# new access list as its own file; on an upgrade, the service's own settings.
$programSet = Save-ProgramSet $From
Write-Host "Staged the new executables in $ProgramDir (*.exe.new; copies of those in place: *.exe.old)."
if ($writeAccess) {
    [void](New-StagedAccessList $User $tunnels)
}
if ($upgrade) {
    Initialize-UpgradeService
}

# --- Upgrade: stop, back up, replace, start; recovery on any failure until
# the started service is checked -----------------------------------------------
$window = $null
if ($upgrade) {
    $window = Invoke-UpgradeWindow $programSet $writeAccess
} else {
    Install-FirstSet $programSet $binary $writeAccess
}
$failed = ($null -ne $window) -and -not $window.Done
if (-not $failed) {
    if ($writeAccess) {
        Write-Host "Wrote $(Get-AccessPath) (1 user, $($tunnels.Count) tunnels)."
    } else {
        Write-Host 'Kept the existing access.txt.'
    }
}

if ($failed) {
    # Resume-AfterFailedUpgrade started (or refused to start) the set in place.
    $running = $window.Outcome.Running
    $pipeOwner = $window.Outcome.PipeOwner
} else {
    if ($upgrade) {
        # Started and checked inside the upgrade window.
        $started = $window.Started
    } else {
        $started = Start-FirstInstall $programSet
    }
    $running = $started.Running
    $pipeOwner = $started.PipeOwner
    try {
        # Kept when a first install could not mark its record: they say
        # what is in place.
        if (-not $started.ContainsKey('Unmarked')) {
            Remove-StagedPrograms
        }
    } catch {
        Write-Warning "Could not remove the staged copies (*.exe.new, *.exe.old): $($_.Exception.Message)"
    }
}

$after = @(Get-WireGuardServices)
$unchanged = Compare-WireGuardServices $before $after

if (-not $running) {
    Show-ServiceFailure
    if ($failed) {
        Write-Warning "!!! The upgrade did not complete and $ServiceName is not running: nothing monitors the tunnels now. Fix the cause shown above and run this script again. !!!"
    }
    exit 1
}
$log = Join-Path $DataDir 'service.log'
$retrying = (Test-Path -LiteralPath $log -PathType Leaf) -and -not (Test-ReparsePoint $log) -and
    (@(Select-String -LiteralPath $log -SimpleMatch 'the pipe could not be served' -List).Count -gt 0)
if ($pipeOwner -ne $LocalSystem) {
    $who = if ($null -eq $pipeOwner) { 'could not be read' } else { "is $pipeOwner" }
    Write-Warning "!!! $ServiceName is running, but $PipePath is not its own (its owner $who; it must be SYSTEM). !!!"
    Write-Warning 'Another process holds the name. WGAL keeps monitoring and recovering and tries again to serve the pipe; the WGAL window shows the watchdog as lost until then. Find the process that holds the pipe, end it, and run this script again.'
    if ($retrying) {
        Write-Warning 'ProgramData\WGAL\service.log confirms: "the pipe could not be served".'
    }
    exit 3
}
Write-Host "$ServiceName is running and serves $PipePath (owned by SYSTEM)."
if ($retrying) {
    Write-Host 'Note: service.log shows the pipe could not be served at some point; it is served now.'
}
Write-Host 'Open the WGAL window from your normal (not elevated) session, with Explorer or the Start menu:'
Write-Host "    $(Join-Path $ProgramDir 'wgal-desktop.exe')"
Write-Host 'Do not start it from this elevated window: it would run as an administrator.'
Write-Host "WGAL's command line, from any terminal (the PATH is not changed):"
Write-Host "    & '$(Join-Path $ProgramDir 'wgal.exe')' --help"
if (-not $unchanged) {
    exit 2
}
if ($failed) {
    $version = if ($window.Outcome.Set -eq 'old') { 'the previous version' } else { 'the new version' }
    Write-Warning "!!! The upgrade did not complete (it failed while $($window.Step)). $ServiceName runs $version. Fix the cause shown above and run this script again. !!!"
    exit 4
}
exit 0
