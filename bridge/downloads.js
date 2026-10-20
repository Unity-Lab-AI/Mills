'use strict';
// The Download panel (the Download button above Settings, on the site and in the app): "Vibe Roller Desktop" and
// "Vibe Roller AI Host", each for Windows and for Linux, as zip files the hub builds from its own checkout when asked.
// No build step and no dependencies: the zip writer below is the whole packaging. Every package carries the hub's own
// address, so what people download from this hub points back at this hub.
//
//   GET /api/downloads            the four packages (id, title, os, file name, size, version)   signed-in
//   GET /api/downloads/<id>.zip   one package                                                    signed-in
//   GET /api/pub/versions         every package's version (no secrets in it)                     public
//   GET /api/pub/desktop-<os>.zip the Desktop package (it holds nothing but this hub's address)  public
//
// Each package is its own installer, launcher and updater, for Windows and Linux: run it and it finds an earlier install
// (and updates it, keeping that machine's own files), or asks: install, or run portable. Every package carries
// VERSION.txt: the hub's version + a fingerprint of the package's contents, so an install updates only when something in
// it really changed. An installed Desktop checks /api/pub/versions at every start and updates itself; an AI host updates
// its code from the hub by itself (bridge/ai-node.js), and a newer package run on that machine updates the rest.
//
// Vibe Roller Desktop   = VibeRoller Chat in its own window (the same app the browser installs; see web/app.js): a launcher
//                         for Chrome / Edge / Brave with a Start-menu / application-menu entry (bridge/desktop/).
// Vibe Roller AI Host   = this machine's AI host (bridge/ai-node.js), with its setup (bridge/vr-setup.js), the
//                         VibeRollerAIHost service that starts it at boot and the heart / knife tools.
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const crypto = require('crypto');
const { ROOT } = require('./common');
const projectVersion = () => { try { return String(JSON.parse(fs.readFileSync(path.join(ROOT, '..', 'versions.json'), 'utf8')).project || ''); } catch { return ''; } };

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
const withHub = (rel, hub) => src(rel).toString('utf8').split('__HUB__').join(hub);
const HOST_CODE = ['ai-node.js', 'host-relay.js', 'common.js', 'worktree.js', 'pc-tools-mcp.js', 'board-mcp.js', 'team-mcp.js', 'vr-setup.js', 'vr-host.js'];
const packages = {
  'desktop-windows': { group: 'desktop', os: 'Windows', title: 'Vibe Roller Desktop', file: 'VibeRollerDesktop-windows.zip',
    build: (hub) => [
      { name: 'VibeRollerDesktop/VibeRoller Desktop.cmd', data: crlf(`@echo off
rem Vibe Roller Desktop: VibeRoller Chat in its own window, and its own installer + updater (README.txt).
rem Double-click: finds an earlier install and updates it, or asks: install or portable. Options: -Install -Portable -Update -Status
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0vr-desktop.ps1" %*
if errorlevel 1 pause
`) },
      { name: 'VibeRollerDesktop/vr-desktop.ps1', data: crlf(withHub('desktop/vr-desktop.ps1', hub)) },
      { name: 'VibeRollerDesktop/README.txt', data: crlf(`Vibe Roller Desktop (Windows)
=============================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: ${hub}

This folder is the installer, the app and the updater in one:
  - Double-click "VibeRoller Desktop.cmd". If VibeRoller Desktop is already installed (any earlier version, wherever
    it was), it is updated to this one and opens. If not, it asks: Install (into %LOCALAPPDATA%\\VibeRoller\\Desktop,
    with Start-menu and Desktop shortcuts) or Portable (runs from this folder, installs nothing).
  - Installed, it keeps itself up to date: at every start it asks ${hub} for the current package and updates itself
    when something changed. The window itself always shows the current VibeRoller (it is the hub's own page).
  - Options (from a command prompt): -Install, -Portable, -Update, -Status.
Needs Chrome, Edge or Brave (one of them is almost always there).

Also possible: open ${hub}/index.html?app=1 in Chrome, Edge or Brave and use the install icon in the address bar.
`) },
    ] },
  'desktop-linux': { group: 'desktop', os: 'Linux', title: 'Vibe Roller Desktop', file: 'VibeRollerDesktop-linux.zip',
    build: (hub) => [
      { name: 'VibeRollerDesktop/viberoller-desktop.sh', exec: true, data: withHub('desktop/viberoller-desktop.sh', hub) },
      { name: 'VibeRollerDesktop/README.txt', data: `Vibe Roller Desktop (Linux)
===========================
VibeRoller Chat in its own window: servers, channels, groups, private messages, calls and Unity, signed in with your
VibeRoller account. Your VibeRoller: ${hub}

This folder is the installer, the app and the updater in one:
  - sh viberoller-desktop.sh   If VibeRoller Desktop is already installed (any earlier version), it is updated to this
    one and opens. If not, it asks: Install (into ~/.local/share/viberoller/desktop, with an application-menu entry and
    ~/.local/bin/viberoller-desktop) or Portable (runs from this folder, installs nothing).
  - Installed, it keeps itself up to date: at every start it asks ${hub} for the current package and updates itself
    when something changed. The window itself always shows the current VibeRoller (it is the hub's own page).
  - Options: --install, --portable, --update, --status.
Needs Chrome, Chromium, Brave or Edge, and curl (for updates).

Also possible: open ${hub}/index.html?app=1 in Chrome, Chromium, Brave or Edge and use the install icon in the address bar.
` },
    ] },
  'ai-host-windows': { group: 'ai-host', os: 'Windows', title: 'Vibe Roller AI Host', file: 'VibeRollerAIHost-windows.zip',
    build: (hub, cfg) => [
      { name: 'VibeRollerAIHost/VibeRoller AI Host.cmd', data: crlf(`@echo off
rem Vibe Roller AI Host: installs, runs and updates this PC's AI host (README.txt). Options: --install --portable --update --status --yes
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
node "%~dp0vr-setup.js" %*
if errorlevel 1 pause
`) },
      ...take('VibeRollerAIHost', [...HOST_CODE, 'start-node.bat', 'host-tools/HostTool.cs', 'host-tools/AiHostService.cs', 'host-tools/build-host-tools.ps1', 'host-tools/build.cmd', 'host-tools/install-ai-host-service.cmd']),
      { name: 'VibeRollerAIHost/ai-node.example.json', data: aiHostExample(hub) },
      ...(cfg ? [{ name: 'VibeRollerAIHost/ai-node.json', data: JSON.stringify(cfg, null, 2) + '\n' }] : []),
      { name: 'VibeRollerAIHost/README.txt', data: crlf(`Vibe Roller AI Host (Windows)
=============================
Makes this PC an AI host for VibeRoller at ${hub}: it lends its AI (its Claude / ChatGPT login, local models, Unity's
engines) without sharing the computer itself. It connects OUT to the server; no port opens here.

This folder is the installer, the host and the updater in one. Needs Node.js (https://nodejs.org).
  - Double-click "VibeRoller AI Host.cmd".
      An AI host is already installed here (any earlier version: the standard folder, the VibeRollerAIHost service's
      folder, C:\\VibeRollerAIHost …) → it is updated to this package; its ai-node.json, logs and settings stay, and the
      running host switches to the new version at its next quiet moment (it never cuts off an answer).
      A host that runs from a VibeRoller git checkout is left alone: it updates itself.
      Nothing installed → it asks: Install (into %LOCALAPPDATA%\\VibeRoller\\AIHost) or Portable (runs from this folder).
  - Your own host's package (VibeRoller → Settings → Connections → My AI hosts → download) has its ai-node.json inside.
    This general one has ai-node.example.json: copy it to ai-node.json and fill in hub, id and token.
  - The control app, "heart & knife" (it replaces the separate heart and knife): after installing it opens by itself;
    later "VibeRoller AI Host.cmd" in the install folder, or the "VibeRoller AI Host" shortcut. It shows whether the
    host runs, is connected, is in use and is up to date; [s] start, [x] stop, [r] restart, [p] pause / resume,
    [u] update now, [l] log, [c] every setting (hub, token, updates, reconnect, who may use each provider: the team
    group and / or named users, usage caps, engines, the security switches), [b] start at boot: the service (from
    boot, administrator once), at sign-in, or off. For scripts: node vr-host.js status | start | stop | set … .
  - The service and the shortcut: host-tools\\build.cmd builds them with the C# compiler that comes with Windows
    ([b] in the control app does it for you).
  - Updates: the host updates its code from the hub by itself, between answers. By hand: [u] in the control app,
    "VibeRoller AI Host.cmd" from a newer download, or node ai-node.js --update in the install folder.
  - Options (from a command prompt): --install, --portable, --update, --status, --yes.
`) },
    ] },
  'ai-host-linux': { group: 'ai-host', os: 'Linux', title: 'Vibe Roller AI Host', file: 'VibeRollerAIHost-linux.zip',
    build: (hub, cfg) => [
      { name: 'VibeRollerAIHost/viberoller-ai-host.sh', exec: true, data: `#!/usr/bin/env sh
# Vibe Roller AI Host: installs, runs and updates this machine's AI host (README.txt). Options: --install --portable --update --status --yes
command -v node >/dev/null 2>&1 || { echo "Node.js is required: https://nodejs.org"; exit 1; }
exec node "$(cd "$(dirname "$0")" && pwd)/vr-setup.js" "$@"
` },
      ...take('VibeRollerAIHost', [...HOST_CODE, 'host-tools/viberoller-ai-host.service']),
      ...take('VibeRollerAIHost', ['start-node.sh', 'host-tools/install-ai-host-service.sh', 'host-tools/viberoller-host-control.sh', 'host-tools/start-viberoller-host.sh', 'host-tools/stop-viberoller-host.sh'], true),
      { name: 'VibeRollerAIHost/ai-node.example.json', data: aiHostExample(hub) },
      ...(cfg ? [{ name: 'VibeRollerAIHost/ai-node.json', data: JSON.stringify(cfg, null, 2) + '\n' }] : []),
      { name: 'VibeRollerAIHost/README.txt', data: `Vibe Roller AI Host (Linux)
===========================
Makes this machine an AI host for VibeRoller at ${hub}: it lends its AI (its Claude / ChatGPT login, local models,
Unity's engines) without sharing the computer itself. It connects OUT to the server; no port opens here.

This folder is the installer, the host and the updater in one. Needs Node.js.
  - sh viberoller-ai-host.sh
      An AI host is already installed here (any earlier version: the standard folder, the viberoller-ai-host service's
      folder, ~/VibeRollerAIHost …) → it is updated to this package; its ai-node.json, logs and settings stay, and the
      running host switches to the new version at its next quiet moment (it never cuts off an answer).
      A host that runs from a VibeRoller git checkout is left alone: it updates itself.
      Nothing installed → it asks: Install (into ~/.local/share/viberoller/ai-host) or Portable (runs from this folder).
  - Your own host's package (VibeRoller → Settings → Connections → My AI hosts → download) has its ai-node.json inside.
    This general one has ai-node.example.json: cp it to ai-node.json and fill in hub, id and token.
  - The control app, "heart & knife": after installing it opens by itself; later viberoller-ai-host.sh in the install
    folder (or sh host-tools/viberoller-host-control.sh). It shows whether the host runs, is connected, is in use and
    is up to date; [s] start, [x] stop, [r] restart, [p] pause / resume, [u] update now, [l] log, [c] every setting
    (hub, token, updates, reconnect, who may use each provider: the team group and / or named users, usage caps,
    engines, the security switches), [b] start at boot: the system service (sudo once), a user service, or off.
    For scripts: node vr-host.js status | start | stop | set … .
  - Updates: the host updates its code from the hub by itself, between answers. By hand: [u] in the control app,
    viberoller-ai-host.sh from a newer download, or node ai-node.js --update in the install folder.
  - Options: --install, --portable, --update, --status, --yes.
` },
    ] },
};
// The package's version: the hub's version + a fingerprint of everything in it but a personal ai-node.json (and the
// hub address, which is the same for everyone here), so an install updates only when something in it changed.
function withVersion(id, entries) {
  const h = crypto.createHash('sha256');
  for (const e of entries) if (!/\/ai-node\.json$/.test(e.name)) h.update(e.name + '\0').update(Buffer.isBuffer(e.data) ? e.data : Buffer.from(e.data, 'utf8'));
  const version = `${projectVersion()}+${h.digest('hex').slice(0, 8)}`;
  const folder = entries[0].name.split('/')[0];
  return { version, entries: [...entries, { name: folder + '/VERSION.txt', data: version + '\n' }] };
}

