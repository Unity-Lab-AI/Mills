@echo off
cd /d "%~dp0"
rem Starts this PC's VibeRoller AI node (bridge\ai-node.js): it lends Unity's local engines (Ollama, image, voice,
rem speech) and its AI to the hub named in bridge\ai-node.json, connecting out. A second copy finds the first and closes.
rem The knife (stop-viberoller-host) pauses it; the heart (start-viberoller-host) starts or resumes it.
rem Exit codes: 0 = stopped, 2 = already running (both close this window); 3 = new code or an update arrived: start
rem again at once; anything else = it crashed: start again after 10 seconds. Closing the window stops it.
title VibeRoller AI node (leave this window open)
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
if not exist "ai-node.json" (echo bridge\ai-node.json is missing: copy ai-node.example.json to ai-node.json and fill in the hub, name and token. & pause & exit /b 1)
:run
node ai-node.js
if errorlevel 4 goto crashed
if errorlevel 3 goto run
if errorlevel 2 exit /b 0
if errorlevel 1 goto crashed
exit /b 0
:crashed
echo The AI node stopped with an error; starting it again in 10 seconds (close this window to stop it).
timeout /t 10 /nobreak >nul
goto run
