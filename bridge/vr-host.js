#!/usr/bin/env node
// VibeRoller AI Host · heart & knife: the one control app for this machine's AI host (Windows and Linux, in a terminal).
// It replaces the separate heart (start) and knife (stop) tools. Run it (the "VibeRoller AI Host" shortcut, or
// node vr-host.js next to ai-node.js) for the live dashboard:
//   is the host running (process, service, how it starts at boot), connected to the hub (and since when, its last
//   heartbeat, the hub's version), in use (answers in flight, the last job), its updates, who may use what, security
//   [s] start  [x] stop  [r] restart  [p] pause / resume  [u] update now  [l] log  [c] settings  [b] start at boot  [q] quit
// Settings (written to ai-node.json; the host applies them by itself at its next quiet moment): the hub's address, the
// token (never shown), auto-update and its timing, the reconnect delay, every provider (on / off, shared with the hub's
// team group and / or named users, usage caps), Unity's engines (on / off) and the security switches (Work on this
// machine, PC file access, a plain-http hub, the engine guard), plus locking the config file down.
// Start at boot: Windows: the VibeRollerAIHost service (administrator once) / at sign-in (no administrator) / off;
// Linux: the system service (sudo once) / a user service (no sudo) / off.
// Without a terminal, or for scripts, the same as commands:
//   node vr-host.js status | start | stop [--now] | restart [--now] | pause | resume | update | log
//                   boot service | sign-in | user | off      set <setting> <value>      settings
// The host itself (ai-node.js) writes ai-node.status.json and picks up the requests this app drops next to it.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const readline = require('readline');
const { spawn, spawnSync } = require('child_process');

const DIR = __dirname, WIN = process.platform === 'win32';
const F = (n) => path.join(DIR, n);
const CFG = F('ai-node.json'), STATUS = F('ai-node.status.json'), LOCK = F('ai-node.lock'), OFF = F('ai-host.off');
const SVC = WIN ? 'VibeRollerAIHost' : 'viberoller-ai-host';
const STARTUP = WIN ? path.join(process.env.APPDATA || '', 'Microsoft', 'Windows', 'Start Menu', 'Programs', 'Startup', 'VibeRoller AI Host.cmd') : '';
const USER_UNIT = WIN ? '' : path.join(os.homedir(), '.config', 'systemd', 'user', SVC + '.service');
const C = process.stdout.isTTY ? { b: '\x1b[1m', d: '\x1b[2m', r: '\x1b[31m', g: '\x1b[32m', y: '\x1b[33m', c: '\x1b[36m', x: '\x1b[0m' } : { b: '', d: '', r: '', g: '', y: '', c: '', x: '' };

const readJson = (f, d = null) => { try { return JSON.parse(fs.readFileSync(f, 'utf8')); } catch { return d; } };
function saveCfg(cfg) { // atomic, and only the owner may read it (it holds the token)
  fs.writeFileSync(CFG + '.tmp', JSON.stringify(cfg, null, 2) + '\n', { mode: 0o600 });
  fs.renameSync(CFG + '.tmp', CFG);
  if (!WIN) { try { fs.chmodSync(CFG, 0o600); } catch {} }
}
const run = (cmd, args, opts = {}) => { const r = spawnSync(cmd, args, { encoding: 'utf8', windowsHide: true, timeout: 30000, ...opts }); return { ok: r.status === 0, out: String(r.stdout || '') + String(r.stderr || ''), status: r.status }; };
const pidAlive = (pid) => { try { process.kill(pid, 0); return true; } catch (e) { return e.code === 'EPERM'; } };
const hostPid = () => { const pid = Number((() => { try { return fs.readFileSync(LOCK, 'utf8').trim(); } catch { return 0; } })()); return pid && pidAlive(pid) ? pid : 0; };
const ago = (t) => { if (!t) return 'never'; const s = Math.max(0, Math.round((Date.now() - t) / 1000)); return s < 60 ? s + ' s ago' : s < 3600 ? Math.round(s / 60) + ' min ago' : Math.round(s / 3600) + ' h ago'; };
const dur = (t) => { if (!t) return ''; const s = Math.round((Date.now() - t) / 1000), h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60; return h ? `${h} h ${m} min` : m ? `${m} min` : `${s} s`; };

