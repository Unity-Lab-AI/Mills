#!/usr/bin/env node
// Mills bridge — runs on Gee's PC. Runs Claude Code (Gee's account) inside the Mills
// folder and exposes it to Alfreddo's browser through a Cloudflare quick tunnel.
// Claude edits files on Alfreddo's PC through the "pc" MCP tools, which this bridge
// relays to his browser (it holds the folder he opened).
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawn, execFile } = require('child_process');

const ROOT = __dirname;
const MILLS_DIR = path.join(ROOT, '..');
const WEB_DIR = path.join(MILLS_DIR, 'web');
const AUTH_FILE = path.join(ROOT, 'auth.json');
const STATE_FILE = path.join(ROOT, 'state.json');
const CONFIG_FILE = path.join(ROOT, 'config.json');
const MCP_RUNTIME = path.join(ROOT, '.mcp-runtime.json');

function readJson(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return fallback; }
}
function writeJson(file, data) { fs.writeFileSync(file, JSON.stringify(data, null, 2)); }

const config = Object.assign({
  port: 7777,
  allowedOrigins: ['https://unity-lab-ai.github.io'],
  permissionMode: 'acceptEdits',
  allowedTools: ['mcp__pc', 'Read', 'Edit', 'Write', 'Glob', 'Grep', 'WebFetch', 'WebSearch'],
  model: '',
  tunnel: true,
  cloudflared: 'cloudflared',
  gistId: '',
}, readJson(CONFIG_FILE, {}));
const localOrigins = [`http://localhost:${config.port}`, `http://127.0.0.1:${config.port}`];

let state = readJson(STATE_FILE, {});
function saveState() { writeJson(STATE_FILE, state); }

if (process.argv.includes('--reset-lock')) {
  delete state.lockedDevice; delete state.lockedAt; saveState();
  console.log('Device lock cleared. The next successful sign-in will claim Mills.');
  process.exit(0);
}

// ---------- auth ----------
const sha256 = (s) => crypto.createHash('sha256').update(s).digest('hex');
function checkPassword(password) {
  const auth = readJson(AUTH_FILE, null);
  if (!auth || typeof password !== 'string') return false;
  const derived = crypto.scryptSync(password, Buffer.from(auth.salt, 'hex'), 64, { N: auth.N, r: auth.r, p: auth.p });
  return crypto.timingSafeEqual(derived, Buffer.from(auth.hash, 'hex'));
}
const sessions = new Map(); // token -> expiry
const SESSION_MS = 12 * 60 * 60 * 1000;
let failures = 0, blockedUntil = 0;
function authed(req) {
  const m = /^Bearer (.+)$/.exec(req.headers.authorization || '');
  const exp = m && sessions.get(m[1]);
  if (!exp || exp < Date.now()) { if (m) sessions.delete(m[1]); return false; }
  return true;
}

// ---------- claude runner + pc tool relay ----------
const INTERNAL_SECRET = crypto.randomBytes(24).toString('hex');
writeJson(MCP_RUNTIME, {
  mcpServers: {
    pc: {
      command: process.execPath,
      args: [path.join(ROOT, 'pc-tools-mcp.js')],
      env: { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET },
    },
  },
});

const SYSTEM_APPEND = [
  'You are talking with Alfreddo (Unity AI Lab engineer) through the Mills web interface, running on Gee\'s account.',
  'Alfreddo has opened a folder on HIS OWN PC in his browser. To build or edit anything on his PC, use the mcp__pc__* tools',
  '(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on Gee\'s Mills folder, not his PC.',
  'Shell commands cannot run on Alfreddo\'s PC; if something needs running there, tell him exactly what to run.',
].join(' ');

let running = null;        // child process
let stream = null;         // the open NDJSON response to Alfreddo's browser
const pending = new Map(); // tool call id -> { res, timer }

function emit(ev) { if (stream) stream.write(JSON.stringify(ev) + '\n'); }

