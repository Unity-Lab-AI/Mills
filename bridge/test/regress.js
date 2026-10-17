#!/usr/bin/env node
// Mills regression runner (hub law 9). Runs the whole docs/REGRESSION.md checklist against a
// THROWAWAY hub that lives alongside production without touching it:
//   - code copied to a scratch folder in the OS temp dir (outside Mills/), deleted afterwards
//   - its own free port, no tunnel, no gist, its own Archive/ and Chat/ inside the scratch folder
//   - throwaway accounts with throwaway passwords; real auth.json files are only hashed (read)
//   - a fake Claude (test/fake-claude.js) instead of the real one
//   - restart tests use the scratch copy's own restart.js (graceful flag only, never --hard)
// Production is only READ: its status page, which ports listen, and checksums of its account files.
//
//   node bridge/test/regress.js            full run, exit code 1 if anything failed
//   node bridge/test/regress.js --keep     keep the scratch folder for inspection
//   node bridge/test/regress.js --no-prod  skip the read-only production checks
//   node bridge/test/regress.js --md       only the markdown renderer cases (bridge/test/md-check.js), no hub
//   node bridge/test/regress.js --install-vendor
//                                          first fetch + integrity-check the vendored browser libraries
//                                          (web/vendor, via bridge/vendor.js), then run the checks
'use strict';

const fs = require('fs');
const os = require('os');
const net = require('net');
const path = require('path');
const crypto = require('crypto');
const zlib = require('zlib');
const { spawn, spawnSync, execFileSync } = require('child_process');

const BRIDGE = path.join(__dirname, '..');
const MILLS = path.join(BRIDGE, '..');
const KEEP = process.argv.includes('--keep');
const NO_PROD = process.argv.includes('--no-prod');
const WIN = process.platform === 'win32';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha = (f) => (fs.existsSync(f) ? crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex') : 'missing');

const results = [];
function check(id, label, pass, detail = '') {
  results.push({ id, label, status: pass ? 'PASS' : 'FAIL', detail });
  console.log(`${pass ? 'PASS' : 'FAIL'} ${id.padEnd(4)} ${label}${detail ? '  — ' + detail : ''}`);
}
function skip(id, label, why) {
  results.push({ id, label, status: 'SKIP', detail: why });
  console.log(`SKIP ${id.padEnd(4)} ${label}  — ${why}`);
}

// ---------- production snapshot (read-only) ----------
// The running hub and its account files live in the MAIN checkout (the host folder), which is not this
// folder when the runner is started from someone's worktree: production checks always look there.
const { readJson } = require(path.join(BRIDGE, 'common.js'));
const PROD = path.join(require(path.join(BRIDGE, 'worktree.js')).REPO, 'bridge');
function prodProfiles() { // every production account: '' = the default one (bridge/ itself), then bridge/profiles/<name>
  const dir = path.join(PROD, 'profiles');
  const named = fs.existsSync(dir) ? fs.readdirSync(dir).filter((p) => /^[a-z0-9-]{1,32}$/.test(p) && ['config.json', 'auth.json'].some((f) => fs.existsSync(path.join(dir, p, f)))).sort() : [];
  return ['', ...named];
}
const prodDir = (p) => (p ? path.join(PROD, 'profiles', p) : PROD);
function prodAccountFiles() {
  return prodProfiles().flatMap((p) => [path.join(prodDir(p), 'auth.json'), ...(p ? [path.join(prodDir(p), 'config.json')] : [])]);
}
const prodBefore = Object.fromEntries(prodAccountFiles().map((f) => [f, sha(f)]));

// ---------- scratch hub ----------
function freePort(start) {
  return new Promise((resolve) => {
    const tryPort = (p) => {
      const s = net.createServer().once('error', () => tryPort(p + 1)).once('listening', () => s.close(() => resolve(p))).listen(p, '127.0.0.1');
    };
    tryPort(start);
  });
}
function listening(port) {
  return new Promise((resolve) => {
    const c = net.connect({ port, host: '127.0.0.1' }, () => { c.destroy(); resolve(true); });
    c.on('error', () => resolve(false));
    c.setTimeout(1500, () => { c.destroy(); resolve(false); });
  });
}

let SCRATCH, SB, PORT, H, hub = null, OLLAMA_PORT = 0, ollamaSrv = null;
const fakeImagePrompts = [], fakeVoiceReqs = [], fakeEdits = [];
const fakeForgejo = { issues: new Map(), n: 0, tick: 1, calls: [] };
let fakeSawPhoto = false;
// Fake Ollama for the Unity section's local brain: scripted tool calls, no GPU, no model.
//   user text "TASK <title>"  → add_task, then a say() line     "DELETE <id>" → delete_task (needs a yes)
//   "TIME" → get_time, then answers with the tool result          "REMIND"      → set_reminder in 1 second
// The last answer echoes whether the system prompt carried the person's name and time zone.
function startFakeOllama() {
  const http = require('http');
  ollamaSrv = http.createServer((req, res) => {
    let body = ''; req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const j = (o) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(o)); };
      if (req.url === '/api/version') return j({ version: 'fake' });
      // fake image engine on the same port: a 1x1 PNG for any prompt; the prompt is kept for checks
      if (req.url === '/health') return j({ ok: true, loaded: true });
      // fake web search (Bing-shaped) and YouTube results page
      if (req.url.startsWith('/search?')) { res.writeHead(200, { 'Content-Type': 'text/html' }); return res.end('<ol><li class="b_algo"><h2><a href="https://www.bing.com/ck/a?!&amp;u=a1' + Buffer.from('https://example.org/regress').toString('base64url') + '&amp;ntb=1">Regress <b>result</b></a></h2><p>snippet &amp; more</p></li></ol>'); }
      if (req.url.startsWith('/results?')) { res.writeHead(200, { 'Content-Type': 'text/html' }); return res.end('var x = {"videoRenderer":{"videoId":"abcdefghijk","thumbnail":{},"title":{"runs":[{"text":"Regress Song"}]}}}'); }
      // fake voice + speech engines on the same port too
      if (req.url === '/say') { fakeVoiceReqs.push(JSON.parse(body || '{}')); res.writeHead(200, { 'Content-Type': 'audio/wav' }); return res.end(Buffer.from('RIFF0000WAVEfake')); }
      if (req.url === '/transcribe') return j({ text: body.length ? 'fake heard ' + body.length : '', language: 'en' });
      if (req.url === '/img2img' || req.url === '/inpaint') fakeEdits.push({ route: req.url, ...JSON.parse(body || '{}') });
      if (req.url === '/txt2img' || req.url === '/img2img' || req.url === '/inpaint') {
        if (req.url === '/txt2img') fakeImagePrompts.push(JSON.parse(body || '{}').prompt || '');
        const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64');
        res.writeHead(200, { 'Content-Type': 'image/png', 'X-Seconds': '0.01' }); return res.end(png);
      }
      // fake OpenAI-compatible endpoint (a person's own "openai" provider): echoes the last user line, streams on request
      if (req.url === '/oai/chat/completions') {
        const b = JSON.parse(body || '{}'), last = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' }, sys = (b.messages || []).find((m) => m.role === 'system');
        const text = 'oai says: ' + last.content + (sys ? ' [sys ' + sys.content.length + ']' : '');
        if (b.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); for (const w of text.split(' ')) res.write('data: ' + JSON.stringify({ model: b.model, choices: [{ delta: { content: w + ' ' } }] }) + '\n\n'); return res.end('data: [DONE]\n\n'); }
        return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: text } }], usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } });
      }
      // fake Forgejo (issue mirroring): in-memory issues under the repo path; /fake/forgejo/* lets the runner act as a person on Forgejo
      const FJ = '/api/v1/repos/UnityAILab/Mills/issues';
      if (req.url.startsWith(FJ) || req.url.startsWith('/fake/forgejo/')) {
        fakeForgejo.calls.push(req.method + ' ' + req.url);
        const b = body ? JSON.parse(body) : {};
        const stamp = () => new Date(Date.now() + (fakeForgejo.tick++)).toISOString();
        const mk = (o) => { const n = ++fakeForgejo.n; const is = { number: n, title: o.title || '', body: o.body || '', state: o.state || 'open', labels: (o.labels || []).map((l) => ({ name: l })), html_url: 'https://git.example.invalid/UnityAILab/Mills/issues/' + n, updated_at: stamp(), created_at: stamp() }; fakeForgejo.issues.set(n, is); return is; };
        if (req.headers.authorization !== 'token fake-forgejo-token' && !req.url.startsWith('/fake/')) { res.writeHead(401); return res.end('{"message":"token"}'); }
        if (req.url === FJ && req.method === 'POST') return j(mk(b));
        if (req.url === '/fake/forgejo/newissue' && req.method === 'POST') return j(mk(b)); // made "on Forgejo" by a person
        let m = new RegExp('^' + FJ.replace(/\//g, '\\/') + '\\/(\\d+)(\\/comments)?$').exec(req.url) || /^\/fake\/forgejo\/issue\/(\d+)$/.exec(req.url);
        if (m) {
          const is = fakeForgejo.issues.get(Number(m[1])); if (!is) { res.writeHead(404); return res.end('{}'); }
          if (m[2]) { (is.comments || (is.comments = [])).push(b.body); return j({ id: 1, body: b.body }); }
          if (req.method === 'PATCH') { Object.assign(is, b.title !== undefined ? { title: b.title } : {}, b.body !== undefined ? { body: b.body } : {}, b.state !== undefined ? { state: b.state } : {}); is.updated_at = stamp(); return j(is); }
          return j(is);
        }
        if (req.url.startsWith(FJ + '?') && req.method === 'GET') { const since = Date.parse(new URL('http://x' + req.url).searchParams.get('since') || 0) || 0; return j([...fakeForgejo.issues.values()].filter((is) => Date.parse(is.updated_at) > since)); }
        res.writeHead(404); return res.end('{}');
      }
      if (req.url === '/api/tags') return j({ models: [{ name: 'unity-brain:latest' }] });
      if (req.url === '/api/show') return j({ capabilities: ['completion', 'tools'] });
      if (req.url !== '/api/chat') { res.writeHead(404); return res.end(); }
      const b = JSON.parse(body || '{}'), msgs = b.messages || [], last = msgs[msgs.length - 1] || {};
      const sys = (msgs[0] || {}).content || '';
      const user = [...msgs].reverse().find((m) => m.role === 'user') || { content: '' };
      if (Array.isArray(user.images) && user.images.length) fakeSawPhoto = true;
      if (/SLOW/.test(user.content) && last.role === 'user') return setTimeout(() => { res.writeHead(200, { 'Content-Type': 'application/x-ndjson' }); res.end(JSON.stringify({ message: { role: 'assistant', content: 'slow answer ' + user.content.slice(-3000) }, done: true }) + '\n'); }, 4000);
      res.writeHead(200, { 'Content-Type': 'application/x-ndjson' });
      const line = (m) => res.write(JSON.stringify({ message: { role: 'assistant', content: '', ...m }, done: false }) + '\n');
      const call = (name, args) => line({ tool_calls: [{ function: { name, arguments: args } }] });
      if (last.role === 'user') {
        const u = user.content, m = /TASK (.+)/.exec(u), d = /DELETE (\S+)/.exec(u);
        if (/PCLIST/.test(u)) call('pc_list', { path: 'docs' });
        else if (/RUNPY/.test(u)) call('run_code', { language: 'python', code: 'print(6*7)' });
        else if (/DELME/.test(u)) call('pc_delete', { path: 'old.txt' });
        else if (/SEARCHME/.test(u)) call('web_search', { query: 'regress query' });
        else if (/PLAYME/.test(u)) call('play_youtube', { query: 'regress song' });
        else if (/README/.test(u)) call('read_page', { url: 'http://127.0.0.1/secret' });
        else if (/EDITME/.test(u)) call('edit_image', { image: 'camera', prompt: 'regress edit scene', keep_face: true });
        else if (/PAINTME/.test(u)) call('inpaint_image', { image: 'last', prompt: 'a red hat', box: [0.1, 0.1, 0.5, 0.5] });
        else if (/SELFIE/.test(u)) call('generate_image', { prompt: 'regress moment at the desk', selfie: true, shape: 'portrait' });
        else if (m) call('add_task', { title: m[1] });
        else if (d) call('delete_task', { task_id: d[1] });
        else if (/TIME/.test(u)) call('get_time', {});
        else if (/REMIND/.test(u)) call('set_reminder', { text: 'regress reminder', when: 'in 1 second' });
        else line({ content: 'plain answer for ' + u.slice(-3000) });
      } else if (last.role === 'tool' && last.tool_name !== 'say') {
        call('say', { text: 'done: ' + last.content.slice(0, 160), emotion: 'happy', act: 'statement' });
      } else {
        line({ content: `final sys-has-name=${/Mills username "t-user"|Mills username "t-admin"/.test(sys)} tz=${(/\(([A-Za-z_]+\/[A-Za-z_]+)\)/.exec(sys) || [])[1] || ''}` });
      }
      res.end(JSON.stringify({ done: true }) + '\n');
    });
  });
  return new Promise((r) => ollamaSrv.listen(0, '127.0.0.1', () => { OLLAMA_PORT = ollamaSrv.address().port; r(); }));
}
async function setup() {
  SCRATCH = fs.mkdtempSync(path.join(os.tmpdir(), 'mills-test-'));
  SB = path.join(SCRATCH, 'bridge');
  fs.mkdirSync(path.join(SB, 'test'), { recursive: true });
  for (const f of fs.readdirSync(BRIDGE)) {
    if ((f.endsWith('.js') && f !== 'master-logins.js') || f === 'cloudflared-quick.yml') fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  }
  fs.copyFileSync(path.join(__dirname, 'fake-claude.js'), path.join(SB, 'test', 'fake-claude.js'));
  fs.cpSync(path.join(MILLS, 'web'), path.join(SCRATCH, 'web'), { recursive: true });
  PORT = await freePort(7799);
  H = `http://127.0.0.1:${PORT}`;
  const real = readJson(path.join(BRIDGE, 'config.json'), {});
  // The scratch copy is its own git repo (main + develop) so per-account worktrees work there.
  const g = (...a) => execFileSync('git', a, { cwd: SCRATCH, stdio: 'ignore', windowsHide: true });
  fs.writeFileSync(path.join(SCRATCH, '.gitignore'), ['bridge/profiles/', 'bridge/profiles-deleted/', 'bridge/*.json', 'bridge/.restart', 'Chat/', 'Archive/', 'worktrees/', 'hub.log', ''].join('\n'));
  g('init', '-q', '-b', 'main'); g('config', 'user.email', 'regress@example.invalid'); g('config', 'user.name', 'regress');
  g('add', '-A'); g('commit', '-q', '-m', 'scratch'); g('branch', 'develop');
  await startFakeOllama();
  const cfg = { ...real, assistant: { ...(real.assistant || {}), ollamaUrl: `http://127.0.0.1:${OLLAMA_PORT}`, imageUrl: `http://127.0.0.1:${OLLAMA_PORT}`, voiceUrl: `http://127.0.0.1:${OLLAMA_PORT}`, speechUrl: `http://127.0.0.1:${OLLAMA_PORT}`, searchUrl: `http://127.0.0.1:${OLLAMA_PORT}/search`, youtubeUrl: `http://127.0.0.1:${OLLAMA_PORT}/results` }, port: PORT, tunnel: false, gistId: '', worktreeRoot: path.join(SCRATCH, 'worktrees'), claudeCommand: process.execPath, claudeArgs: [path.join(SB, 'test', 'fake-claude.js')] };
  fs.writeFileSync(path.join(SB, 'config.json'), JSON.stringify(cfg, null, 2));
  // the scratch hub mirrors its "team" board to the fake Forgejo (a 3-second poll: the minimum)
  fs.writeFileSync(path.join(SB, 'forgejo.json'), JSON.stringify({ url: `http://127.0.0.1:${OLLAMA_PORT}`, token: 'fake-forgejo-token', repo: 'UnityAILab/Mills', board: 'team', pollMinutes: 0.01 }));
  const sp = (args) => spawnSync(process.execPath, [path.join(SB, 'set-password.js'), ...args], { cwd: SB, encoding: 'utf8' });
  sp(['pw-default-0001']);
  for (const [name, conf] of [['t-admin', { user: 'Tadmin', admin: true }], ['t-user', { user: 'Tuser' }]]) {
    fs.mkdirSync(path.join(SB, 'profiles', name), { recursive: true });
    fs.writeFileSync(path.join(SB, 'profiles', name, 'config.json'), JSON.stringify(conf));
    sp(['--profile', name, `pw-${name}-0001`]);
  }
}
async function startHub() {
  const log = fs.openSync(path.join(SCRATCH, 'hub.log'), 'a');
  hub = spawn(process.execPath, [path.join(SB, 'run.js'), '--no-tunnel'], { cwd: SB, stdio: ['ignore', log, log], windowsHide: true });
  for (let i = 0; i < 40; i++) { if ((await call('/api/status')).status === 200) return true; await sleep(250); }
  return false;
}
function stopHub() {
  if (!hub) return;
  try { if (WIN) execFileSync('taskkill', ['/pid', String(hub.pid), '/T', '/F'], { stdio: 'ignore' }); else hub.kill('SIGTERM'); } catch {}
  hub = null;
}

// ---------- http helpers ----------
async function call(p, token, body, extraHeaders = {}) {
  try {
    const r = await fetch(H + p, {
      method: body !== undefined ? 'POST' : 'GET',
      headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: 'Bearer ' + token } : {}), ...extraHeaders },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    return { status: r.status, data: await r.json().catch(() => ({})) };
  } catch (e) { return { status: 0, data: { error: e.message } }; }
}
const login = (username, password, deviceId) => call('/api/login', null, { username, password, deviceId });
// Run a chat to completion; answers pc_tool events like a browser would.
async function chat(token, message, opts = {}) {
  const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }, body: JSON.stringify({ message, fresh: !!opts.fresh, ...(opts.context ? { context: opts.context } : {}) }) });
  if (r.status !== 200) return { status: r.status, events: [] };
  const events = []; let buf = '';
  const reader = r.body.getReader(), dec = new TextDecoder();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, i); buf = buf.slice(i + 1);
      if (!line.trim()) continue;
      const ev = JSON.parse(line); events.push(ev);
      if (ev.type === 'pc_tool') call('/api/pc-result', token, { id: ev.id, ok: true, result: 'fake-listing: a.txt' });
      if (opts.onEvent) opts.onEvent(ev);
    }
  }
  return { status: 200, events };
}
const texts = (events) => events.filter((e) => e.type === 'assistant').flatMap((e) => e.message.content.map((c) => c.text)).join('\n');

