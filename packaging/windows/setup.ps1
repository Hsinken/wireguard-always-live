#Requires -Version 5.1
<#
.SYNOPSIS
Guided install or upgrade of WGAL for people: a small window lists this
machine's active WireGuard tunnels with check boxes, then install.ps1 runs
with the ones ticked. Start it with setup.cmd (double-click).

.DESCRIPTION
Asks for administrator rights once: started without them, it starts itself
again elevated (one UAC prompt) and passes on the SID of the account that
started it, which becomes the WGAL window's user. Everything after that
runs in the elevated process, so nothing asks again.

It reads the tunnels with `install.ps1 -ListTunnels` and, on an upgrade,
the tunnels in the current <ProgramData>\WGAL\access.txt. It changes
nothing itself: it runs this folder's install.ps1 with the system's own
Windows PowerShell, with -Tunnel and -User only when a new list was chosen
(an upgrade that keeps the list passes neither, as install.ps1 expects).
It never reads WireGuard configurations or keys and never starts, stops or
changes a tunnel.

The window speaks Traditional Chinese when the system's display language is
Chinese and English otherwise.

-DryRun shows the same window without administrator rights and prints the
install.ps1 command it would run instead of running it: nothing changes.

Exit codes: install.ps1's; 10 cancelled or nothing to do; 11 the UAC prompt
was declined or the elevated run could not start.
#>
[CmdletBinding()]
param(
    [switch] $Elevated,
    [string] $User = '',
    [switch] $DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Here = $PSScriptRoot
$PowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$Install = Join-Path $Here 'install.ps1'
$AccessPath = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'WGAL\access.txt'

$Chinese = (Get-UICulture).Name -like 'zh*'
function T([string] $Zh, [string] $En) { if ($Chinese) { $Zh } else { $En } }

function Test-Elevated {
    $principal = New-Object Security.Principal.WindowsPrincipal(
        [Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Wait-Close {
    Write-Host ''
    Read-Host (T '按 Enter 關閉這個視窗' 'Press Enter to close this window') | Out-Null
}

# --- Not elevated: start again elevated, once ---------------------------------
if (-not $DryRun -and -not (Test-Elevated)) {
    if ($Elevated) {
        Write-Host (T '沒有取得系統管理員權限，沒有變更任何東西。' 'Administrator rights were not granted; nothing was changed.')
        exit 11
    }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    Write-Host (T '要安裝 WGAL 需要系統管理員權限，Windows 會詢問一次。' 'Installing WGAL needs administrator rights; Windows asks once.')
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'),
        '-Elevated', '-User', $sid)
    try {
        $child = Start-Process -FilePath $PowerShell -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    } catch {
        Write-Host (T '已取消（沒有同意系統管理員權限），沒有變更任何東西。' 'Cancelled (administrator rights were declined); nothing was changed.')
        exit 11
    }
    Write-Host ((T '安裝視窗已結束，代碼 ' 'The setup window ended with code ') + $child.ExitCode)
    exit $child.ExitCode
}

# --- Elevated: pick, then install ------------------------------------------------
if ($User -eq '') {
    $User = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
}
if ($User -cnotmatch '^S-1-(5-21|12-1)-\d+-\d+-\d+-\d+$') {
    Write-Host (T '帳號的 SID 格式不對，沒有變更任何東西。' 'The account SID is not valid; nothing was changed.')
    Wait-Close
    exit 10
}
if (-not (Test-Path -LiteralPath $Install -PathType Leaf)) {
    Write-Host (T '這個資料夾裡沒有 install.ps1。請從解壓縮後的發布包資料夾執行。' 'install.ps1 is not in this folder. Run this from the unzipped release folder.')
    Wait-Close
    exit 10
}

$listed = & $PowerShell -NoProfile -ExecutionPolicy Bypass -File $Install -ListTunnels
if ($LASTEXITCODE -ne 0) {
    Write-Host (T '無法列出 tunnel，沒有變更任何東西。' 'Could not list the tunnels; nothing was changed.')
    Wait-Close
    exit 10
}
$tunnels = @((($listed -join "`n") | ConvertFrom-Json).tunnels)
$usable = @($tunnels | Where-Object { $_.accepted })

$upgrade = $null -ne (Get-Service -Name 'WGALWatchdog' -ErrorAction SilentlyContinue)
# The current list, for showing and ticking only (install.ps1 keeps or
# rewrites the file); unreadable without administrator rights (-DryRun).
$current = @()
if ($upgrade) {
    try {
        if ((Test-Path -LiteralPath $AccessPath -PathType Leaf) -and
            -not ((Get-Item -LiteralPath $AccessPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            $current = @(Get-Content -LiteralPath $AccessPath -Encoding UTF8 |
                Where-Object { $_ -cmatch '^tunnel (\S+)$' } | ForEach-Object { $_.Substring(7) })
        }
    } catch {
        $current = @()
    }
}

if ($usable.Count -eq 0 -and -not $upgrade) {
    Write-Host (T '找不到啟用中的 WireGuard tunnel。請先在官方 WireGuard 程式啟用要監看的 tunnel，再執行一次。' 'No active WireGuard tunnel was found. Activate the tunnels to monitor in the official WireGuard app, then run this again.')
    Wait-Close
    exit 10
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[Windows.Forms.Application]::EnableVisualStyles()

$form = New-Object Windows.Forms.Form
$form.Text = T 'WGAL 安裝' 'WGAL setup'
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.AutoScaleMode = 'Font'
$form.Font = New-Object Drawing.Font('Segoe UI', 10)
$form.ClientSize = New-Object Drawing.Size(460, 400)
$form.TopMost = $true

$intro = New-Object Windows.Forms.Label
$intro.Location = New-Object Drawing.Point(16, 12)
$intro.Size = New-Object Drawing.Size(428, 64)
if ($upgrade) {
    $intro.Text = T 'WGAL 已經安裝，這次會升級。可以維持目前允許的 tunnel，或重新勾選（會取代整份清單）。' 'WGAL is installed; this upgrades it. Keep the tunnels it may use now, or tick a new list (it replaces the whole list).'
} else {
    $intro.Text = T '勾選要交給 WGAL 監看的 tunnel。只列出啟用中的 tunnel；沒啟用的請先在官方 WireGuard 程式啟用。' 'Tick the tunnels WGAL may monitor. Only active tunnels are listed; activate others in the official WireGuard app first.'
}
$form.Controls.Add($intro)

$keep = New-Object Windows.Forms.CheckBox
$keep.Location = New-Object Drawing.Point(16, 80)
$keep.Size = New-Object Drawing.Size(428, 44)
$keep.Visible = $upgrade
$keep.Checked = $upgrade
$keep.Text = (T '維持目前的清單：' 'Keep the current list: ') +
    $(if ($current.Count -gt 0) { $current -join ', ' } else { T '（讀不到）' '(not readable)' })
$form.Controls.Add($keep)

$list = New-Object Windows.Forms.CheckedListBox
$list.Location = New-Object Drawing.Point(16, $(if ($upgrade) { 128 } else { 80 }))
$list.Size = New-Object Drawing.Size(428, $(if ($upgrade) { 200 } else { 248 }))
$list.CheckOnClick = $true
foreach ($t in $tunnels) {
    $state = if ($t.status -eq 'running') { T '執行中' 'running' } else { $t.status }
    $boot = if ($t.starts_at_boot) { T '，開機自動啟動' ', starts at boot' } else { '' }
    $note = if ($t.accepted) { '' } else { T '  （WGAL 不接受這個名稱）' '  (WGAL does not accept this name)' }
    [void]$list.Items.Add("$($t.name)   ($state$boot)$note")
}
for ($i = 0; $i -lt $tunnels.Count; $i++) {
    if ($current -contains $tunnels[$i].name) { $list.SetItemChecked($i, $true) }
}
$form.Controls.Add($list)

$ok = New-Object Windows.Forms.Button
$ok.Text = if ($upgrade) { T '升級' 'Upgrade' } else { T '安裝' 'Install' }
$ok.Location = New-Object Drawing.Point(252, 344)
$ok.Size = New-Object Drawing.Size(92, 34)
$form.Controls.Add($ok)
$form.AcceptButton = $ok

$cancel = New-Object Windows.Forms.Button
$cancel.Text = T '取消' 'Cancel'
$cancel.Location = New-Object Drawing.Point(352, 344)
$cancel.Size = New-Object Drawing.Size(92, 34)
$cancel.DialogResult = [Windows.Forms.DialogResult]::Cancel
$form.Controls.Add($cancel)
$form.CancelButton = $cancel

# A name WGAL does not take cannot be ticked.
$list.Add_ItemCheck({
        param($sender, $e)
        if ($e.NewValue -eq [Windows.Forms.CheckState]::Checked -and -not $tunnels[$e.Index].accepted) {
            $e.NewValue = [Windows.Forms.CheckState]::Unchecked
        }
    })
$sync = {
    $list.Enabled = -not $keep.Checked
}
$keep.Add_CheckedChanged($sync)
& $sync

$script:chosen = $null
$ok.Add_Click({
        $running = @(Get-Process -Name 'wgal-desktop', 'wgal-mcp', 'wgal' -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            [void][Windows.Forms.MessageBox]::Show($form,
                (T '請先結束 WGAL 視窗（系統匣圖示 → 結束介面）、關閉正在使用 WGAL MCP 的 Agent，並等正在執行的 wgal 指令結束，再按一次。' 'Quit the WGAL window first (tray icon, then exit the interface), close any agent using the WGAL MCP server and let any running wgal command finish, then press again.'),
                $form.Text)
            return
        }
        if ($keep.Checked) {
            $script:chosen = @()
        } else {
            $picked = @()
            foreach ($i in $list.CheckedIndices) { $picked += $tunnels[$i].name }
            if ($picked.Count -eq 0) {
                [void][Windows.Forms.MessageBox]::Show($form, (T '請至少勾選一個 tunnel。' 'Tick at least one tunnel.'), $form.Text)
                return
            }
            $script:chosen = $picked
        }
        $form.DialogResult = [Windows.Forms.DialogResult]::OK
        $form.Close()
    })

$result = $form.ShowDialog()
$form.Dispose()
if ($result -ne [Windows.Forms.DialogResult]::OK -or $null -eq $script:chosen) {
    Write-Host (T '已取消，沒有變更任何東西。' 'Cancelled; nothing was changed.')
    Wait-Close
    exit 10
}

$arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Install)
if ($script:chosen.Count -gt 0) {
    $arguments += @('-Tunnel', ($script:chosen -join ','), '-User', $User)
    Write-Host ((T '安裝，允許的 tunnel：' 'Installing; tunnels: ') + ($script:chosen -join ', '))
} else {
    Write-Host (T '升級，維持目前的清單。' 'Upgrading; keeping the current list.')
}
Write-Host ''
if ($DryRun) {
    Write-Host ((T '試跑，不會執行：' 'Dry run, not run: ') + 'install.ps1 ' + (($arguments | Select-Object -Skip 5) -join ' '))
    exit 0
}
& $PowerShell @arguments
$code = $LASTEXITCODE
Write-Host ''
if ($code -eq 0) {
    Write-Host (T '完成。可以從開始功能表開啟 WGAL。' 'Done. Open WGAL from the Start menu.')
} else {
    Write-Host ((T '沒有完成，代碼 ' 'Not completed, code ') + $code + (T '。請看上面的訊息。' '. See the messages above.'))
}
Wait-Close
exit $code
