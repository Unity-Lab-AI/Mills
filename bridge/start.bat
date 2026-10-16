@echo off
cd /d "%~dp0"
rem Usage: start.bat            (Alfreddo's bridge)
rem        start.bat sponge     (Sponge's bridge — needs profiles\sponge\config.json)
set "PROFILE=%~1"
set "WHO=Alfreddo"
set "AUTH=auth.json"
set "ARGS="
if not "%PROFILE%"=="" (
  set "WHO=%PROFILE%"
  set "AUTH=profiles\%PROFILE%\auth.json"
  set "ARGS=--profile %PROFILE%"
)
title Mills Bridge: %WHO% (leave this window open while %WHO% works)
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
where claude >nul 2>nul || (echo Claude Code is required. & pause & exit /b 1)
if not exist "%AUTH%" (
  echo First run: type the Mills password you are giving %WHO%.
  set /p MILLS_PW=Password:
  call node set-password.js %ARGS% "%%MILLS_PW%%" || (pause & exit /b 1)
)
rem run.js = supervisor: owns the tunnel, runs server.js under it and restarts it when asked
rem (node restart.js) or when it crashes. Closing this window stops everything.
node run.js %ARGS%
pause
