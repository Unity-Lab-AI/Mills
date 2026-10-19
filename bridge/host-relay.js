'use strict';
// VibeRoller login relays, the AI host's side (used by bridge/ai-node.js; `node bridge/handoff.js probe` tries them).
// An AI host shares its AI, never its computer: the hub's own Claude Code / Codex run on the server, and only their model
// calls come to the host, which adds its own login and forwards them to ONE fixed provider address:
//   claude-relay  Claude subscription login (<configDir>/.credentials.json)  → https://api.anthropic.com/v1/messages
//   codex-relay   ChatGPT login of the Codex CLI (<codexHome>/auth.json)      → https://chatgpt.com/backend-api/codex/responses
// The token never leaves this machine; the caller's own auth headers are dropped; only a short list of headers passes.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

const HOME = os.homedir();
const RELAYS = {
  'claude-relay': { base: 'https://api.anthropic.com', paths: /^\/v1\/(messages|messages\/count_tokens)(\?[\w=&.-]*)?$/, dir: (p) => p.configDir || path.join(HOME, '.claude'), name: 'Claude' },
  'codex-relay': { base: 'https://chatgpt.com/backend-api/codex', paths: /^\/(responses|models)(\?[\w=&.-]*)?$/, dir: (p) => p.codexHome || path.join(HOME, '.codex'), name: 'Codex' },
};
const PASS_REQ = /^(content-type|accept|anthropic-version|anthropic-beta|openai-beta|x-stainless-[\w-]+|session_id|originator|x-app)$/i;
const PASS_RES = /^(content-type|request-id|anthropic-ratelimit-[\w-]+|retry-after|x-codex-[\w-]+|openai-[\w-]+)$/i;
const relayKind = (p) => (p.kind === 'claude-cli' ? 'claude-relay' : p.kind); // a host's Claude login is shared as a relay, never as a computer
const isRelay = (p) => !!RELAYS[relayKind(p)];

