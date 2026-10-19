#!/usr/bin/env node
// VibeRoller AI Host: this machine lends its AI to a VibeRoller hub that runs somewhere else (the lab server), so
// nobody's login ever sits on the server. It connects OUT to the hub over HTTPS (no port opens here) and offers:
//   providers  its Claude Code login (and/or an Anthropic key, OpenAI-compatible endpoints such as a local Ollama):
//              Work runs and API answers for the people it allows run HERE, Claude Code in this machine's own worktree
//              of the project, and the hub relays the words back to the person's browser
//   engines    Unity's local engines (Ollama / image / voice / speech), reached by name only
//   pc         direct computer access for its owner's Work runs (pc.folder): the PC tools act on that folder here
//              instead of going through the browser
//
// Config: ai-node.json next to this file (the Download panel hands it out filled in: Settings → My AI hosts).
//   { "hub", "id" (UUID), "token", "user", "nickname", "providers": [...], "engines": {...}, "pc": { "folder" },
//     "work": { "hubRepo", "projectsRoot", "claudeCommand", "claudeArgs" } }   (bridge/ai-node.example.json has it all)
// Run:  node ai-node.js [--config <file>]      Service from boot: host-tools/install-ai-host-service (Windows + Linux).
//
// Protocol (hub side: bridge/ai-nodes.js): POST /node/hello (nonce + what we offer → nickname + the hub's nonce; both
// sides derive the same AES-256-GCM session key from the secret and the nonces, and Work jobs + their events travel
// sealed with it, on top of TLS), GET /node/poll (jobs + cancels), POST /node/reply, POST /node/events.
// The knife (stop-viberoller-ai-host / ai-host.off next to this file) pauses the host: it stops polling, so the hub
// sees it offline; the heart removes the file and it carries on by itself.
'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const { spawn } = require('child_process');

const VERSION = '2.0.0';
const CFG_FILE = process.argv.includes('--config') ? process.argv[process.argv.indexOf('--config') + 1] : path.join(__dirname, 'ai-node.json');
const PAUSE_FILE = path.join(__dirname, 'ai-host.off');
const LOCK_FILE = path.join(__dirname, 'ai-node.lock');
const cfg = (() => { try { return JSON.parse(fs.readFileSync(CFG_FILE, 'utf8')); } catch (e) { console.error(`[ai-host] Cannot read ${CFG_FILE}: ${e.message}`); process.exit(1); } })();
const HUB = String(cfg.hub || '').replace(/\/+$/, '');
const ID = String(cfg.id || cfg.name || ''), TOKEN = String(cfg.token || ''), USER = String(cfg.user || '');
const ENGINES = cfg.engines && typeof cfg.engines === 'object' ? cfg.engines : {};
const PROVIDERS = Array.isArray(cfg.providers) ? cfg.providers.filter((p) => p && p.id) : [];
const PC_FOLDER = cfg.pc && cfg.pc.folder ? path.resolve(String(cfg.pc.folder)) : '';
const WORK = cfg.work && typeof cfg.work === 'object' ? cfg.work : {};
if (!/^https?:\/\//.test(HUB) || !ID || !TOKEN) { console.error('[ai-host] ai-node.json needs hub, id (or name) and token.'); process.exit(1); }
if (WORK.hubRepo) process.env.VIBEROLLER_HUB_REPO = path.resolve(String(WORK.hubRepo));
const headers = { 'X-VR-Host': ID, 'X-VR-Host-Token': TOKEN, 'X-VR-Node': ID, 'X-VR-Node-Token': TOKEN };
// one host per machine: a lock file with the running pid (exit code 2 = already running, like the hub's run.js)
(function singleInstance() {
  try {
    const pid = Number(fs.readFileSync(LOCK_FILE, 'utf8').trim());
    if (pid && pid !== process.pid) { process.kill(pid, 0); console.log(`[ai-host] already running (pid ${pid}); nothing to do.`); process.exit(2); }
  } catch {} // no lock, unreadable, or that pid is gone
  fs.writeFileSync(LOCK_FILE, String(process.pid));
  const bye = () => { try { if (Number(fs.readFileSync(LOCK_FILE, 'utf8').trim()) === process.pid) fs.unlinkSync(LOCK_FILE); } catch {} };
  process.on('exit', bye); for (const s of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(s, () => process.exit(0));
})();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), '[ai-host]', ...a);
let nickname = String(cfg.nickname || ID), key = null, kid = '', pollMs = 25000;

