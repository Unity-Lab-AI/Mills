// OpenAI- and Anthropic-compatible API on the VibeRoller hub, one key per person, both formats on every provider.
//
//   GET  /v1/models                      the models the key's provider serves (Anthropic's list shape when the
//                                        call carries an anthropic-version header)
//   POST /v1/chat/completions            OpenAI chat format in (stream or not), answered by the person's
//                                        chosen provider (bridge/providers.js): the host's Claude by default
//                                        (OpenAI → Anthropic: a Claude provider answers an OpenAI client)
//   POST /v1/messages                    Anthropic Messages format in (stream or not: Anthropic's SSE events),
//                                        answered by the same provider, an OpenAI-compatible endpoint too
//                                        (Anthropic → OpenAI: an Anthropic client such as an SDK reaches a GPT / local model)
//   POST /v1/messages/count_tokens       a rough count (about 4 characters a token) for clients that ask first
//   Authorization: Bearer mills_…        the person's own key, made in Settings → API access; the same key also
//   x-api-key: mills_…                   works in Anthropic's header, so one key serves both formats
// Text only: images, tool calls and tool results are passed on as their text (tool_result content), not as tools.
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
    if (body.model !== undefined) { const p = providers.effective(acct.username, a.provider || ''); a.model = p && (p.models || []).includes(body.model) ? body.model : ''; }
    writeState(acct);
    return status(acct);
  }
  function revoke(acct) { delete acct.state.api; writeState(acct); return status(acct); }
  function accountForKey(req) {
    const m = /^Bearer (.+)$/.exec(req.headers.authorization || ''), k = m ? m[1].trim() : String(req.headers['x-api-key'] || '').trim();
    if (!KEY_RE.test(k)) return { error: 'Missing or malformed API key. Use "Authorization: Bearer mills_…" (or "x-api-key: mills_…") with the key from Settings → API access.' };
    const h = hashKey(k);
    const acct = [...accounts.values()].find((a) => a.state.api && a.state.api.hash === h);
    if (!acct) return { error: 'Unknown API key (rotated, revoked, or never issued).' };
    if (status(acct).expired) return { error: 'This API key has expired; rotate it in Settings → API access.' };
    return { acct };
  }

  // ---- OpenAI shapes ----
  function oaError(res, status, message, type = 'invalid_request_error') { send(res, status, { error: { message, type, code: null } }); }
  // Anthropic's error shape; its type names (authentication_error, overloaded_error, …) differ slightly from OpenAI's
  const ANT_TYPE = { service_unavailable: 'overloaded_error', rate_limit_error: 'rate_limit_error', authentication_error: 'authentication_error', invalid_request_error: 'invalid_request_error', not_found_error: 'not_found_error' };
  function antError(res, status, message, type = 'invalid_request_error') { send(res, status, { type: 'error', error: { type: ANT_TYPE[type] || 'api_error', message } }); }
  const id = () => 'chatcmpl-' + crypto.randomBytes(12).toString('hex');
  async function handle(req, res, p) {
    res.setHeader('Access-Control-Allow-Origin', '*'); // bearer-key API for harnesses; the browser app never calls it
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, Authorization, x-api-key, anthropic-version, anthropic-beta');
    if (req.method === 'OPTIONS') { res.writeHead(204); return res.end(); }
    const anthropic = p.startsWith('/v1/messages') || (p === '/v1/models' && !!req.headers['anthropic-version']);
    const fail = anthropic ? antError : oaError;
    const { acct, error } = accountForKey(req);
    if (error) return fail(res, 401, error, 'authentication_error');
    const a = acct.state.api, prov = providers.effective(acct.username, a.provider || '');
    if (!prov) return fail(res, 503, 'No AI is available: this hub has no Claude of its own and no AI host offering one is connected.', 'service_unavailable');
    if (p === '/v1/models' && req.method === 'GET') {
      const ms = prov.models || [];
      if (anthropic) return send(res, 200, { data: ms.map((m) => ({ type: 'model', id: m, display_name: m, created_at: '1970-01-01T00:00:00Z' })), has_more: false, first_id: ms[0] || null, last_id: ms[ms.length - 1] || null });
      return send(res, 200, { object: 'list', data: ms.map((m) => ({ id: m, object: 'model', created: 0, owned_by: prov.kind === 'host' ? 'mills' : prov.owner })) });
    }
    if (p === '/v1/messages/count_tokens' && req.method === 'POST') {
      let body; try { body = await readBody(req); } catch { return antError(res, 400, 'Body is not JSON.'); }
      const { system, messages } = fromAnthropic(body);
      const chars = system.length + messages.reduce((n, m) => n + m.content.length, 0);
      return send(res, 200, { input_tokens: Math.max(1, Math.ceil(chars / 4)) });
    }
    if (p === '/v1/messages' && req.method === 'POST') return messagesApi(req, res, acct, a, prov);
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
          const r = await providers.complete({ provider: prov.id, user: acct.username, model, system, messages, maxTokens: body.max_tokens, signal: ctl.signal, onToken: (t) => chunk({ content: t }) });
          chunk({}, 'stop');
          if (r.usage) res.write('data: ' + JSON.stringify({ id: cid, object: 'chat.completion.chunk', created, model: r.model || model, choices: [], usage: { ...r.usage, total_tokens: (r.usage.prompt_tokens || 0) + (r.usage.completion_tokens || 0) } }) + '\n\n');
          res.end('data: [DONE]\n\n');
        } else {
          const r = await providers.complete({ provider: prov.id, user: acct.username, model, system, messages, maxTokens: body.max_tokens, signal: ctl.signal });
          send(res, 200, { id: cid, object: 'chat.completion', created, model: r.model || model, choices: [{ index: 0, message: { role: 'assistant', content: r.text }, finish_reason: 'stop' }],
            usage: r.usage ? { ...r.usage, total_tokens: (r.usage.prompt_tokens || 0) + (r.usage.completion_tokens || 0) } : { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 } });
        }
      } catch (e) {
        if (!res.headersSent) oaError(res, e.status || 502, e.message, e.status === 503 ? 'service_unavailable' : 'api_error'); // 503 = the host paused its AI
        else { try { res.end('data: ' + JSON.stringify({ error: { message: e.message, type: 'api_error' } }) + '\n\ndata: [DONE]\n\n'); } catch {} }
      } finally { running.delete(acct.username); onIdle(); }
      return;
    }
    return fail(res, 404, 'Unknown endpoint. This hub serves GET /v1/models, POST /v1/chat/completions (OpenAI) and POST /v1/messages (Anthropic).', 'not_found_error');
  }
  const contentText = (c) => (typeof c === 'string' ? c : Array.isArray(c) ? c.map((x) => (x && x.type === 'text' ? x.text : '')).join('') : String(c ?? ''));

  // ---- Anthropic shapes ----
  // A block list → its text: text blocks as they are, a tool result's content, a tool call / picture as a short note.
  function blockText(c) {
    if (typeof c === 'string') return c;
    if (!Array.isArray(c)) return String(c ?? '');
    return c.map((b) => {
      if (!b || typeof b !== 'object') return '';
      if (b.type === 'text') return String(b.text || '');
      if (b.type === 'tool_result') return blockText(b.content);
      if (b.type === 'tool_use') return `[tool call ${b.name || ''}: ${JSON.stringify(b.input || {}).slice(0, 2000)}]`;
      if (b.type === 'image' || b.type === 'document') return `[${b.type} not passed on]`;
      return '';
    }).filter(Boolean).join('\n');
  }
  function fromAnthropic(body) {
    const system = blockText(body.system || '');
    const messages = (Array.isArray(body.messages) ? body.messages : []).filter((m) => m && (m.role === 'user' || m.role === 'assistant'))
      .map((m) => ({ role: m.role, content: blockText(m.content) }));
    return { system, messages };
  }
  const msgId = () => 'msg_' + crypto.randomBytes(12).toString('hex');
  async function messagesApi(req, res, acct, a, prov) {
    let body; try { body = await readBody(req); } catch { return antError(res, 400, 'Body is not JSON.'); }
    const { system, messages } = fromAnthropic(body || {});
    if (!messages.length) return antError(res, 400, 'messages: at least one user or assistant message is required.');
    if (running.has(acct.username)) return antError(res, 429, 'One request at a time per key; the previous one is still answering.', 'rate_limit_error');
    const model = providers.pickModel(prov, typeof body.model === 'string' && (prov.models || []).includes(body.model) ? body.model : a.model);
    const maxTokens = Number(body.max_tokens) > 0 ? Math.min(Math.round(Number(body.max_tokens)), 128000) : undefined;
    const ctl = new AbortController(); running.set(acct.username, ctl);
    req.on('close', () => { if (!res.writableEnded) ctl.abort(); });
    a.calls = (a.calls || 0) + 1; a.lastUsed = new Date().toISOString(); writeState(acct);
    const mid = msgId();
    const antUsage = (u) => ({ input_tokens: (u && u.prompt_tokens) || 0, output_tokens: (u && u.completion_tokens) || 0 });
    try {
      if (body.stream) {
        res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store', Connection: 'keep-alive', 'X-Accel-Buffering': 'no' });
        const ev = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
        ev('message_start', { message: { id: mid, type: 'message', role: 'assistant', content: [], model, stop_reason: null, stop_sequence: null, usage: { input_tokens: 0, output_tokens: 0 } } });
        ev('content_block_start', { index: 0, content_block: { type: 'text', text: '' } });
        ev('ping', {});
        const r = await providers.complete({ provider: prov.id, user: acct.username, model, system, messages, maxTokens, signal: ctl.signal, onToken: (t) => ev('content_block_delta', { index: 0, delta: { type: 'text_delta', text: t } }) });
        ev('content_block_stop', { index: 0 });
        ev('message_delta', { delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: antUsage(r.usage) });
        ev('message_stop', {});
        res.end();
      } else {
        const r = await providers.complete({ provider: prov.id, user: acct.username, model, system, messages, maxTokens, signal: ctl.signal });
        send(res, 200, { id: mid, type: 'message', role: 'assistant', model: r.model || model, content: [{ type: 'text', text: r.text || '' }], stop_reason: 'end_turn', stop_sequence: null, usage: antUsage(r.usage) });
      }
    } catch (e) {
      if (!res.headersSent) antError(res, e.status || 502, e.message, e.status === 503 ? 'service_unavailable' : e.status === 429 ? 'rate_limit_error' : 'api_error');
      else { try { res.end(`event: error\ndata: ${JSON.stringify({ type: 'error', error: { type: 'api_error', message: e.message } })}\n\n`); } catch {} }
    } finally { running.delete(acct.username); onIdle(); }
  }

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
