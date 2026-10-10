#!/usr/bin/env node
// Mills bridge — runs on Alfreddo's PC. Listens on 127.0.0.1 only.
// The web UI (GitHub Pages, or http://localhost:7777 served from here) talks to it.
// It runs Claude Code inside the workspace folder and exposes a confined file API.
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const { spawn } = require('child_process');

const ROOT = __dirname;
const WEB_DIR = path.join(ROOT, '..', 'web');
const AUTH_FILE = path.join(ROOT, 'auth.json');
const STATE_FILE = path.join(ROOT, 'state.json');
const CONFIG_FILE = path.join(ROOT, 'config.json');

const config = Object.assign({
  port: 7777,
  workspace: path.join(os.homedir(), 'Mills-Workspace'),
  allowedOrigins: ['https://unity-lab-ai.github.io'],
  permissionMode: 'acceptEdits',
  allowedTools: ['Bash', 'Edit', 'Write', 'Read', 'Glob', 'Grep', 'WebFetch', 'WebSearch'],
  model: '',
}, readJson(CONFIG_FILE, {}));

config.workspace = path.resolve(config.workspace.replace(/^~(?=$|[\\/])/, os.homedir()));
fs.mkdirSync(config.workspace, { recursive: true });
const localOrigins = [`http://localhost:${config.port}`, `http://127.0.0.1:${config.port}`];

// ---------- persistence ----------
function readJson(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return fallback; }
}
function writeJson(file, data) {
  fs.writeFileSync(file, JSON.stringify(data, null, 2));
}
let state = readJson(STATE_FILE, {});
function saveState() { writeJson(STATE_FILE, state); }

if (process.argv.includes('--reset-lock')) {
  delete state.lockedDevice; delete state.lockedAt; saveState();
  console.log('Device lock cleared. The next successful sign-in will claim this bridge.');
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

const sessions = new Map(); // token -> expiry ms
const SESSION_MS = 12 * 60 * 60 * 1000;
let failures = 0, blockedUntil = 0;

function authed(req) {
  const m = /^Bearer (.+)$/.exec(req.headers.authorization || '');
  if (!m) return false;
  const exp = sessions.get(m[1]);
  if (!exp || exp < Date.now()) { sessions.delete(m[1]); return false; }
  return true;
}

// ---------- workspace file access ----------
function resolveInWorkspace(rel) {
  const full = path.resolve(config.workspace, String(rel || '').replace(/^[\\/]+/, ''));
  const relCheck = path.relative(config.workspace, full);
  if (relCheck.startsWith('..') || path.isAbsolute(relCheck)) throw new Error('Path escapes workspace');
  return full;
}

// ---------- claude runner ----------
let running = null;

function runClaude(message, fresh, res) {
  const args = ['-p', '--output-format', 'stream-json', '--verbose', '--permission-mode', config.permissionMode];
  if (config.allowedTools.length) args.push('--allowedTools', config.allowedTools.join(','));
  if (config.model) args.push('--model', config.model);
  if (!fresh && state.claudeSessionId) args.push('--resume', state.claudeSessionId);

  const child = spawn('claude', args, { cwd: config.workspace, shell: process.platform === 'win32', windowsHide: true });
  running = child;
  res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache' });

  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try {
        const ev = JSON.parse(line);
        if (ev.session_id) { state.claudeSessionId = ev.session_id; }
        res.write(JSON.stringify(ev) + '\n');
      } catch { res.write(JSON.stringify({ type: 'raw', text: line }) + '\n'); }
    }
  });
  child.stderr.on('data', (d) => res.write(JSON.stringify({ type: 'stderr', text: String(d) }) + '\n'));
  child.on('error', (e) => res.write(JSON.stringify({ type: 'bridge_error', text: 'Could not start claude: ' + e.message }) + '\n'));
  child.on('close', (code) => {
    running = null; saveState();
    res.end(JSON.stringify({ type: 'bridge_done', code }) + '\n');
  });
  child.stdin.end(message);
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
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon' };

