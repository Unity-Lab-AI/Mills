// VibeRoller AI hosts, hub side. An AI host is a machine that lends AI to a hub running elsewhere (the hub on the lab
// server; Gee's PC at home): its Claude Code login for Work runs and the API, local models behind OpenAI-compatible
// endpoints (Ollama …), and Unity's engines (Ollama / image / voice / speech). The host program is bridge/ai-node.js
// ("VibeRoller AI Host"); nobody's credentials ever sit on the server: the AI runs where its owner is, and the hub relays.
//
// Identity. Every host has a UUID (its key everywhere), a secret (24+ characters) and a nickname (adjective-animal,
// e.g. silly-panda) the hub issues for people to recognise it. Hosts are made in Settings → My AI hosts (or the legacy
// config "aiNodes": { "<name>": "<secret>" }, where the name is the id). Registry: bridge/ai-hosts.json (gitignored).
// Headers: X-VR-Host (the id) + X-VR-Host-Token (the secret); the older X-VR-Node / X-VR-Node-Token still work.
//
// Connection. The host connects OUT over HTTPS (no port opens at its home):
//   POST /node/hello    its nonce + what it offers (user, providers with models / caps / allowed users, engines, version)
//                       → its nickname, the hub's nonce, the e2e key id. Both sides derive the same session key
//                       (HKDF of the secret and the two nonces); Work jobs and their events travel sealed with it
//                       (AES-256-GCM), on top of TLS, so a reverse proxy in between never sees prompts or answers.
//   GET  /node/poll     long poll for jobs (+ the ids of jobs to cancel)
//   POST /node/reply    the answer to an engine / provider job (whole, base64)
//   POST /node/events   a batch of Work events (sealed), the final one with done + exit code; the answer may say cancel
//   */node/job/<id>/internal/<tool>   the MCP tools of a Work run on the host (PC files, board, team), with the job's token
//   GET  /node/update   the host's files as this hub runs them (name + sha256), signed with the host's own secret (HMAC),
//   GET  /node/update/file/<name>      and each file: hosts that can ("caps": ["update"]) update themselves from it
//
// Older hosts keep working. Nothing new is required in a hello; extra fields in the answer are ignored by older hosts.
// A host that does not say it can relay a login (no "relay" in caps: every host before 3.1) is asked once, with a relay
// job to an address no host forwards: a 3.x host refuses it (403, nothing leaves the machine), a 2.x host does not know
// relays (404); a 2.x host's Claude then runs Work the way it did then (Claude Code on the host), instead of failing.
//
// Jobs. engine (Unity: a name + a path, never an address), provider (an OpenAI-compatible call relayed to one of the
// host's own endpoints, by provider id), work (a Claude Code run on the host, in the person's worktree there).
//
// No AI host must never hold anything up: a host counts as online only while it keeps polling (OFFLINE_MS without a
// poll = offline), a job is refused at once when its host is offline, a job nobody picks up within PICKUP_MS fails, and
// a Work run with no event for IDLE_MS is given up. Everything else on the hub never touches a host.
'use strict';

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const POLL_MS = 25000, JOB_MS = 300000, PICKUP_MS = 40000, IDLE_MS = 600000, MAX_QUEUE = 200;
const ENGINES = ['ollama', 'image', 'voice', 'speech'];
const ADJ = ['happy', 'silly', 'brave', 'quiet', 'swift', 'clever', 'gentle', 'bright', 'lucky', 'sleepy', 'bold', 'calm', 'cosmic', 'dapper', 'eager', 'fuzzy', 'golden', 'jolly', 'kind', 'merry', 'nimble', 'proud', 'rusty', 'shiny', 'sunny', 'tidy', 'witty', 'zesty', 'mellow', 'plucky'];
const ANIMAL = ['panda', 'crocodile', 'otter', 'falcon', 'badger', 'walrus', 'lynx', 'heron', 'gecko', 'koala', 'marmot', 'narwhal', 'ocelot', 'puffin', 'quokka', 'raccoon', 'sparrow', 'tapir', 'urchin', 'viper', 'wombat', 'yak', 'zebra', 'beaver', 'dolphin', 'ferret', 'ibis', 'jackal', 'kiwi', 'moose'];
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const NICK_RE = /^[a-z0-9]+(-[a-z0-9]+){1,3}$/;
// what a host runs (bridge/ai-node.js and what it loads); launchers (.bat / .sh / the service) are never swapped while running
const HOST_FILES = ['ai-node.js', 'host-relay.js', 'common.js', 'worktree.js', 'pc-tools-mcp.js', 'board-mcp.js', 'team-mcp.js'];

