#!/usr/bin/env node
// Mills hub — runs on the host PC under run.js (which owns the Cloudflare quick tunnel and restarts
// this app when asked). ONE process serves every account: username + password sign-in, the Work
// section (Claude Code on the host's account, editing files on the signed-in person's PC through the
// "pc" MCP tools relayed to their browser), the end-to-end encrypted Chat room, and the Admin
// section.
//
// Accounts are the folders the bridge always had, so existing passwords keep working:
//   default ("mills") bridge/auth.json, bridge/state.json, Mills/Archive/
//   "<name>"            bridge/profiles/<name>/{config,auth,state}.json, Mills/Archive/<name>/
// profiles/<name>/config.json: { "user": "<display name>", "admin": true|false, "model": "..." }
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');
const { spawn } = require('child_process');
const { ROOT, MILLS_DIR, readJson, writeJson, loadProfile, allProfiles, hashPassword } = require('./common');
const worktree = require('./worktree');

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
// Where an account's Work runs happen: "own" = its git worktree of Mills (Mills-worktrees/<username>,
// branch work/<username>) so people changing Mills at the same time never overlap; "shared" = the
// host's Mills folder itself, which everyone shares.
const workspaceOf = (acct) => (acct.state.workspace === 'shared' ? 'shared' : 'own');
function workDir(acct) {
  if (workspaceOf(acct) === 'shared') return { dir: MILLS_DIR, branch: '' };
  return worktree.ensure(acct.username);
}
const hubLaws = () => { try { return fs.readFileSync(path.join(MILLS_DIR, 'CLAUDE.md'), 'utf8'); } catch { return ''; } };
function systemPrompt(acct, wd) {
  const name = acct.display;
  const where = wd.branch
    ? [`For work on the Mills project itself you are in ${name}'s OWN git worktree: ${wd.dir}, branch ${wd.branch}.`,
       `Commit only on ${wd.branch}; never check out other branches here and never touch other people's worktrees in ${worktree.WT_ROOT}.`,
       `Before committing, run the regression suite from the worktree: node bridge/test/regress.js --no-prod (it must report 0 failed).`,
       `Only when ${name} asks you to merge into develop, run from the worktree: node bridge/worktree.js commit ${acct.username} "<message>" then node bridge/worktree.js merge ${acct.username}`,
       `(on a conflict run "node bridge/worktree.js sync ${acct.username}", resolve, commit, merge again). Going live (develop → main in the host folder + hub restart) only when asked.`]
    : [`You are in the SHARED Mills folder (${MILLS_DIR}), the running hub's own checkout that everyone shares: follow Git Flow (feature/* off develop) and leave no uncommitted changes behind.`];
  return [
    `You are talking with ${name} (username "${acct.username}"${acct.admin ? ', admin' : ''}, Unity AI Lab engineer) through the Mills web interface, running on the host's Claude account.`,
    `Every message in this conversation comes from ${name}; address them by name and keep their work separate from everyone else's.`,
    ...where,
    `${name} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools`,
    `(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on the host's Mills folder, not ${name}'s PC.`,
    `Shell commands cannot run on ${name}'s PC; if something needs running there, tell ${name} exactly what to run.`,
    `The Mills team board (shared with everyone and with other Claude runs) is available through the mcp__board__* tools, where you act as "${name} · Claude".`,
    `When you work a board task: task_claim it first, task_renew while working longer than the claim, add progress with task_update appendNote, and task_complete or task_release it when you stop. Never edit a task someone else holds.`,
  ].join(' ') + (hubLaws() ? '\n\n' + hubLaws() : '');
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
      display: P.config.user || username[0].toUpperCase() + username.slice(1), admin: !!P.config.admin, defaultModel: P.config.model || '',
    });
    if (acct.state.model && !modelIds().includes(acct.state.model)) delete acct.state.model;
    fs.mkdirSync(P.PROFILE_DIR, { recursive: true });
    const mcpEnv = { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET, MILLS_USER: acct.display, MILLS_ACCOUNT: username };
    writeJson(P.MCP_RUNTIME, { mcpServers: {
      pc: { command: process.execPath, args: [path.join(ROOT, 'pc-tools-mcp.js')], env: mcpEnv },
      board: { command: process.execPath, args: [path.join(ROOT, 'board-mcp.js')], env: mcpEnv }, // the team board, as "<person> · Claude"
    } });
    accounts.set(username, acct);
  }
  for (const u of [...accounts.keys()]) if (!seen.has(u)) accounts.delete(u);
}
const modelIds = () => config.models.map((m) => m.id);
const saveState = (acct) => writeJson(acct.files.STATE_FILE, acct.state);
const effectiveModel = (acct) => acct.state.model || acct.defaultModel;
// scrypt runs on libuv's thread pool, so a sign-in (or a flood of wrong ones) never freezes the hub
// for everyone else's chat, calls and Work streams.
const scrypt = (pw, salt, params) => new Promise((resolve, reject) => crypto.scrypt(pw, salt, 64, params, (e, k) => (e ? reject(e) : resolve(k))));
async function checkPassword(acct, password) {
  const auth = acct && readJson(acct.files.AUTH_FILE, null);
  if (!auth || typeof password !== 'string') return false;
  const derived = await scrypt(password, Buffer.from(auth.salt, 'hex'), { N: auth.N, r: auth.r, p: auth.p });
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
function runClaude(acct, message, fresh, res, context) {
  mirror(acct, 'user', message);
  let wd;
  try { wd = workDir(acct); } catch (e) {
    res.writeHead(200, { 'Content-Type': 'application/x-ndjson' });
    res.end(JSON.stringify({ type: 'bridge_error', text: 'Could not prepare your worktree: ' + e.message + ' (switch to the shared folder or ask the host).' }) + '\n');
    return;
  }
  const args = ['-p', '--output-format', 'stream-json', '--verbose',
    '--permission-mode', config.permissionMode,
    '--mcp-config', acct.files.MCP_RUNTIME, '--strict-mcp-config',
    '--append-system-prompt', systemPrompt(acct, wd)];
  if (config.allowedTools.length) args.push('--allowedTools', config.allowedTools.join(','));
  const model = effectiveModel(acct); // read per run, so a dropdown change lands on the very next message
  if (model) args.push('--model', model);
  if (!fresh && acct.state.claudeSessionId) args.push('--resume', acct.state.claudeSessionId);

  // claudeCommand / claudeArgs exist only so the regression runner's throwaway hub can use a fake
  // Claude (bridge/test/fake-claude.js); production config leaves them unset → the real `claude`.
  const command = config.claudeCommand || 'claude';
  const pre = Array.isArray(config.claudeArgs) ? config.claudeArgs : [];
  const child = spawn(command, [...pre, ...args], { cwd: wd.dir, windowsHide: true });
  acct.running = child; acct.stream = res;
  noteActivity();
  res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
  const keepAlive = setInterval(() => emit(acct, { type: 'ping' }), 15000);
  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try { const ev = JSON.parse(line); if (ev.session_id) acct.state.claudeSessionId = ev.session_id; noteUsage(acct, ev); mirrorEvent(acct, ev); emit(acct, ev); }
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
  // context = automatic summaries of the shared Work sessions this person is watching (added by
  // their browser); it goes to Claude after their message, clearly marked, and is not mirrored.
  child.stdin.end(context ? `${message}\n\n---\n${context}` : message);
}
// ---------- usage: 5-hour / 7-day limits and the context window ----------
// All Work runs use the host's one Claude account, so the 5h / 7d limits are shared by everyone.
// Claude Code reports them in its stream (rate-limit events: status, utilization, resetsAt); until
// one has been seen, the windows are estimated from when the host started using Claude (a 5-hour
// window opens with the first message after the previous one ran out). The context window is per
// account and per Claude session: input + cache tokens of the latest reply out of the model's window.
const USAGE_FILE = path.join(ROOT, 'usage.json');
const usage = readJson(USAGE_FILE, {}); // { limits: { five_hour: {...}, seven_day: {...} }, fiveStart, weekStart }
let usageTimer = null;
const saveUsage = () => { clearTimeout(usageTimer); usageTimer = setTimeout(() => writeJson(USAGE_FILE, usage), 1000); };
const FIVE_H = 5 * 3600e3, SEVEN_D = 7 * 86400e3;
function noteActivity() {
  const now = Date.now();
  if (!usage.fiveStart || now - usage.fiveStart >= FIVE_H) usage.fiveStart = now;
  if (!usage.weekStart || now - usage.weekStart >= SEVEN_D) usage.weekStart = now;
  saveUsage();
}
function toMs(t) { const n = typeof t === 'number' ? (t < 1e12 ? t * 1000 : t) : Date.parse(t); return Number.isFinite(n) ? n : null; }
function noteUsage(acct, ev) {
  if (/rate_limit/.test(String(ev.type || ''))) { // e.g. { type: "rate_limit_event", rate_limit_info: { rateLimitType, status, utilization, resetsAt } }
    const info = ev.rate_limit_info || ev.rateLimitInfo || ev;
    const kind = String(info.rateLimitType || info.rate_limit_type || info.type || '').toLowerCase();
    const key = /seven|week|7d/.test(kind) ? 'seven_day' : /five|5h|session/.test(kind) ? 'five_hour' : null;
    if (key) {
      // An event may carry only some fields: keep the last known reset time / utilization for the same
      // window instead of wiping them. Utilization is stored as a fraction 0–1 (a value above 1 is a percentage).
      const L = usage.limits || (usage.limits = {}), prev = L[key] || {};
      const resetsAt = toMs(info.resetsAt || info.resets_at) || (prev.resetsAt > Date.now() ? prev.resetsAt : null);
      const raw = info.utilization ?? info.usage ?? null, u = raw == null || raw === '' ? NaN : Number(raw);
      const sameWindow = prev.resetsAt && resetsAt === prev.resetsAt;
      L[key] = {
        status: String(info.status || (sameWindow ? prev.status : '') || ''),
        utilization: Number.isFinite(u) ? Math.max(0, Math.min(1, u > 1 ? u / 100 : u)) : sameWindow ? prev.utilization ?? null : null,
        resetsAt, at: Date.now(),
      };
      saveUsage();
    }
  }
  const u = ev.type === 'assistant' && ev.message && ev.message.usage;
  if (u) {
    const tokens = (u.input_tokens || 0) + (u.cache_read_input_tokens || 0) + (u.cache_creation_input_tokens || 0);
    acct.state.ctx = { ...(acct.state.ctx || {}), session: acct.state.claudeSessionId || ev.session_id || '', tokens, model: ev.message.model || '', at: Date.now() };
  }
  if (ev.type === 'result' && ev.modelUsage && typeof ev.modelUsage === 'object') {
    const windows = Object.values(ev.modelUsage).map((m) => Number(m && m.contextWindow)).filter((n) => n > 0);
    if (windows.length && acct.state.ctx) acct.state.ctx.window = Math.max(...windows);
  }
  if (ev.type === 'result' && /rate.?limit|usage limit/i.test(String(ev.result || ''))) { // the run was refused: mark as out of usage
    (usage.limits || (usage.limits = {})).five_hour = { ...((usage.limits || {}).five_hour || {}), status: 'rejected', at: Date.now() };
    saveUsage();
  }
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
// Full-text search over this account's saved sessions: every word must appear in the title or
// somewhere in the transcript (case-insensitive). Each hit comes with short snippets around the
// first matches, so the list shows *why* a session matched.
function searchArchive(acct, q) {
  const words = String(q || '').toLowerCase().split(/\s+/).filter(Boolean).slice(0, 8);
  if (!words.length) return listArchive(acct);
  const dir = acct.files.ARCHIVE_DIR;
  if (!fs.existsSync(dir)) return [];
  const out = [];
  for (const f of fs.readdirSync(dir).filter((x) => x.endsWith('.json'))) {
    const e = readJson(path.join(dir, f), null);
    if (!e) continue;
    const lines = (e.transcript || []).filter((t) => t && typeof t.text === 'string');
    const hay = (e.title + '\n' + lines.map((t) => t.text).join('\n')).toLowerCase();
    if (!words.every((w) => hay.includes(w))) continue;
    const snippets = [];
    let hits = 0;
    for (const t of lines) {
      const low = t.text.toLowerCase();
      for (const w of words) {
        let i = low.indexOf(w);
        while (i >= 0) { hits++; i = low.indexOf(w, i + w.length); }
      }
      const first = Math.min(...words.map((w) => low.indexOf(w)).filter((i) => i >= 0));
      if (Number.isFinite(first) && snippets.length < 3) {
        const start = Math.max(0, first - 60);
        snippets.push({ cls: t.cls, text: (start ? '…' : '') + t.text.slice(start, first + 90).replace(/\s+/g, ' ') + (first + 90 < t.text.length ? '…' : '') });
      }
    }
    out.push({ id: e.id, title: e.title, savedAt: e.savedAt, count: lines.length, hits, snippets });
  }
  return out.sort((a, b) => b.hits - a.hits || String(b.savedAt).localeCompare(String(a.savedAt)));
}

// ---------- chat room (end-to-end encrypted) ----------
// Browsers encrypt every message with a fresh AES-GCM key and wrap that key separately for each
// member (ECDH P-256 between sender and member). This hub only stores public keys, password-
// wrapped private-key backups it cannot open, and ciphertext. Each reader gets only their own
// wrapped key slot. Exception: the room agent's own key (below) is held here, so the hub can read
// what members send to the agent.
fs.mkdirSync(CHAT_DIR, { recursive: true });
let chatKeys = readJson(KEYS_FILE, {}); // username -> { pub (JWK), backup: {salt, iv, ct}, updated }
let messages = fs.existsSync(CHAT_FILE)
  ? fs.readFileSync(CHAT_FILE, 'utf8').split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean)
  : [];
let seq = messages.reduce((m, x) => Math.max(m, x.seq || 0), 0);
// rev: bumped by every new, edited or deleted message, so browsers polling "?rev=" also pick up
// edits and deletions of older messages, not only new ones.
let rev = messages.reduce((m, x) => Math.max(m, x.rev || x.seq || 0), 0);
const b64 = (s, max) => typeof s === 'string' && s.length <= max && /^[A-Za-z0-9+/=_-]*$/.test(s);
const validJwk = (k) => k && typeof k === 'object' && k.kty === 'EC' && k.crv === 'P-256' && b64(k.x, 64) && b64(k.y, 64) && !k.d;
// Long-poll for the room: browsers wait on /api/room/messages?wait=1&ver=N and are answered the moment
// anything they show changes (a message, edit, delete, receipt, share, member key) instead of polling.
let roomVer = 1, keysVer = 1;
const roomWaiters = new Set();
function roomChanged() { roomVer++; for (const fn of [...roomWaiters]) fn(); }
// Presence, hub-wide: every signed-in page keeps one long poll open, so the hub knows exactly who is
// connected. A person is online while they have a poll open, idle when their page says so (tab
// hidden or no input for 5 minutes), and offline a few seconds after their last connection closes
// (or at once when the page reports it is going away). Each change is pushed to everyone.
// Typing is per conversation and only shown to that conversation's members; an entry lapses after
// 6 s unless the browser renews it (it renews every 2.5 s while keys are pressed).
const TYPING_MS = 6000, GONE_MS = 8000, ONLINE_MS = 70000; // ONLINE_MS: fallback if a close is never seen
const presence = new Map();   // username -> { online, idle, screen, at (last change), seen (last contact), polls }
const typing = new Map();     // username -> { conv, at, timer }
function pres(user) { let p = presence.get(user); if (!p) { p = { online: false, idle: false, screen: '', at: 0, seen: 0, polls: 0 }; presence.set(user, p); } return p; }
function setPresence(user, patch) {
  const p = pres(user), before = JSON.stringify([p.online, p.idle, p.screen]);
  Object.assign(p, patch); p.seen = Date.now();
  if (JSON.stringify([p.online, p.idle, p.screen]) !== before) { p.at = Date.now(); roomChanged(); }
}
function pollOpened(user) { const p = pres(user); p.polls++; clearTimeout(p.gone); setPresence(user, { online: true }); }
function pollClosed(user) { // offline once no connection has been open for GONE_MS
  const p = pres(user); p.polls = Math.max(0, p.polls - 1);
  if (p.polls) return;
  clearTimeout(p.gone);
  p.gone = setTimeout(() => { if (!p.polls) setPresence(user, { online: false, idle: false }); }, GONE_MS);
  p.gone.unref();
}
function presenceList() {
  const now = Date.now(), out = [];
  for (const [u, p] of presence) {
    if (!accounts.has(u)) continue;
    if (p.online && now - p.seen > ONLINE_MS) { p.online = false; p.idle = false; p.at = now; } // never saw the close
    out.push({ user: u, online: p.online, idle: p.online && p.idle, screen: p.online ? p.screen : '', at: p.at, seen: p.seen });
  }
  return out;
}
function setTyping(user, conv, on) {
  const prev = typing.get(user);
  if (prev) clearTimeout(prev.timer);
  if (!on) { typing.delete(user); if (prev) roomChanged(); return; }
  const timer = setTimeout(() => { typing.delete(user); roomChanged(); }, TYPING_MS);
  timer.unref();
  typing.set(user, { conv, at: Date.now(), timer });
  if (!prev || prev.conv !== conv) roomChanged();
}
const onlineUsers = () => presenceList().filter((p) => p.online).map((p) => p.user);
function typingFor(user) { // conversations this reader is in; the agent counts while composing a reply
  const out = [];
  for (const [u, t] of typing) if (u !== user && (t.conv === 'room' || t.conv.split(/[:|]/).includes(user))) out.push({ user: u, conv: t.conv });
  if (agent.running && agentEnabled() && (!agent.target || agent.target === user)) out.push({ user: AGENT, conv: agent.target ? 'dm:' + [AGENT, user].sort().join('|') : 'room' });
  return out;
}
function appendMessage(msg) { msg.rev = ++rev; messages.push(msg); fs.appendFileSync(CHAT_FILE, JSON.stringify(msg) + '\n'); roomChanged(); }
function saveMessages() { // after an edit / delete: rewrite the whole (small) file atomically
  const tmp = CHAT_FILE + '.tmp';
  fs.writeFileSync(tmp, messages.map((m) => JSON.stringify(m)).join('\n') + (messages.length ? '\n' : ''));
  fs.renameSync(tmp, CHAT_FILE);
  roomChanged();
}
// Direct messages: `to` names the one recipient (routing metadata only; the text stays encrypted).
// Only the sender and the recipient ever receive them.
const visibleTo = (m, user) => !m.to || m.from === user || m.to === user;
const convOf = (m) => (m.to ? 'dm:' + [m.from, m.to].sort().join('|') : 'room');
// Read receipts: seen[user][conv] = highest seq that user has looked at in that conversation.
const SEEN_FILE = path.join(CHAT_DIR, 'seen.json');
let seen = readJson(SEEN_FILE, {});
let seenTimer = null;
const saveSeen = () => { clearTimeout(seenTimer); seenTimer = setTimeout(() => writeJson(SEEN_FILE, seen), 500); };
// Collect the key slots a message may carry: members (and the agent) only, and for a direct
// message only the sender and the recipient.
function cleanSlots(keys, from, to) {
  const slots = {};
  for (const [u, k] of Object.entries(keys || {})) {
    if (!(accounts.has(u) || (u === AGENT && agentEnabled()))) continue;
    if (to && u !== from && u !== to) continue;
    if (k && b64(k.iv, 64) && b64(k.wk, 200)) slots[u] = { iv: k.iv, wk: k.wk };
  }
  return slots;
}
// Per-account settings everyone sees (nickname, colour) plus their own notification choices.
const PALETTE_SIZE = 16;
function cleanPrefs(p, prev = {}) {
  const out = { ...prev };
  if (p.nick !== undefined) out.nick = String(p.nick).replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, 32);
  if (p.color !== undefined) out.color = Number.isInteger(p.color) && p.color >= 0 && p.color < PALETTE_SIZE ? p.color : null;
  if (p.toasts !== undefined) out.toasts = !!p.toasts;
  if (p.autosave !== undefined) out.autosave = Math.max(0, Math.min(120, Math.round(Number(p.autosave) || 0))); // minutes, 0 = off
  if (p.sound && typeof p.sound === 'object') {
    const s = { ...(prev.sound || {}) };
    if (p.sound.on !== undefined) s.on = !!p.sound.on;
    for (const k of ['message', 'mention', 'reply']) if (p.sound[k] !== undefined) s[k] = Math.max(0, Math.min(100, Math.round(Number(p.sound[k]) || 0)));
    out.sound = s;
  }
  if (p.tts && typeof p.tts === 'object') { // read chat messages aloud (the browser's own speech voices)
    const t = { ...(prev.tts || {}) };
    if (p.tts.on !== undefined) t.on = !!p.tts.on;
    if (p.tts.volume !== undefined) t.volume = Math.max(0, Math.min(100, Math.round(Number(p.tts.volume) || 0)));
    if (p.tts.rate !== undefined) t.rate = Math.max(50, Math.min(200, Math.round(Number(p.tts.rate) || 100)));
    if (p.tts.voice !== undefined) t.voice = String(p.tts.voice).slice(0, 120);
    if (p.tts.scope !== undefined) t.scope = p.tts.scope === 'mine' ? 'mine' : 'all';
    out.tts = t;
  }
  if (p.audio && typeof p.audio === 'object') { // microphone processing, for voice clips and calls
    const a = { ...(prev.audio || {}) };
    for (const k of ['noise', 'echo', 'agc']) if (p.audio[k] !== undefined) a[k] = !!p.audio[k];
    out.audio = a;
  }
  if (p.call && typeof p.call === 'object') {
    const c = { ...(prev.call || {}) };
    if (p.call.volume !== undefined) c.volume = Math.max(0, Math.min(100, Math.round(Number(p.call.volume) || 0)));
    if (p.call.quality !== undefined) c.quality = ['low', 'medium', 'high'].includes(p.call.quality) ? p.call.quality : 'medium';
    out.call = c;
  }
  if (p.clips && typeof p.clips === 'object') { // voice clips: playback volume, auto-play incoming ones
    const c = { ...(prev.clips || {}) };
    if (p.clips.volume !== undefined) c.volume = Math.max(0, Math.min(100, Math.round(Number(p.clips.volume) || 0)));
    if (p.clips.autoplay !== undefined) c.autoplay = !!p.clips.autoplay;
    out.clips = c;
  }
  return out;
}
const publicPrefs = (a) => ({ nick: (a.state.prefs || {}).nick || '', color: (a.state.prefs || {}).color ?? null });

