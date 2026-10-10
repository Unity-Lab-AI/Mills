@echo off
title Mills Bridge (leave this window open while Alfreddo works)
cd /d "%~dp0"
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
where claude >nul 2>nul || (echo Claude Code is required. & pause & exit /b 1)
if not exist auth.json (
  echo First run: type the Mills password you are giving Alfreddo.
  set /p MILLS_PW=Password:
  call node set-password.js "%%MILLS_PW%%" || (pause & exit /b 1)
)
node server.js
pause