const cache = new Map(); // id → { hub, data, version }  (the plain packages; one with a host's config inside is built fresh and never cached)
function built(id, hub, cfg) {
  if (cfg) { const v = withVersion(id, packages[id].build(hub, cfg)); return { data: zip(v.entries), version: v.version }; }
  const c = cache.get(id);
  if (c && c.hub === hub) return c;
  const v = withVersion(id, packages[id].build(hub));
  const out = { hub, data: zip(v.entries), version: v.version };
  cache.set(id, out);
  return out;
}
const build = (id, hub, cfg) => built(id, hub, cfg).data;
function list(hub) {
  return Object.entries(packages).map(([id, p]) => { const b = built(id, hub); return { id, group: p.group, os: p.os, title: p.title, file: p.file, bytes: b.data.length, version: b.version }; });
}
// Public, before any sign-in: the package versions (an installed Desktop asks at every start) and the Desktop packages
// themselves (they hold nothing but this hub's public address). The AI Host packages stay signed-in only.
// native = bridge/native-releases.js: the pre-built, signed native binaries (the Desktop app updates itself from them)
function publicRoute(p, req, res, hub, native = null) {
  if (req.method !== 'GET' || !p.startsWith('/api/pub/')) return false;
  try {
    if (p === '/api/pub/versions') {
      const body = { version: projectVersion() }; for (const id of Object.keys(packages)) body[id] = built(id, hub).version;
      if (native) body.native = native.list();
      res.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); res.end(JSON.stringify(body)); return true;
    }
    // GET /api/pub/native?app=&target= → { manifest, sig } (Ed25519 with the release key); /api/pub/native/file?…&name=
    if (native && (p === '/api/pub/native' || p === '/api/pub/native/file')) {
      const u = new URL(req.url, 'http://x'), app = String(u.searchParams.get('app') || ''), target = String(u.searchParams.get('target') || '');
      if (p === '/api/pub/native') { const s = native.signed(app, target); res.writeHead(s ? 200 : 404, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); res.end(JSON.stringify(s || { error: 'Nothing built for that app and target.' })); return true; }
      const buf = native.file(app, target, String(u.searchParams.get('name') || ''));
      if (!buf) { res.writeHead(404, { 'Content-Type': 'application/json' }); res.end('{"error":"Not a file of that release."}'); return true; }
      res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': buf.length, 'Content-Disposition': `attachment; filename="${u.searchParams.get('name')}"`, 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' }); res.end(buf);
      return true;
    }
    const m = /^\/api\/pub\/(desktop-(?:windows|linux))\.zip$/.exec(p);
    if (!m) { res.writeHead(404, { 'Content-Type': 'application/json' }); res.end('{"error":"Not found"}'); return true; }
    const b = built(m[1], hub);
    res.writeHead(200, { 'Content-Type': 'application/zip', 'Content-Length': b.data.length, 'Content-Disposition': `attachment; filename="${packages[m[1]].file}"`, 'Cache-Control': 'no-store' });
    res.end(b.data);
  } catch (e) { res.writeHead(500, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ error: 'Could not build the package.' })); }
  return true;
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

module.exports = { route, publicRoute, list, zip, packages };
