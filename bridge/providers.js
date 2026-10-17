// Mills AI providers: where a model answer comes from.
//
// The host's own Claude Code login ("host", Gee's) is the default for everything and is not touched by
// anything here. On top of it, any person can hook up an AI of their own and keep it to themselves or
// share it with the team:
//   anthropic   an Anthropic API key           → the hub calls the Messages API with it; Work runs get it as
//                                                  ANTHROPIC_API_KEY for their `claude` process
//   claude-cli  a second Claude Code login      → `claude` run with CLAUDE_CONFIG_DIR=<folder> (the host logs that
//                                                  folder in once with `claude login`); Work runs use it too
//   openai      an OpenAI-compatible endpoint   → chat completions proxied to baseUrl with its key (Unity section
//                                                  brains and the /v1 adapter; not for Work runs, which need Claude)
// Storage: bridge/providers.json on the host (gitignored, secrets inside, never leaves the machine).
// One completion interface for every kind: complete({ provider, model, system, messages, onToken, signal })
// → { text, model, usage }. Used by the OpenAI-compatible /v1 adapter (bridge/openai.js).
'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawn } = require('child_process');

const KINDS = ['anthropic', 'claude-cli', 'openai'];
const ANTHROPIC_URL = 'https://api.anthropic.com/v1/messages';

