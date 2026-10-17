// OpenAI-compatible API on the Mills hub, one key per person.
//
//   GET  /v1/models                      the models the key's provider serves
//   POST /v1/chat/completions            OpenAI chat format in (stream or not), answered by the person's
//                                        chosen provider (bridge/providers.js): the host's Claude by default
//   Authorization: Bearer mills_…        the person's own key, made in Settings → API access
//
// Keys are stored as sha256 hashes in the account's state (the plain key is shown exactly once). Each
// key can expire after N days ("auto rotate": it stops working and Settings offers a fresh one), and can
// be rotated or revoked at any time. Session-signed routes under /api/apikey manage it from Settings.
// Nothing here changes how the browser signs in or how Work runs happen.
'use strict';

const crypto = require('crypto');

module.exports = function createOpenAI({ accounts, providers, send, readBody, writeState, tunnelUrl, onIdle }) {
  const KEY_RE = /^mills_[a-f0-9]{48}$/;
  const hashKey = (k) => crypto.createHash('sha256').update(k).digest('hex');
  const running = new Map(); // username -> AbortController (one API answer at a time per person)

  // ---- keys ----
  function status(acct) {
    const a = acct.state.api || {};
    const expiresAt = a.createdAt && a.expiresDays ? new Date(a.createdAt).getTime() + a.expiresDays * 86400e3 : null;
    return { hasKey: !!a.hash, prefix: a.prefix || '', createdAt: a.createdAt || null, expiresDays: a.expiresDays || 0, expiresAt, expired: !!(expiresAt && expiresAt < Date.now()),
      provider: a.provider || 'host', model: a.model || '', endpoint: (tunnelUrl() || '') + '/v1', calls: a.calls || 0, lastUsed: a.lastUsed || null };
  }
  function rotate(acct, body = {}) {
    const key = 'mills_' + crypto.randomBytes(24).toString('hex');
    const prev = acct.state.api || {};
    acct.state.api = { ...prev, hash: hashKey(key), prefix: key.slice(0, 12), createdAt: new Date().toISOString(), expiresDays: clampDays(body.expiresDays ?? prev.expiresDays), calls: 0 };
    writeState(acct);
    return { key, ...status(acct) };
  }
  const clampDays = (d) => { const n = Math.round(Number(d) || 0); return [0, 1, 7, 30, 90].includes(n) ? n : 0; };
  function settings(acct, body) {
    const a = acct.state.api || (acct.state.api = {});
    if (body.expiresDays !== undefined) a.expiresDays = clampDays(body.expiresDays);
    if (body.provider !== undefined) { const p = providers.get(String(body.provider), acct.username); if (!p) throw new Error('Unknown provider.'); a.provider = p.id; if (!(p.models || []).includes(a.model)) a.model = ''; }
    if (body.model !== undefined) { const p = providers.get(a.provider || 'host', acct.username); a.model = p && (p.models || []).includes(body.model) ? body.model : ''; }
    writeState(acct);
    return status(acct);
  }
  function revoke(acct) { delete acct.state.api; writeState(acct); return status(acct); }
  function accountForKey(req) {
    const m = /^Bearer (.+)$/.exec(req.headers.authorization || '');
    if (!m || !KEY_RE.test(m[1])) return { error: 'Missing or malformed API key. Use "Authorization: Bearer mills_…" with the key from Settings → API access.' };
    const h = hashKey(m[1]);
    const acct = [...accounts.values()].find((a) => a.state.api && a.state.api.hash === h);
    if (!acct) return { error: 'Unknown API key (rotated, revoked, or never issued).' };
    if (status(acct).expired) return { error: 'This API key has expired; rotate it in Settings → API access.' };
    return { acct };
  }

  // ---- OpenAI shapes ----
  function oaError(res, status, message, type = 'invalid_request_error') { send(res, status, { error: { message, type, code: null } }); }
  const id = () => 'chatcmpl-' + crypto.randomBytes(12).toString('hex');
  async function handle(req, res, p) {
    res.setHeader('Access-Control-Allow-Origin', '*'); // bearer-key API for harnesses; the browser app never calls it
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization');
    if (req.method === 'OPTIONS') { res.writeHead(204); return res.end(); }
    const { acct, error } = accountForKey(req);
    if (error) return oaError(res, 401, error, 'authentication_error');
    const a = acct.state.api, prov = providers.get(a.provider || 'host', acct.username) || providers.get('host');
    if (p === '/v1/models' && req.method === 'GET') {
      return send(res, 200, { object: 'list', data: (prov.models || []).map((m) => ({ id: m, object: 'model', created: 0, owned_by: prov.kind === 'host' ? 'mills' : prov.owner })) });
    }
    if (p === '/v1/chat/completions' && req.method === 'POST') {
      let body; try { body = await readBody(req); } catch { return oaError(res, 400, 'Body is not JSON.'); }
      const messages = Array.isArray(body.messages) ? body.messages.filter((m) => m && typeof m.role === 'string').map((m) => ({ role: m.role, content: contentText(m.content) })) : [];
      if (!messages.length) return oaError(res, 400, '"messages" is required.');
      if (running.has(acct.username)) return oaError(res, 429, 'One request at a time per key; the previous one is still answering.', 'rate_limit_error');
      const system = messages.filter((m) => m.role === 'system').map((m) => m.content).join('\n\n');
      const model = providers.pickModel(prov, typeof body.model === 'string' && /^claude|^[\w.:-]+$/.test(body.model) && (prov.models || []).includes(body.model) ? body.model : a.model);
      const ctl = new AbortController(); running.set(acct.username, ctl);
      req.on('close', () => { if (!res.writableEnded) ctl.abort(); });
      a.calls = (a.calls || 0) + 1; a.lastUsed = new Date().toISOString(); writeState(acct);
      const cid = id(), created = Math.floor(Date.now() / 1000);
      try {
        if (body.stream) {
          res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store', Connection: 'keep-alive', 'X-Accel-Buffering': 'no' });
          const chunk = (delta, finish = null) => res.write('data: ' + JSON.stringify({ id: cid, object: 'chat.completion.chunk', created, model, choices: [{ index: 0, delta, finish_reason: finish }] }) + '\n\n');
          chunk({ role: 'assistant', content: '' });
          const r = await providers.complete({ provider: prov.id, model, system, messages, maxTokens: body.max_tokens, signal: ctl.signal, onToken: (t) => chunk({ content: t }) });
          chunk({}, 'stop');
          if (r.usage) res.write('data: ' + JSON.stringify({ id: cid, object: 'chat.completion.chunk', created, model: r.model || model, choices: [], usage: { ...r.usage, total_tokens: (r.usage.prompt_tokens || 0) + (r.usage.completion_tokens || 0) } }) + '\n\n');
          res.end('data: [DONE]\n\n');
        } else {
          const r = await providers.complete({ provider: prov.id, model, system, messages, maxTokens: body.max_tokens, signal: ctl.signal });
          send(res, 200, { id: cid, object: 'chat.completion', created, model: r.model || model, choices: [{ index: 0, message: { role: 'assistant', content: r.text }, finish_reason: 'stop' }],
            usage: r.usage ? { ...r.usage, total_tokens: (r.usage.prompt_tokens || 0) + (r.usage.completion_tokens || 0) } : { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 } });
        }
      } catch (e) {
        if (!res.headersSent) oaError(res, 502, e.message, 'api_error');
        else { try { res.end('data: ' + JSON.stringify({ error: { message: e.message, type: 'api_error' } }) + '\n\ndata: [DONE]\n\n'); } catch {} }
      } finally { running.delete(acct.username); onIdle(); }
      return;
    }
    return oaError(res, 404, 'Unknown endpoint. This hub serves GET /v1/models and POST /v1/chat/completions.');
  }
  const contentText = (c) => (typeof c === 'string' ? c : Array.isArray(c) ? c.map((x) => (x && x.type === 'text' ? x.text : '')).join('') : String(c ?? ''));

  // Session-signed management from Settings (server.js routes these after the sign-in check).
  async function route(p, req, res, acct) {
    if (p === '/api/apikey' && req.method === 'GET') { send(res, 200, status(acct)); return true; }
    if (p === '/api/apikey/rotate' && req.method === 'POST') { send(res, 200, rotate(acct, await readBody(req))); return true; }
    if (p === '/api/apikey/settings' && req.method === 'POST') { try { send(res, 200, settings(acct, await readBody(req))); } catch (e) { send(res, 400, { error: e.message }); } return true; }
    if (p === '/api/apikey/revoke' && req.method === 'POST') { send(res, 200, revoke(acct)); return true; }
    return false;
  }
  return { handle, route, busy: () => running.size > 0, stop: (acct) => { const c = running.get(acct.username); if (c) c.abort(); } };
};
