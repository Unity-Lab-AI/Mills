#!/usr/bin/env node
// The proving ground's sandbox hub (bridge/test/PROVING.md): a persistent copy of THIS checkout's hub, with its own
// data, its own accounts (spongetest, admin; geetest) and a real Claude, for driving end-to-end scenarios by hand.
// It lives under /srv/apps/sharedwork/_proving (outside every repo) on ports 7795 (+7796 behind the Rust hub, +7798 the
// fake OpenAI endpoint) and never touches the live hub, its data, accounts or hosts.
//
//   node bridge/test/proving.js start [--rust-hub]      copy the code, write the config, start the hub detached
//   node bridge/test/proving.js stop | status | reset [--all]
//   node bridge/test/proving.js login <account>         print a session token (Authorization: Bearer <token>)
//   node bridge/test/proving.js work <account> <message> [--model claude-haiku-5-5] [--project proving-ground]
//   node bridge/test/proving.js hosts <n>               n test AI hosts on a fake OpenAI endpoint (0 stops them)
'use strict';

const fs = require('fs');
const net = require('net');
const path = require('path');
const crypto = require('crypto');
const { spawn, spawnSync, execFileSync } = require('child_process');

const BRIDGE = path.join(__dirname, '..');
const MILLS = path.join(BRIDGE, '..');
const PROVING = process.env.VR_PROVING_DIR || '/srv/apps/sharedwork/_proving';
const HUB = path.join(PROVING, 'hub'), SB = path.join(HUB, 'bridge');
const PORT = 7795, BACK = 7796, FAKE_PORT = 7798;
const H = `http://127.0.0.1:${PORT}`;
const PID_FILE = path.join(PROVING, 'hub.pid'), LOG_FILE = path.join(PROVING, 'hub.log'), MODE_FILE = path.join(PROVING, 'hub.mode');
const FAKE_PID = path.join(PROVING, 'fake-openai.pid'), HOSTS_DIR = path.join(HUB, 'hosts');
const ACCOUNTS = path.join(HUB, 'accounts.txt');
const ACCOUNT_LIST = [['spongetest', { user: 'Spongetest', admin: true }], ['geetest', { user: 'Geetest' }]];
const PROJECT = 'proving-ground', PROJECT_URL = 'https://git.unityailab.com/Sponge/proving-ground.git';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const die = (msg, code = 1) => { console.error(msg); process.exit(code); };
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
const readPid = (f) => { try { const n = Number(fs.readFileSync(f, 'utf8').trim()); return n > 0 ? n : 0; } catch { return 0; } };
const listening = (port) => new Promise((resolve) => { const c = net.connect({ port, host: '127.0.0.1' }, () => { c.destroy(); resolve(true); }); c.on('error', () => resolve(false)); c.setTimeout(1500, () => { c.destroy(); resolve(false); }); });

// ---------- http ----------
async function call(p, token, body) {
  try {
    const r = await fetch(H + p, { method: body !== undefined ? 'POST' : 'GET', headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: 'Bearer ' + token } : {}) }, body: body !== undefined ? JSON.stringify(body) : undefined });
    return { status: r.status, data: await r.json().catch(() => ({})) };
  } catch (e) { return { status: 0, data: { error: e.message } }; }
}
function accounts() { // accounts.txt: "<name> <admin|user> <password>"
  const out = {};
  try { for (const l of fs.readFileSync(ACCOUNTS, 'utf8').split('\n')) { const m = /^(\S+)\s+(admin|user)\s+(\S+)$/.exec(l.trim()); if (m) out[m[1]] = m[3]; } } catch {}
  return out;
}
async function login(name) {
  const pw = accounts()[name];
  if (!pw) die(`No such account "${name}" (see ${ACCOUNTS}).`);
  const r = await call('/api/login', null, { username: name, password: pw, deviceId: `proving-${name}-0000000000` });
  if (r.status !== 200 || !r.data.token) die(`Sign-in as ${name} failed: ${r.status} ${JSON.stringify(r.data)} (is the hub running? node bridge/test/proving.js start)`);
  return r.data.token;
}

