@echo off
cd /d "%~dp0"
rem Starts the VibeRoller hub: one window serves every account. To start it by itself at sign-in, run autostart-windows.cmd once.
title VibeRoller (leave this window open)
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
where claude >nul 2>nul || (echo Claude Code is required. & pause & exit /b 1)
if not exist "auth.json" (
  echo First run: type the VibeRoller password for the "mills" account.
  set /p MILLS_PW=Password:
  call node set-password.js "%%MILLS_PW%%" || (pause & exit /b 1)
)
rem run.js = supervisor: owns the tunnel, runs server.js under it and restarts it when asked
rem (node restart.js) or when it crashes. Closing this window stops everything.
node run.js
pause