// ---------- e2e envelope (mirror of the hub's) ----------
const derive = (secret, hostNonce, serverNonce) => Buffer.from(crypto.hkdfSync('sha256', Buffer.from(String(secret), 'utf8'), Buffer.from(hostNonce + serverNonce, 'hex'), 'viberoller-ai-host-v1', 32));
const keyId = (k) => 'vr-' + crypto.createHash('sha256').update(k).digest('hex').slice(0, 24);
function seal(obj) { const iv = crypto.randomBytes(12), c = crypto.createCipheriv('aes-256-gcm', key, iv); const ct = Buffer.concat([c.update(JSON.stringify(obj), 'utf8'), c.final(), c.getAuthTag()]); return { iv: iv.toString('base64'), ct: ct.toString('base64') }; }
function open(enc) {
  const iv = Buffer.from(String(enc.iv || ''), 'base64'), ct = Buffer.from(String(enc.ct || ''), 'base64');
  const d = crypto.createDecipheriv('aes-256-gcm', key, iv); d.setAuthTag(ct.subarray(ct.length - 16));
  return JSON.parse(Buffer.concat([d.update(ct.subarray(0, ct.length - 16)), d.final()]).toString('utf8'));
}

// ---------- hello: what this host offers; the hub answers with the nickname and its nonce ----------
async function hello() {
  const nonce = crypto.randomBytes(16).toString('hex');
  const providers = PROVIDERS.map((p) => ({ id: p.id, kind: p.kind, label: p.label, models: p.models, allowedUsers: p.allowedUsers, max5h: p.max5h, max7d: p.max7d }));
  const r = await fetch(HUB + '/node/hello', { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify({ nonce, user: USER, providers, engines: Object.keys(ENGINES), pc: !!PC_FOLDER, version: VERSION, os: process.platform }), signal: AbortSignal.timeout(20000) });
  if (r.status === 401) throw Object.assign(new Error('the hub does not know this host (id / token): check ai-node.json, or make the host again in Settings → My AI hosts'), { auth: true });
  if (!r.ok) throw new Error('hello answered ' + r.status);
  const j = await r.json();
  key = derive(TOKEN, nonce, String(j.serverNonce || '')); kid = keyId(key);
  if (kid !== j.kid) { key = null; throw new Error('the key ids differ after the handshake (hub ' + j.kid + ', here ' + kid + '): is the token right?'); }
  nickname = j.nickname || nickname; pollMs = Number(j.pollMs) || pollMs;
  if (cfg.nickname !== nickname) { try { cfg.nickname = nickname; fs.writeFileSync(CFG_FILE, JSON.stringify(cfg, null, 2) + '\n'); } catch {} }
  log(`connected to ${HUB} as "${nickname}"${USER ? ' (' + USER + ')' : ''}: ${providers.length} provider(s) [${providers.map((p) => p.id).join(', ')}], engines ${Object.keys(ENGINES).join(', ') || 'none'}${PC_FOLDER ? ', computer access to ' + PC_FOLDER : ''}, key ${kid}`);
}

