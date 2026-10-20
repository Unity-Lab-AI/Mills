@echo off
rem Builds the VibeRoller AI host tools once: VibeRollerAIHost.exe (the Windows service) and ONE shortcut, "VibeRoller AI
rem Host" (heart & knife icon), on the Desktop and in the Start menu: the control app for status, start / stop, every
rem setting and start at boot. Then offers to install the service (starts at boot).   build.cmd quiet = build only.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-host-tools.ps1"
if errorlevel 1 (if /i not "%~1"=="quiet" pause & exit /b 1)
if /i "%~1"=="quiet" exit /b 0
echo.
set /p INSTALL=Install the VibeRollerAIHost service now, so this PC is an AI host from boot? [Y/n]
if /i "%INSTALL%"=="n" (echo Later: "VibeRoller AI Host" on the Desktop, [b] start at boot. & pause & exit /b 0)
call "%~dp0install-ai-host-service.cmd"