function loginToken(p) {
  const kind = relayKind(p), dir = RELAYS[kind].dir(p);
  if (kind === 'claude-relay') { const j = JSON.parse(fs.readFileSync(path.join(dir, '.credentials.json'), 'utf8')).claudeAiOauth || {}; return { token: j.accessToken, expiresAt: Number(j.expiresAt) || 0 }; }
  const j = JSON.parse(fs.readFileSync(path.join(dir, 'auth.json'), 'utf8')), t = j.tokens || {};
  return { token: t.access_token, account: t.account_id, apiKey: j.OPENAI_API_KEY || '' };
}
// let the provider's own CLI refresh its login (it does when it runs and finds the token old), in an empty folder, no tools
const refreshing = new Map();
function refreshLogin(p, { sandbox, log = () => {}, claudeCommand = 'claude' } = {}) {
  const kind = relayKind(p);
  if (refreshing.has(kind)) return refreshing.get(kind);
  fs.mkdirSync(sandbox, { recursive: true });
  const cmd = kind === 'claude-relay' ? String(p.command || claudeCommand) : String(p.command || 'codex');
  const args = kind === 'claude-relay' ? ['-p', '--strict-mcp-config', '--max-turns', '1', '--model', 'claude-haiku-5-5'] : ['exec', '--skip-git-repo-check', '-s', 'read-only', 'Reply with OK.'];
  const env = { ...process.env, ...(kind === 'claude-relay' ? { CLAUDE_CONFIG_DIR: RELAYS[kind].dir(p) } : { CODEX_HOME: RELAYS[kind].dir(p) }) };
  delete env.ANTHROPIC_BASE_URL; delete env.ANTHROPIC_AUTH_TOKEN; delete env.ANTHROPIC_API_KEY;
  const pr = new Promise((resolve) => {
    log(`refreshing the ${RELAYS[kind].name} login`);
    try {
      const c = spawn(cmd, args, { cwd: sandbox, env, windowsHide: true, stdio: ['pipe', 'ignore', 'ignore'], shell: process.platform === 'win32' });
      const t = setTimeout(() => { try { c.kill(); } catch {} resolve(); }, 120000);
      c.on('close', () => { clearTimeout(t); resolve(); }); c.on('error', () => { clearTimeout(t); resolve(); });
      c.stdin.end(kind === 'claude-relay' ? 'Reply with OK.' : '');
    } catch { resolve(); }
  }).finally(() => refreshing.delete(kind));
  refreshing.set(kind, pr);
  return pr;
}
// one forwarded call; answers a fetch Response. req = { path, method, headers, body (Buffer) }
async function forward(p, req, opts = {}) {
  const kind = relayKind(p), R = RELAYS[kind];
  if (!R.paths.test(String(req.path || ''))) throw Object.assign(new Error('That address is not relayed.'), { status: 403 });
  const base = String(p.baseUrl || R.base).replace(/\/+$/, ''); // baseUrl only for tests (a fake provider)
  const send = async () => {
    let t = loginToken(p);
    if (kind === 'claude-relay' && t.expiresAt && t.expiresAt < Date.now() + 120000) { await refreshLogin(p, opts); t = loginToken(p); }
    if (!t.token && !t.apiKey) throw Object.assign(new Error(`This host has no ${R.name} login (sign in to it on this machine once).`), { status: 503 });
    const h = {};
    for (const [k, v] of Object.entries(req.headers || {})) if (PASS_REQ.test(k)) h[k.toLowerCase()] = String(v);
    if (kind === 'claude-relay') {
      h.authorization = 'Bearer ' + t.token;
      const betas = new Set(String(h['anthropic-beta'] || '').split(',').map((s) => s.trim()).filter(Boolean)); betas.add('oauth-2025-04-20');
      h['anthropic-beta'] = [...betas].join(','); h['anthropic-version'] = h['anthropic-version'] || '2023-06-01';
    } else { h.authorization = 'Bearer ' + (t.token || t.apiKey); if (t.account) h['chatgpt-account-id'] = t.account; }
    return fetch(base + req.path, { method: req.method || 'POST', headers: h, body: req.body, signal: AbortSignal.timeout(600000) });
  };
  let r = await send();
  if (r.status === 401) { await refreshLogin(p, opts); r = await send(); }
  return r;
}
const passHeaders = (h) => { const out = {}; h.forEach((v, k) => { if (PASS_RES.test(k)) out[k] = v; }); return out; };
function usageFrom(h) { // Anthropic reports the subscription's windows in its response headers
  const w = (name) => { const u = h.get(`anthropic-ratelimit-unified-${name}-utilization`), r = h.get(`anthropic-ratelimit-unified-${name}-reset`); return u == null ? null : { utilization: Number(u), resetsAt: r ? Number(r) * 1000 : null, status: h.get(`anthropic-ratelimit-unified-${name}-status`) || '' }; };
  const five = w('5h'), seven = w('7d');
  return five || seven ? { ...(five ? { five_hour: five } : {}), ...(seven ? { seven_day: seven } : {}) } : null;
}
// a tiny real call per relay: does this machine's login answer? (never prints a token)
async function probe(p, opts = {}) {
  const kind = relayKind(p);
  const body = kind === 'claude-relay'
    ? { model: (p.models || [])[p.models && p.models.includes('claude-haiku-5-5') ? p.models.indexOf('claude-haiku-5-5') : 0] || 'claude-haiku-5-5', max_tokens: 16, system: [{ type: 'text', text: "You are Claude Code, Anthropic's official CLI for Claude." }], messages: [{ role: 'user', content: 'Reply with the single word OK.' }] }
    : { model: (p.models || [])[0], instructions: 'You are a helpful assistant.', input: [{ role: 'user', content: [{ type: 'input_text', text: 'Reply with the single word OK.' }] }], stream: true, store: false };
  const r = await forward(p, { path: kind === 'claude-relay' ? '/v1/messages' : '/responses', method: 'POST', headers: { 'content-type': 'application/json', 'anthropic-version': '2023-06-01', accept: kind === 'claude-relay' ? 'application/json' : 'text/event-stream' }, body: Buffer.from(JSON.stringify(body)) }, opts);
  const text = await r.text();
  let said = '';
  if (kind === 'claude-relay') { try { said = (JSON.parse(text).content || []).map((c) => c.text || '').join(''); } catch {} }
  else for (const line of text.split('\n')) { if (!line.startsWith('data:')) continue; try { const ev = JSON.parse(line.slice(5)); if (ev.type === 'response.output_text.delta') said += ev.delta; } catch {} }
  return { kind, status: r.status, model: body.model, said: said.trim().slice(0, 40), error: r.ok ? '' : text.replace(/\s+/g, ' ').slice(0, 300), usage: usageFrom(r.headers) };
}

module.exports = { RELAYS, relayKind, isRelay, forward, refreshLogin, passHeaders, usageFrom, probe, loginToken };
