#!/usr/bin/env node
// Restart the Mills hub on the host PC. Claude runs this right after changing hub code or config,
// so nobody has to go find the window.
//
//   node restart.js              graceful: the hub restarts itself as soon as nobody's Claude run
//                                is in progress — under the same tunnel, same public address,
//                                nobody signed out.
//   node restart.js --hard       close the hub window and relaunch start.bat (new tunnel address,
//                                found again by the web app automatically). For a wedged hub.
//   node restart.js --hard --delay 30
//                                same, 30 s from now in a detached helper, so the Claude run that
//                                calls this can finish its reply first. Progress: bridge/restart.log
//   node restart.js --stop       close the hub window, no relaunch.
//   --also-port <n>              (with --hard/--stop) also close whatever bridge window listens on
//                                port <n>, e.g. an old one-bridge-per-person window.
'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');
const { spawn, execFileSync } = require('child_process');
const { ROOT, loadProfile } = require('./common');

const argv = process.argv.slice(2);
const STOP = argv.includes('--stop');
const HARD = argv.includes('--hard') || STOP;
const NOW = argv.includes('--now'); // internal: "I am the detached helper"
const opt = (name) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : undefined; };
const DELAY = Number(opt('--delay')) || 0;
const { config, RESTART_FLAG } = loadProfile('');
const ports = [config.port, ...argv.flatMap((a, i) => (a === '--also-port' ? [Number(argv[i + 1])] : [])).filter(Boolean)];
const LOG = path.join(ROOT, 'restart.log');
const WIN = process.platform === 'win32';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function log(msg) {
  const line = `${new Date().toISOString()} ${msg}`;
  console.log(line);
  try { fs.appendFileSync(LOG, line + '\n'); } catch {}
}
function running(port) {
  return new Promise((resolve) => {
    const req = http.get({ host: '127.0.0.1', port, path: '/api/status', timeout: 1500 }, (res) => { res.resume(); resolve(res.statusCode === 200); });
    req.on('error', () => resolve(false));
    req.on('timeout', () => { req.destroy(); resolve(false); });
  });
}
const ps = (cmd) => execFileSync('powershell', ['-NoProfile', '-Command', cmd], { encoding: 'utf8', windowsHide: true }).trim();
function listenerPid(port) {
  if (WIN) return Number(ps(`Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty OwningProcess`)) || 0;
  try { return Number(execFileSync('lsof', ['-t', `-iTCP:${port}`, '-sTCP:LISTEN'], { encoding: 'utf8' }).split('\n')[0]) || 0; } catch { return 0; }
}
// A window is cmd.exe (start*.bat) → node run.js → node server.js. Climb from the listener to the
// top of that chain so the whole window closes. Stops at anything that is not ours.
function windowRoot(pid) {
  if (!WIN) return pid;
  const list = JSON.parse(ps('Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress') || '[]');
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
function launch() {
  if (WIN) spawn('cmd.exe', ['/c', 'start "" start.bat'], { cwd: ROOT, detached: true, stdio: 'ignore', windowsVerbatimArguments: true }).unref();
  else spawn('sh', ['start.sh'], { cwd: ROOT, detached: true, stdio: 'ignore' }).unref();
}

(async () => {
  if (!HARD) {
    if (!(await running(config.port))) return log('Hub is not running; nothing to restart. Start it with start.bat.');
    fs.writeFileSync(RESTART_FLAG, new Date().toISOString() + '\n');
    return log('Restart requested — the hub restarts itself (same address) as soon as no Claude run is in progress.');
  }
  if (DELAY && !NOW) {
    spawn(process.execPath, [__filename, ...argv, '--now'], { detached: true, stdio: 'ignore', windowsHide: true }).unref();
    return log(`${STOP ? 'Stop' : 'Hard restart'} in ${DELAY} s (detached helper, ports ${ports.join(', ')}). Progress: bridge/restart.log`);
  }
  if (DELAY) await sleep(DELAY * 1000);
  for (const port of ports) {
    const pid = listenerPid(port);
    if (!pid) { log(`port ${port}: nothing listening.`); continue; }
    const root = windowRoot(pid);
    try { killTree(root); log(`port ${port}: stopped (listener pid ${pid}, window root ${root}).`); }
    catch (e) { log(`port ${port}: could not stop pid ${root}: ${e.message}`); }
  }
  if (STOP) return;
  await sleep(2500);
  launch();
  log('Launched start.bat in a new window.');
})().catch((e) => { log('restart failed: ' + e.message); process.exit(1); });