// ---------- chat crypto (same Web Crypto calls as web/index.html) ----------
const subtle = globalThis.crypto.subtle;
const rand = (n) => { // getRandomValues refuses more than 65,536 bytes per call
  const b = new Uint8Array(n);
  for (let i = 0; i < n; i += 65536) globalThis.crypto.getRandomValues(b.subarray(i, i + 65536));
  return b;
};
const enc = new TextEncoder(), dec = new TextDecoder();
const toB64 = (b) => Buffer.from(b).toString('base64'), fromB64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));
const ECDH = { name: 'ECDH', namedCurve: 'P-256' };
async function pwKey(pw, salt) { const b = await subtle.importKey('raw', enc.encode(pw), 'PBKDF2', false, ['deriveKey']); return subtle.deriveKey({ name: 'PBKDF2', salt, iterations: 310000, hash: 'SHA-256' }, b, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']); }
async function lock(priv, pw) { const salt = rand(16), iv = rand(12); const ct = await subtle.encrypt({ name: 'AES-GCM', iv }, await pwKey(pw, salt), await subtle.exportKey('pkcs8', priv)); return { salt: toB64(salt), iv: toB64(iv), ct: toB64(ct) }; }
async function unlock(b, pw) { const p = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(b.iv) }, await pwKey(pw, fromB64(b.salt)), fromB64(b.ct)); return subtle.importKey('pkcs8', p, ECDH, true, ['deriveKey']); }
const ownerOf = new WeakMap(); // private key → username (the v3 wrap is bound to the reader's name)
async function wrapV3(priv, jwk) { // the page's wrapWith: HKDF-SHA256 over the ECDH secret, label "mills-v3 wrap"
  const bitsKey = await subtle.importKey('pkcs8', await subtle.exportKey('pkcs8', priv), ECDH, false, ['deriveBits']);
  const bits = await subtle.deriveBits({ name: 'ECDH', public: await subtle.importKey('jwk', { kty: 'EC', crv: 'P-256', x: jwk.x, y: jwk.y }, ECDH, false, []) }, bitsKey, 256);
  return subtle.deriveKey({ name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: enc.encode('mills-v3 wrap') }, await subtle.importKey('raw', bits, 'HKDF', false, ['deriveKey']), { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}
const shared = async (priv, jwk) => subtle.deriveKey({ name: 'ECDH', public: await subtle.importKey('jwk', { kty: 'EC', crv: 'P-256', x: jwk.x, y: jwk.y }, ECDH, false, []) }, priv, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);

// ---------- the checklist ----------
async function run() {
  const devA = 'device-aaaaaaaaaaaaaaaa', devB = 'device-bbbbbbbbbbbbbbbb', devC = 'device-cccccccccccccccc';

  // Hub & sign-in
  const scripts = fs.readdirSync(BRIDGE).filter((f) => f.endsWith('.js')).map((f) => path.join(BRIDGE, f)).concat(fs.readdirSync(__dirname).map((f) => path.join(__dirname, f)));
  const bad = scripts.filter((f) => spawnSync(process.execPath, ['--check', f]).status !== 0);
  let pageOk = true, pageErr = '';
  // `html` = the page plus the page-side scripts it loads (web/calls.js), so the static checks below keep
  // covering code that moved out of index.html into its own file.
  const indexHtml = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
  const PAGE_SCRIPTS = ['calls.js', 'guilds.js', 'notes.js', 'app.js'];
  const html = indexHtml + '\n' + PAGE_SCRIPTS.map((f) => fs.readFileSync(path.join(MILLS, 'web', f), 'utf8')).join('\n');
  const inlineStart = indexHtml.indexOf('<script>') + 8, inlineEnd = indexHtml.indexOf('</script>', inlineStart);
  try { new Function(indexHtml.slice(inlineStart, inlineEnd)); } catch (e) { pageOk = false; pageErr = 'page: ' + e.message; }
  for (const f of ['md.js', 'board.js', 'sw.js', ...PAGE_SCRIPTS]) if (spawnSync(process.execPath, ['--check', path.join(MILLS, 'web', f)]).status !== 0) bad.push(f);
  for (const f of PAGE_SCRIPTS) if (!indexHtml.includes(`<script src="${f}"></script>`)) { pageOk = false; pageErr += ' ' + f + ' not loaded'; }
  check('R1', 'every bridge script passes node --check; page script (and web/md.js) parses', !bad.length && pageOk, [bad.map((f) => path.basename(f)).join(', '), pageErr].filter(Boolean).join(' '));

  const st = await call('/api/status');
  const log = fs.readFileSync(path.join(SCRATCH, 'hub.log'), 'utf8');
  check('R2', 'hub starts, /api/status ok, startup log lists every account', st.status === 200 && ['mills', 't-admin', 't-user'].every((u) => log.includes(u)));

  const A = (await login('T-Admin', 'pw-t-admin-0001', devA)).data.token;
  const U = (await login('t-user', 'pw-t-user-0001', devB)).data.token;
  const D = (await login('mills', 'pw-default-0001', devC)).data.token;
  check('R3', 'correct username + password → token (case-insensitive username)', !!A && !!U && !!D);

  const w1 = await login('t-user', 'wrong-password-1', devB), w2 = await login('ghost', 'wrong-password-1', devB);
  check('R4', 'wrong password and unknown user → 401 with the same message', w1.status === 401 && w2.status === 401 && w1.data.error === w2.data.error);

  for (let i = 0; i < 5; i++) await login('nobody-x', 'wrong-password', devB);
  const blocked = await login('nobody-x', 'wrong-password', devB), other = await login('t-user', 'pw-t-user-0001', devB);
  check('R5', '5 failures → 429 for that username only', blocked.status === 429 && other.status === 200);

  const locked = await login('t-user', 'pw-t-user-0001', devA);
  check('R6', 'right password from a different device → 403 locked', locked.status === 403 && /locked to another device/.test(locked.data.error || ''));

  const evil = await call('/api/status', null, undefined, { Origin: 'https://evil.example' });
  check('R7', 'request from a non-allowed Origin → 403', evil.status === 403);

  const tmp = (await login('t-user', 'pw-t-user-0001', devB)).data.token;
  await call('/api/logout', tmp, {});
  check('R8', 'logout → old token 401', (await call('/api/check', tmp)).status === 401);
  // Nothing without a sign-in: every API route the hub source mentions (server.js, board.js, assistant.js,
  // so a new route is covered the day it is written) answers 401 to a visitor, GET and POST, except the
  // sign-in itself and the bare "a hub is here" status; through the tunnel that status carries no pid /
  // busy / supervised; unknown paths and folders are 404; and the page ships with only the sign-in form visible.
  const routeSrc = ['server.js', 'board.js', 'assistant.js', 'openai.js', 'team.js'].map((f) => fs.readFileSync(path.join(BRIDGE, f), 'utf8')).join('\n');
  const routes = [...new Set([...routeSrc.matchAll(/'(\/api\/[a-z0-9/_-]+)'/g)].map((m) => m[1]))].filter((r) => r !== '/api/status' && r !== '/api/login' && !/\/$/.test(r));
  const leaks = [];
  for (const r of routes) {
    const g = await call(r), po = await call(r, null, {});
    if (g.status !== 401 || po.status !== 401) leaks.push(`${r} ${g.status}/${po.status}`);
  }
  const anonStatus = (await fetch(H + '/api/status', { headers: { 'cf-connecting-ip': '203.0.113.9' } }).then((r) => r.json()).catch(() => ({})));
  const localStatus = (await call('/api/status')).data;
  const notFound = (await fetch(H + '/nothing-here.html')).status, dirList = (await fetch(H + '/vendor/')).status, traversal = (await fetch(H + '/..%2f..%2fbridge%2fconfig.json')).status;
  const hiddenScreens = ['menu', 'shell', 'room', 'admin', 'board', 'cal', 'assist', 'settings', 'roomFloat', 'callFloat', 'archive'].filter((id) => !new RegExp(`id="${id}"[^>]*\\bhidden\\b`).test(html) && !new RegExp(`<[a-z]+ [^>]*\\bhidden\\b[^>]*id="${id}"`).test(html));
  check('R110', `login gate: ${routes.length} API routes from the hub source answer 401 without a sign-in (GET + POST); anonymous status through the tunnel carries no pid / busy / supervised; unknown paths, folder listings and path traversal → 404; [static] every screen but the sign-in form ships hidden`,
    routes.length >= 50 && !leaks.length && anonStatus.ok === true && anonStatus.pid === undefined && anonStatus.busy === undefined && anonStatus.supervised === undefined && typeof localStatus.pid === 'number'
    && notFound === 404 && dirList === 404 && traversal === 404 && !hiddenScreens.length && /<form id="login"/.test(html),
    [leaks.slice(0, 6).join(', '), hiddenScreens.length ? 'visible: ' + hiddenScreens.join(' ') : '', `404s ${notFound}/${dirList}/${traversal}`].filter(Boolean).join(' | '));

  // Work
  const ck = (await call('/api/check', A)).data;
  check('R12', '/api/check returns username, display, admin, models, model, defaultModel', ck.username === 't-admin' && ck.admin === true && Array.isArray(ck.models) && 'model' in ck && 'defaultModel' in ck);

  const m1 = await call('/api/model', U, { model: 'claude-opus-5-5' }), m2 = await call('/api/model', U, { model: 'gpt-9' });
  const c1 = await chat(U, 'hello model');
  check('R13', 'model: valid saved and used by the next run, unknown → 400', m1.status === 200 && m2.status === 400 && /model=claude-opus-5-5/.test(texts(c1.events)));

  const c2 = await chat(A, 'first message');
  const sid = (c2.events.find((e) => e.type === 'system') || {}).session_id;
  const c3 = await chat(A, 'second message');
  const resumed = (c3.events.find((e) => e.type === 'system') || {}).session_id === sid;
  let busy409 = false, stopped = false;
  const slow = chat(A, 'SLOW stop me');
  await sleep(700);
  busy409 = (await call('/api/chat', A, { message: 'x' })).status === 409;
  await call('/api/stop', A, {});
  const slowRes = await slow;
  stopped = !slowRes.events.some((e) => e.type === 'result');
  check('R14', 'chat streams NDJSON, resumes the session, concurrent → 409, stop ends the run', c2.events.some((e) => e.type === 'result') && resumed && busy409 && stopped);

  const pc = await chat(U, 'PCTOOL please');
  check('R15', 'pc tool relay reaches the right browser and returns its result', /fake-listing: a\.txt/.test(texts(pc.events)));

  const sv = await call('/api/archive/save', U, { title: 'reg test', transcript: [{ cls: 'me', text: 'hi' }] });
  const lu = (await call('/api/archive/list', U, {})).data.items || [], la = (await call('/api/archive/list', A, {})).data.items || [];
  const rn = await call('/api/archive/rename', U, { id: sv.data.id, title: 'renamed' });
  const op = await call('/api/archive/open', U, { id: sv.data.id });
  const dl = await call('/api/archive/delete', U, { id: sv.data.id });
  const gone = !((await call('/api/archive/list', U, {})).data.items || []).length;
  check('R16', 'archive save/list/rename/open/delete per account, invisible to others', sv.status === 200 && lu.length === 1 && !la.some((x) => x.id === sv.data.id) && rn.data.title === 'renamed' && op.status === 200 && dl.status === 200 && gone);
  // Search over past sessions: titles + full transcripts, every word must match, snippets around hits, own sessions only.
  const s1 = await call('/api/archive/save', U, { title: 'Deploy notes', transcript: [{ cls: 'me', text: 'how do I deploy the hub' }, { cls: 'claude', text: 'Run restart.js after the needle-r75 step, then verify.' }] });
  const s2 = await call('/api/archive/save', U, { title: 'Other chat', transcript: [{ cls: 'me', text: 'nothing relevant here' }] });
  const f1 = (await call('/api/archive/search', U, { q: 'needle-r75' })).data.items || [];
  const f2 = (await call('/api/archive/search', U, { q: 'DEPLOY verify' })).data.items || []; // case-insensitive, both words, across title + transcript
  const f3 = (await call('/api/archive/search', U, { q: 'deploy zzz-absent' })).data.items || [];
  const f4 = (await call('/api/archive/search', A, { q: 'needle-r75' })).data.items || [];
  const f5 = (await call('/api/archive/search', U, { q: '' })).data.items || [];
  check('R104', 'past sessions search: matches titles and everything in the transcript (all words, any case), ranked by hits with snippets saying who said it, nothing from other accounts, empty query = the plain list; [static] search box + highlighted matches',
    s1.status === 200 && s2.status === 200 && f1.length === 1 && f1[0].id === s1.data.id && f1[0].hits === 1 && f1[0].snippets.length === 1 && f1[0].snippets[0].cls === 'claude' && /needle-r75/.test(f1[0].snippets[0].text)
    && f2.length === 1 && f2[0].id === s1.data.id && !f3.length && !f4.length && f5.length === 2
    && /id="archiveQ"/.test(html) && /function markText\(/.test(html) && /\/api\/archive\/search/.test(html),
    `needle ${f1.length}, two words ${f2.length}, absent ${f3.length}, other account ${f4.length}, empty ${f5.length}`);
  for (const s of [s1, s2]) await call('/api/archive/delete', U, { id: s.data.id });

  // Page (static: the runner has no browser)
  const hasIds = ['username', 'password', 'menu', 'menuUser', 'goWork', 'goChat', 'goAdmin', 'room', 'admin', 'shell', 'model', 'stop', 'send'].every((id) => html.includes(`id="${id}"`));
  check('R17', '[static] input never disabled while busy; Queue / Send now present', !/\$\('input'\)\.disabled\s*=/.test(html) && html.includes("'Queue'") && html.includes("'Send now'"));
  check('R18', '[static] page retries 502/503/504/530 and re-finds the hub address', /\[502, 503, 504, 530\]/.test(html) && /findBridge\(\)/.test(html));
  check('R19', '[static] login form has username + password; ?user= only pre-fills', hasIds && /get\('user'\)/.test(html) && !/mills-bridge-\$\{/.test(html));
  check('R20', '[static] menu with Work/Chat, Admin hidden unless admin, ← Menu in three screens', (html.match(/data-back/g) || []).length >= 4 && /goAdmin'\)\.hidden = !me\.admin/.test(html));

  // Chat (E2E)
  const pairs = {};
  for (const [who, tok, pw] of [['t-admin', A, 'pw-t-admin-0001'], ['t-user', U, 'pw-t-user-0001']]) {
    const kp = await subtle.generateKey(ECDH, true, ['deriveKey']); const pub = await subtle.exportKey('jwk', kp.publicKey);
    pairs[who] = { priv: kp.privateKey, pub, up: await call('/api/room/mykey', tok, { pub: { kty: 'EC', crv: 'P-256', x: pub.x, y: pub.y }, backup: await lock(kp.privateKey, pw) }) };
  }
  const withD = await call('/api/room/mykey', A, { pub: { ...pairs['t-admin'].pub, d: 'secret' }, backup: { salt: 'a', iv: 'b', ct: 'c' } });
  check('R21', 'key upload accepted; JWK with a private part rejected', pairs['t-admin'].up.status === 200 && pairs['t-user'].up.status === 200 && withD.status === 400);
  const bk = (await call('/api/room/mykey', U)).data; let wrongFails = false;
  const restored = await unlock(bk.backup, 'pw-t-user-0001');
  try { await unlock(bk.backup, 'nope'); } catch { wrongFails = true; }
  check('R22', 'key backup opens with the password, not with a wrong one', !!restored && wrongFails);
  const members = (await call('/api/room/members', A)).data.members;
  const raw = rand(32), iv = rand(12), k = await subtle.importKey('raw', raw, 'AES-GCM', false, ['encrypt']);
  const ct = await subtle.encrypt({ name: 'AES-GCM', iv }, k, enc.encode('regression secret text'));
  const keys = {};
  for (const m of members) if (m.pub) { const kiv = rand(12); keys[m.username] = { iv: toB64(kiv), wk: toB64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv }, await shared(pairs['t-admin'].priv, m.pub), raw)) }; }
  await call('/api/room/send', A, { iv: toB64(iv), ct: toB64(ct), keys });
  const got = ((await call('/api/room/messages?since=0', U)).data.messages || []).at(-1);
  let text = '';
  try {
    const mk = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(got.key.iv) }, await shared(restored, got.fromPub), fromB64(got.key.wk));
    text = dec.decode(await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(got.iv) }, await subtle.importKey('raw', mk, 'AES-GCM', false, ['decrypt']), fromB64(got.ct)));
  } catch {}
  check('R23', 'A→B message decrypts with a restored key; reader gets only its own slot', text === 'regression secret text' && got && !got.keys);
  const chatFiles = fs.readdirSync(path.join(SCRATCH, 'Chat')).map((f) => fs.readFileSync(path.join(SCRATCH, 'Chat', f), 'utf8')).join('');
  check('R24', 'plaintext never in Chat/ files', !chatFiles.includes('regression secret text'));

  // Room agent (fake Claude answers "echo: <prompt>")
  const roomSend = async (tok, priv, plain) => {
    const mem = (await call('/api/room/members', tok)).data.members, r2 = rand(32), iv2 = rand(12);
    const body = await subtle.encrypt({ name: 'AES-GCM', iv: iv2 }, await subtle.importKey('raw', r2, 'AES-GCM', false, ['encrypt']), enc.encode(plain));
    const ks = {};
    for (const m of mem) if (m.pub) { const kiv = rand(12); ks[m.username] = { iv: toB64(kiv), wk: toB64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv }, await shared(priv, m.pub), r2)) }; }
    return call('/api/room/send', tok, { iv: toB64(iv2), ct: toB64(body), keys: ks });
  };
  const roomRead = async (priv, m) => {
    try {
      if (m.v === 3) { // HKDF wrap key, sender/recipient/mid (+ reader) as authenticated data, padded plaintext
        const aad = 'mills-v3|' + m.from + '|' + (m.to || '') + '|' + m.mid;
        const mk = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.key.iv), additionalData: enc.encode(aad + '|' + ownerOf.get(priv)) }, await wrapV3(priv, m.fromPub), fromB64(m.key.wk));
        return dec.decode(await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.iv), additionalData: enc.encode(aad) }, await subtle.importKey('raw', mk, 'AES-GCM', false, ['decrypt']), fromB64(m.ct))).trimEnd();
      }
      const mk = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.key.iv) }, await shared(priv, m.fromPub), fromB64(m.key.wk));
      return dec.decode(await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.iv) }, await subtle.importKey('raw', mk, 'AES-GCM', false, ['decrypt']), fromB64(m.ct)));
    } catch { return null; }
  };
  const agentMsgs = async (tok, priv, after) => {
    const out = [];
    for (const m of (await call('/api/room/messages?since=' + after, tok)).data.messages || []) if (m.from === 'claude') out.push({ seq: m.seq, text: await roomRead(priv, m) });
    return out;
  };
  const waitAgent = async (tok, priv, after, test, ms = 15000) => {
    for (let t = 0; t < ms; t += 400) { const hit = (await agentMsgs(tok, priv, after)).find((x) => x.text && test(x.text)); if (hit) return hit; await sleep(400); }
    return null;
  };
  const privA = pairs['t-admin'].priv;
  ownerOf.set(privA, 't-admin'); ownerOf.set(restored, 't-user');
  const agentMember = ((await call('/api/room/members', U)).data.members || []).find((m) => m.username === 'claude');
  check('R35a', 'room agent listed in members (agent: true, public key)', !!(agentMember && agentMember.agent && agentMember.pub && agentMember.pub.x));
  const greet = await waitAgent(U, restored, 0, (t) => /just signed in/.test(t), 30000);
  check('R38', 'sign-in greeting from the agent (Unity) that the person decrypts', !!greet && !!agentMember && agentMember.display === 'Unity');
  await sleep(2500); // let any queued greeting run finish
  const seq0 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await roomSend(U, restored, 'just chatting, nobody named');
  await sleep(3000);
  const quiet = !(await agentMsgs(U, restored, seq0)).length;
  await roomSend(U, restored, '@claude not-the-handle-r36'); // only its own @-handle calls it now
  await sleep(3000);
  const quiet2 = !(await agentMsgs(U, restored, seq0)).length;
  await roomSend(U, restored, '@unity ping-r36');
  const ans = await waitAgent(U, restored, seq0, (t) => t.includes('ping-r36'));
  const ansA = ans && (await agentMsgs(A, privA, seq0)).find((x) => x.seq === ans.seq);
  await roomSend(A, privA, '@Unity ping-unity');
  const viaUnity = await waitAgent(A, privA, seq0, (t) => t.includes('ping-unity'));
  check('R36', 'unnamed message → no reply; @claude → no reply; @unity → one reply every member decrypts (any capitalisation)', quiet && quiet2 && !!ans && !!(ansA && ansA.text) && !!viaUnity);
  await sleep(1500);
  const pidR = (await call('/api/status')).data.pid;
  await roomSend(U, restored, '@unity SLOW ping-r37');
  let busySeen = false;
  for (let i = 0; i < 10 && !busySeen; i++) { await sleep(200); busySeen = (await call('/api/status')).data.busy === true; }
  fs.writeFileSync(path.join(SB, '.restart'), 'test');
  await sleep(2000);
  const pidMid = (await call('/api/status')).data.pid;
  const slowAns = await waitAgent(U, restored, seq0, (t) => t.includes('ping-r37'), 20000);
  let pidAfter = pidMid;
  for (let i = 0; i < 60 && (pidAfter === pidR || !pidAfter); i++) { await sleep(250); pidAfter = (await call('/api/status')).data.pid; }
  check('R37', 'agent reply makes /api/status busy; a restart waits for it, then happens', busySeen && pidMid === pidR && !!slowAns && !!pidAfter && pidAfter !== pidR);
  const memAfter = ((await call('/api/room/members', U)).data.members || []).find((m) => m.username === 'claude');
  check('R35b', 'room agent key survives the restart', !!memAfter && !!agentMember && memAfter.pub.x === agentMember.pub.x);

  // Per-account worktrees (the scratch copy is its own git repo)
  const cwdOf = (events) => ((texts(events).match(/cwd=(.+?)(?: \||$)/m) || [])[1] || '').trim();
  const samePath = (a, b) => !!a && path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();
  const wtU = path.join(SCRATCH, 'worktrees', 't-user');
  const ckU = (await call('/api/check', U)).data;
  const ownRun = await chat(U, 'where am I', { fresh: true });
  const branchU = fs.existsSync(wtU) ? execFileSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: wtU, encoding: 'utf8' }).trim() : '';
  check('R39', 'default workspace "own": run happens in worktrees/<username> on work/<username>', ckU.workspace === 'own' && samePath(cwdOf(ownRun.events), wtU) && branchU === 'work/t-user', cwdOf(ownRun.events));
  const badWs = await call('/api/workspace', U, { workspace: 'elsewhere' });
  const toShared = await call('/api/workspace', U, { workspace: 'shared' });
  const sharedRun = await chat(U, 'where am I now', { fresh: true });
  const slowWs = chat(U, 'SLOW hold'); await sleep(700);
  const busyWs = await call('/api/workspace', U, { workspace: 'own' });
  await call('/api/stop', U, {}); await slowWs;
  const backOwn = await call('/api/workspace', U, { workspace: 'own' });
  const keptWs = (await call('/api/check', U)).data.workspace, otherWs = (await call('/api/check', A)).data.workspace;
  check('R40', 'shared runs in the Mills folder; unknown → 400; while running → 409; back to own; per account', badWs.status === 400 && toShared.status === 200
    && samePath(cwdOf(sharedRun.events), SCRATCH) && busyWs.status === 409 && backOwn.status === 200 && samePath(backOwn.data.dir, wtU) && keptWs === 'own' && otherWs === 'own');
  const mg = { status: -1, stdout: '', stderr: 'no worktree' };
  let onDevelop = false, again = '', stOut = '', badUser = false;
  if (fs.existsSync(wtU)) {
    fs.writeFileSync(path.join(wtU, 'regress-note.txt'), 'from t-user\n');
    execFileSync('git', ['add', 'regress-note.txt'], { cwd: wtU }); execFileSync('git', ['commit', '-q', '-m', 'note'], { cwd: wtU });
    const wtJs = path.join(SB, 'worktree.js');
    Object.assign(mg, spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' }));
    onDevelop = spawnSync('git', ['show', 'develop:regress-note.txt'], { cwd: SCRATCH, encoding: 'utf8' }).stdout.includes('from t-user');
    again = spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' }).stdout;
    stOut = spawnSync(process.execPath, [wtJs, 'status'], { cwd: SB, encoding: 'utf8' }).stdout;
    badUser = spawnSync(process.execPath, [wtJs, 'ensure', '../evil'], { cwd: SB, encoding: 'utf8' }).status !== 0;
  }
  check('R41', 'worktree.js: merge brings work/<user> into develop; nothing-to-merge; status; bad username refused', mg.status === 0 && onDevelop && /Nothing to merge/.test(again) && /work\/t-user/.test(stOut) && badUser, String(mg.stdout || mg.stderr).trim().split('\n')[0]);
  let cm = { stdout: '', stderr: 'no worktree' }, cmMerged = false, cmClean = '';
  if (fs.existsSync(wtU)) {
    const wtJs = path.join(SB, 'worktree.js');
    fs.writeFileSync(path.join(wtU, 'regress-uncommitted.txt'), 'left uncommitted\n');
    cm = spawnSync(process.execPath, [wtJs, 'commit', 't-user', 'regress commit'], { cwd: wtU, encoding: 'utf8' });
    spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' });
    cmMerged = spawnSync('git', ['show', 'develop:regress-uncommitted.txt'], { cwd: SCRATCH, encoding: 'utf8' }).stdout.includes('left uncommitted');
    cmClean = spawnSync(process.execPath, [wtJs, 'commit', 't-user'], { cwd: SB, encoding: 'utf8' }).stdout;
  }
  check('R59', 'worktree.js: commit stages + commits uncommitted work in the worktree, merge then lands it; clean → nothing to commit', /Committed on work\/t-user/.test(cm.stdout) && cmMerged && /Nothing to commit/.test(cmClean), String(cm.stdout || cm.stderr).trim());
  const wtList = spawnSync('git', ['worktree', 'list', '--porcelain'], { cwd: SCRATCH, encoding: 'utf8' }).stdout;
  const devFree = !/branch refs\/heads\/develop/.test(wtList) && !/_merge-|_develop/.test(wtList);
  const syncBack = fs.existsSync(wtU) ? spawnSync(process.execPath, [path.join(SB, 'worktree.js'), 'sync', 't-user'], { cwd: SB, encoding: 'utf8' }).stdout : '';
  check('R60', 'worktree.js: merging leaves develop free (no leftover merge checkout locks it); sync brings develop back into work/<user>', devFree && /up to date with develop/.test(syncBack), devFree ? syncBack.trim() : wtList.replace(/\s+/g, ' '));
  // Going live: develop → main in the folder that has main checked out (here: the scratch repo itself), then nothing to ship.
  const lv = spawnSync(process.execPath, [path.join(SB, 'worktree.js'), 'live'], { cwd: SB, encoding: 'utf8' });
  const onMain = spawnSync('git', ['show', 'main:regress-note.txt'], { cwd: SCRATCH, encoding: 'utf8' }).stdout.includes('from t-user');
  const lvAgain = spawnSync(process.execPath, [path.join(SB, 'worktree.js'), 'live'], { cwd: SB, encoding: 'utf8' }).stdout;
  const mainClean = spawnSync('git', ['status', '--porcelain'], { cwd: SCRATCH, encoding: 'utf8' }).stdout.trim() === '';
  const wtSrc109 = fs.readFileSync(path.join(BRIDGE, 'worktree.js'), 'utf8');
  check('R109', 'worktree.js live: develop → main (--no-ff) in the checkout that has main, pushes every remote, reports the deploy; second run = nothing to ship; main folder left clean; no restart without --restart; --restart writes the flag into the MAIN checkout\'s bridge (never its own copy) and works with nothing to ship too',
    lv.status === 0 && /Merged develop into main/.test(lv.stdout) && onMain && /Nothing to ship/.test(lvAgain) && mainClean && !fs.existsSync(path.join(SB, '.restart'))
    && /path\.join\(at\.dir, 'bridge', '\.restart'\)/.test(wtSrc109) && !/loadProfile\(''\)\.RESTART_FLAG|const \{ RESTART_FLAG \} = loadProfile/.test(wtSrc109) && /\.\.\.\(opts\.restart \? \[restart\(\)\] : \[\]\)/.test(wtSrc109), String(lv.stdout || lv.stderr).trim().split('\n')[0]);

  // OpenAI-compatible API (/v1) with one key per person (bridge/openai.js), answered by the host's Claude (the fake one here).
  const v1 = async (p, key, body, headers = {}) => { const r = await fetch(H + p, { method: body !== undefined ? 'POST' : 'GET', headers: { 'Content-Type': 'application/json', ...(key ? { Authorization: 'Bearer ' + key } : {}), ...headers }, body: body !== undefined ? JSON.stringify(body) : undefined }); return { status: r.status, text: await r.text(), ct: r.headers.get('content-type') || '' }; };
  const k0 = (await call('/api/apikey', U)).data;
  const made = (await call('/api/apikey/rotate', U, { expiresDays: 7 })).data;
  const noKey = await v1('/v1/models'), badKey = await v1('/v1/models', 'mills_' + 'a'.repeat(48));
  const models = await v1('/v1/models', made.key);
  const chat1 = await v1('/v1/chat/completions', made.key, { model: 'claude-opus-5-5', messages: [{ role: 'system', content: 'be brief' }, { role: 'user', content: 'hi there' }, { role: 'assistant', content: 'hello' }, { role: 'user', content: 'ping-r111' }] });
  const oa1 = JSON.parse(chat1.text || '{}'), c1text = (((oa1.choices || [])[0] || {}).message || {}).content || '';
  const stream1 = await v1('/v1/chat/completions', made.key, { model: 'claude-haiku-5-5', stream: true, messages: [{ role: 'user', content: 'stream-r111' }] });
  const chunks = stream1.text.split('\n').filter((l) => l.startsWith('data: ') && l !== 'data: [DONE]').map((l) => JSON.parse(l.slice(6)));
  const streamed = chunks.map((c) => (((c.choices || [])[0] || {}).delta || {}).content || '').join('');
  const noMsgs = await v1('/v1/chat/completions', made.key, { model: 'x' });
  const afterUse = (await call('/api/apikey', U)).data;
  const rotated = (await call('/api/apikey/rotate', U, {})).data;
  const oldDead = await v1('/v1/models', made.key), newLive = await v1('/v1/models', rotated.key);
  // expiry: a 1-day key made "yesterday" is refused as expired (the runner backdates it in the scratch state file)
  const uState = path.join(SB, 'profiles', 't-user', 'state.json');
  const st1 = JSON.parse(fs.readFileSync(uState, 'utf8')); // (the hub keeps its own copy in memory; expiry is judged from createdAt + expiresDays at call time)
  const setExp = await call('/api/apikey/settings', U, { expiresDays: 1 });
  const stillOk = await v1('/v1/models', rotated.key);
  const revoked = (await call('/api/apikey/revoke', U, {})).data, afterRevoke = await v1('/v1/models', rotated.key);
  check('R111', 'OpenAI-compatible API: Settings makes a per-person key (shown once, sha256 kept), /v1/models + /v1/chat/completions answer with it (system + history flattened into one claude -p run, usage counted, streaming as SSE chunks ending in [DONE]); no key / unknown key → 401 in OpenAI error shape; bad body → 400; rotate kills the old key; expiry setting kept; revoke kills it',
    !k0.hasKey && /^mills_[a-f0-9]{48}$/.test(made.key || '') && made.hasKey && made.expiresDays === 7 && made.prefix === made.key.slice(0, 12)
    && noKey.status === 401 && /authentication_error/.test(noKey.text) && badKey.status === 401 && models.status === 200 && /claude-opus-5-5/.test(models.text)
    && chat1.status === 200 && oa1.object === 'chat.completion' && /ping-r111/.test(c1text) && /Conversation so far:[\s\S]*User: hi there[\s\S]*Assistant: hello/.test(c1text) && !/be brief/.test(c1text) && /model=claude-opus-5-5/.test(c1text) && oa1.usage && typeof oa1.usage.total_tokens === 'number'
    && stream1.status === 200 && /text\/event-stream/.test(stream1.ct) && /stream-r111/.test(streamed) && /model=claude-haiku-5-5/.test(streamed) && /data: \[DONE\]/.test(stream1.text)
    && noMsgs.status === 400 && afterUse.calls === 2 && !!afterUse.lastUsed
    && rotated.key && rotated.key !== made.key && oldDead.status === 401 && newLive.status === 200
    && st1.api && setExp.data.expiresDays === 1 && stillOk.status === 200 && revoked.hasKey === false && afterRevoke.status === 401
    && /id="apiBox"/.test(html) && /\/api\/apikey\/rotate/.test(html) && /function renderApiAccess\(/.test(html),
    `key ${made.key ? 'made' : 'none'}, models ${models.status}, chat ${chat1.status} "${c1text.slice(0, 60)}", stream ${stream1.status} ${chunks.length} chunks, calls ${afterUse.calls}, rotate ${oldDead.status}/${newLive.status}, revoke ${afterRevoke.status}`);

  // AI providers (bridge/providers.js): a person's own Claude login / key / OpenAI endpoint, private or shared; the host's Claude untouched.
  const prov0 = (await call('/api/providers', U)).data;
  const badKind = await call('/api/providers/add', U, { kind: 'magic', label: 'x' });
  const badAnth = await call('/api/providers/add', U, { kind: 'anthropic', label: 'bad', apiKey: 'nope' });
  const cfgDir = path.join(SCRATCH, 'claude-sponge-login');
  const cli = (await call('/api/providers/add', U, { kind: 'claude-cli', label: 'Tuser Claude login', configDir: cfgDir, shared: false })).data;
  const oai = (await call('/api/providers/add', U, { kind: 'openai', label: 'Tuser endpoint', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, apiKey: 'k', models: 'fake-gpt, fake-mini', shared: true })).data;
  const seenByA = (await call('/api/providers', A)).data.providers.map((p) => p.id);
  const secretLeak = JSON.stringify((await call('/api/providers', U)).data).includes('"apiKey"');
  const pickCli = await call('/api/model', U, { provider: cli.provider.id, model: 'claude-sonnet-5-5' });
  const cliRun = await chat(U, 'who runs me', { fresh: true });
  const ck7 = (await call('/api/check', U)).data;
  const pickOai = await call('/api/model', U, { provider: oai.provider.id, model: 'fake-gpt' });
  const useOai = (await call('/api/apikey/rotate', U, {})).data;
  const setOai = await call('/api/apikey/settings', U, { provider: oai.provider.id, model: 'fake-mini' });
  const oaiModels = await v1('/v1/models', useOai.key);
  const oaiChat = await v1('/v1/chat/completions', useOai.key, { model: 'fake-gpt', messages: [{ role: 'system', content: 'sys' }, { role: 'user', content: 'through-r112' }] });
  const oaiText = ((((JSON.parse(oaiChat.text || '{}').choices || [])[0] || {}).message || {}).content) || '';
  const oaiStream = await v1('/v1/chat/completions', useOai.key, { model: 'fake-gpt', stream: true, messages: [{ role: 'user', content: 'flow-r112' }] });
  const oaiStreamed = oaiStream.text.split('\n').filter((l) => l.startsWith('data: ') && l !== 'data: [DONE]').map((l) => ((JSON.parse(l.slice(6)).choices || [])[0] || {}).delta || {}).map((d) => d.content || '').join('');
  const otherTouch = await call('/api/providers/update', A, { id: cli.provider.id, shared: true });
  const shareIt = await call('/api/providers/update', U, { id: cli.provider.id, shared: true });
  const nowSeenByA = (await call('/api/providers', A)).data.providers.map((p) => p.id);
  const rmOther = await call('/api/providers/remove', A, { id: cli.provider.id });
  const rm = await call('/api/providers/remove', U, { id: cli.provider.id });
  const ck7b = (await call('/api/check', U)).data;
  await call('/api/providers/remove', U, { id: oai.provider.id }); await call('/api/apikey/revoke', U, {}); await call('/api/model', U, { model: 'claude-opus-5-5', provider: 'host' });
  check('R112', 'AI providers: the host Claude is always listed; a person adds a Claude Code login (own folder) and an OpenAI endpoint (private or shared; bad kind / key refused; secrets never listed); a private one is invisible to others; picking the login runs Work with CLAUDE_CONFIG_DIR set and that model; an OpenAI provider is refused for Work; the API key answers through the endpoint provider (plain + streaming, system passed); only the owner shares / removes; removal clears the person\'s choice',
    prov0.providers.length === 1 && prov0.providers[0].id === 'host' && badKind.status === 400 && badAnth.status === 400 && cli.provider && oai.provider && !secretLeak
    && !seenByA.includes(cli.provider.id) && seenByA.includes(oai.provider.id)
    && pickCli.status === 200 && pickCli.data.provider === cli.provider.id && new RegExp('cfg=' + cfgDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).test(texts(cliRun.events)) && /model=claude-sonnet-5-5/.test(texts(cliRun.events)) && ck7.provider === cli.provider.id && (ck7.providers || []).some((p) => p.id === cli.provider.id)
    && pickOai.status === 400 && setOai.status === 200 && setOai.data.provider === oai.provider.id && oaiModels.status === 200 && /fake-mini/.test(oaiModels.text)
    && oaiChat.status === 200 && /^oai says: through-r112 \[sys 3\]/.test(oaiText) && /flow-r112/.test(oaiStreamed)
    && otherTouch.status === 400 && shareIt.status === 200 && nowSeenByA.includes(cli.provider.id) && rmOther.status === 400 && rm.status === 200 && ck7b.provider === 'host'
    && /id="provBox"/.test(html) && /\/api\/providers\/add/.test(html) && /o\.value = p\.id \+ '\|' \+ m/.test(html),
    `cli ${pickCli.status} run "${texts(cliRun.events).slice(-120)}", oai work ${pickOai.status}, api via oai ${oaiChat.status} "${oaiText.slice(0, 50)}", stream "${oaiStreamed.slice(0, 40)}", share ${otherTouch.status}/${shareIt.status}, remove ${rmOther.status}/${rm.status}`);

  // Chat v2: settings, private messages, edit / delete, receipts, agent privacy
  const seal = async (tok, priv, plain, to) => { // like the page's sealFor: everyone, or only sender + recipient
    const mem = (await call('/api/room/members', tok)).data.members, r2 = rand(32), iv2 = rand(12);
    const body = await subtle.encrypt({ name: 'AES-GCM', iv: iv2 }, await subtle.importKey('raw', r2, 'AES-GCM', false, ['encrypt']), enc.encode(plain));
    const ks = {};
    for (const m of mem) if (m.pub) { const kiv = rand(12); ks[m.username] = { iv: toB64(kiv), wk: toB64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv }, await shared(priv, m.pub), r2)) }; }
    return { iv: toB64(iv2), ct: toB64(body), keys: ks, ...(to ? { to } : {}) }; // deliberately wraps for everyone: the hub must drop extra slots
  };
  const v2 = (t, r) => JSON.stringify({ v: 2, t, ...(r ? { r } : {}) });
  const pr = await call('/api/prefs', U, { nick: '  Tee\u0007 Nick  ', color: 5, toasts: false, sound: { on: true, message: 150, mention: 20 },
    tts: { all: 0, unity: 'yes', on: true, volume: 400, rate: 10, voice: 'x'.repeat(300), scope: 'weird' }, clips: { volume: -5, autoplay: 1 },
    audio: { noise: false, echo: 1 }, call: { volume: 250, quality: 'ultra' }, autosave: 999 });
  const ttsOk = pr.data.prefs && pr.data.prefs.tts && pr.data.prefs.tts.on === true && pr.data.prefs.tts.all === false && pr.data.prefs.tts.unity === true && pr.data.prefs.tts.volume === 100 && pr.data.prefs.tts.rate === 50
    && /id="setTtsAll"/.test(html) && /id="setTtsUnity"/.test(html) && /const voiceOn = \(\) => prefs\(\)\.tts\.all && prefs\(\)\.tts\.unity/.test(html) && /if \(!text \|\| !prefs\(\)\.tts\.all\) return;/.test(html)
    && pr.data.prefs.tts.voice.length === 120 && pr.data.prefs.tts.scope === 'all'
    && pr.data.prefs.clips && pr.data.prefs.clips.volume === 0 && pr.data.prefs.clips.autoplay === true
    && pr.data.prefs.audio && pr.data.prefs.audio.noise === false && pr.data.prefs.audio.echo === true
    && pr.data.prefs.call && pr.data.prefs.call.volume === 100 && pr.data.prefs.call.quality === 'medium' && pr.data.prefs.autosave === 120;
  const prBad = await call('/api/prefs', A, { color: 99 });
  const memP = ((await call('/api/room/members', A)).data.members || []).find((m) => m.username === 't-user');
  check('R42', 'settings: nickname cleaned, colour kept, volumes / speech settings clamped, text-to-speech master switch + Unity voice stored as booleans on the account, bad colour → none; others see nick + colour; [static] both switches in Settings, speak() and the Voice button obey the master switch', pr.status === 200 && ttsOk && pr.data.prefs.nick === 'Tee Nick'
    && pr.data.prefs.color === 5 && pr.data.prefs.sound.message === 100 && pr.data.prefs.toasts === false && prBad.data.prefs.color === null
    && !!memP && memP.nick === 'Tee Nick' && memP.color === 5 && (await call('/api/check', U)).data.prefs.nick === 'Tee Nick');

  const rev0 = (await call('/api/room/messages?rev=0', A)).data.latestRev || 0;
  const dm = await call('/api/room/send', A, await seal(A, privA, v2('secret dm-r43'), 't-user'));
  const self = await call('/api/room/send', A, await seal(A, privA, v2('x'), 't-admin'));
  const nobody = await call('/api/room/send', A, await seal(A, privA, v2('x'), 'ghost'));
  const forU = ((await call('/api/room/messages?rev=' + rev0, U)).data.messages || []).find((m) => m.seq === dm.data.seq);
  const forD = ((await call('/api/room/messages?rev=0', D)).data.messages || []).find((m) => m.seq === dm.data.seq);
  const onDisk = (fs.readFileSync(path.join(SCRATCH, 'Chat', 'messages.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)).find((m) => m.seq === dm.data.seq) || {});
  check('R43', 'private message: only sender + recipient receive it, extra key slots dropped, to self / unknown → 400', dm.status === 200 && !!forU && forU.to === 't-user'
    && (await roomRead(restored, forU)) === v2('secret dm-r43') && !forD && Object.keys(onDisk.keys || {}).sort().join() === 't-admin,t-user' && self.status === 400 && nobody.status === 400);

  const mine = await call('/api/room/send', U, await seal(U, restored, v2('before edit', dm.data.seq)));
  const revA = (await call('/api/room/messages?rev=0', A)).data.latestRev;
  const ed = await call('/api/room/edit', U, { seq: mine.data.seq, ...(await seal(U, restored, v2('after edit', dm.data.seq))) });
  const edOther = await call('/api/room/edit', A, { seq: mine.data.seq, ...(await seal(A, privA, v2('hijack'))) });
  const changed = ((await call('/api/room/messages?rev=' + revA, A)).data.messages || []).find((m) => m.seq === mine.data.seq);
  check('R44', 'edit own message: re-encrypted text, editedAt, shows up in ?rev= polling; editing someone else\'s → 403', ed.status === 200 && edOther.status === 403
    && !!changed && !!changed.editedAt && (await roomRead(privA, changed)) === v2('after edit', dm.data.seq));

  const revB = (await call('/api/room/messages?rev=0', A)).data.latestRev;
  const delOther = await call('/api/room/delete', A, { seq: mine.data.seq });
  const delOwn = await call('/api/room/delete', U, { seq: mine.data.seq });
  const gone2 = ((await call('/api/room/messages?rev=' + revB, A)).data.messages || []).find((m) => m.seq === mine.data.seq);
  const diskDel = (fs.readFileSync(path.join(SCRATCH, 'Chat', 'messages.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)).find((m) => m.seq === mine.data.seq) || {});
  check('R45', 'delete own message: tombstone without ciphertext, polled by everyone; deleting someone else\'s → 403', delOther.status === 403 && delOwn.status === 200
    && !!gone2 && gone2.deleted === true && !gone2.ct && !diskDel.ct && diskDel.deleted === true);

  const sRoom = await call('/api/room/seen', U, { conv: 'room', seq: dm.data.seq });
  const sDm = await call('/api/room/seen', U, { conv: 'dm:t-admin|t-user', seq: dm.data.seq });
  const sForeign = await call('/api/room/seen', D, { conv: 'dm:t-admin|t-user', seq: 1 });
  const sBad = await call('/api/room/seen', U, { conv: 'nowhere', seq: 1 });
  const seenA = (await call('/api/room/messages?rev=' + revB, A)).data.seen || {}, seenD = (await call('/api/room/messages?rev=' + revB, D)).data.seen || {};
  check('R46', 'receipts: seen per conversation reported to its members only; foreign / unknown conversation → 400', sRoom.status === 200 && sDm.status === 200
    && sForeign.status === 400 && sBad.status === 400 && (seenA['t-user'] || {}).room === dm.data.seq && (seenA['t-user'] || {})['dm:t-admin|t-user'] === dm.data.seq
    && !((seenD['t-user'] || {})['dm:t-admin|t-user']));

  await sleep(1500);
  const seqDm = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await seal(U, restored, v2('private ping-r47'), 'claude'));
  const privAns = await waitAgent(U, restored, seqDm, (t) => t.includes('ping-r47'), 20000);
  const privMsg = privAns && ((await call('/api/room/messages?since=' + seqDm, U)).data.messages || []).find((m) => m.seq === privAns.seq);
  const leaked = ((await call('/api/room/messages?since=' + seqDm, A)).data.messages || []).some((m) => privAns && m.seq === privAns.seq);
  await sleep(1500);
  const seqV2 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await seal(U, restored, v2('@unity ping-v2-r47')));
  const v2Ans = await waitAgent(U, restored, seqV2, (t) => t.includes('ping-v2-r47'), 20000);
  check('R47', 'agent: a private message gets a private answer nobody else receives; it reads the new message format', !!privAns && !!privMsg && privMsg.to === 't-user'
    && /private message to you/.test(privAns.text) && !leaked && !!v2Ans && !v2Ans.text.includes('"v":2'));

  // Voice clips: the audio travels inside the encrypted v2 payload; the hub only enforces a size cap.
  const fakeAudio = Buffer.from(rand(600000)).toString('base64'); // ~800 KB of "audio" → ~1.1 MB encrypted + base64
  const clipSent = await call('/api/room/send', U, await seal(U, restored, JSON.stringify({ v: 2, t: 'clip-r49', a: { m: 'audio/webm', d: fakeAudio, s: 95 } })));
  const clipBack = clipSent.status === 200 && ((await call('/api/room/messages?since=' + (clipSent.data.seq - 1), A)).data.messages || []).find((m) => m.seq === clipSent.data.seq);
  const clipPlain = clipBack ? JSON.parse(await roomRead(privA, clipBack) || '{}') : {};
  const tooBig = await call('/api/room/send', U, { iv: toB64(rand(12)), ct: 'A'.repeat(1700000), keys: { 't-user': { iv: toB64(rand(12)), wk: toB64(rand(48)) } } });
  const clipDisk = fs.readFileSync(path.join(SCRATCH, 'Chat', 'messages.jsonl'), 'utf8');
  check('R49', 'voice clip: a ~2-minute clip is accepted and decrypts intact for others; over the cap → 413; audio never stored in the clear',
    clipSent.status === 200 && clipPlain.a && clipPlain.a.d === fakeAudio && clipPlain.t === 'clip-r49' && tooBig.status === 413 && !clipDisk.includes(fakeAudio.slice(0, 200)),
    `send ${clipSent.status}, too big ${tooBig.status}`);
  // Message format v3: HKDF wrap keys, sender/recipient/mid as authenticated data, padded plaintext.
  const seal3 = async (tok, priv, from, plain, to, mid = toB64(rand(16)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')) => {
    const mem = (await call('/api/room/members', tok)).data.members, r2 = rand(32), iv2 = rand(12), aad = 'mills-v3|' + from + '|' + (to || '') + '|' + mid;
    const pt = Buffer.alloc(256, 0x20); Buffer.from(plain).copy(pt);
    const body = await subtle.encrypt({ name: 'AES-GCM', iv: iv2, additionalData: enc.encode(aad) }, await subtle.importKey('raw', r2, 'AES-GCM', false, ['encrypt']), pt);
    const ks = {};
    for (const m of mem) if (m.pub && (!to || m.username === to || m.username === from)) { const kiv = rand(12); ks[m.username] = { iv: toB64(kiv), wk: toB64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv, additionalData: enc.encode(aad + '|' + m.username) }, await wrapV3(priv, m.pub), r2)) }; }
    return { v: 3, mid, iv: toB64(iv2), ct: toB64(body), keys: ks, ...(to ? { to } : {}) };
  };
  const s3 = await seal3(U, restored, 't-user', v2('v3 hello-r61'));
  const sent3 = await call('/api/room/send', U, s3);
  const back3 = sent3.status === 200 && ((await call('/api/room/messages?since=' + (sent3.data.seq - 1), A)).data.messages || []).find((m) => m.seq === sent3.data.seq);
  const read3 = back3 ? await roomRead(privA, back3) : null;
  const relabel3 = back3 ? await roomRead(privA, { ...back3, to: 't-admin' }) : 'x'; // the hub relabelling the recipient must break decryption
  const dupMid = await call('/api/room/send', U, await seal3(U, restored, 't-user', v2('replay'), '', s3.mid));
  const badMid = await call('/api/room/send', U, { ...(await seal3(U, restored, 't-user', v2('x'))), mid: 'short' });
  const ed3 = await call('/api/room/edit', U, { seq: sent3.data.seq, ...(await seal3(U, restored, 't-user', v2('v3 edited-r61'), '', s3.mid)) });
  const edNewMid = await call('/api/room/edit', U, { seq: sent3.data.seq, ...(await seal3(U, restored, 't-user', v2('x'))) });
  await sleep(1500);
  const seq3 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await seal3(U, restored, 't-user', v2('@unity ping-v3-r61')));
  const ans3 = await waitAgent(U, restored, seq3, (t) => t.includes('ping-v3-r61'), 20000);
  const ans3Msg = ans3 && ((await call('/api/room/messages?since=' + seq3, U)).data.messages || []).find((m) => m.seq === ans3.seq);
  check('R90', 'message format v3: decrypts with HKDF + authenticated sender/recipient/id, relabelled recipient fails, padded to a bucket, duplicate / bad id → 400, edit keeps its id, agent reads and answers in v3',
    read3 === v2('v3 hello-r61') && relabel3 === null && fromB64(back3.ct).length === 256 + 16 && dupMid.status === 400 && badMid.status === 400
    && ed3.status === 200 && edNewMid.status === 400 && !!ans3 && !!ans3Msg && ans3Msg.v === 3 && /^[\w-]{22}$/.test(ans3Msg.mid || ''),
    `send ${sent3.status}, read ${read3 !== null}, dup ${dupMid.status}, bad ${badMid.status}, edit ${ed3.status}/${edNewMid.status}, agent ${!!ans3}`);

  // Room long poll: a waiting reader is answered the moment someone sends, not on a timer.
  const rv0 = (await call('/api/room/messages?rev=0', A)).data;
  const rt0 = Date.now();
  const held = call('/api/room/messages?rev=' + rv0.latestRev + '&wait=1&ver=' + rv0.ver, A);
  await sleep(800);
  const lp = await call('/api/room/send', U, await seal3(U, restored, 't-user', v2('long-poll-r62')));
  const rwoke = await held, tookMs = Date.now() - rt0;
  const stale = await call('/api/room/messages?rev=' + rv0.latestRev + '&wait=1&ver=' + (rv0.ver - 1), A); // old version → answered at once
  check('R91', 'room long poll: held while nothing changes, answered right after a send (not on a timer), stale version answered at once',
    lp.status === 200 && tookMs >= 700 && tookMs < 3000 && (rwoke.data.messages || []).some((m) => m.seq === lp.data.seq) && rwoke.data.ver > rv0.ver && stale.status === 200,
    `woke after ${tookMs} ms`);

  // Format v4: JSON deflated before encryption; a voice clip's audio is its own ciphertext, fetched on play.
  const blobAad4 = (from, mid) => 'mills-v4-blob|' + from + '|' + mid;
  const frame4 = (b) => { let n = 256; while (n < 4 + b.length && n < 65536) n *= 2; if (4 + b.length > 65536) n = Math.ceil((4 + b.length) / 65536) * 65536; const o = Buffer.alloc(n); o.writeUInt32BE(b.length, 0); b.copy(o, 4); return o; };
  const seal4 = async (tok, priv, from, payload, to, opts = {}) => {
    const mid = opts.mid || toB64(rand(16)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    let blob;
    if (opts.audio) { // clip: own key, iv ‖ AES-GCM(frame(audio)), bound to sender + mid
      const bk = rand(32), biv = rand(12);
      const bct = await subtle.encrypt({ name: 'AES-GCM', iv: biv, additionalData: enc.encode(blobAad4(from, mid)) }, await subtle.importKey('raw', bk, 'AES-GCM', false, ['encrypt']), frame4(opts.audio));
      blob = Buffer.concat([Buffer.from(biv), Buffer.from(bct)]).toString('base64');
      payload = { ...payload, a: { m: 'audio/webm', k: toB64(bk), s: 3 } };
    }
    const mem = (await call('/api/room/members', tok)).data.members, r2 = rand(32), iv2 = rand(12), aad = 'mills-v4|' + from + '|' + (to || '') + '|' + mid;
    const body = await subtle.encrypt({ name: 'AES-GCM', iv: iv2, additionalData: enc.encode(aad) }, await subtle.importKey('raw', r2, 'AES-GCM', false, ['encrypt']), frame4(zlib.deflateRawSync(Buffer.from(JSON.stringify(payload)))));
    const ks = {};
    for (const m of mem) if (m.pub && (!to || m.username === to || m.username === from) && (m.username !== 'claude' || opts.agent)) { const kiv = rand(12); ks[m.username] = { iv: toB64(kiv), wk: toB64(await subtle.encrypt({ name: 'AES-GCM', iv: kiv, additionalData: enc.encode(aad + '|' + m.username) }, await wrapV3(priv, m.pub), r2)) }; }
    return { v: 4, mid, iv: toB64(iv2), ct: toB64(body), keys: ks, ...(to ? { to } : {}), ...(blob ? { blob } : {}) };
  };
  const read4 = async (priv, m) => { // the page's v4 path
    const aad = 'mills-v4|' + m.from + '|' + (m.to || '') + '|' + m.mid;
    const mk = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.key.iv), additionalData: enc.encode(aad + '|' + ownerOf.get(priv)) }, await wrapV3(priv, m.fromPub), fromB64(m.key.wk));
    const b = Buffer.from(await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(m.iv), additionalData: enc.encode(aad) }, await subtle.importKey('raw', mk, 'AES-GCM', false, ['decrypt']), fromB64(m.ct)));
    return JSON.parse(zlib.inflateRawSync(b.subarray(4, 4 + b.readUInt32BE(0))).toString('utf8'));
  };
  const longText = 'compress me '.repeat(400); // 4.8 KB of text → far smaller once deflated
  const audio = Buffer.from(rand(40000));
  const s4 = await seal4(A, privA, 't-admin', { v: 2, t: longText + 'clip-r63' }, 't-user', { audio });
  const sent4 = await call('/api/room/send', A, s4);
  const list4 = ((await call('/api/room/messages?since=' + (sent4.data.seq - 1), U)).data.messages || []).find((m) => m.seq === sent4.data.seq);
  const p4 = list4 ? await read4(restored, list4).catch(() => null) : null;
  const blobU = await call('/api/room/blob?seq=' + sent4.data.seq, U), blobD = await call('/api/room/blob?seq=' + sent4.data.seq, D);
  let audioBack = null;
  if (p4 && blobU.status === 200) {
    const bb = Buffer.from(blobU.data.blob, 'base64');
    const plain = Buffer.from(await subtle.decrypt({ name: 'AES-GCM', iv: bb.subarray(0, 12), additionalData: enc.encode(blobAad4('t-admin', s4.mid)) }, await subtle.importKey('raw', fromB64(p4.a.k), 'AES-GCM', false, ['decrypt']), bb.subarray(12)));
    audioBack = plain.subarray(4, 4 + plain.readUInt32BE(0));
  }
  const ed4bad = await call('/api/room/edit', A, { seq: sent4.data.seq, ...(await seal3(A, privA, 't-admin', v2('x'), 't-user', s4.mid)) }); // v3 edit would orphan the clip
  const ed4 = await call('/api/room/edit', A, { seq: sent4.data.seq, ...(await seal4(A, privA, 't-admin', { v: 2, t: 'caption edited', a: p4 && p4.a }, 't-user', { mid: s4.mid })) });
  const stillBlob = await call('/api/room/blob?seq=' + sent4.data.seq, U);
  await sleep(1500);
  const seq4 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await seal4(U, restored, 't-user', { v: 2, t: '@unity ping-v4-r63' }, '', { agent: true }));
  const ans4 = await waitAgent(U, restored, seq4, (t) => t.includes('ping-v4-r63'), 20000);
  check('R92', 'format v4: text deflated before encryption (smaller than the text), clip only via /api/room/blob for its readers (not in the list), clip intact, v3 edit of a clip → 400, v4 caption edit keeps the clip, agent reads v4',
    sent4.status === 200 && !!list4 && !list4.blob && list4.blobLen > 0 && p4 && p4.t === longText + 'clip-r63' && fromB64(list4.ct).length < longText.length
    && blobU.status === 200 && blobD.status === 404 && !!audioBack && audioBack.equals(audio) && ed4bad.status === 400 && ed4.status === 200 && stillBlob.status === 200 && !!ans4,
    `send ${sent4.status}, body ${list4 ? fromB64(list4.ct).length : '-'} B for ${longText.length} B text, blob U ${blobU.status} / D ${blobD.status}, edit ${ed4bad.status}/${ed4.status}, agent ${!!ans4}`);

  // Presence: typing per conversation (only to its members), online list, Unity typing while composing.
  const pv0 = (await call('/api/room/messages?rev=0', A)).data;
  const pHeld = call('/api/room/messages?rev=' + pv0.latestRev + '&wait=1&ver=' + pv0.ver, A);
  await sleep(400);
  const tyOn = await call('/api/room/typing', U, { conv: 'room', on: true });
  const pWoke = (await pHeld).data;
  const tyDm = await call('/api/room/typing', U, { conv: 'dm:t-admin|t-user', on: true });
  const seesDmA = ((await call('/api/room/messages?rev=0', A)).data.typing || []).some((t) => t.user === 't-user' && t.conv === 'dm:t-admin|t-user');
  const seesDmD = ((await call('/api/room/messages?rev=0', D)).data.typing || []).some((t) => t.user === 't-user');
  const tyBad = await call('/api/room/typing', D, { conv: 'dm:t-admin|t-user', on: true });
  await call('/api/room/typing', U, { conv: 'room', on: false });
  const afterOff = (await call('/api/room/messages?rev=0', A)).data;
  const onlineOk = afterOff.online.includes('t-admin') && !afterOff.online.includes('t-user') && !(afterOff.typing || []).some((t) => t.user === 't-user'); // A held a long poll, U never did
  await sleep(1500);
  const seqTy = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await roomSend(U, restored, '@unity SLOW ping-r72');
  await sleep(800);
  const agentTyping = ((await call('/api/room/messages?rev=0', A)).data.typing || []).some((t) => t.user === 'claude' && t.conv === 'room');
  const tyAns = await waitAgent(U, restored, seqTy, (t) => t.includes('ping-r72'), 20000);
  const agentDone = !((await call('/api/room/messages?rev=0', A)).data.typing || []).some((t) => t.user === 'claude');
  check('R101', 'presence: typing wakes the others\' long poll and is per conversation (a private one only to its two people; outsider → 400); off removes it; online list; Unity shows as typing while composing and stops after posting; [static] typing line, Claude-is-thinking bubble, message slide-in, live dot',
    tyOn.status === 200 && (pWoke.typing || []).some((t) => t.user === 't-user' && t.conv === 'room') && tyDm.status === 200 && seesDmA && !seesDmD && tyBad.status === 400 && onlineOk
    && agentTyping && !!tyAns && agentDone
    && /id="typingLine"/.test(html) && /function showThinking\(/.test(html) && /@keyframes msgIn/.test(html) && /@keyframes dotPulse/.test(html) && /\.status\.on::before/.test(html) && /prefers-reduced-motion/.test(html)
    && /line\.classList\.toggle\('thinking', ai\)/.test(html) && /\.typingLine\.thinking \{/.test(html), // Unity composing in the chat = Work's thinking bubble
    `typing woke ${(pWoke.typing || []).length}, dm A/D ${seesDmA}/${seesDmD}, outsider ${tyBad.status}, online ${JSON.stringify(afterOff.online)}, Unity typing ${agentTyping} → done ${agentDone}`);

  // Hub-wide presence: online = a page with its long poll open; idle + screen reported by the page;
  // offline seconds after the last connection closes, at once on "gone" or sign-out; last seen kept.
  const presOf = async (tok, u) => ((await call('/api/room/messages?rev=0', tok)).data.presence || []).find((p) => p.user === u) || {};
  const ac = new AbortController();
  const dHeld = fetch(H + '/api/room/messages?rev=0&wait=1&ver=' + (await call('/api/room/messages?rev=0', D)).data.ver, { headers: { Authorization: 'Bearer ' + D }, signal: ac.signal }).catch(() => null);
  await sleep(300);
  const dOn = await presOf(A, 'mills');
  await call('/api/presence', D, { screen: 'board', idle: true });
  const dIdle = await presOf(A, 'mills');
  ac.abort(); await dHeld; // the page went away without saying so
  const dStillOn = await presOf(A, 'mills');
  await sleep(9500);
  const dOff = await presOf(A, 'mills');
  const ac2 = new AbortController();
  const dHeld2 = fetch(H + '/api/room/messages?rev=0&wait=1&ver=' + (await call('/api/room/messages?rev=0', D)).data.ver, { headers: { Authorization: 'Bearer ' + D }, signal: ac2.signal }).catch(() => null);
  await sleep(300);
  const dBack = await presOf(A, 'mills');
  await call('/api/presence', D, { gone: true });
  const dGone = await presOf(A, 'mills');
  ac2.abort(); await dHeld2;
  check('R103', 'presence: a page with its long poll open is online; idle + screen as the page reports them; a dropped connection shows offline within ~10 s (not before); back online at once on reconnect; "gone" is offline immediately; last seen kept; [static] Who\'s here strip on the menu, idle after 5 min / hidden tab, status text in the chat list',
    dOn.online === true && dIdle.online === true && dIdle.idle === true && dIdle.screen === 'board' && dStillOn.online === true && dOff.online === false && dOff.seen > 0
    && dBack.online === true && dBack.idle === false && dGone.online === false
    && /id="menuWho"/.test(html) && /IDLE_MS = 5 \* 60000/.test(html) && /function statusOf\(/.test(html) && /visibilitychange', presenceTick/.test(html) && /gone: true/.test(html),
    `on ${dOn.online}, idle ${dIdle.idle}/${dIdle.screen}, after drop ${dStillOn.online} → ${dOff.online}, back ${dBack.online}, gone ${dGone.online}`);

  // Hardening headers on every hub answer.
  const hr = await fetch(H + '/api/status');
  check('R93', 'hub answers carry nosniff, no-referrer, frame DENY; API answers are no-store',
    hr.headers.get('x-content-type-options') === 'nosniff' && hr.headers.get('referrer-policy') === 'no-referrer' && hr.headers.get('x-frame-options') === 'DENY' && /no-store/.test(hr.headers.get('cache-control') || ''));

  // Sign-in hashing is async now: parallel wrong guesses must still hit the 5-attempt limit.
  const burst = await Promise.all(Array.from({ length: 12 }, () => call('/api/login', null, { username: 'ghost-r65', password: 'wrong', deviceId: 'x'.repeat(32) })));
  const n401 = burst.filter((r) => r.status === 401).length, n429 = burst.filter((r) => r.status === 429).length;
  const statusDuring = await call('/api/status');
  check('R94', 'parallel wrong sign-ins: at most 5 are checked, the rest → 429; the hub stays responsive', n401 <= 5 && n401 + n429 === 12 && statusDuring.status === 200, `${n401}×401, ${n429}×429`);

  const csp = (/<meta http-equiv="Content-Security-Policy" content="([^"]+)"/.exec(html) || [])[1] || '';
  const missing66 = [
    [/default-src 'none'/.test(csp) && /connect-src 'self' https:\/\/\*\.trycloudflare\.com https:\/\/api\.github\.com;/.test(csp) && /object-src 'none'/.test(csp) && /img-src 'self' https: data: blob:/.test(csp) && !/script-src[^;]*https:/.test(csp), 'CSP (images over https only; scripts never)'],
    [/HUB_URL_RE = \/\^https:\\\/\\\/\[a-z0-9-\]\+\\\.trycloudflare\\\.com\$\//.test(html) && /store\.set\('mills\.hub'/.test(html), 'hub address pinned + cached'],
    [/function trustedPub\(/.test(html) && /changedKeys\.has\(m\.username\)\) continue; \/\/ never encrypt to an unverified key/.test(html) && /Trust new key/.test(html), 'key pinning'],
    [/streams: \[call\.local\]/.test(html) && /sender\.setStreams\(isScreenTransceiver\(peer\.pc, t\) \? call\.screenStream : call\.local\)/.test(html) && /e\.streams && e\.streams\[0\]/.test(html), 'call lip-sync streams'],
    [/sendSignal\(user, \{ cs: peer\.iceOut\.splice\(0\) \}\)/.test(html), 'ICE batching'],
    [/&wait=1&ver=' \+ roomVer/.test(html) && !/setInterval\(\(\) => \{ pollRoom\(\)/.test(html), 'room long poll, no 3 s timer'],
    [/new CompressionStream\('deflate-raw'\)/.test(html) && /\/api\/room\/blob\?seq=/.test(html), 'v4 deflate + clip on play'],
  ].filter(([ok]) => !ok).map(([, n]) => n);
  const mdc = await require('./md-check.js').run();
  const mdSrc = fs.readFileSync(path.join(MILLS, 'web', 'md.js'), 'utf8');
  const mdStatic = !/\.innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(mdSrc) && /securityLevel: 'strict'/.test(mdSrc) && /htmlLabels: false/.test(mdSrc)
    && /<script src="md\.js"><\/script>/.test(html) && /MD\.render\(el, text\)/.test(html) && /MD\.render\(el, text, \{ mention:/.test(html);
  check('R96', 'markdown renderer (web/md.js): headings, emphasis, links (safe only), lists / tasks, tables, quotes / alerts, code with colouring + Copy, mermaid on demand, math, footnotes, mentions; raw HTML and javascript: links stay text; never assigns HTML; used by both chats',
    !mdc.failed.length && mdStatic, mdc.failed.length ? mdc.failed.join(' | ') : (mdStatic ? `${mdc.total} cases` : 'static check failed'));
  // Custom SVG icons instead of emoji: nothing the page shows may be an emoji (code comments and the
  // regex that strips the old "⚙ " from saved sessions are not shown), and every icon used exists.
  const EMOJI = /[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}\u{2B00}-\u{2BFF}\u{23E9}-\u{23FA}\u{25B6}\u{27F3}\u{21A9}\u{21AA}\u{2713}\u{2715}\u{25A0}]/u;
  const shown = (src) => src.split('\n').map((l) => l.replace(/^\s*\/\/.*$/, '').replace(/\s\/\/ .*$/, '').replace(/<!--.*?-->/g, '')).filter((l) => !/const toolText/.test(l));
  const emojiLines = [...shown(html).map((l, k) => [l, 'index.html:' + (k + 1)]), ...shown(fs.readFileSync(path.join(MILLS, 'web', 'md.js'), 'utf8')).map((l, k) => [l, 'md.js:' + (k + 1)])]
    .filter(([l]) => EMOJI.test(l) && !/^\s*\*|\/\*/.test(l)).map(([, at]) => at);
  const symbols = new Set([...html.matchAll(/<symbol id="i-([\w-]+)"/g)].map((m) => m[1]));
  const code68 = shown(html).join('\n');
  const usedIcons = new Set([...code68.matchAll(/href="#i-([\w-]+)"/g), ...code68.matchAll(/\bic\('([\w-]+)'/g), ...code68.matchAll(/(?<![.\w])label\([^,\n]+, '([\w-]+)'/g)].map((m) => m[1])
    .concat(['reply', 'edit', 'trash', 'stop', 'send', 'talk', 'image', 'mic', 'mic-off', 'speaker', 'speaker-off', 'folder', 'file'])); // via add(), dictLabel(), ternaries, md.js
  const missingIcons = [...usedIcons].filter((n) => !symbols.has(n));
  const serverEmoji = /mirror\(acct, 'tool', '⚙/.test(fs.readFileSync(path.join(MILLS, 'bridge', 'server.js'), 'utf8'));
  check('R97', '[static] no emoji shown anywhere on the site (custom SVG icon sprite instead), every icon used exists in the sprite, hub tool lines carry no emoji',
    !emojiLines.length && !missingIcons.length && symbols.size >= 30 && !serverEmoji, [emojiLines.slice(0, 8).join(' '), missingIcons.join(' ')].filter(Boolean).join(' | ') || `${symbols.size} icons`);
  // Vendored browser libraries (web/vendor, fetched + integrity-checked by bridge/vendor.js).
  const vendorManifest = path.join(MILLS, 'web', 'vendor', 'VENDOR.json');
  if (!fs.existsSync(vendorManifest)) skip('R102', 'vendored Mermaid present and matching web/vendor/VENDOR.json', 'not installed yet: run `node bridge/vendor.js` in the Mills folder (diagrams show their source until then)');
  else {
    const vm = JSON.parse(fs.readFileSync(vendorManifest, 'utf8')), mm = vm.mermaid || {}, kt = vm.katex || {};
    const mFile = path.join(MILLS, 'web', 'vendor', mm.file || 'mermaid.min.js');
    const mOk = fs.existsSync(mFile) && crypto.createHash('sha256').update(fs.readFileSync(mFile)).digest('hex') === mm.sha256 && /^11\./.test(mm.version || '');
    const mLoads = mOk && /mermaid/.test(fs.readFileSync(mFile, 'utf8').slice(0, 2000));
    const kFiles = kt.files || [], kOk = kFiles.length > 3 && kFiles.every((f) => { const p = path.join(MILLS, 'web', 'vendor', f.file); return fs.existsSync(p) && crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex') === f.sha256; })
      && kFiles.some((f) => f.file === 'katex.min.js') && kFiles.some((f) => f.file === 'katex.min.css') && kFiles.filter((f) => /^fonts\/.+\.woff2$/.test(f.file)).length >= 10 && !kFiles.some((f) => /\.(ttf|woff)$/.test(f.file));
    const kCss = kOk && /font-src 'self'/.test(csp) && /katexSrc: 'vendor\/katex\.min\.js', katexCss: 'vendor\/katex\.min\.css'/.test(mdSrc) && /trust: false/.test(mdSrc);
    check('R102', 'vendored Mermaid 11 + KaTeX (js, css, woff2 fonts only) present and matching web/vendor/VENDOR.json, served from the site itself (CSP allows no outside scripts; font-src self), KaTeX rendered with trust off', mOk && mLoads && kOk && kCss, `mermaid@${mm.version || '?'}, katex@${kt.version || '?'} (${kFiles.length} files)`);
  }
  const boardJs76 = fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8');
  const missing76 = [
    [/viewport-fit=cover/.test(html), 'viewport-fit'],
    [/@media \(max-width: 700px\) \{/.test(html) && /input, textarea, select \{ font-size: 16px; \}/.test(html), 'phone breakpoint + no zoom-on-focus'],
    [/@media \(pointer: coarse\) \{/.test(html) && /button \{ min-height: 2\.5rem; \}/.test(html), 'touch-sized buttons'],
    [/@media \(max-width: 1366px\)/.test(html), 'laptop breakpoint'],
    [/#roomSide \{ position: absolute;/.test(html) && /const isNarrow = \(\) => window\.matchMedia\('\(max-width: 700px\)'\)/.test(html) && /if \(isNarrow\(\)\) \$\('roomCore'\)\.classList\.add\('noside'\)/.test(html), 'chat drawer on phones'],
    [/#bdDetail \{ position: fixed; inset: 0;/.test(html) && /scroll-snap-type: x mandatory/.test(html), 'board sheet + column snap'],
    [/function renderAgenda\(/.test(boardJs76) && /#calGrid\.agenda/.test(html), 'calendar agenda on phones'],
    [/env\(safe-area-inset-bottom\)/.test(html), 'safe-area insets'],
    [/#adminBody table \{ display: block; overflow-x: auto;/.test(html), 'admin table scrolls'],
    [/\.modal \.box, #archive \.box \{ width: 100vw;/.test(html), 'full-screen dialogs on phones'],
    [/\.floatWin \{ left: 8px !important;/.test(html), 'floating windows fill phones'],
  ].filter(([ok]) => !ok).map(([, n]) => n);
  // Mills Chat app: PWA manifest + service worker + icons, app mode, the Discord-shaped side bar and member column.
  const manifest = JSON.parse(fs.readFileSync(path.join(MILLS, 'web', 'manifest.webmanifest'), 'utf8'));
  const swSrc = fs.readFileSync(path.join(MILLS, 'web', 'sw.js'), 'utf8');
  const icons = (manifest.icons || []).map((i) => path.join(MILLS, 'web', i.src));
  const servedManifest = await fetch(H + '/manifest.webmanifest'), servedIcon = await fetch(H + '/icons/mills.svg'), servedSw = await fetch(H + '/sw.js');
  const missing114 = [
    [manifest.start_url === './index.html?app=1' && manifest.display === 'standalone' && icons.length >= 2 && icons.every((f) => fs.existsSync(f)), 'manifest (start_url ?app=1, standalone, icons on disk)'],
    [/<link rel="manifest" href="manifest\.webmanifest">/.test(indexHtml) && /manifest-src 'self'/.test(csp) && /<link rel="icon" href="icons\/mills\.svg"/.test(indexHtml), 'page links the manifest + icon, CSP allows it'],
    [servedManifest.status === 200 && /manifest\+json/.test(servedManifest.headers.get('content-type') || '') && servedIcon.status === 200 && servedSw.status === 200, 'hub serves manifest / icon / sw'],
    [/url\.pathname\.includes\('\/api\/'\)/.test(swSrc) && /includes\('\/v1\/'\)/.test(swSrc) && /fetch\(e\.request\)\.then/.test(swSrc), 'service worker: network first, never caches /api or /v1'],
    [/navigator\.serviceWorker\.register\('sw\.js'\)/.test(html) && /beforeinstallprompt/.test(html) && /function appOs\(/.test(html) && /id="appBox"/.test(html) && /renderAppCard\(\)/.test(html), 'install card in Settings (OS detected, install prompt)'],
    [/const APP_MODE = /.test(html) && /document\.body\.classList\.add\('appMode'\)/.test(html) && /showScreen\(APP_MODE \? 'room' : 'menu'\)/.test(html) && /body\.appMode #room header \{ display: none; \}/.test(html), 'app mode = chat only'],
    [/id="serverRail"/.test(html) && /id="chanList"/.test(html) && /id="memberList"/.test(html) && /const GUILDS = /.test(html) && /GUILDS\.refresh\(\)/.test(html) && /d\.guildsVer && d\.guildsVer !== GUILDS\.state\.ver/.test(html), 'server rail, channel list, member column, live refresh'],
    [/isGuildConv\(currentConv\) \? currentConv : ''/.test(html) && /allowed = isGuildConv\(conv\) \? GUILDS\.members\(conv\) : null/.test(html) && /const convOfMsg = \(m\) => \(m\.conv \? m\.conv/.test(html), 'channel / group messages: conv sent, keys wrapped for members only, routed by conv'],
    [/className = 'botTag'/.test(html) && /'Bots — 1'/.test(html), 'Unity and the Claudes carry a BOT badge'],
  ].filter(([ok]) => !ok).map(([, n]) => n);
  check('R114', 'Mills Chat app: installable PWA (manifest, icons, service worker that never caches hub answers, CSP manifest-src), app mode shows the chat only, Settings install card detects Windows / Linux; Discord-shaped side bar (server rail, channels, groups, DMs) + member column; channel / group messages wrapped for members only; BOT badges', !missing114.length, missing114.join(', '));
  const slashCmds = [...html.matchAll(/\{ cmd: '(\/[a-z]+)'/g)].map((m) => m[1]);
  check('R115', '[static] slash commands in the Claude box: "/" opens a filtered menu of the common commands (compact, clear, model, review, cost, status, help, commit …), arrows / Tab / Enter pick, Esc closes; Mills commands run here (clear, stop, model, share, board …), the rest go to Claude Code as typed',
    /id="slashMenu"/.test(html) && slashCmds.length >= 20 && ['/compact', '/clear', '/model', '/review', '/cost', '/status', '/help', '/commit', '/stop', '/share'].every((c) => slashCmds.includes(c))
    && /function renderSlash\(/.test(html) && /function runLocalSlash\(/.test(html) && /if \(message\.startsWith\('\/'\) && runLocalSlash\(message\)\) return;/.test(html) && /e\.key === 'ArrowDown'/.test(html) && /pickSlash\(list\[slashIdx\]\)/.test(html),
    slashCmds.length + ' commands');
  check('R105', '[static] responsive: phone breakpoint (drawer chat list, board sheet + swipe columns, calendar agenda, full-screen dialogs, stacked Work panes, safe-area padding, no zoom-on-focus), touch-sized controls, laptop header tightening, admin table scrolls', !missing76.length, missing76.join(', '));
  check('R95', '[static] CSP, hub address pinned to the tunnel, key pinning (TOFU + Trust new key), call lip-sync + ICE batching, room long poll, v4 deflate + clips fetched on play', !missing66.length, missing66.join(', '));
  // Calls: the hub only tracks who is in which call and relays opaque (encrypted) set-up messages.
  const jA = await call('/api/call/join', A, { conv: 'room', cam: false, mic: true });
  const pollU = (await call('/api/call/poll?after=0&ver=-1', U)).data, pollD = (await call('/api/call/poll?after=0&ver=-1', D)).data;
  const dmJoin = await call('/api/call/join', U, { conv: 'dm:t-admin|t-user', cam: false, mic: true });
  const dmIntruder = await call('/api/call/join', D, { conv: 'dm:t-admin|t-user' }), badConv = await call('/api/call/join', D, { conv: 'dm:zzz|aaa' });
  const dmSeenByD = !!((await call('/api/call/poll?after=0&ver=-1', D)).data.calls || {})['dm:t-admin|t-user'];
  await call('/api/call/join', U, { conv: 'room', cam: false, mic: true }); // one call at a time: moves U out of the private one
  const stU = (await call('/api/call/poll?after=0&ver=-1', U)).data;
  const opaque = { iv: toB64(rand(12)), ct: toB64(rand(300)) };
  const sigToD = await call('/api/call/signal', A, { to: 'mills', conv: 'room', ...opaque });
  const v0 = stU.ver, a0 = stU.after;
  const t0 = Date.now();
  const waiting = call(`/api/call/poll?after=${a0}&ver=${v0}`, U); // must hang until the signal arrives
  await sleep(700);
  const sig = await call('/api/call/signal', A, { to: 't-user', conv: 'room', ...opaque });
  const woke = (await waiting).data, waited = Date.now() - t0;
  const got1 = (woke.signals || []).find((s) => s.from === 't-admin');
  const camState = await call('/api/call/state', A, { conv: 'room', cam: true, mic: false, screen: true });
  const afterCam = (((await call('/api/call/poll?after=0&ver=-1', U)).data.calls || {}).room || []).find((p) => p.user === 't-admin');
  await call('/api/call/leave', A, {});
  const afterLeave = (((await call('/api/call/poll?after=0&ver=-1', U)).data.calls || {}).room || []).map((p) => p.user);
  check('R50', 'calls: presence per conversation, private calls only for their two people, one call at a time, signals relayed opaquely and only inside the call, long-poll wakes on a signal, cam/mic/screen state, leave',
    jA.status === 200 && Array.isArray(jA.data.iceServers) && ((pollU.calls || {}).room || []).some((p) => p.user === 't-admin') && ((pollD.calls || {}).room || []).some((p) => p.user === 't-admin')
    && dmJoin.status === 200 && dmIntruder.status === 400 && badConv.status === 400 && !dmSeenByD && !(stU.calls || {})['dm:t-admin|t-user']
    && sigToD.status === 409 && sig.status === 200 && !!got1 && got1.ct === opaque.ct && waited >= 600 && waited < 20000
    && camState.status === 200 && afterCam && afterCam.cam === true && afterCam.mic === false && afterCam.screen === true && !afterLeave.includes('t-admin') && afterLeave.includes('t-user'),
    `dm ${dmJoin.status}/${dmIntruder.status}, signal ${sigToD.status}/${sig.status}, waited ${waited} ms`);
  await call('/api/call/leave', U, {});
  // Usage bar: 5h / 7d windows (estimated, then Claude's own numbers) + per-session context window
  const uBefore = (await call('/api/usage', A)).data;
  const uRun = await chat(A, 'USAGE please', { fresh: true });
  const uAfter = (await call('/api/usage', A)).data, uOther = (await call('/api/usage', D)).data;
  await chat(A, 'LIMITPARTIAL', { fresh: false });
  const uPartial = (await call('/api/usage', A)).data;
  await chat(A, 'brand new session', { fresh: true });
  const uFresh = (await call('/api/usage', A)).data;
  const in2h = uAfter.five_hour && uAfter.five_hour.resetsAt - Date.now();
  check('R52', 'usage: windows estimated before Claude reports them, then Claude\'s utilization + reset time; context 54k/200k per account + session, cleared by a new session',
    uBefore.five_hour && uBefore.five_hour.source === 'estimate' && uBefore.seven_day && uBefore.seven_day.resetsAt > Date.now()
    && uRun.status === 200 && uAfter.five_hour.source === 'claude' && uAfter.five_hour.utilization === 0.42 && in2h > 7000e3 && in2h <= 7200e3
    && uAfter.context && uAfter.context.tokens === 54000 && uAfter.context.window === 200000 && !uOther.context && !uFresh.context,
    JSON.stringify({ five: uAfter.five_hour, ctx: uAfter.context }).slice(0, 200));
  check('R98', 'usage sync: a limit event without numbers keeps the last utilization + reset time; a percentage (55) is read as 55 %; the page shows the 5h / 7d % from the same number as the fill, re-reads usage live during a reply and every 15 s, and empties a window once it resets',
    uPartial.five_hour && uPartial.five_hour.utilization === 0.42 && uPartial.five_hour.resetsAt === uAfter.five_hour.resetsAt
    && uPartial.seven_day && uPartial.seven_day.source === 'claude' && uPartial.seven_day.utilization === 0.55
    && /<span class="uPctS">/.test(html) && /function windowUse\(/.test(html) && /refreshUsageSoon\(\)/.test(html) && /if \(left === 0\) return \{ left, pct: 0/.test(html) && /15000\); \/\/ the limits are shared/.test(html),
    JSON.stringify({ five: uPartial.five_hour, seven: uPartial.seven_day }).slice(0, 220));
  // Team board (ideas from HyperProductivityManager): tasks, claims, revisions, recurrence, agents.
  const bv0 = (await call('/api/board', A)).data;
  const bHeld = call('/api/board?wait=1&ver=' + bv0.ver, U); // U's board long poll must wake on A's change
  await sleep(500);
  const jan31 = new Date(2031, 0, 31, 17).getTime();
  const bt = await call('/api/board/create', A, { title: 'board task r70', body: '**hello**', status: 'todo', labels: 'x, y', priority: 4, dueAt: jan31, recur: 'monthly' });
  const bWoke = await bHeld;
  const tid = bt.data.task && bt.data.task.id;
  const bClaim = await call('/api/board/claim', A, { id: tid, minutes: 10 });
  const bBlockedU = await call('/api/board/update', U, { id: tid, title: 'hijack' }), bClaimU = await call('/api/board/claim', U, { id: tid });
  const bStale = await call('/api/board/update', A, { id: tid, rev: 1, title: 'stale rev' });
  const bOk = await call('/api/board/update', A, { id: tid, rev: bClaim.data.task.rev, title: 'board task r70 edited', assignee: 't-user' });
  const bDone1 = await call('/api/board/complete', A, { id: tid });
  const after1 = (await call('/api/board/task?id=' + tid, U)).data.task;
  const bDone2 = await call('/api/board/complete', A, { id: tid });
  const after2 = (await call('/api/board/task?id=' + tid, U)).data.task;
  const d1 = new Date(after1.dueAt), d2 = new Date(after2.dueAt);
  const nd = require(path.join(BRIDGE, 'board.js')).nextDue;
  const leap = new Date(nd(new Date(2032, 0, 31, 17).getTime(), 'monthly', 31)).getDate();
  const agentRun = await chat(A, 'BOARDTOOL please');
  const agentOut = JSON.parse((texts(agentRun.events).match(/BOARD:(\{.*?\})/) || [])[1] || '{}');
  const bList = (await call('/api/board', U)).data;
  const agentTask = (bList.tasks || []).find((t) => t.id === agentOut.id);
  const agentBlocks = await call('/api/board/update', A, { id: agentOut.id, title: 'human edit while the agent holds it' }); // A the human ≠ A's Claude
  const agentDetail = (await call('/api/board/task?id=' + agentOut.id, A)).data.task || {};
  const bDel = await call('/api/board/delete', U, { id: agentOut.id });
  const bDel2 = await call('/api/board/delete', A, { id: tid });
  const bGone = !((await call('/api/board', A)).data.tasks || []).some((t) => t.id === tid);
  const onDiskB = JSON.parse(fs.readFileSync(path.join(SCRATCH, 'Board', 'board.json'), 'utf8')).tasks.find((t) => t.id === tid) || {};
  const mcpRt = fs.readFileSync(path.join(SB, '.mcp-runtime.json'), 'utf8');
  check('R99', 'team board: create wakes other people\'s long poll; a claim blocks others\' edits + claims; a stale revision is refused; assignee; monthly repeat re-arms month-end aware (Jan 31 → Feb 28 → Mar 31, leap Feb 29); Claude in a Work run creates / claims / notes as "<person> · Claude" and its claim blocks even its own person; delete leaves a tombstone; board MCP wired into Work runs',
    bt.status === 200 && (bWoke.data.tasks || []).some((t) => t.id === tid) && bClaim.status === 200 && bClaim.data.task.status === 'doing'
    && bBlockedU.status === 409 && bClaimU.status === 409 && bStale.status === 409 && bOk.status === 200 && bOk.data.task.assignee === 't-user'
    && bDone1.status === 200 && after1.status === 'todo' && d1.getMonth() === 1 && d1.getDate() === 28 && bDone2.status === 200 && d2.getMonth() === 2 && d2.getDate() === 31 && leap === 29
    && agentOut.made && agentOut.claimed && agentOut.noted && agentTask && agentTask.claim && agentTask.claim.agent && /· Claude$/.test(agentTask.claim.by)
    && agentBlocks.status === 409 && /progress from the agent r70/.test(agentDetail.body || '') && bDel.status === 409 && bDel2.status === 200 && bGone && onDiskB.deleted === true
    && /board-mcp\.js/.test(mcpRt) && /mcp__board/.test(fs.readFileSync(path.join(SB, 'config.json'), 'utf8')),
    `create ${bt.status}, claim ${bClaim.status}, blocked ${bBlockedU.status}/${bClaimU.status}, stale ${bStale.status}, due ${d1.getMonth() + 1}/${d1.getDate()} → ${d2.getMonth() + 1}/${d2.getDate()} (leap ${leap}), agent ${JSON.stringify(agentOut)}, agent-held edit ${agentBlocks.status}`);
  // Claude plans with the board + calendar (task_create / task_update with text due dates, board_calendar, task_move, task_delete, board_new) and reaches the wider hub (channels / groups it makes, hub_status, sessions_search, my_providers).
  await call('/api/archive/save', A, { title: 'Needle session', transcript: [{ cls: 'me', text: 'remember needle-r116' }] });
  const planRun = await chat(A, 'BOARDPLAN please', { fresh: true });
  const plan = JSON.parse((texts(planRun.events).match(/PLAN:(\{.*?\})(?=\s*$|\n)/) || [])[1] || '{}');
  const tomorrow = new Date(); tomorrow.setDate(tomorrow.getDate() + 1); tomorrow.setHours(17, 0, 0, 0);
  const moreRun = await chat(A, 'TEAMMORE please', { fresh: true });
  const more = JSON.parse((texts(moreRun.events).match(/MORE:(\{.*?\})(?=\s*$|\n)/) || [])[1] || '{}');
  const { parseDue } = require(path.join(BRIDGE, 'board.js'));
  let badThrows = false; try { parseDue('someday maybe'); } catch { badThrows = true; }
  const nextFri = parseDue('next friday'), plus3 = parseDue('+3d'), iso = parseDue('2031-03-04 09:30');
  check('R116', 'Claude plans the board + calendar: text due dates (tomorrow, next friday, +3d, YYYY-MM-DD HH:MM, none; a typo is refused), board_calendar groups by day, task_move changes the column, task_update re-dates / clears, board_new, task_delete leaves a tombstone; wider reach: team_channel_create (+ a post into it), team_group_create, hub_status (people + usage windows), sessions_search finds own past sessions, my_providers lists the host Claude',
    plan.id && plan.dueAt === tomorrow.getTime() && Array.isArray(plan.calDays) && plan.calDays.some((d) => d.startsWith(new Date(tomorrow).toISOString().slice(0, 10)) || d.includes(':1')) && plan.movedTo === 'review'
    && typeof plan.redue === 'number' && new Date(plan.redue).getDay() === 5 && plan.undue === null && /Could not read the due date/.test(plan.badDue) && Array.isArray(plan.board) && plan.board.includes('plan-board-r116') && plan.deleted === true && /No such task/.test(plan.afterGet)
    && badThrows && new Date(nextFri).getDay() === 5 && nextFri > Date.now() && Math.round((plus3 - parseDue('today')) / 86400e3) === 3 && new Date(iso).getHours() === 9 && new Date(iso).getMinutes() === 30
    && more.conv === 'ch:mills/claude-made' && more.said === true && Array.isArray(more.grp) && more.grp.includes('t-admin') && more.grp.includes('t-user') && more.people >= 3 && Array.isArray(more.usage) && more.usage.includes('fiveHour') && more.hits === 1 && Array.isArray(more.providers) && more.providers.includes('host'),
    `plan ${JSON.stringify(plan).slice(0, 260)} | more ${JSON.stringify(more).slice(0, 200)}`);
  const boardJs117 = fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8');
  check('R117', '[static] board, ideas from HyperProductivityManager: due-today / overdue reminders once a day for tasks assigned to you, Delegate (assignee + signed hand-over note), whole-board activity dialog, "/" focuses the filter, Ctrl+1/2/3 switch Board / Calendar / Work',
    /function remindDue\(/.test(boardJs117) && /mills\.remind\./.test(boardJs117) && /new Notification\(/.test(boardJs117) && /async function delegate\(/.test(boardJs117) && /appendNote: 'Handed to '/.test(boardJs117)
    && /function showActivity\(/.test(boardJs117) && /act\.onclick = showActivity/.test(boardJs117) && /\$\('bdFilter'\)\.focus\(\)/.test(boardJs117) && /\{ 1: 'board', 2: 'cal', 3: 'shell' \}/.test(boardJs117));
  // Team notes (bridge/notes.js): folders + markdown files, case-insensitive names, no path escape, conflict check, search, Claude's tools.
  const nt0 = (await call('/api/notes', U)).data;
  const ntF = await call('/api/notes/folder', U, { path: 'Meetings' });
  const ntW = await call('/api/notes/write', U, { path: 'Meetings/2026-10-17.md', text: 'line one\r\nneedle-r118 here\r\n' });
  const ntDup = await call('/api/notes/write', U, { path: 'meetings/2026-10-17.MD', text: 'other case' }); // same name ignoring case: refused
  const ntEsc = await call('/api/notes/write', U, { path: '../outside.md', text: 'x' }), ntDot = await call('/api/notes/write', U, { path: '.hidden.md', text: 'x' }), ntNoMd = await call('/api/notes/write', U, { path: 'Meetings/plain.txt', text: 'x' });
  const ntR = (await fetch(H + '/api/notes/read?path=' + encodeURIComponent('meetings/2026-10-17.md'), { headers: { Authorization: 'Bearer ' + A } })).json ? await (await fetch(H + '/api/notes/read?path=' + encodeURIComponent('meetings/2026-10-17.md'), { headers: { Authorization: 'Bearer ' + A } })).json() : {};
  const ntStale = await call('/api/notes/write', A, { path: 'Meetings/2026-10-17.md', text: 'stale', baseAt: 1 });
  const ntOk = await call('/api/notes/write', A, { path: 'Meetings/2026-10-17.md', text: 'line one\nneedle-r118 here\nadded by A\n', baseAt: ntR.at });
  const ntS = await (await fetch(H + '/api/notes/search?q=needle-r118%20added', { headers: { Authorization: 'Bearer ' + U } })).json();
  const ntMv = await call('/api/notes/move', U, { from: 'Meetings/2026-10-17.md', to: 'Meetings/Archive/first.md' });
  const ntTree = (await call('/api/notes', U)).data;
  const ntRun = await chat(A, 'NOTESTOOL please', { fresh: true });
  const ntOut = JSON.parse((texts(ntRun.events).match(/NOTES:(\{.*?\})(?=\s*$|\n)/) || [])[1] || '{}');
  const ntDelFull = await call('/api/notes/delete', U, { path: 'Meetings' }); // not empty → refused
  await call('/api/notes/delete', U, { path: 'Meetings/Archive/first.md' }); await call('/api/notes/delete', U, { path: 'Meetings/Archive' });
  const ntDelOk = await call('/api/notes/delete', U, { path: 'Meetings' });
  const onDiskNotes = fs.existsSync(path.join(SCRATCH, 'Notes'));
  check('R118', 'team notes: folders + .md notes on the host (Mills/Notes), CRLF → LF, names case-insensitive (duplicate refused), no path escape / hidden / non-.md, read by anyone signed in, stale save → 409, search over path + text with snippets, move into a new folder, Claude writes / lists / reads (any case) / deletes as "<person> · Claude" and cannot escape, non-empty folder not deletable; [static] Notes screen + menu button + web/notes.js',
    Array.isArray(nt0.tree) && ntF.status === 200 && ntW.status === 200 && ntW.data.created === true && ntDup.status === 400 && ntEsc.status === 400 && ntDot.status === 400 && ntNoMd.status === 400
    && ntR.text === 'line one\nneedle-r118 here\n' && /^(Tuser|Tee Nick)$/.test(ntR.by) && ntStale.status === 409 && ntOk.status === 200 && ntOk.data.by === 'Tadmin'
    && (ntS.items || []).length === 1 && /needle-r118/.test(ntS.items[0].snippet) && ntMv.status === 200 && ntMv.data.to === 'Meetings/Archive/first.md'
    && ntTree.tree.some((f) => f.type === 'folder' && f.name === 'Meetings' && f.children.some((c) => c.type === 'folder' && c.name === 'Archive' && c.children.some((n) => n.name === 'first')))
    && ntOut.wrote === 'Tadmin · Claude' && Array.isArray(ntOut.listed) && ntOut.listed.includes('plans/Claude Plan.md') && ntOut.read === true && ntOut.bad === false && ntOut.deleted === true
    && ntDelFull.status === 400 && ntDelOk.status === 200 && onDiskNotes
    && /id="goNotes"/.test(html) && /<div id="notes" class="screen"/.test(html) && /<script src="notes\.js"><\/script>/.test(html) && /const NOTES = /.test(html) && /NOTES\.onVersion\(d\.notesVer\)/.test(html),
    `write ${ntW.status}, dup ${ntDup.status}, esc ${ntEsc.status}, read ${JSON.stringify((ntR.text || '').slice(0, 30))} by ${ntR.by}, stale ${ntStale.status}, ok ${ntOk.status} by ${ntOk.data && ntOk.data.by}, search ${(ntS.items || []).length}, move ${ntMv.status}, tree ${JSON.stringify(ntTree.tree).slice(0, 120)}, claude ${JSON.stringify(ntOut)}, del ${ntDelFull.status}/${ntDelOk.status}`);
  check('R119', '[static] My day planner (Board + Calendar headers): Overdue / Today / This week / Later / No date over the tasks assigned to or held by you, quick actions Done / Tomorrow / Next week / Open / Claude, "Plan with Claude" hands the list to Work',
    /id="bdMyDay"/.test(html) && /id="calMyDay"/.test(html) && /<div id="myDay" class="modal" hidden>/.test(html) && /function renderMyDay\(/.test(boardJs117 + fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8'))
    && /function planWithClaude\(/.test(fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8')) && /b\('repeat', 'Next week'/.test(fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8')) && /Plan my day\./.test(fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8')));
  // Forgejo issue mirroring (bridge/forgejo.js) against the fake Forgejo: both directions, no loops.
  const fjView = (await call('/api/board', U)).data;
  const fjT = (await call('/api/board/create', U, { title: 'mirror me r120', body: 'from mills', status: 'todo', labels: 'x' })).data.task;
  const until = async (test, ms = 12000) => { for (let t = 0; t < ms; t += 300) { const v = await test(); if (v) return v; await sleep(300); } return null; };
  const fjCard = await until(async () => ((await call('/api/board', U)).data.tasks || []).find((t) => t.id === fjT.id && t.forgejo));
  const fjIssueLive = fjCard && [...fakeForgejo.issues.values()].find((is) => is.number === fjCard.forgejo.number), fjIssue = fjIssueLive && { ...fjIssueLive }; // snapshot: the fake mutates it on later PATCHes
  await call('/api/board/update', U, { id: fjT.id, title: 'mirror me r120 renamed' });
  const fjRenamed = await until(async () => { const is = fakeForgejo.issues.get(fjCard.forgejo.number); return is && is.title === 'mirror me r120 renamed' ? is : null; });
  await call('/api/board/complete', U, { id: fjT.id });
  const fjClosed = await until(async () => { const is = fakeForgejo.issues.get(fjCard.forgejo.number); return is && is.state === 'closed' ? is : null; });
  // a person on Forgejo: makes an issue → a task appears; closes it → the task completes; renames → the title follows
  const fjMade = await (await fetch(`http://127.0.0.1:${OLLAMA_PORT}/fake/forgejo/newissue`, { method: 'POST', body: JSON.stringify({ title: 'from forgejo r120', body: 'issue body', labels: ['bug'] }) })).json();
  const fjIn = await until(async () => ((await call('/api/board', U)).data.tasks || []).find((t) => t.forgejo && t.forgejo.number === fjMade.number));
  const fjInDetail = fjIn ? (await call('/api/board/task?id=' + fjIn.id, U)).data.task : null;
  await fetch(`http://127.0.0.1:${OLLAMA_PORT}/fake/forgejo/issue/${fjMade.number}`, { method: 'PATCH', body: JSON.stringify({ title: 'from forgejo r120 edited', state: 'closed' }) });
  const fjInDone = await until(async () => { const t = ((await call('/api/board', U)).data.tasks || []).find((x) => x.id === (fjIn || {}).id); return t && t.status === 'done' && t.title === 'from forgejo r120 edited' ? t : null; });
  await sleep(3500); // a few more polls: nothing may ping-pong
  const callsAfter = fakeForgejo.calls.filter((c) => c.startsWith('PATCH')).length;
  await sleep(3500);
  const callsLater = fakeForgejo.calls.filter((c) => c.startsWith('PATCH')).length;
  const fjDel = (await call('/api/board/delete', U, { id: fjT.id })).status;
  const fjDelComment = await until(async () => { const is = fakeForgejo.issues.get(fjCard.forgejo.number); return is && (is.comments || []).some((c) => /Deleted on the Mills board/.test(c)) ? is : null; });
  check('R120', 'Forgejo issue mirroring: a new task becomes an issue (title, body + Mills marker), rename and completion follow it (closed), an issue made on Forgejo becomes a task with label forgejo, its close + rename land on the task, no ping-pong between the two, deleting the task closes the issue with a comment; the board view shows the mirror',
    fjView.mirror && fjView.mirror.on === true && fjView.mirror.repo === 'UnityAILab/Mills' && !!fjCard && !!fjIssue && fjIssue.title === 'mirror me r120' && /from mills[\s\S]*Mills task T-\d+/.test(fjIssue.body)
    && !!fjRenamed && !!fjClosed && !!fjIn && fjIn.labels.includes('forgejo') && fjIn.labels.includes('bug') && fjInDetail && /issue body[\s\S]*From Forgejo issue/.test(fjInDetail.body) && !!fjInDone && callsLater === callsAfter
    && fjDel === 200 && !!fjDelComment && fjDelComment.state === 'closed',
    `issue ${fjIssue ? '#' + fjIssue.number : 'none'}, renamed ${!!fjRenamed}, closed ${!!fjClosed}, inbound ${fjIn ? fjIn.id : 'none'} done ${!!fjInDone}, patches ${callsAfter}→${callsLater}, delete ${fjDel} comment ${!!fjDelComment}`
    + ` | mirror ${JSON.stringify(fjView.mirror)} | issue body ${JSON.stringify((fjIssue || {}).body)} | in labels ${JSON.stringify((fjIn || {}).labels)} | in body ${JSON.stringify(((fjInDetail || {}).body || '').slice(0, 80))} | comment state ${(fjDelComment || {}).state}`);
  check('R100', '[static] Board + Calendar in the main menu, web/board.js loaded, Board / Calendar screens, Tasks pane in Work (rail + layout), "Give to Claude", Claude told to claim before working',
    /id="goBoard"/.test(html) && /id="goCal"/.test(html) && /<script src="board\.js"><\/script>/.test(html) && /<div id="board" class="screen"/.test(html) && /<div id="cal" class="screen"/.test(html)
    && /data-pane="tasks"/.test(html) && /const PANES = \['files', 'edit', 'chat', 'tasks', 'roomPane'\]/.test(html) && /function giveToClaude\(/.test(fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8'))
    && /task_claim it first/.test(fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8')));
  check('R53', '[static] usage bar above the Claude messages: 5h hh:mm:ss, 7d dd/hh/mm, colour scale green → orange → yellow → red, purple when out',
    /id="usageBar"/.test(html) && html.indexOf('id="usageBar"') < html.indexOf('id="log"') && /\[0, \[46, 204, 113\]\], \[33, \[255, 159, 26\]\], \[66, \[255, 212, 59\]\], \[100, \[255, 59, 59\]\]/.test(html)
    && /OUT_COLOR = 'rgb\(160, 90, 255\)'/.test(html) && /pad2\(Math\.floor\(s \/ 3600\)\) \+ ':'/.test(html) && /'d ' \+ pad2/.test(html));
  check('R54', '[static] calls: speaking ring from a local level meter, per-person volume + mute kept only in this browser, Mute / Unmute button with M shortcut; theater view (call fills the screen, chat stays left, Esc leaves); screen share as a third sender on every connection with its own full-width tile',
    /\.tile\.speaking \{/.test(html) && /createAnalyser\(\)/.test(html) && /addMeter\(user, e\.track\)/.test(html) && /mills\.vol\.' \+ u/.test(html)
    && /mills\.mute\.' \+ user/.test(html) && /className = 'pVol'/.test(html) && /micOn \? 'mic-off' : 'mic', micOn \? 'Mute' : 'Unmute'/.test(html) && /M = mute \/ unmute yourself/.test(html)
    && /id="callTheater"/.test(html) && /id="callStage"/.test(html) && /#roomCore\.theater \.roomMain/.test(html) && /rc\.classList\.toggle\('theater', theater\)/.test(html) && /theaterOn\(\) && !e\.target\.closest/.test(html)
    && /id="callScreen"/.test(html) && /getDisplayMedia\(/.test(html) && /pc\.addTransceiver\('video', \{ direction: 'sendrecv', streams: \[call\.screenStream\] \}\)/.test(html) && /isScreenTransceiver\(pc, e\.transceiver\)/.test(html) && /\.tile\.screenTile \{ grid-column: 1 \/ -1/.test(html) && /screen: !!call\.screen/.test(html));
  check('R55', '[static] dictation in chat + Work: browser speech recognition, appends (continue), ~3 s silence stop, never auto-sends except "send message" + silence',
    /id="roomDictate"/.test(html) && /id="workDictate"/.test(html) && /window\.SpeechRecognition \|\| window\.webkitSpeechRecognition/.test(html)
    && /setTimeout\(dictStop, 3000\)/.test(html) && /const WAKE = \/\\s\*\\bsend message/.test(html) && /if \(box\.value\) d\.submit\(\)/.test(html)
    && /base: base \? base \+ ' ' : ''/.test(html) && /btn\.hidden = !SpeechRec/.test(html));
  // Unity reads at most the last 6 messages: the 5 the caller sends along + the call itself
  await sleep(1500);
  const seqCtx = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  const ctx8 = Array.from({ length: 8 }, (_, i) => ({ n: 'Someone', t: 'ctx-item-' + (i + 1) }));
  await call('/api/room/send', U, await seal(U, restored, JSON.stringify({ v: 2, t: '@unity FULLECHO ping-ctx-r57', ctx: ctx8 })));
  const ctxAns = await waitAgent(U, restored, seqCtx, (t) => t.includes('ping-ctx-r57'), 20000);
  const ctxText = (ctxAns && ctxAns.text) || '';
  check('R57', 'Unity: only @unity calls her; she gets the call + at most the 5 messages before it (older context dropped)',
    !!ctxAns && ['ctx-item-4', 'ctx-item-5', 'ctx-item-6', 'ctx-item-7', 'ctx-item-8'].every((x) => ctxText.includes(x)) && !ctxText.includes('ctx-item-3') && !ctxText.includes('ctx-item-1'),
    ctxText.slice(0, 160));
  check('R57b', '[static] browsers encrypt for Unity only when a message @-mentions her (or is private to her), with the 5 previous messages',
    /if \(m\.agent && !forAgent\) continue;/.test(html) && /function callsAgent\(/.test(html) && /\.slice\(-5\)/.test(html) && /ctx: agentCtx\(currentConv\)/.test(html));

  // Team tools in a Work run: Claude posts into the chat as "<person> · Claude" (everyone, or privately), sees presence.
  await sleep(1500);
  const seqTeam = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  const teamRun = await chat(A, 'TEAMSAY please', { fresh: true });
  const teamOut = JSON.parse((texts(teamRun.events).match(/TEAM:(\{.*?\})/) || [])[1] || '{}');
  const teamMsgsU = ((await call('/api/room/messages?since=' + seqTeam, U)).data.messages || []).filter((m) => m.from === 'claude' && m.as);
  const teamMsgsD = ((await call('/api/room/messages?since=' + seqTeam, D)).data.messages || []).filter((m) => m.from === 'claude' && m.as);
  const roomPost = teamMsgsU.find((m) => !m.to), dmPost = teamMsgsU.find((m) => m.to === 't-user');
  const roomText = roomPost ? await roomRead(restored, roomPost) : null, dmText = dmPost ? await roomRead(restored, dmPost) : null;
  await sleep(2500);
  const unityReplied = ((await call('/api/room/messages?since=' + seqTeam, U)).data.messages || []).some((m) => m.from === 'claude' && !m.as);
  check('R108', 'team tools: a Work run posts into the chat as "<person> · Claude" (label + via outside the encryption, body sealed with the hub key so every member reads it), a private post reaches only its person, unknown person refused, presence listed, Unity does not answer these; team MCP wired into Work runs',
    teamOut.all === 'Tadmin · Claude' && teamOut.dm === 't-user' && teamOut.bad === false && teamOut.people >= 3
    && !!roomPost && roomPost.via === 't-admin' && roomText === 'hello team from a Work run r108' && !!dmPost && dmText === 'private note r108'
    && teamMsgsD.some((m) => !m.to) && !teamMsgsD.some((m) => m.to) && !unityReplied
    && /team-mcp\.js/.test(fs.readFileSync(path.join(SB, '.mcp-runtime.json'), 'utf8')) && /mcp__team/.test(fs.readFileSync(path.join(SB, 'config.json'), 'utf8'))
    && /const authorOf = \(m\) => m\.as \|\| nickOf\(m\.from\)/.test(html),
    `tool ${JSON.stringify(teamOut)}, room "${roomText}", dm "${dmText}", D sees dm ${teamMsgsD.some((m) => m.to)}, Unity replied ${unityReplied}`);

  // Servers, channels and groups (bridge/guilds.js): membership decides key slots, visibility, typing, seen, calls, Unity's answers.
  const g0 = (await call('/api/guilds', U)).data;
  const gSv = await call('/api/guilds/server', U, { name: 'Dev Team', members: ['t-user'] });
  const gSid = gSv.data.result && gSv.data.result.id;
  const chn = await call('/api/guilds/channel', U, { server: gSid, name: 'Build Logs', topic: 'ci' });
  const chConv = 'ch:' + gSid + '/build-logs';
  const grp = await call('/api/guilds/group', U, { name: 'us two', members: ['t-admin'] });
  const grpConv = grp.data.result && 'grp:' + grp.data.result.id;
  const aView = (await call('/api/guilds', A)).data;
  const sealConv = async (tok, priv, from, plain, conv, agent = false) => ({ ...(await seal4(tok, priv, from, { v: 2, t: plain }, '', { agent })), conv });
  const chSent = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'channel hello-r113', chConv)); // wraps for everyone: the hub must keep members only
  const chDisk = chSent.status === 200 ? (fs.readFileSync(path.join(SCRATCH, 'Chat', 'messages.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)).find((m) => m.seq === chSent.data.seq) || {}) : {};
  const chSeenByA = ((await call('/api/room/messages?since=' + (chSent.data.seq - 1), A)).data.messages || []).some((m) => m.seq === chSent.data.seq);
  const chOutsider = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'intruder', chConv));
  const gBadConv = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'x', 'ch:nope/general'));
  const tyOutsider = await call('/api/room/typing', A, { conv: chConv, on: true }), tyMember = await call('/api/room/typing', U, { conv: chConv, on: true });
  const seenMember = await call('/api/room/seen', U, { conv: chConv, seq: chSent.data.seq }), seenOutsider = await call('/api/room/seen', A, { conv: chConv, seq: 1 });
  const callOutsider = await call('/api/call/join', A, { conv: chConv, mic: true }), callMember = await call('/api/call/join', U, { conv: chConv, mic: true });
  await call('/api/call/leave', U, {});
  const grpSent = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'group hello-r113', grpConv));
  const grpForU = ((await call('/api/room/messages?since=' + (grpSent.data.seq - 1), U)).data.messages || []).find((m) => m.seq === grpSent.data.seq);
  const grpForD = ((await call('/api/room/messages?since=' + (grpSent.data.seq - 1), D)).data.messages || []).some((m) => m.seq === grpSent.data.seq);
  const grpText = grpForU ? await read4(restored, grpForU).catch(() => null) : null;
  const added = await call('/api/guilds/server/add', U, { server: gSid, add: ['t-admin'] });
  const aViewAfter = (await call('/api/guilds', A)).data;
  const chSent2 = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'now A sees-r113', chConv));
  const chForA2 = ((await call('/api/room/messages?since=' + (chSent2.data.seq - 1), A)).data.messages || []).find((m) => m.seq === chSent2.data.seq);
  const chText2 = chForA2 ? await read4(privA, chForA2).catch(() => null) : null;
  await sleep(1500);
  const seqG = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await sealConv(U, restored, 't-user', '@unity ping-ch-r113', chConv, true));
  const ansCh = await waitAgent(U, restored, seqG, (t) => t.includes('ping-ch-r113'), 20000);
  const ansChMsg = ansCh && ((await call('/api/room/messages?since=' + seqG, U)).data.messages || []).find((m) => m.seq === ansCh.seq);
  const teamCh = await chat(A, 'TEAMSAY please', { fresh: true }); // (team tools: the fake also posts; here we only need the hub to accept conv)
  const gDelOther = await call('/api/guilds/channel/delete', A, { server: gSid, channel: 'build-logs' });
  const gDelOwn = await call('/api/guilds/channel/delete', U, { server: gSid, channel: 'build-logs' });
  const gLeave = await call('/api/guilds/group/leave', U, { group: grp.data.result.id });
  const gAfterLeave = (await call('/api/guilds', U)).data;
  const rmSv = await call('/api/guilds/server/delete', U, { server: gSid });
  check('R113', 'servers / channels / groups: default Mills server with #general = room; a person creates a server + channel + group; a channel message keeps key slots for members only (outsider gets nothing, cannot post, type, mark seen or join its call; unknown channel → 400); a group message reaches its two people only; adding a member makes new messages readable to them; @unity in a channel is answered in that channel; only the creator deletes a channel; leaving a group; deleting the server',
    g0.servers.length === 1 && g0.servers[0].id === 'mills' && g0.servers[0].channels[0].conv === 'room' && gSv.status === 200 && !!gSid && chn.status === 200 && grp.status === 200 && !!grpConv
    && !aView.servers.some((s) => s.id === gSid) && aView.groups.some((g) => g.conv === grpConv)
    && chSent.status === 200 && chDisk.conv === chConv && Object.keys(chDisk.keys || {}).sort().join() === 't-user' && !chSeenByA
    && chOutsider.status === 400 && gBadConv.status === 400 && tyOutsider.status === 400 && tyMember.status === 200 && seenMember.status === 200 && seenOutsider.status === 400 && callOutsider.status === 400 && callMember.status === 200
    && grpSent.status === 200 && grpForU && grpText && grpText.t === 'group hello-r113' && !grpForD
    && added.status === 200 && aViewAfter.servers.some((s) => s.id === gSid) && chSent2.status === 200 && chText2 && chText2.t === 'now A sees-r113'
    && !!ansCh && ansChMsg && ansChMsg.conv === chConv && teamCh.status === 200
    && gDelOther.status === 400 && gDelOwn.status === 200 && gLeave.status === 200 && !gAfterLeave.groups.some((g) => g.conv === grpConv) && rmSv.status === 200,
    `server ${gSv.status}, channel ${chn.status}, send ${chSent.status} slots ${Object.keys(chDisk.keys || {}).join('+')}, A sees ${chSeenByA}, outsider ${chOutsider.status}/${tyOutsider.status}/${seenOutsider.status}/${callOutsider.status}, group ${grpSent.status} U "${grpText && grpText.t}" D ${grpForD}, after add "${chText2 && chText2.t}", unity ${!!ansCh} in ${ansChMsg && ansChMsg.conv}, delete ${gDelOther.status}/${gDelOwn.status}`);

  // Shared Work sessions: watch-only windows with live typing / queue, summaries as context
  const shA = await call('/api/work/share', A, { conv: 'room', transcript: [{ cls: 'me', text: 'seed-hello-r58' }, { cls: 'claude', text: 'seed-reply-r58' }] });
  const shareListU = (await call('/api/room/messages?rev=0', U)).data.shares || [], shareListA = (await call('/api/room/messages?rev=0', A)).data.shares || [];
  const wa1 = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', U)).data;
  const seedOk = (wa1.events || []).some((e) => e.t === 'user' && e.text === 'seed-hello-r58') && (wa1.events || []).some((e) => e.t === 'claude' && e.text === 'seed-reply-r58');
  const t58 = Date.now();
  const wPending = call(`/api/work/watch?user=t-admin&after=${wa1.after}&ver=${wa1.ver}&since=${encodeURIComponent(wa1.since)}`, U);
  await sleep(600);
  const ranA = await chat(A, 'live-run-r58', { fresh: true });
  const wa2 = (await wPending).data, waited58 = Date.now() - t58;
  const liveRun = (wa2.events || []).some((e) => e.t === 'user' && e.text === 'live-run-r58');
  await call('/api/work/live', A, { typing: 'typing-r58', queue: ['queued-r58'] });
  const wa3 = (await call(`/api/work/watch?user=t-admin&after=${wa2.after}&ver=${wa2.ver}&since=${encodeURIComponent(wa2.since)}`, U)).data;
  const ctxRun = await chat(A, 'with-context-r58', { context: 'CTX-SUMMARY-R58' });
  const wa4 = (await call(`/api/work/watch?user=t-admin&after=${wa3.after}&ver=${wa3.ver}&since=${encodeURIComponent(wa3.since)}`, U)).data;
  const ctxMirrored = (wa4.events || []).some((e) => e.text.includes('CTX-SUMMARY-R58') && e.t === 'user');
  const dmShare = await call('/api/work/share', A, { conv: 'dm:t-admin|t-user', transcript: [] }); // per-person sharing is gone: refused, everyone-share stays
  const dmD = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', D)).data, dmU = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', U)).data;
  const selfWatch = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', A)).data;
  await call('/api/work/share', A, { conv: '' });
  const afterStop = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', U)).data;
  check('R58', 'shared Work: shared with everyone only (a per-person share → 400), listed for everyone (not to yourself), seeded transcript, live run wakes watchers, typing + queue live, context reaches own Claude but not watchers, stop',
    shA.status === 200 && shareListU.some((s) => s.user === 't-admin' && s.conv === 'room') && !shareListA.some((s) => s.user === 't-admin')
    && seedOk && liveRun && waited58 >= 500 && ranA.status === 200 && wa3.live && wa3.live.typing === 'typing-r58' && (wa3.live.queue || [])[0] === 'queued-r58'
    && texts(ctxRun.events).includes('CTX-SUMMARY-R58') && !ctxMirrored && dmShare.status === 400 && !dmD.stopped && !dmU.stopped && selfWatch.stopped === true && afterStop.stopped === true,
    `seed ${seedOk}, live ${liveRun} (${waited58} ms), per-person share ${dmShare.status}, D watches ${!dmD.stopped}, U watches ${!dmU.stopped}`);
  check('R58b', '[static] share button (Share with everyone / Stop sharing, no per-person choice), Join work banner, watch-only windows with live typing + queue, summaries in Work messages',
    /id="shareBtn"/.test(html) && !/id="shareSel"/.test(html) && /'Stop sharing' : 'Share with everyone'/.test(html) && /const conv = myShare \? '' : 'room'/.test(html) && /id="shareBanner"/.test(html) && /function openWatch\(/.test(html) && /\/api\/work\/watch\?user=/.test(html)
    && /function watchContext\(/.test(html) && /context \? \{ context \} : \{\}/.test(html) && /\/api\/work\/live/.test(html));
  check('R56', '[static] layout stays inside the viewport: page = viewport (dvh), flex children may shrink, long text wraps, floating windows clamped',
    /height: 100dvh/.test(html) && /html \{ height: 100%; overflow: hidden; \}/.test(html) && /Stay inside the viewport/.test(html)
    && /\.msg \{ min-width: 0; max-width: 100%; overflow-wrap: anywhere/.test(html) && /\.floatWin \{ max-width: calc\(100vw - 16px\)/.test(html));
  check('R51', '[static] Past sessions naming + auto-save (default 5 min, auto-named, keeps overwriting the same saved session)',
    />Past sessions<\/button>/.test(html) && />Save session<\/button>/.test(html) && !/>Archive<\/button>/.test(html) && /id="setAutosave"/.test(html)
    && /autosave: p\.autosave \?\? 5/.test(html) && /saveToArchive\(true, true\)/.test(html) && /if \(auto\) title = autoName\(\)/.test(html));
  const ids48 = ['callBtn', 'callBanner', 'callCore', 'callTiles', 'callMic', 'callCam', 'callLeave', 'callFloat', 'callSideHost', 'audNoise', 'audEcho', 'audAgc', 'callVolume', 'callQuality', 'micBtn', 'recBar', 'recSend', 'recCancel', 'clipVolume', 'clipAuto','rail', 'panes', 'roomPane', 'roomFloat', 'roomCore', 'roomSide', 'settings', 'setNick', 'setColors', 'volMessage', 'volMention', 'volReply', 'setToasts', 'toasts', 'replyBar', 'mentionList', 'railRoomBadge', 'goSettings', 'setTts', 'ttsVolume', 'ttsRate', 'ttsVoice', 'ttsScope', 'ttsStop'];
  check('R48', '[static] Work rail + resizable / minimizable panes + floating chat, settings in menu / chat / rail, tones, toasts, receipts, edit / delete / reply / @',
    ids48.every((id) => html.includes(`id="${id}"`)) && (html.match(/data-settings/g) || []).length >= 4 && /function bindSplit/.test(html) && /data-min="files"/.test(html)
    && /TONES = \{ message:/.test(html) && /function toast\(/.test(html) && /function seenText/.test(html) && /\/api\/room\/edit/.test(html) && /\/api\/room\/delete/.test(html)
    && /function insertMention/.test(html) && /PALETTE = \[/.test(html) && /timeStyle/.test(html) && /new SpeechSynthesisUtterance/.test(html) && /function ttsNext/.test(html) && /getUserMedia/.test(html) && /new MediaRecorder/.test(html) && /function enqueueClip/.test(html)
    && /new RTCPeerConnection/.test(html) && /setCodecPreferences/.test(html) && /'video\/VP9', 'video\/AV1'/.test(html) && /useinbandfec=1/.test(html)
    && /noiseSuppression: a\.noise/.test(html) && /audio: audioConstraints\(\)/.test(html) && /<use href="#i-camera"\/><\/svg>Camera off<\/button>/.test(html) && /const SNAPS = /.test(html)
    && /crypto\.subtle\.encrypt\(\{ name: 'AES-GCM', iv \}, k, enc\.encode\(JSON\.stringify\(\{ conv: call\.conv/.test(html), ids48.filter((id) => !html.includes(`id="${id}"`)).join(', '));

  { // Unity section (assistant): own block so its names never clash with other checks
  async function uchat(tok, message, client = { tz: 'Asia/Tokyo', locale: 'en-US' }) {
    const r = await fetch(H + '/api/assistant/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok }, body: JSON.stringify({ message, client }) });
    if (r.status !== 200) return { status: r.status, events: [] };
    const events = (await r.text()).split('\n').filter((l) => l.trim()).map((l) => JSON.parse(l));
    return { status: 200, events, final: (events.find((e) => e.type === 'final') || {}).text || '', err: (events.find((e) => e.type === 'error') || {}).text || '' };
  }
  const us = (await call('/api/assistant/state', U)).data;
  check('R61', 'Unity section state: local brain default and loaded, Claude models offered', us.brain === 'local' && us.local && us.local.model === true && us.brains.some((b) => b.id === 'local') && us.brains.some((b) => b.id === 'claude-opus-5-5'), JSON.stringify(us.local || us));
  const t1 = await uchat(U, 'TASK buy regress milk');
  const dataU = (await call('/api/assistant/data', U)).data, dataA = (await call('/api/assistant/data', A)).data;
  const said = t1.events.find((e) => e.type === 'say');
  check('R62', 'local brain: tool call adds a task for that person only; reply spoken via say; name + their time zone in the prompt', t1.events.some((e) => e.type === 'tool' && e.name === 'add_task')
    && (dataU.tasks || []).some((t) => t.title === 'buy regress milk') && !(dataA.tasks || []).length && !!said && /sys-has-name=true tz=Asia\/Tokyo/.test(t1.final), t1.err || t1.final);
  const tid = ((dataU.tasks || [])[0] || {}).id;
  const t2 = await uchat(U, 'DELETE ' + tid);
  const waiting = (await call('/api/assistant/state', U)).data.pending;
  const stillThere = ((await call('/api/assistant/data', U)).data.tasks || []).some((t) => t.id === tid);
  const ans = await call('/api/assistant/answer', U, { yes: true });
  const gone = !((await call('/api/assistant/data', U)).data.tasks || []).some((t) => t.id === tid);
  check('R63', 'destructive tool waits for Yes: confirm event, nothing deleted until the answer', t2.events.some((e) => e.type === 'confirm') && waiting && waiting.tool === 'delete_task' && stillThere && ans.data.ok && gone);
  const t3 = await uchat(U, 'TIME please', { tz: 'America/Chicago', locale: 'en-US' });
  const timeRes = (t3.events.find((e) => e.type === 'tool_result' && e.name === 'get_time') || {}).result || {};
  check('R64', "get_time answers in the person's own time zone (from their browser)", timeRes.timezone === 'America/Chicago' && /\d/.test(timeRes.now || ''));
  await uchat(U, 'REMIND me');
  let notice = null;
  for (let i = 0; i < 50 && !notice; i++) { await sleep(500); notice = ((await call('/api/assistant/state', U)).data.notices || []).find((n) => n.text === 'regress reminder'); }
  check('R65', 'a due reminder reaches that person as a notice', !!notice);
  const badBrain = await call('/api/assistant/brain', U, { brain: 'gpt-9' });
  const toClaude = await call('/api/assistant/brain', U, { brain: 'claude-opus-5-5' });
  const t4 = await uchat(U, 'ASSISTTOOL add it');
  const viaClaude = ((await call('/api/assistant/data', U)).data.tasks || []).some((t) => t.title === 'from claude brain');
  await call('/api/assistant/brain', U, { brain: 'local' });
  check('R66', 'brain dropdown: unknown → 400; Claude brain uses the same tools through the relay', badBrain.status === 400 && toClaude.status === 200 && t4.status === 200 && viaClaude, t4.err);
  const look = (readJson(path.join(BRIDGE, 'config.json'), {}).assistant || {}).selfieLook || '';
  const sf = await uchat(U, 'SELFIE now');
  const imgEv = sf.events.find((e) => e.type === 'image');
  const ownImg = imgEv ? await fetch(H + '/api/assistant/image?ref=' + encodeURIComponent(imgEv.ref), { headers: { Authorization: 'Bearer ' + U } }) : null;
  const otherImg = imgEv ? await fetch(H + '/api/assistant/image?ref=' + encodeURIComponent(imgEv.ref), { headers: { Authorization: 'Bearer ' + A } }) : null;
  const sentPrompt = fakeImagePrompts[fakeImagePrompts.length - 1] || '';
  check('R68', 'selfie: picture made by the host engine, scene first then her full look, private picture only for its owner', !!imgEv && imgEv.ref.startsWith('t-user/')
    && ownImg.status === 200 && (ownImg.headers.get('content-type') || '').includes('image/png') && otherImg.status === 404
    && sentPrompt.startsWith('regress moment') && (!look || sentPrompt.includes(look)), sentPrompt.slice(0, 80));
  const eng = fs.readFileSync(path.join(BRIDGE, 'image-engine.py'), 'utf8');
  check('R69', '[static] image engine: no prompt cap (long prompts encoded in pieces), no built-in negative prompt', /def _embeds/.test(eng) && !/_tokens_fit/.test(eng) && /^NEG = ""/m.test(eng));
  const seqI = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await roomSend(U, restored, '@unity SELFIE for the room');
  const roomPic = await waitAgent(U, restored, seqI, (t) => /\[\[image:room\/[a-z0-9-]+\]\]/.test(t), 20000);
  const roomRef = roomPic && /\[\[image:(room\/[a-z0-9-]+)\]\]/.exec(roomPic.text)[1];
  const seenByA = roomRef ? (await fetch(H + '/api/assistant/image?ref=' + encodeURIComponent(roomRef), { headers: { Authorization: 'Bearer ' + A } })).status : 0;
  check('R70', 'chat-room @unity runs on the local brain: a room picture is attached and every member can load it', !!roomPic && seenByA === 200);
  const cl = await call('/api/assistant/client', U, { client: { tz: 'America/Denver', locale: 'en-US' } });
  const t5 = await uchat(U, 'TIME again', {});
  const tr5 = (t5.events.find((e) => e.type === 'tool_result' && e.name === 'get_time') || {}).result || {};
  check('R71', "the person's clock reported at sign-in is used when a message carries none", cl.data.tz === 'America/Denver' && tr5.timezone === 'America/Denver');
  const pol = spawnSync('git', ['grep', '-n', '-i', '-I', 'pollinations', '--', '.', ':!bridge/test/regress.js'], { cwd: MILLS, encoding: 'utf8' });
  check('R72', 'no hosted image service (Pollinations) anywhere in tracked files', pol.status === 1, (pol.stdout || '').split('\n')[0]);
  const sayR = await fetch(H + '/api/assistant/say', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + U }, body: JSON.stringify({ text: 'hello [[image:room/x]] *you*', emotion: 'horny', act: 'moan' }) });
  const sayBody = Buffer.from(await sayR.arrayBuffer()).toString();
  const sent = fakeVoiceReqs[fakeVoiceReqs.length - 1] || {};
  const emptySay = await call('/api/assistant/say', U, { text: '   ' });
  check('R73', 'her voice: a line comes back as audio/wav; unknown emotion/act mapped to ones the voice knows; image markers and markdown not read out', sayR.status === 200 && (sayR.headers.get('content-type') || '').includes('audio/wav')
    && sayBody.startsWith('RIFF') && sent.emotion === 'neutral' && sent.speech_act === 'statement' && sent.text === 'hello  you' && emptySay.status === 400, JSON.stringify(sent));
  const lis = await fetch(H + '/api/assistant/listen', { method: 'POST', headers: { 'Content-Type': 'audio/webm', Authorization: 'Bearer ' + U }, body: Buffer.alloc(1234, 7) });
  const lisD = await lis.json().catch(() => ({}));
  const lisEmpty = await fetch(H + '/api/assistant/listen', { method: 'POST', headers: { 'Content-Type': 'audio/webm', Authorization: 'Bearer ' + U }, body: Buffer.alloc(0) });
  const lisAnon = await fetch(H + '/api/assistant/listen', { method: 'POST', headers: { 'Content-Type': 'audio/webm' }, body: Buffer.alloc(10, 1) });
  check('R74', 'speech to text: a recording comes back as text; empty → 400; signed-out → 401', lis.status === 200 && lisD.text === 'fake heard 1234' && lisEmpty.status === 400 && lisAnon.status === 401, JSON.stringify(lisD));
  check('R75', '[static] Unity screen has the mic button and the voice toggle', /id="assistMic"/.test(html) && /id="assistVoice"/.test(html) && /MediaRecorder/.test(html));
  const dot = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
  const up = await call('/api/assistant/upload', U, { image: dot });
  const upBad = await call('/api/assistant/upload', U, { image: 'data:text/html;base64,PHNjcmlwdD4=' });
  const upOther = up.data.ref ? (await fetch(H + '/api/assistant/image?ref=' + encodeURIComponent(up.data.ref), { headers: { Authorization: 'Bearer ' + A } })).status : 0;
  check('R76', 'photo upload (webcam / file): stored as that person\'s private picture; non-images refused', /^t-user\/[a-z0-9-]+$/.test(up.data.ref || '') && upBad.status === 400 && upOther === 404, JSON.stringify(up.data));
  fakeSawPhoto = false;
  const ed = await (async () => { const r = await fetch(H + '/api/assistant/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + U }, body: JSON.stringify({ message: 'EDITME please', photo: up.data.ref, client: { tz: 'UTC' } }) }); return (await r.text()).split('\n').filter((l) => l.trim()).map((l) => JSON.parse(l)); })();
  const e1 = fakeEdits.find((e) => e.route === '/img2img') || {};
  const edFinal = ed.find((e) => e.type === 'final') || {};
  check('R77', 'sent photo: the brain sees it (vision) and edit_image works from it, keeping the face; result attached', fakeSawPhoto && e1.prompt === 'regress edit scene' && e1.face === true
    && e1.image === dot.split(',')[1] && (edFinal.images || []).length === 1, JSON.stringify({ fakeSawPhoto, face: e1.face, images: edFinal.images }));
  await uchat(U, 'PAINTME now');
  const e2 = fakeEdits.find((e) => e.route === '/inpaint') || {};
  check('R78', 'inpaint_image repaints a box of the last picture', JSON.stringify(e2.box) === '[0.1,0.1,0.5,0.5]' && e2.prompt === 'a red hat' && typeof e2.image === 'string' && e2.image.length > 10);
  check('R79', '[static] Unity screen has camera and file buttons', /id="assistCam"/.test(html) && /id="assistFile"/.test(html) && /getUserMedia\(\{ video: true \}\)/.test(html));
  const toolRes = (ev, name) => (ev.events.find((e) => e.type === 'tool_result' && e.name === name) || {}).result || {};
  const ws = toolRes(await uchat(U, 'SEARCHME'), 'web_search');
  check('R80', 'web_search: results with the real address unwrapped, title and snippet as text', (ws.results || [])[0] && ws.results[0].url === 'https://example.org/regress' && ws.results[0].title === 'Regress result' && ws.results[0].snippet === 'snippet & more', JSON.stringify(ws).slice(0, 150));
  const pl = await uchat(U, 'PLAYME');
  const plr = toolRes(pl, 'play_youtube'), plf = pl.events.find((e) => e.type === 'final') || {};
  check('R81', 'play_youtube: finds the video and attaches the player to the reply', plr.video === 'abcdefghijk' && plr.playing === 'Regress Song' && (plf.media || []).includes('abcdefghijk'));
  const rp = toolRes(await uchat(U, 'README'), 'read_page');
  check('R82', 'read_page refuses the host\'s own network (no localhost / private addresses)', rp.ok === false && /private network/.test(rp.error || ''), rp.error);
  check('R83', '[static] page embeds [[youtube:id]] as a player; a YouTube link posted in the team chat gets an in-chat player (no autoplay; watch / youtu.be / shorts / embed / live forms); the CSP allows exactly that frame host',
    /youtube-nocookie\.com\/embed/.test(html) && /youtube:\[\\w-\]\{11\}/.test(html) && /function ytIds\(/.test(html) && /for \(const vid of ytIds\(rec\.text\)\) body\.append\(ytPlayer\(vid, false\)\)/.test(html)
    && /frame-src https:\/\/www\.youtube-nocookie\.com;/.test(csp) && !/frame-src[^;]*(https:|\*)[;"]/.test(csp));
  // the runner plays the browser: it answers her own-computer calls while the reply streams
  async function uchatLive(tok, message, answer) {
    const r = await fetch(H + '/api/assistant/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok }, body: JSON.stringify({ message, client: { tz: 'UTC' } }) });
    const events = [], reader = r.body.getReader(), td = new TextDecoder(); let buf = '';
    for (;;) {
      const { value, done } = await reader.read(); if (done) break;
      buf += td.decode(value, { stream: true }); let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i); buf = buf.slice(i + 1); if (!line.trim()) continue;
        const ev = JSON.parse(line); events.push(ev);
        if (ev.type === 'pc_tool' || ev.type === 'run_code') call('/api/assistant/pc-result', tok, { id: ev.id, ...answer(ev) });
      }
    }
    return events;
  }
  const seenCalls = [];
  const pcEv = await uchatLive(U, 'PCLIST', (ev) => { seenCalls.push(ev); return { ok: true, result: 'a.txt\nnotes/' }; });
  const pcRes = (pcEv.find((e) => e.type === 'tool_result' && e.name === 'pc_list') || {}).result || {};
  check('R84', "pc_list runs in the person's browser (their opened folder) and the answer comes back to her", seenCalls[0] && seenCalls[0].type === 'pc_tool' && seenCalls[0].name === 'list' && seenCalls[0].args.path === 'docs' && pcRes.ok && pcRes.result === 'a.txt\nnotes/', JSON.stringify(pcRes));
  const pyEv = await uchatLive(U, 'RUNPY', (ev) => ({ ok: true, result: ev.language === 'python' && ev.code === 'print(6*7)' ? '42' : 'wrong' }));
  const pyRes = (pyEv.find((e) => e.type === 'tool_result' && e.name === 'run_code') || {}).result || {};
  const stray = await call('/api/assistant/pc-result', A, { id: 'nope', ok: true, result: 'x' });
  check('R85', "run_code runs in the person's browser sandbox (not on the host); stray answers refused", pyRes.ok && pyRes.result === '42' && stray.status === 404, JSON.stringify(pyRes));
  const dl = await uchatLive(U, 'DELME', () => ({ ok: true, result: 'deleted' }));
  const dlAns = await call('/api/assistant/answer', U, { yes: true });
  check('R86', 'pc_delete asks Yes / No first; Yes hands the delete to the page', dl.some((e) => e.type === 'confirm' && e.tool === 'pc_delete') && dlAns.data.browser && dlAns.data.browser.name === 'delete' && dlAns.data.browser.args.path === 'old.txt', JSON.stringify(dlAns.data));
  const notAdmin = await call('/api/admin/unity-tools', U, { username: 't-user', pc: false });
  const off = await call('/api/admin/unity-tools', A, { username: 't-user', pc: false, code: false });
  const stOff = (await call('/api/assistant/state', U)).data.tools || {};
  const sumOff = ((await call('/api/admin/accounts', A)).data.accounts || []).find((x) => x.username === 't-user') || {};
  const offEv = await uchatLive(U, 'PCLIST', () => ({ ok: true, result: 'should not happen' }));
  const offRes = (offEv.find((e) => e.type === 'tool_result' && e.name === 'pc_list') || {}).result || {};
  await call('/api/admin/unity-tools', A, { username: 't-user', pc: true, code: true });
  const stOn = (await call('/api/assistant/state', U)).data.tools || {};
  check('R87', 'admin switches per account: non-admin 403; off → tools refused for that person; on again', notAdmin.status === 403 && off.data.pc === false && off.data.code === false && stOff.pc === false && sumOff.unityPc === false
    && offRes.ok === false && /switched/.test(offRes.error || '') && !offEv.some((e) => e.type === 'pc_tool') && stOn.pc === true && stOn.code === true, JSON.stringify(offRes));
  check('R88', '[static] page: code sandbox workers + admin Unity switches', /function jsWorkerMain/.test(html) && /function pyWorkerMain/.test(html) && /unity-tools/.test(html));
  check('R106', '[static] every message you send shows its status: Work (queued / working / done / stopped / failed), Unity section, @unity in Chat (waiting / answering / answered)',
    /function msgStatus\(/.test(html) && /msgStatus\(el, 'queued'/.test(html) && /msgStatus\(mine, 'working'/.test(html) && /msgStatus\(mineU, 'working'/.test(html) && /function refreshAgentWaits/.test(html) && /'Unity answered'/.test(html));
  const nImg = fakeImagePrompts.length;
  const ti = await uchat(U, 'titties');
  const tiFinal = ti.events.find((e) => e.type === 'final') || {};
  const tiPrompt = fakeImagePrompts[nImg] || '';
  check('R107', 'a picture ask the brain answers with text only still gets a picture (hub backstop), a picture of her with her full look', (tiFinal.images || []).length === 1 && tiPrompt.startsWith('titties') && (!look || tiPrompt.includes(look)), tiPrompt.slice(0, 80));
  const nImg2 = fakeImagePrompts.length;
  const wx = await uchat(U, 'whats up with you today');
  check('R108', 'no picture when none was asked for', !((wx.events.find((e) => e.type === 'final') || {}).images || []).length && fakeImagePrompts.length === nImg2);
  check('R67', '[static] menu has the Unity button and Unity screen', /id="goAssist">(<svg[^]*?<\/svg>)?Unity</.test(html) && /<b>Unity<\/b>/.test(html));
  }

  // Admin
  check('R25', 'non-admin → 403 on admin endpoints', (await call('/api/admin/accounts', U)).status === 403 && (await call('/api/admin/create', U, { username: 'x' })).status === 403);
  const cr = (await call('/api/admin/create', A, { username: 't-new', display: 'Tnew' })).data;
  check('R26', 'admin creates an account; it signs in', !!cr.password && (await login('t-new', cr.password, devA)).status === 200);
  const NT = (await login('t-new', cr.password, devA)).data.token;
  const np = (await call('/api/admin/password', A, { username: 't-new' })).data;
  check('R27', 'new password (throwaway account) signs the target out and works', (await call('/api/check', NT)).status === 401 && (await login('t-new', np.password, devA)).status === 200);
  const refused = (await login('t-new', np.password, devB)).status === 403;
  await call('/api/admin/unlock', A, { username: 't-new' });
  const claimed = (await login('t-new', np.password, devB)).status === 200;
  const so = await call('/api/admin/signout', A, { username: 't-new' });
  const sc = await call('/api/admin/stop', A, { username: 't-user' });
  check('R28', 'reset lock lets a new device claim; sign out; stop', refused && claimed && so.status === 200 && sc.status === 200);
  const delSelf = await call('/api/admin/delete', A, { username: 't-admin' }), delDef = await call('/api/admin/delete', A, { username: 'mills' });
  const del = await call('/api/admin/delete', A, { username: 't-new' });
  const moved = fs.existsSync(path.join(SB, 'profiles-deleted')) && fs.readdirSync(path.join(SB, 'profiles-deleted')).some((d) => d.startsWith('t-new-'));
  check('R29', 'delete moves the folder aside; self and default account protected', delSelf.status === 400 && delDef.status === 400 && del.status === 200 && moved);
  await call('/api/admin/room/clear', A, {});
  check('R30', 'clear chat empties history', ((await call('/api/room/messages?since=0', U)).data.messages || []).length === 0);

  // Persistence & restart (scratch restart.js: graceful flag only)
  const authBefore = sha(path.join(SB, 'profiles', 't-admin', 'auth.json'));
  const pid1 = (await call('/api/status')).data.pid;
  spawnSync(process.execPath, [path.join(SB, 'restart.js')], { cwd: SB });
  let pid2 = pid1;
  for (let i = 0; i < 60 && (pid2 === pid1 || !pid2); i++) { await sleep(250); pid2 = (await call('/api/status')).data.pid; }
  const after = await call('/api/check', U);
  check('R9', 'graceful restart: new pid, token kept, lock + model kept, flag consumed', pid2 && pid2 !== pid1 && after.status === 200 && after.data.model === 'claude-opus-5-5'
    && (await login('t-user', 'pw-t-user-0001', devA)).status === 403 && !fs.existsSync(path.join(SB, '.restart')));

  const pid3 = (await call('/api/status')).data.pid;
  const slow2 = chat(U, 'SLOW during restart');
  await sleep(600);
  fs.writeFileSync(path.join(SB, '.restart'), 'test');
  await sleep(1800);
  const midPid = (await call('/api/status')).data.pid;
  const heldOff = await call('/api/chat', A, { message: 'queued right behind' }), heldOffU = await call('/api/assistant/chat', A, { message: 'queued too' });
  await slow2;
  let pid4 = midPid;
  for (let i = 0; i < 60 && (pid4 === pid3 || !pid4); i++) { await sleep(250); pid4 = (await call('/api/status')).data.pid; }
  check('R10', 'restart waits for an active run, then happens', midPid === pid3 && pid4 && pid4 !== pid3);
  check('R89', 'while a restart is pending no NEW run starts (503, the page retries), so queued messages cannot hold it off', heldOff.status === 503 && heldOffU.status === 503);

  check('R11', 'account password files unchanged (scratch and production)', sha(path.join(SB, 'profiles', 't-admin', 'auth.json')) === authBefore
    && prodAccountFiles().every((f) => prodBefore[f] === sha(f)));

  // Ship hygiene (real repo, read-only)
  const terms = fs.existsSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'))
    ? fs.readFileSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'), 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#')) : [];
  const generic = ['[0-9a-f]{32}', '"hash"\\s*:\\s*"[0-9a-f]', 'C:\\\\\\\\Users\\\\\\\\', 'trycloudflare\\.com/', 'password\\s*[:=]\\s*"[^"]{6,}'];
  const pattern = [...generic, ...terms.map((t) => '\\b' + t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b')].join('|');
  // web/vendor holds third-party libraries (checked by R102 against their published hashes), not our text
  const gg = spawnSync('git', ['grep', '-n', '-I', '-i', '-E', pattern, '--', '.', ':!bridge/test/regress.js', ':!web/vendor'], { cwd: MILLS, encoding: 'utf8' });
  check('R31', `no names / passwords / ids / hashes / local paths in tracked files (${terms.length} local terms)`, gg.status === 1, (gg.stdout || '').split('\n').slice(0, 3).join(' | '));
  const junk = [];
  (function walk(d) {
    for (const f of fs.readdirSync(d, { withFileTypes: true })) {
      if (['.git', '.claude', 'node_modules'].includes(f.name)) continue;
      const p = path.join(d, f.name);
      if (f.isDirectory()) walk(p); else if (/\.tmp$|^zz-|^mills-test-/.test(f.name)) junk.push(path.relative(MILLS, p));
    }
  })(MILLS);
  check('R32', 'no temp / scratch / test leftovers in the Mills folder', !junk.length, junk.slice(0, 5).join(', '));

  // Production (read-only)
  if (NO_PROD) { skip('R33', 'production hub', '--no-prod'); skip('R34', 'production accounts', '--no-prod'); skip('R35', 'account files survive git', '--no-prod'); return; }
  const prodPort = readJson(path.join(PROD, 'config.json'), {}).port || 7777;
  let prod = null; try { prod = await (await fetch(`http://127.0.0.1:${prodPort}/api/status`)).json(); } catch {}
  check('R33', `production: ${prodPort} answers, no leftover old port 7778`, !!(prod && prod.ok) && !(await listening(7778)), prod ? `pid ${prod.pid}, supervised ${prod.supervised}, busy ${prod.busy}${fs.existsSync(path.join(PROD, '.restart')) ? ', RESTART PENDING (flag present)' : ''}` : 'not answering');
  const accts = prodProfiles();
  const intact = accts.every((p) => { const a = readJson(path.join(prodDir(p), 'auth.json'), null); return a && a.salt && a.hash; });
  check('R34', `production: every account (${accts.length}, in ${PROD}) still has an intact password file`, intact, 'sign-in itself is proven by the host (law 6), not by this runner');
  const tracked = spawnSync('git', ['ls-files', 'bridge/profiles', 'bridge/auth.json', 'bridge/state.json', 'bridge/hub-state.json'], { cwd: MILLS, encoding: 'utf8' }).stdout.trim();
  const configs = accts.filter(Boolean).every((p) => fs.existsSync(path.join(prodDir(p), 'config.json')));
  check('R35', 'account files are untracked (git cannot delete them on merge) and every account has its config.json', !tracked && configs, tracked || (configs ? '' : 'a profile is missing config.json'));
}

(async () => {
  let code = 0;
  if (process.argv.includes('--md')) { // quick: only the markdown renderer cases (no throwaway hub)
    const r = await require('./md-check.js').run();
    console.log(r.failed.length ? 'FAILED:\n' + r.failed.join('\n') : `md-check: all ${r.total} cases pass`);
    process.exit(r.failed.length ? 1 : 0);
  }
  try {
    if (process.argv.includes('--install-vendor')) { console.log('Installing vendored libraries (bridge/vendor.js)…'); await require(path.join(BRIDGE, 'vendor.js')).installAll(); console.log(''); }
    await setup();
    console.log(`Throwaway hub: ${SCRATCH} on port ${PORT} (no tunnel). Production is only read.\n`);
    if (!(await startHub())) throw new Error('throwaway hub did not start; see ' + path.join(SCRATCH, 'hub.log'));
    await run();
  } catch (e) { console.log('RUNNER ERROR: ' + e.stack); code = 2; results.push({ id: 'RUNNER', label: 'runner crashed; every check after the last PASS above never ran', status: 'FAIL', detail: e.message }); }
  stopHub();
  if (ollamaSrv) ollamaSrv.close();
  await sleep(800);
  const failed = results.filter((r) => r.status === 'FAIL'), skipped = results.filter((r) => r.status === 'SKIP');
  console.log(`\n${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped.`);
  if (failed.length) code = code || 1;
  if (SCRATCH && !KEEP) { try { fs.rmSync(SCRATCH, { recursive: true, force: true }); } catch (e) { console.log('Could not delete scratch folder: ' + e.message); } }
  else if (SCRATCH) console.log('Scratch kept at ' + SCRATCH);
  process.exit(code);
})();
