#!/usr/bin/env node
// Mills hub — runs on Gee's PC under run.js (which owns the Cloudflare quick tunnel and restarts
// this app when asked). ONE process serves every account: username + password sign-in, the Work
// section (Claude Code on Gee's account, editing files on the signed-in person's PC through the
// "pc" MCP tools relayed to their browser), the end-to-end encrypted Chat room, and Gee's Admin
// section.
//
// Accounts are the folders the bridge always had, so existing passwords keep working:
//   "mills" (Alfreddo)  bridge/auth.json, bridge/state.json, Mills/Archive/
//   "<name>"            bridge/profiles/<name>/{config,auth,state}.json, Mills/Archive/<name>/
// profiles/<name>/config.json: { "user": "<display name>", "admin": true|false, "model": "..." }
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');
const { ROOT, MILLS_DIR, readJson, writeJson, loadProfile, allProfiles, hashPassword } = require('./common');

const WEB_DIR = path.join(MILLS_DIR, 'web');
const HUB = loadProfile('');          // hub-wide config: port, origins, tools, models, tunnel
const config = HUB.config;
const RESTART_FLAG = HUB.RESTART_FLAG;
const HUB_STATE_FILE = path.join(ROOT, 'hub-state.json'); // sign-in sessions for every account
const CHAT_DIR = path.join(MILLS_DIR, 'Chat');             // ciphertext only; gitignored
const CHAT_FILE = path.join(CHAT_DIR, 'messages.jsonl');
const KEYS_FILE = path.join(CHAT_DIR, 'keys.json');
const DELETED_DIR = path.join(ROOT, 'profiles-deleted');
const DEFAULT_USERNAME = config.defaultUsername || 'mills';
const USERNAME_RE = /^[a-z0-9-]{1,32}$/;
const SUPERVISED = !!process.env.MILLS_SUPERVISED; // started by run.js: tunnel + self-restart available
const localOrigins = [`http://localhost:${config.port}`, `http://127.0.0.1:${config.port}`];
const INTERNAL_SECRET = crypto.randomBytes(24).toString('hex');
const SESSION_MS = 12 * 60 * 60 * 1000;
const newPassword = () => crypto.randomBytes(15).toString('base64url'); // 20 characters

// ---------- accounts ----------
const accounts = new Map(); // username -> account
function systemPrompt(name) {
  return [
    `You are talking with ${name} (Unity AI Lab engineer) through the Mills web interface, running on Gee's account.`,
    `${name} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools`,
    `(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on Gee's Mills folder, not ${name}'s PC.`,
    `Shell commands cannot run on ${name}'s PC; if something needs running there, tell ${name} exactly what to run.`,
  ].join(' ');
}
function loadAccounts() {
  const seen = new Set();
  for (const profile of allProfiles()) {
    const P = loadProfile(profile);
    const username = profile || DEFAULT_USERNAME;
    if (seen.has(username)) { console.log(`[mills] Skipping profiles/${profile}: "${username}" is already an account.`); continue; }
    seen.add(username);
    const acct = accounts.get(username) || { running: null, stream: null, pending: new Map(), state: readJson(P.STATE_FILE, {}) };
    Object.assign(acct, {
      username, profile, files: P,
      display: P.config.user || username, admin: !!P.config.admin, defaultModel: P.config.model || '',
    });
    if (acct.state.model && !modelIds().includes(acct.state.model)) delete acct.state.model;
    fs.mkdirSync(P.PROFILE_DIR, { recursive: true });
    writeJson(P.MCP_RUNTIME, { mcpServers: { pc: {
      command: process.execPath, args: [path.join(ROOT, 'pc-tools-mcp.js')],
      env: { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET, MILLS_USER: acct.display, MILLS_ACCOUNT: username },
    } } });
    accounts.set(username, acct);
  }
  for (const u of [...accounts.keys()]) if (!seen.has(u)) accounts.delete(u);
}
const modelIds = () => config.models.map((m) => m.id);
const saveState = (acct) => writeJson(acct.files.STATE_FILE, acct.state);
const effectiveModel = (acct) => acct.state.model || acct.defaultModel;
function checkPassword(acct, password) {
  const auth = acct && readJson(acct.files.AUTH_FILE, null);
  if (!auth || typeof password !== 'string') return false;
  const derived = crypto.scryptSync(password, Buffer.from(auth.salt, 'hex'), 64, { N: auth.N, r: auth.r, p: auth.p });
  return crypto.timingSafeEqual(derived, Buffer.from(auth.hash, 'hex'));
}
loadAccounts();

