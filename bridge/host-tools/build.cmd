@echo off
rem Builds the VibeRoller AI host tools once: start-viberoller-ai-host.exe (black heart), stop-viberoller-ai-host.exe (knife)
rem and VibeRollerAIHost.exe (the Windows service), with Desktop shortcuts. Then offers to install the service (starts at boot).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-host-tools.ps1"
if errorlevel 1 (pause & exit /b 1)
echo.
set /p INSTALL=Install the VibeRollerAIHost service now, so this PC is an AI host from boot? [Y/n]
if /i "%INSTALL%"=="n" (echo Later: run install-ai-host-service.cmd. & pause & exit /b 0)
call "%~dp0install-ai-host-service.cmd"
