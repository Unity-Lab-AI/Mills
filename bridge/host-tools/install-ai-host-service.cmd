@echo off
rem VibeRoller: install the VibeRollerAIHost Windows service, once, so this PC lends Unity's engines to the VibeRoller
rem server from the moment it boots (before anyone signs in). Needs administrator rights: it asks for them itself.
rem   install-ai-host-service.cmd           install (or update) + start the service; signed-in users may start / stop it
rem   install-ai-host-service.cmd remove    stop and remove the service
rem Needs VibeRollerAIHost.exe next to this file (build.cmd builds it) and Node.js installed.
setlocal
set "HERE=%~dp0"
set "EXE=%HERE%VibeRollerAIHost.exe"
set "NAME=VibeRollerAIHost"
net session >nul 2>nul
if errorlevel 1 (
  echo Asking for administrator rights...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -ArgumentList '%*' -Verb RunAs -WorkingDirectory '%HERE%'"
  exit /b
)
if /i "%~1"=="remove" (
  sc stop %NAME% >nul 2>nul
  sc delete %NAME%
  echo The %NAME% service is removed. The heart / knife now start and stop the node the old way ^(start-node.bat / the pause switch^).
  pause
  exit /b
)
if not exist "%EXE%" (echo %EXE% is missing: run build.cmd first. & pause & exit /b 1)
if not exist "%HERE%..\ai-node.js" (echo ai-node.js was not found one folder up from here. & pause & exit /b 1)
set "NODE="
for /f "delims=" %%n in ('where node 2^>nul') do if not defined NODE set "NODE=%%n"
if not defined NODE (echo Node.js was not found on the PATH: install it from https://nodejs.org and run this again. & pause & exit /b 1)
sc query %NAME% >nul 2>nul && (sc stop %NAME% >nul 2>nul & timeout /t 3 /nobreak >nul & sc delete %NAME% >nul 2>nul & timeout /t 2 /nobreak >nul)
sc create %NAME% binPath= "\"%EXE%\" \"%NODE%\"" start= auto DisplayName= "VibeRoller AI Host" obj= LocalSystem
if errorlevel 1 (echo Could not create the service. & pause & exit /b 1)
sc description %NAME% "Lends this PC's AI engines (Unity's brain, image, voice, speech) to the VibeRoller server. Starts at boot. The heart / knife on the Desktop start and stop it."
sc failure %NAME% reset= 86400 actions= restart/5000/restart/10000/restart/30000 >nul
rem Signed-in users (not only administrators) may start and stop it: that is what the heart and the knife do.
sc sdset %NAME% "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPWPDTLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)S:(AU;FA;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;WD)" >nul
rem The sign-in launcher from autostart-windows.cmd is no longer needed: the service runs from boot.
if exist "%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\VibeRoller.cmd" del "%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\VibeRoller.cmd"
if not exist "%HERE%..\ai-node.json" echo NOTE: ..\ai-node.json is missing. Copy ai-node.example.json to ai-node.json and fill in the hub address, this PC's node name and its token; the service waits until it is there.
sc start %NAME% >nul 2>nul
timeout /t 3 /nobreak >nul
sc query %NAME% | find "RUNNING" >nul && (echo The %NAME% service is installed, running, and starts at every boot. Log: ..\ai-node.log) || (echo The service is installed but not running yet. See ..\ai-node.log, or: sc query %NAME%)
echo The heart ^(start-viberoller-ai-host^) starts it, the knife ^(stop-viberoller-ai-host^) stops it, without administrator rights.
pause
