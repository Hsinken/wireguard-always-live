@echo off
rem Guided install or upgrade of WGAL for people: double-click this file.
rem It asks for administrator rights once (one UAC prompt), shows this
rem machine's active WireGuard tunnels with check boxes, and runs install.ps1
rem with the ones you tick (see setup.ps1). It uses the system's own Windows
rem PowerShell (never one found on PATH); -ExecutionPolicy Bypass applies to
rem that one PowerShell process only and changes no system setting.
setlocal
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
exit /b %ERRORLEVEL%
