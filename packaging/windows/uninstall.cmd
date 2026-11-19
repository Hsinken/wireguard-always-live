@echo off
rem Removes WGAL; its data is kept unless -RemoveData is given.
rem For a Command Prompt run as administrator; the same as running
rem uninstall.ps1 in PowerShell. Usage: uninstall.cmd [-RemoveData]
rem It runs this folder's uninstall.ps1 with the system's own Windows PowerShell
rem (never one found on PATH). -ExecutionPolicy Bypass applies to that one
rem PowerShell process only and changes no system setting.
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
exit /b %ERRORLEVEL%