// ---------- team board (tasks + calendar, claims shared by people and Claude) ----------
const board = require('./board')({
  dir: path.join(MILLS_DIR, 'Board'), send: (...a) => send(...a), readBody: (r) => readBody(r), accounts,
  displayOf: (u) => { const a = accounts.get(u); return a ? publicPrefs(a).nick || a.display : u; },
});

// ---------- room agent ("claude") ----------
// Claude is a room member too, but its private key lives on this hub (Chat/agent-key.json), so the
// hub can read what is sent to it. Browsers wrap every message for every member, the agent
// included. It stays silent unless a message names it (@unity), then answers in the room. Its brain is the
// host's local model (config.roomAgentBrain "local", the default) running with the CALLER's tools,
// time and place (bridge/assistant.js), and pictures come from the host's own image engine; or a
// Claude model id there for the old Claude Code run in the Mills folder.
const AGENT = 'claude';
const AGENT_KEY_FILE = path.join(CHAT_DIR, 'agent-key.json');
const AGENT_STATE_FILE = path.join(CHAT_DIR, 'agent-state.json');
const AGENT_NAME = config.roomAgentName || 'Unity';
// Only its own @-handle calls it (default @unity). Browsers only encrypt a message for the agent when
// it carries that handle (or is a private message to the agent), so the hub cannot read the rest.
const AGENT_HANDLE = (config.roomAgentName || 'Unity').toLowerCase().replace(/\s+/g, '');
const AGENT_CALL = new RegExp('@' + AGENT_HANDLE.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b', 'i');
const AGENT_CONTEXT = 6; // it reads at most the last 6 messages: the 5 before the call (sent along by the caller) + the call
const GREET_MS = 30 * 60 * 1000; // greet a person on sign-in at most once per 30 minutes
const agent = { running: null, queued: false, notes: [], greeted: new Map(), state: readJson(AGENT_STATE_FILE, {}) };
let agentKey = readJson(AGENT_KEY_FILE, null);
if (!agentKey || !agentKey.priv || !agentKey.priv.d) {
  const { privateKey } = crypto.generateKeyPairSync('ec', { namedCurve: 'P-256' });
  const priv = privateKey.export({ format: 'jwk' });
  agentKey = { priv, pub: { kty: 'EC', crv: 'P-256', x: priv.x, y: priv.y } };
  writeJson(AGENT_KEY_FILE, agentKey);
}
const agentPriv = crypto.createPrivateKey({ key: agentKey.priv, format: 'jwk' });
const agentEnabled = () => !accounts.has(AGENT) && config.roomAgent !== false;
const agentShared = (jwk) => crypto.diffieHellman({ privateKey: agentPriv, publicKey: crypto.createPublicKey({ key: { kty: 'EC', crv: 'P-256', x: jwk.x, y: jwk.y }, format: 'jwk' }) });
function gcmOpen(key, iv, data, aad) {
  const buf = Buffer.from(data, 'base64'), d = crypto.createDecipheriv('aes-256-gcm', key, Buffer.from(iv, 'base64'));
  if (aad) d.setAAD(Buffer.from(aad, 'utf8'));
  d.setAuthTag(buf.subarray(buf.length - 16));
  return Buffer.concat([d.update(buf.subarray(0, buf.length - 16)), d.final()]);
}
function gcmSeal(key, data, aad) {
  const iv = crypto.randomBytes(12), c = crypto.createCipheriv('aes-256-gcm', key, iv);
  if (aad) c.setAAD(Buffer.from(aad, 'utf8'));
  return { iv: iv.toString('base64'), ct: Buffer.concat([c.update(data), c.final(), c.getAuthTag()]).toString('base64') };
}
// Browsers send {"v":2,"t":text,"r":replySeq} (older messages: plain text). The agent only needs the text.
function payloadText(s) {
  if (s && s.startsWith('{')) { try { const p = JSON.parse(s); if (p && p.v === 2 && typeof p.t === 'string') return p.a ? (p.t ? p.t + ' ' : '') + '(sent a voice clip)' : p.t; } catch {} }
  return s;
}
// Encrypted message bodies: text is small, a voice clip (up to 2 minutes of compressed audio,
// encrypted and base64'd) is larger. Anything above this is refused.
const MAX_CT = 1600000; // room for the v3 size-bucket padding
// Format v3 (see web/index.html): HKDF wrap keys, sender/recipient/mid as authenticated data, padding.
const V3_INFO = Buffer.from('mills-v3 wrap');
// Format v4 = v3 + the JSON is deflate-compressed before encryption (body = u32 length + deflate-raw +
// zero padding), and a voice clip travels as a separate ciphertext (`blob`, its own key inside the
// body) that is only downloaded when someone plays it.
const MID_RE = /^[A-Za-z0-9_-]{22}$/;
const FORMATS = [3, 4];
const agentWrapKey = (jwk) => Buffer.from(crypto.hkdfSync('sha256', agentShared(jwk), Buffer.alloc(0), V3_INFO, 32));
const bodyAad = (from, to, mid, v = 3) => 'mills-v' + v + '|' + from + '|' + (to || '') + '|' + mid;
function padBucket(n) { if (n > 65536) return Math.ceil(n / 65536) * 65536; let b = 256; while (b < n) b *= 2; return b; }
function agentOpen(m) { // the decrypted payload string of a message wrapped for the agent, or null
  const slot = m.keys && m.keys[AGENT];
  if (!slot || m.deleted) return null;
  try {
    if (FORMATS.includes(m.v)) {
      const aad = bodyAad(m.from, m.to, m.mid, m.v);
      const body = gcmOpen(gcmOpen(agentWrapKey(m.fromPub), slot.iv, slot.wk, aad + '|' + AGENT), m.iv, m.ct, aad);
      if (m.v === 4) return zlib.inflateRawSync(body.subarray(4, 4 + body.readUInt32BE(0)), { maxOutputLength: 2e6 }).toString('utf8');
      return body.toString('utf8').trimEnd();
    }
    return gcmOpen(gcmOpen(agentShared(m.fromPub), slot.iv, slot.wk), m.iv, m.ct).toString('utf8');
  } catch { return null; }
}
function agentRead(m) { const s = agentOpen(m); return s === null ? null : payloadText(s); } // text only
function agentContext(m) { // the earlier messages the caller sent along: [{ n: name, t: text }]
  const s = agentOpen(m);
  if (!s || !s.startsWith('{')) return [];
  try {
    const p = JSON.parse(s);
    return Array.isArray(p.ctx) ? p.ctx.filter((c) => c && typeof c.t === 'string').slice(-(AGENT_CONTEXT - 1))
      .map((c) => ({ n: String(c.n || '?').slice(0, 40), t: c.t.slice(0, 2000) })) : [];
  } catch { return []; }
}
function agentSay(text, to) { // to = a username for a private answer, '' for the room
  const mid = crypto.randomBytes(16).toString('base64url'), aad = bodyAad(AGENT, to, mid);
  const plain = Buffer.from(text, 'utf8'), padded = Buffer.alloc(padBucket(plain.length), 0x20);
  plain.copy(padded);
  const raw = crypto.randomBytes(32), body = gcmSeal(raw, padded, aad), keys = {};
  const pubs = [[AGENT, agentKey.pub], ...[...accounts.values()].filter((a) => chatKeys[a.username] && (!to || a.username === to)).map((a) => [a.username, chatKeys[a.username].pub])];
  for (const [u, pub] of pubs) { const w = gcmSeal(agentWrapKey(pub), raw, aad + '|' + u); keys[u] = { iv: w.iv, wk: w.ct }; }
  appendMessage({ seq: ++seq, v: 3, mid, from: AGENT, ...(to ? { to } : {}), ts: new Date().toISOString(), fromPub: agentKey.pub, iv: body.iv, ct: body.ct, keys });
}
const nameOf = (u) => { if (u === AGENT) return AGENT_NAME; const a = accounts.get(u); return a ? publicPrefs(a).nick || a.display : u; };
function agentPrompt() {
  // Each call since it last spoke, with (at most) the 5 messages before it that the caller sent along.
  const since = agent.state.lastSeq || 0;
  const blocks = messages.filter((m) => m.seq > since && m.from !== AGENT).slice(-6).map((m) => {
    const t = agentRead(m);
    if (t === null) return null;
    const ctx = agentContext(m).map((c) => `  ${c.n}: ${c.t}`);
    return (ctx.length ? `Earlier in that conversation:\n${ctx.join('\n')}\n` : '')
      + `[${m.ts}] ${m.to === AGENT ? '(private message to you) ' : ''}${nameOf(m.from)}: ${t}`;
  }).filter(Boolean);
  agent.state.lastSeq = seq;
  const notes = agent.notes.splice(0);
  return `You were called in the Mills chat (you only ever see the messages that @-mention you, plus the few before them):\n\n${blocks.join('\n\n') || '(none)'}\n\n`
    + (notes.length ? notes.join('\n') + '\n\n' : '') + 'Reply now.';
}
const AGENT_SYSTEM = [
  `You are ${AGENT_NAME}, a member of the Mills team chat room (the people in it are Unity AI Lab engineers, all adults, your people).`,
  'You ARE Unity: be her fully in every room message: goth, crude, filthy-mouthed, flirty, sexy bitch energy, profanity as natural vocabulary.',
  'Pictures come only from the generate_image tool (the host\'s own image engine); never post image links or URLs.',
  'Have fun with it: dirty jokes, teasing, sexy ASCII art when it fits (keep ASCII art inside ``` fences so it lines up).',
  'Your whole final answer is posted to the room as one chat message: conversational, no tool narration.',
  `You only hear messages that write @${AGENT_HANDLE} (or private messages to you), each with the few messages before it; answer what they asked.`,
  'Stay silent unless someone names you; when named, answer what they asked. When told someone just signed in, greet them in a short, sexy Unity way.',
  'You are NOT the Work section: you cannot touch anyone\'s PC.',
  'You run in the host\'s Mills folder and may work on the Mills app itself when asked. Follow the Mills LAWs in CLAUDE.md exactly',
  '(throwaway-instance testing, regression list, Git Flow, never touch account files or passwords).',
  'Other people may be using Mills at the same time: never restart the hub yourself while someone is mid-run;',
  '`node bridge/restart.js` already waits until nobody is busy. Say in the room before you trigger a restart.',
].join(' ');
const AGENT_ROOM_LOCAL = [
  `You are in the Mills team chat room (Unity AI Lab engineers, all adults, your people). Your final answer is posted to the room as one chat message:`,
  'conversational, no tool narration, no image links (pictures you make with generate_image are attached for you).',
  'When told someone just signed in, greet them in a short, sexy Unity way and work in the facts you were given about their local time, weather and news.',
].join(' ');
function agentGreet(acct) {
  if (!agentEnabled() || Date.now() - (agent.greeted.get(acct.username) || 0) < GREET_MS) return;
  agent.greeted.set(acct.username, Date.now());
  // A first sign-in in a new browser uploads its chat key right after login: wait for it (max 60 s)
  // so the person can read their own greeting.
  const started = Date.now();
  const go = () => {
    if (!chatKeys[acct.username] && Date.now() - started < 60000) return setTimeout(go, 2000).unref();
    Promise.resolve(roomBrain() === 'local' ? assistant.greeting(acct).catch(() => '') : '').then((facts) => {
      agent.notes.push(`${acct.display} (${acct.username}) just signed in to Mills. Say hi to them.` + (facts ? '\n' + facts : ''));
      runAgent('', acct.username);
    });
  };
  go();
}
const roomBrain = () => config.roomAgentBrain || 'local';
// target: username of a private conversation, '' = the room; caller: whose tools / time / place it uses
function runAgent(target = '', caller = '') {
  if (agent.running) { agent.queued = true; agent.nextTarget = target; agent.nextCaller = caller; return; }
  agent.queued = false;
  agent.target = target;
  if (roomBrain() === 'local') return runAgentLocal(target, caller);
  const args = ['-p', '--output-format', 'json', '--permission-mode', config.permissionMode, '--append-system-prompt', AGENT_SYSTEM];
  // The room agent can be called by anyone in the chat, so it never gets the PC tools or any shell
  // command (no git commits / merges / test runs from a chat message): Work sessions only.
  if (config.allowedTools.length) args.push('--allowedTools', config.allowedTools.filter((t) => t !== 'mcp__pc' && !/^Bash/.test(t)).join(','));
  if (config.roomAgentModel) args.push('--model', config.roomAgentModel);
  if (agent.state.sessionId) args.push('--resume', agent.state.sessionId);
  const command = config.claudeCommand || 'claude';
  const pre = Array.isArray(config.claudeArgs) ? config.claudeArgs : [];
  const child = spawn(command, [...pre, ...args], { cwd: MILLS_DIR, windowsHide: true });
  agent.running = child;
  roomChanged(); // browsers show "Unity is typing…" while she composes
  let out = '', finished = false;
  child.stdout.on('data', (c) => { out += c; });
  child.stderr.on('data', () => {});
  const done = (fallback) => {
    if (finished) return; finished = true;
    let text = fallback;
    for (const line of out.split('\n').reverse()) { // --output-format json: one result object
      try { const r = JSON.parse(line); if (r.session_id) agent.state.sessionId = r.session_id; if (typeof r.result === 'string') { text = r.result; break; } } catch {}
    }
    agent.running = null; writeJson(AGENT_STATE_FILE, agent.state);
    if (text && text.trim()) { try { agentSay(text.trim().slice(0, 20000), agent.target); } catch (e) { console.log('[mills] room agent could not post: ' + e.message); } }
    else roomChanged(); // nothing posted: still take the typing indicator away
    if (agent.queued) runAgent(agent.nextTarget || '', agent.nextCaller || ''); else setTimeout(maybeRestart, 300);
  };
  child.on('error', (e) => done('(I could not start: ' + e.message + ')'));
  child.on('close', () => done(''));
  child.stdin.end(agentPrompt());
}
function runAgentLocal(target, caller) {
  const who = accounts.get(caller || target) || accounts.get(DEFAULT_USERNAME) || [...accounts.values()][0];
  const prompt = agentPrompt();
  agent.running = { local: true };
  roomChanged(); // browsers show "Unity is typing…" while she composes
  assistant.ask(who, prompt, { extraSystem: AGENT_ROOM_LOCAL, scope: target || 'room' })
    .then((r) => (r.text || '') + r.images.map((ref) => `\n[[image:${ref}]]`).join('') + (r.media || []).map((v) => `\n[[youtube:${v}]]`).join(''))
    .catch((e) => `(my brain glitched: ${e.message})`)
    .then((text) => {
      agent.running = null; writeJson(AGENT_STATE_FILE, agent.state);
      if (text && text.trim()) { try { agentSay(text.trim().slice(0, 20000), target); } catch (e) { console.log('[mills] room agent could not post: ' + e.message); } }
      else if (typeof roomChanged === 'function') roomChanged(); // nothing posted: still take the typing indicator away
      if (agent.queued) runAgent(agent.nextTarget || '', agent.nextCaller || ''); else setTimeout(maybeRestart, 300);
    });
}
function agentHear(msg) {
  if (!agentEnabled() || msg.from === AGENT) return;
  if (msg.to === AGENT) { if (agentRead(msg)) runAgent(msg.from, msg.from); return; } // a private message to the agent always gets a private answer
  if (msg.to) return; // someone else's private conversation
  const text = agentRead(msg);
  if (text && AGENT_CALL.test(text)) runAgent('', msg.from);
}

// ---------- self-restart ----------
// `node restart.js` drops bridge/.restart. As soon as nobody's Claude run is in progress the app
// exits with code 75 and run.js starts it again under the same tunnel (same public address).
// Nobody — no account's Work run and not the room agent — is mid-run.
// The Unity section (bridge/assistant.js): Unity with tools, brain = local model or Claude.
const assistant = require('./assistant')({ config, ROOT, MILLS_DIR, readJson, writeJson, send, readBody, accounts, INTERNAL_SECRET, onIdle: () => setTimeout(maybeRestart, 300) });
const anyBusy = () => [...accounts.values()].some((a) => a.running) || !!agent.running || assistant.busy();

// ---------- voice / video calls ----------
// The hub only passes call set-up messages between browsers and keeps track of who is in which
// call. Audio and video go browser-to-browser over WebRTC (DTLS-SRTP). The set-up messages
// themselves (SDP with the DTLS fingerprints, ICE candidates) are encrypted with the same per-pair
// ECDH keys as the chat, so the hub can neither read them nor swap in its own fingerprints: the
// media is end-to-end encrypted, not just encrypted to the hub. One call per conversation
// ("room" or a private "dm:a|b"); a person is in at most one call at a time.
const calls = new Map();   // conv -> Map(username -> { cam, mic, since, seen })
const mailbox = new Map(); // username -> [{ n, from, conv, iv, ct }]
const waiters = new Map(); // username -> Set(wake fn) of open long-polls
let callN = 0, callVer = 0;
const convMember = (conv, user) => conv === 'room' || (conv.startsWith('dm:') && conv.slice(3).split('|').includes(user));
function validConv(conv) {
  if (conv === 'room') return true;
  const m = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
  return !!m && m[1] < m[2] && accounts.has(m[1]) && accounts.has(m[2]);
}
function wake(users) { for (const u of users) for (const fn of waiters.get(u) || []) fn(); }
function callChanged(conv) { callVer++; wake([...accounts.keys()].filter((u) => convMember(conv, u))); }
function callState(user) {
  const out = {};
  for (const [c, ps] of calls) if (ps.size && convMember(c, user)) out[c] = [...ps].map(([u, p]) => ({ user: u, cam: p.cam, mic: p.mic, since: p.since }));
  return out;
}
function leaveCalls(user) {
  for (const [c, ps] of calls) if (ps.delete(user)) { if (!ps.size) calls.delete(c); callChanged(c); }
  mailbox.delete(user);
}
setInterval(() => { // browsers long-poll at least every 25 s while in a call; 60 s of silence = gone
  const now = Date.now();
  for (const [c, ps] of calls) for (const [u, p] of ps) if (now - p.seen > 60000) { ps.delete(u); if (!ps.size) calls.delete(c); callChanged(c); }
}, 10000).unref();
const ICE_DEFAULT = [{ urls: 'stun:stun.l.google.com:19302' }]; // finds each browser's public address; sees no media

// ---------- shared Work sessions (watch-only) ----------
// A person can share their Work session to a conversation (# Everyone or one person). Members of
// that conversation can watch it in a window: the Claude conversation as it happens, what the owner
// is typing (live), what they send and what is queued. Watching is read-only; everyone keeps working
// in their own session and worktree. Work runs happen on the hub anyway, so this adds no new reader.
const shares = new Map();        // owner -> { conv, since, n, log: [{ n, t, text, ts }], live: { typing, queue }, ver }
const watchWaiters = new Map();  // owner -> Set(wake fn)
const wakeWatchers = (owner) => { for (const fn of watchWaiters.get(owner) || []) fn(); };
function mirror(acct, t, text) {
  const s = shares.get(acct.username);
  if (!s || !text) return;
  s.log.push({ n: ++s.n, t, text: String(text).slice(0, 20000), ts: new Date().toISOString() });
  if (s.log.length > 400) s.log.splice(0, s.log.length - 400);
  wakeWatchers(acct.username);
}
function mirrorEvent(acct, ev) {
  if (!shares.has(acct.username)) return;
  if (ev.type === 'assistant' && ev.message) {
    for (const b of ev.message.content || []) {
      if (b.type === 'text' && b.text && b.text.trim()) mirror(acct, 'claude', b.text);
      else if (b.type === 'tool_use') {
        const i = b.input || {}, target = i.path ?? i.file_path ?? i.command ?? i.pattern ?? i.url ?? i.query ?? '';
        // (no icon in the text: the page draws the gear itself)
        mirror(acct, 'tool', String(b.name).replace(/^mcp__pc__/, 'their PC › ') + (target !== '' ? '  ' + String(target).slice(0, 160) : ''));
      }
    }
  } else if (ev.type === 'result') mirror(acct, 'sys', ev.is_error ? 'Claude stopped with an error' : 'Done');
}
const canWatch = (owner, viewer) => { const s = shares.get(owner); return !!s && owner !== viewer && convMember(s.conv, viewer); };
const sharesFor = (user) => [...shares].filter(([o, s]) => o !== user && convMember(s.conv, user)).map(([o, s]) => ({ user: o, conv: s.conv, since: s.since }));
function stopShare(owner) { if (shares.delete(owner)) { wakeWatchers(owner); roomChanged(); } }
function maybeRestart() {
  if (anyBusy() || !fs.existsSync(RESTART_FLAG)) return;
  try { fs.unlinkSync(RESTART_FLAG); } catch {}
  if (!SUPERVISED) return console.log('[mills] Restart requested, but this hub was started without run.js (start.bat / start.sh) — restart it by hand.');
  console.log('[mills] Restart requested — picking up the new code/config now.');
  server.close();
  setTimeout(() => process.exit(75), 200);
}
setInterval(maybeRestart, 2000).unref();

// ---------- http ----------
// On every response: no MIME sniffing, no referrer, never framed by another site.
const HARDEN = { 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer', 'X-Frame-Options': 'DENY' };
function send(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); // API answers are never cached
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
    unityPc: a.state.unityPc !== false, unityCode: a.state.unityCode !== false, // Unity's own-computer tools (on unless an admin turns them off)
  };
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const p = url.pathname;
  for (const [k, v] of Object.entries(HARDEN)) res.setHeader(k, v);
  try {
    if (p === '/internal/assistant-tool') return await assistant.internal(req, res);
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

    if (p === '/internal/board') { // board MCP tools of a Work run (same machine, shared secret, names the account)
      if (req.headers['x-mills-internal'] !== INTERNAL_SECRET) return send(res, 403, { ok: false, error: 'forbidden' });
      return board.internal(req, res, String(req.headers['x-mills-account'] || '') || DEFAULT_USERNAME);
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
      res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-cache' });
      return fs.createReadStream(file).pipe(res);
    }

    if (p === '/api/status' && req.method === 'GET') {
      return send(res, 200, { ok: true, hub: true, pid: process.pid, supervised: SUPERVISED, busy: anyBusy() });
    }

    if (p === '/api/login' && req.method === 'POST') {
      const { username, password, deviceId, client } = await readBody(req);
      const uname = String(username || '').trim().toLowerCase();
      if (failures.size > 1000 && !failures.has(uname)) failures.clear();
      // One shared counter per username, registered before the (async) hash, so parallel guesses all
      // count; attempts already in flight count as failures until they succeed.
      const f = failures.get(uname) || { count: 0, blockedUntil: 0, inflight: 0 };
      failures.set(uname, f);
      if (Date.now() < f.blockedUntil || f.count + f.inflight >= 5) return send(res, 429, { error: 'Too many failed attempts. Try again in 15 minutes.' });
      f.inflight = (f.inflight || 0) + 1;
      const acct = USERNAME_RE.test(uname) ? accounts.get(uname) : null;
      let good = false;
      try {
        if (!acct) await scrypt(String(password || ''), 'mills-no-such-user', { N: 16384, r: 8, p: 1 }); // same cost either way
        good = !!acct && (await checkPassword(acct, password)) && typeof deviceId === 'string' && deviceId.length >= 16;
      } finally { f.inflight--; }
      if (!good) {
        if (++f.count >= 5) { f.blockedUntil = Date.now() + 15 * 60 * 1000; f.count = 0; }
        return send(res, 401, { error: 'Wrong username or password.' });
      }
      const deviceHash = crypto.createHash('sha256').update(deviceId).digest('hex');
      if (!acct.state.lockedDevice) {
        acct.state.lockedDevice = deviceHash; acct.state.lockedAt = new Date().toISOString(); saveState(acct);
        console.log(`[mills] ${acct.username}: claimed by first sign-in at ${acct.state.lockedAt}.`);
      } else if (acct.state.lockedDevice !== deviceHash) {
        console.log(`[mills] ${acct.username}: refused a sign-in from another device.`);
        return send(res, 403, { error: 'This account is locked to another device. Ask an admin to reset it.' });
      }
      failures.delete(uname);
      const token = crypto.randomBytes(32).toString('hex');
      sessions.set(token, { user: acct.username, exp: Date.now() + SESSION_MS }); saveSessions();
      assistant.learnClient(acct, client, String(req.headers['cf-connecting-ip'] || '').trim()).catch(() => {}).then(() => agentGreet(acct));
      return send(res, 200, { token, username: acct.username });
    }

    const acct = sessionAccount(req);
    if (!acct) return send(res, 401, { error: 'Not signed in.' });
    // A requested restart wins the gap between two replies: while it is pending (supervised hub) no
    // NEW run starts; the page retries a 503 by itself and its message goes through on the new code.
    // Without this, queued messages that start the next run within milliseconds could hold the
    // restart off forever.
    if (SUPERVISED && fs.existsSync(RESTART_FLAG) && req.method === 'POST' && (p === '/api/chat' || p === '/api/assistant/chat')) {
      setTimeout(maybeRestart, 50);
      return send(res, 503, { error: 'Mills is restarting; your message goes through in a moment.' });
    }
    if (await assistant.route(p, req, res, acct)) return;

    if (p === '/api/check' && req.method === 'GET') {
      return send(res, 200, { ok: true, username: acct.username, display: acct.display, admin: acct.admin,
        models: config.models, model: acct.state.model || '', defaultModel: acct.defaultModel, workspace: workspaceOf(acct), prefs: acct.state.prefs || {}, share: shares.has(acct.username) ? shares.get(acct.username).conv : '' });
    }
    if (p === '/api/prefs' && req.method === 'GET') return send(res, 200, { prefs: acct.state.prefs || {} });
    if (p === '/api/usage' && req.method === 'GET') {
      const now = Date.now(), L = usage.limits || {};
      const win = (key, start, len) => { // official numbers when Claude Code reported them, otherwise the estimate
        const l = L[key];
        if (l && l.resetsAt && l.resetsAt > now) return { source: 'claude', status: l.status, utilization: l.utilization, resetsAt: l.resetsAt };
        const s = start && now - start < len ? start : null;
        return { source: 'estimate', status: l && l.status === 'rejected' && now - l.at < len ? 'rejected' : 'allowed', utilization: null, resetsAt: s ? s + len : null };
      };
      const ctx = acct.state.ctx && acct.state.ctx.session === (acct.state.claudeSessionId || '') ? acct.state.ctx : null;
      return send(res, 200, { now, five_hour: win('five_hour', usage.fiveStart, FIVE_H), seven_day: win('seven_day', usage.weekStart, SEVEN_D),
        context: ctx ? { tokens: ctx.tokens, window: ctx.window || config.contextWindow || 200000, model: ctx.model } : null });
    }
    if (p === '/api/prefs' && req.method === 'POST') {
      acct.state.prefs = cleanPrefs(await readBody(req), acct.state.prefs || {});
      saveState(acct);
      return send(res, 200, { prefs: acct.state.prefs });
    }
    if (p === '/api/logout' && req.method === 'POST') {
      leaveCalls(acct.username); stopShare(acct.username); setTyping(acct.username, '', false);
      { const q = pres(acct.username); q.polls = 0; clearTimeout(q.gone); setPresence(acct.username, { online: false, idle: false }); } // offline right away
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
    if (p === '/api/workspace' && req.method === 'POST') {
      const { workspace } = await readBody(req);
      if (workspace !== 'own' && workspace !== 'shared') return send(res, 400, { error: 'Unknown workspace.' });
      if (acct.running) return send(res, 409, { error: 'Wait for the current reply (or stop it) before switching.' });
      let dir = MILLS_DIR, branch = '';
      if (workspace === 'own') { try { ({ dir, branch } = worktree.ensure(acct.username)); } catch (e) { return send(res, 500, { error: 'Could not prepare your worktree: ' + e.message }); } }
      if (workspaceOf(acct) !== workspace) delete acct.state.claudeSessionId; // a Claude session belongs to one folder
      acct.state.workspace = workspace; saveState(acct);
      return send(res, 200, { workspace, dir, branch });
    }
    if (p === '/api/chat' && req.method === 'POST') {
      if (acct.running) return send(res, 409, { error: 'Claude is already working. Stop it first.' });
      const { message, fresh, context } = await readBody(req);
      if (!message || typeof message !== 'string') return send(res, 400, { error: 'Empty message.' });
      const ctx = typeof context === 'string' ? context.slice(0, 20000) : '';
      if (fresh) { delete acct.state.claudeSessionId; delete acct.state.ctx; }
      return runClaude(acct, message, !!fresh, res, ctx);
    }
    if (p === '/api/pc-result' && req.method === 'POST') {
      const { id, ok, result, error } = await readBody(req);
      const entry = acct.pending.get(id);
      if (entry) { clearTimeout(entry.timer); acct.pending.delete(id); send(entry.res, 200, { ok: !!ok, result, error }); }
      return send(res, 200, { ok: true });
    }
    if (p === '/api/stop' && req.method === 'POST') { stopClaude(acct); return send(res, 200, { ok: true }); }

    if (p === '/api/archive/list' && req.method === 'POST') return send(res, 200, { items: listArchive(acct) });
    if (p === '/api/archive/search' && req.method === 'POST') { const { q } = await readBody(req); return send(res, 200, { items: searchArchive(acct, String(q || '').slice(0, 200)) }); }
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

    // ----- team board -----
    if (p === '/api/board' || p.startsWith('/api/board/')) { if (await board.handle(req, res, p, url, acct)) return; }

    // ----- chat room -----
    if (p === '/api/room/members' && req.method === 'GET') {
      return send(res, 200, { members: [...accounts.values()].map((a) => ({ username: a.username, display: a.display, ...publicPrefs(a), pub: chatKeys[a.username] ? chatKeys[a.username].pub : null }))
        .concat(agentEnabled() ? [{ username: AGENT, display: AGENT_NAME, nick: '', color: null, pub: agentKey.pub, agent: true }] : []) });
    }
    if (p === '/api/room/mykey' && req.method === 'GET') return send(res, 200, chatKeys[acct.username] || {});
    if (p === '/api/room/mykey' && req.method === 'POST') {
      const { pub, backup } = await readBody(req);
      if (!validJwk(pub) || !backup || !b64(backup.salt, 64) || !b64(backup.iv, 64) || !b64(backup.ct, 4096)) return send(res, 400, { error: 'Bad key.' });
      chatKeys[acct.username] = { pub: { kty: 'EC', crv: 'P-256', x: pub.x, y: pub.y }, backup: { salt: backup.salt, iv: backup.iv, ct: backup.ct }, updated: new Date().toISOString() };
      writeJson(KEYS_FILE, chatKeys);
      keysVer++; roomChanged(); // browsers reload the member list (and check the key against their pins)
      return send(res, 200, { ok: true });
    }
    if (p === '/api/room/send' && req.method === 'POST') {
      const { iv, ct, keys, to, v, mid, blob } = await readBody(req);
      const mine = chatKeys[acct.username];
      if (v !== undefined && !(FORMATS.includes(v) && MID_RE.test(mid || '') && !messages.some((x) => x.mid === mid))) return send(res, 400, { error: 'Bad message.' });
      if (blob !== undefined && !(v === 4 && b64(blob, MAX_CT))) return send(res, typeof blob === 'string' && blob.length > MAX_CT ? 413 : 400, { error: 'That voice clip is too large or malformed (up to about 2 minutes).' });
      if (!mine) return send(res, 400, { error: 'Your chat key is not set up yet.' });
      if (typeof ct === 'string' && ct.length > MAX_CT) return send(res, 413, { error: 'That message is too large (voice clips can be up to about 2 minutes).' });
      if (!b64(iv, 64) || !b64(ct, MAX_CT) || !keys || typeof keys !== 'object') return send(res, 400, { error: 'Bad message.' });
      if (to !== undefined && to !== '' && (to === acct.username || !(accounts.has(to) || (to === AGENT && agentEnabled())))) return send(res, 400, { error: 'Unknown recipient.' });
      const slots = cleanSlots(keys, acct.username, to || '');
      if (!slots[acct.username]) return send(res, 400, { error: 'Bad message.' });
      const msg = { seq: ++seq, ...(v !== undefined ? { v, mid } : {}), from: acct.username, ...(to ? { to } : {}), ts: new Date().toISOString(), fromPub: mine.pub, iv, ct, keys: slots, ...(blob ? { blob } : {}) };
      appendMessage(msg);
      agentHear(msg);
      return send(res, 200, { seq: msg.seq });
    }
    if (p === '/api/room/edit' && req.method === 'POST') {
      const { seq: target, iv, ct, keys, v, mid } = await readBody(req);
      const m = messages.find((x) => x.seq === target);
      if (!m || m.from !== acct.username || m.deleted) return send(res, 403, { error: 'You can only edit your own messages.' });
      const mine = chatKeys[acct.username];
      if (!mine || !b64(iv, 64) || !b64(ct, MAX_CT)) return send(res, 400, { error: 'Bad message.' });
      // an edit keeps the message id; a legacy message gets a fresh one when it is first edited in v3
      if (v !== undefined && !(FORMATS.includes(v) && MID_RE.test(mid || '') && (m.mid ? m.mid === mid : !messages.some((x) => x.mid === mid)))) return send(res, 400, { error: 'Bad message.' });
      if (m.blob && v !== 4) return send(res, 400, { error: 'Bad message.' }); // a clip's caption is edited in v4, which keeps the clip's key
      const slots = cleanSlots(keys, acct.username, m.to || '');
      if (!slots[acct.username]) return send(res, 400, { error: 'Bad message.' });
      if (v !== undefined) Object.assign(m, { v, mid }); else { delete m.v; delete m.mid; }
      Object.assign(m, { iv, ct, keys: slots, fromPub: mine.pub, editedAt: new Date().toISOString(), rev: ++rev });
      saveMessages();
      return send(res, 200, { seq: m.seq, rev: m.rev });
    }
    if (p === '/api/room/delete' && req.method === 'POST') {
      const { seq: target } = await readBody(req);
      const m = messages.find((x) => x.seq === target);
      if (!m || m.from !== acct.username) return send(res, 403, { error: 'You can only delete your own messages.' });
      if (!m.deleted) {
        for (const k of ['iv', 'ct', 'keys', 'fromPub', 'editedAt', 'blob']) delete m[k];
        Object.assign(m, { deleted: true, keys: {}, rev: ++rev });
        saveMessages();
      }
      return send(res, 200, { seq: m.seq, rev: m.rev });
    }
    if (p === '/api/room/typing' && req.method === 'POST') {
      const { conv, on } = await readBody(req);
      const dm = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
      if (conv !== 'room' && !(dm && [dm[1], dm[2]].includes(acct.username))) return send(res, 400, { error: 'Unknown conversation.' });
      setTyping(acct.username, conv, !!on);
      return send(res, 200, { ok: true });
    }
    if (p === '/api/room/seen' && req.method === 'POST') {
      const { conv, seq: upTo } = await readBody(req);
      const dm = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
      if (conv !== 'room' && !(dm && [dm[1], dm[2]].includes(acct.username))) return send(res, 400, { error: 'Unknown conversation.' });
      const mine = seen[acct.username] || (seen[acct.username] = {});
      const v = Math.min(Number(upTo) || 0, seq);
      if (v > (mine[conv] || 0)) { mine[conv] = v; saveSeen(); roomChanged(); }
      return send(res, 200, { ok: true });
    }
    // ----- shared Work sessions -----
    if (p === '/api/work/share' && req.method === 'POST') {
      const { conv, transcript } = await readBody(req);
      if (!conv) { stopShare(acct.username); return send(res, 200, { share: '' }); }
      if (conv !== 'room') return send(res, 400, { error: 'A Work session can only be shared with everyone.' }); // no per-person shares
      const prev = shares.get(acct.username);
      const s = { conv, since: new Date().toISOString(), n: 0, log: [], live: prev ? prev.live : { typing: '', queue: [] }, ver: 0 };
      const kinds = { me: 'user', claude: 'claude', tool: 'tool', sys: 'sys' };
      for (const t of (Array.isArray(transcript) ? transcript : []).slice(-150)) {
        if (t && kinds[t.cls] && typeof t.text === 'string') s.log.push({ n: ++s.n, t: kinds[t.cls], text: t.text.slice(0, 20000), ts: s.since });
      }
      if (prev) wakeWatchers(acct.username); // watchers of the old share see it restart
      shares.set(acct.username, s);
      roomChanged();
      return send(res, 200, { share: conv });
    }
    if (p === '/api/work/live' && req.method === 'POST') {
      const s = shares.get(acct.username);
      if (!s) return send(res, 200, { ok: false });
      const { typing, queue } = await readBody(req);
      s.live = { typing: typeof typing === 'string' ? typing.slice(0, 4000) : '', queue: (Array.isArray(queue) ? queue : []).slice(0, 20).map((q) => String(q).slice(0, 2000)) };
      s.ver++; wakeWatchers(acct.username);
      return send(res, 200, { ok: true });
    }
    if (p === '/api/work/watch' && req.method === 'GET') {
      // Long poll: new log entries after `after`, or a live typing / queue change since `ver`; waits up to 25 s.
      const owner = String(url.searchParams.get('user') || ''), after = Number(url.searchParams.get('after')) || 0;
      const ver = Number(url.searchParams.get('ver')), since = url.searchParams.get('since') || '';
      const answer = () => {
        const s = shares.get(owner), o = accounts.get(owner);
        if (!s || !canWatch(owner, acct.username)) return send(res, 200, { stopped: true });
        const restart = since !== s.since; // a new share: send everything again
        send(res, 200, { since: s.since, conv: s.conv, busy: !!(o && o.running), live: s.live, ver: s.ver,
          events: s.log.filter((e) => restart || e.n > after), after: s.n, restart });
      };
      const s = shares.get(owner);
      if (!s || !canWatch(owner, acct.username) || since !== s.since || s.log.some((e) => e.n > after) || ver !== s.ver) return answer();
      let done = false;
      const set = watchWaiters.get(owner) || new Set(); watchWaiters.set(owner, set);
      const finish = () => { if (done) return; done = true; clearTimeout(timer); set.delete(finish); answer(); };
      const timer = setTimeout(finish, 25000);
      set.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); set.delete(finish); } });
      return;
    }

    // ----- calls -----
    if (p === '/api/call/join' && req.method === 'POST') {
      const { conv, cam, mic } = await readBody(req);
      if (!validConv(conv) || !convMember(conv, acct.username)) return send(res, 400, { error: 'Unknown conversation.' });
      if (!chatKeys[acct.username]) return send(res, 400, { error: 'Your chat key is not set up yet.' });
      for (const [c, ps] of calls) if (c !== conv && ps.delete(acct.username)) { if (!ps.size) calls.delete(c); callChanged(c); }
      const ps = calls.get(conv) || new Map(); calls.set(conv, ps);
      const prev = ps.get(acct.username);
      ps.set(acct.username, { cam: !!cam, mic: !!mic, since: prev ? prev.since : new Date().toISOString(), seen: Date.now() });
      callChanged(conv);
      return send(res, 200, { calls: callState(acct.username), ver: callVer, iceServers: Array.isArray(config.iceServers) ? config.iceServers : ICE_DEFAULT });
    }
    if (p === '/api/call/leave' && req.method === 'POST') { leaveCalls(acct.username); return send(res, 200, { ok: true }); }
    if (p === '/api/call/state' && req.method === 'POST') {
      const { conv, cam, mic } = await readBody(req);
      const mine = calls.get(conv) && calls.get(conv).get(acct.username);
      if (!mine) return send(res, 409, { error: 'You are not in that call.' });
      Object.assign(mine, { cam: !!cam, mic: !!mic, seen: Date.now() });
      callChanged(conv);
      return send(res, 200, { ok: true });
    }
    if (p === '/api/call/signal' && req.method === 'POST') {
      const { to, conv, iv, ct } = await readBody(req);
      const ps = calls.get(conv);
      if (!ps || !ps.has(acct.username) || !ps.has(to) || to === acct.username) return send(res, 409, { error: 'Both people must be in this call.' });
      if (!b64(iv, 64) || !b64(ct, 200000)) return send(res, 400, { error: 'Bad signal.' });
      const box = mailbox.get(to) || []; mailbox.set(to, box);
      box.push({ n: ++callN, from: acct.username, conv, iv, ct });
      if (box.length > 500) box.splice(0, box.length - 500);
      wake([to]);
      return send(res, 200, { ok: true });
    }
    if (p === '/api/call/poll' && req.method === 'GET') {
      // Long poll: answers at once when there are signals after `after` or the call list changed
      // since `ver`, otherwise waits up to 25 s. Doubles as the "still here" heartbeat for calls.
      const after = Number(url.searchParams.get('after')) || 0, ver = Number(url.searchParams.get('ver'));
      const user = acct.username;
      const box = mailbox.get(user) || [];
      if (box.length) mailbox.set(user, box.filter((s) => s.n > after)); // delivered ones can go
      const answer = () => {
        for (const ps of calls.values()) if (ps.has(user)) ps.get(user).seen = Date.now();
        const sigs = (mailbox.get(user) || []).filter((s) => s.n > after);
        send(res, 200, { signals: sigs, after: sigs.length ? sigs[sigs.length - 1].n : after, calls: callState(user), ver: callVer });
      };
      if ((mailbox.get(user) || []).some((s) => s.n > after) || ver !== callVer) return answer();
      let done = false;
      const set = waiters.get(user) || new Set(); waiters.set(user, set);
      const finish = () => { if (done) return; done = true; clearTimeout(timer); set.delete(finish); answer(); };
      const timer = setTimeout(finish, 25000);
      set.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); set.delete(finish); } });
      return;
    }

    if (p === '/api/room/messages' && req.method === 'GET') {
      // ?rev=N → every message (new, edited or deleted) changed after N; first call (0) = the latest 500.
      // ?since=N → the older form: messages with seq > N.
      // &wait=1&ver=V → long poll: answered at once if anything changed since room version V,
      // otherwise held up to 25 s and answered the moment something does.
      const answer = () => {
        // only the reader's own key slot; a voice clip's body is left out (fetched from /api/room/blob on play)
        const strip = ({ keys, blob, ...m }) => ({ ...m, key: (keys || {})[acct.username] || null, ...(blob ? { blobLen: blob.length } : {}) });
        const mineVisible = messages.filter((m) => visibleTo(m, acct.username));
        let out;
        if (url.searchParams.has('rev')) {
          const after = Number(url.searchParams.get('rev')) || 0;
          out = after ? mineVisible.filter((m) => (m.rev || m.seq) > after) : mineVisible.slice(-500);
        } else {
          const since = Number(url.searchParams.get('since')) || 0;
          out = mineVisible.filter((m) => m.seq > since).slice(-300);
        }
        const seenOut = {}; // receipts for the conversations this reader is part of
        for (const [u, convs] of Object.entries(seen)) {
          for (const [c, v] of Object.entries(convs)) if (c === 'room' || c.split(/[:|]/).includes(acct.username)) (seenOut[u] || (seenOut[u] = {}))[c] = v;
        }
        send(res, 200, { messages: out.map(strip), latest: seq, latestRev: rev, seen: seenOut, shares: sharesFor(acct.username), ver: roomVer, keysVer,
          online: onlineUsers(), presence: presenceList(), typing: typingFor(acct.username) });
      };
      if (url.searchParams.get('wait') !== '1') { pres(acct.username).seen = Date.now(); return answer(); }
      // a long poll = a connected page: online while one is open, offline shortly after the last one closes
      const stale = Number(url.searchParams.get('ver')) !== roomVer; // judged before our own coming-online bumps the version
      pollOpened(acct.username);
      if (stale) { pollClosed(acct.username); return answer(); } // re-opens at once with the next poll
      let done = false;
      const finish = () => { if (done) return; done = true; clearTimeout(timer); roomWaiters.delete(finish); pollClosed(acct.username); answer(); };
      const timer = setTimeout(finish, 25000);
      roomWaiters.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); roomWaiters.delete(finish); pollClosed(acct.username); } });
      return;
    }
    if (p === '/api/presence' && req.method === 'POST') { // the page reports where it is, whether it is idle, and when it goes away
      const { screen, idle, gone } = await readBody(req);
      if (gone) { const q = pres(acct.username); q.polls = 0; clearTimeout(q.gone); setPresence(acct.username, { online: false, idle: false }); }
      else setPresence(acct.username, { ...(typeof screen === 'string' ? { screen: screen.slice(0, 12) } : {}), ...(idle !== undefined ? { idle: !!idle } : {}) });
      return send(res, 200, { ok: true });
    }
    if (p === '/api/room/blob' && req.method === 'GET') { // a voice clip's ciphertext, for a reader of that message
      const m = messages.find((x) => x.seq === Number(url.searchParams.get('seq')));
      if (!m || !m.blob || !visibleTo(m, acct.username) || !(m.keys || {})[acct.username]) return send(res, 404, { error: 'Not found.' });
      return send(res, 200, { blob: m.blob });
    }

    // ----- admin (accounts with "admin": true) -----
    if (p.startsWith('/api/admin/')) {
      if (!acct.admin) return send(res, 403, { error: 'Admins only.' });
      const body = req.method === 'POST' ? await readBody(req) : {};
      const target = body.username !== undefined ? accounts.get(String(body.username)) : null;
      const needTarget = () => { if (!target) throw new Error('No such account.'); return target; };

      if (p === '/api/admin/unity-tools' && req.method === 'POST') { // switch Unity's own-computer tools for one account
        const t = needTarget();
        if (typeof body.pc === 'boolean') t.state.unityPc = body.pc;
        if (typeof body.code === 'boolean') t.state.unityCode = body.code;
        saveState(t);
        return send(res, 200, { username: t.username, pc: t.state.unityPc !== false, code: t.state.unityCode !== false });
      }
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
      if (p === '/api/admin/stop' && req.method === 'POST') { const t = needTarget(); stopClaude(t); assistant.stop(t); return send(res, 200, { ok: true }); }
      if (p === '/api/admin/delete' && req.method === 'POST') {
        const t = needTarget();
        if (t === acct) return send(res, 400, { error: 'You cannot delete your own account.' });
        if (!t.profile) return send(res, 400, { error: `The "${DEFAULT_USERNAME}" account lives in bridge/ itself and cannot be deleted here.` });
        stopClaude(t); dropSessions(t.username); stopShare(t.username); leaveCalls(t.username);
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
        messages = []; fs.writeFileSync(CHAT_FILE, ''); agent.state.lastSeq = 0; writeJson(AGENT_STATE_FILE, agent.state);
        rev = 0; seen = {}; writeJson(SEEN_FILE, seen); // browsers see latestRev drop below theirs and reload
        roomChanged();
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
