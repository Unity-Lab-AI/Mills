// The AI hosts when the Rust hub in front runs them (its aihosts module, native/crates/vr-hub/src/modules/ai_hosts.rs):
// the same object bridge/ai-nodes.js makes, for the rest of this hub (Work runs, the API, the chat, Unity), without its
// own registry, sessions or routes. The hosts connect to the front; this hub
//   - knows what they offer from the front's snapshot (POST /internal/front/hosts-state, on every change and every 5 s;
//     asked for once at start): providers, who may use them, online, caps. Online is counted here from the host's last
//     poll, so it never lags behind the snapshot.
//   - carries its calls to them through the front: /__front/ai/fetch (Unity's engines, a host's endpoint, a host's
//     login; the answer streams back as it comes), /__front/ai/work (a Work run: one JSON line per event),
//     /__front/ai/cancel, /__front/ai/pc-tool.
// /api/hosts/new | rename | remove | share | config and every /node/* route are the front's; /node/job/<id>/internal/<tool>
// comes here with the account the front checked the job's token for.
'use strict';
const fs = require('fs');
const path = require('path');

const ENGINES = ['ollama', 'image', 'voice', 'speech'];
const OFFLINE = "Unity's AI host is offline: no AI host that lends her engines is connected (the machine is off or paused). Chat, the board, notes and Work keep working; Unity is back as soon as it is.";
const FRONT_HEADERS = /^(x-vr-front|x-content-type-options|referrer-policy|x-frame-options|cache-control|content-length|transfer-encoding|connection|keep-alive)$/i;

