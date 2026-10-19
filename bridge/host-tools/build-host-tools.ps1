# Builds the VibeRoller AI host tools on Windows, once:  right-click > Run with PowerShell, or run build.cmd.
#   start-viberoller-ai-host.exe  (black heart icon)  start this PC's AI host (the VibeRollerAIHost service, or the node / hub)
#   stop-viberoller-ai-host.exe   (knife icon)        stop it; VibeRoller on the server keeps running for everyone
#   VibeRollerAIHost.exe          (heart icon)        the Windows service itself (install-ai-host-service.cmd registers it to start at boot)
# Uses only what Windows already has: .NET Framework's C# compiler (csc.exe) and System.Drawing for the icons.
# Output lands next to this script; it also puts shortcuts on the Desktop. Re-run any time (it overwrites).
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Add-Type -AssemblyName System.Drawing

function Save-Ico([System.Drawing.Bitmap]$bmp, [string]$path) {
  # an .ico holding one 256 px PNG image (Windows Vista and later read these)
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $png = $ms.ToArray()
  $fs = [System.IO.File]::Create($path); $w = New-Object System.IO.BinaryWriter($fs)
  $w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]1)          # header: reserved, type icon, one image
  $w.Write([Byte]0); $w.Write([Byte]0); $w.Write([Byte]0); $w.Write([Byte]0) # 256 x 256, no palette
  $w.Write([UInt16]1); $w.Write([UInt16]32); $w.Write([UInt32]$png.Length); $w.Write([UInt32]22)
  $w.Write($png); $w.Close(); $fs.Close()
}
function New-Canvas { $b = New-Object System.Drawing.Bitmap 256, 256; $g = [System.Drawing.Graphics]::FromImage($b); $g.SmoothingMode = 'AntiAlias'; $g.Clear([System.Drawing.Color]::Transparent); return @($b, $g) }

# ── the black heart: glossy black with a deep-red rim and a soft highlight ──
$b, $g = New-Canvas
$heart = New-Object System.Drawing.Drawing2D.GraphicsPath
$heart.AddBezier(128, 226, 40, 160, 6, 110, 22, 66)
$heart.AddBezier(22, 66, 40, 18, 104, 14, 128, 70)
$heart.AddBezier(128, 70, 152, 14, 216, 18, 234, 66)
$heart.AddBezier(234, 66, 250, 110, 216, 160, 128, 226)
$heart.CloseFigure()
$fill = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.Point 0, 20), (New-Object System.Drawing.Point 0, 230), [System.Drawing.Color]::FromArgb(255, 58, 50, 62), [System.Drawing.Color]::FromArgb(255, 6, 4, 8))
$g.FillPath($fill, $heart)
$rim = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 170, 12, 40)), 9
$rim.LineJoin = 'Round'; $g.DrawPath($rim, $heart)
$shine = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(90, 255, 255, 255))
$g.FillEllipse($shine, 52, 52, 54, 34)
$g.Dispose(); Save-Ico $b (Join-Path $here 'heart.ico'); $b.Dispose()

