@echo off
title Mills Bridge
cd /d "%~dp0"
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
where claude >nul 2>nul || (echo Claude Code is required: npm install -g @anthropic-ai/claude-code, then run "claude" once to log in & pause & exit /b 1)
if not exist auth.json (
  echo First run: enter the Mills password you were given.
  set /p MILLS_PW=Password: 
  call node set-password.js "%%MILLS_PW%%" || (pause & exit /b 1)
)
start "" https://unity-lab-ai.github.io/Mills/
node server.js
pause