module.exports = function createAiNodesFront({ ROOT, config, port, secret, log = () => {} }) {
  const FILE = path.join(ROOT, 'ai-hosts.json');
  const base = `http://127.0.0.1:${port}`;
  let st = { at: 0, offlineMs: 60000, engineHost: '', hosts: {}, byId: {}, providers: {}, mine: {}, all: [], status: [], enabled: false };
  const setState = (s) => { if (s && typeof s === 'object' && s.hosts && typeof s.hosts === 'object') st = s; };
  // at start: ask once (the front may still be starting: it pushes the state by itself within seconds anyway)
  (async () => { for (let i = 0; i < 20 && !st.at; i++) { try { const r = await call('/__front/ai/state', {}); setState(await r.json()); } catch {} if (!st.at) await new Promise((ok) => setTimeout(ok, 500)); } })();
  function call(p, body, signal) {
    return fetch(base + p, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Front-Secret': secret }, body: JSON.stringify(body || {}), signal });
  }

  // ---------- what hosts offer (from the snapshot) ----------
  const online = (id) => { const h = st.hosts[id]; return !!h && !!h.live && (h.polling || Date.now() - Number(h.seenMs || 0) < st.offlineMs); };
  // a cap ends when its window resets: counted here too (cappedUntil), not only at the next snapshot
  const capOf = (p) => (p.capped && p.cappedUntil && Date.now() >= p.cappedUntil ? '' : p.capped || '');
  const fresh = (p) => (p ? { ...p, online: online(p.hostId), capped: capOf(p) } : null);
  const providersFor = (user) => (st.providers[user] || []).map((id) => fresh(st.byId[id])).filter(Boolean);
  function provider(id, user) {
    if (user && !(st.providers[user] || []).includes(id)) return null;
    return fresh(st.byId[id]);
  }
  const status = () => st.status.map((s) => ({ ...s, online: online(s.id) }));
  const pcHostFor = (user) => { for (const [id, h] of Object.entries(st.hosts)) if (user && h.owner === user && h.pc && online(id)) return id; return null; };
  function pickEngineHost(engine) {
    const has = (h) => !Array.isArray(h.engines) || h.engines.includes(engine);
    if (st.engineHost) { const h = st.hosts[st.engineHost]; return h && online(st.engineHost) && has(h) ? st.engineHost : null; }
    for (const [id, h] of Object.entries(st.hosts)) if (h.live && online(id) && has(h)) return id;
    return null;
  }
  const reason = () => (pickEngineHost('ollama') ? '' : OFFLINE);
  const withOnline = (list) => (list || []).map((h) => ({ ...h, online: online(h.id), providers: (h.providers || []).map((p) => ({ ...p, capped: capOf(p) })) }));
  const answeredInFront = () => { throw new Error('The hub in front answers this (Settings → My AI hosts).'); };
  const hosts = {
    mine: (user) => withOnline(st.mine[user]),
    all: () => withOnline(st.all),
    get: (id) => withOnline(st.all).find((h) => h.id === id) || null,
    create: answeredInFront, rename: answeredInFront, remove: answeredInFront, setShare: answeredInFront,
    // the host program's config for one of the person's hosts (the AI Host download carries it): read from the registry
    configFor(user, id, hub) {
      let db = {}; try { db = JSON.parse(fs.readFileSync(FILE, 'utf8')); } catch {}
      const r = ((db && db.hosts) || {})[id]; if (!r || r.user !== user) throw new Error('Not your host.');
      return { _: `VibeRoller AI Host "${r.nickname}" of ${user}: made in Settings → My AI hosts. Keep this file private: the token is this host's key to the hub.`, hub, id: r.id, nickname: r.nickname, user, token: r.secret,
        providers: [{ id: 'claude', kind: 'claude-relay', label: `${user}'s Claude`, models: (config.models || []).map((m) => m.id), max5h: 90, max7d: 90, shareWithGroup: true, allowedUsers: [] },
          { id: 'codex', kind: 'codex-relay', label: `${user}'s ChatGPT / Codex`, models: ['gpt-5.5'], shareWithGroup: true, allowedUsers: [] }],
        engines: { ollama: 'http://127.0.0.1:11434', image: 'http://127.0.0.1:5124', voice: 'http://127.0.0.1:5123', speech: 'http://127.0.0.1:5125' },
        autoUpdate: true, update: { quietMinutes: 3, checkMinutes: 5 }, reconnect: { maxDelaySeconds: 5 }, security: { engineGuard: true, allowInsecureHub: false } };
    },
  };

  // ---------- calls carried to a host, through the front ----------
  // same shape as fetch(): a Response, its body streaming as the host sends it
  async function relay(meta, init = {}) {
    const rq = new Request('http://engine.invalid' + meta.path, init); // turns any body (string, buffer, form) and headers into bytes + a list
    const headers = {}; rq.headers.forEach((v, k) => { headers[k] = v; });
    const body = init.body !== undefined && init.body !== null ? Buffer.from(await rq.arrayBuffer()) : undefined;
    const m = Buffer.from(JSON.stringify({ ...meta, method: rq.method, headers })).toString('base64');
    const r = await fetch(base + '/__front/ai/fetch', { method: 'POST', headers: { 'X-VR-Front-Secret': secret, 'X-VR-AI': m, 'Content-Type': 'application/octet-stream' }, body, signal: init.signal });
    if (r.headers.get('x-vr-ai-error')) { const j = await r.json().catch(() => ({})); throw Object.assign(new Error(j.error || 'The AI host did not answer.'), { status: r.status }); }
    const h = new Headers(); r.headers.forEach((v, k) => { if (!FRONT_HEADERS.test(k)) h.set(k, v); });
    return new Response([101, 204, 205, 304].includes(r.status) ? null : r.body, { status: r.status, headers: h });
  }
  async function engineFetch(engine, p, init = {}) {
    if (!ENGINES.includes(engine)) throw new Error('Unknown engine ' + engine);
    if (!pickEngineHost(engine)) throw new Error(OFFLINE);
    return relay({ mode: 'engine', engine, path: p }, init);
  }
  const providerFetch = (hostId, providerId, p, init = {}) => relay({ mode: 'provider', host: hostId, provider: providerId, path: p }, init);
  const loginFetch = (hostId, providerId, p, init = {}) => relay({ mode: 'login', host: hostId, provider: providerId, path: p }, init);
  async function pcTool(hostId, name, args) {
    const j = await (await call('/__front/ai/pc-tool', { host: hostId, name: String(name || ''), args: args || {} })).json();
    if (j.error) throw new Error(j.error);
    return j.result;
  }
  // A Work run on a host. What can be told at once is (as before): not connected, offline, no such provider, capped.
  function runWork(hostId, providerId, payload, handlers) {
    const h = st.hosts[hostId];
    if (!h || !h.hello) throw new Error('That AI host has not connected yet.');
    if (!online(hostId)) throw new Error(`${h.nickname}${h.owner ? ` (${h.owner}'s AI host)` : ' (AI host)'} is offline right now. Pick another AI in the dropdown, or wait for it.`);
    const prov = (h.providers || []).find((p) => p.id === providerId);
    if (!prov) throw new Error('That host no longer offers this provider.');
    if (capOf(prov)) throw new Error(`${h.nickname}: ${capOf(prov)}. Pick another AI or wait for the window to reset.`);
    let id = '', cancelled = false, ended = false;
    const end = (fn) => { if (ended) return; ended = true; try { fn(); } catch {} };
    (async () => {
      const r = await call('/__front/ai/work', { host: hostId, provider: providerId, payload });
      const dec = new TextDecoder(); let buf = '';
      for await (const chunk of r.body) {
        buf += dec.decode(chunk, { stream: true });
        let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
          if (!line) continue;
          let m; try { m = JSON.parse(line); } catch { continue; }
          if (m.id) { id = m.id; if (cancelled) call('/__front/ai/cancel', { id }).catch(() => {}); }
          else if (m.ev) { try { handlers.onEvent(m.ev); } catch {} }
          else if (m.done !== undefined) end(() => handlers.onDone({ code: Number(m.done) || 0 }));
          else if (m.error) end(() => handlers.onError(String(m.error)));
        }
      }
      end(() => handlers.onError('The hub in front stopped the run.'));
    })().catch((e) => { log('[ai-host] a Work run through the front: ' + e.message); end(() => handlers.onError('The hub in front could not be reached: ' + e.message)); });
    return { get id() { return id; }, cancel: () => { cancelled = true; if (id) call('/__front/ai/cancel', { id }).catch(() => {}); } };
  }

  return { route: async () => false, engineFetch, providerFetch, loginFetch, runWork, jobFor: () => null, pcHostFor, pcTool, status, reason, online: () => !!pickEngineHost('ollama'), hostOnline: online, providersFor, provider, hosts,
    hostFlags: (r) => ({ pc: !!(st.hosts[r && r.id] || {}).pc }), ENGINES, get enabled() { return !!st.enabled; }, FILE, setState, front: true };
};