# ── the knife: a steel blade with an edge bevel, a black handle with silver rivets ──
$b, $g = New-Canvas
$g.TranslateTransform(128, 128); $g.RotateTransform(-45); $g.TranslateTransform(-128, -128)
$blade = New-Object System.Drawing.Drawing2D.GraphicsPath
$blade.AddLine(26, 120, 150, 108); $blade.AddLine(150, 108, 150, 146); $blade.AddBezier(150, 146, 100, 146, 52, 140, 26, 120); $blade.CloseFigure()
$steel = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.Point 0, 106), (New-Object System.Drawing.Point 0, 148), [System.Drawing.Color]::FromArgb(255, 245, 247, 252), [System.Drawing.Color]::FromArgb(255, 120, 126, 140))
$g.FillPath($steel, $blade)
$g.DrawPath((New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 70, 74, 86)), 3), $blade)
$g.DrawLine((New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(160, 255, 255, 255)), 3), 40, 122, 146, 113)
$g.FillRectangle((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 150, 154, 166))), 148, 100, 12, 54) # bolster
$handle = New-Object System.Drawing.Drawing2D.GraphicsPath
$handle.AddArc(158, 106, 84, 42, 270, 180); $handle.AddLine(200, 148, 158, 146); $handle.AddLine(158, 146, 158, 108); $handle.CloseFigure()
$g.FillPath((New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.Point 0, 104), (New-Object System.Drawing.Point 0, 150), [System.Drawing.Color]::FromArgb(255, 52, 48, 56), [System.Drawing.Color]::FromArgb(255, 4, 4, 6))), $handle)
$g.DrawPath((New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 170, 12, 40)), 4), $handle)
foreach ($x in 178, 206) { $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 210, 214, 224))), $x, 120, 12, 12) }
$g.Dispose(); Save-Ico $b (Join-Path $here 'knife.ico'); $b.Dispose()

# ── compile the tools with the C# compiler that ships with Windows ──
$csc = Get-ChildItem "$env:WINDIR\Microsoft.NET\Framework64\v4*\csc.exe", "$env:WINDIR\Microsoft.NET\Framework\v4*\csc.exe" -ErrorAction SilentlyContinue | Select-Object -Last 1
if (-not $csc) { throw 'The .NET Framework 4 C# compiler (csc.exe) was not found under Windows\Microsoft.NET.' }
$src = Join-Path $here 'HostTool.cs'
$refs = '/reference:System.Windows.Forms.dll', '/reference:System.ServiceProcess.dll'
& $csc.FullName /nologo /target:winexe /optimize+ /define:START /win32icon:"$here\heart.ico" @refs /out:"$here\start-viberoller-ai-host.exe" $src
if ($LASTEXITCODE) { throw 'Building start-viberoller-ai-host.exe failed.' }
& $csc.FullName /nologo /target:winexe /optimize+ /define:STOP /win32icon:"$here\knife.ico" @refs /out:"$here\stop-viberoller-ai-host.exe" $src
if ($LASTEXITCODE) { throw 'Building stop-viberoller-ai-host.exe failed.' }
# the service: a console-subsystem exe (services must not be winexe), with the heart as its icon
& $csc.FullName /nologo /target:exe /optimize+ /win32icon:"$here\heart.ico" /reference:System.ServiceProcess.dll /out:"$here\VibeRollerAIHost.exe" (Join-Path $here 'AiHostService.cs')
if ($LASTEXITCODE) { throw 'Building VibeRollerAIHost.exe failed.' }
# older names from before the service existed: remove them so there is one heart and one knife
foreach ($old in 'start-viberoller-host.exe', 'stop-viberoller-host.exe') { Remove-Item (Join-Path $here $old) -ErrorAction SilentlyContinue }
$desk = [Environment]::GetFolderPath('Desktop')
foreach ($old in 'Start VibeRoller AI host.lnk', 'Stop VibeRoller AI host.lnk') { Remove-Item (Join-Path $desk $old) -ErrorAction SilentlyContinue }

# ── Desktop shortcuts (they keep the icons) ──
$shell = New-Object -ComObject WScript.Shell
foreach ($t in @(@('start-viberoller-ai-host', 'Start VibeRoller AI Host'), @('stop-viberoller-ai-host', 'Stop VibeRoller AI Host'))) {
  $lnk = $shell.CreateShortcut((Join-Path $desk ($t[1] + '.lnk')))
  $lnk.TargetPath = Join-Path $here ($t[0] + '.exe'); $lnk.WorkingDirectory = $here; $lnk.Description = $t[1]; $lnk.Save()
}
Write-Host 'Built start-viberoller-ai-host.exe (heart), stop-viberoller-ai-host.exe (knife) and VibeRollerAIHost.exe (the service), with shortcuts on the Desktop.'
Write-Host 'Next: install-ai-host-service.cmd registers the service to start at boot (it asks for administrator rights once).'
