#!/usr/bin/env node
// VibeRoller supervisor — what start.bat / start.sh run. It owns the Cloudflare quick tunnel and the
// gist that advertises it, and keeps the bridge app (server.js) running underneath:
//   - app exits 75  → it asked to restart (Claude changed code/config and the current run has
//                     finished): respawned at once, under the SAME tunnel, so the public address
//                     does not change and the web app just reconnects.
//   - app exits 0/2 → clean stop / "already running in another window": everything shuts down.
//   - anything else → crash: retried after 5 s, up to 5 times in two minutes.
// Running server.js directly still works for local use, but gives no tunnel and no self-restart.
// Since 4.0.0 the Rust hub (vr-hub) is the hub and the JavaScript one runs behind it as its fallback; the Rust hub
// exiting 75 restarts both (it owns bridge/.restart then).
'use strict';

const fs = require('fs');
const path = require('path');
const { spawn, execFile } = require('child_process');
const { ROOT, readJson, writeJson, profileFromArgv, loadProfile } = require('./common');

const PROFILE = profileFromArgv();
const { config, STATE_FILE, TUNNEL_FILE, GIST_FILE } = loadProfile(PROFILE);
const NO_TUNNEL = !config.tunnel || process.argv.includes('--no-tunnel');
const tag = `[mills${PROFILE ? ':' + PROFILE : ''}]`;

let app = null, cf = null, shuttingDown = false, crashes = [];

// ---------- the Rust hub in front (native/crates/vr-hub; config "rustHub": { "enabled": true, "backPort": 7779 }) ----------
// It takes config.port (what nginx sends the portal to) and answers the modules already moved to Rust; this hub moves
// behind it to backPort on 127.0.0.1 and answers the rest. Its binary: VR_HUB_BIN, else native/dist/<target>/, else
// native/target/release/. If it is missing or keeps failing, this hub takes config.port again by itself: nobody is
// ever cut off by the Rust hub. On by default since 3.22.0 (every module passes the whole suite through it); config
// "rustHub": false (or { "enabled": false }) runs this hub alone.
const crypto = require('crypto');
const TARGET = process.platform === 'win32' ? 'x86_64-pc-windows-gnu' : 'x86_64-unknown-linux-gnu';
const hubBin = () => {
  const exe = process.platform === 'win32' ? 'vr-hub.exe' : 'vr-hub';
  return [process.env.VR_HUB_BIN, path.join(ROOT, '..', 'native', 'dist', TARGET, exe), path.join(ROOT, '..', 'native', 'target', 'release', exe)].find((f) => f && fs.existsSync(f)) || '';
};
let front = null, frontCrashes = [], rust = (() => {
  const r = config.rustHub === undefined ? {} : config.rustHub;
  if (!r || r.enabled === false || PROFILE) return null;
  const bin = hubBin();
  if (!bin) { console.log(`${tag} No vr-hub binary here (sh native/build.sh builds it; the auto-deploy does on the server): the JavaScript hub runs alone.`); return null; }
  return { bin, backPort: Number(r.backPort) || config.port + 2, secret: crypto.randomBytes(24).toString('hex'), mods: rustModules(bin) };
})();
// the modules it answers: the JavaScript hub leaves those alone (their data is the Rust hub's)
function rustModules(bin) { try { return require('child_process').execFileSync(bin, ['--modules'], { encoding: 'utf8', timeout: 10000, windowsHide: true }).trim(); } catch { return ''; } }
function startFront() {
  if (!rust || shuttingDown) return;
  front = spawn(rust.bin, ['--port', String(config.port), '--back', String(rust.backPort), '--root', ROOT], { stdio: 'inherit', windowsHide: true, env: { ...process.env, VR_FRONT_SECRET: rust.secret, VR_NODE: process.execPath, MILLS_SUPERVISED: '1' } }); // VR_NODE: the Node.js its Work runs' tools (bridge/*-mcp.js) and bridge/worktree.js run on
  front.on('exit', (code) => {
    front = null;
    if (shuttingDown) return;
    if (code === 0) return shutdown(0);
    // 75: a restart was asked for and nobody is busy (4.0.0: the Rust hub owns bridge/.restart): both hubs start again
    // on the new code, the JavaScript one behind it first
    if (code === 75) {
      console.log(`${tag} Restarting both hubs with the new code…`);
      rust.mods = rustModules(rust.bin); // a new build may answer more
      if (app) { app.removeAllListeners('exit'); app.on('exit', () => { app = null; startApp(); }); app.kill('SIGTERM'); } else startApp();
      return;
    }
    const now = Date.now(); frontCrashes = frontCrashes.filter((t) => now - t < 60000); frontCrashes.push(now);
    if (frontCrashes.length >= 3 || code === 2) { // keeps failing (or its port is taken): this hub takes the port back
      console.log(`${tag} The Rust hub stopped ${frontCrashes.length} time(s) (last code ${code}): the JavaScript hub takes port ${config.port} back by itself.`);
      rust = null;
      if (app) { app.removeAllListeners('exit'); app.on('exit', () => { app = null; startApp(); }); app.kill('SIGTERM'); } else startApp();
      return;
    }
    console.log(`${tag} The Rust hub exited with code ${code}; starting it again.`);
    setTimeout(startFront, 1000);
  });
}