// ---------- usage caps: this host's own view of its Claude windows, from the events of its runs ----------
const usage = {}; // provider id -> { five_hour: { utilization, resetsAt, status }, seven_day: {...} }
const toMs = (t) => { const n = typeof t === 'number' ? (t < 1e12 ? t * 1000 : t) : Date.parse(t); return Number.isFinite(n) ? n : null; };
function noteUsage(providerId, ev) {
  if (!/rate_limit/.test(String(ev.type || ''))) return;
  const info = ev.rate_limit_info || ev.rateLimitInfo || ev, kind = String(info.rateLimitType || info.rate_limit_type || info.type || '').toLowerCase();
  const w = /seven|week|7d/.test(kind) ? 'seven_day' : /five|5h|session/.test(kind) ? 'five_hour' : null;
  if (!w) return;
  const u = usage[providerId] || (usage[providerId] = {}), prev = u[w] || {}, raw = info.utilization ?? info.usage ?? null, n = raw == null || raw === '' ? NaN : Number(raw);
  u[w] = { status: String(info.status || prev.status || ''), utilization: Number.isFinite(n) ? Math.max(0, Math.min(1, n > 1 ? n / 100 : n)) : prev.utilization ?? null, resetsAt: toMs(info.resetsAt || info.resets_at) || prev.resetsAt || null };
}
function overCap(p) {
  const u = usage[p.id] || {}, pc = (w) => (w && Number.isFinite(w.utilization) && (!w.resetsAt || w.resetsAt > Date.now()) ? w.utilization * 100 : 0);
  if (p.max5h && pc(u.five_hour) >= p.max5h) return `this host's 5-hour Claude usage is at its ${p.max5h}% cap`;
  if (p.max7d && pc(u.seven_day) >= p.max7d) return `this host's weekly Claude usage is at its ${p.max7d}% cap`;
  return '';
}