function runClaude(message, fresh, res) {
  const args = ['-p', '--output-format', 'stream-json', '--verbose',
    '--permission-mode', config.permissionMode,
    '--mcp-config', MCP_RUNTIME, '--strict-mcp-config',
    '--append-system-prompt', SYSTEM_APPEND];
  if (config.allowedTools.length) args.push('--allowedTools', config.allowedTools.join(','));
  if (config.model) args.push('--model', config.model);
  if (!fresh && state.claudeSessionId) args.push('--resume', state.claudeSessionId);

  const child = spawn('claude', args, { cwd: MILLS_DIR, windowsHide: true });
  running = child; stream = res;
  res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
  const keepAlive = setInterval(() => emit({ type: 'ping' }), 15000);

  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try {
        const ev = JSON.parse(line);
        if (ev.session_id) state.claudeSessionId = ev.session_id;
        emit(ev);
      } catch { emit({ type: 'raw', text: line }); }
    }
  });
  child.stderr.on('data', (d) => emit({ type: 'stderr', text: String(d) }));
  child.on('error', (e) => emit({ type: 'bridge_error', text: 'Could not start claude: ' + e.message }));
  child.on('close', (code) => {
    clearInterval(keepAlive);
    for (const [id, p] of pending) { clearTimeout(p.timer); send(p.res, 200, { ok: false, error: 'Run ended' }); pending.delete(id); }
    running = null; saveState();
    res.end(JSON.stringify({ type: 'bridge_done', code }) + '\n');
    stream = null;
  });
  child.stdin.end(message);
}

function stopClaude() {
  if (!running) return;
  if (process.platform === 'win32') spawn('taskkill', ['/pid', String(running.pid), '/T', '/F']);
  else running.kill('SIGTERM');
}

// ---------- http ----------
function send(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(body));
}
function readBody(req) {
  return new Promise((resolve, reject) => {
    let data = '';
    req.on('data', (c) => { data += c; if (data.length > 20e6) req.destroy(); });
    req.on('end', () => { try { resolve(data ? JSON.parse(data) : {}); } catch (e) { reject(e); } });
    req.on('error', reject);
  });
}
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png' };

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;

  try {
    // Internal channel from the MCP relay (same machine, shared secret).
    if (p === '/internal/pc-tool') {
      if (req.headers['x-mills-internal'] !== INTERNAL_SECRET) return send(res, 403, { ok: false, error: 'forbidden' });
      const { name, args } = await readBody(req);
      if (!stream) return send(res, 200, { ok: false, error: "Alfreddo's browser is not connected." });
      const id = crypto.randomBytes(8).toString('hex');
      const timer = setTimeout(() => { pending.delete(id); send(res, 200, { ok: false, error: 'Timed out waiting for the browser.' }); }, 120000);
      pending.set(id, { res, timer });
      return emit({ type: 'pc_tool', id, name, args });
    }

    const origin = req.headers.origin;
    if (origin && (config.allowedOrigins.includes(origin) || localOrigins.includes(origin))) {
      res.setHeader('Access-Control-Allow-Origin', origin);
      res.setHeader('Vary', 'Origin');
      res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization');
      res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    } else if (origin) {
      return send(res, 403, { error: 'Origin not allowed' });
    }
    if (req.method === 'OPTIONS') { res.writeHead(204); return res.end(); }

    if (!p.startsWith('/api/')) {
      const file = path.join(WEB_DIR, p === '/' ? 'index.html' : path.normalize(p));
      if (!file.startsWith(WEB_DIR) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) return send(res, 404, { error: 'Not found' });
      res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
      return fs.createReadStream(file).pipe(res);
    }

    if (p === '/api/status' && req.method === 'GET') return send(res, 200, { ok: true });

    if (p === '/api/login' && req.method === 'POST') {
      if (Date.now() < blockedUntil) return send(res, 429, { error: 'Too many failed attempts. Try again in 15 minutes.' });
      const { password, deviceId } = await readBody(req);
      if (!checkPassword(password) || typeof deviceId !== 'string' || deviceId.length < 16) {
        if (++failures >= 5) { blockedUntil = Date.now() + 15 * 60 * 1000; failures = 0; }
        return send(res, 401, { error: 'Wrong password.' });
      }
      const deviceHash = sha256(deviceId);
      if (!state.lockedDevice) {
        state.lockedDevice = deviceHash; state.lockedAt = new Date().toISOString(); saveState();
        console.log(`[mills] Claimed by first sign-in at ${state.lockedAt}. Everyone else is locked out.`);
      } else if (state.lockedDevice !== deviceHash) {
        console.log('[mills] Refused a sign-in from another device.');
        return send(res, 403, { error: 'Mills is locked to another device.' });
      }
      failures = 0;
      const token = crypto.randomBytes(32).toString('hex');
      sessions.set(token, Date.now() + SESSION_MS);
      return send(res, 200, { token });
    }

    if (!authed(req)) return send(res, 401, { error: 'Not signed in.' });

    if (p === '/api/check' && req.method === 'GET') return send(res, 200, { ok: true });

    if (p === '/api/logout' && req.method === 'POST') {
      sessions.delete((req.headers.authorization || '').slice(7));
      return send(res, 200, { ok: true });
    }

    if (p === '/api/chat' && req.method === 'POST') {
      if (running) return send(res, 409, { error: 'Claude is already working. Stop it first.' });
      const { message, fresh } = await readBody(req);
      if (!message || typeof message !== 'string') return send(res, 400, { error: 'Empty message.' });
      if (fresh) delete state.claudeSessionId;
      return runClaude(message, !!fresh, res);
    }

    if (p === '/api/pc-result' && req.method === 'POST') {
      const { id, ok, result, error } = await readBody(req);
      const entry = pending.get(id);
      if (entry) { clearTimeout(entry.timer); pending.delete(id); send(entry.res, 200, { ok: !!ok, result, error }); }
      return send(res, 200, { ok: true });
    }

    if (p === '/api/stop' && req.method === 'POST') { stopClaude(); return send(res, 200, { ok: true }); }

    return send(res, 404, { error: 'Unknown endpoint' });
  } catch (e) {
    return send(res, 400, { error: e.message });
  }
});