// ---------- bridge app ----------
// Handed off (bridge/.handoff, written by bridge/handoff.js switch): the hub lives on the server now, and this machine
// runs the AI host (bridge/ai-node.js) instead, lending its Claude and Unity's engines to it. No tunnel then.
const HANDED_OFF = () => fs.existsSync(path.join(ROOT, '.handoff'));
function startApp() {
  if (HANDED_OFF()) {
    console.log(`${tag} Handed off: the hub lives on the server; this machine runs the VibeRoller AI Host (bridge/ai-node.js). "node bridge/handoff.js undo" brings the hub back here.`);
    app = spawn(process.execPath, [path.join(ROOT, 'ai-node.js')], { stdio: 'inherit', windowsHide: true });
    app.on('exit', (code) => {
      app = null;
      if (shuttingDown) return;
      if (code === 0 || code === 2) return shutdown(code); // 2 = another AI host already runs here
      console.log(`${tag} AI host exited with code ${code}; restarting in 5 s.`);
      setTimeout(startApp, 5000);
    });
    return;
  }
  app = spawn(process.execPath, [path.join(ROOT, 'server.js'), ...process.argv.slice(2)], {
    stdio: 'inherit', env: { ...process.env, MILLS_SUPERVISED: '1', ...(rust ? { VR_BACK_PORT: String(rust.backPort), VR_FRONT_SECRET: rust.secret, VR_FRONT_MODULES: rust.mods } : {}) }, windowsHide: true,
  });
  if (rust && !front) startFront();
  app.on('exit', (code) => {
    app = null;
    if (shuttingDown) return;
    if (code === 75) { console.log(`${tag} Bridge restarting with the new code…`); return startApp(); }
    if (code === 0 || code === 2) return shutdown(code);
    const now = Date.now();
    crashes = crashes.filter((t) => now - t < 120000); crashes.push(now);
    if (crashes.length >= 5) {
      console.log(`${tag} Bridge crashed ${crashes.length} times in two minutes; giving up. Fix the error above, then run start.bat again.`);
      return shutdown(1);
    }
    console.log(`${tag} Bridge exited with code ${code}; restarting in 5 s.`);
    setTimeout(startApp, 5000);
  });
}

