@echo off
rem VibeRoller autostart for Windows: run this once (double-click) on the PC that should always host VibeRoller.
rem It writes a launcher into your Startup folder that starts this checkout's bridge\start.bat minimized
rem every time you sign in to Windows. Run it again after moving the folder. Undo: autostart-windows.cmd remove
rem (or delete VibeRoller.cmd from the Startup folder: Win+R, shell:startup).
setlocal
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "LAUNCHER=%STARTUP%\VibeRoller.cmd"
if /i "%~1"=="remove" (
  del "%LAUNCHER%" 2>nul
  echo VibeRoller no longer starts when you sign in.
  goto :eof
)
set "START=%~dp0start.bat"
if not exist "%START%" (echo start.bat not found next to this file. & pause & exit /b 1)
(
  echo @echo off
  echo rem VibeRoller autostart: starts the hub's supervisor minimized at sign-in ^(written by bridge\autostart-windows.cmd^).
  echo rem Safe to run twice: a second copy finds the hub's port taken and closes without opening a second tunnel.
  echo start "VibeRoller (leave this window open)" /min cmd /c ""%START%""
) > "%LAUNCHER%"
echo VibeRoller now starts by itself when you sign in to Windows: %LAUNCHER%
