# Vibe Roller Desktop (Windows): VibeRoller Chat in its own window, and its own installer + updater.
# Run through "VibeRoller Desktop.cmd" (or the Start-menu / Desktop shortcut once installed):
#   (no option)  installed copy → open VibeRoller, then check the hub for a newer package and update itself.
#                downloaded package → an earlier install is found → update it and open; none → ask: install or portable.
#   -Install     install (or update) into %LOCALAPPDATA%\VibeRoller\Desktop, with Start-menu + Desktop shortcuts
#   -Portable    open VibeRoller from this folder, installing nothing
#   -Update      update the install that was found to this package (nothing else)
#   -Status      what is installed where, and its version
#   -Yes         no questions (install when nothing is found)
# Earlier installs are found in the standard folder and through "VibeRoller Desktop" shortcuts (older packages were
# installed in place, wherever they were unzipped). The hub's address is filled in when the hub builds the package.
param([switch]$Install, [switch]$Portable, [switch]$Update, [switch]$Status, [switch]$Yes)
$ErrorActionPreference = 'Stop'
$Hub = '__HUB__'
$Url = "$Hub/index.html?app=1"
$Here = $PSScriptRoot
$Std = Join-Path $env:LOCALAPPDATA 'VibeRoller\Desktop'
$Pkg = 'desktop-windows'

function Ver($dir) {
  $f = Join-Path $dir 'VERSION.txt'
  if (Test-Path $f) { return (Get-Content $f -Raw).Trim() }
  if (Test-Path (Join-Path $dir 'VibeRoller Desktop.cmd')) { return '(older, no VERSION.txt)' }
  return ''
}
function Same($a, $b) { return ([IO.Path]::GetFullPath($a).TrimEnd('\') -ieq [IO.Path]::GetFullPath($b).TrimEnd('\')) }
function Shortcuts { @([Environment]::GetFolderPath('Desktop'), (Join-Path ([Environment]::GetFolderPath('StartMenu')) 'Programs')) | ForEach-Object { Join-Path $_ 'VibeRoller Desktop.lnk' } }

function Launch {
  $browsers = @(
    (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
    (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Google\Chrome\Application\chrome.exe'),
    (Join-Path $env:LOCALAPPDATA 'Google\Chrome\Application\chrome.exe'),
    (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft\Edge\Application\msedge.exe'),
    (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
    (Join-Path $env:ProgramFiles 'BraveSoftware\Brave-Browser\Application\brave.exe'),
    (Join-Path $env:LOCALAPPDATA 'BraveSoftware\Brave-Browser\Application\brave.exe'))
  foreach ($b in $browsers) { if (Test-Path $b) { Start-Process $b -ArgumentList "--app=$Url", '--window-size=1280,860'; return } }
  Start-Process $Url
}

# the folders of earlier installs: the standard one, and wherever a "VibeRoller Desktop" shortcut points
function Find-Installs {
  $found = @()
  if ((Test-Path $Std) -and -not (Same $Std $Here)) { $found += $Std }
  $sh = New-Object -ComObject WScript.Shell
  foreach ($l in Shortcuts) {
    if (-not (Test-Path $l)) { continue }
    $s = $sh.CreateShortcut($l); $t = $s.TargetPath
    if ($t -match 'powershell') { if ($s.Arguments -match '-File "([^"]+)"') { $t = $Matches[1] } }
    if (-not $t) { continue }
    $d = Split-Path $t -Parent
    if ($d -and (Test-Path $d) -and -not (Same $d $Here) -and -not ($found | Where-Object { Same $_ $d })) { $found += $d }
  }
  return $found
}

function Copy-Into($to) {
  New-Item -ItemType Directory -Force -Path $to | Out-Null
  Get-ChildItem -Path $Here -File | ForEach-Object { Copy-Item $_.FullName -Destination (Join-Path $to $_.Name) -Force }
  # shortcuts start PowerShell directly (no console window flashes)
  $sh = New-Object -ComObject WScript.Shell
  foreach ($l in Shortcuts) {
    $s = $sh.CreateShortcut($l)
    $s.TargetPath = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $s.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$(Join-Path $to 'vr-desktop.ps1')`""
    $s.WorkingDirectory = $to; $s.Description = 'VibeRoller Chat in its own window'
    $s.IconLocation = (Join-Path $env:SystemRoot 'System32\shell32.dll') + ',43'; $s.Save()
  }
}

# an installed copy keeps itself current: the hub names the package version it would hand out now
function Self-Update {
  try {
    $v = Invoke-RestMethod -Uri "$Hub/api/pub/versions" -TimeoutSec 8
    $want = $v.$Pkg
    if (-not $want -or $want -eq (Ver $Std)) { return }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('vr-desktop-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    Invoke-WebRequest -Uri "$Hub/api/pub/$Pkg.zip" -OutFile (Join-Path $tmp 'p.zip') -TimeoutSec 60 -UseBasicParsing
    Expand-Archive -Path (Join-Path $tmp 'p.zip') -DestinationPath $tmp -Force
    & powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File (Join-Path $tmp 'VibeRollerDesktop\vr-desktop.ps1') -Update -Yes
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  } catch {}
}

$mine = Ver $Here
$isInstall = Same $Here $Std
if ($Status) {
  Write-Host "This package: $Here ($mine)"
  $all = Find-Installs; if (-not $all) { Write-Host 'No other install found.' }
  foreach ($d in $all) { Write-Host "Install: $d ($(Ver $d))" }
  exit 0
}
if ($Portable) { Launch; exit 0 }
if ($isInstall -and -not $Install -and -not $Update) { Launch; Self-Update; exit 0 }

$found = Find-Installs
if ($found -and -not $Install) {
  $old = $found[0]
  if ((Ver $old) -eq $mine -and (Same $old $Std) -and -not $Update) { if (-not $Update) { Launch }; exit 0 }
  Copy-Into $Std
  if (-not (Same $old $Std)) { Write-Host "Moved the older VibeRoller Desktop ($old) to $Std; the old folder can be deleted." }
  if (-not $Update) { Write-Host "VibeRoller Desktop updated to $mine."; Launch }
  exit 0
}
if ($Update) { Write-Host 'No earlier install found to update. Run without options (or with -Install) to install.'; exit 1 }
$choice = 'i'
if (-not $Install -and -not $Yes) { $choice = (Read-Host "Install VibeRoller Desktop into $Std with Start-menu and Desktop shortcuts (I), or run it portable from this folder (P)? [I/p]").Trim().ToLower() }
if ($choice.StartsWith('p')) { Launch; exit 0 }
Copy-Into $Std
Write-Host "VibeRoller Desktop is installed ($mine): Start menu and Desktop. It keeps itself up to date."
Launch