// ---------- the service and start at boot ----------
function service() { // { exists, running, scope }
  if (WIN) {
    const q = run('sc', ['query', SVC]); if (!q.ok) return { exists: false, running: false };
    return { exists: true, running: /RUNNING/.test(q.out), auto: /AUTO_START/.test(run('sc', ['qc', SVC]).out), scope: 'service' };
  }
  const has = (user) => run('systemctl', [...(user ? ['--user'] : []), 'cat', SVC + '.service']).ok;
  for (const user of [false, true]) if (has(user)) {
    const a = (k) => run('systemctl', [...(user ? ['--user'] : []), k, SVC]).out.trim();
    return { exists: true, running: a('is-active') === 'active', auto: a('is-enabled') === 'enabled', scope: user ? 'user' : 'system' };
  }
  return { exists: false, running: false };
}
function bootMode(svc = service()) {
  if (svc.exists && svc.auto) return WIN ? 'service' : svc.scope === 'user' ? 'user' : 'service';
  if (WIN && fs.existsSync(STARTUP)) return 'sign-in';
  return 'off';
}
const bootLabel = (m) => ({ service: WIN ? 'the VibeRollerAIHost service (from boot, before anyone signs in)' : 'the system service (from boot)', user: 'a user service (when you are signed in; from boot with lingering on)', 'sign-in': 'at sign-in (a minimized window)', off: 'not at boot (start it by hand)' }[m] || m);
function setBoot(mode) {
  const lines = [];
  const svc = service();
  if (WIN) {
    const tools = F('host-tools');
    if (mode === 'service') {
      if (!fs.existsSync(path.join(tools, 'VibeRollerAIHost.exe'))) { lines.push('Building the service program first (host-tools\\build.cmd) …'); run('cmd.exe', ['/c', path.join(tools, 'build.cmd'), 'quiet'], { stdio: 'inherit', timeout: 300000 }); }
      try { fs.rmSync(STARTUP, { force: true }); } catch {}
      lines.push('Installing the service: Windows asks for administrator rights once …');
      run('cmd.exe', ['/c', path.join(tools, 'install-ai-host-service.cmd')], { stdio: 'inherit', timeout: 300000 });
    } else {
      if (svc.exists) { lines.push('Removing the service: Windows asks for administrator rights once …'); run('cmd.exe', ['/c', path.join(tools, 'install-ai-host-service.cmd'), 'remove'], { stdio: 'inherit', timeout: 300000 }); }
      if (mode === 'sign-in') { fs.mkdirSync(path.dirname(STARTUP), { recursive: true }); fs.writeFileSync(STARTUP, `@echo off\r\ncd /d "${DIR}"\r\nstart "VibeRoller AI node" /min "${F('start-node.bat')}"\r\n`); lines.push('It starts at sign-in now (a minimized window).'); }
      else { try { fs.rmSync(STARTUP, { force: true }); } catch {} lines.push('It no longer starts by itself; start it here when you want it.'); }
    }
    return lines;
  }
  const node = process.execPath;
  if (mode === 'service') {
    if (svc.exists && svc.scope === 'user') { run('systemctl', ['--user', 'disable', '--now', SVC]); try { fs.rmSync(USER_UNIT, { force: true }); } catch {} }
    lines.push('Installing the system service (sudo asks for your password) …');
    run('sudo', ['sh', F(path.join('host-tools', 'install-ai-host-service.sh'))], { stdio: 'inherit', timeout: 300000 });
  } else {
    if (svc.exists && svc.scope === 'system') { lines.push('Removing the system service (sudo asks for your password) …'); run('sudo', ['sh', F(path.join('host-tools', 'install-ai-host-service.sh')), 'remove'], { stdio: 'inherit', timeout: 300000 }); }
    if (mode === 'user') {
      fs.mkdirSync(path.dirname(USER_UNIT), { recursive: true });
      fs.writeFileSync(USER_UNIT, `[Unit]\nDescription=VibeRoller AI Host (user service)\nAfter=network-online.target\n\n[Service]\nWorkingDirectory=${DIR}\nExecStart=${node} ${F('ai-node.js')}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n`);
      run('systemctl', ['--user', 'daemon-reload']); const r = run('systemctl', ['--user', 'enable', '--now', SVC]);
      lines.push(r.ok ? 'It runs as your user service now, and starts when you sign in. To start it at boot without signing in: sudo loginctl enable-linger ' + os.userInfo().username : 'Could not enable the user service: ' + r.out.trim().split('\n').pop());
    } else {
      if (svc.exists && svc.scope === 'user') run('systemctl', ['--user', 'disable', '--now', SVC]);
      try { fs.rmSync(USER_UNIT, { force: true }); } catch {}
      lines.push('It no longer starts by itself; start it here when you want it.');
    }
  }
  return lines;
}

