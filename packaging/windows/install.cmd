@echo off
rem Installs or upgrades WGAL from this folder.
rem For a Command Prompt run as administrator; the same as running
rem install.ps1 in PowerShell. Usage: install.cmd -Tunnel <name>[,<name>...] [-User <SID>]
rem install.cmd -ListTunnels lists the active tunnels as JSON and changes nothing
rem (no administrator rights needed). For a window with check boxes, use setup.cmd.
rem It runs this folder's install.ps1 with the system's own Windows PowerShell
rem (never one found on PATH). -ExecutionPolicy Bypass applies to that one
rem PowerShell process only and changes no system setting.
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*
exit /b %ERRORLEVEL%