// ---------- engine + provider + pc jobs: one HTTP call here, the answer back whole ----------
async function reply(job, status, headersOut, body) {
  try { await fetch(HUB + '/node/reply', { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: job.id, status, headers: headersOut, body: body ? Buffer.from(body).toString('base64') : '' }), signal: AbortSignal.timeout(60000) }); }
  catch (e) { log('could not hand back a job:', e.message); }
}
async function relayJob(job) {
  try {
    let base, extra = {};
    if (String(job.engine).startsWith('provider:')) {
      const p = PROVIDERS.find((x) => x.id === job.engine.slice(9) && x.kind === 'openai');
      if (!p) throw Object.assign(new Error('This host offers no such endpoint.'), { status: 404 });
      base = String(p.baseUrl || ''); if (p.apiKey) extra = { authorization: 'Bearer ' + p.apiKey };
    } else {
      base = ENGINES[job.engine];
      if (!base) throw Object.assign(new Error('This host does not lend the ' + job.engine + ' engine.'), { status: 404 });
    }
    if (!/^\/[^\s]*$/.test(String(job.path || ''))) throw Object.assign(new Error('bad path'), { status: 400 });
    const r = await fetch(String(base).replace(/\/+$/, '') + job.path, { method: job.method || 'GET', headers: { ...(job.headers && job.headers['content-type'] ? { 'content-type': job.headers['content-type'] } : {}), ...extra }, body: job.body ? Buffer.from(job.body, 'base64') : undefined, signal: AbortSignal.timeout(290000) });
    const ct = r.headers.get('content-type') || '';
    // a streamed answer (SSE, NDJSON) goes back piece by piece as the engine produces it, so words show up as they are made
    if (r.body && /text\/event-stream|ndjson|text\/plain/i.test(ct)) return streamBack(job, r, ct);
    await reply(job, r.status, { 'content-type': ct }, Buffer.from(await r.arrayBuffer()));
  } catch (e) { await reply(job, e.status || 502, { 'content-type': 'application/json' }, Buffer.from(JSON.stringify({ error: e.message }))); }
}
async function streamBack(job, r, ct) {
  const post = (b) => fetch(HUB + '/node/stream', { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify({ id: job.id, ...b }), signal: AbortSignal.timeout(60000) }).then((x) => x.status !== 404).catch(() => true);
  if (!(await post({ status: r.status, headers: { 'content-type': ct } }))) return; // the hub no longer waits for this job
  const reader = r.body.getReader(); let held = [], heldBytes = 0, lastSend = Date.now();
  const flush = async () => { if (!held.length) return true; const chunk = Buffer.concat(held); held = []; heldBytes = 0; lastSend = Date.now(); return post({ chunk: chunk.toString('base64') }); };
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      held.push(Buffer.from(value)); heldBytes += value.length;
      if (heldBytes >= 4096 || Date.now() - lastSend >= 60) { if (!(await flush())) { try { reader.cancel(); } catch {} return; } } // a few words at a time, never a flood of tiny requests
    }
    await flush(); await post({ done: true });
  } catch (e) { await post({ done: true, error: e.message }); }
}
// direct computer access: the PC tools on pc.folder, the same five the browser offers, never above the folder
function pcPath(p) {
  const segs = String(p || '').replace(/\\/g, '/').split('/').filter((s) => s && s !== '.');
  if (segs.includes('..')) throw new Error('Paths cannot go above the shared folder.');
  const full = path.join(PC_FOLDER, ...segs);
  if (full !== PC_FOLDER && !full.startsWith(PC_FOLDER + path.sep)) throw new Error('Paths cannot go above the shared folder.');
  return { full, rel: segs.join('/') };
}
const pcOps = {
  list({ path: p = '' }) { const { full } = pcPath(p); const out = fs.readdirSync(full, { withFileTypes: true }).map((e) => (e.isDirectory() ? e.name + '/' : e.name)).sort(); return out.join('\n') || '(empty folder)'; },
  read({ path: p }) { const { full } = pcPath(p); if (fs.statSync(full).size > 5e6) throw new Error('That file is larger than 5 MB.'); return fs.readFileSync(full, 'utf8'); },
  write({ path: p, content = '' }) { const { full, rel } = pcPath(p); if (!rel) throw new Error('Missing file name.'); fs.mkdirSync(path.dirname(full), { recursive: true }); fs.writeFileSync(full, String(content)); return 'Wrote ' + rel + ' (' + String(content).length + ' chars)'; },
  delete({ path: p }) { const { full, rel } = pcPath(p); if (!rel) throw new Error('Refusing to delete the whole folder.'); fs.rmSync(full, { recursive: true, force: true }); return 'Deleted ' + rel; },
  mkdir({ path: p }) { const { full, rel } = pcPath(p); fs.mkdirSync(full, { recursive: true }); return 'Created folder ' + rel; },
};
async function pcJob(job) {
  let out;
  try {
    if (!PC_FOLDER) throw new Error('This host offers no computer access (pc.folder is not set).');
    const name = String(job.path || '').slice(1), args = job.body ? JSON.parse(Buffer.from(job.body, 'base64').toString('utf8')) : {};
    if (!pcOps[name]) throw new Error('Unknown tool ' + name);
    out = { ok: true, result: pcOps[name](args || {}) };
  } catch (e) { out = { ok: false, error: e.message }; }
  await reply(job, 200, { 'content-type': 'application/json' }, Buffer.from(JSON.stringify(out)));
}