// ---------- sessions (persisted: a restart signs nobody out) ----------
const hubState = readJson(HUB_STATE_FILE, {});
const sessions = new Map(Object.entries(hubState.sessions || {}).filter(([, s]) => s && s.exp > Date.now() && accounts.has(s.user)));
for (const acct of accounts.values()) { // sessions saved by the old one-bridge-per-person layout
  if (!acct.state.sessions) continue;
  for (const [t, exp] of Object.entries(acct.state.sessions)) if (exp > Date.now()) sessions.set(t, { user: acct.username, exp });
  delete acct.state.sessions; saveState(acct);
}
function saveSessions() { hubState.sessions = Object.fromEntries(sessions); writeJson(HUB_STATE_FILE, hubState); }
saveSessions();
function sessionAccount(req) {
  const m = /^Bearer (.+)$/.exec(req.headers.authorization || '');
  const s = m && sessions.get(m[1]);
  if (!s || s.exp < Date.now() || !accounts.has(s.user)) { if (s) { sessions.delete(m[1]); saveSessions(); } return null; }
  return accounts.get(s.user);
}
function dropSessions(username) {
  for (const [t, s] of sessions) if (s.user === username) sessions.delete(t);
  saveSessions();
}
const failures = new Map(); // username -> { count, blockedUntil }

// ---------- claude runner + pc tool relay (one per account) ----------
function emit(acct, ev) { if (acct.stream) acct.stream.write(JSON.stringify(ev) + '\n'); }
function runClaude(acct, message, fresh, res) {
  const args = ['-p', '--output-format', 'stream-json', '--verbose',
    '--permission-mode', config.permissionMode,
    '--mcp-config', acct.files.MCP_RUNTIME, '--strict-mcp-config',
    '--append-system-prompt', systemPrompt(acct.display)];
  if (config.allowedTools.length) args.push('--allowedTools', config.allowedTools.join(','));
  const model = effectiveModel(acct); // read per run, so a dropdown change lands on the very next message
  if (model) args.push('--model', model);
  if (!fresh && acct.state.claudeSessionId) args.push('--resume', acct.state.claudeSessionId);

  const child = spawn('claude', args, { cwd: MILLS_DIR, windowsHide: true });
  acct.running = child; acct.stream = res;
  res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
  const keepAlive = setInterval(() => emit(acct, { type: 'ping' }), 15000);
  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try { const ev = JSON.parse(line); if (ev.session_id) acct.state.claudeSessionId = ev.session_id; emit(acct, ev); }
      catch { emit(acct, { type: 'raw', text: line }); }
    }
  });
  child.stderr.on('data', (d) => emit(acct, { type: 'stderr', text: String(d) }));
  child.on('error', (e) => emit(acct, { type: 'bridge_error', text: 'Could not start claude: ' + e.message }));
  child.on('close', (code) => {
    clearInterval(keepAlive);
    for (const [id, p] of acct.pending) { clearTimeout(p.timer); send(p.res, 200, { ok: false, error: 'Run ended' }); acct.pending.delete(id); }
    acct.running = null; saveState(acct);
    res.end(JSON.stringify({ type: 'bridge_done', code }) + '\n');
    acct.stream = null;
    setTimeout(maybeRestart, 300); // a restart requested during this run happens now that it is over
  });
  child.stdin.end(message);
}
function stopClaude(acct) {
  if (!acct.running) return;
  if (process.platform === 'win32') spawn('taskkill', ['/pid', String(acct.running.pid), '/T', '/F'], { windowsHide: true });
  else acct.running.kill('SIGTERM');
}

// ---------- conversation archive (per account) ----------
const archiveFile = (acct, id) => {
  if (typeof id !== 'string' || !/^[a-f0-9]{16}$/.test(id)) throw new Error('Bad conversation id.');
  return path.join(acct.files.ARCHIVE_DIR, id + '.json');
};
const cleanTitle = (t) => String(t || '').replace(/\s+/g, ' ').trim().slice(0, 120) || 'Untitled conversation';
function listArchive(acct) {
  const dir = acct.files.ARCHIVE_DIR;
  if (!fs.existsSync(dir)) return [];
  return fs.readdirSync(dir).filter((f) => f.endsWith('.json'))
    .map((f) => readJson(path.join(dir, f), null)).filter(Boolean)
    .map(({ id, title, savedAt, transcript }) => ({ id, title, savedAt, count: (transcript || []).length }))
    .sort((a, b) => String(b.savedAt).localeCompare(String(a.savedAt)));
}

