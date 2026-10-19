// VibeRoller AI nodes, hub side: machines that lend their local AI engines to a hub running elsewhere (the hub on
// the lab server, Unity's Ollama / image / voice / speech engines on Gee's PC). The node program is bridge/ai-node.js.
//
// A node connects OUT to the hub over HTTPS (no ports open at its home): it long-polls GET /node/poll and answers
// each job with POST /node/reply. Both carry X-VR-Node (its name) and X-VR-Node-Token (its secret), checked
// against config "aiNodes": { "<name>": "<secret, 24+ characters>" } (put it in the gitignored config.local.json).
// A job names an ENGINE (ollama / image / voice / speech) and a path, never an address: the node builds the address
// from its own config, so the hub can only ever reach the engines the node chose to lend, nothing else on its network.
// Bodies travel base64 inside JSON; answers come back whole (Unity's words arrive at the end, not word by word).
// With config "assistant": { "via": "node" } Unity's engine calls go through a node (config assistant.node picks
// one by name; else any online one).
//
// No AI host must never hold anything up: a node counts as online only while it keeps polling (every poll ends
// within POLL_MS; "aiNodeOfflineMs", default 60 s, without one = offline), a job is refused at once when no node is
// online, and a job no node picks up within PICKUP_MS fails instead of waiting. A node paused by its host (the knife,
// bridge/ai-host.off on its machine) simply stops polling. Everything else on the hub (chat, board, notes, Work's
// history, people's own AI keys) never touches a node.
'use strict';

const crypto = require('crypto');

const POLL_MS = 25000, JOB_MS = 300000, PICKUP_MS = 40000, MAX_QUEUE = 200;
const ENGINES = ['ollama', 'image', 'voice', 'speech'];

module.exports = function createAiNodes({ config, send, readBody, log = () => {} }) {
  const secrets = config.aiNodes && typeof config.aiNodes === 'object' ? config.aiNodes : {};
  const OFFLINE_MS = Math.max(1000, Number(config.aiNodeOfflineMs) || 60000);
  const nodes = new Map();   // name -> { seen, queue: [], waiters: Set(fn) }
  const pending = new Map(); // job id -> { resolve, reject, timer, pickup, node }
  const sha = (s) => crypto.createHash('sha256').update(String(s)).digest();
  function who(req) {
    const name = String(req.headers['x-vr-node'] || ''), want = secrets[name];
    if (!/^[a-z0-9-]{1,40}$/.test(name) || typeof want !== 'string' || want.length < 24) return null;
    return crypto.timingSafeEqual(sha(req.headers['x-vr-node-token'] || ''), sha(want)) ? name : null;
  }
  const node = (name) => { let n = nodes.get(name); if (!n) { n = { seen: 0, queue: [], waiters: new Set() }; nodes.set(name, n); } return n; };
  // online = a poll is open right now (the connection itself is the proof), or one ended less than OFFLINE_MS ago
  const online = (name) => { const n = nodes.get(name); return !!n && (n.waiters.size > 0 || Date.now() - n.seen < OFFLINE_MS); };
  function pick() {
    const want = config.assistant && config.assistant.node;
    if (want) return online(want) ? want : null;
    for (const name of nodes.keys()) if (online(name)) return name;
    return null;
  }
  const OFFLINE = "Unity's AI host is offline: no AI node is connected (the machine that lends her engines is off or paused). Chat, the board, notes and Work keep working; Unity is back as soon as it is.";
  const reason = () => (pick() ? '' : OFFLINE);
  // the hub's fetch for an engine: same shape as fetch(), answers a Response
  async function engineFetch(engine, path, init = {}) {
    if (!ENGINES.includes(engine)) throw new Error('Unknown engine ' + engine);
    const name = pick();
    if (!name) throw new Error(OFFLINE);
    const req = new Request('http://engine.invalid' + path, init); // turns any body (string, buffer, form) into bytes + headers
    const body = init.body !== undefined ? Buffer.from(await req.arrayBuffer()).toString('base64') : '';
    const n = node(name);
    if (n.queue.length >= MAX_QUEUE) throw new Error('The AI node is overloaded; try again in a moment.');
    const id = crypto.randomBytes(9).toString('base64url');
    const job = { id, engine, path, method: req.method, headers: { 'content-type': req.headers.get('content-type') || '' }, body };
    return new Promise((resolve, reject) => {
      const done = (e) => { const p = pending.get(id); if (!p) return; clearTimeout(p.timer); clearTimeout(p.pickup); pending.delete(id); n.queue = n.queue.filter((j) => j.id !== id); if (e) reject(e); };
      const timer = setTimeout(() => done(new Error('The AI node did not answer in time.')), JOB_MS);
      const pickup = setTimeout(() => { if (n.queue.some((j) => j.id === id)) done(new Error(OFFLINE)); }, PICKUP_MS); // never picked up: the node went away
      pending.set(id, { resolve, reject, timer, pickup, node: name });
      if (init.signal) { if (init.signal.aborted) return done(new Error('cancelled')); init.signal.addEventListener('abort', () => done(new Error('cancelled')), { once: true }); }
      n.queue.push(job);
      for (const w of [...n.waiters]) w();
    });
  }
  async function route(p, req, res) {
    if (!p.startsWith('/node/')) return false;
    const name = who(req);
    if (!name) { send(res, 401, { error: 'Unknown AI node.' }); return true; }
    const n = node(name);
    n.seen = Date.now();
    if (p === '/node/poll' && req.method === 'GET') {
      const answer = () => { const jobs = n.queue.splice(0, 8); for (const j of jobs) { const pj = pending.get(j.id); if (pj) clearTimeout(pj.pickup); } send(res, 200, { jobs }); };
      if (n.queue.length) { answer(); return true; }
      let finished = false;
      const finish = () => { if (finished) return; finished = true; clearTimeout(t); n.waiters.delete(finish); n.seen = Date.now(); answer(); };
      const t = setTimeout(finish, POLL_MS);
      n.waiters.add(finish);
      req.on('close', () => { if (!finished) { finished = true; clearTimeout(t); n.waiters.delete(finish); } });
      return true;
    }
    if (p === '/node/reply' && req.method === 'POST') {
      const b = await readBody(req), job = pending.get(String(b.id || ''));
      if (!job || job.node !== name) { send(res, 404, { error: 'No such job.' }); return true; }
      clearTimeout(job.timer); clearTimeout(job.pickup); pending.delete(b.id);
      const headers = {}; if (b.headers && typeof b.headers['content-type'] === 'string') headers['content-type'] = b.headers['content-type'];
      job.resolve(new Response(b.body ? Buffer.from(String(b.body), 'base64') : null, { status: Number(b.status) || 502, headers }));
      send(res, 200, { ok: true }); return true;
    }
    send(res, 404, { error: 'Not found' }); return true;
  }
  const status = () => [...nodes.entries()].map(([name, n]) => ({ name, online: online(name), polling: n.waiters.size > 0, seen: n.seen ? new Date(n.seen).toISOString() : null, queued: n.queue.length }));
  return { route, engineFetch, status, reason, online: () => !!pick(), ENGINES, enabled: Object.keys(secrets).length > 0 };
};