// ---------- the fake OpenAI endpoint (the suite's: echoes the last user line), a detached process of its own ----------
function fakeOpenai() {
  const http = require('http');
  http.createServer((req, res) => {
    let body = ''; req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const j = (o) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(o)); };
      if (req.url === '/api/version') return j({ version: 'fake' });
      if (req.url === '/health') return j({ ok: true, loaded: true });
      if (req.url === '/oai/chat/completions' || req.url === '/v1/chat/completions') {
        const b = JSON.parse(body || '{}'), last = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' };
        const text = 'oai says: ' + last.content;
        if (b.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); for (const w of text.split(' ')) res.write('data: ' + JSON.stringify({ model: b.model, choices: [{ delta: { content: w + ' ' } }] }) + '\n\n'); return res.end('data: [DONE]\n\n'); }
        return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: text } }], usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } });
      }
      res.writeHead(404); res.end('{}');
    });
  }).listen(FAKE_PORT, '127.0.0.1');
}
function detached(args, logFile, opts = {}) {
  const fd = fs.openSync(logFile, 'a');
  const child = spawn(opts.command || process.execPath, args, { cwd: opts.cwd || PROVING, env: opts.env || process.env, detached: true, stdio: ['ignore', fd, fd] });
  child.unref();
  return child.pid;
}
function killGroup(pid) { // the supervisor and everything under it (detached = its own process group)
  if (!pid || !alive(pid)) return;
  for (const sig of ['SIGTERM', 'SIGKILL']) { try { process.kill(-pid, sig); } catch { try { process.kill(pid, sig); } catch {} } if (sig === 'SIGTERM') { for (let i = 0; i < 40 && alive(pid); i++) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 250); } if (!alive(pid)) break; }
}
async function startFake() {
  if (readPid(FAKE_PID) && alive(readPid(FAKE_PID)) && await listening(FAKE_PORT)) return;
  fs.mkdirSync(PROVING, { recursive: true });
  fs.writeFileSync(FAKE_PID, String(detached([__filename, '_fake-openai'], path.join(PROVING, 'fake-openai.log'))));
  for (let i = 0; i < 20 && !(await listening(FAKE_PORT)); i++) await sleep(150);
}

