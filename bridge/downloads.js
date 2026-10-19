'use strict';
// The Download panel (the Download button above Settings, on the site and in the app): "Vibe Roller Desktop" and
// "Vibe Roller AI Host", each for Windows and for Linux, as zip files the hub builds from its own checkout when asked.
// No build step and no dependencies: the zip writer below is the whole packaging. Every package carries the hub's own
// address, so what people download from this hub points back at this hub.
//
//   GET /api/downloads            the four packages (id, title, os, file name, size)
//   GET /api/downloads/<id>.zip   one package
//
// Vibe Roller Desktop   = VibeRoller Chat in its own window (the same app the browser installs; see web/app.js): a launcher
//                         for Chrome / Edge / Brave with a Start-menu / application-menu entry.
// Vibe Roller AI Host   = this machine's AI node (bridge/ai-node.js) that lends Unity's engines to the hub, with the
//                         VibeRollerAIHost service that starts it at boot and the heart / knife tools.
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const { ROOT } = require('./common');

// ---------- a small zip writer (deflate where it helps, one central directory, unix modes for the .sh files) ----------
const CRC = new Uint32Array(256).map((_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
function crc32(buf) { let c = 0xffffffff; for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; }
function zip(entries) {
  const d = new Date(), time = (d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1), date = ((d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate();
  const parts = [], central = []; let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.name, 'utf8'), data = Buffer.isBuffer(e.data) ? e.data : Buffer.from(e.data, 'utf8');
    const def = zlib.deflateRawSync(data, { level: 9 }), method = def.length < data.length ? 8 : 0, body = method ? def : data, crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6); local.writeUInt16LE(method, 8); local.writeUInt16LE(time, 10); local.writeUInt16LE(date, 12);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(body.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(name.length, 26); local.writeUInt16LE(0, 28);
    const cen = Buffer.alloc(46);
    cen.writeUInt32LE(0x02014b50, 0); cen.writeUInt16LE(0x031e, 4); cen.writeUInt16LE(20, 6); cen.writeUInt16LE(0x0800, 8); cen.writeUInt16LE(method, 10); cen.writeUInt16LE(time, 12); cen.writeUInt16LE(date, 14);
    cen.writeUInt32LE(crc, 16); cen.writeUInt32LE(body.length, 20); cen.writeUInt32LE(data.length, 24); cen.writeUInt16LE(name.length, 28); cen.writeUInt16LE(0, 30); cen.writeUInt16LE(0, 32);
    cen.writeUInt16LE(0, 34); cen.writeUInt16LE(0, 36); cen.writeUInt32LE((((e.exec ? 0o100755 : 0o100644) << 16) >>> 0), 38); cen.writeUInt32LE(offset, 42);
    parts.push(local, name, body); central.push(cen, name);
    offset += local.length + name.length + body.length;
  }
  const cd = Buffer.concat(central), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(0, 4); end.writeUInt16LE(0, 6); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10); end.writeUInt32LE(cd.length, 12); end.writeUInt32LE(offset, 16); end.writeUInt16LE(0, 20);
  return Buffer.concat([...parts, cd, end]);
}

// ---------- the packages ----------
const src = (rel) => fs.readFileSync(path.join(ROOT, rel));
const crlf = (s) => String(s).replace(/\r?\n/g, '\r\n');
const take = (folder, rels, exec = false) => rels.map((rel) => ({ name: folder + '/' + rel.replace(/^host-tools\//, 'host-tools/'), data: src(rel), exec }));

function aiHostExample(hub) {
  const ex = JSON.parse(src('ai-node.example.json').toString('utf8'));
  ex.hub = hub;
  return JSON.stringify(ex, null, 2) + '\n';
}
const packages = {
  'desktop-windows': { group: 'desktop', os: 'Windows', title: 'Vibe Roller Desktop', file: 'VibeRollerDesktop-windows.zip',
    build: (hub) => [
      { name: 'VibeRollerDesktop/VibeRoller Desktop.cmd', data: crlf(`@echo off
rem Vibe Roller Desktop: opens VibeRoller Chat in its own window (app mode) with Chrome, Edge or Brave, whichever is here.
rem Address of your VibeRoller: ${hub}
set "URL=${hub}/index.html?app=1"
for %%b in ("%ProgramFiles%\\Google\\Chrome\\Application\\chrome.exe" "%ProgramFiles(x86)%\\Google\\Chrome\\Application\\chrome.exe" "%LOCALAPPDATA%\\Google\\Chrome\\Application\\chrome.exe" "%ProgramFiles(x86)%\\Microsoft\\Edge\\Application\\msedge.exe" "%ProgramFiles%\\Microsoft\\Edge\\Application\\msedge.exe" "%ProgramFiles%\\BraveSoftware\\Brave-Browser\\Application\\brave.exe" "%LOCALAPPDATA%\\BraveSoftware\\Brave-Browser\\Application\\brave.exe") do if exist %%b (start "" %%b --app=%URL% --window-size=1280,860 & exit /b)
start "" "%URL%"
`) },
      { name: 'VibeRollerDesktop/Install VibeRoller Desktop.cmd', data: crlf(`@echo off
rem Puts "VibeRoller Desktop" into the Start menu and on the Desktop (shortcuts to VibeRoller Desktop.cmd next to this file).
rem Run again after moving this folder. Remove: delete the two shortcuts.
setlocal
set "HERE=%~dp0"
powershell -NoProfile -Command "$s = New-Object -ComObject WScript.Shell; foreach ($d in @([Environment]::GetFolderPath('Desktop'), (Join-Path ([Environment]::GetFolderPath('StartMenu')) 'Programs'))) { $l = $s.CreateShortcut((Join-Path $d 'VibeRoller Desktop.lnk')); $l.TargetPath = '%HERE%VibeRoller Desktop.cmd'; $l.WorkingDirectory = '%HERE%'; $l.Description = 'VibeRoller Chat in its own window'; $l.IconLocation = '%%SystemRoot%%\\System32\\shell32.dll,43'; $l.Save() }"
echo VibeRoller Desktop is in your Start menu and on your Desktop.
pause
`) },
      { name: 'VibeRollerDesktop/README.txt', data: crlf(`Vibe Roller Desktop (Windows)
=============================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: ${hub}

Best install (own icon, Start-menu entry, runs like any app): open ${hub}/index.html?app=1 in Chrome, Edge or
Brave and use the install icon at the right of the address bar (or the browser menu > "Install VibeRoller Chat").

This folder is the alternative when the browser cannot install it:
  1. Unzip this folder anywhere (e.g. Documents\\VibeRollerDesktop).
  2. Double-click "Install VibeRoller Desktop.cmd" once: it puts "VibeRoller Desktop" in your Start menu and on your Desktop.
  3. "VibeRoller Desktop" opens VibeRoller Chat in its own window with Chrome, Edge or Brave.
Needs one of those browsers. If VibeRoller is only reachable from inside the server (the portal), connect your tunnel first.
`) },
    ] },
  'desktop-linux': { group: 'desktop', os: 'Linux', title: 'Vibe Roller Desktop', file: 'VibeRollerDesktop-linux.zip',
    build: (hub) => [
      { name: 'VibeRollerDesktop/viberoller-desktop.sh', exec: true, data: `#!/usr/bin/env sh
# Vibe Roller Desktop: opens VibeRoller Chat in its own window (app mode) with Chrome, Chromium, Brave or Edge, whichever is here.
URL="${hub}/index.html?app=1"
for b in google-chrome google-chrome-stable chromium chromium-browser brave-browser brave microsoft-edge microsoft-edge-stable; do
  command -v "$b" >/dev/null 2>&1 && exec "$b" --app="$URL" --window-size=1280,860
done
exec xdg-open "$URL"
` },
      { name: 'VibeRollerDesktop/install-viberoller-desktop.sh', exec: true, data: `#!/usr/bin/env sh
# Puts "VibeRoller Desktop" into your application menu (a .desktop entry with VibeRoller's icon), for this user only.
# Run again after moving this folder. Remove: rm ~/.local/share/applications/viberoller-desktop.desktop ~/.local/bin/viberoller-desktop
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/scalable/apps"
cp "$HERE/viberoller-desktop.sh" "$HOME/.local/bin/viberoller-desktop"; chmod +x "$HOME/.local/bin/viberoller-desktop"
curl -fsSL -m 10 "${hub}/icons/mills.svg" -o "$HOME/.local/share/icons/hicolor/scalable/apps/viberoller.svg" 2>/dev/null || echo "(icon not fetched; the menu entry works without it)"
cat > "$HOME/.local/share/applications/viberoller-desktop.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=VibeRoller Desktop
Comment=VibeRoller Chat in its own window
Exec=$HOME/.local/bin/viberoller-desktop
Icon=viberoller
Terminal=false
Categories=Network;Chat;
StartupWMClass=${hub.replace(/^https?:\/\//, '').replace(/[/:].*$/, '')}__index.html
EOF
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "VibeRoller Desktop is in your application menu."
` },
      { name: 'VibeRollerDesktop/README.txt', data: `Vibe Roller Desktop (Linux)
===========================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: ${hub}

Best install (own icon, application-menu entry, runs like any app): open ${hub}/index.html?app=1 in Chrome, Chromium,
Brave or Edge and use the install icon at the right of the address bar (or the browser menu > "Install VibeRoller Chat").

This folder is the alternative when the browser cannot install it:
  1. Unzip it anywhere, e.g. ~/VibeRollerDesktop.
  2. sh install-viberoller-desktop.sh   (once: adds "VibeRoller Desktop" to your application menu, with the icon)
  3. "VibeRoller Desktop" opens VibeRoller Chat in its own window; viberoller-desktop.sh does the same from a shell.
Needs Chrome, Chromium, Brave or Edge. If VibeRoller is only reachable from inside the server (the portal), connect your tunnel first.
` },
    ] },
  'ai-host-windows': { group: 'ai-host', os: 'Windows', title: 'Vibe Roller AI Host', file: 'VibeRollerAIHost-windows.zip',
    build: (hub, cfg) => [
      ...take('VibeRollerAIHost', ['ai-node.js', 'host-relay.js', 'common.js', 'worktree.js', 'pc-tools-mcp.js', 'board-mcp.js', 'team-mcp.js', 'start-node.bat', 'host-tools/HostTool.cs', 'host-tools/AiHostService.cs', 'host-tools/build-host-tools.ps1', 'host-tools/build.cmd', 'host-tools/install-ai-host-service.cmd']),
      { name: 'VibeRollerAIHost/ai-node.example.json', data: aiHostExample(hub) },
      ...(cfg ? [{ name: 'VibeRollerAIHost/ai-node.json', data: JSON.stringify(cfg, null, 2) + '\n' }] : []),
      { name: 'VibeRollerAIHost/README.txt', data: crlf(`Vibe Roller AI Host (Windows)
=============================
Makes this PC an AI host for VibeRoller: it lends the AI engines running here (Unity's Ollama brain, and the image, voice
and speech engines) to the VibeRoller server at ${hub}. It connects OUT to the server; no port opens here.
The VibeRollerAIHost service starts it at boot, before anyone signs in. Two Desktop tools start and stop it:
the black heart (start-viberoller-ai-host.exe) and the knife (stop-viberoller-ai-host.exe). Stopping affects only
this PC's AI hosting: VibeRoller keeps running for everyone, Unity says her AI host is offline until it is back.

Needs: Node.js (https://nodejs.org), and the engines themselves (Ollama etc.) running on this PC.

  1. Unzip this folder somewhere permanent, e.g. C:\\VibeRollerAIHost (not Downloads).
  2. Copy ai-node.example.json to ai-node.json and fill in: hub (already ${hub}), name (this PC's node name) and token.
     The VibeRoller admin gives you name + token: they are what the hub has under aiNodes in bridge\\config.local.json.
  3. Double-click host-tools\\build.cmd once. It builds the heart, the knife and the service with the C# compiler that
     comes with Windows, puts "Start VibeRoller AI Host" and "Stop VibeRoller AI Host" on your Desktop, and offers to
     install the service (asks for administrator rights once). Say Y: the service starts now and at every boot.
  4. Check: ai-node.log next to ai-node.js says it is lending its engines; in VibeRoller, Unity answers again.

Later: install-ai-host-service.cmd again after moving the folder; install-ai-host-service.cmd remove to take it out.
Without the service (no administrator rights), start-node.bat runs the node in a window, and the heart / knife pause and
resume it.
`) },
    ] },
  'ai-host-linux': { group: 'ai-host', os: 'Linux', title: 'Vibe Roller AI Host', file: 'VibeRollerAIHost-linux.zip',
    build: (hub, cfg) => [
      ...take('VibeRollerAIHost', ['ai-node.js', 'host-relay.js', 'common.js', 'worktree.js', 'pc-tools-mcp.js', 'board-mcp.js', 'team-mcp.js', 'host-tools/viberoller-ai-host.service']),
      ...take('VibeRollerAIHost', ['start-node.sh', 'host-tools/install-ai-host-service.sh', 'host-tools/start-viberoller-host.sh', 'host-tools/stop-viberoller-host.sh'], true),
      { name: 'VibeRollerAIHost/ai-node.example.json', data: aiHostExample(hub) },
      ...(cfg ? [{ name: 'VibeRollerAIHost/ai-node.json', data: JSON.stringify(cfg, null, 2) + '\n' }] : []),
      { name: 'VibeRollerAIHost/README.txt', data: `Vibe Roller AI Host (Linux)
===========================
Makes this machine an AI host for VibeRoller: it lends the AI engines running here (Unity's Ollama brain, and the image,
voice and speech engines) to the VibeRoller server at ${hub}. It connects OUT to the server; no port opens here.
The viberoller-ai-host systemd service starts it at boot, before anyone signs in. Two tools start and stop it:
the heart (host-tools/start-viberoller-host.sh) and the knife (host-tools/stop-viberoller-host.sh). Stopping affects only
this machine's AI hosting: VibeRoller keeps running for everyone, Unity says her AI host is offline until it is back.

Needs: Node.js, and the engines themselves (Ollama etc.) running on this machine.

  1. Unzip this folder somewhere permanent, e.g. ~/VibeRollerAIHost.
  2. cp ai-node.example.json ai-node.json and fill in: hub (already ${hub}), name (this machine's node name) and token.
     The VibeRoller admin gives you name + token: they are what the hub has under aiNodes in bridge/config.local.json.
  3. sudo sh host-tools/install-ai-host-service.sh    (once: installs + starts the service, at every boot from now on;
     your user may start / stop it without a password)
  4. Check: journalctl -u viberoller-ai-host -f says it is lending its engines; in VibeRoller, Unity answers again.

Later: run the install again after moving the folder; sudo sh host-tools/install-ai-host-service.sh remove takes it out.
Without the service (no sudo), sh start-node.sh runs the node in a terminal, and the heart / knife pause and resume it.
` },
    ] },
};

const cache = new Map(); // id → { hub, data }  (the plain packages; one with a host's config inside is built fresh and never cached)
function build(id, hub, cfg) {
  if (cfg) return zip(packages[id].build(hub, cfg));
  const c = cache.get(id);
  if (c && c.hub === hub) return c.data;
  const data = zip(packages[id].build(hub));
  cache.set(id, { hub, data });
  return data;
}
function list(hub) {
  return Object.entries(packages).map(([id, p]) => ({ id, group: p.group, os: p.os, title: p.title, file: p.file, bytes: build(id, hub).length }));
}
// Signed-in people only (the hub routes here after its session check). hub = this hub's address for the packages.
// ?host=<id> puts that person's own AI host config (Settings → My AI hosts) into the AI Host package: unzip and run.
function route(p, req, res, hub, hostConfig = null) {
  if (req.method !== 'GET') return false;
  const m = /^\/api\/downloads\/([a-z-]+)\.zip(?:\?.*)?$/.exec(p);
  if (p !== '/api/downloads' && (!m || !packages[m[1]])) return false;
  try { // everything is built before the first byte goes out, so a missing file is a clean 500, never a half answer
    if (!m) { const body = JSON.stringify({ items: list(hub) }); res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); res.end(body); return true; }
    const hostId = new URL(req.url, 'http://x').searchParams.get('host');
    let cfg = null;
    if (hostId && packages[m[1]].group === 'ai-host') { if (!hostConfig) throw new Error('No host config available.'); cfg = hostConfig(hostId); }
    const data = build(m[1], hub, cfg);
    res.writeHead(200, { 'Content-Type': 'application/zip', 'Content-Length': data.length, 'Content-Disposition': `attachment; filename="${cfg ? packages[m[1]].file.replace('.zip', '-' + cfg.nickname + '.zip') : packages[m[1]].file}"`, 'Cache-Control': 'no-store' });
    res.end(data);
  } catch (e) {
    res.writeHead(500, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
    res.end(JSON.stringify({ error: 'Could not build the package: ' + e.message }));
  }
  return true;
}

module.exports = { route, list, zip, packages };