// ---------- Work jobs: Claude Code runs here, in this machine's worktree of the project ----------
const runs = new Map(); // job id -> { child, stopped }
function stopRun(id, why) { const r = runs.get(id); if (!r || r.stopped) return; r.stopped = true; log(`stopping run ${id} (${why})`); kill(r.child); }
async function postEvents(jobId, batch) {
  try {
    const r = await fetch(HUB + '/node/events', { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify({ job: jobId, enc: seal(batch) }), signal: AbortSignal.timeout(30000) });
    const j = await r.json().catch(() => ({}));
    if (r.status === 404) return 'gone'; // the hub gave the run up: stop it here too
    return j.cancel ? 'cancel' : 'ok';
  } catch (e) { log('could not send events:', e.message); return 'ok'; }
}
function workDirFor(payload) {
  if (payload.sandbox) { const d = path.join(__dirname, '..', 'Api', 'sandbox'); fs.mkdirSync(d, { recursive: true }); return { dir: d, note: 'sandbox' }; }
  const worktree = require('./worktree');
  const tryEnsure = () => worktree.ensure(payload.account, payload.hub ? undefined : payload.project);
  try { return { dir: tryEnsure().dir, note: 'worktree' }; }
  catch (e) {
    if (!payload.repoUrl) throw new Error(`This host has no copy of the project ${payload.project} and the hub gave no address to clone it from (${e.message}).`);
    if (payload.hub) {
      const dest = process.env.VIBEROLLER_HUB_REPO || path.join(worktree.PROJECTS_ROOT, 'VibeRoller', '_repo');
      if (!fs.existsSync(path.join(dest, '.git'))) { log(`cloning VibeRoller from ${payload.repoUrl} into ${dest}`); fs.mkdirSync(path.dirname(dest), { recursive: true }); require('child_process').execFileSync('git', ['clone', '-q', '--', payload.repoUrl, dest], { stdio: 'ignore', windowsHide: true }); require('child_process').execFileSync('git', ['-C', dest, 'checkout', '-q', 'develop'], { stdio: 'ignore', windowsHide: true }); }
      process.env.VIBEROLLER_HUB_REPO = dest; delete require.cache[require.resolve('./worktree')];
      return { dir: require('./worktree').ensure(payload.account).dir, note: 'worktree (fresh clone)' };
    }
    log(`pulling in ${payload.project} from ${payload.repoUrl}`);
    worktree.clone(payload.repoUrl, payload.project, USER || 'host');
    return { dir: tryEnsure().dir, note: 'worktree (fresh clone)' };
  }
}
async function workJob(job) {
  let payload; try { payload = open(job.enc || {}); } catch { log('a Work job arrived in an envelope this key cannot open; saying hello again'); key = null; return; }
  const jobId = job.id, p = PROVIDERS.find((x) => x.id === payload.providerId);
  const events = []; let flushing = null, closed = false;
  const flush = async (final) => {
    if (flushing) await flushing;
    if (!events.length && !final) return;
    const batch = { events: events.splice(0), usage: p ? usage[p.id] || null : null, ...(final || {}) };
    flushing = postEvents(jobId, batch).then((r) => { if (r === 'cancel' || r === 'gone') stopRun(jobId, r === 'gone' ? 'the hub gave it up' : 'stopped by its person'); });
    await flushing;
  };
  const emit = (ev) => { events.push(ev); if (!closed && events.length >= 40) flush(); };
  if (!p || p.kind === 'openai') { emit({ type: 'bridge_error', text: 'This host no longer offers that provider for Work.' }); return flush({ done: true, code: 1 }); }
  const cap = overCap(p); if (cap) { emit({ type: 'bridge_error', text: `${nickname}: ${cap}. Pick another AI or wait for the window to reset.` }); return flush({ done: true, code: 1 }); }
  let wd; try { wd = workDirFor(payload); } catch (e) { emit({ type: 'bridge_error', text: `${nickname} could not prepare the folder: ${e.message}` }); return flush({ done: true, code: 1 }); }
  const mcpFile = path.join(__dirname, `.mcp-host-${jobId.replace(/[^A-Za-z0-9_-]/g, '')}.json`);
  const args = ['-p', '--output-format', 'stream-json', '--verbose', '--permission-mode', payload.sandbox ? 'default' : String(payload.permissionMode || 'acceptEdits')];
  if (payload.sandbox) args.push('--strict-mcp-config');
  else {
    const env = { MILLS_BRIDGE: String(payload.mcpBase || ''), MILLS_INTERNAL: String(payload.token || ''), MILLS_USER: String(payload.display || payload.account), MILLS_ACCOUNT: String(payload.account) };
    fs.writeFileSync(mcpFile, JSON.stringify({ mcpServers: { pc: { command: process.execPath, args: [path.join(__dirname, 'pc-tools-mcp.js')], env }, board: { command: process.execPath, args: [path.join(__dirname, 'board-mcp.js')], env }, team: { command: process.execPath, args: [path.join(__dirname, 'team-mcp.js')], env } } }));
    args.push('--mcp-config', mcpFile, '--strict-mcp-config');
    const tools = Array.isArray(payload.allowedTools) ? payload.allowedTools : [];
    if (!payload.hub) tools.push(`Bash(node ${path.join(__dirname, 'worktree.js').replace(/\\/g, '/')}:*)`);
    if (tools.length) args.push('--allowedTools', tools.join(','));
  }
  if (payload.systemPrompt) args.push('--append-system-prompt', String(payload.systemPrompt));
  if (payload.model) args.push('--model', String(payload.model));
  if (payload.resume) args.push('--resume', String(payload.resume));
  const env = { ...process.env, ...(p.kind === 'claude-cli' && p.configDir ? { CLAUDE_CONFIG_DIR: String(p.configDir) } : {}), ...(p.kind === 'anthropic' && p.apiKey ? { ANTHROPIC_API_KEY: String(p.apiKey) } : {}), ...(payload.env && typeof payload.env === 'object' ? payload.env : {}) };
  const command = String(WORK.claudeCommand || 'claude'), pre = Array.isArray(WORK.claudeArgs) ? WORK.claudeArgs : [];
  log(`run ${jobId} for ${payload.account}: ${payload.sandbox ? 'API answer' : payload.project + ' in ' + wd.dir} (${p.label}${payload.model ? ', ' + payload.model : ''})`);
  let child;
  try { child = spawn(command, [...pre, ...args], { cwd: wd.dir, windowsHide: true, env }); }
  catch (e) { emit({ type: 'bridge_error', text: `${nickname} could not start claude: ${e.message}` }); try { fs.unlinkSync(mcpFile); } catch {} return flush({ done: true, code: 1 }); }
  const run = { child, stopped: false };
  runs.set(jobId, run);
  const timer = setInterval(() => flush(), 150);
  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk; let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try { const ev = JSON.parse(line); noteUsage(p.id, ev); emit(ev); } catch { emit({ type: 'raw', text: line }); }
    }
  });
  child.stderr.on('data', (d) => emit({ type: 'stderr', text: String(d) }));
  child.on('error', (e) => emit({ type: 'bridge_error', text: `${nickname} could not start claude: ${e.message}` }));
  child.on('close', async (code) => {
    closed = true; clearInterval(timer); runs.delete(jobId);
    try { fs.unlinkSync(mcpFile); } catch {}
    await flush({ done: true, code: run.stopped ? 130 : code });
    log(`run ${jobId} ended (code ${code})`);
  });
  child.stdin.end(String(payload.message || ''));
}
function kill(child) { try { if (process.platform === 'win32') spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' }); else child.kill('SIGTERM'); } catch {} }

// ---------- the loop ----------
(async function loop() {
  let paused = false, quiet = 0;
  for (;;) {
    if (fs.existsSync(PAUSE_FILE)) { if (!paused) { paused = true; log('paused by this machine\'s host (ai-host.off)'); } await sleep(3000); continue; }
    if (paused) { paused = false; log('back on'); }
    try {
      if (!key) await hello();
      const r = await fetch(HUB + '/node/poll', { headers, signal: AbortSignal.timeout(pollMs + 15000) });
      if (r.status === 401) { log('the hub does not know this host (id / token): check ai-node.json'); key = null; await sleep(30000); continue; }
      if (!r.ok) throw new Error('hub answered ' + r.status);
      const { jobs = [], cancel = [] } = await r.json();
      quiet = 0;
      for (const id of cancel) stopRun(id, 'stopped by its person');
      for (const job of jobs) { // in parallel; the next poll starts right away
        if (job.kind === 'work') workJob(job);
        else if (job.engine === 'pc') pcJob(job);
        else relayJob(job);
      }
    } catch (e) {
      if (e.auth) { log(e.message); await sleep(60000); continue; }
      if (quiet++ % 20 === 0) log('hub not reachable:', e.message);
      await sleep(3000);
    }
  }
})();