// ---------- start / stop / restart / pause / update ----------
function start() {
  try { fs.rmSync(OFF, { force: true }); } catch {}
  const svc = service();
  if (svc.exists) {
    const r = WIN ? run('sc', ['start', SVC]) : run('systemctl', [...(svc.scope === 'user' ? ['--user'] : []), 'start', SVC]);
    return r.ok || /already|1056/i.test(r.out) ? 'Started (the service runs it).' : 'Could not start the service: ' + r.out.trim().split('\n').pop();
  }
  if (hostPid()) return `It is running already (pid ${hostPid()}).`;
  if (!fs.existsSync(CFG)) return 'No ai-node.json here yet: set the hub, id and token first ([c] settings), or copy ai-node.example.json.';
  const p = WIN ? spawn('cmd.exe', ['/c', 'start', '"VibeRoller AI node"', '/min', F('start-node.bat')], { cwd: DIR, detached: true, stdio: 'ignore', windowsHide: true })
    : spawn('sh', [F('start-node.sh')], { cwd: DIR, detached: true, stdio: 'ignore' });
  p.unref();
  return 'Started.';
}
function stop(now) {
  const svc = service();
  if (svc.exists && svc.running) {
    const r = WIN ? run('sc', ['stop', SVC]) : run('systemctl', [...(svc.scope === 'user' ? ['--user'] : []), 'stop', SVC]);
    return r.ok ? 'Stopped (the service). VibeRoller keeps running for everyone; Unity says her AI host is offline.' : 'Could not stop the service: ' + r.out.trim().split('\n').pop();
  }
  if (!hostPid()) return 'It is not running.';
  fs.writeFileSync(F('.stop-now'), now ? 'now' : 'quiet');
  return now ? 'Stopping now.' : 'Stopping as soon as nothing is in flight (it never cuts off an answer).';
}
function restart(now) {
  if (!hostPid()) return start();
  fs.writeFileSync(F('.restart-now'), now ? 'now' : 'quiet');
  return now ? 'Restarting now.' : 'Restarting as soon as nothing is in flight.';
}
function pause() { fs.writeFileSync(OFF, 'AI paused from the control app at ' + new Date().toISOString() + '\n'); return 'Paused: the host stays up but lends nothing (the hub sees it offline). Resume with [p].'; }
function resume() { try { fs.rmSync(OFF, { force: true }); } catch {} return 'Resumed.'; }
function updateNow() { if (!hostPid()) return 'It is not running: start it, and it looks for an update right after it connects.'; fs.writeFileSync(F('.update-now'), new Date().toISOString()); return 'Asked it to look for an update now (applied as soon as nothing is in flight).'; }
function logTail(n = 30) {
  const f = F('ai-node.log');
  if (fs.existsSync(f)) { const lines = fs.readFileSync(f, 'utf8').split(/\r?\n/).filter(Boolean); return lines.slice(-n); }
  if (!WIN) { const svc = service(); if (svc.exists) return run('journalctl', [...(svc.scope === 'user' ? ['--user'] : []), '-u', SVC, '-n', String(n), '--no-pager']).out.split('\n'); }
  return ['(no log file here: started by hand, it logs to its own window)'];
}

