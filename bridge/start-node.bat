@echo off
cd /d "%~dp0"
rem Starts this PC's VibeRoller AI node (bridge\ai-node.js): it lends Unity's local engines (Ollama, image, voice,
rem speech) to the hub named in bridge\ai-node.json, connecting out. A second copy finds the first and closes.
rem The knife (stop-viberoller-host) pauses it; the heart (start-viberoller-host) starts or resumes it.
title VibeRoller AI node (leave this window open)
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
if not exist "ai-node.json" (echo bridge\ai-node.json is missing: copy ai-node.example.json to ai-node.json and fill in the hub, name and token. & pause & exit /b 1)
node ai-node.js
if errorlevel 2 exit /b 0
pause