// ---------- the hub copy ----------
const TOOLS_EXTRA = ['Bash(node test/:*)', 'Bash(node:*)', 'Bash(git checkout:*)', 'Bash(git switch:*)', 'Bash(git branch:*)', 'Bash(git rev-parse:*)', 'Bash(git show:*)', 'Bash(git config:*)', 'Bash(date:*)', 'Bash(ls:*)', 'Bash(cat:*)', 'Bash(pwd:*)'];
function copyCode() {
  fs.mkdirSync(path.join(SB, 'test'), { recursive: true });
  for (const f of fs.readdirSync(BRIDGE)) if ((f.endsWith('.js') && f !== 'master-logins.js') || f === 'cloudflared-quick.yml') fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  for (const f of ['ai-node.example.json', 'start-node.bat', 'start-node.sh']) fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  fs.cpSync(path.join(BRIDGE, 'host-tools'), path.join(SB, 'host-tools'), { recursive: true, filter: (s) => !/\.(exe|ico)$/i.test(s) });
  fs.cpSync(path.join(MILLS, 'web'), path.join(HUB, 'web'), { recursive: true });
  for (const f of ['versions.json', 'CHANGELOG.md', 'LAWS.md']) if (fs.existsSync(path.join(MILLS, f))) fs.copyFileSync(path.join(MILLS, f), path.join(HUB, f));
  fs.writeFileSync(path.join(HUB, '.gitignore'), ['bridge/profiles/', 'bridge/profiles-deleted/', 'bridge/*.json', 'bridge/.restart', 'bridge/.fj/', 'Chat/', 'Archive/', 'Board/', 'Notes/', 'Api/', 'worktrees/', 'projects/', 'hosts/', 'accounts.txt', 'bridge/host2/', 'bridge/ai-node.lock', ''].join('\n'));
  const g = (...a) => spawnSync('git', a, { cwd: HUB, stdio: 'ignore' });
  if (!fs.existsSync(path.join(HUB, '.git'))) { g('init', '-q', '-b', 'main'); g('config', 'user.email', 'proving@example.invalid'); g('config', 'user.name', 'proving'); g('add', '-A'); g('commit', '-q', '-m', 'proving ground'); g('branch', 'develop'); }
  else { g('add', '-A'); g('commit', '-q', '-m', 'proving ground refresh'); }
}
function writeConfig(rustBin) {
  const real = (() => { try { return JSON.parse(fs.readFileSync(path.join(BRIDGE, 'config.json'), 'utf8')); } catch { return {}; } })();
  const base = `http://127.0.0.1:${FAKE_PORT}`;
  const cfg = {
    port: PORT, defaultUsername: real.defaultUsername || 'mills', permissionMode: real.permissionMode || 'acceptEdits', model: '',
    models: real.models || [{ id: 'claude-haiku-5-5', label: 'Haiku' }], allowedTools: [...(real.allowedTools || []).filter((t) => !/^Bash\((ssh|fj)/.test(t)), ...TOOLS_EXTRA],
    tunnel: false, gistId: '', allowedOrigins: [], hostGroup: ACCOUNT_LIST.map(([n]) => n),
    assistant: { ollamaUrl: base, imageUrl: base, voiceUrl: base, speechUrl: base, searchUrl: base + '/search', youtubeUrl: base + '/results', via: 'node' },
    worktreeRoot: path.join(HUB, 'worktrees'), projectsRoot: path.join(HUB, 'projects'),
    claudeCommand: 'claude', // the REAL one on PATH (its model calls go through the environment's ANTHROPIC_BASE_URL relay)
    rustHub: rustBin ? { enabled: true, backPort: BACK } : false,
  };
  fs.writeFileSync(path.join(SB, 'config.json'), JSON.stringify(cfg, null, 2));
}
function ensureAccounts() {
  const have = accounts(), lines = [];
  const sp = (args) => spawnSync(process.execPath, [path.join(SB, 'set-password.js'), ...args], { cwd: SB, encoding: 'utf8' });
  if (!fs.existsSync(path.join(SB, 'auth.json'))) sp([crypto.randomBytes(12).toString('hex')]); // the default profile: nobody signs in as it
  for (const [name, conf] of ACCOUNT_LIST) {
    const pw = have[name] || crypto.randomBytes(12).toString('hex');
    if (!fs.existsSync(path.join(SB, 'profiles', name, 'auth.json'))) {
      fs.mkdirSync(path.join(SB, 'profiles', name), { recursive: true });
      fs.writeFileSync(path.join(SB, 'profiles', name, 'config.json'), JSON.stringify(conf));
      const r = sp(['--profile', name, pw]);
      if (r.status !== 0) die(`set-password for ${name} failed: ${r.stdout}${r.stderr}`);
    }
    lines.push(`${name} ${conf.admin ? 'admin' : 'user'} ${pw}`);
  }
  fs.writeFileSync(ACCOUNTS, lines.join('\n') + '\n', { mode: 0o600 });
  fs.chmodSync(ACCOUNTS, 0o600);
}
async function running() { const pid = readPid(PID_FILE); return !!pid && alive(pid) && (await call('/api/status')).status === 200; }

// ---------- commands ----------
async function start() {
  if (await running()) { const s = (await call('/api/status')).data; return console.log(`The proving ground is already running: ${H} (pid ${readPid(PID_FILE)}, version ${s.version}, ${fs.existsSync(MODE_FILE) ? fs.readFileSync(MODE_FILE, 'utf8') : ''}). Use stop first to change anything.`); }
  if (!process.env.ANTHROPIC_BASE_URL || !process.env.ANTHROPIC_AUTH_TOKEN) die('Refusing to start: ANTHROPIC_BASE_URL and ANTHROPIC_AUTH_TOKEN are not in the environment. The proving ground\'s Work turns use the real claude through the relay of the Work run that drives it; run this from a Work run.');
  if (await listening(PORT)) die(`Port ${PORT} is taken by something that is not the proving ground (no pid file match). Not touching it.`);
  const useRust = process.argv.includes('--rust-hub');
  let rustBin = '';
  if (useRust) {
    rustBin = process.env.VR_HUB_BIN || path.join(MILLS, 'native', 'target', 'regress', 'release', 'vr-hub');
    if (!fs.existsSync(rustBin)) die(`--rust-hub: no vr-hub binary at ${rustBin} (node bridge/test/regress.js --rust-hub builds it, or set VR_HUB_BIN).`);
    if (await listening(BACK)) die(`Port ${BACK} (the JavaScript hub behind the Rust one) is taken.`);
  }
  fs.mkdirSync(HUB, { recursive: true });
  copyCode(); writeConfig(rustBin); ensureAccounts();
  await startFake();
  const env = { ...process.env, MILLS_SUPERVISED: '' };
  delete env.MILLS_SUPERVISED; delete env.VR_BACK_PORT; delete env.VR_FRONT_SECRET; delete env.VR_FRONT_MODULES;
  if (rustBin) env.VR_HUB_BIN = rustBin;
  const pid = detached([path.join(SB, 'run.js'), '--no-tunnel'], LOG_FILE, { cwd: SB, env });
  fs.writeFileSync(PID_FILE, String(pid)); fs.writeFileSync(MODE_FILE, rustBin ? 'rust-hub' : 'js-hub');
  let up = false;
  for (let i = 0; i < 120 && !up; i++) { up = (await call('/api/status')).status === 200; if (!up) await sleep(500); }
  if (!up) die(`The hub did not come up; see ${LOG_FILE}\n` + tail(LOG_FILE, 15));
  // the chip-away project, cloned from Forgejo with the run's own git login
  const t = await login('spongetest');
  const have = ((await call('/api/projects', t)).data.projects || []).some((p) => (p.name || p) === PROJECT);
  if (!have) {
    const r = await call('/api/projects', t, { url: PROJECT_URL, name: PROJECT });
    if (r.status !== 200) console.error(`Could not clone ${PROJECT_URL}: ${r.status} ${JSON.stringify(r.data)}`);
    else console.log(`Cloned ${PROJECT_URL} as project ${PROJECT}.`);
  }
  console.log(`The proving ground is up: ${H} (${rustBin ? 'Rust hub in front' : 'JavaScript hub'}), pid ${pid}, log ${LOG_FILE}, accounts ${ACCOUNTS}`);
}
async function stop() {
  await hosts(0, true);
  const pid = readPid(PID_FILE);
  if (pid && alive(pid)) { killGroup(pid); console.log(`Stopped the hub (pid ${pid}).`); } else console.log('The hub was not running.');
  try { fs.unlinkSync(PID_FILE); } catch {}
  const fp = readPid(FAKE_PID); if (fp && alive(fp)) { try { process.kill(fp, 'SIGTERM'); } catch {} }
  try { fs.unlinkSync(FAKE_PID); } catch {}
  for (let i = 0; i < 20 && (await listening(PORT)); i++) await sleep(250);
  if (await listening(PORT)) console.error(`Port ${PORT} still answers.`);
}
const tail = (f, n) => { try { return fs.readFileSync(f, 'utf8').split('\n').filter(Boolean).slice(-n).join('\n'); } catch { return '(no log)'; } };
async function status() {
  const up = await running(), s = up ? (await call('/api/status')).data : {};
  console.log(`running: ${up}${up ? ` (pid ${readPid(PID_FILE)}, ${fs.existsSync(MODE_FILE) ? fs.readFileSync(MODE_FILE, 'utf8') : '?'})` : ''}`);
  console.log(`port: ${PORT}${fs.existsSync(MODE_FILE) && /rust/.test(fs.readFileSync(MODE_FILE, 'utf8')) ? ` (back ${BACK})` : ''}  fake openai: ${FAKE_PORT} ${await listening(FAKE_PORT) ? 'up' : 'down'}`);
  console.log(`version: ${s.version || '-'}  commit: ${s.commit || '-'}`);
  console.log(`accounts: ${Object.keys(accounts()).join(', ') || '(none)'}  (${ACCOUNTS})`);
  if (up) { const t = await login('spongetest'); const c = (await call('/api/check', t)).data; console.log(`projects: ${(c.projects || []).map((p) => p.name || p).join(', ')}  providers: ${(c.providers || []).map((p) => p.id).join(', ')}`); }
  console.log(`log (${LOG_FILE}):\n${tail(LOG_FILE, 8)}`);
}
async function reset() {
  await stop();
  const keep = process.argv.includes('--all') ? '' : (() => { try { return fs.readFileSync(ACCOUNTS, 'utf8'); } catch { return ''; } })();
  fs.rmSync(HUB, { recursive: true, force: true });
  for (const f of [LOG_FILE, MODE_FILE, path.join(PROVING, 'fake-openai.log')]) try { fs.unlinkSync(f); } catch {}
  if (keep) { fs.mkdirSync(HUB, { recursive: true }); fs.writeFileSync(ACCOUNTS, keep, { mode: 0o600 }); }
  console.log(`Wiped ${HUB}${keep ? ' (accounts.txt kept: the same passwords come back at the next start)' : ''}.`);
}
async function work(name, message) {
  if (!name || !message) die('Usage: work <account> <message> [--model claude-haiku-5-5] [--project proving-ground]');
  const opt = (n, d) => { const i = process.argv.indexOf(n); return i >= 0 ? process.argv[i + 1] : d; };
  const model = opt('--model', 'claude-haiku-5-5'), project = opt('--project', PROJECT);
  const t = await login(name);
  const ws = await call('/api/workspace', t, { project });
  if (ws.status !== 200) die(`workspace ${project}: ${ws.status} ${JSON.stringify(ws.data)}`);
  const md = await call('/api/model', t, { model });
  if (md.status !== 200) die(`model ${model}: ${md.status} ${JSON.stringify(md.data)}`);
  const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + t }, body: JSON.stringify({ message, fresh: true }) });
  if (r.status !== 200) die(`chat: ${r.status} ${await r.text()}`);
  let failed = false, done = false, buf = '';
  const see = (line) => {
    if (!line.trim()) return;
    process.stdout.write(line + '\n');
    try { const ev = JSON.parse(line); if (ev.type === 'bridge_error' || (ev.type === 'result' && ev.is_error)) failed = true; if (ev.type === 'bridge_done') { done = true; if (ev.code) failed = true; } } catch {}
  };
  process.on('SIGINT', () => { call('/api/stop', t, {}).finally(() => process.exit(130)); });
  const reader = r.body.getReader(), dec = new TextDecoder();
  for (;;) {
    const { value, done: end } = await reader.read();
    if (end) break;
    buf += dec.decode(value, { stream: true });
    let i; while ((i = buf.indexOf('\n')) >= 0) { see(buf.slice(0, i)); buf = buf.slice(i + 1); }
  }
  see(buf);
  process.exit(done && !failed ? 0 : 1);
}
async function hosts(n, quiet) {
  if (!(await running())) { if (quiet) { killHostProcs(); return; } die('The proving ground is not running.'); }
  const t = await login('spongetest');
  killHostProcs();
  for (const h of ((await call('/api/hosts', t)).data.mine || [])) await call('/api/hosts/remove', t, { id: h.id });
  fs.rmSync(HOSTS_DIR, { recursive: true, force: true });
  if (!n) { if (!quiet) console.log('No test hosts.'); return; }
  await startFake();
  for (let i = 1; i <= n; i++) {
    const nh = (await call('/api/hosts/new', t, {})).data.host;
    if (!nh) die('Could not make a host record.');
    const cfg = (await call('/api/hosts/config', t, { id: nh.id })).data.config;
    cfg.hub = H;
    cfg.engines = {};
    cfg.autoUpdate = false;
    cfg.providers = [{ id: 'local', kind: 'openai', label: `Test model ${i}`, baseUrl: `http://127.0.0.1:${FAKE_PORT}/oai`, models: ['tiny-local'], shareWithGroup: true, allowedUsers: [] }];
    const dir = path.join(HOSTS_DIR, 'host-' + i); fs.mkdirSync(dir, { recursive: true });
    for (const f of ['ai-node.js', 'host-relay.js']) fs.copyFileSync(path.join(SB, f), path.join(dir, f));
    fs.writeFileSync(path.join(dir, 'ai-node.json'), JSON.stringify(cfg, null, 2), { mode: 0o600 });
    fs.writeFileSync(path.join(dir, 'pid'), String(detached([path.join(dir, 'ai-node.js'), '--config', path.join(dir, 'ai-node.json')], path.join(dir, 'out.log'), { cwd: dir })));
  }
  let online = 0;
  for (let k = 0; k < 40 && online < n; k++) { await sleep(500); online = ((await call('/api/hosts', t)).data.all || []).filter((h) => h.online && (h.providers || []).length).length; }
  console.log(`${online} of ${n} test hosts online.`);
  if (online < n) process.exitCode = 1;
}
function killHostProcs() {
  try { for (const d of fs.readdirSync(HOSTS_DIR)) { const p = readPid(path.join(HOSTS_DIR, d, 'pid')); if (p && alive(p)) killGroup(p); } } catch {}
}

(async () => {
  const [cmd, ...rest] = process.argv.slice(2).filter((a, i, all) => !(a.startsWith('--') || (all[i - 1] || '').match(/^--(model|project)$/)));
  if (cmd === '_fake-openai') return fakeOpenai();
  if (cmd === 'start') return start();
  if (cmd === 'stop') return stop();
  if (cmd === 'status') return status();
  if (cmd === 'reset') return reset();
  if (cmd === 'login') { if (!rest[0]) die('Usage: login <account>'); return console.log(await login(rest[0])); }
  if (cmd === 'work') return work(rest[0], rest.slice(1).join(' '));
  if (cmd === 'hosts') { const n = Number(rest[0]); if (!Number.isInteger(n) || n < 0 || n > 8) die('Usage: hosts <0-8>'); return hosts(n); }
  die('Usage: node bridge/test/proving.js start [--rust-hub] | stop | status | reset [--all] | login <account> | work <account> <message> [--model m] [--project p] | hosts <n>', 2);
})().catch((e) => die(e.stack || String(e)));