// ---------- status ----------
function snapshot() {
  const cfg = readJson(CFG, null), st = readJson(STATUS, {}) || {}, pid = hostPid(), svc = service();
  const live = !!pid && st.pid === pid;
  const connected = live && st.connected && Date.now() - (st.lastOkAt || 0) < 90000;
  return { cfg, st, pid, svc, live, connected, paused: fs.existsSync(OFF), boot: bootMode(svc), version: (() => { try { return fs.readFileSync(F('VERSION.txt'), 'utf8').trim(); } catch { return st.version || ''; } })() };
}
function securityLines(cfg) {
  const sec = (cfg && cfg.security) || {}, out = [];
  const hub = String((cfg && cfg.hub) || '');
  out.push(/^https:\/\//.test(hub) ? `${C.g}hub over https${C.x}` : /^http:\/\/(127\.0\.0\.1|localhost)/.test(hub) ? 'hub on this machine' : `${C.r}hub NOT over https${C.x}${sec.allowInsecureHub ? ' (allowed)' : ' (the host refuses it)'}`);
  out.push(cfg && cfg.allowWork ? `${C.y}Work runs on this machine: ON${C.x}` : `${C.g}Work on this machine: off${C.x}`);
  out.push(cfg && cfg.pc && cfg.pc.folder ? `${C.y}PC files: ${cfg.pc.folder}${C.x}` : `${C.g}PC files: off${C.x}`);
  out.push(sec.engineGuard === false ? `${C.y}engine guard off${C.x}` : `${C.g}engine guard on${C.x}`);
  if (!WIN) { try { out.push(fs.statSync(CFG).mode & 0o077 ? `${C.r}config readable by others${C.x}` : `${C.g}config private${C.x}`); } catch {} }
  return out;
}
function dashboard() {
  const s = snapshot(), cfg = s.cfg || {}, st = s.st;
  const L = [];
  L.push(`${C.b}${C.r}♥${C.x}${C.b} VibeRoller AI Host · heart & knife${C.x}  ${C.d}${s.version} · ${DIR}${C.x}`);
  L.push('─'.repeat(78));
  L.push(`Host       ${st.nickname || cfg.nickname || cfg.id || '(no ai-node.json yet)'}${cfg.user ? '  · owner ' + cfg.user : ''}`);
  L.push(`Running    ${s.pid ? `${C.g}YES${C.x} (pid ${s.pid}${st.startedAt && s.live ? ', up ' + dur(st.startedAt) : ''})` : `${C.r}NO${C.x}`}${s.svc.exists ? ` · service ${s.svc.running ? 'running' : 'stopped'}` : ''}${s.paused ? `  ${C.y}PAUSED (the knife)${C.x}` : ''}`);
  L.push(`At boot    ${bootLabel(s.boot)}`);
  L.push(`Hub        ${s.connected ? `${C.g}CONNECTED${C.x} to ${cfg.hub} since ${dur(st.connectedAt)} · heartbeat ${ago(st.lastOkAt)}${st.hubVersion ? ' · hub ' + st.hubVersion : ''}` : s.pid ? `${C.r}NOT CONNECTED${C.x}${st.downSince ? ' for ' + dur(st.downSince) : ''}${st.lastError ? ' · ' + st.lastError : ''} (it keeps reconnecting)` : `${C.d}—${C.x}`}`);
  L.push(`In use     ${s.live ? (st.active || st.runs ? `${C.y}${st.active || 0} answer(s) in flight${st.runs ? ', ' + st.runs + ' Work run(s)' : ''}${C.x}` : 'idle') + ` · last job ${ago(st.lastJobAt)}` : `${C.d}—${C.x}`}${st.pending && s.live ? `  ${C.y}${st.pending}${C.x}` : ''}`);
  L.push(`Updates    ${cfg.autoUpdate === false ? `${C.y}automatic updates OFF${C.x}` : 'automatic'} · last: ${st.lastUpdate ? st.lastUpdate + ' (' + ago(st.lastUpdateAt) + ')' : 'none yet'}`);
  const provs = Array.isArray(cfg.providers) ? cfg.providers : [];
  L.push(`Sharing    ${provs.length ? provs.map((p) => `${p.enabled === false ? C.d + p.id + ' (off)' + C.x : p.id}: ${[p.shareWithGroup ? 'the group' + (st.group && st.group.length ? ' (' + st.group.join(', ') + ')' : '') : '', ...(p.allowedUsers || [])].filter(Boolean).join(', ') || 'owner only'}`).join(' · ') : 'no providers'}`);
  L.push(`Engines    ${Object.keys(cfg.engines || {}).join(', ') || 'none'}${Object.keys(cfg.enginesOff || {}).length ? `  ${C.d}(off: ${Object.keys(cfg.enginesOff).join(', ')})${C.x}` : ''}`);
  L.push(`Security   ${securityLines(cfg).join(' · ')}`);
  L.push('─'.repeat(78));
  L.push(`${C.b}[s]${C.x} start  ${C.b}[x]${C.x} stop  ${C.b}[r]${C.x} restart  ${C.b}[p]${C.x} ${s.paused ? 'resume' : 'pause'}  ${C.b}[u]${C.x} update now  ${C.b}[l]${C.x} log  ${C.b}[c]${C.x} settings  ${C.b}[b]${C.x} start at boot  ${C.b}[q]${C.x} quit`);
  return L;
}

// ---------- settings ----------
const yes = (v) => /^(1|y|yes|on|true)$/i.test(String(v).trim());
const userList = (v) => String(v || '').split(/[,\s]+/).map((s) => s.trim().toLowerCase()).filter((s) => /^[a-z0-9-]{1,32}$/.test(s));
function settingsList(cfg) { // [{ key, label, value (shown), set(cfg, input) → message }]
  const S = [];
  const pick = (o, k, d) => (o && o[k] !== undefined ? o[k] : d);
  S.push({ key: 'hub', label: 'Hub address', value: cfg.hub || '', set: (c, v) => { v = String(v).trim().replace(/\/+$/, ''); if (!/^https:\/\/[^\s/]+$/.test(v) && !/^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?$/.test(v)) throw new Error('Give the hub\'s https:// address (e.g. https://portal.unityailab.com).'); c.hub = v; } });
  S.push({ key: 'token', label: 'Token (secret; never shown)', value: cfg.token ? '•••••••• (set)' : '(not set)', set: (c, v) => { v = String(v).trim(); if (v.length < 24) throw new Error('A token is at least 24 characters (VibeRoller → Settings → My AI hosts).'); c.token = v; } });
  S.push({ key: 'autoUpdate', label: 'Automatic updates from the hub', value: cfg.autoUpdate === false ? 'off' : 'on', set: (c, v) => { c.autoUpdate = yes(v); } });
  S.push({ key: 'update.quietMinutes', label: 'Update / restart only after this many quiet minutes', value: String(pick(cfg.update, 'quietMinutes', 3)), set: (c, v) => { const n = Number(v); if (!(n >= 1 && n <= 60)) throw new Error('1 to 60.'); c.update = { ...(c.update || {}), quietMinutes: n }; } });
  S.push({ key: 'update.checkMinutes', label: 'Look for updates every N minutes', value: String(pick(cfg.update, 'checkMinutes', 5)), set: (c, v) => { const n = Number(v); if (!(n >= 1 && n <= 60)) throw new Error('1 to 60.'); c.update = { ...(c.update || {}), checkMinutes: n }; } });
  S.push({ key: 'reconnect.maxDelaySeconds', label: 'Reconnect: longest wait between tries (seconds)', value: String(pick(cfg.reconnect, 'maxDelaySeconds', 5)), set: (c, v) => { const n = Number(v); if (!(n >= 1 && n <= 60)) throw new Error('1 to 60.'); c.reconnect = { ...(c.reconnect || {}), maxDelaySeconds: n }; } });
  for (const p of Array.isArray(cfg.providers) ? cfg.providers : []) {
    const find = (c) => c.providers.find((x) => x.id === p.id);
    S.push({ key: `provider.${p.id}.enabled`, label: `${p.label || p.id} (${p.kind}): offered`, value: p.enabled === false ? 'off' : 'on', set: (c, v) => { find(c).enabled = yes(v); } });
    S.push({ key: `provider.${p.id}.shareWithGroup`, label: `${p.label || p.id}: shared with the hub's team group`, value: p.shareWithGroup ? 'yes' : 'no', set: (c, v) => { find(c).shareWithGroup = yes(v); } });
    S.push({ key: `provider.${p.id}.allowedUsers`, label: `${p.label || p.id}: shared with these users (comma list)`, value: (p.allowedUsers || []).join(', ') || '(none)', set: (c, v) => { find(c).allowedUsers = userList(v); } });
    S.push({ key: `provider.${p.id}.max5h`, label: `${p.label || p.id}: 5-hour usage cap (%)`, value: String(p.max5h || 90), set: (c, v) => { const n = Number(v); if (!(n >= 1 && n <= 100)) throw new Error('1 to 100.'); find(c).max5h = n; } });
    S.push({ key: `provider.${p.id}.max7d`, label: `${p.label || p.id}: 7-day usage cap (%)`, value: String(p.max7d || 90), set: (c, v) => { const n = Number(v); if (!(n >= 1 && n <= 100)) throw new Error('1 to 100.'); find(c).max7d = n; } });
  }
  for (const name of [...Object.keys(cfg.engines || {}), ...Object.keys(cfg.enginesOff || {})]) {
    const on = !!(cfg.engines || {})[name];
    S.push({ key: `engine.${name}.enabled`, label: `Engine ${name} (Unity)`, value: on ? 'on' : 'off', set: (c, v) => { c.engines = c.engines || {}; c.enginesOff = c.enginesOff || {}; const url = c.engines[name] || c.enginesOff[name]; delete c.engines[name]; delete c.enginesOff[name]; (yes(v) ? c.engines : c.enginesOff)[name] = url; if (!Object.keys(c.enginesOff).length) delete c.enginesOff; } });
  }
  S.push({ key: 'allowWork', label: 'SECURITY · let Work runs execute ON this machine (off = share AI only)', value: cfg.allowWork ? 'ON' : 'off', set: (c, v) => { c.allowWork = yes(v); } });
  S.push({ key: 'pc.folder', label: 'SECURITY · direct file access for your own Work runs (a folder; empty = off)', value: (cfg.pc && cfg.pc.folder) || 'off', set: (c, v) => { v = String(v).trim(); if (!v || /^off$/i.test(v)) { delete c.pc; return; } if (!fs.existsSync(v) || !fs.statSync(v).isDirectory()) throw new Error('That folder does not exist.'); c.pc = { ...(c.pc || {}), folder: path.resolve(v) }; } });
  S.push({ key: 'security.allowInsecureHub', label: 'SECURITY · allow a plain-http hub on another machine', value: cfg.security && cfg.security.allowInsecureHub ? 'ON' : 'off', set: (c, v) => { c.security = { ...(c.security || {}), allowInsecureHub: yes(v) }; } });
  S.push({ key: 'security.engineGuard', label: 'SECURITY · engine guard (the hub may use the engines, never manage them)', value: cfg.security && cfg.security.engineGuard === false ? 'off' : 'on', set: (c, v) => { c.security = { ...(c.security || {}), engineGuard: yes(v) }; } });
  return S;
}
function setSetting(key, value) {
  const cfg = readJson(CFG, null);
  if (!cfg) throw new Error('No ai-node.json here yet: copy ai-node.example.json to ai-node.json (or download your own host\'s package).');
  const s = settingsList(cfg).find((x) => x.key === key);
  if (!s) throw new Error(`No setting "${key}". "node vr-host.js settings" lists them.`);
  s.set(cfg, value); saveCfg(cfg);
  return `${s.label}: ${settingsList(cfg).find((x) => x.key === key).value}. ${hostPid() ? 'The host applies it at its next quiet moment.' : ''}`;
}
function lockDownConfig() {
  if (!fs.existsSync(CFG)) return 'No ai-node.json here yet.';
  if (!WIN) { fs.chmodSync(CFG, 0o600); return 'ai-node.json: readable by you only.'; }
  const me = process.env.USERNAME || os.userInfo().username;
  const r = run('icacls', [CFG, '/inheritance:r', '/grant:r', `${me}:F`, 'SYSTEM:F', 'Administrators:F']);
  return r.ok ? `ai-node.json: only you, SYSTEM (the service) and Administrators may read it.` : 'Could not change its permissions: ' + r.out.trim().split('\n').pop();
}

// ---------- commands (scripts, or no terminal) ----------
function command(args) {
  const [cmd, a, b] = args, now = args.includes('--now');
  const out = (s) => console.log(Array.isArray(s) ? s.join('\n') : s);
  switch (cmd) {
    case 'status': { const s = snapshot(); out(dashboard().slice(0, -2).map((l) => l.replace(/\x1b\[\d+m/g, ''))); return s.pid ? 0 : 3; }
    case 'start': out(start()); return 0;
    case 'stop': out(stop(now)); return 0;
    case 'restart': out(restart(now)); return 0;
    case 'pause': out(pause()); return 0;
    case 'resume': out(resume()); return 0;
    case 'update': out(updateNow()); return 0;
    case 'log': out(logTail(Number(a) || 40)); return 0;
    case 'lockdown': out(lockDownConfig()); return 0;
    case 'boot': if (!['service', 'sign-in', 'user', 'off'].includes(a) || (a === 'sign-in' && !WIN) || (a === 'user' && WIN)) { out(WIN ? 'boot service | sign-in | off' : 'boot service | user | off'); return 1; } out(setBoot(a)); return 0;
    case 'settings': { const cfg = readJson(CFG, null); if (!cfg) { out('No ai-node.json here yet.'); return 1; } for (const s of settingsList(cfg)) out(`${s.key.padEnd(34)} ${String(s.value).padEnd(24)} ${s.label}`); return 0; }
    case 'set': try { out(setSetting(a, args.slice(2).filter((x) => x !== '--now').join(' '))); return 0; } catch (e) { out(e.message); return 1; }
    default: out('node vr-host.js status | start | stop [--now] | restart [--now] | pause | resume | update | log [n] | lockdown | boot <mode> | settings | set <setting> <value>'); return 1;
  }
}

// ---------- the dashboard (a terminal) ----------
function tui() {
  readline.emitKeypressEvents(process.stdin);
  process.stdin.setRawMode(true);
  let msg = '', mode = 'dash', timer = null;
  const draw = () => { if (mode !== 'dash') return; process.stdout.write('\x1b[2J\x1b[H' + dashboard().join('\n') + '\n' + (msg ? '\n' + msg + '\n' : '')); };
  const ask = (q) => new Promise((resolve) => { process.stdin.setRawMode(false); const rl = readline.createInterface({ input: process.stdin, output: process.stdout }); rl.question(q, (v) => { rl.close(); process.stdin.setRawMode(true); resolve(v); }); });
  const say = (m) => { msg = m; draw(); };
  async function settingsScreen() {
    mode = 'settings';
    for (;;) {
      const cfg = readJson(CFG, null);
      process.stdout.write('\x1b[2J\x1b[H' + `${C.b}Settings${C.x}  ${C.d}(written to ai-node.json; the host applies them at its next quiet moment)${C.x}\n\n`);
      if (!cfg) { process.stdout.write('No ai-node.json here yet: copy ai-node.example.json to ai-node.json, or download your own host\'s package (VibeRoller → Settings → My AI hosts).\n'); await ask('\nEnter to go back '); break; }
      const list = settingsList(cfg);
      list.forEach((s, i) => process.stdout.write(`${String(i + 1).padStart(3)}. ${s.label.padEnd(66)} ${C.c}${s.value}${C.x}\n`));
      process.stdout.write(`  L. Lock the config file down (it holds the token)\n\n`);
      const v = (await ask('Number to change (Enter = back): ')).trim();
      if (!v) break;
      if (/^l$/i.test(v)) { process.stdout.write(lockDownConfig() + '\n'); await ask('Enter '); continue; }
      const s = list[Number(v) - 1]; if (!s) continue;
      if (/^SECURITY/.test(s.label)) process.stdout.write(`${C.y}This is a security setting: it changes what VibeRoller may do on this machine.${C.x}\n`);
      const nv = await ask(`${s.label}\n  now: ${s.value}\n  new (Enter = keep${/on$|off$|^yes|^no/i.test(s.value) ? '; on / off' : ''}): `);
      if (!nv.trim()) continue;
      try { process.stdout.write(setSetting(s.key, nv) + '\n'); } catch (e) { process.stdout.write(`${C.r}${e.message}${C.x}\n`); }
      await ask('Enter ');
    }
    mode = 'dash'; draw();
  }
  async function bootScreen() {
    mode = 'boot';
    const opts = WIN ? ['service', 'sign-in', 'off'] : ['service', 'user', 'off'];
    process.stdout.write('\x1b[2J\x1b[H' + `${C.b}Start at boot${C.x}  now: ${bootLabel(bootMode())}\n\n` + opts.map((o, i) => `  ${i + 1}. ${bootLabel(o)}`).join('\n') + '\n\n');
    const v = (await ask('Choose (Enter = keep): ')).trim();
    const o = opts[Number(v) - 1];
    if (o) { for (const l of setBoot(o)) process.stdout.write(l + '\n'); await ask('Enter '); }
    mode = 'dash'; draw();
  }
  async function logScreen() { mode = 'log'; process.stdout.write('\x1b[2J\x1b[H' + `${C.b}Log${C.x} (the last lines)\n\n` + logTail(40).join('\n') + '\n'); await ask('\nEnter to go back '); mode = 'dash'; draw(); }
  process.stdin.on('keypress', async (str, key) => {
    if (mode !== 'dash' || !key) return;
    if (key.name === 'q' || (key.ctrl && key.name === 'c')) { clearInterval(timer); process.stdin.setRawMode(false); process.stdout.write('\n'); process.exit(0); }
    const k = key.name;
    if (k === 's') say(start());
    else if (k === 'x') say(stop(key.shift));
    else if (k === 'r') say(restart(key.shift));
    else if (k === 'p') say(fs.existsSync(OFF) ? resume() : pause());
    else if (k === 'u') say(updateNow());
    else if (k === 'l') await logScreen();
    else if (k === 'c') await settingsScreen();
    else if (k === 'b') await bootScreen();
  });
  timer = setInterval(draw, 2000);
  draw();
  if (!fs.existsSync(CFG)) say(`${C.y}No ai-node.json here yet: [c] settings explains, or run the VibeRoller AI Host package to set one up.${C.x}`);
  else say(`${C.d}Shift+X / Shift+R stop / restart at once (otherwise they wait until nothing is in flight).${C.x}`);
}

if (require.main === module) {
  const args = process.argv.slice(2);
  if (args.length || !process.stdin.isTTY) process.exit(command(args.length ? args : ['status']));
  tui();
}
module.exports = { snapshot, settingsList, setSetting, command };