module.exports = function createProviders({ ROOT, MILLS_DIR, config, readJson, writeJson, accounts }) {
  const FILE = path.join(ROOT, 'providers.json');
  const SANDBOX = path.join(MILLS_DIR, 'Api', 'sandbox'); // empty folder `claude -p` runs in for API calls: no project files in reach
  let db = readJson(FILE, { providers: [] });
  if (!Array.isArray(db.providers)) db = { providers: [] };
  const save = () => writeJson(FILE, db);
  const hostModels = () => config.models.map((m) => m.id);

  // ---- registry ----
  const HOST = { id: 'host', kind: 'host', label: 'Host Claude (default)', owner: '', shared: true, models: hostModels() };
  const pub = (p) => ({ id: p.id, kind: p.kind, label: p.label, owner: p.owner, shared: !!p.shared, models: p.models || [], baseUrl: p.baseUrl || undefined, configDir: p.configDir || undefined, hasKey: !!p.apiKey, createdAt: p.createdAt });
  const visible = (p, user) => p.shared || p.owner === user;
  function list(user) { return [{ ...HOST, models: hostModels() }, ...db.providers.filter((p) => visible(p, user)).map(pub)]; }
  function get(id, user) {
    if (!id || id === 'host') return { ...HOST, models: hostModels() };
    const p = db.providers.find((x) => x.id === id);
    return p && (!user || visible(p, user)) ? p : null;
  }
  function add(user, body) {
    const kind = String(body.kind || '');
    if (!KINDS.includes(kind)) throw new Error('kind must be anthropic, claude-cli or openai.');
    const label = String(body.label || '').trim().slice(0, 60);
    if (!label) throw new Error('Give it a label.');
    const p = { id: 'p-' + crypto.randomBytes(5).toString('hex'), kind, label, owner: user, shared: !!body.shared, createdAt: new Date().toISOString(), models: [] };
    if (kind === 'anthropic') {
      p.apiKey = String(body.apiKey || '').trim();
      if (!/^sk-ant-[\w-]{20,}$/.test(p.apiKey)) throw new Error('That does not look like an Anthropic API key (sk-ant-…).');
      p.models = cleanModels(body.models, hostModels());
    } else if (kind === 'claude-cli') {
      p.configDir = String(body.configDir || '').trim();
      if (!p.configDir || !path.isAbsolute(p.configDir)) throw new Error('configDir must be an absolute folder on the host (the one you ran `claude login` with CLAUDE_CONFIG_DIR set to).');
      p.models = cleanModels(body.models, hostModels());
    } else {
      p.baseUrl = String(body.baseUrl || '').trim().replace(/\/+$/, '');
      if (!/^https?:\/\/[^\s/]+/.test(p.baseUrl)) throw new Error('baseUrl must be an http(s) address, e.g. https://api.example.com/v1');
      p.apiKey = String(body.apiKey || '').trim();
      p.models = cleanModels(body.models, []);
      if (!p.models.length) throw new Error('List at least one model id the endpoint serves.');
    }
    db.providers.push(p); save();
    return pub(p);
  }
  const cleanModels = (m, fallback) => { const l = (Array.isArray(m) ? m : String(m || '').split(/[,\s]+/)).map((s) => String(s).trim()).filter(Boolean).slice(0, 20); return l.length ? l : fallback; };
  function update(user, id, body) {
    const p = db.providers.find((x) => x.id === id);
    if (!p || p.owner !== user) throw new Error('Not your provider.');
    if (body.label !== undefined) p.label = String(body.label).trim().slice(0, 60) || p.label;
    if (body.shared !== undefined) p.shared = !!body.shared;
    if (body.models !== undefined) p.models = cleanModels(body.models, p.kind === 'openai' ? p.models : hostModels());
    if (body.apiKey && p.kind !== 'claude-cli') p.apiKey = String(body.apiKey).trim();
    save(); return pub(p);
  }
  function remove(user, id) {
    const i = db.providers.findIndex((x) => x.id === id);
    if (i < 0 || db.providers[i].owner !== user) throw new Error('Not your provider.');
    db.providers.splice(i, 1); save();
    for (const a of accounts.values()) { // nobody keeps pointing at it
      let changed = false;
      if (a.state.provider === id) { delete a.state.provider; changed = true; }
      if (a.state.api && a.state.api.provider === id) { delete a.state.api.provider; changed = true; }
      if (changed) writeJson(a.files.STATE_FILE, a.state);
    }
    return true;
  }
  // Environment a Work run's `claude` gets when its person picked a Claude-capable provider: nothing for the host.
  function claudeEnv(p) {
    if (!p || p.kind === 'host') return {};
    if (p.kind === 'anthropic') return { ANTHROPIC_API_KEY: p.apiKey };
    if (p.kind === 'claude-cli') return { CLAUDE_CONFIG_DIR: p.configDir };
    return null; // not Claude: a Work run cannot use it
  }
  const pickModel = (p, model) => (model && (p.models || []).includes(model) ? model : (p.models || [])[0] || model || '');

  // ---- one completion for the /v1 adapter and the Unity section ----
  function flatten(system, messages) { // chat → one prompt for `claude -p` (stateless, like the API)
    const turns = messages.filter((m) => m.role !== 'system');
    const last = turns.length && turns[turns.length - 1].role === 'user' ? turns.pop() : null;
    const hist = turns.map((m) => `${m.role === 'assistant' ? 'Assistant' : 'User'}: ${m.content}`).join('\n\n');
    return (hist ? `Conversation so far:\n\n${hist}\n\n` : '') + (last ? (hist ? 'User: ' : '') + last.content : 'Continue.');
  }
  function completeClaudeCli(p, { model, system, messages, onToken, signal }) {
    return new Promise((resolve, reject) => {
      fs.mkdirSync(SANDBOX, { recursive: true });
      const args = ['-p', '--output-format', 'stream-json', '--verbose', '--permission-mode', 'default', '--strict-mcp-config'];
      if (system) args.push('--append-system-prompt', system);
      if (model) args.push('--model', model);
      const pre = Array.isArray(config.claudeArgs) ? config.claudeArgs : [];
      const env = { ...process.env, ...(claudeEnv(p) || {}) };
      const child = spawn(config.claudeCommand || 'claude', [...pre, ...args], { cwd: SANDBOX, windowsHide: true, env });
      let buf = '', text = '', usage = null, usedModel = model || '', err = '';
      const stop = () => { try { if (process.platform === 'win32') spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { windowsHide: true }); else child.kill('SIGTERM'); } catch {} };
      if (signal) signal.addEventListener('abort', stop, { once: true });
      child.stdout.on('data', (c) => {
        buf += c; let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
          if (!line) continue;
          let ev; try { ev = JSON.parse(line); } catch { continue; }
          if (ev.type === 'system' && ev.model) usedModel = ev.model;
          if (ev.type === 'assistant' && ev.message) {
            if (ev.message.model) usedModel = ev.message.model;
            if (ev.message.usage) usage = ev.message.usage;
            for (const b of ev.message.content || []) if (b.type === 'text' && b.text) { text += b.text; if (onToken) onToken(b.text); }
          }
          if (ev.type === 'result' && ev.is_error) err = String(ev.result || ev.subtype || 'error');
        }
      });
      child.stderr.on('data', (d) => { err = (err + String(d)).slice(-600); });
      child.on('error', (e) => reject(new Error('Could not start claude: ' + e.message)));
      child.on('close', (code) => {
        if (signal && signal.aborted) return reject(new Error('cancelled'));
        if (!text && (code || err)) return reject(new Error(err.trim() || 'claude exited with code ' + code));
        resolve({ text, model: usedModel, usage: usage ? { prompt_tokens: (usage.input_tokens || 0) + (usage.cache_read_input_tokens || 0) + (usage.cache_creation_input_tokens || 0), completion_tokens: usage.output_tokens || 0 } : null });
      });
      child.stdin.end(flatten(system, messages));
    });
  }
  async function completeAnthropic(p, { model, system, messages, onToken, signal, maxTokens }) {
    const body = { model: model || p.models[0], max_tokens: maxTokens || 4096, stream: !!onToken,
      messages: messages.filter((m) => m.role !== 'system').map((m) => ({ role: m.role === 'assistant' ? 'assistant' : 'user', content: String(m.content) })) };
    if (system) body.system = system;
    const r = await fetch(ANTHROPIC_URL, { method: 'POST', signal, headers: { 'content-type': 'application/json', 'x-api-key': p.apiKey, 'anthropic-version': '2023-06-01' }, body: JSON.stringify(body) });
    if (!r.ok) throw new Error('Anthropic answered ' + r.status + ': ' + (await r.text()).slice(0, 300));
    if (!onToken) { const j = await r.json(); return { text: (j.content || []).map((c) => c.text || '').join(''), model: j.model, usage: j.usage ? { prompt_tokens: j.usage.input_tokens, completion_tokens: j.usage.output_tokens } : null }; }
    let text = '', usedModel = model, usage = null;
    for await (const ev of sse(r.body)) {
      if (ev.type === 'content_block_delta' && ev.delta && ev.delta.text) { text += ev.delta.text; onToken(ev.delta.text); }
      if (ev.type === 'message_start' && ev.message) { usedModel = ev.message.model || usedModel; usage = { prompt_tokens: (ev.message.usage || {}).input_tokens || 0, completion_tokens: 0 }; }
      if (ev.type === 'message_delta' && ev.usage && usage) usage.completion_tokens = ev.usage.output_tokens || 0;
    }
    return { text, model: usedModel, usage };
  }
  async function completeOpenAI(p, { model, system, messages, onToken, signal, maxTokens }) {
    const body = { model: model || p.models[0], stream: !!onToken, messages: [...(system ? [{ role: 'system', content: system }] : []), ...messages.filter((m) => m.role !== 'system').map((m) => ({ role: m.role, content: String(m.content) }))] };
    if (maxTokens) body.max_tokens = maxTokens;
    const r = await fetch(p.baseUrl + '/chat/completions', { method: 'POST', signal, headers: { 'content-type': 'application/json', ...(p.apiKey ? { authorization: 'Bearer ' + p.apiKey } : {}) }, body: JSON.stringify(body) });
    if (!r.ok) throw new Error('The endpoint answered ' + r.status + ': ' + (await r.text()).slice(0, 300));
    if (!onToken) { const j = await r.json(); return { text: ((j.choices || [])[0] || {}).message?.content || '', model: j.model || body.model, usage: j.usage || null }; }
    let text = '', usedModel = body.model, usage = null;
    for await (const ev of sse(r.body)) {
      const d = ((ev.choices || [])[0] || {}).delta || {};
      if (d.content) { text += d.content; onToken(d.content); }
      if (ev.model) usedModel = ev.model; if (ev.usage) usage = ev.usage;
    }
    return { text, model: usedModel, usage };
  }
  async function* sse(body) { // "data: {json}" lines → objects; [DONE] ends it
    const dec = new TextDecoder(); let buf = '';
    for await (const chunk of body) {
      buf += dec.decode(chunk, { stream: true }); let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
        if (!line.startsWith('data:')) continue;
        const data = line.slice(5).trim();
        if (data === '[DONE]') return;
        try { yield JSON.parse(data); } catch {}
      }
    }
  }
  async function complete({ provider, model, system = '', messages = [], onToken = null, signal, maxTokens }) {
    const p = get(provider) || HOST;
    const m = pickModel(p, model);
    if (p.kind === 'openai') return completeOpenAI(p, { model: m, system, messages, onToken, signal, maxTokens });
    if (p.kind === 'anthropic') return completeAnthropic(p, { model: m, system, messages, onToken, signal, maxTokens });
    return completeClaudeCli(p, { model: m, system, messages, onToken, signal }); // host or claude-cli
  }

  return { list, get, add, update, remove, claudeEnv, pickModel, complete, KINDS, FILE, SANDBOX };
};
