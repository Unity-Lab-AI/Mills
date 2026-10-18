@echo off
rem Builds start-viberoller-host.exe (black heart) and stop-viberoller-host.exe (knife) once, with Desktop shortcuts.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-host-tools.ps1"
pause
