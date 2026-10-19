@echo off
rem VibeRoller autostart for Windows: run this once (double-click) on the PC that should always be there for VibeRoller.
rem It writes a launcher into your Startup folder that starts, minimized, every time you sign in to Windows:
rem   - this checkout's bridge\start.bat (the hub) when there is no bridge\ai-node.json, or
rem   - bridge\start-node.bat (this PC's AI node, lending Unity's engines to the hub on the server) when there is.
rem Run it again after moving or renaming the folder, or after creating ai-node.json. Undo: autostart-windows.cmd remove
rem (or delete VibeRoller.cmd from the Startup folder: Win+R, shell:startup).
setlocal
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "LAUNCHER=%STARTUP%\VibeRoller.cmd"
if /i "%~1"=="remove" (
  del "%LAUNCHER%" 2>nul
  echo VibeRoller no longer starts when you sign in.
  goto :eof
)
if exist "%~dp0ai-node.json" (set "START=%~dp0start-node.bat" & set "WHAT=this PC's AI node") else (set "START=%~dp0start.bat" & set "WHAT=the hub")
if not exist "%START%" (echo %START% not found. & pause & exit /b 1)
(
  echo @echo off
  echo rem VibeRoller autostart: starts %WHAT% minimized at sign-in ^(written by bridge\autostart-windows.cmd; run it again after a move^).
  echo rem Safe to run twice: a second copy finds the first and closes.
  echo start "VibeRoller (leave this window open)" /min cmd /c ""%START%""
) > "%LAUNCHER%"
echo VibeRoller now starts %WHAT% by itself when you sign in to Windows: %LAUNCHER%
