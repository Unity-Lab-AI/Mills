#!/usr/bin/env node
// VibeRoller supervisor — what start.bat / start.sh run. It owns the Cloudflare quick tunnel and the
// gist that advertises it, and keeps the bridge app (server.js) running underneath:
//   - app exits 75  → it asked to restart (Claude changed code/config and the current run has
//                     finished): respawned at once, under the SAME tunnel, so the public address
//                     does not change and the web app just reconnects.
//   - app exits 0/2 → clean stop / "already running in another window": everything shuts down.
//   - anything else → crash: retried after 5 s, up to 5 times in two minutes.
// Running server.js directly still works for local use, but gives no tunnel and no self-restart.
'use strict';

const path = require('path');
const { spawn, execFile } = require('child_process');
const { ROOT, readJson, writeJson, profileFromArgv, loadProfile } = require('./common');

const PROFILE = profileFromArgv();
const { config, STATE_FILE, TUNNEL_FILE, GIST_FILE } = loadProfile(PROFILE);
const NO_TUNNEL = !config.tunnel || process.argv.includes('--no-tunnel');
const tag = `[mills${PROFILE ? ':' + PROFILE : ''}]`;

let app = null, cf = null, shuttingDown = false, crashes = [];

// ---------- bridge app ----------
function startApp() {
  app = spawn(process.execPath, [path.join(ROOT, 'server.js'), ...process.argv.slice(2)], {
    stdio: 'inherit', env: { ...process.env, MILLS_SUPERVISED: '1' }, windowsHide: true,
  });
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
  setTimeout(() => process.exit(code), 600);
}
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => shutdown(0));
process.on('exit', () => { if (cf) cf.kill(); if (app) app.kill(); });

startApp();
// Open the tunnel only once the app has survived startup: if its port was already taken (another
// window has this bridge) we must not overwrite that bridge's published address.
if (!NO_TUNNEL) setTimeout(() => { if (app && !shuttingDown) startTunnel(); }, 1500);
