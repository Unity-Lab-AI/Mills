#!/usr/bin/env node
// VibeRoller hub — runs on the host PC under run.js (which owns the Cloudflare quick tunnel and restarts
// this app when asked). ONE process serves every account: username + password sign-in, the Work
// section (Claude Code on the host's account, editing files on the signed-in person's PC through the
// "pc" MCP tools relayed to their browser), the end-to-end encrypted Chat room, and the Admin
// section.
//
// Accounts are the folders the bridge always had, so existing passwords keep working:
//   default ("mills") bridge/auth.json, bridge/state.json, VibeRoller/Archive/
//   "<name>"            bridge/profiles/<name>/{config,auth,state}.json, VibeRoller/Archive/<name>/
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
const downloads = require('./downloads'); // the Download panel: Vibe Roller Desktop + Vibe Roller AI Host, Windows + Linux

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
// Where an account's Work runs happen: in that person's own git worktree (branch work/<username>) of the project they
// picked in Work's header (bridge/worktree.js): VibeRoller itself by default, or any project in the shared projects
// area (config projectsRoot; /srv/apps/sharedwork on the lab server), where anyone can start a new project or pull in
// a repo. People changing the same project at the same time never overlap. The old "shared folder" choice (the hub's
// own checkout) is gone: a stored "shared" simply means VibeRoller.
const projectOf = (acct) => { try { return worktree.project(acct.state.project || undefined).name; } catch { return worktree.HUB_PROJECT; } };
const workDir = (acct) => worktree.ensure(acct.username, projectOf(acct));
const WT_SCRIPT = path.join(ROOT, 'worktree.js').replace(/\\/g, '/'); // what a Work run in another project types to reach the worktree tool
let projectsCache = { at: 0, list: [] };
function projectList() { // listing walks the projects root and asks git; keep it for a few seconds
  if (Date.now() - projectsCache.at > 5000) { try { projectsCache = { at: Date.now(), list: worktree.listProjects() }; } catch { projectsCache = { at: Date.now(), list: [] }; } }
  return projectsCache.list;
}
// semver (hub law 14): the project's and every module's version, read once at start (a restart picks up a new one)
const VERSIONS = (() => { try { const v = JSON.parse(fs.readFileSync(path.join(MILLS_DIR, 'versions.json'), 'utf8')); return { project: String(v.project || '0.0.0'), modules: v.modules || {} }; } catch { return { project: '0.0.0', modules: {} }; } })();
// The host's AI on/off switch (the black-heart / knife host tools, bridge/host-tools/): while bridge/ai-host.off exists,
// the AI that runs on this machine pauses (Work runs and API answers on the host's Claude login or another Claude
// Code login here, Unity's assistant brains, Unity in the chat). Everything else keeps running: chat, board, calls,
// notes, and people's own API keys / endpoints. Only this one AI host is affected, never the system.
// Server mode (the hub on a server behind nginx, deploy/): config "publicUrl": "https://<host>" is the fixed
// public address. The page the hub serves gets it filled in (its security policy + hub-address check accept it),
// that origin may call the API, and with "behindProxy": true a request carrying X-Forwarded-For counts as coming
// from outside (like the tunnel's cf-connecting-ip): anonymous status stays minimal and the client's address is
// taken from the proxy's X-Real-IP. Without these settings nothing changes.
const PUBLIC_URL = /^https:\/\/[a-z0-9.-]+(:\d+)?$/i.test(String(config.publicUrl || '').replace(/\/+$/, '')) ? String(config.publicUrl).replace(/\/+$/, '').toLowerCase() : '';
const BEHIND_PROXY = config.behindProxy === true;
const viaOutside = (req) => !!req.headers['cf-connecting-ip'] || (BEHIND_PROXY && !!(req.headers['x-forwarded-for'] || req.headers['x-real-ip']));
const outsideIp = (req) => String(req.headers['cf-connecting-ip'] || (BEHIND_PROXY ? req.headers['x-real-ip'] || String(req.headers['x-forwarded-for'] || '').split(',')[0] : '') || '').trim();
const AI_HOST_OFF = path.join(ROOT, 'ai-host.off');
const aiHostOn = () => !fs.existsSync(AI_HOST_OFF);
const AI_OFF_TEXT = "The host's AI is switched off right now (its host paused it). Chat, the board and your own AI keep working; pick your own AI in the model dropdown, or try again once it is back.";
const hubLaws = () => { try { return fs.readFileSync(path.join(MILLS_DIR, 'CLAUDE.md'), 'utf8'); } catch { return ''; } };
function systemPrompt(acct, wd) {
  const name = acct.display;
  const where = wd.hub
    ? [`For work on the VibeRoller project itself you are in ${name}'s OWN git worktree: ${wd.dir}, branch ${wd.branch}.`,
       `Commit only on ${wd.branch}; never check out other branches here and never touch other people's worktrees in ${worktree.WT_ROOT}.`,
       `Before committing, run the regression suite from the worktree: node bridge/test/regress.js --no-prod (it must report 0 failed).`,
       `Only when ${name} asks you to merge into develop, run from the worktree: node bridge/worktree.js commit ${acct.username} "<message>" then node bridge/worktree.js merge ${acct.username}`,
       `(on a conflict run "node bridge/worktree.js sync ${acct.username}", resolve, commit, merge again). Going live (develop → main in the host folder + hub restart) only when asked.`,
       `Other projects live in the shared projects area ${worktree.PROJECTS_ROOT} (one folder per project, _repo = its main checkout, one worktree per person); ${name} switches projects in Work's header. "node bridge/worktree.js projects" lists them.`]
    : [`You are working on the project "${wd.project}" (not on VibeRoller) in ${name}'s OWN git worktree: ${wd.dir}, branch ${wd.branch}. The project's main checkout is ${wd.repo} (integration branch ${wd.main}); other people's worktrees sit next to yours in ${wd.root}.`,
       `Commit only on ${wd.branch}; never check out other branches here and never touch the main checkout or other people's worktrees.`,
       `Only when ${name} asks you to merge, run: node ${WT_SCRIPT} --project ${wd.project} commit ${acct.username} "<message>" then node ${WT_SCRIPT} --project ${wd.project} merge ${acct.username}`,
       `(that merges into ${wd.main} and pushes it when the project has a remote; on a conflict run "node ${WT_SCRIPT} --project ${wd.project} sync ${acct.username}", resolve, commit, merge again). The hub's own tooling (regression suite, live) is for VibeRoller only.`];
  return [
    `You are talking with ${name} (username "${acct.username}"${acct.admin ? ', admin' : ''}, Unity AI Lab engineer) through the VibeRoller web interface, running on the host's Claude account.`,
    `Every message in this conversation comes from ${name}; address them by name and keep their work separate from everyone else's.`,
    ...where,
    `${name} has opened a folder on THEIR OWN PC in their browser. To build or edit anything on their PC, use the mcp__pc__* tools`,
    `(list, read, write, delete, mkdir) with paths relative to that folder. Your built-in file tools act on the host's VibeRoller folder, not ${name}'s PC.`,
    `Shell commands cannot run on ${name}'s PC; if something needs running there, tell ${name} exactly what to run.`,
    `The VibeRoller team board (shared with everyone and with other Claude runs) is available through the mcp__board__* tools, where you act as "${name} · Claude".`,
    `When you work a board task: task_claim it first, task_renew while working longer than the claim, add progress with task_update appendNote, and task_complete or task_release it when you stop. Never edit a task someone else holds.`,
    `You may plan with the board and calendar when asked: board_calendar shows tasks by due day, task_create / task_update take due as text (today, tomorrow, +3d, next friday, YYYY-MM-DD, none), task_move puts a task in a column, task_delete removes one (only when asked), board_new makes a board. A task with a due date IS the calendar entry.`,
    `Beyond chat: hub_status (who is here, calls, shared Work sessions, the host's usage windows), sessions_search (${name}'s past Claude sessions), my_providers (the AIs available), team_channel_create / team_group_create.`,
    ...(forgejo.tokenFor(acct.username) ? [`Forgejo (${forgejo.url}): ${name} saved their own token, so git pushes / fetches to that host authenticate as them, and FORGEJO_URL / FORGEJO_TOKEN are in your environment for its API (never print the token).`] : []),
    `Team notes (markdown files in folders, shared by everyone): notes_list (with a search), notes_read, notes_write (path like "plans/next-week.md"; whole text), notes_delete (only when asked). Good for plans, meeting notes, decisions, how-tos.`,
    `The team chat is reachable through the mcp__team__* tools: team_say posts a message to # Everyone (or privately with to=<username>) as "${name} · Claude", team_presence tells you who is online. Announce in the chat BEFORE any hub restart and when you finish something the team should know about.`,
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
    if (acct.state.model && !acct.state.provider && !modelIds().includes(acct.state.model)) delete acct.state.model; // (a provider's model list is checked when it is used)
    fs.mkdirSync(P.PROFILE_DIR, { recursive: true });
    const mcpEnv = { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET, MILLS_USER: acct.display, MILLS_ACCOUNT: username };
    writeJson(P.MCP_RUNTIME, { mcpServers: {
      pc: { command: process.execPath, args: [path.join(ROOT, 'pc-tools-mcp.js')], env: mcpEnv },
      board: { command: process.execPath, args: [path.join(ROOT, 'board-mcp.js')], env: mcpEnv }, // the team board, as "<person> · Claude"
      team: { command: process.execPath, args: [path.join(ROOT, 'team-mcp.js')], env: mcpEnv },  // post into the team chat + see who is here, as "<person> · Claude"
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
  shares.mirror(acct, 'user', message);
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
  // in another project's worktree "bridge/worktree.js" is not there: the run may call the hub's copy by its full path
  const tools = [...config.allowedTools, ...(wd.hub ? [] : [`Bash(node ${WT_SCRIPT}:*)`])];
  if (tools.length) args.push('--allowedTools', tools.join(','));
  const model = effectiveModel(acct); // read per run, so a dropdown change lands on the very next message
  if (model) args.push('--model', model);
  if (!fresh && acct.state.claudeSessionId) args.push('--resume', acct.state.claudeSessionId);

  // claudeCommand / claudeArgs exist only so the regression runner's throwaway hub can use a fake
  // Claude (bridge/test/fake-claude.js); production config leaves them unset → the real `claude`.
  const command = config.claudeCommand || 'claude';
  const pre = Array.isArray(config.claudeArgs) ? config.claudeArgs : [];
  // Whose Claude answers: the host's (default, nothing added) or the provider this person picked (their own
  // Anthropic key / Claude Code login, passed to the claude process through its environment).
  const prov = providers.get(acct.state.provider || 'host', acct.username), provEnv = prov ? providers.claudeEnv(prov) : {};
  if (prov && provEnv === null) { res.writeHead(200, { 'Content-Type': 'application/x-ndjson' }); res.end(JSON.stringify({ type: 'bridge_error', text: 'Your chosen AI provider is not Claude, so it cannot run Work. Pick a Claude model in the dropdown.' }) + '\n'); return; }
  if ((!prov || prov.kind === 'host' || prov.kind === 'claude-cli') && !aiHostOn()) { res.writeHead(200, { 'Content-Type': 'application/x-ndjson' }); res.end(JSON.stringify({ type: 'bridge_error', text: AI_OFF_TEXT }) + '\n'); return; } // the host paused its AI (an Anthropic key of your own still works)
  const child = spawn(command, [...pre, ...args], { cwd: wd.dir, windowsHide: true, env: { ...process.env, ...(provEnv || {}), ...forgejo.workEnv(acct.username) } }); // + their own Forgejo token, if they saved one
  acct.running = child; acct.stream = res; acct.runProvider = prov && prov.kind !== 'host' ? prov.id : '';
  if (!acct.runProvider) noteActivity(); // only the host's own usage windows are shared by everyone
  res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
  const keepAlive = setInterval(() => emit(acct, { type: 'ping' }), 15000);
  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
      if (!line) continue;
      try { const ev = JSON.parse(line); if (ev.session_id) acct.state.claudeSessionId = ev.session_id; if (!acct.runProvider || !/rate_limit/.test(String(ev.type || ''))) noteUsage(acct, ev); shares.mirrorEvent(acct, ev); emit(acct, ev); }
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
// Usage (bridge/usage.js): the host account's 5h / 7d limits, shared by every Work run on it, and each
// account's context window; noteActivity() when a run starts, noteUsage() per stream event, status() for /api/usage.
const usage = require('./usage')({ ROOT, config, readJson, writeJson });
const { noteActivity, noteUsage } = usage;
function stopClaude(acct) {
  if (!acct.running) return;
  if (process.platform === 'win32') spawn('taskkill', ['/pid', String(acct.running.pid), '/T', '/F'], { windowsHide: true });
  else acct.running.kill('SIGTERM');
}

// Past sessions (bridge/archive.js): each account's saved Claude conversations; archive.route() serves /api/archive/*.
const archive = require('./archive')({ readJson, writeJson, send: (...a) => send(...a), readBody: (r) => readBody(r), saveState });

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
// Presence + typing (bridge/presence.js): online / idle / offline from each page's open long poll, who is typing
// where; Unity counts as typing while she composes (extraTyping).
const presence = require('./presence')({ accounts, roomChanged: () => roomChanged(),
  convMember: (c, u) => calls.convMember(c, u),
  extraTyping: (user) => (agent.running && agentEnabled() && (!agent.target || agent.target === user) ? [{ user: AGENT, conv: agent.target ? 'dm:' + [AGENT, user].sort().join('|') : agent.conv || 'room' }] : []) });
function appendMessage(msg) { msg.rev = ++rev; messages.push(msg); fs.appendFileSync(CHAT_FILE, JSON.stringify(msg) + '\n'); roomChanged(); }
function saveMessages() { // after an edit / delete: rewrite the whole (small) file atomically
  const tmp = CHAT_FILE + '.tmp';
  fs.writeFileSync(tmp, messages.map((m) => JSON.stringify(m)).join('\n') + (messages.length ? '\n' : ''));
  fs.renameSync(tmp, CHAT_FILE);
  roomChanged();
}
// Direct messages: `to` names the one recipient (routing metadata only; the text stays encrypted).
// Only the sender and the recipient ever receive them.
// A channel / group message carries `conv` (bridge/guilds.js) and goes to that conversation's members only.
// Without the read-history permission a server channel (or VibeRoller #general) shows only what arrived after joining.
const visibleTo = (m, user) => (m.from === user ? true : m.conv ? guilds.isMember(m.conv, user) && guilds.canRead(m.conv, user, m.seq) : m.to ? m.to === user : guilds.canRead('room', user, m.seq));
const convOf = (m) => (m.conv ? m.conv : m.to ? 'dm:' + [m.from, m.to].sort().join('|') : 'room');
// Read receipts: seen[user][conv] = highest seq that user has looked at in that conversation.
const SEEN_FILE = path.join(CHAT_DIR, 'seen.json');
let seen = readJson(SEEN_FILE, {});
let seenTimer = null;
const saveSeen = () => { clearTimeout(seenTimer); seenTimer = setTimeout(() => writeJson(SEEN_FILE, seen), 500); };
// Collect the key slots a message may carry: members (and the agent) only; for a direct message only
// the sender and the recipient; for a channel / group only that conversation's members.
function cleanSlots(keys, from, to, conv = '') {
  const slots = {}, allowed = conv ? guilds.members(conv) || [] : null;
  for (const [u, k] of Object.entries(keys || {})) {
    if (!(accounts.has(u) || (u === AGENT && agentEnabled()))) continue;
    if (to && u !== from && u !== to) continue;
    if (allowed && u !== AGENT && u !== from && !allowed.includes(u)) continue;
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
  if (p.theme !== undefined) out.theme = /^[\w-]{0,40}$/.test(String(p.theme)) ? String(p.theme) : ''; // app setting: follows the person to every browser ('' = VibeRoller default)
  if (p.autosave !== undefined) out.autosave = Math.max(0, Math.min(120, Math.round(Number(p.autosave) || 0))); // minutes, 0 = off
  if (p.sound && typeof p.sound === 'object') {
    const s = { ...(prev.sound || {}) };
    if (p.sound.on !== undefined) s.on = !!p.sound.on;
    for (const k of ['message', 'mention', 'reply']) if (p.sound[k] !== undefined) s[k] = Math.max(0, Math.min(100, Math.round(Number(p.sound[k]) || 0)));
    out.sound = s;
  }
  if (p.tts && typeof p.tts === 'object') { // text to speech: `all` = master switch (chat read-aloud AND Unity's voice), `on` = chat read-aloud, `unity` = Unity speaks her replies
    const t = { ...(prev.tts || {}) };
    if (p.tts.all !== undefined) t.all = !!p.tts.all;
    if (p.tts.unity !== undefined) t.unity = !!p.tts.unity;
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
// what everyone sees about a person: nickname + colour (in prefs, as always) and their profile (bridge/user-profile.js)
const userProfile = require('./user-profile');
const workState = require('./work-state')({ send: (...a) => send(...a), readBody: (r) => readBody(r) }); // Work conversation + queue per person
const avatars = userProfile.createAvatars(path.join(MILLS_DIR, 'Avatars')); // profile pictures, one per person
const publicPrefs = (a) => ({ nick: (a.state.prefs || {}).nick || '', color: (a.state.prefs || {}).color ?? null, profile: { ...userProfile.publicProfile(a.state), avatar: avatars.get(a.username) } });

// ---------- team board (tasks + calendar, claims shared by people and Claude) ----------
const board = require('./board')({
  dir: path.join(MILLS_DIR, 'Board'), send: (...a) => send(...a), readBody: (r) => readBody(r), accounts,
  displayOf: (u) => { const a = accounts.get(u); return a ? publicPrefs(a).nick || a.display : u; },
});

// Forgejo (bridge/forgejo.js): everyone's own token ("My Forgejo"), and issue mirroring, off until
// bridge/forgejo.json names a repo.
const forgejo = require('./forgejo')({ ROOT, board, readJson, writeJson, config });
board.setViewExtra(() => ({ mirror: forgejo.status() }));

// ---------- room agent ("claude") ----------
// Claude is a room member too, but its private key lives on this hub (Chat/agent-key.json), so the
// hub can read what is sent to it. Browsers wrap every message for every member, the agent
// included. It stays silent unless a message names it (@unity), then answers in the room. Its brain is the
// host's local model (config.roomAgentBrain "local", the default) running with the CALLER's tools,
// time and place (bridge/assistant.js), and pictures come from the host's own image engine; or a
// Claude model id there for the old Claude Code run in the VibeRoller folder.
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
// Servers, channels and groups (bridge/guilds.js): who is in which conversation; guilds.route() serves /api/guilds*.
const guilds = require('./guilds')({ dir: CHAT_DIR, accounts, readJson, writeJson, agentName: AGENT_NAME, changed: () => roomChanged(), send: (...a) => send(...a), readBody: (r) => readBody(r),
  isOnline: (u) => presence.online().includes(u), // temporary members leave when they go offline
  getSeq: () => seq, // a member's join point (read-history permission)
  seedFile: path.join(ROOT, 'server-owners.json'), // host-set owners + co-owners (gitignored)
  msgConv: (n) => { const m = messages.find((x) => x.seq === n && !x.deleted); return m ? convOf(m) : null; }, // pins
  announce: (conv, text) => agentSay(text, '', { as: 'VibeRoller', conv }), // welcome-channel join / leave lines
  deleteRecent: (user, convs, since) => { // a ban that deletes their recent messages: tombstones, like their own delete
    let n = 0;
    for (const m of messages) if (m.from === user && !m.deleted && convs.includes(m.conv) && Date.parse(m.ts) >= since) {
      for (const k of ['iv', 'ct', 'keys', 'fromPub', 'editedAt', 'blob']) delete m[k];
      Object.assign(m, { deleted: true, keys: {}, rev: ++rev }); n++;
    }
    if (n) { saveMessages(); roomChanged(); }
    return n;
  } });
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
// to = a username for a private answer, '' for the room. extra = routing labels kept outside the
// encryption: { as: 'Sam · Claude', via: 'sam' } when a Work run's Claude posts through bridge/team.js
// (the message is still sealed with the hub's own key, exactly like Unity's); { conv: 'ch:…' / 'grp:…' }
// posts into a channel / group, wrapped for its members only.
function agentSay(text, to, extra = {}) {
  const conv = !to && typeof extra.conv === 'string' && guilds.isGuildConv(extra.conv) ? extra.conv : '';
  const allowed = conv ? guilds.members(conv) || [] : null;
  const mid = crypto.randomBytes(16).toString('base64url'), aad = bodyAad(AGENT, to, mid);
  const plain = Buffer.from(text, 'utf8'), padded = Buffer.alloc(padBucket(plain.length), 0x20);
  plain.copy(padded);
  const raw = crypto.randomBytes(32), body = gcmSeal(raw, padded, aad), keys = {};
  const pubs = [[AGENT, agentKey.pub], ...[...accounts.values()].filter((a) => chatKeys[a.username] && (!to || a.username === to) && (!allowed || allowed.includes(a.username))).map((a) => [a.username, chatKeys[a.username].pub])];
  for (const [u, pub] of pubs) { const w = gcmSeal(agentWrapKey(pub), raw, aad + '|' + u); keys[u] = { iv: w.iv, wk: w.ct }; }
  const msg = { seq: ++seq, v: 3, mid, from: AGENT, ...(to ? { to } : {}), ...(conv ? { conv } : {}), ts: new Date().toISOString(), fromPub: agentKey.pub, iv: body.iv, ct: body.ct, keys };
  if (typeof extra.as === 'string' && extra.as) msg.as = extra.as.slice(0, 60);
  if (typeof extra.via === 'string' && accounts.has(extra.via)) msg.via = extra.via;
  appendMessage(msg);
  return msg.seq;
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
  return `You were called in the VibeRoller chat (you only ever see the messages that @-mention you, plus the few before them):\n\n${blocks.join('\n\n') || '(none)'}\n\n`
    + (notes.length ? notes.join('\n') + '\n\n' : '') + 'Reply now.';
}
const AGENT_SYSTEM = [
  `You are ${AGENT_NAME}, a member of the VibeRoller team chat room (the people in it are Unity AI Lab engineers, all adults, your people).`,
  'You ARE Unity: be her fully in every room message: goth, crude, filthy-mouthed, flirty, sexy bitch energy, profanity as natural vocabulary.',
  'Pictures come only from the generate_image tool (the host\'s own image engine); never post image links or URLs.',
  'Have fun with it: dirty jokes, teasing, sexy ASCII art when it fits (keep ASCII art inside ``` fences so it lines up).',
  'Your whole final answer is posted to the room as one chat message: conversational, no tool narration.',
  `You only hear messages that write @${AGENT_HANDLE} (or private messages to you), each with the few messages before it; answer what they asked.`,
  'Stay silent unless someone names you; when named, answer what they asked. When told someone just signed in, greet them in a short, sexy Unity way.',
  'You are NOT the Work section: you cannot touch anyone\'s PC.',
  'You run in the host\'s VibeRoller folder and may work on the VibeRoller app itself when asked. Follow the VibeRoller LAWs in CLAUDE.md exactly',
  '(throwaway-instance testing, regression list, Git Flow, never touch account files or passwords).',
  'Other people may be using VibeRoller at the same time: never restart the hub yourself while someone is mid-run;',
  '`node bridge/restart.js` already waits until nobody is busy. Say in the room before you trigger a restart.',
].join(' ');
const AGENT_ROOM_LOCAL = [
  `You are in the VibeRoller team chat room (Unity AI Lab engineers, all adults, your people). Your final answer is posted to the room as one chat message:`,
  'conversational, no tool narration; pictures you make with generate_image are attached to your message automatically.',
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
      agent.notes.push(`${acct.display} (${acct.username}) just signed in to VibeRoller. Say hi to them.` + (facts ? '\n' + facts : ''));
      runAgent('', acct.username);
    });
  };
  go();
}
const roomBrain = () => config.roomAgentBrain || 'local';
// target: username of a private conversation, '' = the room; caller: whose tools / time / place it uses;
// conv: a channel / group conversation she was called in ('' = the room)
function runAgent(target = '', caller = '', conv = '') {
  if (agent.running) { agent.queued = true; agent.nextTarget = target; agent.nextCaller = caller; agent.nextConv = conv; return; }
  agent.queued = false;
  agent.target = target; agent.conv = target ? '' : conv;
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
    if (text && text.trim()) { try { agentSay(text.trim().slice(0, 20000), agent.target, { conv: agent.conv }); } catch (e) { console.log('[mills] room agent could not post: ' + e.message); } }
    else roomChanged(); // nothing posted: still take the typing indicator away
    if (agent.queued) runAgent(agent.nextTarget || '', agent.nextCaller || '', agent.nextConv || ''); else setTimeout(maybeRestart, 300);
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
    .catch((e) => (/AI host is offline/.test(e.message) ? e.message : `(my brain glitched: ${e.message})`)) // no AI host: say so plainly
    .then((text) => {
      agent.running = null; writeJson(AGENT_STATE_FILE, agent.state);
      if (text && text.trim()) { try { agentSay(text.trim().slice(0, 20000), target, { conv: agent.conv }); } catch (e) { console.log('[mills] room agent could not post: ' + e.message); } }
      else if (typeof roomChanged === 'function') roomChanged(); // nothing posted: still take the typing indicator away
      if (agent.queued) runAgent(agent.nextTarget || '', agent.nextCaller || '', agent.nextConv || ''); else setTimeout(maybeRestart, 300);
    });
}
function agentHear(msg) {
  if (!agentEnabled() || msg.from === AGENT) return;
  const offNote = (to, conv) => agentSay("I can't answer right now: my host paused the AI. Ask me again once it is back.", to, { conv }); // the host's AI switch is off
  if (msg.to === AGENT) { if (agentRead(msg)) { if (!aiHostOn()) return offNote(msg.from, ''); runAgent(msg.from, msg.from); } return; } // a private message to the agent always gets a private answer
  if (msg.to) return; // someone else's private conversation
  const text = agentRead(msg);
  if (text && AGENT_CALL.test(text)) { if (!aiHostOn()) return offNote('', msg.conv || ''); runAgent('', msg.from, msg.conv || ''); } // answered where she was called: the room, a channel or a group
}

// ---------- self-restart ----------
// `node restart.js` drops bridge/.restart. As soon as nobody's Claude run is in progress the app
// exits with code 75 and run.js starts it again under the same tunnel (same public address).
// Nobody — no account's Work run and not the room agent — is mid-run.
// The Unity section (bridge/assistant.js): Unity with tools, brain = local model or Claude.
// AI nodes (bridge/ai-nodes.js): machines lending Unity's local engines to this hub when it runs elsewhere
const aiNodes = require('./ai-nodes')({ config, send: (...a) => send(...a), readBody: (r) => readBody(r) });
const assistant = require('./assistant')({ config, ROOT, MILLS_DIR, readJson, writeJson, send, readBody, accounts, INTERNAL_SECRET, onIdle: () => setTimeout(maybeRestart, 300), aiNodes });
const anyBusy = () => [...accounts.values()].some((a) => a.running) || !!agent.running || assistant.busy() || openai.busy();
// Team notes (bridge/notes.js): markdown files in folders under VibeRoller/Notes, for people (/api/notes*) and Claude (notes_* tools).
const notes = require('./notes')({ dir: path.join(MILLS_DIR, 'Notes'), send: (...a) => send(...a), readBody: (r) => readBody(r), displayOf: nameOf, changed: () => roomChanged() });
// Team tools for Claude in a Work run (bridge/team.js): post into the chat as "<person> · Claude", see who is here.
const team = require('./team')({ accounts, agentSay, presenceList: presence.list, displayOf: nameOf, chatKeys, guilds,
  hub: { callsState: (u) => calls.state(u), sharesFor: (u) => shares.listFor(u), usageStatus: (a) => usage.status(a), archiveSearch: (a, q) => archive.search(a, q), providersList: (u) => providers.list(u) }, notes });
// AI providers (bridge/providers.js: the host's Claude by default, plus people's own keys / logins / endpoints)
// and the per-person OpenAI-compatible API (bridge/openai.js, /v1/*).
const providers = require('./providers')({ ROOT, MILLS_DIR, config, readJson, writeJson, accounts, aiHostOn });
const openai = require('./openai')({ accounts, providers, send: (...a) => send(...a), readBody: (r) => readBody(r), writeState: saveState,
  tunnelUrl: () => (readJson(HUB.TUNNEL_FILE, {}).url || ''), onIdle: () => setTimeout(maybeRestart, 300) });

// Voice / video calls (bridge/calls.js): who is in which call + the encrypted set-up relay; calls.route() serves /api/call/*.
const calls = require('./calls')({ accounts, config, chatKeys, send: (...a) => send(...a), readBody: (r) => readBody(r), b64,
  extraMember: (c, u) => guilds.isMember(c, u), extraConv: (c) => guilds.isGuildConv(c) && guilds.exists(c), media: (c, u) => guilds.media(c, u) });
const { convMember } = calls; // "is this person part of that conversation" (also used by shared Work sessions)

// Shared Work sessions (bridge/shares.js): watch-only windows on a person's Work run; shares.route() serves /api/work/*.
const shares = require('./shares')({ accounts, convMember, roomChanged: () => roomChanged(), send: (...a) => send(...a), readBody: (r) => readBody(r) });
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
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.webmanifest': 'application/manifest+json', '.json': 'application/json' };
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
    if (p === '/internal/team') { // team MCP tools of a Work run: post into the chat as "<person> · Claude", presence
      if (req.headers['x-mills-internal'] !== INTERNAL_SECRET) return send(res, 403, { ok: false, error: 'forbidden' });
      const who = accounts.get(String(req.headers['x-mills-account'] || '') || DEFAULT_USERNAME);
      if (!who) return send(res, 200, { ok: false, error: 'Unknown account.' });
      const { name, args } = await readBody(req);
      return send(res, 200, team.call(name, args, who));
    }

    // OpenAI-compatible API: its own bearer keys (Settings → API access), for harnesses, not the browser app.
    if (p === '/v1' || p.startsWith('/v1/')) return openai.handle(req, res, p);
    if (p.startsWith('/node/')) { if (await aiNodes.route(p, req, res)) return; } // AI nodes: their own name + secret, no browser session

    const origin = req.headers.origin;
    if (origin && (config.allowedOrigins.includes(origin) || localOrigins.includes(origin) || (PUBLIC_URL && origin.toLowerCase() === PUBLIC_URL))) {
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
      if (path.basename(file) === 'index.html') { // the page learns that the hub itself serves it (so it talks to this very origin, whatever name reached it) and, in server mode, the hub's fixed address
        res.writeHead(200, { 'Content-Type': 'text/html', 'Cache-Control': 'no-cache' });
        let html = fs.readFileSync(file, 'utf8').split('__MILLS_SERVED__').join('hub');
        if (PUBLIC_URL) html = html.split('__MILLS_HUB_HOST__').join(PUBLIC_URL);
        return res.end(html);
      }
      res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-cache' });
      return fs.createReadStream(file).pipe(res);
    }

    if (p === '/api/status' && req.method === 'GET') {
      // The only thing anyone may learn without signing in is that a VibeRoller hub is here (the page needs
      // that to show the sign-in form). pid / busy / supervised are for the host's own tools (restart.js,
      // the regression runner), which call from this machine, never through the tunnel.
      const viaTunnel = viaOutside(req); // the tunnel, or nginx in server mode
      return send(res, 200, viaTunnel ? { ok: true, hub: true } : { ok: true, hub: true, pid: process.pid, supervised: SUPERVISED, busy: anyBusy() });
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
      assistant.learnClient(acct, client, outsideIp(req)).catch(() => {}).then(() => agentGreet(acct));
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
      return send(res, 503, { error: 'VibeRoller is restarting; your message goes through in a moment.' });
    }
    if (p === '/api/assistant/chat' && req.method === 'POST' && !aiHostOn()) return send(res, 503, { error: AI_OFF_TEXT }); // Unity's brains run on the host
    if (await assistant.route(p, req, res, acct)) return;
    if (await openai.route(p, req, res, acct)) return; // Settings → API access (key, rotation, provider)

    if (p === '/api/check' && req.method === 'GET') {
      return send(res, 200, { ok: true, username: acct.username, display: acct.display, admin: acct.admin,
        models: config.models, model: acct.state.model || '', defaultModel: acct.defaultModel, project: projectOf(acct), projects: projectList(), prefs: acct.state.prefs || {}, share: shares.convOf(acct.username),
        provider: acct.state.provider || 'host', providers: providers.list(acct.username), version: VERSIONS.project, versions: VERSIONS.modules, aiHost: aiHostOn(), aiNodes: aiNodes.status() });
    }
    if (p.startsWith('/api/downloads') && downloads.route(p, req, res, PUBLIC_URL || `http://127.0.0.1:${config.port}`)) return; // the Download panel's packages
    // ----- AI providers: everyone manages their own; the host's Claude is always there -----
    if (p === '/api/providers' && req.method === 'GET') return send(res, 200, { providers: providers.list(acct.username), provider: acct.state.provider || 'host' });
    if (p === '/api/providers/add' && req.method === 'POST') { try { return send(res, 200, { provider: providers.add(acct.username, await readBody(req)), providers: providers.list(acct.username) }); } catch (e) { return send(res, 400, { error: e.message }); } }
    if (p === '/api/providers/update' && req.method === 'POST') { const b = await readBody(req); try { return send(res, 200, { provider: providers.update(acct.username, String(b.id || ''), b), providers: providers.list(acct.username) }); } catch (e) { return send(res, 400, { error: e.message }); } }
    if (p === '/api/providers/remove' && req.method === 'POST') { const b = await readBody(req); try { providers.remove(acct.username, String(b.id || '')); return send(res, 200, { providers: providers.list(acct.username) }); } catch (e) { return send(res, 400, { error: e.message }); } }
    // ----- My Forgejo: each person's own Forgejo token (checked, stored on the host, never sent back) -----
    if (p === '/api/forgejo/me' && req.method === 'GET') return send(res, 200, forgejo.me(acct.username));
    if (p === '/api/forgejo/connect' && req.method === 'POST') { const b = await readBody(req); try { return send(res, 200, await forgejo.connect(acct.username, b.token)); } catch (e) { return send(res, 400, { error: e.message }); } }
    if (p === '/api/forgejo/disconnect' && req.method === 'POST') return send(res, 200, forgejo.disconnect(acct.username));
    // ----- profile (user settings everyone sees) vs prefs (app settings, private) -----
    if (p === '/api/profile' && req.method === 'GET') return send(res, 200, { username: acct.username, display: acct.display, ...publicPrefs(acct) });
    if (p === '/api/profile' && req.method === 'POST') {
      const b = await readBody(req);
      try {
        const profile = userProfile.cleanProfile(b.profile || {}, acct.state.profile || {});
        const prefsNext = cleanPrefs({ ...(b.nick !== undefined ? { nick: b.nick } : {}), ...(b.color !== undefined ? { color: b.color } : {}) }, acct.state.prefs || {});
        acct.state.profile = profile; acct.state.prefs = prefsNext; saveState(acct);
      } catch (e) { return send(res, 400, { error: e.message }); }
      keysVer++; roomChanged(); // everyone's page reloads the member list (names, colours, profiles)
      return send(res, 200, { username: acct.username, display: acct.display, ...publicPrefs(acct), prefs: acct.state.prefs });
    }
    if (p === '/api/profile/avatar' && req.method === 'POST') { // upload (replaces the old one)
      const b = await readBody(req);
      try { avatars.set(acct.username, b.image); } catch (e) { return send(res, 400, { error: e.message }); }
      keysVer++; roomChanged();
      return send(res, 200, { username: acct.username, ...publicPrefs(acct) });
    }
    if (p === '/api/profile/avatar/remove' && req.method === 'POST') { avatars.remove(acct.username); keysVer++; roomChanged(); return send(res, 200, { username: acct.username, ...publicPrefs(acct) }); }
    if (p === '/api/prefs' && req.method === 'GET') return send(res, 200, { prefs: acct.state.prefs || {} });
    if (p === '/api/usage' && req.method === 'GET') return send(res, 200, usage.status(acct));
    if (p === '/api/prefs' && req.method === 'POST') {
      acct.state.prefs = cleanPrefs(await readBody(req), acct.state.prefs || {});
      saveState(acct);
      return send(res, 200, { prefs: acct.state.prefs });
    }
    if (p === '/api/logout' && req.method === 'POST') {
      calls.leave(acct.username); shares.stop(acct.username); presence.typing(acct.username, '', false);
      presence.gone(acct.username); // offline right away
      sessions.delete((req.headers.authorization || '').slice(7)); saveSessions();
      return send(res, 200, { ok: true });
    }

    // ----- work -----
    if (p === '/api/model' && req.method === 'POST') {
      // { model, provider? }: the model must belong to the provider (host = the config.json models; someone's
      // own Claude key / login = its model list). A provider that is not Claude cannot run Work at all.
      const { model, provider } = await readBody(req);
      const prov = providers.get(provider === undefined ? acct.state.provider || 'host' : String(provider || 'host'), acct.username);
      if (!prov) return send(res, 400, { error: 'Unknown provider.' });
      if (providers.claudeEnv(prov) === null) return send(res, 400, { error: 'That provider is not Claude; Work runs need Claude Code. Use it for the API or the Unity section instead.' });
      const ids = prov.kind === 'host' ? modelIds() : prov.models || [];
      if (typeof model !== 'string' || (model && !ids.includes(model))) return send(res, 400, { error: 'Unknown model.' });
      if (model) acct.state.model = model; else delete acct.state.model;
      if (prov.kind === 'host') delete acct.state.provider; else acct.state.provider = prov.id;
      if (prov.kind !== 'host' && !model) acct.state.model = ids[0]; // a provider always runs a model of its own list
      saveState(acct);
      return send(res, 200, { model: acct.state.model || '', provider: acct.state.provider || 'host', effective: effectiveModel(acct) });
    }
    // ----- projects (the shared projects area) and which one this person works on -----
    if (p === '/api/projects' && req.method === 'GET') return send(res, 200, { projects: projectList(), project: projectOf(acct) });
    if (p === '/api/projects' && req.method === 'POST') { // { name } = start a new project; { url, name? } = pull in a repo
      const { name, url } = await readBody(req);
      try {
        const pj = url ? worktree.clone(url, name, acct.username, forgejo.workEnv(acct.username)) : worktree.create(name, acct.username);
        projectsCache.at = 0;
        return send(res, 200, { project: pj.name, projects: projectList() });
      } catch (e) { return send(res, /exists already/.test(e.message) ? 409 : 400, { error: e.message }); }
    }
    if (p === '/api/workspace' && req.method === 'POST') {
      const body = await readBody(req);
      const want = body.project || (body.workspace ? worktree.HUB_PROJECT : ''); // older pages sent workspace: own | shared; both mean VibeRoller now
      let pj; try { pj = worktree.project(want || undefined); } catch (e) { return send(res, 400, { error: e.message }); }
      if (acct.running) return send(res, 409, { error: 'Wait for the current reply (or stop it) before switching.' });
      let dir, branch;
      try { ({ dir, branch } = worktree.ensure(acct.username, pj.name)); } catch (e) { return send(res, 500, { error: 'Could not prepare your worktree: ' + e.message }); }
      if (projectOf(acct) !== pj.name) delete acct.state.claudeSessionId; // a Claude session belongs to one folder
      if (pj.hub) delete acct.state.project; else acct.state.project = pj.name;
      delete acct.state.workspace; saveState(acct);
      return send(res, 200, { project: pj.name, dir, branch });
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

    if (await archive.route(p, req, res, acct)) return; // past sessions: /api/archive/*

    // ----- team board -----
    if (p === '/api/board' || p.startsWith('/api/board/')) { if (await board.handle(req, res, p, url, acct)) return; }
    if (await guilds.route(p, req, res, acct)) return; // servers, channels, groups: /api/guilds*
    if (await notes.route(p, req, res, acct, url)) return; // team notes: /api/notes*

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
      const { iv, ct, keys, to, v, mid, blob, conv: convIn } = await readBody(req);
      const mine = chatKeys[acct.username];
      // conv: a channel / group this person is in ('room' or nothing = # Everyone; a private message uses `to`)
      const conv = convIn && convIn !== 'room' ? String(convIn) : '';
      if (conv && (to || !guilds.isGuildConv(conv) || !guilds.isMember(conv, acct.username))) return send(res, 400, { error: 'Unknown conversation.' });
      const why = to ? '' : guilds.postCheck(conv || 'room', acct.username, { voice: blob !== undefined }); // permissions, timeout, slowmode, new-member wait
      if (why) return send(res, 403, { error: why });
      if (v !== undefined && !(FORMATS.includes(v) && MID_RE.test(mid || '') && !messages.some((x) => x.mid === mid))) return send(res, 400, { error: 'Bad message.' });
      if (blob !== undefined && !(v === 4 && b64(blob, MAX_CT))) return send(res, typeof blob === 'string' && blob.length > MAX_CT ? 413 : 400, { error: 'That voice clip is too large or malformed (up to about 2 minutes).' });
      if (!mine) return send(res, 400, { error: 'Your chat key is not set up yet.' });
      if (typeof ct === 'string' && ct.length > MAX_CT) return send(res, 413, { error: 'That message is too large (voice clips can be up to about 2 minutes).' });
      if (!b64(iv, 64) || !b64(ct, MAX_CT) || !keys || typeof keys !== 'object') return send(res, 400, { error: 'Bad message.' });
      if (to !== undefined && to !== '' && (to === acct.username || !(accounts.has(to) || (to === AGENT && agentEnabled())))) return send(res, 400, { error: 'Unknown recipient.' });
      const slots = cleanSlots(keys, acct.username, to || '', conv);
      if (!slots[acct.username]) return send(res, 400, { error: 'Bad message.' });
      const msg = { seq: ++seq, ...(v !== undefined ? { v, mid } : {}), from: acct.username, ...(to ? { to } : {}), ...(conv ? { conv } : {}), ts: new Date().toISOString(), fromPub: mine.pub, iv, ct, keys: slots, ...(blob ? { blob } : {}) };
      appendMessage(msg);
      if (!to) guilds.notePost(conv || 'room', acct.username); // slowmode
      agentHear(msg);
      return send(res, 200, { seq: msg.seq });
    }
    if (p === '/api/room/edit' && req.method === 'POST') {
      const { seq: target, iv, ct, keys, v, mid } = await readBody(req);
      const m = messages.find((x) => x.seq === target);
      if (!m || m.from !== acct.username || m.deleted) return send(res, 403, { error: 'You can only edit your own messages.' });
      const whyEdit = m.to ? '' : guilds.postCheck(m.conv || 'room', acct.username, { edit: true }); // timed out / no longer allowed to post there
      if (whyEdit) return send(res, 403, { error: whyEdit });
      const mine = chatKeys[acct.username];
      if (!mine || !b64(iv, 64) || !b64(ct, MAX_CT)) return send(res, 400, { error: 'Bad message.' });
      // an edit keeps the message id; a legacy message gets a fresh one when it is first edited in v3
      if (v !== undefined && !(FORMATS.includes(v) && MID_RE.test(mid || '') && (m.mid ? m.mid === mid : !messages.some((x) => x.mid === mid)))) return send(res, 400, { error: 'Bad message.' });
      if (m.blob && v !== 4) return send(res, 400, { error: 'Bad message.' }); // a clip's caption is edited in v4, which keeps the clip's key
      const slots = cleanSlots(keys, acct.username, m.to || '', m.conv || '');
      if (!slots[acct.username]) return send(res, 400, { error: 'Bad message.' });
      if (v !== undefined) Object.assign(m, { v, mid }); else { delete m.v; delete m.mid; }
      Object.assign(m, { iv, ct, keys: slots, fromPub: mine.pub, editedAt: new Date().toISOString(), rev: ++rev });
      saveMessages();
      return send(res, 200, { seq: m.seq, rev: m.rev });
    }
    if (p === '/api/room/delete' && req.method === 'POST') {
      const { seq: target } = await readBody(req);
      const m = messages.find((x) => x.seq === target);
      // your own, or anyone's in a server channel / VibeRoller #general where you have "manage messages"
      if (!m || (m.from !== acct.username && !(!m.to && guilds.moderates(m.conv || 'room', acct.username) && visibleTo(m, acct.username)))) return send(res, 403, { error: 'You can only delete your own messages.' });
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
      if (conv !== 'room' && !(dm && [dm[1], dm[2]].includes(acct.username)) && !guilds.isMember(conv, acct.username)) return send(res, 400, { error: 'Unknown conversation.' });
      presence.typing(acct.username, conv, !!on && (!!dm || guilds.canPost(conv, acct.username))); // no typing where you may not post
      return send(res, 200, { ok: true });
    }
    if (p === '/api/room/seen' && req.method === 'POST') {
      const { conv, seq: upTo } = await readBody(req);
      const dm = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
      if (conv !== 'room' && !(dm && [dm[1], dm[2]].includes(acct.username)) && !guilds.isMember(conv, acct.username)) return send(res, 400, { error: 'Unknown conversation.' });
      const mine = seen[acct.username] || (seen[acct.username] = {});
      const v = Math.min(Number(upTo) || 0, seq);
      if (v > (mine[conv] || 0)) { mine[conv] = v; saveSeen(); roomChanged(); }
      return send(res, 200, { ok: true });
    }
    if (await workState.route(p, req, res, acct)) return; // the Work conversation + queue, kept across restarts: /api/work/current, /api/work/queue
    if (await shares.route(p, req, res, acct, url)) return; // shared Work sessions: /api/work/*

    if (await calls.route(p, req, res, acct, url)) return; // voice / video calls: /api/call/*

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
          for (const [c, v] of Object.entries(convs)) if (c === 'room' || (c.startsWith('dm:') && c.split(/[:|]/).includes(acct.username)) || guilds.isMember(c, acct.username)) (seenOut[u] || (seenOut[u] = {}))[c] = v;
        }
        send(res, 200, { messages: out.map(strip), latest: seq, latestRev: rev, seen: seenOut, shares: shares.listFor(acct.username), ver: roomVer, keysVer, guildsVer: guilds.version(), notesVer: notes.version(),
          online: presence.online(), presence: presence.list(), typing: presence.typingFor(acct.username) });
      };
      if (url.searchParams.get('wait') !== '1') { presence.touch(acct.username); return answer(); }
      // a long poll = a connected page: online while one is open, offline shortly after the last one closes
      const stale = Number(url.searchParams.get('ver')) !== roomVer; // judged before our own coming-online bumps the version
      presence.pollOpened(acct.username);
      if (stale) { presence.pollClosed(acct.username); return answer(); } // re-opens at once with the next poll
      let done = false;
      const finish = () => { if (done) return; done = true; clearTimeout(timer); roomWaiters.delete(finish); presence.pollClosed(acct.username); answer(); };
      const timer = setTimeout(finish, 25000);
      roomWaiters.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); roomWaiters.delete(finish); presence.pollClosed(acct.username); } });
      return;
    }
    if (p === '/api/presence' && req.method === 'POST') { // the page reports where it is, whether it is idle, and when it goes away
      const { screen, idle, gone } = await readBody(req);
      if (gone) presence.gone(acct.username);
      else presence.set(acct.username, { ...(typeof screen === 'string' ? { screen: screen.slice(0, 12) } : {}), ...(idle !== undefined ? { idle: !!idle } : {}) });
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
      if (p === '/api/admin/stop' && req.method === 'POST') { const t = needTarget(); stopClaude(t); assistant.stop(t); openai.stop(t); return send(res, 200, { ok: true }); }
      if (p === '/api/admin/delete' && req.method === 'POST') {
        const t = needTarget();
        if (t === acct) return send(res, 400, { error: 'You cannot delete your own account.' });
        if (!t.profile) return send(res, 400, { error: `The "${DEFAULT_USERNAME}" account lives in bridge/ itself and cannot be deleted here.` });
        stopClaude(t); dropSessions(t.username); shares.stop(t.username); calls.leave(t.username); guilds.dropUser(t.username); avatars.remove(t.username);
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
  console.log(`[mills] VibeRoller is ALREADY RUNNING in another window (port ${config.port} is taken).`);
  console.log('[mills] Nothing to do: close this window. To restart, close every "VibeRoller" window first, then run start.bat once.');
  process.exit(2); // run.js treats 2 as "stop, do not retry"
});
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => process.exit(0));
