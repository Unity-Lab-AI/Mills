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
const CONFIG_FILE = path.join(ROOT, 'config.json');

// ---------- profile ----------
// One bridge process per person. With no --profile the bridge keeps its original layout
// (Alfreddo: bridge/auth.json, bridge/state.json, Mills/Archive). Any other profile keeps its
// own password, device lock, Claude session, tunnel address and Archive folder under
// bridge/profiles/<name>/ and Mills/Archive/<name>/, so several people can use Mills at once,
// each through their own bridge window and their own tunnel.
const profileFlag = process.argv.indexOf('--profile');
const PROFILE = (profileFlag >= 0 && process.argv[profileFlag + 1]) || process.env.MILLS_PROFILE || '';
if (PROFILE && !/^[a-z0-9-]{1,32}$/.test(PROFILE)) {
  console.error('Profile names use lowercase letters, digits and dashes only.');
  process.exit(1);
}
const PROFILE_DIR = PROFILE ? path.join(ROOT, 'profiles', PROFILE) : ROOT;
const AUTH_FILE = path.join(PROFILE_DIR, 'auth.json');
const STATE_FILE = path.join(PROFILE_DIR, 'state.json');
const MCP_RUNTIME = path.join(PROFILE_DIR, '.mcp-runtime.json');
const ARCHIVE_DIR = PROFILE ? path.join(MILLS_DIR, 'Archive', PROFILE) : path.join(MILLS_DIR, 'Archive'); // saved conversations, one JSON file each
const GIST_FILE = PROFILE ? `mills-bridge-${PROFILE}.json` : 'mills-bridge.json'; // what the web app looks for

function readJson(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return fallback; }
}
function writeJson(file, data) { fs.writeFileSync(file, JSON.stringify(data, null, 2)); }

const baseConfig = readJson(CONFIG_FILE, {});
const profileConfig = PROFILE ? readJson(path.join(PROFILE_DIR, 'config.json'), null) : {};
if (PROFILE && (!profileConfig || !profileConfig.port)) {
  console.error(`Missing bridge/profiles/${PROFILE}/config.json with at least {"port": <number>, "user": "<name>"}.`);
  process.exit(1);
}
if (PROFILE) { delete baseConfig.port; delete baseConfig.gistId; } // never share a port or a gist between profiles
const config = Object.assign({
  port: 7777,
  user: 'Alfreddo', // who sits at the browser for this bridge (shown to Claude and in messages)
  allowedOrigins: ['https://unity-lab-ai.github.io'],
  permissionMode: 'acceptEdits',
  allowedTools: ['mcp__pc', 'Read', 'Edit', 'Write', 'Glob', 'Grep', 'WebFetch', 'WebSearch'],
  model: '',
  tunnel: true,
  cloudflared: 'cloudflared',
  gistId: '',
}, baseConfig, profileConfig);
if (PROFILE && !profileConfig.user) config.user = PROFILE[0].toUpperCase() + PROFILE.slice(1);
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
fs.mkdirSync(PROFILE_DIR, { recursive: true });
writeJson(MCP_RUNTIME, {
  mcpServers: {
    pc: {
      command: process.execPath,
      args: [path.join(ROOT, 'pc-tools-mcp.js')],
      env: { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET, MILLS_USER: config.user },
    },
  },
});