// onUsage(source, windows) / savedUsage(source): a host provider's 5h / 7d windows are kept by bridge/usage.js (on disk), so
// they survive a hub restart and show in /api/usage; source = "h:<hostId>/<providerId>".
// release() = { version, commit } of the code this hub runs (what /node/update hands to hosts).
module.exports = function createAiNodes({ ROOT, config, send, readBody, readJson, writeJson, accounts = null, log = () => {}, onUsage = () => {}, savedUsage = () => null, release = () => ({ version: '', commit: '' }) }) {
  const FILE = path.join(ROOT, 'ai-hosts.json');
  const OFFLINE_MS = Math.max(1000, Number(config.aiNodeOfflineMs) || 60000);
  const db = readJson(FILE, { hosts: {} }); if (!db.hosts || typeof db.hosts !== 'object') db.hosts = {};
  const save = () => writeJson(FILE, db);
  // legacy static hosts from config: id = name, no owner, nickname = name
  const legacy = Object.entries(config.aiNodes && typeof config.aiNodes === 'object' ? config.aiNodes : {}).filter(([n, s]) => /^[a-z0-9-]{1,40}$/.test(n) && typeof s === 'string' && s.length >= 24)
    .map(([n, s]) => [n, { id: n, user: '', nickname: n, secret: s, legacy: true, createdAt: null, share: {} }]);
  const record = (id) => db.hosts[id] || legacy.find(([n]) => n === id)?.[1] || null;
  const allRecords = () => [...Object.values(db.hosts), ...legacy.map(([, r]) => r)];
  const sha = (s) => crypto.createHash('sha256').update(String(s)).digest();
  const sameSecret = (a, b) => crypto.timingSafeEqual(sha(a || ''), sha(b || ''));

  // ---------- nicknames ----------
  function newNickname() {
    const taken = new Set(allRecords().map((r) => r.nickname));
    for (let i = 0; i < 200; i++) { const n = ADJ[crypto.randomInt(ADJ.length)] + '-' + ANIMAL[crypto.randomInt(ANIMAL.length)]; if (!taken.has(n)) return n; }
    return ADJ[crypto.randomInt(ADJ.length)] + '-' + ANIMAL[crypto.randomInt(ANIMAL.length)] + '-' + crypto.randomInt(100);
  }

  // ---------- live state per host ----------
  const live = new Map(); // id -> { seen, queue: [], waiters: Set, cancel: Set, hello: {...}|null, key: Buffer|null, kid: '' , usage: {} }
  const state = (id) => { let n = live.get(id); if (!n) { n = { seen: 0, queue: [], waiters: new Set(), cancel: new Set(), hello: null, key: null, kid: '', usage: {}, relayOk: {} }; live.set(id, n); } return n; };
  const online = (id) => { const n = live.get(id); return !!n && (n.waiters.size > 0 || Date.now() - n.seen < OFFLINE_MS); };
  // A host's session (its hello and the session key) survives a hub restart: kept in bridge/ai-host-sessions.json
  // (gitignored; the key is derived from the host's secret, which ai-hosts.json holds anyway). Without this every
  // restart or deploy made the hub forget what each host offers, while the host, still holding its key, kept polling
  // without ever saying hello again: its Claude vanished for everyone until someone restarted the host by hand.
  const SESSIONS = path.join(ROOT, 'ai-host-sessions.json');
  function restoreSessions() { // at start, once the key helpers below exist
    for (const [id, s] of Object.entries(readJson(SESSIONS, {}) || {})) {
      if (!record(id) || !s || !s.hello || !/^[0-9a-f]{64}$/.test(String(s.key || ''))) continue;
      const n = state(id); n.hello = s.hello; n.key = Buffer.from(s.key, 'hex'); n.kid = keyId(n.key); n.relayOk = s.relayOk || {}; n.seen = Date.now(); // counts as online for a minute: it is polling again by then
    }
  }
  let sessionsTimer = null;
  const saveSessions = () => { clearTimeout(sessionsTimer); sessionsTimer = setTimeout(() => { const o = {}; for (const [id, n] of live) if (n.hello && n.key && record(id)) o[id] = { hello: n.hello, key: n.key.toString('hex'), relayOk: n.relayOk || {} }; writeJson(SESSIONS, o); }, 500); };
  // up / down, both ways: a host is up while it keeps polling (every POLL_MS); the hub logs each change with how long it
  // lasted, and every poll answer carries the hub's heartbeat (time, version, commit) so a host sees the hub the same way
  const wasUp = new Map(); // id -> { up, since }
  setInterval(() => {
    for (const r of allRecords()) {
      const up = online(r.id), prev = wasUp.get(r.id);
      if (!prev) { if (live.has(r.id)) wasUp.set(r.id, { up, since: Date.now() }); continue; }
      if (prev.up !== up) { log(`[ai-host] ${r.nickname} is ${up ? 'back' : 'OFFLINE'} (${up ? 'away' : 'up'} for ${Math.round((Date.now() - prev.since) / 1000)} s)`); wasUp.set(r.id, { up, since: Date.now() }); }
    }
  }, 5000).unref();
  const pending = new Map(); // job id -> { kind, host, resolve, reject, timer, pickup, idle, token, account, onEvent, onDone }

  // ---------- e2e: a session key per hello, derived by both sides from the secret and the two nonces ----------
  const derive = (secret, hostNonce, serverNonce) => Buffer.from(crypto.hkdfSync('sha256', Buffer.from(String(secret), 'utf8'), Buffer.from(hostNonce + serverNonce, 'hex'), 'viberoller-ai-host-v1', 32));
  const keyId = (key) => 'vr-' + crypto.createHash('sha256').update(key).digest('hex').slice(0, 24);
  function seal(key, obj) { const iv = crypto.randomBytes(12), c = crypto.createCipheriv('aes-256-gcm', key, iv); const ct = Buffer.concat([c.update(JSON.stringify(obj), 'utf8'), c.final(), c.getAuthTag()]); return { iv: iv.toString('base64'), ct: ct.toString('base64') }; }
  function open(key, enc) {
    const iv = Buffer.from(String(enc.iv || ''), 'base64'), ct = Buffer.from(String(enc.ct || ''), 'base64');
    if (iv.length !== 12 || ct.length < 16) throw new Error('bad envelope');
    const d = crypto.createDecipheriv('aes-256-gcm', key, iv); d.setAuthTag(ct.subarray(ct.length - 16));
    return JSON.parse(Buffer.concat([d.update(ct.subarray(0, ct.length - 16)), d.final()]).toString('utf8'));
  }
  restoreSessions();

  // ---------- who is calling ----------
  function who(req) {
    const id = String(req.headers['x-vr-host'] || req.headers['x-vr-node'] || ''), token = String(req.headers['x-vr-host-token'] || req.headers['x-vr-node-token'] || '');
    const r = record(id);
    if (!r || !token) return null;
    return sameSecret(token, r.secret) ? r : null;
  }

  // ---------- what hosts offer ----------
  const cleanModels = (m) => (Array.isArray(m) ? m : []).map((s) => String(s).trim()).filter((s) => /^[\w.:/-]{1,80}$/.test(s)).slice(0, 30);
  const cleanUsers = (u) => (Array.isArray(u) ? u : []).map((s) => String(s).trim().toLowerCase()).filter((s) => /^[a-z0-9-]{1,32}$/.test(s)).slice(0, 50);
  const pct = (v, d) => { const n = Number(v); return Number.isFinite(n) && n > 0 && n <= 100 ? Math.round(n) : d; };
  function cleanProviders(list) {
    const out = [];
    for (const p of Array.isArray(list) ? list.slice(0, 20) : []) {
      const id = String(p.id || '').trim(), kind = String(p.kind || '');
      if (!/^[a-z0-9-]{1,32}$/.test(id) || !['claude-cli', 'claude-relay', 'codex-relay', 'anthropic', 'openai'].includes(kind) || out.some((x) => x.id === id)) continue;
      out.push({ id, kind, label: String(p.label || id).trim().slice(0, 60) || id, models: cleanModels(p.models), allowedUsers: cleanUsers(p.allowedUsers), max5h: pct(p.max5h, 90), max7d: pct(p.max7d, 90) });
    }
    return out;
  }
  // effective sharing: the owner's choice in Settings (record.share[providerId]) wins over what the host declared
  const allowed = (r, prov) => (r.share && Array.isArray(r.share[prov.id]) ? r.share[prov.id] : prov.allowedUsers);
  // whose host: the record's owner (hosts made in Settings); a legacy config host (no owner) is whose it says in hello
  const ownerOf = (r) => r.user || (live.get(r.id) && live.get(r.id).hello ? live.get(r.id).hello.user : '') || '';
  const mayUse = (r, prov, user) => !!user && (user === ownerOf(r) || allowed(r, prov).includes(user));
  const usageOf = (n, prov) => { // what this hub saw since it started, else the windows kept on disk (a restart forgets nothing)
    const seen = n.usage[prov.id] || {}, kept = savedUsage(`h:${hostIdOf(n)}/${prov.id}`) || {};
    const pick = (k) => (seen[k] && seen[k].utilization != null ? seen[k] : kept[k] || seen[k] || null);
    return { five_hour: pick('five_hour'), seven_day: pick('seven_day') };
  };
  const hostIdOf = (n) => { for (const [id, x] of live) if (x === n) return id; return ''; };
  const overCap = (n, prov) => { const u = usageOf(n, prov); const pc = (w) => (w && Number.isFinite(w.utilization) && (!w.resetsAt || w.resetsAt > Date.now()) ? w.utilization * 100 : 0); return pc(u.five_hour) >= prov.max5h ? `its 5-hour usage is at the ${prov.max5h}% cap its host set` : pc(u.seven_day) >= prov.max7d ? `its weekly usage is at the ${prov.max7d}% cap its host set` : ''; };
  // hub-side kind: a host's Claude (login relay) runs Work on THIS hub with the host's login; Codex and endpoints answer the API
  const hubKind = (k) => (k === 'openai' ? 'host-openai' : k === 'codex-relay' ? 'host-codex' : 'host-claude');
  // the providers people can pick: id "h:<hostId>/<providerId>"; offline hosts stay listed (greyed), so a choice survives
  function providersFor(user) {
    const out = [];
    for (const r of allRecords()) {
      const n = live.get(r.id); if (!n || !n.hello) continue;
      for (const p of n.hello.providers) {
        if (!mayUse(r, p, user)) continue;
        out.push({ id: `h:${r.id}/${p.id}`, kind: hubKind(p.kind), remoteKind: remoteKindOf(n, p), label: p.label, owner: ownerOf(r), shared: true, models: p.models, hostId: r.id, nickname: r.nickname, online: online(r.id), max5h: p.max5h, max7d: p.max7d, usage: usageOf(n, p), capped: overCap(n, p) });
      }
    }
    return out;
  }
  function provider(id, user) {
    const m = /^h:([^/]+)\/([a-z0-9-]+)$/.exec(String(id || '')); if (!m) return null;
    const r = record(m[1]), n = live.get(m[1]); if (!r || !n || !n.hello) return null;
    const p = n.hello.providers.find((x) => x.id === m[2]); if (!p || (user && !mayUse(r, p, user))) return null;
    return { id, kind: hubKind(p.kind), remoteKind: remoteKindOf(n, p), label: p.label, owner: ownerOf(r), shared: true, models: p.models, hostId: r.id, providerId: p.id, nickname: r.nickname, online: online(r.id), max5h: p.max5h, max7d: p.max7d, usage: usageOf(n, p), capped: overCap(n, p) };
  }
  // ---------- older hosts: can this one relay a login, or does its Claude run Work itself (2.x)? ----------
  // A relay job to a path no host forwards: 3.x refuses it (403, nothing leaves that machine), 2.x knows no relays (404).
  async function probeRelays(id) {
    const n = live.get(id); if (!n || !n.hello) return;
    for (const p of n.hello.providers.filter((x) => x.kind === 'claude-cli' || x.kind === 'claude-relay')) {
      try {
        const res = await relay(id, { engine: 'relay:' + p.id, path: '/v1/__viberoller_probe' }, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' }, 'provider', true);
        const text = await res.text().catch(() => '');
        n.relayOk[p.id] = !(res.status === 404 && /does not lend/i.test(text)); saveSessions();
        if (!n.relayOk[p.id]) log(`[ai-host] ${(record(id) || {}).nickname || id} is an older host (no login relay): its Claude runs Work on that machine, as it did before`);
      } catch {} // offline or slow: asked again at its next hello
    }
  }
  // the kind the hub uses: a 2.x host's Claude runs Work itself ("claude-cli-work", the legacy path in server.js)
  const remoteKindOf = (n, p) => (p.kind === 'claude-cli' && n.relayOk && n.relayOk[p.id] === false ? 'claude-cli-work' : p.kind);

  // ---------- updates for hosts: the files a host runs, as this hub runs them ----------
  let manifestCache = { stamp: '', m: null };
  function updateManifest() {
    const rel = release();
    const stats = HOST_FILES.map((f) => { try { const s = fs.statSync(path.join(ROOT, f)); return `${f}:${s.mtimeMs}:${s.size}`; } catch { return f + ':-'; } }).join('|');
    if (manifestCache.stamp !== stats + rel.commit) {
      const files = HOST_FILES.filter((f) => fs.existsSync(path.join(ROOT, f))).map((f) => { const b = fs.readFileSync(path.join(ROOT, f)); return { name: f, sha256: crypto.createHash('sha256').update(b).digest('hex'), size: b.length }; });
      manifestCache = { stamp: stats + rel.commit, m: { version: String(rel.version || ''), commit: String(rel.commit || ''), files } };
    }
    return manifestCache.m && manifestCache.m.files.length ? manifestCache.m : null;
  }
  const updateInfo = () => { const rel = release(); return { version: String(rel.version || ''), commit: String(rel.commit || '') }; };

  function noteHostUsage(id, providerId, usage) { // the host reports its own windows with every event batch
    const n = state(id); if (!usage || typeof usage !== 'object') return;
    const num = (v) => (v == null || v === '' ? NaN : Number(v)); // a missing number is unknown, not 0 %
    const clean = (w) => (w && typeof w === 'object' ? { utilization: Number.isFinite(num(w.utilization)) ? Math.max(0, Math.min(1, num(w.utilization) > 1 ? num(w.utilization) / 100 : num(w.utilization))) : null, resetsAt: Number(w.resetsAt) || null, status: String(w.status || '') } : null);
    n.usage[providerId] = { five_hour: clean(usage.five_hour), seven_day: clean(usage.seven_day), at: Date.now() };
    onUsage(`h:${id}/${providerId}`, n.usage[providerId]);
  }

  // ---------- Settings → My AI hosts ----------
  const pubHost = (r) => { const n = live.get(r.id) || null; return { id: r.id, nickname: r.nickname, user: r.user, legacy: !!r.legacy, createdAt: r.createdAt, online: online(r.id), seen: n && n.seen ? new Date(n.seen).toISOString() : null, kid: n ? n.kid : '', version: n && n.hello ? n.hello.version : '', os: n && n.hello ? n.hello.os : '', engines: n && n.hello ? n.hello.engines : [], providers: n && n.hello ? n.hello.providers.map((p) => ({ ...p, allowedUsers: allowed(r, p), declared: p.allowedUsers, usage: usageOf(n, p), capped: overCap(n, p) })) : [] }; };
  const hosts = {
    mine: (user) => Object.values(db.hosts).filter((r) => r.user === user).map(pubHost),
    all: () => allRecords().map(pubHost),
    get: (id) => (record(id) ? pubHost(record(id)) : null),
    create(user, nickname) {
      if (!/^[a-z0-9-]{1,32}$/.test(user || '')) throw new Error('Unknown account.');
      if (Object.values(db.hosts).filter((r) => r.user === user).length >= 10) throw new Error('Ten hosts per person is plenty; remove one first.');
      const id = crypto.randomUUID(), nick = nickname ? hosts.checkNick(nickname) : newNickname();
      db.hosts[id] = { id, user, nickname: nick, secret: crypto.randomBytes(24).toString('hex'), createdAt: new Date().toISOString(), share: {} };
      save(); return pubHost(db.hosts[id]);
    },
    checkNick(nick) { nick = String(nick || '').trim().toLowerCase(); if (!NICK_RE.test(nick) || nick.length > 40) throw new Error('A nickname is two to four lowercase words joined by dashes, like silly-panda.'); if (allRecords().some((r) => r.nickname === nick)) throw new Error('That nickname is taken.'); return nick; },
    rename(user, id, nickname) { const r = db.hosts[id]; if (!r || r.user !== user) throw new Error('Not your host.'); r.nickname = hosts.checkNick(nickname); save(); return pubHost(r); },
    remove(user, id) { const r = db.hosts[id]; if (!r || r.user !== user) throw new Error('Not your host.'); delete db.hosts[id]; live.delete(id); save(); return true; },
    setShare(user, id, providerId, users) { const r = db.hosts[id]; if (!r || r.user !== user) throw new Error('Not your host.'); if (!/^[a-z0-9-]{1,32}$/.test(providerId || '')) throw new Error('Unknown provider.'); r.share = r.share || {}; if (users === null) delete r.share[providerId]; else r.share[providerId] = cleanUsers(users); save(); return pubHost(r); },
    // the host program's config, filled in for one of the person's hosts (goes into the AI Host download, never shown twice casually)
    configFor(user, id, hub) {
      const r = db.hosts[id]; if (!r || r.user !== user) throw new Error('Not your host.');
      return { _: `VibeRoller AI Host "${r.nickname}" of ${user}: made in Settings → My AI hosts. Keep this file private: the token is this host's key to the hub.`, hub, id: r.id, nickname: r.nickname, user, token: r.secret,
        providers: [{ id: 'claude', kind: 'claude-relay', label: `${user}'s Claude`, models: (config.models || []).map((m) => m.id), max5h: 90, max7d: 90, allowedUsers: cleanUsers(config.hostGroup || []) },
          { id: 'codex', kind: 'codex-relay', label: `${user}'s ChatGPT / Codex`, models: ['gpt-5.5'], allowedUsers: cleanUsers(config.hostGroup || []) }],
        engines: { ollama: 'http://127.0.0.1:11434', image: 'http://127.0.0.1:5124', voice: 'http://127.0.0.1:5123', speech: 'http://127.0.0.1:5125' } };
    },
  };

  // ---------- jobs ----------
  function pickEngineHost(engine) {
    const want = config.assistant && config.assistant.node;
    if (want) return online(want) && (live.get(want).hello ? live.get(want).hello.engines.includes(engine) : true) ? want : null;
    for (const [id, n] of live) if (online(id) && (!n.hello || n.hello.engines.includes(engine))) return id;
    return null;
  }
  const OFFLINE = "Unity's AI host is offline: no AI host that lends her engines is connected (the machine is off or paused). Chat, the board, notes and Work keep working; Unity is back as soon as it is.";
  const reason = () => (pickEngineHost('ollama') ? '' : OFFLINE);
  function enqueue(id, job, entry) { // a job for one host; entry = pending record; fails if never picked up
    const n = state(id);
    if (n.queue.length >= MAX_QUEUE) throw new Error('That AI host is overloaded; try again in a moment.');
    entry.host = id;
    entry.pickup = setTimeout(() => { if (n.queue.some((j) => j.id === job.id)) finish(job.id, new Error(entry.kind === 'work' ? `${n.hello ? n.hello.nickname || id : id} did not pick the job up: the host is away.` : OFFLINE)); }, PICKUP_MS);
    pending.set(job.id, entry);
    n.queue.push(job);
    for (const w of [...n.waiters]) w();
  }
  function finish(jobId, err, result) {
    const p = pending.get(jobId); if (!p) return;
    clearTimeout(p.timer); clearTimeout(p.pickup); clearTimeout(p.idle); pending.delete(jobId);
    const n = live.get(p.host); if (n) { n.queue = n.queue.filter((j) => j.id !== jobId); n.cancel.delete(jobId); }
    if (p.kind === 'work') { if (err) p.onError(err.message); else p.onDone(result); return; }
    if (p.controller) { try { if (err) p.controller.error(err); else p.controller.close(); } catch {} return; } // a streamed answer: the Response was handed out at its first chunk
    if (err) p.reject(err); else p.resolve(result);
  }
  // Unity's engines: same shape as fetch(), answers a Response
  async function engineFetch(engine, path, init = {}) {
    if (!ENGINES.includes(engine)) throw new Error('Unknown engine ' + engine);
    const id = pickEngineHost(engine);
    if (!id) throw new Error(OFFLINE);
    return relay(id, { engine, path }, init, 'engine');
  }
  // an OpenAI-compatible call to one of a host's own endpoints (its local models), by provider id
  async function providerFetch(hostId, providerId, path, init = {}) {
    if (!online(hostId)) throw Object.assign(new Error(`That AI host (${(record(hostId) || {}).nickname || hostId}) is offline right now.`), { status: 503 });
    return relay(hostId, { engine: 'provider:' + providerId, path }, init, 'provider');
  }
  // A login relay: a model call of this hub's own Claude Code / Codex, carried to the host that adds its login (never the
  // caller's headers for auth: the host replaces them). Answers stream back. Usage headers feed the host's caps here.
  async function loginFetch(hostId, providerId, path, init = {}) {
    if (!online(hostId)) throw Object.assign(new Error(`That AI host (${(record(hostId) || {}).nickname || hostId}) is offline right now.`), { status: 503 });
    const n = live.get(hostId), prov = n && n.hello ? n.hello.providers.find((p) => p.id === providerId) : null;
    if (prov) { const cap = overCap(n, prov); if (cap) throw Object.assign(new Error(`${(record(hostId) || {}).nickname}: ${cap}.`), { status: 429 }); }
    const res = await relay(hostId, { engine: 'relay:' + providerId, path }, init, 'provider', true);
    noteHeaders(hostId, providerId, res.headers);
    return res;
  }
  function noteHeaders(hostId, providerId, h) {
    const w = (name) => { const u = h.get(`anthropic-ratelimit-unified-${name}-utilization`), rs = h.get(`anthropic-ratelimit-unified-${name}-reset`); return u == null ? null : { utilization: Number(u), resetsAt: rs ? Number(rs) * 1000 : null, status: h.get(`anthropic-ratelimit-unified-${name}-status`) || '' }; };
    const five = w('5h'), seven = w('7d');
    if (five || seven) { const n = state(hostId), prev = n.usage[providerId] || {}; noteHostUsage(hostId, providerId, { five_hour: five || prev.five_hour, seven_day: seven || prev.seven_day }); }
  }
  const PASS_RES = /^(content-type|request-id|anthropic-ratelimit-[\w-]+|retry-after|x-codex-[\w-]+|openai-[\w-]+)$/i;
  const pickHeaders = (src) => { const o = {}; for (const [k, v] of Object.entries(src && typeof src === 'object' ? src : {})) if (PASS_RES.test(k) && typeof v === 'string') o[k.toLowerCase()] = v; return o; };
  async function relay(id, target, init, kind, allHeaders = false) {
    const req = new Request('http://engine.invalid' + target.path, init); // turns any body (string, buffer, form) into bytes + headers
    const body = init.body !== undefined ? Buffer.from(await req.arrayBuffer()).toString('base64') : '';
    const jobId = crypto.randomBytes(9).toString('base64url');
    const hdrs = { 'content-type': req.headers.get('content-type') || '' };
    if (allHeaders) req.headers.forEach((v, k) => { if (!/^(authorization|x-api-key|host|content-length|connection|cookie)$/i.test(k)) hdrs[k] = v; }); // the host keeps only what the provider needs
    const job = { id: jobId, kind, engine: target.engine, path: target.path, method: req.method, headers: hdrs, body };
    return new Promise((resolve, reject) => {
      const entry = { kind, resolve, reject, timer: setTimeout(() => finish(jobId, new Error('The AI host did not answer in time.')), JOB_MS) };
      if (init.signal) { if (init.signal.aborted) return reject(new Error('cancelled')); init.signal.addEventListener('abort', () => finish(jobId, new Error('cancelled')), { once: true }); }
      try { enqueue(id, job, entry); } catch (e) { clearTimeout(entry.timer); reject(e); }
    });
  }
  // A Work run on a host: Claude Code runs there, in that person's worktree of the project; events stream back.
  // payload: everything the host needs (account, display, project, branch, repoUrl, message, systemPrompt, model, resume,
  // allowedTools, permissionMode, env, sandbox); handlers: onEvent(ev), onDone({ code }), onError(text). Returns { id, cancel }.
  function runWork(hostId, providerId, payload, handlers) {
    const r = record(hostId), n = live.get(hostId);
    if (!r || !n || !n.hello || !n.key) throw new Error('That AI host has not connected yet.');
    if (!online(hostId)) throw new Error(`${r.nickname}${ownerOf(r) ? ` (${ownerOf(r)}'s AI host)` : ' (AI host)'} is offline right now. Pick another AI in the dropdown, or wait for it.`);
    const prov = n.hello.providers.find((p) => p.id === providerId);
    if (!prov) throw new Error('That host no longer offers this provider.');
    const cap = overCap(n, prov); if (cap) throw new Error(`${r.nickname}: ${cap}. Pick another AI or wait for the window to reset.`);
    const id = crypto.randomBytes(9).toString('base64url'), token = crypto.randomBytes(24).toString('hex');
    const job = { id, kind: 'work', enc: seal(n.key, { ...payload, providerId, token, mcpBase: `${payload.hubUrl}/node/job/${id}` }) };
    const entry = { kind: 'work', token, account: payload.account, display: payload.display, providerId, onEvent: handlers.onEvent, onDone: handlers.onDone, onError: handlers.onError, timer: null,
      idle: setTimeout(() => finish(id, new Error('No word from the AI host for ten minutes; the run was given up.')), IDLE_MS) };
    enqueue(hostId, job, entry);
    return { id, cancel: () => { const p = pending.get(id); if (!p) return; n.cancel.add(id); for (const w of [...n.waiters]) w(); } };
  }
  const jobFor = (id, token) => { const p = pending.get(String(id || '')); return p && p.kind === 'work' && token && sameSecret(token, p.token) ? { account: p.account, display: p.display } : null; };
  // Direct computer access: a person's own AI host that announced "pc" (the native app with a folder on their machine)
  // answers the PC tools of their Work runs instead of their browser.
  const pcHostFor = (user) => { for (const r of allRecords()) { const n = live.get(r.id); if (user && ownerOf(r) === user && n && n.hello && n.hello.pc && online(r.id)) return r.id; } return null; };
  async function pcTool(hostId, name, args) {
    const r = await relay(hostId, { engine: 'pc', path: '/' + String(name || '') }, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(args || {}) }, 'pc');
    return r.json();
  }

  // ---------- routes ----------
  async function route(p, req, res) {
    if (!p.startsWith('/node/')) return false;
    if (p.startsWith('/node/job/')) return false; // the MCP relay of a Work run: the hub handles it with the job's token (server.js)
    const r = who(req);
    if (!r) { send(res, 401, { error: 'Unknown AI host.' }); return true; }
    const n = state(r.id);
    n.seen = Date.now();
    if (p === '/node/hello' && req.method === 'POST') {
      const b = await readBody(req);
      const hostNonce = String(b.nonce || '');
      if (!/^[0-9a-f]{32,64}$/.test(hostNonce)) { send(res, 400, { error: 'hello needs a hex nonce (32+ characters).' }); return true; }
      const serverNonce = crypto.randomBytes(16).toString('hex');
      n.key = derive(r.secret, hostNonce, serverNonce); n.kid = keyId(n.key);
      const caps = (Array.isArray(b.caps) ? b.caps : []).map(String).filter((c) => /^[a-z-]{1,20}$/.test(c)).slice(0, 20);
      n.hello = { at: Date.now(), nickname: r.nickname, user: r.user || String(b.user || ''), providers: cleanProviders(b.providers), engines: (Array.isArray(b.engines) ? b.engines : Object.keys(b.engines || {})).filter((e) => ENGINES.includes(e)), version: String(b.version || '').slice(0, 20), os: String(b.os || '').slice(0, 20), pc: b.pc === true && !!(r.user || /^[a-z0-9-]{1,32}$/.test(String(b.user || ''))), caps };
      n.relayOk = {}; saveSessions();
      log(`[ai-host] ${r.nickname} (${n.hello.user || 'no owner'}) connected: ${n.hello.providers.length} provider(s), engines ${n.hello.engines.join(', ') || 'none'}, key ${n.kid}${caps.length ? ', can ' + caps.join(' ') : ', an older host'}`);
      // extra fields are safe: every host so far reads only the ones it knows
      send(res, 200, { id: r.id, nickname: r.nickname, user: r.user, serverNonce, kid: n.kid, pollMs: POLL_MS, offlineMs: OFFLINE_MS, group: cleanUsers(config.hostGroup || []), update: updateInfo() });
      if (!caps.includes('relay')) setTimeout(() => probeRelays(r.id), 1500); // an older host: can it relay a login, or only run Work itself?
      return true;
    }
    if (p === '/node/update' && req.method === 'GET') { // the host's files as this hub runs them, signed with the host's own secret
      const m = updateManifest();
      if (!m) { send(res, 404, { error: 'No updates here.' }); return true; }
      send(res, 200, { ...m, sig: crypto.createHmac('sha256', String(r.secret)).update(JSON.stringify(m)).digest('hex') });
      return true;
    }
    if (p.startsWith('/node/update/file/') && req.method === 'GET') {
      const name = p.slice('/node/update/file/'.length);
      if (!HOST_FILES.includes(name)) { send(res, 404, { error: 'Not a host file.' }); return true; }
      let buf; try { buf = fs.readFileSync(path.join(ROOT, name)); } catch { send(res, 404, { error: 'Not here.' }); return true; }
      res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': buf.length, 'Cache-Control': 'no-store' }); res.end(buf);
      return true;
    }
    if (p === '/node/poll' && req.method === 'GET') {
      // a host polling without a session here (its session could not be restored): ask it to say hello again. Hosts
      // from 3.3 on (they send X-VR-Host-Version) re-hello at once on 428; older ones do it on 401, 30 s later.
      // (1.x hosts never say hello and send only X-VR-Node headers: they keep polling for engine jobs as before)
      if (!n.hello && req.headers['x-vr-host']) { send(res, req.headers['x-vr-host-version'] ? 428 : 401, { error: 'Say hello again: this hub does not know what you offer yet.', rehello: true }); return true; }
      const beat = () => { const rel = release(); return { t: Date.now(), version: String(rel.version || ''), commit: String(rel.commit || '') }; };
      const answer = () => { const jobs = n.queue.splice(0, 8); for (const j of jobs) { const pj = pending.get(j.id); if (pj) clearTimeout(pj.pickup); } send(res, 200, { jobs, cancel: [...n.cancel], beat: beat() }); n.cancel.clear(); };
      if (n.queue.length || n.cancel.size) { answer(); return true; }
      let finished = false;
      const fin = () => { if (finished) return; finished = true; clearTimeout(t); n.waiters.delete(fin); n.seen = Date.now(); answer(); };
      const t = setTimeout(fin, POLL_MS);
      n.waiters.add(fin);
      req.on('close', () => { if (!finished) { finished = true; clearTimeout(t); n.waiters.delete(fin); } });
      return true;
    }
    if (p === '/node/reply' && req.method === 'POST') { // a whole answer in one go
      const b = await readBody(req), job = pending.get(String(b.id || ''));
      if (!job || job.host !== r.id || job.kind === 'work' || job.controller) { send(res, 404, { error: 'No such job.' }); return true; }
      const headers = pickHeaders(b.headers);
      finish(b.id, null, new Response(b.body ? Buffer.from(String(b.body), 'base64') : null, { status: Number(b.status) || 502, headers }));
      send(res, 200, { ok: true }); return true;
    }
    if (p === '/node/stream' && req.method === 'POST') { // an answer in pieces, as the host's engine produces them: words arrive as they are made
      const b = await readBody(req), job = pending.get(String(b.id || ''));
      if (!job || job.host !== r.id || job.kind === 'work') { send(res, 404, { error: 'No such job.' }); return true; }
      if (!job.controller) { // the first piece carries the status + headers: the Response goes out now, its body fills up behind it
        const headers = pickHeaders(b.headers);
        let ctl = null;
        const body = new ReadableStream({ start(c) { ctl = c; }, cancel() { finish(String(b.id), new Error('cancelled')); } });
        job.controller = ctl; clearTimeout(job.pickup);
        job.resolve(new Response(body, { status: Number(b.status) || 502, headers }));
      }
      if (b.chunk) { try { job.controller.enqueue(new Uint8Array(Buffer.from(String(b.chunk), 'base64'))); } catch {} }
      if (b.done) finish(String(b.id), b.error ? new Error(String(b.error)) : null);
      send(res, 200, { ok: true }); return true;
    }
    if (p === '/node/events' && req.method === 'POST') {
      const b = await readBody(req), job = pending.get(String(b.job || ''));
      if (!job || job.host !== r.id || job.kind !== 'work') { send(res, 404, { error: 'No such job.' }); return true; }
      let batch; try { batch = open(n.key, b.enc || {}); } catch { send(res, 400, { error: 'Could not open the envelope (the keys differ: say hello again).' }); return true; }
      clearTimeout(job.idle); job.idle = setTimeout(() => finish(job.id || b.job, new Error('No word from the AI host for ten minutes; the run was given up.')), IDLE_MS);
      if (batch.usage) noteHostUsage(r.id, job.providerId, batch.usage);
      for (const ev of Array.isArray(batch.events) ? batch.events : []) { try { job.onEvent(ev); } catch {} }
      const cancelled = n.cancel.has(String(b.job));
      if (batch.done) finish(String(b.job), null, { code: Number(batch.code) || 0 });
      send(res, 200, { ok: true, cancel: cancelled }); return true;
    }
    send(res, 404, { error: 'Not found' }); return true;
  }
  const status = () => allRecords().map((r) => { const n = live.get(r.id); return { id: r.id, name: r.nickname, nickname: r.nickname, user: r.user, online: online(r.id), polling: !!n && n.waiters.size > 0, seen: n && n.seen ? new Date(n.seen).toISOString() : null, queued: n ? n.queue.length : 0, engines: n && n.hello ? n.hello.engines : [], providers: n && n.hello ? n.hello.providers.map((p) => p.id) : [], kid: n ? n.kid : '', version: n && n.hello ? n.hello.version : '', since: (wasUp.get(r.id) || {}).since ? new Date(wasUp.get(r.id).since).toISOString() : null }; });
  const pubHostFlags = (r) => { const n = live.get(r.id); return n && n.hello ? { pc: !!n.hello.pc } : { pc: false }; };
  return { route, engineFetch, providerFetch, loginFetch, runWork, jobFor, pcHostFor, pcTool, status, reason, online: () => !!pickEngineHost('ollama'), hostOnline: online, providersFor, provider, hosts, hostFlags: pubHostFlags, ENGINES, enabled: allRecords().length > 0, FILE };
};
