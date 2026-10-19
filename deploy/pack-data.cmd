@echo off
rem VibeRoller: pack this hub's DATA for the move to the server, exactly as it is (accounts with their password hashes
rem and device locks, chat, keys, board, notes, pictures, saved sessions, settings). Nothing is reset or changed, and
rem no code is included. Run on the host PC from the VibeRoller folder, WITH THE HUB STOPPED (close its window first):
rem   deploy\pack-data.cmd      → %USERPROFILE%\Desktop\viberoller-data-<date>.tar.gz  (copy it to the server, then
rem                               there: sudo sh deploy/restore-data.sh <that file>)
setlocal
cd /d "%~dp0.."
curl -s -m 2 http://127.0.0.1:7777/api/status >nul 2>nul && (echo The hub is still running here. Stop it first ^(close its window^), then run this again. & pause & exit /b 1)
where tar >nul 2>nul || (echo tar.exe is missing ^(it comes with Windows 10 and later^). & pause & exit /b 1)
for /f %%d in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd-HHmm"') do set STAMP=%%d
set "OUT=%USERPROFILE%\Desktop\viberoller-data-%STAMP%.tar.gz"
setlocal enabledelayedexpansion
set LIST=
for %%p in (bridge\auth.json bridge\state.json bridge\hub-state.json bridge\usage.json bridge\assistant.json bridge\providers.json bridge\forgejo.json bridge\forgejo-users.json bridge\server-owners.json bridge\profiles bridge\profiles-deleted Chat Archive Board Notes Avatars Images Api) do if exist "%%p" set LIST=!LIST! "%%p"
if "!LIST!"=="" (echo Nothing to pack here: no data folders found next to bridge\. & pause & exit /b 1)
tar -czf "%OUT%" --exclude="*.tmp" --exclude="*.log" !LIST!
if errorlevel 1 (echo Packing failed. & pause & exit /b 1)
echo Packed:!LIST!
echo.
echo → %OUT%
echo Copy that file to the server, then there: sudo sh deploy/restore-data.sh viberoller-data-%STAMP%.tar.gz
echo (bridge\config.json, bridge\tunnel.json and the code are not included on purpose.)
pause
