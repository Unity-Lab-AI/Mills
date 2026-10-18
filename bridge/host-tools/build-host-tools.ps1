# Builds the VibeRoller host tools on Windows, once:  right-click > Run with PowerShell, or run build.cmd.
#   start-viberoller-host.exe  (black heart icon)  switch this machine's AI hosting on (starts VibeRoller if needed)
#   stop-viberoller-host.exe   (knife icon)        pause this machine's AI hosting; VibeRoller keeps running for everyone
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

# ── compile both tools from HostTool.cs with the C# compiler that ships with Windows ──
$csc = Get-ChildItem "$env:WINDIR\Microsoft.NET\Framework64\v4*\csc.exe", "$env:WINDIR\Microsoft.NET\Framework\v4*\csc.exe" -ErrorAction SilentlyContinue | Select-Object -Last 1
if (-not $csc) { throw 'The .NET Framework 4 C# compiler (csc.exe) was not found under Windows\Microsoft.NET.' }
$src = Join-Path $here 'HostTool.cs'
& $csc.FullName /nologo /target:winexe /optimize+ /define:START /win32icon:"$here\heart.ico" /reference:System.Windows.Forms.dll /out:"$here\start-viberoller-host.exe" $src
if ($LASTEXITCODE) { throw 'Building start-viberoller-host.exe failed.' }
& $csc.FullName /nologo /target:winexe /optimize+ /define:STOP /win32icon:"$here\knife.ico" /reference:System.Windows.Forms.dll /out:"$here\stop-viberoller-host.exe" $src
if ($LASTEXITCODE) { throw 'Building stop-viberoller-host.exe failed.' }

# ── Desktop shortcuts (they keep the icons) ──
$shell = New-Object -ComObject WScript.Shell
$desk = [Environment]::GetFolderPath('Desktop')
foreach ($t in @(@('start-viberoller-host', 'Start VibeRoller AI host'), @('stop-viberoller-host', 'Stop VibeRoller AI host'))) {
  $lnk = $shell.CreateShortcut((Join-Path $desk ($t[1] + '.lnk')))
  $lnk.TargetPath = Join-Path $here ($t[0] + '.exe'); $lnk.WorkingDirectory = $here; $lnk.Description = $t[1]; $lnk.Save()
}
Write-Host 'Built start-viberoller-host.exe (heart) and stop-viberoller-host.exe (knife), with shortcuts on the Desktop.'