// ---------- tunnel ----------
// The web app finds the current tunnel URL in a gist named "Mills bridge address" (or
// "Mills bridge address (<profile>)") on the host GitHub account (the one gh is logged in to), created here on first run. Each
// profile has its own gist and its own file name (mills-bridge-<profile>.json). The gist id is
// kept in tunnel.json (older bridges kept it in state.json, which is read as a fallback).
function publishUrl(publicUrl) {
  const content = JSON.stringify({ url: publicUrl, updated: new Date().toISOString() });
  const gistId = config.gistId || readJson(TUNNEL_FILE, {}).gistId || readJson(STATE_FILE, {}).gistId || '';
  const description = 'Mills bridge address' + (PROFILE ? ` (${PROFILE})` : '');
  const args = gistId
    ? ['api', '-X', 'PATCH', `gists/${gistId}`, '-f', `files[${GIST_FILE}][content]=${content}`]
    : ['api', '-X', 'POST', 'gists', '-f', `description=${description}`, '-F', 'public=true',
       '-f', `files[${GIST_FILE}][content]=${content}`, '--jq', '.id'];
  execFile('gh', args, (err, stdout) => {
    if (err) return console.log(`${tag} Could not publish tunnel URL (is gh logged in?): ` + err.message);
    writeJson(TUNNEL_FILE, { gistId: gistId || stdout.trim(), url: publicUrl, updated: new Date().toISOString() });
    console.log(`${tag} Published tunnel URL for the web app.`);
  });
}
// Two kinds of tunnel. Default: a QUICK tunnel (random *.trycloudflare.com address, published to the gist
// each start). With "tunnelName" + "tunnelHostname" in config.json: a NAMED tunnel (the host ran
// `cloudflared tunnel login`, `cloudflared tunnel create <name>` and `cloudflared tunnel route dns <name> <hostname>`
// once), whose address never changes; it is published the same way, and the web app must be deployed
// with the MILLS_HUB_HOST repository variable set to https://<hostname> so its security policy allows it.
function startTunnel() {
  const named = !!config.tunnelName;
  const args = named
    ? ['tunnel', '--no-autoupdate', 'run', '--url', `http://127.0.0.1:${config.port}`, String(config.tunnelName)]
    : ['tunnel', '--config', path.join(ROOT, 'cloudflared-quick.yml'), '--no-autoupdate', '--url', `http://127.0.0.1:${config.port}`];
  cf = spawn(config.cloudflared, args, { windowsHide: true });
  let announced = false;
  if (named) {
    const host = String(config.tunnelHostname || '').replace(/^https?:\/\//, '').replace(/\/+$/, '');
    if (!host) console.log(`${tag} tunnelName is set but tunnelHostname is not: add "tunnelHostname": "<hostname>" to config.json so the address can be published.`);
    else { announced = true; console.log(`${tag} Public URL (named tunnel): https://${host}`); publishUrl('https://' + host); }
  }
  const scan = (d) => {
    const m = /https:\/\/[a-z0-9-]+\.trycloudflare\.com/.exec(String(d));
    if (m && !announced) { announced = true; console.log(`${tag} Public URL: ` + m[0]); publishUrl(m[0]); }
  };
  cf.stdout.on('data', scan); cf.stderr.on('data', scan);
  cf.on('error', (e) => console.log(`${tag} cloudflared failed to start: ` + e.message));
  cf.on('close', (code) => {
    cf = null;
    if (shuttingDown) return;
    console.log(`${tag} Tunnel exited (${code}); restarting in 5 s.`);
    setTimeout(startTunnel, 5000);
  });
}

// ---------- lifecycle ----------
function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  if (app) {
    if (process.platform === 'win32') spawn('taskkill', ['/pid', String(app.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    else app.kill('SIGTERM');
  }
  if (cf) cf.kill();
  if (front) { try { front.kill('SIGTERM'); } catch {} }
  setTimeout(() => process.exit(code), 600);
}
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => shutdown(0));
process.on('exit', () => { if (cf) cf.kill(); if (app) app.kill(); if (front) front.kill(); });

startApp();
// Open the tunnel only once the app has survived startup: if its port was already taken (another
// window has this bridge) we must not overwrite that bridge's published address. Never after a hand-off.
if (!NO_TUNNEL && !HANDED_OFF()) setTimeout(() => { if (app && !shuttingDown && !HANDED_OFF()) startTunnel(); }, 1500);