const server = http.createServer(async (req, res) => {
  const origin = req.headers.origin;
  if (origin && (config.allowedOrigins.includes(origin) || localOrigins.includes(origin))) {
    res.setHeader('Access-Control-Allow-Origin', origin);
    res.setHeader('Vary', 'Origin');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization');
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, PUT, DELETE, OPTIONS');
    res.setHeader('Access-Control-Allow-Private-Network', 'true');
  } else if (origin) {
    return send(res, 403, { error: 'Origin not allowed' });
  }
  if (req.method === 'OPTIONS') { res.writeHead(204); return res.end(); }

  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;

  try {
    if (!p.startsWith('/api/')) {
      // Serve the same UI locally so it also works without GitHub Pages.
      const file = path.join(WEB_DIR, p === '/' ? 'index.html' : path.normalize(p));
      if (!file.startsWith(WEB_DIR) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) return send(res, 404, { error: 'Not found' });
      res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
      return fs.createReadStream(file).pipe(res);
    }

    if (p === '/api/status' && req.method === 'GET') {
      return send(res, 200, { ok: true, claimed: !!state.lockedDevice, workspace: config.workspace, hasPassword: fs.existsSync(AUTH_FILE) });
    }

    if (p === '/api/login' && req.method === 'POST') {
      if (Date.now() < blockedUntil) return send(res, 429, { error: 'Too many failed attempts. Try again later.' });
      const { password, deviceId } = await readBody(req);
      if (!checkPassword(password) || typeof deviceId !== 'string' || deviceId.length < 16) {
        failures++;
        if (failures >= 5) { blockedUntil = Date.now() + 15 * 60 * 1000; failures = 0; }
        return send(res, 401, { error: 'Wrong password.' });
      }
      const deviceHash = sha256(deviceId);
      if (!state.lockedDevice) {
        state.lockedDevice = deviceHash; state.lockedAt = new Date().toISOString(); saveState();
        console.log(`[mills] Claimed by first sign-in at ${state.lockedAt}. All other devices are now locked out.`);
      } else if (state.lockedDevice !== deviceHash) {
        return send(res, 403, { error: 'This Mills bridge is locked to another device.' });
      }
      failures = 0;
      const token = crypto.randomBytes(32).toString('hex');
      sessions.set(token, Date.now() + SESSION_MS);
      return send(res, 200, { token, workspace: config.workspace });
    }

    if (!authed(req)) return send(res, 401, { error: 'Not signed in.' });

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

    if (p === '/api/stop' && req.method === 'POST') {
      if (running) {
        if (process.platform === 'win32') spawn('taskkill', ['/pid', String(running.pid), '/T', '/F']);
        else running.kill('SIGTERM');
      }
      return send(res, 200, { ok: true });
    }

    if (p === '/api/files' && req.method === 'GET') {
      const dir = resolveInWorkspace(url.searchParams.get('path'));
      const entries = fs.readdirSync(dir, { withFileTypes: true })
        .filter((e) => e.name !== '.git' && e.name !== 'node_modules')
        .map((e) => ({ name: e.name, dir: e.isDirectory() }))
        .sort((a, b) => (b.dir - a.dir) || a.name.localeCompare(b.name));
      return send(res, 200, { entries });
    }

    if (p === '/api/file') {
      const file = resolveInWorkspace(url.searchParams.get('path'));
      if (req.method === 'GET') {
        if (fs.statSync(file).size > 5e6) return send(res, 413, { error: 'File too large to open in the editor.' });
        return send(res, 200, { content: fs.readFileSync(file, 'utf8') });
      }
      if (req.method === 'PUT') {
        const { content } = await readBody(req);
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, String(content ?? ''));
        return send(res, 200, { ok: true });
      }
      if (req.method === 'DELETE') {
        fs.rmSync(file, { recursive: true, force: true });
        return send(res, 200, { ok: true });
      }
    }

    return send(res, 404, { error: 'Unknown endpoint' });
  } catch (e) {
    return send(res, 400, { error: e.message });
  }
});

server.listen(config.port, '127.0.0.1', () => {
  console.log(`[mills] Bridge running at http://localhost:${config.port}`);
  console.log(`[mills] Workspace: ${config.workspace}`);
  console.log(`[mills] Device lock: ${state.lockedDevice ? 'claimed (' + state.lockedAt + ')' : 'unclaimed — first sign-in claims it'}`);
  if (!fs.existsSync(AUTH_FILE)) console.log('[mills] WARNING: no bridge/auth.json — run "node set-password.js" first.');
});