// ---------- chat room (end-to-end encrypted) ----------
// Browsers encrypt every message with a fresh AES-GCM key and wrap that key separately for each
// member (ECDH P-256 between sender and member). This hub only stores public keys, password-
// wrapped private-key backups it cannot open, and ciphertext. Each reader gets only their own
// wrapped key slot.
fs.mkdirSync(CHAT_DIR, { recursive: true });
let chatKeys = readJson(KEYS_FILE, {}); // username -> { pub (JWK), backup: {salt, iv, ct}, updated }
let messages = fs.existsSync(CHAT_FILE)
  ? fs.readFileSync(CHAT_FILE, 'utf8').split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean)
  : [];
let seq = messages.reduce((m, x) => Math.max(m, x.seq || 0), 0);
const b64 = (s, max) => typeof s === 'string' && s.length <= max && /^[A-Za-z0-9+/=_-]*$/.test(s);
const validJwk = (k) => k && typeof k === 'object' && k.kty === 'EC' && k.crv === 'P-256' && b64(k.x, 64) && b64(k.y, 64) && !k.d;

// ---------- self-restart ----------
// `node restart.js` drops bridge/.restart. As soon as nobody's Claude run is in progress the app
// exits with code 75 and run.js starts it again under the same tunnel (same public address).
function maybeRestart() {
  if ([...accounts.values()].some((a) => a.running) || !fs.existsSync(RESTART_FLAG)) return;
  try { fs.unlinkSync(RESTART_FLAG); } catch {}
  if (!SUPERVISED) return console.log('[mills] Restart requested, but this hub was started without run.js (start.bat / start.sh) — restart it by hand.');
  console.log('[mills] Restart requested — picking up the new code/config now.');
  server.close();
  setTimeout(() => process.exit(75), 200);
}
setInterval(maybeRestart, 2000).unref();

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
function accountSummary(a) {
  return {
    username: a.username, display: a.display, admin: a.admin,
    locked: !!a.state.lockedDevice, lockedAt: a.state.lockedAt || null,
    signedIn: [...sessions.values()].filter((s) => s.user === a.username && s.exp > Date.now()).length,
    busy: !!a.running, model: effectiveModel(a) || '', chatKey: !!chatKeys[a.username],
    hasPassword: fs.existsSync(a.files.AUTH_FILE),
  };
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;
  try {
    // Internal channel from the MCP relay (same machine, shared secret, names the account).
    if (p === '/internal/pc-tool') {
      if (req.headers['x-mills-internal'] !== INTERNAL_SECRET) return send(res, 403, { ok: false, error: 'forbidden' });
      const acct = accounts.get(String(req.headers['x-mills-account'] || '') || DEFAULT_USERNAME);
      const { name, args } = await readBody(req);
      if (!acct || !acct.stream) return send(res, 200, { ok: false, error: `${acct ? acct.display : 'The user'}'s browser is not connected.` });
      const id = crypto.randomBytes(8).toString('hex');
      const timer = setTimeout(() => { acct.pending.delete(id); send(res, 200, { ok: false, error: 'Timed out waiting for the browser.' }); }, 120000);
      acct.pending.set(id, { res, timer });
      return emit(acct, { type: 'pc_tool', id, name, args });
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

    if (p === '/api/status' && req.method === 'GET') {
      return send(res, 200, { ok: true, hub: true, pid: process.pid, supervised: SUPERVISED, busy: [...accounts.values()].some((a) => a.running) });
    }

    if (p === '/api/login' && req.method === 'POST') {
      const { username, password, deviceId } = await readBody(req);
      const uname = String(username || '').trim().toLowerCase();
      const f = failures.get(uname) || { count: 0, blockedUntil: 0 };
      if (Date.now() < f.blockedUntil) return send(res, 429, { error: 'Too many failed attempts. Try again in 15 minutes.' });
      const acct = USERNAME_RE.test(uname) ? accounts.get(uname) : null;
      if (!acct) crypto.scryptSync(String(password || ''), 'mills-no-such-user', 64, { N: 16384, r: 8, p: 1 }); // same cost either way
      if (!acct || !checkPassword(acct, password) || typeof deviceId !== 'string' || deviceId.length < 16) {
        if (failures.size > 1000) failures.clear();
        if (++f.count >= 5) { f.blockedUntil = Date.now() + 15 * 60 * 1000; f.count = 0; }
        failures.set(uname, f);
        return send(res, 401, { error: 'Wrong username or password.' });
      }
      const deviceHash = crypto.createHash('sha256').update(deviceId).digest('hex');
      if (!acct.state.lockedDevice) {
        acct.state.lockedDevice = deviceHash; acct.state.lockedAt = new Date().toISOString(); saveState(acct);
        console.log(`[mills] ${acct.username}: claimed by first sign-in at ${acct.state.lockedAt}.`);
      } else if (acct.state.lockedDevice !== deviceHash) {
        console.log(`[mills] ${acct.username}: refused a sign-in from another device.`);
        return send(res, 403, { error: 'This account is locked to another device. Ask Gee to reset it.' });
      }
      failures.delete(uname);
      const token = crypto.randomBytes(32).toString('hex');
      sessions.set(token, { user: acct.username, exp: Date.now() + SESSION_MS }); saveSessions();
      return send(res, 200, { token, username: acct.username });
    }

    const acct = sessionAccount(req);
    if (!acct) return send(res, 401, { error: 'Not signed in.' });

    if (p === '/api/check' && req.method === 'GET') {
      return send(res, 200, { ok: true, username: acct.username, display: acct.display, admin: acct.admin,
        models: config.models, model: acct.state.model || '', defaultModel: acct.defaultModel });
    }
    if (p === '/api/logout' && req.method === 'POST') {
      sessions.delete((req.headers.authorization || '').slice(7)); saveSessions();
      return send(res, 200, { ok: true });
    }

    // ----- work -----
    if (p === '/api/model' && req.method === 'POST') {
      const { model } = await readBody(req);
      if (typeof model !== 'string' || (model && !modelIds().includes(model))) return send(res, 400, { error: 'Unknown model.' });
      if (model) acct.state.model = model; else delete acct.state.model;
      saveState(acct);
      return send(res, 200, { model: acct.state.model || '', effective: effectiveModel(acct) });
    }
    if (p === '/api/chat' && req.method === 'POST') {
      if (acct.running) return send(res, 409, { error: 'Claude is already working. Stop it first.' });
      const { message, fresh } = await readBody(req);
      if (!message || typeof message !== 'string') return send(res, 400, { error: 'Empty message.' });
      if (fresh) delete acct.state.claudeSessionId;
      return runClaude(acct, message, !!fresh, res);
    }
    if (p === '/api/pc-result' && req.method === 'POST') {
      const { id, ok, result, error } = await readBody(req);
      const entry = acct.pending.get(id);
      if (entry) { clearTimeout(entry.timer); acct.pending.delete(id); send(entry.res, 200, { ok: !!ok, result, error }); }
      return send(res, 200, { ok: true });
    }
    if (p === '/api/stop' && req.method === 'POST') { stopClaude(acct); return send(res, 200, { ok: true }); }

    if (p === '/api/archive/list' && req.method === 'POST') return send(res, 200, { items: listArchive(acct) });
    if (p === '/api/archive/save' && req.method === 'POST') {
      const { id, title, transcript } = await readBody(req);
      if (!Array.isArray(transcript) || !transcript.length) return send(res, 400, { error: 'Nothing to save yet.' });
      if (!acct.state.claudeSessionId) return send(res, 400, { error: 'This conversation has no Claude session yet.' });
      const entryId = id || crypto.randomBytes(8).toString('hex');
      const file = archiveFile(acct, entryId);
      const prev = readJson(file, {});
      fs.mkdirSync(acct.files.ARCHIVE_DIR, { recursive: true });
      const entry = {
        id: entryId, title: cleanTitle(title || prev.title), savedAt: new Date().toISOString(),
        sessionId: acct.state.claudeSessionId,
        transcript: transcript.filter((t) => t && typeof t.cls === 'string' && typeof t.text === 'string'),
      };
      writeJson(file, entry);
      return send(res, 200, { id: entry.id, title: entry.title });
    }
    if (p === '/api/archive/open' && req.method === 'POST') {
      if (acct.running) return send(res, 409, { error: 'Claude is working. Stop it first.' });
      const entry = readJson(archiveFile(acct, (await readBody(req)).id), null);
      if (!entry) return send(res, 404, { error: 'Conversation not found.' });
      acct.state.claudeSessionId = entry.sessionId; saveState(acct);
      return send(res, 200, entry);
    }
    if (p === '/api/archive/rename' && req.method === 'POST') {
      const { id, title } = await readBody(req);
      const file = archiveFile(acct, id);
      const entry = readJson(file, null);
      if (!entry) return send(res, 404, { error: 'Conversation not found.' });
      entry.title = cleanTitle(title); writeJson(file, entry);
      return send(res, 200, { id, title: entry.title });
    }
    if (p === '/api/archive/delete' && req.method === 'POST') {
      const file = archiveFile(acct, (await readBody(req)).id);
      if (fs.existsSync(file)) fs.unlinkSync(file);
      return send(res, 200, { ok: true });
    }

    // ----- chat room -----
    if (p === '/api/room/members' && req.method === 'GET') {
      return send(res, 200, { members: [...accounts.values()].map((a) => ({ username: a.username, display: a.display, pub: chatKeys[a.username] ? chatKeys[a.username].pub : null })) });
    }
    if (p === '/api/room/mykey' && req.method === 'GET') return send(res, 200, chatKeys[acct.username] || {});
    if (p === '/api/room/mykey' && req.method === 'POST') {
      const { pub, backup } = await readBody(req);
      if (!validJwk(pub) || !backup || !b64(backup.salt, 64) || !b64(backup.iv, 64) || !b64(backup.ct, 4096)) return send(res, 400, { error: 'Bad key.' });
      chatKeys[acct.username] = { pub: { kty: 'EC', crv: 'P-256', x: pub.x, y: pub.y }, backup: { salt: backup.salt, iv: backup.iv, ct: backup.ct }, updated: new Date().toISOString() };
      writeJson(KEYS_FILE, chatKeys);
      return send(res, 200, { ok: true });
    }
    if (p === '/api/room/send' && req.method === 'POST') {
      const { iv, ct, keys } = await readBody(req);
      const mine = chatKeys[acct.username];
      if (!mine) return send(res, 400, { error: 'Your chat key is not set up yet.' });
      if (!b64(iv, 64) || !b64(ct, 400000) || !keys || typeof keys !== 'object') return send(res, 400, { error: 'Bad message.' });
      const slots = {};
      for (const [u, k] of Object.entries(keys)) if (accounts.has(u) && k && b64(k.iv, 64) && b64(k.wk, 200)) slots[u] = { iv: k.iv, wk: k.wk };
      if (!slots[acct.username]) return send(res, 400, { error: 'Bad message.' });
      const msg = { seq: ++seq, from: acct.username, ts: new Date().toISOString(), fromPub: mine.pub, iv, ct, keys: slots };
      messages.push(msg);
      fs.appendFileSync(CHAT_FILE, JSON.stringify(msg) + '\n');
      return send(res, 200, { seq: msg.seq });
    }
    if (p === '/api/room/messages' && req.method === 'GET') {
      const since = Number(url.searchParams.get('since')) || 0;
      const out = messages.filter((m) => m.seq > since).slice(-300)
        .map(({ keys, ...m }) => ({ ...m, key: keys[acct.username] || null })); // only the reader's own slot
      return send(res, 200, { messages: out, latest: seq });
    }

    // ----- admin (accounts with "admin": true, i.e. gee) -----
    if (p.startsWith('/api/admin/')) {
      if (!acct.admin) return send(res, 403, { error: 'Admins only.' });
      const body = req.method === 'POST' ? await readBody(req) : {};
      const target = body.username !== undefined ? accounts.get(String(body.username)) : null;
      const needTarget = () => { if (!target) throw new Error('No such account.'); return target; };

      if (p === '/api/admin/accounts' && req.method === 'GET') return send(res, 200, { accounts: [...accounts.values()].map(accountSummary), me: acct.username });
      if (p === '/api/admin/create' && req.method === 'POST') {
        const username = String(body.username || '').trim().toLowerCase();
        if (!USERNAME_RE.test(username)) return send(res, 400, { error: 'Usernames use lowercase letters, digits and dashes (max 32).' });
        if (accounts.has(username) || fs.existsSync(path.join(ROOT, 'profiles', username))) return send(res, 400, { error: 'That username is taken.' });
        const dir = path.join(ROOT, 'profiles', username);
        fs.mkdirSync(dir, { recursive: true });
        const display = String(body.display || '').trim().slice(0, 40) || username[0].toUpperCase() + username.slice(1);
        writeJson(path.join(dir, 'config.json'), { user: display, ...(body.admin ? { admin: true } : {}) });
        const password = newPassword();
        writeJson(path.join(dir, 'auth.json'), hashPassword(password));
        loadAccounts();
        console.log(`[mills] ${acct.username} created account "${username}".`);
        return send(res, 200, { username, password });
      }
      if (p === '/api/admin/password' && req.method === 'POST') {
        const t = needTarget(), password = newPassword();
        writeJson(t.files.AUTH_FILE, hashPassword(password));
        if (t !== acct) dropSessions(t.username);
        console.log(`[mills] ${acct.username} set a new password for "${t.username}".`);
        return send(res, 200, { username: t.username, password });
      }
      if (p === '/api/admin/unlock' && req.method === 'POST') {
        const t = needTarget(); delete t.state.lockedDevice; delete t.state.lockedAt; saveState(t);
        return send(res, 200, { ok: true });
      }
      if (p === '/api/admin/signout' && req.method === 'POST') { const t = needTarget(); if (t !== acct) dropSessions(t.username); return send(res, 200, { ok: true }); }
      if (p === '/api/admin/stop' && req.method === 'POST') { stopClaude(needTarget()); return send(res, 200, { ok: true }); }
      if (p === '/api/admin/delete' && req.method === 'POST') {
        const t = needTarget();
        if (t === acct) return send(res, 400, { error: 'You cannot delete your own account.' });
        if (!t.profile) return send(res, 400, { error: `The "${DEFAULT_USERNAME}" account lives in bridge/ itself and cannot be deleted here.` });
        stopClaude(t); dropSessions(t.username);
        fs.mkdirSync(DELETED_DIR, { recursive: true });
        fs.renameSync(t.files.PROFILE_DIR, path.join(DELETED_DIR, `${t.username}-${Date.now()}`)); // kept, not destroyed
        delete chatKeys[t.username]; writeJson(KEYS_FILE, chatKeys);
        loadAccounts();
        console.log(`[mills] ${acct.username} deleted account "${t.username}" (moved to bridge/profiles-deleted/).`);
        return send(res, 200, { ok: true });
      }
      if (p === '/api/admin/restart' && req.method === 'POST') {
        fs.writeFileSync(RESTART_FLAG, new Date().toISOString() + '\n');
        return send(res, 200, { ok: true, supervised: SUPERVISED });
      }
      if (p === '/api/admin/room/clear' && req.method === 'POST') {
        messages = []; fs.writeFileSync(CHAT_FILE, '');
        console.log(`[mills] ${acct.username} cleared the chat history.`);
        return send(res, 200, { ok: true });
      }
      return send(res, 404, { error: 'Unknown admin endpoint' });
    }

    return send(res, 404, { error: 'Unknown endpoint' });
  } catch (e) {
    return send(res, 400, { error: e.message });
  }
});

server.listen(config.port, '127.0.0.1', () => {
  console.log(`[mills] Hub running at http://localhost:${config.port}  (Claude works in ${MILLS_DIR})`);
  for (const a of accounts.values()) {
    console.log(`[mills]   ${a.username.padEnd(10)} ${a.display}${a.admin ? ' (admin)' : ''} · lock ${a.state.lockedDevice ? 'claimed' : 'unclaimed'} · model ${effectiveModel(a) || 'default'}${fs.existsSync(a.files.AUTH_FILE) ? '' : ' · NO PASSWORD SET'}`);
  }
  console.log(`[mills] Sign-ins kept from before this start: ${sessions.size}.`);
  if (!SUPERVISED && config.tunnel && !process.argv.includes('--no-tunnel')) console.log('[mills] Started without run.js: no tunnel and no self-restart. Use start.bat / start.sh for the full hub.');
});
server.on('error', (e) => {
  if (e.code !== 'EADDRINUSE') throw e;
  console.log(`[mills] Mills is ALREADY RUNNING in another window (port ${config.port} is taken).`);
  console.log('[mills] Nothing to do: close this window. To restart, close every "Mills" window first, then run start.bat once.');
  process.exit(2); // run.js treats 2 as "stop, do not retry"
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(0));