// ---------- tunnel ----------
// The web app finds the current tunnel URL in a gist named "Mills bridge address"
// on Gee's GitHub account (created here on first run).
function publishUrl(publicUrl) {
  const content = JSON.stringify({ url: publicUrl, updated: new Date().toISOString() });
  const gistId = config.gistId || state.gistId;
  const args = gistId
    ? ['api', '-X', 'PATCH', `gists/${gistId}`, '-f', `files[mills-bridge.json][content]=${content}`]
    : ['api', '-X', 'POST', 'gists', '-f', 'description=Mills bridge address', '-F', 'public=true',
       '-f', `files[mills-bridge.json][content]=${content}`, '--jq', '.id'];
  execFile('gh', args, (err, stdout) => {
    if (err) return console.log('[mills] Could not publish tunnel URL (is gh logged in?): ' + err.message);
    if (!gistId) { state.gistId = stdout.trim(); saveState(); }
    console.log('[mills] Published tunnel URL for the web app.');
  });
}
function startTunnel() {
  const cf = spawn(config.cloudflared, ['tunnel', '--no-autoupdate', '--url', `http://127.0.0.1:${config.port}`], { windowsHide: true });
  let announced = false;
  const scan = (d) => {
    const m = /https:\/\/[a-z0-9-]+\.trycloudflare\.com/.exec(String(d));
    if (m && !announced) { announced = true; console.log('[mills] Public URL: ' + m[0]); publishUrl(m[0]); }
  };
  cf.stdout.on('data', scan); cf.stderr.on('data', scan);
  cf.on('error', (e) => console.log('[mills] cloudflared failed to start: ' + e.message));
  cf.on('close', (code) => { console.log(`[mills] Tunnel exited (${code}); restarting in 5s.`); setTimeout(startTunnel, 5000); });
  process.on('exit', () => cf.kill());
}

server.listen(config.port, '127.0.0.1', () => {
  console.log(`[mills] Bridge running at http://localhost:${config.port}  (Claude works in ${MILLS_DIR})`);
  console.log(`[mills] Device lock: ${state.lockedDevice ? 'claimed ' + state.lockedAt : 'unclaimed — first sign-in claims it'}`);
  if (!fs.existsSync(AUTH_FILE)) console.log('[mills] WARNING: no bridge/auth.json — run "node set-password.js <password>" first.');
  if (config.tunnel && !process.argv.includes('--no-tunnel')) startTunnel();
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(0));
