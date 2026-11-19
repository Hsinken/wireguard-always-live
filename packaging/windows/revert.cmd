@echo off
rem Ends the WGAL service without it being restarted, and starts the named
rem tunnels again if they are stopped (see README.zh-TW.md).
rem For a Command Prompt run as administrator; the same as running
rem revert.ps1 in PowerShell. Usage: revert.cmd -Tunnel <name>[,<name>...]
rem It runs this folder's revert.ps1 with the system's own Windows PowerShell
rem (never one found on PATH). -ExecutionPolicy Bypass applies to that one
rem PowerShell process only and changes no system setting.
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0revert.ps1" %*
exit /b %ERRORLEVEL%
