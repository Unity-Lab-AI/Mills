#!/usr/bin/env node
// Restart the Mills bridges from Gee's PC. Meant to be run by Claude right after it has changed
// bridge code or config, so nobody has to go find the bridge windows.
//
//   node restart.js                  graceful: every RUNNING bridge drops a .restart flag and
//                                    restarts itself as soon as its current Claude run has
//                                    finished — under the same tunnel, same public address.
//   node restart.js sponge           graceful, one profile ("default" = Alfreddo's bridge).
//   node restart.js --hard           close the bridge windows and relaunch them with start.bat
//                                    (new tunnel addresses). Needed for bridges that were started
//                                    before run.js existed, or that are wedged.
//   node restart.js --hard --delay 30
//                                    same, but 30 s from now in a detached helper, so the Claude
//                                    run that calls this can finish its reply first. Progress is
//                                    written to bridge/restart.log.
//   node restart.js --stop sponge    close that bridge's window and do not relaunch it.
'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');
const { spawn, execFileSync } = require('child_process');
const { ROOT, loadProfile, allProfiles } = require('./common');

const argv = process.argv.slice(2);
const STOP = argv.includes('--stop');
const HARD = argv.includes('--hard') || STOP;
const NOW = argv.includes('--now'); // internal: "I am the detached helper"
const delayIx = argv.indexOf('--delay');
const DELAY = delayIx >= 0 ? Number(argv[delayIx + 1]) || 0 : 0;
const wanted = argv.filter((a, i) => !a.startsWith('--') && argv[i - 1] !== '--delay').map((p) => (p === 'default' ? '' : p));
const LOG = path.join(ROOT, 'restart.log');
const WIN = process.platform === 'win32';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function log(msg) {
  const line = `${new Date().toISOString()} ${msg}`;
  console.log(line);
  try { fs.appendFileSync(LOG, line + '\n'); } catch {}
}

function status(port) {
  return new Promise((resolve) => {
    const req = http.get({ host: '127.0.0.1', port, path: '/api/status', timeout: 1500 }, (res) => {
      let d = ''; res.on('data', (c) => { d += c; }); res.on('end', () => { try { resolve(JSON.parse(d)); } catch { resolve(null); } });
    });
    req.on('error', () => resolve(null));
    req.on('timeout', () => { req.destroy(); resolve(null); });
  });
}

const ps = (cmd) => execFileSync('powershell', ['-NoProfile', '-Command', cmd], { encoding: 'utf8', windowsHide: true }).trim();

// Who is listening on the bridge's port. Exact, no guessing from command lines.
function listenerPid(port) {
  if (WIN) return Number(ps(`Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty OwningProcess`)) || 0;
  try { return Number(execFileSync('lsof', ['-t', `-iTCP:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).split('\n')[0]) || 0; } catch { return 0; }
}

// A bridge window is cmd.exe (start.bat) → node run.js → node server.js (older ones: cmd.exe →
// node server.js). Climb from the listener to the top of that chain so the whole window closes
// instead of sitting at "Press any key". Stops at anything that is not ours (a terminal, Explorer).
function windowRoot(pid) {
  if (!WIN) return pid;
  const raw = ps('Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress');
  const list = JSON.parse(raw || '[]');
  const byPid = new Map((Array.isArray(list) ? list : [list]).map((p) => [p.ProcessId, p]));
  let cur = byPid.get(pid);
  while (cur) {
    const parent = byPid.get(cur.ParentProcessId);
    const ours = parent && (parent.Name === 'node.exe' || (parent.Name === 'cmd.exe' && /start[\w-]*\.bat/i.test(parent.CommandLine || '')));
    if (!ours) break;
    cur = parent;
  }
  return cur ? cur.ProcessId : pid;
}

function killTree(pid) {
  if (WIN) execFileSync('taskkill', ['/pid', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  else { try { process.kill(pid, 'SIGTERM'); } catch {} }
}

function launch(profile) {
  if (WIN) {
    spawn('cmd.exe', ['/c', `start "" start.bat ${profile}`.trim()], { cwd: ROOT, detached: true, stdio: 'ignore', windowsVerbatimArguments: true }).unref();
  } else {
    spawn('sh', ['start.sh', ...(profile ? [profile] : [])], { cwd: ROOT, detached: true, stdio: 'ignore' }).unref();
  }
}

(async () => {
  const targets = [];
  for (const profile of wanted.length ? wanted : allProfiles()) {
    const { config, RESTART_FLAG } = loadProfile(profile); // exits with a message if the profile does not exist
    targets.push({ profile, name: profile || 'default', port: config.port, flag: RESTART_FLAG, running: !!(await status(config.port)) });
  }
  const explicit = wanted.length > 0;
  const picked = targets.filter((t) => t.running || explicit);
  for (const t of targets.filter((t) => !t.running && !explicit)) log(`${t.name}: not running, skipped.`);
  if (!picked.length) return log('No bridge to restart.');

  if (!HARD) {
    for (const t of picked) {
      fs.writeFileSync(t.flag, new Date().toISOString() + '\n');
      log(`${t.name}: restart requested — it restarts itself (same address) as soon as its current Claude run is finished.`);
    }
    return;
  }

  if (DELAY && !NOW) {
    spawn(process.execPath, [__filename, ...argv, '--now'], { detached: true, stdio: 'ignore', windowsHide: true }).unref();
    return log(`${STOP ? 'Stop' : 'Hard restart'} of ${picked.map((t) => t.name).join(', ')} in ${DELAY} s (detached helper). Progress: bridge/restart.log`);
  }
  if (DELAY) await sleep(DELAY * 1000);

  for (const t of picked) {
    const pid = listenerPid(t.port);
    if (!pid) { log(`${t.name}: nothing listening on port ${t.port}.`); continue; }
    const root = windowRoot(pid);
    try { killTree(root); log(`${t.name}: stopped (listener pid ${pid}, window root ${root}).`); }
    catch (e) { log(`${t.name}: could not stop pid ${root}: ${e.message}`); }
  }
  if (STOP) return;
  await sleep(2500);
  for (const t of picked) { launch(t.profile); log(`${t.name}: launched start.bat${t.profile ? ' ' + t.profile : ''} in a new window.`); }
})().catch((e) => { log('restart failed: ' + e.message); process.exit(1); });