const USER = config.user;
const SYSTEM_APPEND = [
  `You are talking with ${USER} (Unity AI Lab engineer) through the Mills web interface, running on Gee's account.`,
  `${USER} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools`,
  `(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on Gee's Mills folder, not ${USER}'s PC.`,
  `Shell commands cannot run on ${USER}'s PC; if something needs running there, tell ${USER} exactly what to run.`,
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

// ---------- conversation archive ----------
// Each saved conversation keeps its transcript (for display) and the Claude session id
// (so opening it resumes the same conversation with full context).
const archiveFile = (id) => {
  if (typeof id !== 'string' || !/^[a-f0-9]{16}$/.test(id)) throw new Error('Bad conversation id.');
  return path.join(ARCHIVE_DIR, id + '.json');
};
const cleanTitle = (t) => String(t || '').replace(/\s+/g, ' ').trim().slice(0, 120) || 'Untitled conversation';
function listArchive() {
  if (!fs.existsSync(ARCHIVE_DIR)) return [];
  return fs.readdirSync(ARCHIVE_DIR).filter((f) => f.endsWith('.json'))
    .map((f) => readJson(path.join(ARCHIVE_DIR, f), null)).filter(Boolean)
    .map(({ id, title, savedAt, transcript }) => ({ id, title, savedAt, count: (transcript || []).length }))
    .sort((a, b) => String(b.savedAt).localeCompare(String(a.savedAt)));
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
      if (!stream) return send(res, 200, { ok: false, error: `${USER}'s browser is not connected.` });
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

    if (p === '/api/archive/list' && req.method === 'POST') return send(res, 200, { items: listArchive() });

    if (p === '/api/archive/save' && req.method === 'POST') {
      // Saving again with the same id updates that entry (newer transcript + session).
      const { id, title, transcript } = await readBody(req);
      if (!Array.isArray(transcript) || !transcript.length) return send(res, 400, { error: 'Nothing to save yet.' });
      if (!state.claudeSessionId) return send(res, 400, { error: 'This conversation has no Claude session yet.' });
      const entryId = id || crypto.randomBytes(8).toString('hex');
      const file = archiveFile(entryId);
      const prev = readJson(file, {});
      fs.mkdirSync(ARCHIVE_DIR, { recursive: true });
      const entry = {
        id: entryId, title: cleanTitle(title || prev.title), savedAt: new Date().toISOString(),
        sessionId: state.claudeSessionId,
        transcript: transcript.filter((t) => t && typeof t.cls === 'string' && typeof t.text === 'string'),
      };
      writeJson(file, entry);
      return send(res, 200, { id: entry.id, title: entry.title });
    }

    if (p === '/api/archive/open' && req.method === 'POST') {
      if (running) return send(res, 409, { error: 'Claude is working. Stop it first.' });
      const entry = readJson(archiveFile((await readBody(req)).id), null);
      if (!entry) return send(res, 404, { error: 'Conversation not found.' });
      state.claudeSessionId = entry.sessionId; saveState(); // next message continues it
      return send(res, 200, entry);
    }

    if (p === '/api/archive/rename' && req.method === 'POST') {
      const { id, title } = await readBody(req);
      const file = archiveFile(id);
      const entry = readJson(file, null);
      if (!entry) return send(res, 404, { error: 'Conversation not found.' });
      entry.title = cleanTitle(title); writeJson(file, entry);
      return send(res, 200, { id, title: entry.title });
    }

    if (p === '/api/archive/delete' && req.method === 'POST') {
      const file = archiveFile((await readBody(req)).id);
      if (fs.existsSync(file)) fs.unlinkSync(file);
      return send(res, 200, { ok: true });
    }

    return send(res, 404, { error: 'Unknown endpoint' });
  } catch (e) {
    return send(res, 400, { error: e.message });
  }
});

// ---------- tunnel ----------
// The web app finds the current tunnel URL in a gist named "Mills bridge address" (or
// "Mills bridge address (<profile>)") on Gee's GitHub account, created here on first run.
// Each profile has its own gist and its own file name (mills-bridge-<profile>.json), which is
// how the web app tells the bridges apart.
function publishUrl(publicUrl) {
  const content = JSON.stringify({ url: publicUrl, updated: new Date().toISOString() });
  const gistId = config.gistId || state.gistId;
  const description = 'Mills bridge address' + (PROFILE ? ` (${PROFILE})` : '');
  const args = gistId
    ? ['api', '-X', 'PATCH', `gists/${gistId}`, '-f', `files[${GIST_FILE}][content]=${content}`]
    : ['api', '-X', 'POST', 'gists', '-f', `description=${description}`, '-F', 'public=true',
       '-f', `files[${GIST_FILE}][content]=${content}`, '--jq', '.id'];
  execFile('gh', args, (err, stdout) => {
    if (err) return console.log('[mills] Could not publish tunnel URL (is gh logged in?): ' + err.message);
    if (!gistId) { state.gistId = stdout.trim(); saveState(); }
    console.log('[mills] Published tunnel URL for the web app.');
  });
}
function startTunnel() {
  const cf = spawn(config.cloudflared, ['tunnel', '--config', path.join(ROOT, 'cloudflared-quick.yml'),
  '--no-autoupdate', '--url', `http://127.0.0.1:${config.port}`], { windowsHide: true });
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
  console.log(`[mills] Bridge for ${USER}${PROFILE ? ` (profile "${PROFILE}")` : ''} running at http://localhost:${config.port}  (Claude works in ${MILLS_DIR})`);
  console.log(`[mills] Device lock: ${state.lockedDevice ? 'claimed ' + state.lockedAt : 'unclaimed — first sign-in claims it'}`);
  if (!fs.existsSync(AUTH_FILE)) console.log(`[mills] WARNING: no ${path.relative(MILLS_DIR, AUTH_FILE).replace(/\\/g, '/')} — run "node set-password.js${PROFILE ? ' --profile ' + PROFILE : ''} <password>" first.`);
  if (config.tunnel && !process.argv.includes('--no-tunnel')) startTunnel();
});
server.on('error', (e) => {
  if (e.code !== 'EADDRINUSE') throw e;
  console.log(`[mills] Mills${PROFILE ? ` for ${USER}` : ''} is ALREADY RUNNING in another window (port ${config.port} is taken).`);
  console.log(`[mills] Nothing to do: close this window. To restart, close every "Mills Bridge: ${USER}" window first, then run ${PROFILE ? `start.bat ${PROFILE}` : 'start.bat'} once.`);
  process.exit(1);
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(0));

