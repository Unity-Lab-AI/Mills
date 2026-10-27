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
// Run from a Work run, this process carries that person's Forgejo login (FORGEJO_* and a git extraHeader through
// GIT_CONFIG_*), and a Work run on an AI host's Claude carries the live hub's login relay (ANTHROPIC_BASE_URL /
// _AUTH_TOKEN); the throwaway hub and its fake Claude must never see either (they leaked into R122 and R112 once).
for (const k of Object.keys(process.env)) if (/^(FORGEJO_|GIT_CONFIG_(COUNT|KEY_\d+|VALUE_\d+)$|ANTHROPIC_(BASE_URL|AUTH_TOKEN|API_KEY)$)/.test(k)) delete process.env[k];

const BRIDGE = path.join(__dirname, '..');
const MILLS = path.join(BRIDGE, '..');
const KEEP = process.argv.includes('--keep');
const NO_PROD = process.argv.includes('--no-prod');
const WIN = process.platform === 'win32';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sha = (f) => (fs.existsSync(f) ? crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex') : 'missing');

const results = [];
// --stop-after R188: a development shortcut, the run ends cleanly after that check (the checks after it do not run)
const STOP_AFTER = (() => { const i = process.argv.indexOf('--stop-after'); return i >= 0 ? process.argv[i + 1] || '' : ''; })();
function check(id, label, pass, detail = '') {
  results.push({ id, label, status: pass ? 'PASS' : 'FAIL', detail });
  console.log(`${pass ? 'PASS' : 'FAIL'} ${id.padEnd(4)} ${label}${detail ? '  — ' + detail : ''}`);
  if (STOP_AFTER && id === STOP_AFTER) { const e = new Error('stopped after ' + id); e.stopAfter = true; throw e; }
}
const R489_LABEL = 'The Work session list (T-287) has what it needs: the hub names an untitled session, pins and unpins it, lists the messages waiting in it, shows it working (with the counter and the room per host) and then done with an unseen reply, renames and collapses a folder, archives the session out of the list and brings it back when opened; the page loads web/sessions.js instead of the Past sessions dialog, and its dots differ in colour and in shape for every state with the meaning on hover (a stalled run shows idle with its reason), it groups sessions into pinned, folders, today, earlier, hidden and archived (search opens what matches), counts "3 working · 1 needs you", shows "Gee\'s host 3/5", and builds the one session menu in its order with only what the hub supports';
function skip(id, label, why) {
  results.push({ id, label, status: 'SKIP', detail: why });
  console.log(`SKIP ${id.padEnd(4)} ${label}  — ${why}`);
}
// A spec check: what the hub SHOULD do (team note plans/full-rust-hub.md), written before it is built. Passing = PASS;
// failing = SPEC (not built yet: it names the gap, never blocks a release) unless --spec-strict makes it a FAIL. When
// the item is built, its spec() becomes a check().
const SPEC_STRICT = process.argv.includes('--spec-strict');
function spec(id, label, pass, detail = '') {
  if (pass) return check(id, label, true, detail);
  results.push({ id, label, status: SPEC_STRICT ? 'FAIL' : 'SPEC', detail });
  console.log(`${SPEC_STRICT ? 'FAIL' : 'SPEC'} ${id.padEnd(4)} ${label}  — not built yet${detail ? ': ' + detail : ''}`);
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
// the versioning law in the scratch repo: a merge needs the project version moved up (patch +1 here)
function bumpPatch(dir) {
  const f = path.join(dir, 'versions.json'), v = JSON.parse(fs.readFileSync(f, 'utf8'));
  v.project = v.project.replace(/\d+$/, (n) => String(Number(n) + 1)); fs.writeFileSync(f, JSON.stringify(v, null, 2) + '\n');
}
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

let RUST_BACK = 0;
let SCRATCH, SB, PORT, H, hub = null, OLLAMA_PORT = 0, ollamaSrv = null, FAKE_LINUX = ''; // FAKE_LINUX: R159's stand-in Linux accounts (ignored by git: under bridge/profiles)
// read a zip like an unzipper would (the Download packages): the central directory names every entry and points at its
// local header; deflated entries are inflated, so the files inside can be checked
function unzipAll(buf) {
  const out = []; let i = buf.indexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02]));
  while (i >= 0 && i < buf.length && buf.readUInt32LE(i) === 0x02014b50) {
    const method = buf.readUInt16LE(i + 10), csize = buf.readUInt32LE(i + 20), n = buf.readUInt16LE(i + 28), x = buf.readUInt16LE(i + 30), c = buf.readUInt16LE(i + 32), off = buf.readUInt32LE(i + 42);
    const name = buf.toString('utf8', i + 46, i + 46 + n), ln = buf.readUInt16LE(off + 26), lx = buf.readUInt16LE(off + 28), start = off + 30 + ln + lx;
    const raw = buf.subarray(start, start + csize);
    out.push({ name, data: method === 8 ? require('zlib').inflateRawSync(raw) : raw });
    i += 46 + n + x + c;
  }
  return out;
}
const fakeImagePrompts = [], fakeVoiceReqs = [], fakeEdits = [];
const fakeForgejo = { issues: new Map(), n: 0, tick: 1, calls: [], relCalls: [] };
const fakeOaiTools = []; // R346: the bodies the fake OpenAI endpoint got that carried tools or tool messages
const FAKE_ASSET = Buffer.from(Array.from({ length: 300000 }, (_, i) => (i * 7 + 3) % 251)); // T-250: a release file of 300 KB
let fakeSawPhoto = false;
let fakeOllamaNoBrain = false; // R473: the brain is gone from /api/tags (a chat model and an embedder stay); "MODEL?" answers with the model asked for
// Fake Ollama for the Unity section's local brain: scripted tool calls, no GPU, no model.
//   user text "TASK <title>"  → add_task, then a say() line     "DELETE <id>" → delete_task (needs a yes)
//   "TIME" → get_time, then answers with the tool result          "REMIND"      → set_reminder in 1 second
// The last answer echoes whether the system prompt carried the person's name and time zone.
function startFakeOllama() {
  const http = require('http');
  ollamaSrv = http.createServer((req, res) => {
    let body = ''; const chunks = []; req.on('data', (c) => { body += c; chunks.push(c); });
    req.on('end', () => {
      const j = (o, status = 200) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(o)); };
      if (req.url === '/api/version') return j({ version: 'fake' });
      // fake Forgejo repos (T-238), as the person whose token is fake-personal-token-1 (login tuser-fj): search, create (a bare
      // repo that takes pushes through git's own smart-HTTP backend), get; any other caller is refused
      if (/^\/api\/v1\/(repos\/search|user\/repos|repos\/[^/?]+\/[^/?]+$|repos\/[^/?]+\/[^/?]+\/keys(\?|$))/.test(req.url) || /^\/[^/]+\/[^/]+\.git\//.test(req.url)) {
        const auth = req.headers.authorization || '';
        const who = auth === 'token fake-personal-token-1' || (/^Basic /.test(auth) && Buffer.from(auth.slice(6), 'base64').toString() === 'tuser-fj:fake-personal-token-1') ? 'tuser-fj' : '';
        if (!who) { res.writeHead(401); return res.end('{"message":"token"}'); }
        const FR = fakeForgejo.repos || (fakeForgejo.repos = new Map()); fakeForgejo.repoCalls = fakeForgejo.repoCalls || [];
        const root = path.join(SCRATCH, 'fjrepos'), u = new URL('http://x' + req.url);
        if (u.pathname === '/api/v1/repos/search') {
          const q = (u.searchParams.get('q') || '').toLowerCase(); fakeForgejo.repoCalls.push('search ' + q);
          return j({ ok: true, data: [...FR.values()].filter((r) => !q || (r.full_name + ' ' + r.description).toLowerCase().includes(q)) });
        }
        if (req.method === 'POST' && u.pathname === '/api/v1/user/repos') {
          const b = JSON.parse(body || '{}'), full = who + '/' + b.name;
          fakeForgejo.repoCalls.push('create ' + full + (b.private ? ' private' : ' public'));
          if (FR.has(full)) return j({ message: 'The repository with the same name already exists.' }, 409);
          const dir = path.join(root, who, b.name + '.git'); fs.mkdirSync(dir, { recursive: true });
          execFileSync('git', ['init', '-q', '--bare', '-b', 'main', dir]); execFileSync('git', ['-C', dir, 'config', 'http.receivepack', 'true']);
          const r = { full_name: full, private: !!b.private, fork: false, mirror: false, description: b.description || '', clone_url: `http://127.0.0.1:${OLLAMA_PORT}/${full}.git` };
          FR.set(full, r); return j(r, 201);
        }
        // T-249: a repo's deploy keys (list, add) and its visibility (PATCH private)
        let k = /^\/api\/v1\/repos\/([^/]+)\/([^/]+)\/keys$/.exec(u.pathname);
        if (k) {
          const r = FR.get(k[1] + '/' + k[2]); if (!r) return j({ message: 'not found' }, 404);
          r.keys = r.keys || [];
          if (req.method === 'POST') { const b = JSON.parse(body || '{}'); fakeForgejo.repoCalls.push('key ' + k[1] + '/' + k[2] + (b.read_only ? ' read-only' : ' read-write')); const key = { id: r.keys.length + 1, title: b.title, key: b.key, read_only: !!b.read_only }; r.keys.push(key); return j(key, 201); }
          return j(r.keys);
        }
        let m = /^\/api\/v1\/repos\/([^/]+)\/([^/]+)$/.exec(u.pathname);
        if (m && req.method === 'PATCH') { const r = FR.get(m[1] + '/' + m[2]); if (!r) return j({ message: 'not found' }, 404); const b = JSON.parse(body || '{}'); if (typeof b.private === 'boolean') r.private = b.private; fakeForgejo.repoCalls.push('patch ' + r.full_name + ' private=' + r.private); return j(r); }
        if (m) { const r = FR.get(m[1] + '/' + m[2]); return r ? j(r) : j({ message: 'not found' }, 404); }
        // git over HTTP: git http-backend does the work
        const cgi = spawn('git', ['http-backend'], { env: { PATH: process.env.PATH, HOME: process.env.HOME || root, GIT_PROJECT_ROOT: root, GIT_HTTP_EXPORT_ALL: '1', PATH_INFO: u.pathname, REQUEST_METHOD: req.method, QUERY_STRING: u.search.slice(1), CONTENT_TYPE: req.headers['content-type'] || '', CONTENT_LENGTH: String(Buffer.concat(chunks).length), REMOTE_USER: who, REMOTE_ADDR: '127.0.0.1' } });
        const out = []; cgi.stdout.on('data', (c) => out.push(c)); cgi.stdin.on('error', () => {}); cgi.stdin.end(Buffer.concat(chunks));
        cgi.on('close', () => {
          const all = Buffer.concat(out), cut = all.indexOf('\r\n\r\n'); const head = all.subarray(0, cut).toString().split('\r\n'), hdr = {}; let status = 200;
          for (const h of head) { const k = h.indexOf(':'); if (k < 0) continue; const n = h.slice(0, k).trim(), v = h.slice(k + 1).trim(); if (n.toLowerCase() === 'status') status = parseInt(v, 10) || 200; else hdr[n] = v; }
          res.writeHead(status, hdr); res.end(all.subarray(cut + 4));
        });
        return;
      }
      // T-250: a fake Forgejo release API (a private repo: only with the hub's token) and the asset download
      if ((req.url.startsWith('/api/v1/repos/UnityAILab/') && /\/releases(\?|$)/.test(req.url)) || req.url.startsWith('/UnityAILab/fake-prod/releases/download/')) {
        fakeForgejo.relCalls.push({ url: req.url, auth: req.headers.authorization || '' });
        if (req.headers.authorization !== 'token fake-forgejo-token') { res.writeHead(404); return res.end('{}'); }
        if (req.url.startsWith('/api/v1/repos/UnityAILab/fake-prod/')) return j([
          { tag_name: 'v2.0.0', name: 'Two', draft: false, prerelease: false, published_at: '2026-10-20T10:00:00Z', body: 'Second release notes', assets: [{ name: 'fake-prod-windows-x86_64.zip', size: FAKE_ASSET.length }, { name: 'fake-prod-linux-x86_64.tar.gz', size: 5 }] },
          { tag_name: 'v1.0.0', name: 'One', draft: false, prerelease: false, published_at: '2026-10-01T10:00:00Z', body: 'First', assets: [{ name: 'fake-prod-macos-arm64.dmg', size: 3 }] },
          { tag_name: 'v0.0.1-draft', name: 'Draft', draft: true, assets: [] }]);
        if (req.url === '/UnityAILab/fake-prod/releases/download/v2.0.0/fake-prod-windows-x86_64.zip') { res.writeHead(200, { 'Content-Type': 'application/zip', 'Content-Length': FAKE_ASSET.length }); return res.end(FAKE_ASSET); }
        res.writeHead(404); return res.end('{}');
      }
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
      // fake Anthropic + ChatGPT backends behind an AI host's login relays: they only answer to the HOST's tokens (never
      // the hub's placeholder), and report usage like Anthropic does (CAPTEST in the body = 95 % used, resets in 3 s)
      if (req.url.startsWith('/anthropic/v1/messages')) {
        const ok = req.headers.authorization === 'Bearer fake-oauth-token' && /oauth-2025-04-20/.test(req.headers['anthropic-beta'] || '');
        if (!ok) { res.writeHead(401, { 'Content-Type': 'application/json' }); return res.end(JSON.stringify({ type: 'error', error: { type: 'authentication_error', message: 'bad relay auth: ' + String(req.headers.authorization || '').slice(0, 30) } })); }
        const b = JSON.parse(body || '{}'), cap = body.includes('CAPTEST');
        const rl = { 'anthropic-ratelimit-unified-5h-utilization': cap ? '0.95' : '0.42', 'anthropic-ratelimit-unified-5h-reset': String(Math.floor(Date.now() / 1000) + (cap ? 3 : 7200)), 'anthropic-ratelimit-unified-5h-status': 'allowed',
          'anthropic-ratelimit-unified-7d-utilization': '0.37', 'anthropic-ratelimit-unified-7d-reset': String(Math.floor(Date.now() / 1000) + 4 * 86400), 'anthropic-ratelimit-unified-7d-status': 'allowed' }; // the 7d window reaches the hub only through these headers (fake-claude reports 5h alone)
        const last = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' };
        const said = 'upstream ok: ' + String(typeof last.content === 'string' ? last.content : JSON.stringify(last.content)).slice(0, 60) + (Array.isArray(b.system) && /Claude Code/.test(JSON.stringify(b.system[0] || '')) ? ' [cc]' : '');
        if (b.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream', ...rl }); res.write('data: ' + JSON.stringify({ type: 'message_start', message: { model: b.model, usage: { input_tokens: 3 } } }) + '\n\n'); for (const w of said.split(' ')) res.write('data: ' + JSON.stringify({ type: 'content_block_delta', delta: { type: 'text_delta', text: w + ' ' } }) + '\n\n'); return res.end('data: ' + JSON.stringify({ type: 'message_stop' }) + '\n\n'); }
        res.writeHead(200, { 'Content-Type': 'application/json', ...rl }); return res.end(JSON.stringify({ model: b.model, content: [{ type: 'text', text: said }], usage: { input_tokens: 3, output_tokens: 4 } }));
      }
      if (req.url.startsWith('/codex/responses')) {
        if (req.headers.authorization !== 'Bearer fake-codex-token' || req.headers['chatgpt-account-id'] !== 'acct-1') { res.writeHead(401); return res.end('{"error":"bad codex auth"}'); }
        const b = JSON.parse(body || '{}'), txt = 'codex says: ' + JSON.stringify(b.input || []).match(/"text":"([^"]*)"/)[1];
        res.writeHead(200, { 'Content-Type': 'text/event-stream' });
        for (const w of txt.split(' ')) res.write('data: ' + JSON.stringify({ type: 'response.output_text.delta', delta: w + ' ' }) + '\n\n');
        return res.end('data: ' + JSON.stringify({ type: 'response.completed', response: { model: b.model, usage: { input_tokens: 2, output_tokens: 3 } } }) + '\n\n');
      }
      // fake OpenAI-compatible endpoint (a person's own "openai" provider): echoes the last user line, streams on request
      if (req.url === '/oai/chat/completions' || req.url === '/v1/chat/completions') { // /v1: Ollama's own OpenAI-compatible path (R182)
        const b = JSON.parse(body || '{}'), last = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' }, sys = (b.messages || []).find((m) => m.role === 'system');
        const text = 'oai says: ' + last.content + (sys ? ' [sys ' + sys.content.length + ']' : '');
        if (Array.isArray(b.tools) || (b.messages || []).some((m) => m.role === 'tool')) { // R346 (T-317): tools in, a tool call out; a tool result in, text out
          fakeOaiTools.push(b);
          const lm = (b.messages || [])[(b.messages || []).length - 1] || {};
          const usage = { prompt_tokens: 5, completion_tokens: 6, total_tokens: 11 };
          if (lm.role === 'tool') return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: 'oai says: tool said ' + lm.content }, finish_reason: 'stop' }], usage });
          return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: null, tool_calls: [{ id: 'call_r346', type: 'function', function: { name: b.tools[0].function.name, arguments: '{"q":"r346","n":2}' } }] }, finish_reason: 'tool_calls' }], usage });
        }
        if (b.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); for (const w of text.split(' ')) res.write('data: ' + JSON.stringify({ model: b.model, choices: [{ delta: { content: w + ' ' } }] }) + '\n\n'); return res.end('data: [DONE]\n\n'); }
        const cached = /r185/.test(String(last.content)) ? { prompt_tokens: 2000, completion_tokens: 40, total_tokens: 2040, prompt_tokens_details: { cached_tokens: 1500 } } : null; // R185: a cache hit the hub must count
        return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: text } }], usage: cached || { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } });
      }
      // fake Forgejo (issue mirroring): in-memory issues under the repo path; /fake/forgejo/* lets the runner act as a person on Forgejo
      const FJ = '/api/v1/repos/UnityAILab/Mills/issues';
      const FJ_LOGINS = { 'token fake-forgejo-token': 'mills-hub', 'token fake-personal-token-1': 'tuser-fj' }; // hub token + one person's
      if (req.url === '/api/v1/user') { const l = FJ_LOGINS[req.headers.authorization]; if (!l) { res.writeHead(401); return res.end('{"message":"token"}'); } return j({ id: 7, login: l }); }
      if (req.url.startsWith(FJ) || req.url.startsWith('/fake/forgejo/')) {
        fakeForgejo.calls.push(req.method + ' ' + req.url);
        const b = body ? JSON.parse(body) : {};
        const who = FJ_LOGINS[req.headers.authorization] || 'someone';
        const stamp = () => new Date(Date.now() + (fakeForgejo.tick++)).toISOString();
        const mk = (o) => { const n = ++fakeForgejo.n; const is = { number: n, title: o.title || '', body: o.body || '', state: o.state || 'open', labels: (o.labels || []).map((l) => ({ name: l })), html_url: 'https://git.example.invalid/UnityAILab/Mills/issues/' + n, updated_at: stamp(), created_at: stamp(), user: { login: who } }; fakeForgejo.issues.set(n, is); return is; };
        if (!FJ_LOGINS[req.headers.authorization] && !req.url.startsWith('/fake/')) { res.writeHead(401); return res.end('{"message":"token"}'); }
        if (req.method === 'PATCH' && !req.url.startsWith('/fake/')) fakeForgejo.patchedBy = who;
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
      if (req.url === '/api/tags') return j({ models: fakeOllamaNoBrain ? [{ name: 'nomic-embed-text:latest' }, { name: 'plain-chat:latest' }] : [{ name: 'unity-brain:latest' }] });
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
        else if (/MODEL\?/.test(u)) line({ content: 'model is ' + b.model });
        else line({ content: 'plain answer for ' + u.slice(-3000) });
      } else if (last.role === 'tool' && last.tool_name !== 'say') {
        call('say', { text: 'done: ' + last.content.slice(0, 160), emotion: 'happy', act: 'statement' });
      } else {
        line({ content: `final sys-has-name=${/(VibeRoller|Mills) username "t-(user|admin)"/.test(sys)} tz=${(/\(([A-Za-z_]+\/[A-Za-z_]+)\)/.exec(sys) || [])[1] || ''}` });
      }
      res.end(JSON.stringify({ done: true, prompt_eval_count: 100, eval_count: 10 }) + '\n'); // Ollama's counts on the final chunk (R186)
    });
  });
  return new Promise((r) => ollamaSrv.listen(0, '127.0.0.1', () => { OLLAMA_PORT = ollamaSrv.address().port; r(); }));
}
async function setup() {
  SCRATCH = fs.mkdtempSync(path.join(os.tmpdir(), 'mills-test-'));
  SB = path.join(SCRATCH, 'bridge');
  FAKE_LINUX = path.join(SB, 'profiles', '_linux');
  fs.mkdirSync(path.join(SB, 'test'), { recursive: true });
  for (const f of fs.readdirSync(BRIDGE)) {
    if ((f.endsWith('.js') && f !== 'master-logins.js') || f === 'cloudflared-quick.yml') fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  }
  fs.copyFileSync(path.join(__dirname, 'fake-claude.js'), path.join(SB, 'test', 'fake-claude.js'));
  fs.copyFileSync(path.join(__dirname, 'fake-jcode.js'), path.join(SB, 'test', 'fake-jcode.js')); // R180: a stand-in `jcodepgm run --ndjson`
  // the JavaScript AI host's launchers, example config and host tools
  for (const f of ['ai-node.example.json', 'start-node.bat', 'start-node.sh']) fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  fs.cpSync(path.join(BRIDGE, 'host-tools'), path.join(SB, 'host-tools'), { recursive: true, filter: (s) => !/\.(exe|ico)$/i.test(s) });
  fs.cpSync(path.join(MILLS, 'web'), path.join(SCRATCH, 'web'), { recursive: true });
  for (const f of ['versions.json', 'CHANGELOG.md']) if (fs.existsSync(path.join(MILLS, f))) fs.copyFileSync(path.join(MILLS, f), path.join(SCRATCH, f)); // semver (law 14)
  PORT = await freePort(7799);
  if (RUST_HUB) RUST_BACK = await freePort(PORT + 20); // the JavaScript hub behind the Rust one
  H = `http://127.0.0.1:${PORT}`;
  const real = readJson(path.join(BRIDGE, 'config.json'), {});
  // The scratch copy is its own git repo (main + develop) so per-account worktrees work there.
  const g = (...a) => execFileSync('git', a, { cwd: SCRATCH, stdio: 'ignore', windowsHide: true });
  fs.writeFileSync(path.join(SCRATCH, '.gitignore'), ['bridge/profiles/', 'bridge/profiles-deleted/', 'bridge/*.json', 'bridge/.restart', 'bridge/.fj/', 'bin/', 'Chat/', 'Archive/', 'Hyperwork/', 'worktrees/', 'projects/', 'upstream.git/', 'host-pc/', 'host-claude-login/', 'host-codex/', 'Api/', 'bridge/host2/', 'bridge-handoff/', 'rust-host/', 'native/', 'handoff-run.log', 'hub.log', 'ai-node.log', 'host2.log', 'bridge/ai-node.lock', 'bridge/hub-signing.key', ''].join('\n'));
  g('init', '-q', '-b', 'main'); g('config', 'user.email', 'regress@example.invalid'); g('config', 'user.name', 'regress');
  g('add', '-A'); g('commit', '-q', '-m', 'scratch'); g('branch', 'develop');
  await startFakeOllama();
  reTestSetup();
  const cfg = { ...real, assistant: { ...(real.assistant || {}), ollamaUrl: `http://127.0.0.1:${OLLAMA_PORT}`, imageUrl: `http://127.0.0.1:${OLLAMA_PORT}`, voiceUrl: `http://127.0.0.1:${OLLAMA_PORT}`, speechUrl: `http://127.0.0.1:${OLLAMA_PORT}`, searchUrl: `http://127.0.0.1:${OLLAMA_PORT}/search`, youtubeUrl: `http://127.0.0.1:${OLLAMA_PORT}/results`, via: 'node' }, releases: [{ product: 'fake-prod', repo: 'UnityAILab/fake-prod', label: 'Fake Product' }, { product: 'fake-gone', repo: 'UnityAILab/fake-gone', label: 'Gone' }], aiNodes: { 'test-node': NODE_SECRET, 'old-node': OLD_NODE_SECRET, 'upd-node': OLD_NODE_SECRET + '-upd', 'nohello-node': OLD_NODE_SECRET + '-nh', 'group-node': OLD_NODE_SECRET + '-grp', 'rust-node': OLD_NODE_SECRET + '-rs', 'ollama-node': OLD_NODE_SECRET + '-ol', 'jcode-node': OLD_NODE_SECRET + '-jc', 'plain-node': OLD_NODE_SECRET + '-pl', 'nobrain-node': OLD_NODE_SECRET + '-nb' }, hostGroup: ['t-user'], workSecret: WORK_SECRET, ...(RE_T.ok ? { re: { image: RE_T.image, maxConcurrent: 3, idleWarnHours: 4, runtime: 'podman', namePrefix: RE_T.prefix, env: RE_T.env.env || {}, engineArgs: RE_T.env.pre || [], cpus: '1', memory: '512m', mcpCommand: ['sh', '/opt/fake-rea/mcp.sh'], skillDir: '/opt/fake-rea/skill' } } : {}), hyperworkTeams: { core: ['t-admin', 't-user'] }, hyperworkSecrets: { R444_TOKEN: 'r444-sekrit-token-77' }, hyperworkNetwork: ['127.0.0.1'], hyperworkEventSecret: 'r472-event-secret-0123456789', rustHub: RUST_HUB ? { enabled: true, backPort: RUST_BACK, alone: RUST_ALONE } : false /* on by default since 3.22.0: the normal run is the JavaScript hub alone */, linuxPasswdFile: path.join(FAKE_LINUX, 'passwd'), aiNodeOfflineMs: 4000, presence: { workGapMs: 1500 }, port: PORT, tunnel: false, gistId: '', worktreeRoot: path.join(SCRATCH, 'worktrees'), projectsRoot: path.join(SCRATCH, 'projects'), projectsLocalClone: true, claudeCommand: process.execPath, claudeArgs: [path.join(SB, 'test', 'fake-claude.js')] };
  fs.writeFileSync(path.join(SB, 'config.json'), JSON.stringify(cfg, null, 2));
  // a stand-in for the server's Linux accounts (R159): t-user has one and linked it (its ~/uploaded_files exists and the
  // hub may write it); t-admin has none
  fs.mkdirSync(path.join(FAKE_LINUX, 'home', 't-user', 'uploaded_files'), { recursive: true });
  fs.writeFileSync(path.join(FAKE_LINUX, 'passwd'), `root:x:0:0:root:/root:/bin/bash\nt-user:x:1500:1500::${path.join(FAKE_LINUX, 'home', 't-user')}:/bin/bash\n`);
  // the scratch hub mirrors its "team" board to the fake Forgejo (a 3-second poll: the minimum)
  // host-set owners (bridge/server-owners.json, R134): a server that exists before the hub starts gets a new owner +
  // a co-owner; the Mills server gets a named owner (t-admin, a hub admin already, so nothing else changes)
  fs.mkdirSync(path.join(SCRATCH, 'Chat'), { recursive: true });
  fs.writeFileSync(path.join(SCRATCH, 'Chat', 'guilds.json'), JSON.stringify({ servers: [{ id: 'seedtest', name: 'Seed Test', members: ['t-admin', 't-user'], owner: 't-user', createdBy: 't-user', channels: [{ id: 'general', name: 'general' }] }], groups: [] }));
  fs.writeFileSync(path.join(SB, 'server-owners.json'), JSON.stringify({ servers: { seedtest: { owner: 't-admin', coOwners: ['t-user', 'nobody-here'] }, mills: { owner: 't-admin', coOwners: [] } } }));
  fs.writeFileSync(path.join(SB, 'release-key.json'), JSON.stringify(TEST_RELEASE_KEY)); // R164: the scratch hub signs native releases with the test key
  fs.writeFileSync(path.join(SB, 'forgejo.json'), JSON.stringify({ url: `http://127.0.0.1:${OLLAMA_PORT}`, token: 'fake-forgejo-token', repo: 'UnityAILab/Mills', board: 'team', pollMinutes: 0.01 }));
  const sp = (args) => spawnSync(process.execPath, [path.join(SB, 'set-password.js'), ...args], { cwd: SB, encoding: 'utf8' });
  sp(['pw-default-0001']);
  for (const [name, conf] of [['t-admin', { user: 'Tadmin', admin: true }], ['t-user', { user: 'Tuser' }]]) {
    fs.mkdirSync(path.join(SB, 'profiles', name), { recursive: true });
    fs.writeFileSync(path.join(SB, 'profiles', name, 'config.json'), JSON.stringify(conf));
    sp(['--profile', name, `pw-${name}-0001`]);
  }
  // R172: a stand-in `fj` first on the PATH of the scratch hub and the test host, so a run's fj wrapper (the person's
  // own keys file) is made and can be checked (the real fj is never called)
  if (process.platform !== 'win32') { const bin = path.join(SCRATCH, 'bin'); fs.mkdirSync(bin, { recursive: true }); fs.writeFileSync(path.join(bin, 'fj'), '#!/bin/sh\necho "fake fj $*"\n', { mode: 0o755 }); process.env.PATH = bin + path.delimiter + (process.env.PATH || ''); }
  process.env.VR_TEST_HOOKS = '1'; // the Rust hub's test hooks (/__vr-test/…), for the runner's throwaway hub only (R174)
}
// The throwaway hub runs Unity's engines THROUGH an AI node (bridge/ai-node.js, its own process), the way the hub on
// the server will: every Unity check below then also tests the node path end to end (R137).
const NODE_SECRET = 'test-node-secret-0123456789abcdef';
const WORK_SECRET = 'work-secret-r323-0123456789abcdef0123'; // R323 (T-308): fixes the Rust hub's DEPRECATED shared MCP secret (config workSecret) so the test can send an old-style call
const OLD_NODE_SECRET = 'old-node-secret-0123456789abcdef'; // R152: a 2.x host (no login relay), played by the test itself
let aiNodeProc = null;
function startAiNode() {
  if (aiNodeProc) return;
  const f = path.join(SB, 'ai-node.json'), base = `http://127.0.0.1:${OLLAMA_PORT}`;
  // the test host offers Unity's engines, a "Claude" (the fake one) for t-user and t-admin's Work, a local model (the fake
  // OpenAI endpoint) and direct computer access to a folder in the scratch area (like the native app would)
  // the host's own logins (fake tokens), which must never leave it: the fake backends answer only to these
  const cl = path.join(SCRATCH, 'host-claude-login'), cx = path.join(SCRATCH, 'host-codex');
  fs.mkdirSync(cl, { recursive: true }); fs.mkdirSync(cx, { recursive: true });
  fs.writeFileSync(path.join(cl, '.credentials.json'), JSON.stringify({ claudeAiOauth: { accessToken: 'fake-oauth-token', refreshToken: 'x', expiresAt: Date.now() + 86400e3 } }));
  fs.writeFileSync(path.join(cx, 'auth.json'), JSON.stringify({ tokens: { access_token: 'fake-codex-token', account_id: 'acct-1' } }));
  fs.writeFileSync(f, JSON.stringify({ hub: H, id: 'test-node', token: NODE_SECRET, user: 't-admin', engines: { ollama: base, image: base, voice: base, speech: base },
    providers: [{ id: 'claude', kind: 'claude-relay', label: 'Tadmin\'s Claude', models: ['claude-fable-5-1', 'claude-haiku-5-5'], max5h: 90, max7d: 90, allowedUsers: ['t-user'], configDir: cl, baseUrl: base + '/anthropic' },
      { id: 'codex', kind: 'codex-relay', label: 'Tadmin\'s Codex', models: ['gpt-test'], allowedUsers: ['t-user'], codexHome: cx, baseUrl: base + '/codex' },
      { id: 'local', kind: 'openai', label: 'Host local model', baseUrl: base + '/oai', models: ['tiny-local'], allowedUsers: ['t-user'] }] }));
  const log = fs.openSync(path.join(SCRATCH, 'ai-node.log'), 'a');
  // --rust-host: the native host (native/crates/vr-host) plays the test host, so every host check runs against it
  aiNodeProc = RUST_HOST ? spawn(RUST_HOST, ['host', '--config', f], { cwd: SB, stdio: ['ignore', log, log], windowsHide: true })
    : spawn(process.execPath, [path.join(SB, 'ai-node.js'), '--config', f], { cwd: SB, stdio: ['ignore', log, log], windowsHide: true });
}
// the native programs, built for this machine when a Rust toolchain is here (native/build.sh): R163 / R164, --rust-host
// built into their own target folder with a test-only release key (the scratch hub signs with it), so they never mix
// with the real builds of native/build.sh
const TEST_RELEASE_KEY = { secret: '4570e54a425b32604b04d14d172976b602af918eb6322d14e929867896b61b50', public: '241fc450dffa7aaf2155ed06404cd10d824f590c7c0c00a70b4ec6fa8e9b5a20' };
// Clean build law (LAWS.md): the suite's native build goes into a temporary folder of its own, deleted when the run ends
// (and a folder an earlier run left behind, after a crash, is swept once it is a day old)
const NATIVE_TARGET = (() => {
  const base = os.tmpdir();
  try { for (const d of fs.readdirSync(base)) if (/^vr-regress-/.test(d) && Date.now() - fs.statSync(path.join(base, d)).mtimeMs > 86400e3) fs.rmSync(path.join(base, d), { recursive: true, force: true }); } catch {}
  const dir = fs.mkdtempSync(path.join(base, 'vr-regress-'));
  process.on('exit', () => { try { fs.rmSync(dir, { recursive: true, force: true }); } catch {} });
  for (const sig of ['SIGINT', 'SIGTERM']) process.once(sig, () => process.exit(130));
  return dir;
})();
const NATIVE_BIN = (() => {
  const exe = (n) => path.join(NATIVE_TARGET, 'release', n + (WIN ? '.exe' : ''));
  return { host: exe('vr-host'), interop: path.join(NATIVE_TARGET, 'release', 'examples', 'interop' + (WIN ? '.exe' : '')) };
})();
let nativeBuilt = null;
function buildNative() { // → '' when built, else why not (once per run)
  if (nativeBuilt !== null) return nativeBuilt;
  const home = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
  const env = { ...process.env, RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(home, '.rustup'), CARGO_HOME: process.env.CARGO_HOME || path.join(home, '.cargo'), CARGO_TARGET_DIR: NATIVE_TARGET, VR_RELEASE_PUBKEY: TEST_RELEASE_KEY.public, VR_VERSION: (() => { try { return JSON.parse(fs.readFileSync(path.join(MILLS, 'versions.json'), 'utf8')).project; } catch { return ''; } })() };
  env.PATH = path.join(env.CARGO_HOME, 'bin') + path.delimiter + (env.PATH || '');
  const r = spawnSync('cargo', ['build', '--release', '-q', '-p', 'vr-host', '-p', 'vr-core', '-p', 'vr-hub', '--examples', '--bins'], { cwd: path.join(MILLS, 'native'), env, encoding: 'utf8', timeout: 1200000 });
  return (nativeBuilt = buildResult(r));
}
function buildResult(r) {
  if (r.error) return 'no Rust toolchain here (' + r.error.code + ')';
  return r.status === 0 && fs.existsSync(NATIVE_BIN.host) ? '' : 'cargo build failed: ' + String(r.stderr || '').trim().split('\n').slice(-3).join(' ');
}
// the desktop app (Tauri) for this machine: built with native/tauri-env.sh's sysroot (Linux), into the same test target
function buildDesktop() { // → { bin } or { why }
  if (WIN) return { why: 'the desktop smoke check runs on Linux (Xvfb)' };
  const home = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
  const sh = `export RUSTUP_HOME="${home}/.rustup" CARGO_HOME="${home}/.cargo" CARGO_TARGET_DIR="${NATIVE_TARGET}" VR_RELEASE_PUBKEY=${TEST_RELEASE_KEY.public} OWNER_HOME="${home}"; export PATH="$CARGO_HOME/bin:$PATH"; . ./tauri-env.sh; [ "$HAVE_TAURI_LINUX" = 1 ] || { echo NO-SYSROOT; exit 3; }; cargo build --release -q -p vr-desktop`;
  const r = spawnSync('sh', ['-c', sh], { cwd: path.join(MILLS, 'native'), encoding: 'utf8', timeout: 1800000 });
  if (r.status === 3) return { why: 'no WebKitGTK sysroot here (sh native/tauri-env.sh setup)' };
  const bin = path.join(NATIVE_TARGET, 'release', 'vr-desktop');
  return r.status === 0 && fs.existsSync(bin) ? { bin } : { why: 'cargo build failed: ' + String(r.stderr || r.stdout || '').trim().split('\n').slice(-3).join(' ') };
}
// --rust-hub: the Rust hub (native/crates/vr-hub) in front of the scratch hub, as config "rustHub" runs it on the server:
// every check below then goes through it (the modules it has, it answers; the rest it passes to the JavaScript hub)
// --rust-alone (FR6): the Rust hub ALONE, no JavaScript hub started at all (config rustHub.alone): every check through it
const RUST_ALONE = process.argv.includes('--rust-alone');
const RUST_HUB = (() => {
  if (!process.argv.includes('--rust-hub') && !RUST_ALONE) return '';
  const why = buildNative();
  if (why) { console.error('--rust-hub: ' + why); process.exit(2); }
  const bin = path.join(NATIVE_TARGET, 'release', 'vr-hub' + (WIN ? '.exe' : ''));
  process.env.VR_HUB_BIN = bin;
  return bin;
})();
const RUST_HOST = (() => {
  if (!process.argv.includes('--rust-host')) return '';
  const why = buildNative();
  if (why) { console.error('--rust-host: ' + why); process.exit(2); }
  return NATIVE_BIN.host;
})();
// T-303: RE containers (R316 to R320). The checks need the real Podman and a tiny local image: reTestSetup builds one from a
// 2-line Containerfile (a non-root user, so the exchange folder's sub-uid files are exercised) from a Debian image that is
// already in local storage (nothing is pulled), or says why not (the checks then skip). reTestCleanup removes every
// container the hub made under the test prefix and the image.
const RE_T = { ok: false, why: 'the Rust hub\'s: --rust-hub runs it', env: {}, image: 'localhost/pgm-re-test:p' + process.pid, prefix: 'pgm-re-test-' + process.pid + '-' }; // per suite run: two suites on one engine (shared storage) must not take each other's image or containers
// eng = { env: extra environment, pre: global engine arguments } (the hub's re.env / re.engineArgs)
function podman(args, eng) { const e = eng || {}; return spawnSync('podman', [...(e.pre || []), ...args], { encoding: 'utf8', env: { ...process.env, ...(e.env || {}) }, timeout: 240000, windowsHide: true }); }
function reTestSetup() {
  if (!RUST_HUB) return;
  if (WIN) { RE_T.why = 'the engine here is Docker on Windows: not run by this check (T-303 leaves the Windows path)'; return; }
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 're-test-image-')); // outside the scratch copy: its main must stay clean for R109
  RE_T.dir = dir;
  // T-254: a fake MCP stdio server and a small skill folder, where the hub's re.mcpCommand / re.skillDir point (see R341 to R345)
  fs.mkdirSync(path.join(dir, 'fake-rea', 'skill', 'references'), { recursive: true });
  fs.copyFileSync(path.join(MILLS, 'native', 'crates', 'pgm-sandbox-internal', 'tests', 'fake-mcp.sh'), path.join(dir, 'fake-rea', 'mcp.sh'));
  fs.writeFileSync(path.join(dir, 'fake-rea', 'skill', 'SKILL.md'), '# Fake REA skill\n\nUse the tools; read references/environments.md for the environments (héllo).\n');
  fs.writeFileSync(path.join(dir, 'fake-rea', 'skill', 'references', 'environments.md'), '# Environments\n\nA reference file.\n');
  let why = 'podman is not available';
  // the engine settings to try, in order: this account's own storage; its real HOME (the shared HOME's overlay folders have ACLs the
  // engine cannot use); the shared rootless storage on the server (/var/tmp/gee-podman, where the base image lives)
  const engines = [{ env: {}, pre: [] }, { env: { HOME: os.userInfo().homedir }, pre: [] }, { env: {}, pre: ['--root', '/var/tmp/gee-podman/root', '--runroot', '/var/tmp/gee-podman/run'] }];
  for (const eng of engines) {
    if (podman(['version'], eng).status !== 0) { why = 'podman is not available'; break; }
    for (const from of ['docker.io/library/debian:trixie-slim', 'localhost/pgm-re-base:1.0.1']) {
      fs.writeFileSync(path.join(dir, 'Containerfile'), `FROM ${from}\nUSER root\nRUN id -u re >/dev/null 2>&1 || useradd -m -u 1000 re\nCOPY fake-rea /opt/fake-rea\nRUN chmod -R a+rX /opt/fake-rea\nUSER re\n`);
      const b = podman(['build', '--pull=never', '-q', '-t', RE_T.image, '-f', path.join(dir, 'Containerfile'), dir], eng);
      if (b.status !== 0) { why = 'no local debian:trixie-slim image to build the test image from (' + String(b.stderr || '').trim().split('\n').pop().slice(0, 100) + ')'; continue; }
      const r = podman(['run', '--rm', '--pull=never', '--network', 'none', '--cap-drop', 'ALL', RE_T.image, 'id', '-u'], eng);
      if (r.status !== 0 || r.stdout.trim() !== '1000') { why = 'podman cannot run containers here (' + String(r.stderr || '').trim().split('\n').pop().slice(0, 100) + ')'; continue; }
      RE_T.ok = true; RE_T.env = eng; RE_T.why = '';
      return;
    }
  }
  RE_T.why = why;
}
function reTestCleanup() {
  if (!RE_T.ok) return;
  const left = (podman(['ps', '-a', '--filter', 'name=' + RE_T.prefix, '--format', '{{.Names}}'], RE_T.env).stdout || '').split('\n').filter(Boolean);
  if (left.length) podman(['rm', '-f', '-t', '0', ...left], RE_T.env);
  podman(['rmi', '-f', RE_T.image], RE_T.env);
  try { fs.rmSync(RE_T.dir, { recursive: true, force: true }); } catch {}
}
async function startHub() {
  const log = fs.openSync(path.join(SCRATCH, 'hub.log'), 'a');
  hub = spawn(process.execPath, [path.join(SB, 'run.js'), '--no-tunnel'], { cwd: SB, stdio: ['ignore', log, log], windowsHide: true });
  for (let i = 0; i < 40; i++) { if ((await call('/api/status')).status === 200) { startAiNode(); return true; } await sleep(250); }
  return false;
}
function stopAiNode() {
  if (!aiNodeProc) return;
  try { if (WIN) execFileSync('taskkill', ['/pid', String(aiNodeProc.pid), '/T', '/F'], { stdio: 'ignore' }); else aiNodeProc.kill('SIGTERM'); } catch {}
  aiNodeProc = null;
  try { fs.unlinkSync(path.join(SB, 'ai-node.lock')); } catch {} // a killed node leaves its lock behind; a real one removes it on exit
}
function stopHub() {
  stopAiNode();
  if (!hub) return;
  try { if (WIN) execFileSync('taskkill', ['/pid', String(hub.pid), '/T', '/F'], { stdio: 'ignore' }); else hub.kill('SIGTERM'); } catch {}
  hub = null;
}

// ---------- http helpers ----------
async function call(p, token, body, extraHeaders = {}, retry = true) {
  try {
    const r = await fetch(H + p, {
      method: body !== undefined ? 'POST' : 'GET',
      headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: 'Bearer ' + token } : {}), ...extraHeaders },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    return { status: r.status, data: await r.json().catch(() => ({})) };
  } catch (e) {
    // a GET on a pooled connection the hub just closed as idle (its keep-alive timeout) fails with "fetch failed": ask once more
    if (retry && body === undefined) { await sleep(150); return call(p, token, body, extraHeaders, false); }
    return { status: 0, data: { error: e.message } };
  }
}
const login = (username, password, deviceId) => call('/api/login', null, { username, password, deviceId });
// Run a chat to completion; answers pc_tool events like a browser would.
async function chat(token, message, opts = {}) {
  const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }, body: JSON.stringify({ message, fresh: !!opts.fresh, ...(opts.context ? { context: opts.context } : {}), ...(opts.session ? { session: opts.session } : {}), ...(opts.startedBy ? { _testStartedBy: opts.startedBy } : {}) }) });
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
  const scripts = fs.readdirSync(BRIDGE).filter((f) => f.endsWith('.js')).map((f) => path.join(BRIDGE, f)).concat(fs.readdirSync(__dirname).filter((f) => f.endsWith('.js')).map((f) => path.join(__dirname, f)));
  const bad = scripts.filter((f) => spawnSync(process.execPath, ['--check', f]).status !== 0);
  let pageOk = true, pageErr = '';
  // `html` = the page plus the page-side scripts it loads (web/calls.js), so the static checks below keep
  // covering code that moved out of index.html into its own file.
  const indexHtml = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
  const EDITOR_FILES = ['editor/core.js', 'editor/view.js', 'editor/kinds-workflow.js', 'editor/kinds-boxes.js', 'editor/workflows-ui.js']; // the visual editor foundation (T-205), loaded after the page script
  const PAGE_SCRIPTS = ['sessions.js', 'localai.js', 'hyperwork.js', 'attach.js', 'library.js', 'calls.js',  'coview.js', 'presence.js', 'machines.js', 'guilds.js', 'guild-settings.js', 'profile.js', 'notes.js', 'app.js'];
  const html = indexHtml + '\n' + PAGE_SCRIPTS.map((f) => fs.readFileSync(path.join(MILLS, 'web', f), 'utf8')).join('\n');
  const inlineStart = indexHtml.indexOf('<script>') + 8, inlineEnd = indexHtml.indexOf('</script>', inlineStart);
  try { new Function(indexHtml.slice(inlineStart, inlineEnd)); } catch (e) { pageOk = false; pageErr = 'page: ' + e.message; }
  for (const f of ['md.js', 'board.js', 'sw.js', ...PAGE_SCRIPTS, ...EDITOR_FILES]) if (spawnSync(process.execPath, ['--check', path.join(MILLS, 'web', f)]).status !== 0) bad.push(f);
  for (const f of PAGE_SCRIPTS) if (!indexHtml.includes(`<script src="${f}"></script>`)) { pageOk = false; pageErr += ' ' + f + ' not loaded'; }
  { // the editor foundation (T-205): its scripts load after the page script, in this order, the service worker caches them, the menu and the screen exist
    const afterPage = indexHtml.indexOf('</script>', indexHtml.indexOf('<script>')), sw1 = fs.readFileSync(path.join(MILLS, 'web', 'sw.js'), 'utf8'); let last = afterPage;
    for (const f of EDITOR_FILES) { const at = indexHtml.indexOf(`<script src="${f}"></script>`); if (at < last) { pageOk = false; pageErr += ' ' + f + ' not loaded after the page script (in order)'; } else last = at; if (!sw1.includes(`'${f}'`)) { pageOk = false; pageErr += ' ' + f + ' not in sw.js'; } }
    if (!/id="goWorkflows"/.test(indexHtml) || !/<div id="workflows" class="screen" hidden>/.test(indexHtml) || !/showScreen\('workflows'\)/.test(indexHtml) || !/cmd: '\/workflows'/.test(indexHtml)) { pageOk = false; pageErr += ' the Workflows screen, its menu tile or its command is missing'; }
  }
  check('R1', 'every bridge script passes node --check; page script (and web/md.js) parses; the editor foundation scripts (web/editor/*.js) parse, load after the page script and are cached by sw.js, and the Workflows screen has its menu tile and command', !bad.length && pageOk, [bad.map((f) => path.basename(f)).join(', '), pageErr].filter(Boolean).join(' '));

  const st = await call('/api/status');
  const log = fs.readFileSync(path.join(SCRATCH, 'hub.log'), 'utf8');
  check('R2', 'hub starts, /api/status ok, startup log lists every account', st.status === 200 && ['mills', 't-admin', 't-user'].every((u) => log.includes(u)));

  const A = (await login('T-Admin', 'pw-t-admin-0001', devA)).data.token;
  const U = (await login('t-user', 'pw-t-user-0001', devB)).data.token;
  const D = (await login('mills', 'pw-default-0001', devC)).data.token;
  if (process.argv.includes('--only-r323') && RUST_HUB) await r323Check(); // development: just R323 (with --stop-after R323), a minute instead of an hour
  if (process.argv.includes('--only-r328') && RUST_HUB) await machineToolsCheck(); // development: just R328 to R330 (with --stop-after R330)
  if (process.argv.includes('--only-r488') && RUST_HUB) await r488Check(); // development: just R488 (with --stop-after R488)
  if (process.argv.includes('--only-r337') && RUST_HUB) await r337Check(); // development: just R337 (with --stop-after R337)
  if (process.argv.includes('--only-r489') && RUST_HUB) await r489Check(); // development: just R489 (with --stop-after R489)
  check('R3', 'correct username + password → token (case-insensitive username)', !!A && !!U && !!D);

  const w1 = await login('t-user', 'wrong-password-1', devB), w2 = await login('ghost', 'wrong-password-1', devB);
  check('R4', 'wrong password and unknown user → 401 with the same message', w1.status === 401 && w2.status === 401 && w1.data.error === w2.data.error);

  for (let i = 0; i < 5; i++) await login('nobody-x', 'wrong-password', devB);
  const blocked = await login('nobody-x', 'wrong-password', devB), other = await login('t-user', 'pw-t-user-0001', devB);
  check('R5', '5 failures → 429 for that username only', blocked.status === 429 && other.status === 200);

  // R153: a stranger cannot lock a teammate out; floods from one address stop; big pre-sign-in bodies are refused
  {
    const from = (ip, body, raw) => fetch(H + '/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json', 'CF-Connecting-IP': ip }, body: raw || JSON.stringify(body) }).then(async (r) => ({ status: r.status, data: await r.json().catch(() => ({})) }));
    for (let i = 0; i < 6; i++) await from('203.0.113.9', { username: 't-user', password: 'guess-' + i, deviceId: devB });
    const strangerBlocked = await from('203.0.113.9', { username: 't-user', password: 'pw-t-user-0001', deviceId: devB });
    const ownerFine = await login('t-user', 'pw-t-user-0001', devB); // t-user from their own address: not affected
    for (let i = 0; i < 20; i++) await from('203.0.113.10', { username: 'made-up-' + i, password: 'x', deviceId: devB });
    const sprayBlocked = await from('203.0.113.10', { username: 'another-made-up', password: 'x', deviceId: devB });
    const bigBody = await from('203.0.113.11', null, JSON.stringify({ username: 't-user', password: 'x'.repeat(40000) }));
    const srv153 = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8'), ngx = fs.readFileSync(path.join(MILLS, 'deploy', 'nginx-portal.conf'), 'utf8');
    check('R153', 'sign-in limits are per address: 5 wrong guesses at a teammate\'s password from a stranger\'s address block only that address (the teammate still signs in); 20 failures from one address (any usernames) block it; a sign-in body over 16 KB → 413; counters are never wiped all at once; system errors are answered without server paths; [static] nginx adds per-address rate + connection limits',
      strangerBlocked.status === 429 && ownerFine.status === 200 && sprayBlocked.status === 429 && bigBody.status === 413
      && !/failures\.clear\(\)/.test(srv153) && /'<path>'/.test(srv153) && /limit_req zone=vr_portal_login/.test(ngx) && /limit_conn vr_portal_conn/.test(ngx),
      `stranger ${strangerBlocked.status} owner ${ownerFine.status} spray ${sprayBlocked.status} big ${bigBody.status}`);
  }

  const locked = await login('t-user', 'pw-t-user-0001', devA);
  check('R6', 'right password from a different device → 403 locked', locked.status === 403 && /locked to its own devices/.test(locked.data.error || ''));
  { // R168: an account may have several devices (the browser and the app on one PC are two): an admin allows one more,
    // the next new device joins, the old one keeps working, a further new one is refused again (4.1.1)
    const notAdmin = await call('/api/admin/allow-device', (await login('t-user', 'pw-t-user-0001', devB)).data.token, { username: 't-user' });
    const allowed = await call('/api/admin/allow-device', A, { username: 't-user' });
    const listed = ((await call('/api/admin/accounts', A)).data.accounts || []).find((x) => x.username === 't-user') || {};
    const joined = await login('t-user', 'pw-t-user-0001', devA);
    const oldOne = await login('t-user', 'pw-t-user-0001', devB);
    const devD = 'device-dddddddddddddddd', third = await login('t-user', 'pw-t-user-0001', devD);
    const after = ((await call('/api/admin/accounts', A)).data.accounts || []).find((x) => x.username === 't-user') || {};
    for (const t of [joined, oldOne]) if (t.data && t.data.token) await call('/api/logout', t.data.token, {});
    check('R168', 'one account, several devices: an admin\'s "allow one more device" (admins only) lets the next new device join (the account list says 1 device, +1 allowed, then 2), the first device keeps working, and a further new device is refused again until the admin allows it',
      notAdmin.status === 403 && allowed.status === 200 && listed.devices === 1 && listed.allowNext === true && joined.status === 200 && oldOne.status === 200 && third.status === 403 && after.devices === 2 && after.allowNext === false,
      `not admin ${notAdmin.status}; allow ${allowed.status}; listed ${listed.devices}/${listed.allowNext}; joined ${joined.status} old ${oldOne.status} third ${third.status}; after ${after.devices}/${after.allowNext}`);
  }

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
  const hiddenScreens = ['menu', 'shell', 'room', 'admin', 'board', 'cal', 'assist', 'settings', 'roomFloat', 'callFloat', 'sessPanel'].filter((id) => !new RegExp(`id="${id}"[^>]*\\bhidden\\b`).test(html) && !new RegExp(`<[a-z]+ [^>]*\\bhidden\\b[^>]*id="${id}"`).test(html));
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
    && /id="spQ"/.test(html) && /function markText\(/.test(html) && /\/api\/archive\/search/.test(html),
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
  const sent23 = (await call('/api/room/send', A, { iv: toB64(iv), ct: toB64(ct), keys })).data || {};
  // (its own message by number: Unity's sign-in greeting can land after it)
  const got = ((await call('/api/room/messages?since=0', U)).data.messages || []).find((m) => m.seq === sent23.seq);
  let text = '';
  try {
    const mk = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(got.key.iv) }, await shared(restored, got.fromPub), fromB64(got.key.wk));
    text = dec.decode(await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(got.iv) }, await subtle.importKey('raw', mk, 'AES-GCM', false, ['decrypt']), fromB64(got.ct)));
  } catch {}
  check('R23', 'A→B message decrypts with a restored key; reader gets only its own slot', text === 'regression secret text' && got && !got.keys);
  const chatFiles = fs.readdirSync(path.join(SCRATCH, 'Chat'), { recursive: true }).map((f) => path.join(SCRATCH, 'Chat', String(f))).filter((f) => fs.statSync(f).isFile()).map((f) => fs.readFileSync(f, 'utf8')).join(''); // the attachment store (Chat/files) too
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
  check('R39', 'default project VibeRoller: the run happens in worktrees/<username> on work/<username>; /api/check names the project and lists the projects (VibeRoller first)', ckU.project === 'VibeRoller' && Array.isArray(ckU.projects) && ckU.projects[0] && ckU.projects[0].name === 'VibeRoller' && ckU.projects[0].hub === true && samePath(cwdOf(ownRun.events), wtU) && branchU === 'work/t-user', cwdOf(ownRun.events));
  const badWs = await call('/api/workspace', U, { project: 'elsewhere' });
  const legacyShared = await call('/api/workspace', U, { workspace: 'shared' }); // an older page's request: the shared folder is gone, it means VibeRoller
  const legacyRun = await chat(U, 'where am I now', { fresh: true });
  const slowWs = chat(U, 'SLOW hold'); await sleep(700);
  const busyWs = await call('/api/workspace', U, { project: 'VibeRoller' });
  await call('/api/stop', U, {}); await slowWs;
  const backOwn = await call('/api/workspace', U, { project: 'vibEROLLER' });
  const keptWs = (await call('/api/check', U)).data.project, otherWs = (await call('/api/check', A)).data.project;
  check('R40', 'the shared-folder choice is gone: an old "shared" request lands in the person\'s own VibeRoller worktree (never the hub\'s checkout); unknown project → 400; while running → 409; the project name is case-insensitive; per account', badWs.status === 400 && legacyShared.status === 200 && legacyShared.data.project === 'VibeRoller'
    && samePath(cwdOf(legacyRun.events), wtU) && !samePath(cwdOf(legacyRun.events), SCRATCH) && busyWs.status === 409 && backOwn.status === 200 && samePath(backOwn.data.dir, wtU) && keptWs === 'VibeRoller' && otherWs === 'VibeRoller', `legacy ${legacyShared.status} ${JSON.stringify(legacyShared.data)} cwd ${cwdOf(legacyRun.events)}`);
  const mg = { status: -1, stdout: '', stderr: 'no worktree' };
  let onDevelop = false, again = '', stOut = '', badUser = false;
  let unbumped = { status: -1, stderr: '' };
  if (fs.existsSync(wtU)) {
    fs.writeFileSync(path.join(wtU, 'regress-note.txt'), 'from t-user\n');
    execFileSync('git', ['add', 'regress-note.txt'], { cwd: wtU }); execFileSync('git', ['commit', '-q', '-m', 'note'], { cwd: wtU });
    const wtJs = path.join(SB, 'worktree.js');
    unbumped = spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' }); // the versioning law: no bump, no merge
    bumpPatch(wtU); execFileSync('git', ['commit', '-qam', 'bump'], { cwd: wtU });
    Object.assign(mg, spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' }));
    onDevelop = spawnSync('git', ['show', 'develop:regress-note.txt'], { cwd: SCRATCH, encoding: 'utf8' }).stdout.includes('from t-user');
    again = spawnSync(process.execPath, [wtJs, 'merge', 't-user'], { cwd: SB, encoding: 'utf8' }).stdout;
    stOut = spawnSync(process.execPath, [wtJs, 'status'], { cwd: SB, encoding: 'utf8' }).stdout;
    badUser = spawnSync(process.execPath, [wtJs, 'ensure', '../evil'], { cwd: SB, encoding: 'utf8' }).status !== 0;
  }
  check('R41', 'worktree.js: merge brings work/<user> into develop; nothing-to-merge; status; bad username refused; the versioning law: a branch whose version is not above develop\'s is not merged, and the message offers the right next numbers', mg.status === 0 && onDevelop && /Nothing to merge/.test(again) && /work\/t-user/.test(stOut) && badUser
    && unbumped.status !== 0 && /every merge moves the version up/.test(unbumped.stderr) && /\d+\.\d+\.\d+ for fixes/.test(unbumped.stderr), String(mg.stdout || mg.stderr).trim().split('\n')[0] + ' | unbumped: ' + String(unbumped.stderr).trim().slice(0, 160));
  let cm = { stdout: '', stderr: 'no worktree' }, cmMerged = false, cmClean = '';
  if (fs.existsSync(wtU)) {
    const wtJs = path.join(SB, 'worktree.js');
    fs.writeFileSync(path.join(wtU, 'regress-uncommitted.txt'), 'left uncommitted\n');
    bumpPatch(wtU);
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

  // Projects (the shared projects area, config projectsRoot): anyone starts a new project or pulls in a repo; everyone
  // works on a project in their own worktree of it; worktree.js --project merges work/<user> into its integration branch.
  {
    const wtJs = path.join(SB, 'worktree.js'), PR = path.join(SCRATCH, 'projects');
    const wt = (...a) => spawnSync(process.execPath, [wtJs, ...a], { cwd: SB, encoding: 'utf8' });
    const listBefore = await call('/api/projects', U);
    const made = await call('/api/projects', U, { name: 'demo-app' });
    const dup = await call('/api/projects', A, { name: 'Demo-App' });
    const badName = await call('/api/projects', U, { name: '../evil' });
    const badName2 = await call('/api/projects', U, { name: '_repo' });
    const badUrl = await call('/api/projects', U, { url: 'ext::sh -c id' });
    const badUrl2 = await call('/api/projects', U, { url: '/etc' });
    const badUrl3 = await call('/api/projects', U, { url: 'C:\\Windows' });
    const repoDir = path.join(PR, 'demo-app', '_repo');
    const repoBranch = fs.existsSync(repoDir) ? spawnSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: repoDir, encoding: 'utf8' }).stdout.trim() : '';
    const repoMain = fs.existsSync(repoDir) ? spawnSync('git', ['rev-parse', '--verify', '--quiet', 'refs/heads/main'], { cwd: repoDir, encoding: 'utf8' }).status === 0 : false;
    // a bare "upstream" with develop + main, pulled in as a second project
    const bare = path.join(SCRATCH, 'upstream.git');
    spawnSync('git', ['init', '-q', '--bare', bare], { encoding: 'utf8' });
    if (fs.existsSync(repoDir)) spawnSync('git', ['push', '-q', bare, 'main', 'develop'], { cwd: repoDir, encoding: 'utf8' });
    const pulled = await call('/api/projects', U, { url: 'file://' + bare.replace(/\\/g, '/'), name: 'pulled' });
    const pulledDir = path.join(PR, 'pulled', '_repo');
    const pulledBranch = fs.existsSync(pulledDir) ? spawnSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: pulledDir, encoding: 'utf8' }).stdout.trim() : '';
    const listAfter = await call('/api/projects', U);
    const names = (listAfter.data.projects || []).map((p) => p.name);
    check('R140', 'projects: POST /api/projects { name } starts a new project in <projectsRoot>/<name>/_repo (README as the first commit, main + develop, develop checked out); a name taken (any case) → 409; bad names and local / helper clone addresses → 400; { url } pulls a repo in (develop checked out when it has one); GET lists VibeRoller first, then the projects by name',
      listBefore.status === 200 && listBefore.data.projects.length === 1 && made.status === 200 && made.data.project === 'demo-app' && dup.status === 409 && badName.status === 400 && badName2.status === 400 && badUrl.status === 400 && badUrl2.status === 400 && badUrl3.status === 400
      && repoBranch === 'develop' && repoMain && fs.existsSync(path.join(repoDir, 'README.md')) && pulled.status === 200 && pulled.data.project === 'pulled' && pulledBranch === 'develop' && names.join(',') === 'VibeRoller,demo-app,pulled',
      `before ${listBefore.status} ${(listBefore.data.projects || []).length}; made ${made.status} ${JSON.stringify(made.data).slice(0, 80)} dup ${dup.status} bad ${badName.status}/${badName2.status}/${badUrl.status}/${badUrl2.status}/${badUrl3.status} repo ${repoBranch} main ${repoMain} readme ${fs.existsSync(path.join(repoDir, 'README.md'))} pulled ${pulled.status} ${pulledBranch} list ${names.join(',')}`);
    const sw = await call('/api/workspace', U, { project: 'demo-app' });
    const pRun = await chat(U, 'where am I in the project', { fresh: true });
    const pDir = path.join(PR, 'demo-app', 't-user');
    const pBranch = fs.existsSync(pDir) ? spawnSync('git', ['rev-parse', '--abbrev-ref', 'HEAD'], { cwd: pDir, encoding: 'utf8' }).stdout.trim() : '';
    const swA = await call('/api/workspace', A, { project: 'demo-app' });
    const toolsLine = (texts(pRun.events).match(/wt=([^|]*)/) || [])[1] || '';
    let pMerged = false, pMergeOut = '', pStatus = '', pProjects = '';
    if (fs.existsSync(pDir)) {
      fs.writeFileSync(path.join(pDir, 'feature.txt'), 'from t-user in demo-app\n');
      pMergeOut = wt('--project', 'demo-app', 'commit', 't-user', 'a feature').stdout + wt('--project', 'demo-app', 'merge', 't-user').stdout;
      pMerged = spawnSync('git', ['show', 'develop:feature.txt'], { cwd: repoDir, encoding: 'utf8' }).stdout.includes('from t-user in demo-app');
      pStatus = wt('status').stdout; pProjects = wt('projects').stdout;
    }
    const back = await call('/api/workspace', U, { project: 'VibeRoller' });
    const backRun = await chat(U, 'back home', { fresh: true });
    check('R141', 'working on a project: switching to it puts the run in <projectsRoot>/<project>/<username> on work/<username> (each person their own); the run may call the hub\'s worktree.js by its full path (not in the VibeRoller worktree); worktree.js --project commit + merge land the work on the project\'s develop; status and projects list it; switching back returns to the VibeRoller worktree',
      sw.status === 200 && samePath(sw.data.dir, pDir) && sw.data.branch === 'work/t-user' && samePath(cwdOf(pRun.events), pDir) && pBranch === 'work/t-user' && swA.status === 200 && samePath(swA.data.dir, path.join(PR, 'demo-app', 't-admin'))
      && /worktree\.js/.test(toolsLine) && !/wt=[^|]*worktree\.js/.test(texts(ownRun.events)) && pMerged && /Merged work\/t-user into develop/.test(pMergeOut) && /demo-app/.test(pStatus) && /demo-app/.test(pProjects) && /pulled/.test(pProjects)
      && back.status === 200 && samePath(back.data.dir, wtU) && samePath(cwdOf(backRun.events), wtU),
      `switch ${sw.status} ${JSON.stringify(sw.data).slice(0, 100)} cwd ${cwdOf(pRun.events)} tools "${toolsLine}" merged ${pMerged} ${pMergeOut.split('\n')[0]}`);

    // R150: the worktree law. "pulled" has a Forgejo stand-in (the bare repo as origin); someone else pushes to it.
    const g = (cwd, ...a) => spawnSync('git', a, { cwd, encoding: 'utf8' });
    const other = path.join(SCRATCH, 'other-machine');
    g(SCRATCH, 'clone', '-q', '-b', 'develop', bare, other);
    const otherPush = (file, text) => { fs.writeFileSync(path.join(other, file), text); g(other, 'add', '-A'); g(other, '-c', 'user.name=other', '-c', 'user.email=o@x', 'commit', '-q', '-m', 'other ' + file); return g(other, 'push', '-q', 'origin', 'develop').status === 0; };
    const qDir = path.join(PR, 'pulled', 't-user');
    wt('--project', 'pulled', 'ensure', 't-user');
    const pushed1 = otherPush('other1.txt', 'one\n');
    const chk1 = wt('--project', 'pulled', 'check', 't-user');
    const auto1 = wt('--project', 'pulled', 'auto', 't-user');
    const ff1 = fs.existsSync(path.join(qDir, 'other1.txt'));
    const chk2 = wt('--project', 'pulled', 'check', 't-user');
    // own commit + an uncommitted edit + an untracked file, then Forgejo moves again → update renews it, carrying all of it
    fs.writeFileSync(path.join(qDir, 'mine.txt'), 'my commit\n'); g(qDir, 'add', 'mine.txt'); g(qDir, '-c', 'user.name=t', '-c', 'user.email=t@x', 'commit', '-q', '-m', 'my work');
    fs.appendFileSync(path.join(qDir, 'README.md'), 'my uncommitted line\n');
    fs.writeFileSync(path.join(qDir, 'scratch-note.txt'), 'untracked\n');
    g(qDir, 'checkout', '-q', '--detach'); // and on the wrong branch, like a worktree made by hand
    const pushed2 = otherPush('other2.txt', 'two\n');
    const chk3 = wt('--project', 'pulled', 'check', 't-user');
    const upd = wt('--project', 'pulled', 'update', 't-user');
    const rd = (f) => { try { return fs.readFileSync(path.join(qDir, f), 'utf8'); } catch { return ''; } };
    const renewed = g(qDir, 'rev-parse', '--abbrev-ref', 'HEAD').stdout.trim() === 'work/t-user' && rd('other2.txt') === 'two\n' && rd('mine.txt') === 'my commit\n'
      && /my uncommitted line/.test(rd('README.md')) && rd('scratch-note.txt') === 'untracked\n' && g(qDir, 'log', '-1', '--format=%s').stdout.trim() === 'my work'
      && !fs.readdirSync(path.join(PR, 'pulled')).some((d) => d.startsWith('_old-')) && /refs\/viberoller\/renewed\/t-user-/.test(g(path.join(PR, 'pulled', '_repo'), 'for-each-ref', 'refs/viberoller').stdout);
    // an uncommitted edit that clashes with Forgejo's: the new worktree is made, the old one is KEPT with the reason
    g(qDir, 'add', '-A'); g(qDir, '-c', 'user.name=t', '-c', 'user.email=t@x', 'commit', '-q', '-m', 'carried');
    fs.writeFileSync(path.join(qDir, 'other2.txt'), 'mine instead\n');
    otherPush('other2.txt', 'theirs instead\n');
    const clash = wt('--project', 'pulled', 'renew', 't-user');
    const oldKept = fs.readdirSync(path.join(PR, 'pulled')).find((d) => d.startsWith('_old-t-user-')) || '';
    const oldHasEdit = oldKept && fs.readFileSync(path.join(PR, 'pulled', oldKept, 'other2.txt'), 'utf8') === 'mine instead\n';
    if (oldKept) g(path.join(PR, 'pulled', '_repo'), 'worktree', 'remove', '--force', path.join(PR, 'pulled', oldKept));
    // merge pushes to Forgejo first; a refused push changes nothing anywhere
    const mergeOk = wt('--project', 'pulled', 'merge', 't-user');
    const onForgejo = g(bare, 'log', '-1', '--format=%s', 'develop').stdout.trim();
    fs.writeFileSync(path.join(qDir, 'blocked.txt'), 'x\n'); g(qDir, 'add', '-A'); g(qDir, '-c', 'user.name=t', '-c', 'user.email=t@x', 'commit', '-q', '-m', 'blocked');
    const hook = path.join(bare, 'hooks', 'pre-receive'); fs.writeFileSync(hook, '#!/bin/sh\necho refused-by-test >&2\nexit 1\n'); fs.chmodSync(hook, 0o755);
    const devBefore = g(path.join(PR, 'pulled', '_repo'), 'rev-parse', 'develop').stdout.trim();
    const mergeRefused = wt('--project', 'pulled', 'merge', 't-user');
    const devAfter = g(path.join(PR, 'pulled', '_repo'), 'rev-parse', 'develop').stdout.trim();
    fs.rmSync(hook, { force: true });
    const wtSrc150 = fs.readFileSync(path.join(BRIDGE, 'worktree.js'), 'utf8'), srvSrc150 = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8'), laws = (() => { try { return fs.readFileSync(path.join(MILLS, 'LAWS.md'), 'utf8'); } catch { return ''; } })();
    check('R150', 'the worktree law: "check" fetches Forgejo and says STALE (exit 3) when a worktree is behind or on the wrong branch, FRESH (exit 0) otherwise; the hub\'s background "auto" fast-forwards a clean one by itself; "update" renews a dirty / wrong-branch one: a NEW worktree on work/<user> from Forgejo with the own commit, the uncommitted edit and the untracked file carried over, then the old one deleted (a backup ref kept); when an uncommitted edit clashes, the old worktree is KEPT with the reason; "merge" pushes to Forgejo first and a refused push changes nothing; run from inside a worktree it reads the main checkout\'s config.local.json; every Work run gets LAWS.md and the freshness instructions with the latest background finding',
      pushed1 && pushed2 && chk1.status === 3 && /1 commit\(s\) behind origin\/develop/.test(chk1.stdout) && /fast-forwarded/.test(auto1.stdout) && ff1 && chk2.status === 0 && /FRESH/.test(chk2.stdout)
      && chk3.status === 3 && /wrong branch/.test(chk3.stdout) && renewed && /Everything came over/.test(upd.stdout)
      && /NOT everything came over/.test(clash.stdout) && oldHasEdit
      && /pushed it to Forgejo/.test(mergeOk.stdout) && onForgejo === 'Merge work/t-user into develop' && mergeRefused.status !== 0 && /nothing was merged anywhere/.test(mergeRefused.stderr) && devBefore === devAfter
      && /config\.local\.json/.test(wtSrc150) && /const CFG = \(\(\) => \{/.test(wtSrc150) && /\['LAWS\.md', 'CLAUDE\.md'\]/.test(srvSrc150) && /freshLaw\(acct, wd\)/.test(srvSrc150) && /checkFresh\(acct\); \/\/ the finding lands/.test(srvSrc150) && /Worktree law/.test(laws)
      && spawnSync('git', ['check-ignore', '-q', 'LAWS.md'], { cwd: MILLS }).status === 1, // published, not swallowed by the whitelist .gitignore (3.2.0 shipped without it)
      `parts ${JSON.stringify({ pushed1, pushed2, chk1: chk1.status, chk3: chk3.status, chk3wrong: /wrong branch/.test(chk3.stdout), renewed, updAll: /Everything came over/.test(upd.stdout), clash: /NOT everything came over/.test(clash.stdout), oldHasEdit, mergeOk: /pushed it to Forgejo/.test(mergeOk.stdout), onForgejo, refusedMsg: /nothing was merged anywhere/.test(mergeRefused.stderr), devSame: devBefore === devAfter, laws: /Worktree law/.test(laws) })} upd "${upd.stdout.replace(/\s+/g, ' ').slice(-200)}"; chk1 ${chk1.status} "${chk1.stdout.trim().slice(0, 120)}"; auto "${auto1.stdout.trim().slice(-60)}" ff ${ff1}; chk2 ${chk2.status}; chk3 ${chk3.status}; renewed ${renewed} "${upd.stdout.replace(/\s+/g, ' ').slice(0, 300)}"; clash kept ${!!oldKept} edit ${oldHasEdit}; merge "${mergeOk.stdout.split('\n')[0]}" forgejo "${onForgejo}"; refused ${mergeRefused.status} "${mergeRefused.stderr.trim().slice(0, 100)}" dev same ${devBefore === devAfter}`);
  }

  // OpenAI-compatible API (/v1) with one key per person (bridge/openai.js), answered by the host's Claude (the fake one here).
  const v1 = async (p, key, body, headers = {}) => { const r = await fetch(H + p, { method: body !== undefined ? 'POST' : 'GET', headers: { 'Content-Type': 'application/json', ...(key ? { Authorization: 'Bearer ' + key } : {}), ...headers }, body: body !== undefined ? JSON.stringify(body) : undefined }); return { status: r.status, text: await r.text(), ct: r.headers.get('content-type') || '' }; };
  const k0 = (await call('/api/apikey', U)).data;
  const madeRes = await call('/api/apikey/rotate', U, { expiresDays: 7 }), made = madeRes.data;
  if (!made.key) console.log(`  (R111 diagnostics: key rotate answered ${madeRes.status} ${JSON.stringify(made).slice(0, 200)})`); // an intermittent "key none" once: record why
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
  // (the hub keeps its own copy in memory; expiry is judged from createdAt + expiresDays at call time). Behind the Rust
  // hub the key's record is its own: bridge/api-keys.json, by username
  const st1 = RUST_HUB ? { api: (JSON.parse(fs.readFileSync(path.join(SB, 'api-keys.json'), 'utf8')) || {})['t-user'] } : JSON.parse(fs.readFileSync(uState, 'utf8'));
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
  // (a custom endpoint or a Claude config folder: only an admin with an AI host of their own online, 4.1.0; so the admin
  // owns these here, with a host of theirs saying hello for the moment, and t-user is the other person)
  const prov0 = (await call('/api/providers', A)).data;
  const badKind = await call('/api/providers/add', A, { kind: 'magic', label: 'x' });
  const badAnth = await call('/api/providers/add', A, { kind: 'anthropic', label: 'bad', apiKey: 'nope' });
  const cfgDir = path.join(SCRATCH, 'claude-sponge-login');
  const cliNoHost = await call('/api/providers/add', A, { kind: 'claude-cli', label: 'no host yet', configDir: cfgDir });
  const tmpHost = (await call('/api/hosts/new', A, {})).data.host || {}, tmpCfg = (await call('/api/hosts/config', A, { id: tmpHost.id })).data.config || {};
  await fetch(H + '/node/hello', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Host': tmpHost.id, 'X-VR-Host-Token': tmpCfg.token }, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex') }) });
  const cliNotAdmin = await call('/api/providers/add', U, { kind: 'claude-cli', label: 'Tuser Claude login', configDir: cfgDir });
  const oaiNotAdmin = await call('/api/providers/add', U, { kind: 'openai', label: 'Tuser endpoint', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, models: 'fake-gpt' });
  const cli = (await call('/api/providers/add', A, { kind: 'claude-cli', label: 'Tadmin Claude login', configDir: cfgDir, shared: false })).data;
  const oai = (await call('/api/providers/add', A, { kind: 'openai', label: 'Tadmin endpoint', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, apiKey: 'k', models: 'fake-gpt, fake-mini', shared: true })).data;
  await call('/api/hosts/remove', A, { id: tmpHost.id });
  const seenByU = (await call('/api/providers', U)).data.providers.map((p) => p.id);
  const secretLeak = JSON.stringify((await call('/api/providers', A)).data).includes('"apiKey"');
  const pickCli = await call('/api/model', A, { provider: cli.provider.id, model: 'claude-sonnet-5-5' });
  const cliRun = await chat(A, 'who runs me', { fresh: true });
  const ck7 = (await call('/api/check', A)).data;
  const pickOai = await call('/api/model', A, { provider: oai.provider.id, model: 'fake-gpt' });
  const useOai = (await call('/api/apikey/rotate', A, {})).data;
  const setOai = await call('/api/apikey/settings', A, { provider: oai.provider.id, model: 'fake-mini' });
  const oaiModels = await v1('/v1/models', useOai.key);
  const oaiChat = await v1('/v1/chat/completions', useOai.key, { model: 'fake-gpt', messages: [{ role: 'system', content: 'sys' }, { role: 'user', content: 'through-r112' }] });
  const oaiText = ((((JSON.parse(oaiChat.text || '{}').choices || [])[0] || {}).message || {}).content) || '';
  const oaiStream = await v1('/v1/chat/completions', useOai.key, { model: 'fake-gpt', stream: true, messages: [{ role: 'user', content: 'flow-r112' }] });
  const oaiStreamed = oaiStream.text.split('\n').filter((l) => l.startsWith('data: ') && l !== 'data: [DONE]').map((l) => ((JSON.parse(l.slice(6)).choices || [])[0] || {}).delta || {}).map((d) => d.content || '').join('');
  // Anthropic → OpenAI: the Messages format in (Anthropic's x-api-key header, the same key), the OpenAI endpoint answers
  const antHdr = { 'x-api-key': useOai.key, 'anthropic-version': '2023-06-01' };
  const antMsg = await v1('/v1/messages', null, { model: 'claude-opus-5-5', max_tokens: 100, system: [{ type: 'text', text: 'sys' }], messages: [{ role: 'user', content: [{ type: 'text', text: 'anthropic-in-r160' }] }] }, antHdr);
  const antJ = (() => { try { return JSON.parse(antMsg.text); } catch { return {}; } })();
  const antStream = await v1('/v1/messages', null, { model: 'fake-gpt', max_tokens: 100, stream: true, messages: [{ role: 'user', content: 'anthropic-stream-r160' }] }, antHdr);
  const antEvents = antStream.text.split('\n').filter((l) => l.startsWith('event: ')).map((l) => l.slice(7));
  const antStreamed = antStream.text.split('\n').filter((l) => l.startsWith('data: ')).map((l) => { try { return JSON.parse(l.slice(6)); } catch { return {}; } }).filter((d) => d.type === 'content_block_delta').map((d) => d.delta.text).join('');
  const antModels = await v1('/v1/models', null, undefined, antHdr), antModelsJ = (() => { try { return JSON.parse(antModels.text); } catch { return {}; } })();
  const antCount = await v1('/v1/messages/count_tokens', null, { model: 'x', messages: [{ role: 'user', content: 'abcdefgh' }] }, antHdr);
  const antNoKey = await v1('/v1/messages', null, { messages: [{ role: 'user', content: 'x' }] }, { 'anthropic-version': '2023-06-01' });
  const antEmpty = await v1('/v1/messages', null, { max_tokens: 5, messages: [] }, antHdr);
  const oaiXKey = await v1('/v1/chat/completions', null, { model: 'fake-gpt', messages: [{ role: 'user', content: 'xkey-r160' }] }, { 'x-api-key': useOai.key });
  const otherTouch = await call('/api/providers/update', U, { id: cli.provider.id, shared: true });
  const shareIt = await call('/api/providers/update', A, { id: cli.provider.id, shared: true });
  const nowSeenByU = (await call('/api/providers', U)).data.providers.map((p) => p.id);
  const rmOther = await call('/api/providers/remove', U, { id: cli.provider.id });
  const rm = await call('/api/providers/remove', A, { id: cli.provider.id });
  const ck7b = (await call('/api/check', A)).data;
  await call('/api/providers/remove', A, { id: oai.provider.id }); await call('/api/apikey/revoke', A, {}); await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' });
  check('R112', 'AI providers: the host Claude is always listed; an admin with an AI host of their own online adds a Claude Code login (own folder) and an OpenAI endpoint (private or shared; bad kind / key refused; without a host of theirs online, or not an admin → refused; secrets never listed); a private one is invisible to others; picking the login runs Work with CLAUDE_CONFIG_DIR set and that model; an OpenAI provider is refused for Work; the API key answers through the endpoint provider (plain + streaming, system passed); only the owner shares / removes; removal clears the person\'s choice',
    // (the test AI host's own providers, "h:…", are listed too: they survive the early hub restart since 3.3.0)
    prov0.providers.filter((p) => !String(p.id).startsWith('h:')).length === 1 && prov0.providers[0].id === 'host' && badKind.status === 400 && badAnth.status === 400 && cliNoHost.status === 400 && cliNotAdmin.status === 400 && oaiNotAdmin.status === 400 && cli.provider && oai.provider && !secretLeak
    && !seenByU.includes(cli.provider.id) && seenByU.includes(oai.provider.id)
    && pickCli.status === 200 && pickCli.data.provider === cli.provider.id && new RegExp('cfg=' + cfgDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).test(texts(cliRun.events)) && /model=claude-sonnet-5-5/.test(texts(cliRun.events)) && ck7.provider === cli.provider.id && (ck7.providers || []).some((p) => p.id === cli.provider.id)
    && pickOai.status === 400 && setOai.status === 200 && setOai.data.provider === oai.provider.id && oaiModels.status === 200 && /fake-mini/.test(oaiModels.text)
    && oaiChat.status === 200 && /^oai says: through-r112 \[sys 3\]/.test(oaiText) && /flow-r112/.test(oaiStreamed)
    && otherTouch.status === 400 && shareIt.status === 200 && nowSeenByU.includes(cli.provider.id) && rmOther.status === 400 && rm.status === 200 && ck7b.provider === 'host'
    && /id="provBox"/.test(html) && /\/api\/providers\/add/.test(html) && /o\.value = p\.id \+ '\|' \+ m/.test(html),
    `providers at start ${prov0.providers.map((p) => p.id).join(',')}; refused no-host ${cliNoHost.status} not-admin ${cliNotAdmin.status}/${oaiNotAdmin.status}; cli ${cli.provider ? 'made' : JSON.stringify(cli).slice(0, 120)} ${pickCli.status} run "${texts(cliRun.events).slice(-120)}", oai work ${pickOai.status}, api via oai ${oaiChat.status} "${oaiText.slice(0, 50)}", stream "${oaiStreamed.slice(0, 40)}", share ${otherTouch.status}/${shareIt.status}, remove ${rmOther.status}/${rm.status}`);
  // Jcode PGM as a Work harness (R180, 5.7.0): an admin adds a `jcode` provider (a jcodepgm binary on the hub; the fake
  // here), picks it, and a Work run goes through `jcodepgm run --ndjson` in the worktree with a private jcode home that
  // carries the hub's MCP servers and the system prompt as its overlay; jcode's events reach the page in the shape it
  // renders; a session resumes with the same harness only; an error ends as a failed result; a non-admin is refused.
  if (RUST_HUB) {
    const jNotAdmin = await call('/api/providers/add', U, { kind: 'jcode', label: 'Tuser jcode', command: process.execPath });
    const jBadHome = await call('/api/providers/add', A, { kind: 'jcode', label: 'bad home', command: process.execPath, home: 'relative/x' });
    const jc = (await call('/api/providers/add', A, { kind: 'jcode', label: 'Jcode PGM (fake)', command: process.execPath, args: [path.join(SB, 'test', 'fake-jcode.js')], provider: 'viberoller', models: 'claude-haiku-5-5, claude-sonnet-5-5', shared: false })).data;
    const jView = jc.provider || {};
    const pickJ = await call('/api/model', A, { provider: jView.id, model: 'claude-haiku-5-5' });
    const jRun1 = await chat(A, 'hello from r180 MCP TOOL', { fresh: true });
    const t1 = texts(jRun1.events);
    const init1 = jRun1.events.find((e) => e.type === 'system' && e.subtype === 'init') || {};
    const toolUse = jRun1.events.some((e) => e.type === 'assistant' && e.message.content.some((c) => c.type === 'tool_use' && c.name === 'bash' && c.input && c.input.command === 'echo hi'));
    const toolRes = jRun1.events.some((e) => e.type === 'user' && e.message.content.some((c) => c.type === 'tool_result' && c.tool_use_id === 't1' && c.is_error === false));
    const res1 = jRun1.events.find((e) => e.type === 'result') || {};
    const done1 = jRun1.events.find((e) => e.type === 'bridge_done') || {};
    const jRun2 = await chat(A, 'again r180');
    const t2 = texts(jRun2.events);
    const jFail = await chat(A, 'please FAIL r180');
    const resF = jFail.events.find((e) => e.type === 'result') || {};
    const doneF = jFail.events.find((e) => e.type === 'bridge_done') || {};
    const jHome = path.join(SB, 'profiles', 't-admin', 'jcode-home');
    const jMcp = fs.existsSync(path.join(jHome, 'mcp.json')) ? JSON.parse(fs.readFileSync(path.join(jHome, 'mcp.json'), 'utf8')) : {};
    const jOverlay = fs.existsSync(path.join(jHome, 'prompt-overlay.md')) ? fs.readFileSync(path.join(jHome, 'prompt-overlay.md'), 'utf8') : '';
    // back to Claude: a jcode session id is never resumed by Claude Code
    await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' });
    const back = await chat(A, 'back to claude r180');
    const backText = texts(back.events);
    await call('/api/providers/remove', A, { id: jView.id });
    check('R180', 'Jcode PGM as a Work harness: an admin adds a jcode provider (a jcodepgm binary on the hub; refused for a non-admin, a relative home refused); picking it runs Work through `jcodepgm run --ndjson` in the worktree with the asked model and provider profile, a private jcode home per person carrying the hub\'s MCP servers (the pc tool answers through them) and the system prompt as the prompt overlay; the events reach the page in its shape (init, assistant text + tool_use, tool_result, result) and the run ends 0; the next message resumes the same jcode session; an error is a failed result and a non-zero end; switching back to Claude never resumes a jcode session',
      jNotAdmin.status === 400 && jBadHome.status === 400 && !!jView.id && jView.harness === 'jcode' && pickJ.status === 200 && pickJ.data.provider === jView.id
      && init1.harness === 'jcode' && /^fakej-/.test(init1.session_id || '') && /jcode-answer model=claude-haiku-5-5 provider=viberoller/.test(t1) && /resume=no/.test(t1) && /overlay:yes/.test(t1) && /MCP:\{"ok":true/.test(t1)
      && toolUse && toolRes && res1.is_error === false && res1.harness === 'jcode' && done1.code === 0
      && /resume=yes/.test(t2) && resF.is_error === true && /fake failure/.test(resF.result || '') && doneF.code === 1
      && jMcp.mcpServers && jMcp.mcpServers.pc && jMcp.mcpServers.board && jMcp.mcpServers.team && /Worktree law/.test(jOverlay)
      && back.status === 200 && !/jcode-answer/.test(backText),
      `add ${jNotAdmin.status}/${jBadHome.status}/${jc.provider ? 'made' : JSON.stringify(jc).slice(0, 100)}, pick ${pickJ.status}, run1 "${t1.slice(0, 160)}" tool ${toolUse}/${toolRes} result ${res1.is_error} code ${done1.code}; run2 "${t2.slice(-40)}"; fail ${resF.is_error} code ${doneF.code}; home mcp ${Object.keys(jMcp.mcpServers || {}).join(',')} overlay ${jOverlay.length}b; back "${backText.slice(0, 60)}"`);
  } else skip('R180', 'Jcode PGM as a Work harness (a jcode provider runs Work through jcodepgm run --ndjson)', 'the Rust hub\'s: --rust-hub runs it');
  // HubEvent in the page (R187, 5.10.0; Jcode-Mods WORKPLAN 9b): the hub's Work events carry one schema, the `hub` object
  // (harness.rs HubEvent::into_wire) next to the Claude-shaped alias fields; the page's Work renderer reads it first
  // (hubView) and falls back to the alias for an event without it, so an older hub or host renders the same. Static +
  // the page's own hubView run on sample events (both modes), and with the Rust hub a Claude run (the fake claude) and a
  // Jcode PGM run (the fake jcodepgm) of the same shape give the same HubEvent structure.
  {
    const L187 = 'HubEvent is the one schema the page consumes: the hub adds a `hub` object (kind init / assistant{blocks text, tool_use} / tool_results / result, harness, session) to every harness event while the alias fields stay; the page\'s handleEvent reads hubView(ev) (the hub object first, else derived from the alias, the same view) and no longer reads the alias fields itself; a Claude run and a Jcode PGM run of the same shape (text, tool use, tool result, result) have the same structure through HubEvent and both keep the alias';
    const page187 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), harness187 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'harness.rs'), 'utf8');
    const hvSrc = (/function hubView\(ev\) \{[\s\S]*?\n\}\n(?=function handleEvent)/.exec(page187) || [''])[0];
    const heSrc = (/function handleEvent\(ev\) \{[\s\S]*?\n\}\n(?=\n\/\/ ── Slash commands)/.exec(page187) || [''])[0];
    let hubView = null; try { hubView = require('vm').runInNewContext(hvSrc + '; hubView'); } catch {}
    const aliasEvs = hubView ? [
      { type: 'system', subtype: 'init', session_id: 'a1', model: 'm1' },
      { type: 'assistant', message: { content: [{ type: 'text', text: 'T' }, { type: 'tool_use', id: 't1', name: 'Bash', input: { command: 'ls' } }], usage: { input_tokens: 2 } } },
      { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'x', is_error: true }] } },
      { type: 'result', is_error: false, result: 'ok', duration_ms: 2000, subtype: 'success' }].map((e) => JSON.parse(JSON.stringify(hubView(e)))) : [];
    const hubFirst = hubView ? hubView({ type: 'assistant', hub: { v: 1, kind: 'assistant', blocks: [{ type: 'text', text: 'HUB' }] }, message: { content: [{ type: 'text', text: 'ALIAS' }] } }) : null;
    const fromAlias = aliasEvs.length === 4 && aliasEvs[0].kind === 'init' && aliasEvs[0].model === 'm1' && aliasEvs[1].kind === 'assistant' && aliasEvs[1].blocks.map((b) => b.type).join() === 'text,tool_use' && aliasEvs[1].blocks[1].input.command === 'ls' && aliasEvs[1].usage.input_tokens === 2
      && aliasEvs[2].kind === 'tool_results' && aliasEvs[2].results[0].id === 't1' && aliasEvs[2].results[0].error === true && aliasEvs[3].kind === 'result' && aliasEvs[3].error === false && aliasEvs[3].text === 'ok' && aliasEvs[3].durationMs === 2000 && hubView({ type: 'rate_limit_event' }) === null;
    const stat = !!hubView && fromAlias && !!hubFirst && hubFirst.blocks[0].text === 'HUB' && /const hv = hubView\(ev\), hk = hv \? hv\.kind : ''/.test(heSrc) && !/ev\.message|ev\.is_error|ev\.duration_ms|ev\.subtype|ev\.result/.test(heSrc)
      && /pub fn into_wire\(self\)/.test(harness187) && /HUB_SCHEMA/.test(harness187) && /crate::harness::wire\(ev\)/.test(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'work.rs'), 'utf8'));
    if (RUST_HUB) {
      const shape = (evs) => evs.filter((e) => e.hub).map((e) => e.hub.kind + '[' + (e.hub.kind === 'assistant' ? e.hub.blocks.map((b) => b.type) : e.hub.kind === 'tool_results' ? e.hub.results.map((r) => 'result:' + r.error) : e.hub.kind === 'result' ? ['error:' + e.hub.error] : []).join(',') + ']').join(' ');
      await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' });
      const cRun = await chat(A, 'claude run CLAUDETOOL r187', { fresh: true });
      const jp = (await call('/api/providers/add', A, { kind: 'jcode', label: 'Jcode PGM (fake, r187)', command: process.execPath, args: [path.join(SB, 'test', 'fake-jcode.js')], provider: 'viberoller', models: 'claude-haiku-5-5', shared: false })).data.provider || {};
      await call('/api/model', A, { provider: jp.id, model: 'claude-haiku-5-5' });
      const jRun = await chat(A, 'jcode run TOOL r187', { fresh: true });
      await call('/api/providers/remove', A, { id: jp.id });
      await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' });
      const sc = shape(cRun.events), sj = shape(jRun.events), want = 'init[] assistant[tool_use] tool_results[result:false] assistant[text] result[error:false]';
      const aliasKept = (evs) => evs.some((e) => e.type === 'assistant' && e.message.content.some((c) => c.type === 'tool_use' && c.input.command === 'echo hi')) && evs.some((e) => e.type === 'user' && e.message.content.some((c) => c.type === 'tool_result' && c.tool_use_id === 't1')) && evs.some((e) => e.type === 'result' && e.is_error === false);
      const hubOf = (evs, k) => (evs.find((e) => e.hub && e.hub.kind === k) || {}).hub || {};
      check('R187', L187,
        stat && sc === want && sj === want && hubOf(cRun.events, 'init').harness === 'claude' && hubOf(jRun.events, 'init').harness === 'jcode' && /^fake-/.test(hubOf(cRun.events, 'init').session) && /^fakej-/.test(hubOf(jRun.events, 'init').session)
        && hubOf(cRun.events, 'result').error === false && hubOf(jRun.events, 'result').harness === 'jcode' && /jcode-answer/.test(hubOf(jRun.events, 'result').text) && aliasKept(cRun.events) && aliasKept(jRun.events)
        && cRun.events.some((e) => e.type === 'bridge_done' && e.code === 0) && jRun.events.some((e) => e.type === 'bridge_done' && e.code === 0),
        `static ${stat} (alias view ${fromAlias}); claude "${sc}" jcode "${sj}"`);
    } else {
      check('R187', L187, stat, `static ${stat} (alias view ${fromAlias}); the runs through both harnesses: --rust-hub`);
    }
  }
  // Jcode PGM on the AI host (R188, 5.10.0; Jcode-Mods WORKPLAN 9c): opt-in. A native host with allowWork and a provider of
  // kind "jcode" (jcodeCommand: the fake jcodepgm) announces the cap `work-jcode`, the hub lists that provider (kind
  // host-jcode) and sends it a Work job: jcodepgm runs ON the host in its worktree with a private jcode home per person
  // (the team MCP servers as mcp.json, the hub's system prompt as the overlay); the events come back as HubEvents and a
  // second message resumes the session. A host that allows no Work says hello with exactly the old caps and offers no
  // jcode provider: the hub never lists one, so never sends it a job.
  if (RUST_HUB) {
    const L188 = 'Jcode PGM on the AI host, opt-in: a native host with allowWork and a "jcode" provider announces the cap work-jcode and the hub lists it (host-jcode) and sends it a Work job; jcodepgm run --ndjson runs ON the host in its worktree with a private jcode home per person (mcp.json with the team servers, the system prompt as the prompt overlay), its events reach the page as HubEvents (init, tool use, tool result, text, result), the next message resumes the same jcode session; a host with the provider but no allowWork says hello with exactly the old caps, offers no jcode provider and cannot be picked; only the host\'s owner can run Work on it; it answers no API calls';
    const jdir = path.join(SCRATCH, 'jcode-host'), pdir = path.join(SCRATCH, 'plain-host'), repo = path.join(jdir, 'repo');
    fs.mkdirSync(repo, { recursive: true }); fs.mkdirSync(pdir, { recursive: true });
    const gitj = (...a) => execFileSync('git', ['-c', 'user.email=r188@example.invalid', '-c', 'user.name=r188', ...a], { cwd: repo, stdio: 'ignore', windowsHide: true });
    gitj('init', '-q', '-b', 'main'); fs.writeFileSync(path.join(repo, 'README.md'), 'r188\n'); gitj('add', '-A'); gitj('commit', '-q', '-m', 'r188'); gitj('branch', 'develop');
    const provJ = { id: 'jcode', kind: 'jcode', label: 'R188 jcode', models: ['claude-haiku-5-5', 'claude-sonnet-5-5'], jcodeCommand: process.execPath, jcodeArgs: [path.join(SB, 'test', 'fake-jcode.js')], jcodeProvider: 'viberoller' };
    const jcfg = path.join(jdir, 'ai-node.json'), pcfg = path.join(pdir, 'ai-node.json');
    fs.writeFileSync(jcfg, JSON.stringify({ hub: H, id: 'jcode-node', token: OLD_NODE_SECRET + '-jc', user: 't-admin', autoUpdate: false, allowWork: true, providers: [provJ], engines: {}, work: { hubRepo: repo, worktreeRoot: path.join(jdir, 'worktrees') } }), { mode: 0o600 });
    fs.writeFileSync(pcfg, JSON.stringify({ hub: H, id: 'plain-node', token: OLD_NODE_SECRET + '-pl', user: 't-admin', autoUpdate: false, providers: [{ ...provJ }], engines: {} }), { mode: 0o600 });
    const hostBin = NATIVE_BIN.host;
    const procs = [[jcfg, jdir], [pcfg, pdir]].map(([cfg, dir]) => { const fd = fs.openSync(path.join(dir, 'out.log'), 'a'); return spawn(hostBin, ['run', '--config', cfg], { cwd: dir, stdio: ['ignore', fd, fd], windowsHide: true }); });
    const nodes = async (id) => { for (let i = 0; i < 80; i++) { const n = ((await call('/api/check', A)).data.aiNodes || []).find((x) => x.id === id); if (n && n.online && (n.caps || []).length) return n; await sleep(250); } return null; };
    const jn = await nodes('jcode-node'), pn = await nodes('plain-node');
    const provs = async () => (await call('/api/check', A)).data.providers || [];
    let jprov = null; for (let i = 0; i < 40 && !jprov; i++) { jprov = (await provs()).find((p) => p.id === 'h:jcode-node/jcode') || null; if (!jprov) await sleep(250); }
    const plainListed = (await provs()).some((p) => p.id === 'h:plain-node/jcode');
    const pickPlain = await call('/api/model', A, { provider: 'h:plain-node/jcode', model: 'claude-haiku-5-5' });
    const otherSees = (((await call('/api/check', U)).data.providers) || []).some((p) => p.id === 'h:jcode-node/jcode');
    const pick = await call('/api/model', A, { provider: 'h:jcode-node/jcode', model: 'claude-sonnet-5-5' });
    await call('/api/workspace', A, { project: 'VibeRoller' }); // the host's repo here is the hub project's (R141 uses demo-app on a host that clones it)
    const r1 = await chat(A, 'host jcode TOOL r188', { fresh: true });
    const r1t = texts(r1.events), r1init = (r1.events.find((e) => e.hub && e.hub.kind === 'init') || {}).hub || {}, r1res = (r1.events.find((e) => e.hub && e.hub.kind === 'result') || {}).hub || {};
    const r2 = await chat(A, 'host jcode again r188'), r2t = texts(r2.events);
    const rF = await chat(A, 'host jcode FAIL r188'), rFres = (rF.events.find((e) => e.hub && e.hub.kind === 'result') || {}).hub || {};
    const jhome = path.join(jdir, 'jcode', 't-admin', 'jcode-home');
    const jmcp = fs.existsSync(path.join(jhome, 'mcp.json')) ? JSON.parse(fs.readFileSync(path.join(jhome, 'mcp.json'), 'utf8')) : {};
    const joverlay = fs.existsSync(path.join(jhome, 'prompt-overlay.md')) ? fs.readFileSync(path.join(jhome, 'prompt-overlay.md'), 'utf8') : '';
    await call('/api/workspace', A, { project: 'demo-app' });
    // the API answers through that provider are refused (it runs Work, not answers); back to Claude afterwards
    const key = (await call('/api/apikey/rotate', A, {})).data.key;
    await call('/api/apikey/settings', A, { provider: 'h:jcode-node/jcode' });
    const apiTry = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + key }, body: JSON.stringify({ model: 'claude-haiku-5-5', messages: [{ role: 'user', content: 'x' }] }) });
    await call('/api/apikey/revoke', A, {});
    await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' });
    const hostLog = (() => { try { return fs.readFileSync(path.join(jdir, 'out.log'), 'utf8'); } catch { return ''; } })();
    for (const p of procs) { try { p.kill(); } catch {} }
    const hostRs = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'work.rs'), 'utf8');
    check('R188', L188,
      !!jn && jn.caps.includes('work-jcode') && ['relay', 'update', 'update-bin'].every((c) => jn.caps.includes(c)) && !!pn && pn.caps.join(',') === 'relay,update,update-bin,control,signed-pc' && (pn.providers || []).length === 0
      && !!jprov && jprov.kind === 'host-jcode' && jprov.owner === 't-admin' && !otherSees && !plainListed && pickPlain.status !== 200 && pick.status === 200 && pick.data.provider === 'h:jcode-node/jcode'
      && r1.events.some((e) => e.type === 'bridge_info' && /through Jcode PGM/.test(e.text)) && r1init.harness === 'jcode' && /^fakej-/.test(r1init.session)
      && /jcode-answer model=claude-sonnet-5-5 provider=viberoller/.test(r1t) && /cwd=\S*jcode-host\S*worktrees\S*t-admin/.test(r1t) && /resume=no/.test(r1t) && /overlay:yes/.test(r1t)
      && r1.events.some((e) => e.type === 'assistant' && e.hub && e.hub.blocks.some((b) => b.type === 'tool_use' && b.input.command === 'echo hi')) && r1.events.some((e) => e.type === 'user' && e.hub && e.hub.results[0].id === 't1')
      && r1res.harness === 'jcode' && r1res.error === false && r1.events.some((e) => e.type === 'bridge_done' && e.code === 0)
      && /resume=yes/.test(r2t) && rFres.error === true && rF.events.some((e) => e.type === 'bridge_done' && e.code === 1)
      && jmcp.mcpServers && jmcp.mcpServers.pc && jmcp.mcpServers.board && jmcp.mcpServers.team && /Worktree law/.test(joverlay)
      && apiTry.status >= 400 && /pub fn jcode_spec/.test(hostRs),
      `caps ${jn && jn.caps.join(',')} / plain ${pn && pn.caps.join(',')} providers ${pn && (pn.providers || []).join(',')}; listed ${jprov && jprov.kind} other ${otherSees} plain ${plainListed}/${pickPlain.status}; pick ${pick.status}; run1 ${r1.status} [${r1.events.map((e) => e.type).join(' ')}] errors "${r1.events.filter((e) => e.type === 'bridge_error' || e.type === 'stderr' || e.type === 'bridge_info').map((e) => e.text).join(' | ').slice(0, 300)}" "${r1t.slice(0, 200)}" init ${JSON.stringify(r1init).slice(0, 80)}; run2 "${r2t.slice(-40)}"; fail ${rFres.error}; home ${Object.keys(jmcp.mcpServers || {}).join(',')} overlay ${joverlay.length}b; api ${apiTry.status}; log "${hostLog.replace(/\s+/g, ' ').slice(-200)}"`);
  } else skip('R188', 'Jcode PGM on the AI host, opt-in (a native host with allowWork and a jcode provider announces work-jcode and runs Work through jcodepgm)', 'the Rust hub\'s: --rust-hub runs it');
  // The prompt cache (R181, 5.7.0; Jcode-Mods TOKENS.md): the system prompt a Work run gets is the same bytes every run,
  // so the provider's prompt cache can hit: no "N min ago" and no timestamp in it; the worktree law stays there, the
  // latest freshness check travels with the message instead.
  if (RUST_HUB) {
    const r181 = await chat(A, 'cache check r181', { fresh: true });
    const e181 = texts(r181.events);
    check('R181', 'the Work system prompt is cache-stable: the same bytes every run (no relative time, no timestamp in it); the worktree law stays in it and the volatile freshness check result travels with the message',
      /sysvolatile=false/.test(e181) && /syslaw=true/.test(e181),
      `echo "${(e181.match(/sysvolatile=[a-z]+ \| syslaw=[a-z]+/) || [''])[0]}"`);
  } else skip('R181', 'the Work system prompt is cache-stable', 'the Rust hub\'s Work: --rust-hub runs it');
  // The token library in the hub (R185, 5.9.0): an answer outside Work (the /v1 API) goes through the usage ledger, the
  // provider's cached tokens normalised, and /api/usage shows them as `answers` with the cache hit ratio.
  if (RUST_HUB) {
    const h185 = (await call('/api/hosts/new', A, {})).data.host || {}, c185 = (await call('/api/hosts/config', A, { id: h185.id })).data.config || {};
    await fetch(H + '/node/hello', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Host': h185.id, 'X-VR-Host-Token': c185.token }, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex') }) });
    const p185 = (await call('/api/providers/add', A, { kind: 'openai', label: 'R185 endpoint', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, apiKey: 'k', models: 'fake-gpt', shared: false })).data;
    const k185 = (await call('/api/apikey/rotate', A, {})).data;
    await call('/api/apikey/settings', A, { provider: p185.provider && p185.provider.id, model: 'fake-gpt' });
    const u0 = (await call('/api/usage', A)).data;
    const a1 = await v1('/v1/chat/completions', k185.key, { model: 'fake-gpt', messages: [{ role: 'user', content: 'cache r185' }] });
    const a2 = await v1('/v1/chat/completions', k185.key, { model: 'fake-gpt', messages: [{ role: 'user', content: 'cache r185 again' }] });
    const u1 = (await call('/api/usage', A)).data, ans = u1.answers || {};
    const b0 = (u0.answers && u0.answers.turns) || 0;
    const body185 = (() => { try { return JSON.parse(a1.text); } catch { return {}; } })();
    await call('/api/providers/remove', A, { id: p185.provider && p185.provider.id }); await call('/api/apikey/revoke', A, {}); await call('/api/model', A, { model: 'claude-opus-5-5', provider: 'host' }); await call('/api/hosts/remove', A, { id: h185.id });
    check('R185', 'the token library in the hub: an answer outside Work (a /v1 call through an OpenAI-compatible endpoint whose usage has prompt_tokens_details.cached_tokens) is normalised by the usage ledger and /api/usage shows it under `answers` (turns, input paid, cached, written, hit ratio, per provider kind) while every existing key stays; the caller still gets the endpoint\'s own OpenAI-style usage',
      p185.provider && a1.status === 200 && a2.status === 200 && ans.turns === b0 + 2 && ans.cached >= 3000 && ans.input >= 1000 && ans.hit > 0.5 && ans.hit < 0.8 && ans.by && ans.by.openai && ans.by.openai.cached >= 3000
      && body185.usage && body185.usage.prompt_tokens === 2000 && 'five_hour' in u1 && 'seven_day' in u1 && 'context' in u1,
      `answers ${JSON.stringify(ans).slice(0, 260)}; call ${a1.status}/${a2.status}; usage ${JSON.stringify(body185.usage || {})}`);
  } else skip('R185', 'answers outside Work through the usage ledger', 'the Rust hub\'s: --rust-hub runs it');
  // Sharing with scopes (R189, 5.11.0; Jcode-Mods ARCHITECTURE.md §4): the owner of a shared provider picks, per person (and
  // per "@group"), which of its models they may use and what for (Work, the API, Unity); stored on the hub, never on the host.
  if (RUST_HUB) {
    const L189 = 'sharing with scopes: the owner picks per person (or @group) which models of a shared provider they may use and what for (work / api / unity), stored on the hub in the host record next to share (the host config is untouched); the person sees only those models in /api/check, a /v1 answer on another model or without the api use is refused naming the owner, so is a Work run without the work use; the person\'s own scope wins over the group\'s; default (no entry) is everything; only the owner sets it, models must be the provider\'s own; [static] the Settings editor';
    const h189 = (await call('/api/hosts/new', A, {})).data.host || {}, c189 = (await call('/api/hosts/config', A, { id: h189.id })).data.config || {};
    const dir189 = path.join(SCRATCH, 'r189-host'); fs.mkdirSync(dir189, { recursive: true });
    for (const f of ['ai-node.js', 'host-relay.js']) fs.copyFileSync(path.join(SB, f), path.join(dir189, f)); // its own folder: the host keeps one instance per folder
    const base189 = `http://127.0.0.1:${OLLAMA_PORT}`, f189 = path.join(dir189, 'ai-node.json');
    fs.writeFileSync(f189, JSON.stringify({ hub: H, id: h189.id, token: c189.token, user: 't-admin', autoUpdate: false, engines: {}, providers: [
      { id: 'sc-claude', kind: 'claude-relay', label: 'R189 Claude', models: ['claude-fable-5-1', 'claude-haiku-5-5'], allowedUsers: ['t-user'], configDir: path.join(SCRATCH, 'host-claude-login'), baseUrl: base189 + '/anthropic' },
      { id: 'sc-local', kind: 'openai', label: 'R189 local', baseUrl: base189 + '/oai', models: ['tiny-a', 'tiny-b'], allowedUsers: ['t-user'] }] }), { mode: 0o600 });
    const log189 = fs.openSync(path.join(dir189, 'out.log'), 'a');
    const proc189 = RUST_HOST ? spawn(RUST_HOST, ['host', '--config', f189], { cwd: dir189, stdio: ['ignore', log189, log189], windowsHide: true }) : spawn(process.execPath, [path.join(dir189, 'ai-node.js'), '--config', f189], { cwd: dir189, stdio: ['ignore', log189, log189], windowsHide: true });
    const sees = async (pid) => ((await call('/api/check', U)).data.providers || []).find((p) => p.id === 'h:' + h189.id + '/' + pid) || null;
    let loc = null; for (let i = 0; i < 80 && !(loc && loc.models.length === 2); i++) { loc = await sees('sc-local'); if (!loc || loc.models.length !== 2) await sleep(250); }
    const scope = (provider, user, models, uses, tok = A) => call('/api/hosts/scope', tok, { id: h189.id, provider, user, models, uses });
    const k189 = (await call('/api/apikey/rotate', U, {})).data.key;
    const ask = (model) => v1('/v1/chat/completions', k189, { model, messages: [{ role: 'user', content: 'scope r189' }] });
    await call('/api/apikey/settings', U, { provider: 'h:' + h189.id + '/sc-local', model: 'tiny-a' });
    const unscopedB = await ask('tiny-b'); // the default: every model, every use
    const notOwner = await scope('sc-local', 't-user', ['tiny-a'], ['api'], U), badModel = await scope('sc-local', 't-user', ['nope'], '*'), badUse = await scope('sc-local', 't-user', '*', ['root']), forOwner = await scope('sc-local', 't-admin', ['tiny-a'], '*'), badWho = await scope('sc-local', 'Bad Name!', ['tiny-a'], '*');
    // t-user: one model and the API only, on both providers
    const s1 = await scope('sc-local', 't-user', ['tiny-a'], ['api']), s2 = await scope('sc-claude', 't-user', ['claude-haiku-5-5'], ['api']);
    const locU = await sees('sc-local'), claU = await sees('sc-claude');
    const ownerSees = ((await call('/api/check', A)).data.providers || []).find((p) => p.id === 'h:' + h189.id + '/sc-local');
    const okA = await ask('tiny-a'), noB = await ask('tiny-b'), noBBody = (() => { try { return JSON.parse(noB.text); } catch { return {}; } })();
    // Work on the Claude without the work use: refused naming the owner, nothing started
    await call('/api/model', U, { provider: 'h:' + h189.id + '/sc-claude', model: 'claude-haiku-5-5' });
    const wNo = await chat(U, 'scope work r189', { fresh: true }), wNoText = wNo.events.filter((e) => e.type === 'bridge_error').map((e) => e.text).join(' | ');
    // the use switched on: the scope no longer stops it
    await scope('sc-claude', 't-user', ['claude-haiku-5-5'], ['api', 'work']);
    const wYes = await chat(U, 'scope work again r189', { fresh: true }), wYesText = wYes.events.filter((e) => e.type === 'bridge_error').map((e) => e.text).join(' | ');
    // the API without the api use
    await scope('sc-local', 't-user', ['tiny-a'], ['work']);
    const noApi = await ask('tiny-a'), noApiBody = (() => { try { return JSON.parse(noApi.text); } catch { return {}; } })();
    // "@group" (t-user is the hub's team group): applies once t-user has no entry of its own; the own entry wins again after
    await scope('sc-local', '@group', ['tiny-b'], '*'); await scope('sc-local', 't-user', '*', '*');
    const viaGroup = await sees('sc-local'), groupAsk = await ask('tiny-b'), groupNoA = await ask('tiny-a');
    await scope('sc-local', 't-user', ['tiny-a'], '*');
    const ownWins = await sees('sc-local'), ownAsk = await ask('tiny-a'), ownNoB = await ask('tiny-b');
    const db189 = (() => { try { return JSON.parse(fs.readFileSync(path.join(SB, 'ai-hosts.json'), 'utf8')).hosts[h189.id] || {}; } catch { return {}; } })();
    const hostCfg189 = JSON.parse(fs.readFileSync(f189, 'utf8'));
    const mine189 = (await call('/api/hosts', A)).data.mine || [], shown = ((mine189.find((h) => h.id === h189.id) || {}).providers || []).find((p) => p.id === 'sc-local') || {};
    const html189 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    try { proc189.kill(); } catch {}
    await call('/api/apikey/revoke', U, {}); await call('/api/model', U, { model: 'claude-opus-5-5', provider: 'host' }); await call('/api/hosts/remove', A, { id: h189.id });
    check('R189', L189,
      !!loc && loc.models.join() === 'tiny-a,tiny-b' && !loc.scope && unscopedB.status === 200
      && notOwner.status === 400 && badModel.status === 400 && /not a model/.test(JSON.stringify(badModel.data)) && badUse.status === 400 && forOwner.status === 400 && badWho.status === 400 && s1.status === 200 && s2.status === 200
      && !!locU && locU.models.join() === 'tiny-a' && locU.scope && locU.scope.uses.join() === 'api' && !!claU && claU.models.join() === 'claude-haiku-5-5' && !!ownerSees && ownerSees.models.join() === 'tiny-a,tiny-b'
      && okA.status === 200 && noB.status === 403 && /t-admin/.test(noBBody.error && noBBody.error.message) && /tiny-b/.test(noBBody.error && noBBody.error.message)
      && /t-admin/.test(wNoText) && /Work runs/.test(wNoText) && !/shares R189/.test(wYesText)
      && noApi.status === 403 && /t-admin/.test(noApiBody.error && noApiBody.error.message) && /the API/.test(noApiBody.error && noApiBody.error.message)
      && !!viaGroup && viaGroup.models.join() === 'tiny-b' && groupAsk.status === 200 && groupNoA.status === 403
      && !!ownWins && ownWins.models.join() === 'tiny-a' && ownAsk.status === 200 && ownNoB.status === 403
      && db189.scopes && db189.scopes['sc-local'] && db189.scopes['sc-local']['t-user'].models.join() === 'tiny-a' && db189.scopes['sc-local']['t-user'].uses === '*' && db189.scopes['sc-local']['@group'].models.join() === 'tiny-b' && !hostCfg189.scopes && !JSON.stringify(hostCfg189).includes('scope')
      && shown.scopes && shown.scopes['t-user'] && shown.scopes['t-user'].models.join() === 'tiny-a'
      && /\/api\/hosts\/scope/.test(html189) && /hostScopes/.test(html189) && /What each person may use/.test(html189),
      `unscoped ${unscopedB.status}; refused ${[notOwner, badModel, badUse, forOwner, badWho].map((r) => r.status).join('/')}; t-user sees local ${locU && locU.models.join()} claude ${claU && claU.models.join()} owner ${ownerSees && ownerSees.models.join()}; api ${okA.status}/${noB.status} "${String(noBBody.error && noBBody.error.message).slice(0, 120)}"; work "${wNoText.slice(0, 140)}" then "${wYesText.slice(0, 80)}"; no api ${noApi.status}; group ${viaGroup && viaGroup.models.join()} ${groupAsk.status}/${groupNoA.status}; own ${ownWins && ownWins.models.join()} ${ownAsk.status}/${ownNoB.status}; stored ${JSON.stringify(db189.scopes || {}).slice(0, 200)}`);
  } else skip('R189', 'sharing with scopes (per person: models and work / api / unity)', 'the Rust hub\'s: --rust-hub runs it');
  // Hosting controls on the hub page (R191, 5.12.0; T-160): the person who runs a host monitors it (status the host reports),
  // controls it (pause / resume / restart / update / log) and sees who is using it, from Settings → My AI hosts. The cap `control`.
  if (RUST_HUB && NATIVE_BIN.host) {
    const L191 = 'hosting controls on the hub page: a native host announces the cap `control` and reports its status (version, uptime, paused, in flight, usage windows, logins), GET /api/hosts/status lists the person\'s own hosts with it (and nobody else\'s); POST /api/hosts/control pauses it from the hub (it keeps listening, shows paused and is not offered for work), resumes it, returns its log redacted of the token, asks a restart (the host ends once nothing is in flight); refused for a non-owner (403), an unknown action (400) and a host that did not announce the cap ("older than 5.13"); [static] the page has the buttons';
    const h191 = (await call('/api/hosts/new', A, {})).data.host || {}, c191 = (await call('/api/hosts/config', A, { id: h191.id })).data.config || {};
    const old191 = (await call('/api/hosts/new', A, {})).data.host || {}, oc191 = (await call('/api/hosts/config', A, { id: old191.id })).data.config || {};
    const dir191 = path.join(SCRATCH, 'r191-host'); fs.mkdirSync(dir191, { recursive: true });
    const f191 = path.join(dir191, 'ai-node.json');
    fs.writeFileSync(f191, JSON.stringify({ hub: H, id: h191.id, token: c191.token, user: 't-admin', autoUpdate: false, engines: {}, providers: [{ id: 'r191', kind: 'claude-relay', label: 'R191 Claude', models: ['claude-haiku-5-5'], allowedUsers: [], configDir: path.join(SCRATCH, 'host-claude-login'), baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/anthropic` }] }), { mode: 0o600 });
    fs.writeFileSync(path.join(dir191, 'ai-node.log'), Array.from({ length: 60 }, (_, i) => `12:00:${String(i).padStart(2, '0')} [ai-host] line ${i + 1}${i === 58 ? ' token=' + c191.token : ''}`).join('\n') + '\n');
    const out191 = fs.openSync(path.join(dir191, 'out.log'), 'a');
    const proc191 = spawn(NATIVE_BIN.host, ['host', '--config', f191], { cwd: dir191, stdio: ['ignore', out191, out191], windowsHide: true });
    let exit191 = null; proc191.on('exit', (c) => { exit191 = c; });
    const mine191 = async () => (((await call('/api/hosts/status', A)).data || {}).hosts || []).find((x) => x.id === h191.id) || null;
    const until191 = async (pred) => { let x = null; for (let i = 0; i < 80; i++) { x = await mine191(); if (x && pred(x)) return x; await sleep(250); } return x; };
    const ctl = (action, tok = A, id = h191.id) => call('/api/hosts/control', tok, { id, action });
    const s0 = await until191((x) => x.online && x.control && x.status && x.status.version);
    const otherSees = (((await call('/api/hosts/status', U)).data || {}).hosts || []).length;
    const bad = await ctl('format-disk'), nonOwner = await ctl('pause', U), noHost = await ctl('pause', A, 'no-such-host');
    // a host that did not announce the cap (an older one: hello without caps) is refused with the clear message
    const hh = { 'Content-Type': 'application/json', 'X-VR-Host': old191.id, 'X-VR-Host-Token': oc191.token, 'X-VR-Host-Version': '3.6.0' };
    await fetch(H + '/node/hello', { method: 'POST', headers: hh, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex'), user: 't-admin', providers: [], engines: {}, version: '3.6.0' }) });
    const olderRes = await ctl('pause', A, old191.id), olderSt = (((await call('/api/hosts/status', A)).data || {}).hosts || []).find((x) => x.id === old191.id) || {};
    const pa = await ctl('pause');
    const s1 = await until191((x) => x.paused);
    const offFile = fs.existsSync(path.join(dir191, 'ai-host.off'));
    const re = await ctl('resume');
    const s2 = await until191((x) => !x.paused && x.online);
    const lg = await ctl('logs'), lines191 = (lg.data || {}).lines || [];
    const html191 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    // R204 (T-286): the host's own Work-session limits: announced in hello, set by its owner from the page, saved in its settings
    const lim0 = (s2 || {}).sessionLimits || {};
    const limSet = await call('/api/hosts/control', A, { id: h191.id, action: 'limits', limits: { perUser: 2, perProvider: 1, exceptions: { mills: 4 }, ownerBypass: false } });
    const limBad = await call('/api/hosts/control', A, { id: h191.id, action: 'limits', limits: { perUser: 0 } });
    const limOther = await call('/api/hosts/control', U, { id: h191.id, action: 'limits', limits: { perUser: 9 } });
    const s3 = await until191((x) => x.sessionLimits && x.sessionLimits.perUser === 2);
    const saved204 = (() => { try { return JSON.parse(fs.readFileSync(f191, 'utf8')).sessions || {}; } catch { return {}; } })();
    check('R204', 'T-286 a host\'s own Work-session limits: it announces them in hello (defaults 5 / 3, owner may exceed), its owner sets them from Settings → My AI hosts (POST /api/hosts/control {action: "limits"}), the host saves them in its own settings and the hub uses them; a bad value (0) is refused, and nobody else may set them',
      lim0.perUser === 5 && lim0.perProvider === 3 && lim0.ownerBypass === true && limSet.status === 200 && (limSet.data.limits || {}).perUser === 2 && limBad.status === 400 && limOther.status === 403
      && !!s3 && s3.sessionLimits.perProvider === 1 && s3.sessionLimits.exceptions.mills === 4 && s3.sessionLimits.ownerBypass === false && saved204.perUser === 2 && saved204.ownerBypass === false,
      `announced ${JSON.stringify(lim0)} · set ${limSet.status} ${JSON.stringify(limSet.data.limits || limSet.data.error || '')} · bad ${limBad.status} · not the owner ${limOther.status} · hub now ${JSON.stringify((s3 || {}).sessionLimits || {})} · saved ${JSON.stringify(saved204)}`);
    const rs = await ctl('restart');
    for (let i = 0; i < 60 && exit191 === null; i++) await sleep(250);
    try { proc191.kill(); } catch {}
    await call('/api/hosts/remove', A, { id: h191.id }); await call('/api/hosts/remove', A, { id: old191.id });
    check('R191', L191,
      !!s0 && s0.online && s0.control === true && s0.caps.includes('control') && typeof s0.status.version === 'string' && s0.status.startedAt > 0 && s0.status.paused === false && Array.isArray(s0.inUse) && s0.providers.some((p) => p.id === 'r191') && otherSees === 0
      && bad.status === 400 && nonOwner.status === 403 && noHost.status === 403
      && olderRes.status === 400 && /older than 5\.13/.test(JSON.stringify(olderRes.data)) && olderSt.control === false
      && pa.status === 200 && !!s1 && s1.paused === true && s1.online === false && s1.status.pausedBy === 'hub' && offFile
      && re.status === 200 && !!s2 && s2.paused === false && !fs.existsSync(path.join(dir191, 'ai-host.off'))
      && lg.status === 200 && lines191.length === 50 && /line 60$/.test(lines191[49]) && !JSON.stringify(lines191).includes(c191.token) && lines191.some((l) => /token=\*\*\*/.test(l))
      && rs.status === 200 && exit191 === 3
      && /\/api\/hosts\/control/.test(html191) && /hostLiveBox/.test(html191) && /Show log/.test(html191) && /Update now/.test(html191),
      `status ${s0 && JSON.stringify(s0.status).slice(0, 160)} control ${s0 && s0.control} other ${otherSees}; refused ${bad.status}/${nonOwner.status}/${noHost.status}; older ${olderRes.status} "${JSON.stringify(olderRes.data).slice(0, 100)}"; pause ${pa.status} paused ${s1 && s1.paused}/${s1 && s1.online} by ${s1 && s1.status && s1.status.pausedBy} off ${offFile}; resume ${re.status} ${s2 && s2.paused}; logs ${lg.status} ${lines191.length} lines; restart ${rs.status} exit ${exit191}; host log "${(() => { try { return fs.readFileSync(path.join(dir191, 'out.log'), 'utf8').replace(/\s+/g, ' ').slice(-160); } catch { return ''; } })()}"`);
  } else { skip('R204', 'a host\'s own Work-session limits', 'the Rust hub\'s and the native host\'s: --rust-hub with a built vr-host runs it'); skip('R191', 'hosting controls on the hub page (status, pause / resume / restart / log from Settings → My AI hosts)', 'the Rust hub\'s and the native host\'s: --rust-hub with a built vr-host runs it'); }
  // Anthropic in → Claude (the host's, the fake one here), with the Bearer form of the same kind of key
  const antKey = (await call('/api/apikey/rotate', U, {})).data.key;
  const antClaude = await v1('/v1/messages', antKey, { model: 'claude-haiku-5-5', max_tokens: 50, messages: [{ role: 'user', content: 'claude-in-r160' }] }, { 'anthropic-version': '2023-06-01' });
  const antClaudeJ = (() => { try { return JSON.parse(antClaude.text); } catch { return {}; } })(), antClaudeText = ((antClaudeJ.content || [])[0] || {}).text || '';
  await call('/api/apikey/revoke', U, {});
  const oaSrc160 = fs.readFileSync(path.join(BRIDGE, 'openai.js'), 'utf8');
  check('R160', 'protocol adapters: Anthropic → OpenAI: POST /v1/messages (system / content blocks, x-api-key header with the person\'s key) is answered by an OpenAI-compatible endpoint in Anthropic\'s message shape, streamed as Anthropic\'s events (message_start … content_block_delta … message_stop); /v1/models in Anthropic\'s list shape when anthropic-version is sent; count_tokens answers; no key → 401 and empty messages → 400 in Anthropic\'s error shape; the same key in x-api-key also opens /v1/chat/completions; Anthropic in → the host\'s Claude answers with the asked model',
    antMsg.status === 200 && antJ.type === 'message' && antJ.role === 'assistant' && /^oai says: anthropic-in-r160 \[sys 3\]/.test(((antJ.content || [])[0] || {}).text || '') && antJ.stop_reason === 'end_turn' && antJ.usage && typeof antJ.usage.input_tokens === 'number'
    && antStream.status === 200 && /text\/event-stream/.test(antStream.ct) && antEvents[0] === 'message_start' && antEvents.includes('content_block_delta') && antEvents[antEvents.length - 1] === 'message_stop' && /anthropic-stream-r160/.test(antStreamed)
    && antModels.status === 200 && Array.isArray(antModelsJ.data) && antModelsJ.data.some((m) => m.type === 'model' && m.id === 'fake-gpt') && antModelsJ.has_more === false
    && antCount.status === 200 && JSON.parse(antCount.text).input_tokens >= 2
    && antNoKey.status === 401 && /"type":"error"/.test(antNoKey.text) && /authentication_error/.test(antNoKey.text) && antEmpty.status === 400 && /invalid_request_error/.test(antEmpty.text)
    && oaiXKey.status === 200 && /xkey-r160/.test(oaiXKey.text)
    && antClaude.status === 200 && /claude-in-r160/.test(antClaudeText) && /model=claude-haiku-5-5/.test(antClaudeText)
    && /x-api-key/.test(oaSrc160),
    `messages ${antMsg.status} "${antMsg.text.slice(0, 120)}"; stream ${antStream.status} events ${antEvents.join(',')}; models ${antModels.status}; count ${antCount.status}; nokey ${antNoKey.status}; empty ${antEmpty.status}; oai x-key ${oaiXKey.status}; claude ${antClaude.status} "${antClaudeText.slice(0, 80)}"`);

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
  // R190: the notification sound patterns (T-177): each kind its own motif, a style choice kept by the hub
  {
    const sp = await call('/api/prefs', U, { sound: { set: 'classic', call: 33, board: 140 } });
    const sp2 = await call('/api/prefs', U, { sound: { set: 'loud-and-weird' } });
    const s1 = (sp.data.prefs || {}).sound || {}, s2 = (sp2.data.prefs || {}).sound || {};
    const block = (/const PATTERNS = \{([\s\S]*?)\n\};/.exec(html) || [])[1] || '';
    const kinds = ['message', 'mention', 'reply', 'call', 'board'];
    const bodies = kinds.map((k) => ((new RegExp('\\n  ' + k + ': (.+),?\\n').exec(block + '\n') || [])[1] || ''));
    check('R190', 'notification sounds: a distinct pattern per kind (message, mention, reply, call, board; no two the same), Classic kept as a choice; the hub keeps the style (only classic / patterns) and the call / board volumes (clamped); calls ring with `call`, board reminders tick with `board`',
      kinds.every((k, i) => bodies[i].length > 10) && new Set(bodies).size === kinds.length && /id="soundSet"/.test(html) && /id="volCall"/.test(html) && /id="volBoard"/.test(html)
      && /set === 'classic'/.test(html) && s1.set === 'classic' && s1.call === 33 && s1.board === 100 && s2.set === 'classic'
      && /playTone\('call'/.test(fs.readFileSync(path.join(MILLS, 'web', 'calls.js'), 'utf8')) && /playTone\('board'/.test(fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8')),
      `kinds ${bodies.map((b) => b.length).join('/')}; saved ${JSON.stringify(s1)} then ${JSON.stringify(s2)}`);
    await call('/api/prefs', U, { sound: { set: 'patterns' } });
  }
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
  const delOther = await call('/api/room/delete', D, { seq: mine.data.seq }); // a plain member (hub admins own the Mills server: they may moderate #general, R125)
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
    `woke after ${tookMs} ms; send ${lp.status} ${JSON.stringify(lp.data).slice(0, 80)}; woke with it ${(rwoke.data.messages || []).some((m) => m.seq === lp.data.seq)} (${(rwoke.data.messages || []).map((m) => m.seq).join(',')}); ver ${rwoke.data.ver}/${rv0.ver}; stale ${stale.status}`);

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
  if (RUST_HUB) { // R166 (--rust-hub only): the Rust hub is really in front, and the JavaScript one is not reachable around it
    const fr = await fetch(H + '/__vr-front').then(async (r) => ({ status: r.status, j: await r.json().catch(() => ({})), h: r.headers.get('x-vr-front') })).catch(() => ({}));
    const st = await fetch(H + '/api/status');
    const backDirect = await fetch(`http://127.0.0.1:${RUST_BACK}/api/status`).then((r) => r.status).catch(() => 'closed');
    check('R166', 'the Rust hub (vr-hub) is in front: it answers /__vr-front with its version and native modules, every hub answer passes through it (X-VR-Front), and the JavaScript hub listens behind it on its own port on 127.0.0.1 only (--rust-alone: there is no JavaScript hub at all)',
      fr.status === 200 && fr.j.front === 'vr-hub' && !!st.headers.get('x-vr-front') && (RUST_ALONE ? backDirect === 'closed' : backDirect === 200),
      `front ${fr.status} ${JSON.stringify(fr.j)}; status via front ${st.headers.get('x-vr-front')}; back ${backDirect}`);
  }
  {
    // R162: sessions kept as hashes; the page's scripts pinned by hash; every route's failure answered
    // (behind the Rust hub its auth module keeps them, in front-sessions.json, the same way)
    const hs = JSON.parse(fs.readFileSync(path.join(SB, RUST_HUB ? 'front-sessions.json' : 'hub-state.json'), 'utf8')), keys = Object.keys(hs.sessions || {});
    const tokHash = crypto.createHash('sha256').update(U).digest('hex');
    const served = await (await fetch(H + '/')).text();
    const csp = (/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/.exec(served) || [])[1] || '';
    const scriptSrc = (/script-src ([^;]*)/.exec(csp) || [])[1] || '';
    const inline = [...served.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => `'sha256-${crypto.createHash('sha256').update(m[1], 'utf8').digest('base64')}'`);
    const srv162 = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8');
    check('R162', 'security: sign-in sessions are stored as sha256 hashes (hub-state.json holds no usable token, and the signed-in token still works); the page as the hub serves it allows no inline script but its own (script-src \'self\' + the sha256 of each of its inline scripts, no \'unsafe-inline\'; worker-src blob: for the code sandbox); every request runs through handleRequest(...).catch, so a route\'s unawaited promise that fails is answered instead of hanging',
      hs.sessionsHashed === true && keys.length > 0 && !keys.includes(U) && keys.includes(tokHash) && !JSON.stringify(hs).includes(U)
      && inline.length >= 1 && !/unsafe-inline/.test(scriptSrc) && inline.every((h) => scriptSrc.includes(h)) && /worker-src blob:/.test(csp) && /'unsafe-inline'/.test(indexHtml)
      && /handleRequest\(req, res\)\.catch\(/.test(srv162) && /async function handleRequest\(req, res\)/.test(srv162),
      `hashed ${hs.sessionsHashed} keys ${keys.length} raw ${keys.includes(U)} mine ${keys.includes(tokHash)}; script-src "${scriptSrc.slice(0, 120)}" inline ${inline.length}`);
  }

  // Sign-in hashing is async now: parallel wrong guesses must still hit the 5-attempt limit.
  const burst = await Promise.all(Array.from({ length: 12 }, () => call('/api/login', null, { username: 'ghost-r65', password: 'wrong', deviceId: 'x'.repeat(32) })));
  const n401 = burst.filter((r) => r.status === 401).length, n429 = burst.filter((r) => r.status === 429).length;
  const statusDuring = await call('/api/status');
  check('R94', 'parallel wrong sign-ins: at most 5 are checked, the rest → 429; the hub stays responsive', n401 <= 5 && n401 + n429 === 12 && statusDuring.status === 200, `${n401}×401, ${n429}×429`);

  const csp = (/<meta http-equiv="Content-Security-Policy" content="([^"]+)"/.exec(html) || [])[1] || '';
  const missing66 = [
    [/default-src 'none'/.test(csp) && /connect-src 'self' http:\/\/127\.0\.0\.1:11434 https:\/\/\*\.trycloudflare\.com https:\/\/api\.github\.com ipc: http:\/\/ipc\.localhost __MILLS_HUB_HOST__;/.test(csp) && /object-src 'none'/.test(csp) && /img-src 'self' https: data: blob:/.test(csp) && !/script-src[^;]*https:/.test(csp), 'CSP (images over https only; scripts never)'],
    [/: \/\^https:\\\/\\\/\[a-z0-9-\]\+\\\.trycloudflare\\\.com\$\/;/.test(html) && /store\.set\('mills\.hub'/.test(html) && /const HUB_HOST = /.test(html), 'hub address pinned + cached (quick tunnel, or the named tunnel host injected at deploy)'],
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
    [/\.modal \.box \{ width: 100vw;/.test(html), 'full-screen dialogs on phones'],
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
    && /function renderSlash\(/.test(html) && /function runLocalSlash\(/.test(html) && /if \(message\.startsWith\('\/'\) && !picked\.length && !fromLib\.length && runLocalSlash\(message\)\)/.test(html) && /e\.key === 'ArrowDown'/.test(html) && /pickSlash\(list\[slashIdx\]\)/.test(html),
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
  // T-242: stopping a share clears only the screen flag; another person's share is shown to the others, not mixed up with mine
  const seeShare = async () => { const l = (((await call('/api/call/poll?after=0&ver=-1', A)).data.calls || {}).room || []); return Object.fromEntries(l.map((p) => [p.user, p])); };
  const uShares = await call('/api/call/state', U, { conv: 'room', cam: false, mic: true, screen: true });
  const bothSharing = await seeShare();
  const aStops = await call('/api/call/state', A, { conv: 'room', cam: true, mic: false, screen: false });
  const shareAfterStop = (await seeShare());
  await call('/api/call/state', U, { conv: 'room', cam: false, mic: true, screen: false });
  const noneSharing = await seeShare();
  check('R275', 'calls: the screen flag round-trips per person in the call state (two people can share, stopping clears only that flag, camera and mic stay)',
    camState.status === 200 && uShares.status === 200 && aStops.status === 200
    && bothSharing['t-admin'].screen === true && bothSharing['t-user'].screen === true && shareAfterStop['t-admin'].screen === false && shareAfterStop['t-admin'].cam === true && shareAfterStop['t-user'].screen === true
    && noneSharing['t-user'].screen === false && noneSharing['t-user'].mic === true, `a ${shareAfterStop['t-admin'].screen}/${shareAfterStop['t-admin'].cam}, u ${shareAfterStop['t-user'].screen}`);
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
    && uAfter.context && uAfter.context.tokens === 54000 && uAfter.context.window === 200000 && !uOther.context && !uFresh.context
    && (!RUST_HUB || (uAfter.context.cache && uAfter.context.cache.read === 45000 && uAfter.context.cache.paid === 4000 && uAfter.context.cache.write === 5000 && uAfter.context.cache.turns >= 1 && Math.abs(uAfter.context.cache.hit - 45000 / 54000) < 0.01)), // the session's prompt-cache ledger (5.7.0; the Rust hub's)
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
  const mcpRt = fs.readFileSync((RUST_ALONE ? path.join(SB, 'profiles', 't-admin', '.mcp-runtime-hub.json') : path.join(SB, '.mcp-runtime.json')), 'utf8'); // the MCP servers a Work run gets: the JavaScript hub's file, or the Rust hub's own (alone)
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
  // My Forgejo: each person's own token, checked against Forgejo, never sent back; mirroring + Work runs act as them.
  const fjMe0 = (await call('/api/forgejo/me', U)).data;
  const fjBadShape = await call('/api/forgejo/connect', U, { token: 'x' });
  const fjWrong = await call('/api/forgejo/connect', U, { token: 'not-a-real-token-123' });
  const fjOk = await call('/api/forgejo/connect', U, { token: 'fake-personal-token-1' });
  const fjMeU = await call('/api/forgejo/me', U), fjMeA = (await call('/api/forgejo/me', A)).data;
  const fjLeak = JSON.stringify(fjOk.data).includes('fake-personal-token-1') || JSON.stringify(fjMeU.data).includes('fake-personal-token-1');
  const fjMineT = (await call('/api/board/create', U, { title: 'mine r122', status: 'todo' })).data.task;
  const fjHubT = (await call('/api/board/create', A, { title: 'hub r122', status: 'todo' })).data.task;
  const fjMineI = await until(async () => [...fakeForgejo.issues.values()].find((is) => is.title === 'mine r122'));
  const fjHubI = await until(async () => [...fakeForgejo.issues.values()].find((is) => is.title === 'hub r122'));
  await call('/api/board/complete', U, { id: fjMineT.id });
  await until(async () => { const is = fjMineI && fakeForgejo.issues.get(fjMineI.number); return is && is.state === 'closed'; });
  const fjPatchBy = fakeForgejo.patchedBy;
  const fjEnvOf = (events) => ((texts(events).match(/fj=([^\s|]*)/) || [])[1] || ''), fjwOf = (events) => ((texts(events).match(/fjw=([^\s|]*)/) || [])[1] || '');
  const fjRunUEv = (await chat(U, 'forgejo env r122', { fresh: true })).events, fjRunAEv = (await chat(A, 'forgejo env r122', { fresh: true })).events;
  const fjRunU = fjEnvOf(fjRunUEv), fjRunA = fjEnvOf(fjRunAEv);
  // R172: the run's `fj` is the person's wrapper (their keys file with their token for the Forgejo host, owner-only), nobody else's
  const fjwU = fjwOf(fjRunUEv), fjwA = fjwOf(fjRunAEv);
  const fjwFiles = [path.join(SB, 'profiles', 't-user', '.fj'), path.join(SB, '.fj', 't-user')].map((d) => ({ d, keys: path.join(d, 'forgejo-cli', 'keys.json'), wrap: path.join(d, 'fj-bin', 'fj') })).filter((x) => fs.existsSync(x.keys));
  const fjwKeys = fjwFiles.length ? JSON.parse(fs.readFileSync(fjwFiles[0].keys, 'utf8')) : {};
  const fjwMode = fjwFiles.length && process.platform !== 'win32' ? (fs.statSync(fjwFiles[0].keys).mode & 0o777).toString(8) + '/' + (fs.statSync(fjwFiles[0].d).mode & 0o777).toString(8) : 'n/a';
  const fjwRuns = fjwFiles.length && process.platform !== 'win32' ? spawnSync(fjwFiles[0].wrap, ['whoami', '--host', 'x'], { encoding: 'utf8' }).stdout.trim() : '';
  const fjOff = (await call('/api/forgejo/disconnect', U, {})).data;
  const fjRunU2Ev = (await chat(U, 'forgejo env r122 after', { fresh: true })).events;
  const fjRunU2 = fjEnvOf(fjRunU2Ev), fjwU2 = fjwOf(fjRunU2Ev);
  const fjwHost = `127.0.0.1:${OLLAMA_PORT}`, fjwLeak = JSON.stringify(fjwKeys).includes('fake-personal-token-1') && !JSON.stringify(fjwKeys).includes('fake-personal-token-1'); // (the file is the person's own: their token belongs there)
  { // R193: a merge never pushes to "upstream" (a project's original, often someone else's public repo)
    const d193 = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-r193-')), g193 = (cwd, ...a) => spawnSync('git', a, { cwd, encoding: 'utf8' });
    for (const r of ['upstream', 'mirror']) g193(d193, 'init', '-q', '--bare', r + '.git');
    const w193 = path.join(d193, 'w'); fs.mkdirSync(w193); g193(w193, 'init', '-q', '-b', 'main');
    g193(w193, '-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '--allow-empty', '-m', 'x');
    for (const r of ['upstream', 'mirror']) g193(w193, 'remote', 'add', r, path.join(d193, r + '.git'));
    const out193 = require(path.join(MILLS, 'bridge', 'worktree.js')).pushAll(w193, 'main', ['origin']).join(' | ');
    const has193 = (r) => g193(path.join(d193, r + '.git'), 'rev-parse', '-q', '--verify', 'refs/heads/main').status === 0;
    const up193 = has193('upstream'), mir193 = has193('mirror');
    fs.rmSync(d193, { recursive: true, force: true });
    check('R193', 'a merge never pushes to "upstream" (publish keeps a project\'s original there only to pull from, fetch-only; our work stays private): pushAll skips it and still pushes to another mirror',
      !up193 && mir193 && /fetch-only/.test(fs.readFileSync(path.join(MILLS, 'bridge', 'worktree.js'), 'utf8')), `upstream got main ${up193}, mirror got main ${mir193} (${out193})`);
  }
  check('R172', 'fj (the Forgejo CLI) signed in as the person in their Work run: the `fj` first on the PATH is a wrapper of theirs over the real fj, pointing it at their own keys file (their token for the Forgejo host, owner-only, under their profile here / the host\'s .fj/<account>); no token → no wrapper (nobody else\'s); gone when they disconnect; [static] bridge/.fj/ never ships, worktree.js repos / repo-create / publish',
    process.platform === 'win32' || (fjwU === `wrapper:${fjwHost}=Application` && fjwA === '' && fjwFiles.length === 1 && ((fjwKeys.hosts || {})[fjwHost] || {}).token === 'fake-personal-token-1' && fjwMode === '600/700' && fjwRuns === 'fake fj whoami --host x' && fjwU2 === '' && !fjwLeak
      && /^bridge\/\.fj\/$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /cmd === 'repos'/.test(fs.readFileSync(path.join(MILLS, 'bridge', 'worktree.js'), 'utf8')) && /async function publish\(/.test(fs.readFileSync(path.join(MILLS, 'bridge', 'worktree.js'), 'utf8'))),
    `U "${fjwU}" A "${fjwA}" files ${fjwFiles.map((x) => x.d).join(',')} token ok ${((fjwKeys.hosts || {})[fjwHost] || {}).token === 'fake-personal-token-1'} mode ${fjwMode} runs "${fjwRuns}" after "${fjwU2}"`);
  const fjAnon = await fetch(H + '/api/forgejo/me');
  for (const t of [fjMineT, fjHubT]) await call('/api/board/delete', t === fjHubT ? A : U, { id: t.id });
  check('R122', 'My Forgejo: a person saves their own Forgejo token (Settings), the hub checks it with Forgejo (bad shape / wrong token refused), shows who it belongs to and never sends it back; their board changes reach Forgejo as them (others fall back to the hub token); their own Work run gets FORGEJO_TOKEN + a git extraHeader for the Forgejo host, nobody else\'s does; Remove takes it away; signed-out callers get 401; the token file is gitignored',
    fjMe0.connected === false && !!fjMe0.url && fjBadShape.status === 400 && fjWrong.status === 400 && fjOk.status === 200 && fjOk.data.login === 'tuser-fj' && fjMeU.data.connected === true && fjMeA.connected === false && !fjLeak
    && fjMineI && fjMineI.user.login === 'tuser-fj' && fjHubI && fjHubI.user.login === 'mills-hub' && fjPatchBy === 'tuser-fj'
    && fjRunU === `tuser-fj:tok:http.http://127.0.0.1:${OLLAMA_PORT}/.extraHeader` && fjRunA === '::' && fjOff.connected === false && fjRunU2 === '::' && fjAnon.status === 401
    && /^bridge\/forgejo-users\.json$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /id="fjBox"/.test(html) && /function renderForgejo\(/.test(html),
    `me0 ${JSON.stringify(fjMe0)}, bad ${fjBadShape.status}/${fjWrong.status}, ok ${fjOk.status} ${JSON.stringify(fjOk.data)}, A ${fjMeA.connected}, leak ${fjLeak}, issues by ${fjMineI && fjMineI.user.login}/${fjHubI && fjHubI.user.login}, patch by ${fjPatchBy}, runs U "${fjRunU}" A "${fjRunA}" after "${fjRunU2}", anon ${fjAnon.status}`);
  const polish = fs.readFileSync(path.join(MILLS, 'web', 'polish.css'), 'utf8'), swJs = fs.readFileSync(path.join(MILLS, 'web', 'sw.js'), 'utf8');
  const polishRes = await fetch(H + '/polish.css'), polishType = polishRes.headers.get('content-type') || '';
  const fixedColours = [...polish.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(/#[0-9a-f]{3,8}\b/gi)].map((m) => m[0].toLowerCase()).filter((c) => !['#fff', '#000', '#ff6ad5'].includes(c)); // only mixing helpers, never a fixed hue
  check('R129', 'look and feel: web/polish.css (served as text/css, linked after the page styles, cached by the app) gives depth, gradients, glass and motion built only from the theme variables; menu tiles with icons and descriptions; the gradient Mills mark (sprite hidden at zero size so gradients paint); new icons in use (hash, megaphone, lock, users, mail, link, pin, crown, globe, clock, user, plug, logout, sparkle, shield)',
    polishRes.status === 200 && /text\/css/.test(polishType) && /<link rel="stylesheet" href="polish\.css">\s*<\/head>/.test(html) && /'polish\.css'/.test(swJs) && !fixedColours.length
    && /var\(--accent\)/.test(polish) && /prefers-reduced-motion/.test(polish) && /button:where\(:not\(\.ghost\)/.test(polish)
    && /class="tile" id="goAssist"><svg class="ic lead"><use href="#i-sparkle"\/><\/svg>Unity<small>/.test(html) && (html.match(/class="tile"/g) || []).length >= 7 && /class="brandMark"/.test(html) && /<symbol id="i-mills"/.test(html) && /stop-color: var\(--accent\)/.test(html) && !/<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg" style="display:none">/.test(html)
    && ['hash', 'megaphone', 'mail', 'link', 'pin', 'crown', 'globe', 'clock', 'user', 'plug', 'logout', 'sparkle', 'shield'].every((n) => new RegExp(`(href="#i-${n}"|ic\\('${n}'|'${n}' :|\\? '${n}'|'${n}')`).test(html)),
    `served ${polishRes.status} ${polishType}, fixed colours ${fixedColours.join(' ') || 'none'}`);
  // Hub law 11: every site change includes the Windows / Linux app (Mills Chat = the page in app mode, installed).
  const indexHtml131 = () => fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
  const pageFiles = [...indexHtml131().matchAll(/<(?:script src|link rel="stylesheet" href)="([\w./-]+)"/g)].map((m) => m[1]).filter((f) => !/^vendor\//.test(f));
  const swShell = (/const OWN = \[([^\]]*)\]/.exec(swJs) || [, ''])[1];
  const notCached = pageFiles.filter((f) => !swShell.includes(`'${f}'`));
  const appCss = indexHtml131().split('\n').filter((l) => /body\.appMode/.test(l)).join('\n');
  const appCssHex = (appCss.replace(/\/\*[\s\S]*?\*\//g, '').match(/#[0-9a-f]{3,8}\b/gi) || []);
  const appJs131 = fs.readFileSync(path.join(MILLS, 'web', 'app.js'), 'utf8');
  check('R131', '[static] law 11, the Windows / Linux app goes with every site change: every page script + stylesheet is in the app\'s service-worker shell, app-mode styles use no hard-coded colours (they follow the themes), the app has its own way into Settings + Sign out (user panel, since app mode hides the headers), the app keeps up with renders (servers + conversation changes), the README describes the app',
    !notCached.length && !appCssHex.length && /function renderAppUser\(/.test(appJs131) && /openSettings\(\)/.test(appJs131) && /signOut\(\)/.test(appJs131) && /renderAppUser\(\)/.test(fs.readFileSync(path.join(MILLS, 'web', 'guilds.js'), 'utf8')) && /renderAppUser\(\); \/\/ the app's user panel/.test(html)
    && /VibeRoller Chat/.test(fs.readFileSync(path.join(MILLS, 'README.md'), 'utf8')) && /user panel/.test(fs.readFileSync(path.join(MILLS, 'README.md'), 'utf8')),
    `page files ${pageFiles.join(' ')}; not in the shell: ${notCached.join(' ') || 'none'}; app-mode hex: ${appCssHex.join(' ') || 'none'}`);
  // Hub law 14: semver for every module and the project; a module changed against production moves up.
  const SEMVER = /^(\d+)\.(\d+)\.(\d+)(?:-[\w.]+)?$/;
  const semCmp = (a, b) => { const x = SEMVER.exec(a), y = SEMVER.exec(b); for (let i = 1; i <= 3; i++) { const d = Number(x[i]) - Number(y[i]); if (d) return d; } return 0; };
  const readVersions = (root) => { try { return JSON.parse(fs.readFileSync(path.join(root, 'versions.json'), 'utf8')); } catch { return null; } };
  const V = readVersions(MILLS) || { project: '', modules: {} }, prodRoot = path.join(PROD, '..'), PV = prodRoot !== MILLS ? readVersions(prodRoot) : null;
  const modsOnDisk = [...fs.readdirSync(BRIDGE).filter((f) => f.endsWith('.js')).map((f) => 'bridge/' + f), ...fs.readdirSync(path.join(MILLS, 'web')).filter((f) => /\.(js|css)$/.test(f) || f === 'index.html').map((f) => 'web/' + f), ...fs.readdirSync(path.join(MILLS, 'web', 'editor')).filter((f) => /\.js$/.test(f)).map((f) => 'web/editor/' + f)];
  const unlisted = modsOnDisk.filter((m) => !(m in V.modules)), ghosts = Object.keys(V.modules).filter((m) => !modsOnDisk.includes(m));
  const badSem = [V.project, ...Object.values(V.modules)].filter((s) => !SEMVER.test(String(s)));
  const eolFree = (b) => b.toString('latin1').replace(/\r\n/g, '\n'); // .bat / .cmd / .ps1 are checked out with CRLF (.gitattributes): line endings are not a change
  const same = (rel) => { try { return eolFree(fs.readFileSync(path.join(MILLS, rel))) === eolFree(fs.readFileSync(path.join(prodRoot, rel))); } catch { return false; } };
  const notBumped = [], changedMods = [];
  if (PV && PV.modules) for (const m of Object.keys(V.modules)) {
    if (!(m in PV.modules)) { changedMods.push(m); continue; } // new module (starts at 1.0.0)
    if (!same(m)) { changedMods.push(m); if (!(SEMVER.test(V.modules[m]) && SEMVER.test(PV.modules[m]) && semCmp(V.modules[m], PV.modules[m]) > 0)) notBumped.push(`${m} ${PV.modules[m]}→${V.modules[m]}`); }
  }
  const projectMoved = !PV || (changedMods.length ? SEMVER.test(V.project) && SEMVER.test(PV.project) && semCmp(V.project, PV.project) > 0 : true);
  const changelog = fs.existsSync(path.join(MILLS, 'CHANGELOG.md')) ? fs.readFileSync(path.join(MILLS, 'CHANGELOG.md'), 'utf8') : '';
  const hasSection = new RegExp('^## ' + V.project.replace(/\./g, '\\.') + ' — \\d{4}-\\d{2}-\\d{2}$', 'm').test(changelog);
  const ckV = (await call('/api/check', U)).data;
  check('R132', 'law 14, semver: versions.json has a valid project version and one per module (every bridge/*.js, web/*.js, web/*.css, web/index.html; none missing, none extra); every module that differs from the production checkout has a higher version there, and the project moved up when anything did; CHANGELOG.md has a dated section for the project version; /api/check and Settings show it',
    SEMVER.test(V.project) && !badSem.length && !unlisted.length && !ghosts.length && !notBumped.length && projectMoved && hasSection
    && ckV.version === V.project && ckV.versions && ckV.versions['bridge/server.js'] === V.modules['bridge/server.js'] && /id="aboutVibeRoller"/.test(html) && /function renderAbout\(/.test(html),
    `project ${V.project}${PV ? ' (production ' + PV.project + ')' : ' (production has no versions.json yet: baseline)'}, ${Object.keys(V.modules).length} modules; changed vs production: ${changedMods.join(' ') || 'none'}; not bumped: ${notBumped.join(', ') || 'none'}; unlisted: ${unlisted.join(' ') || 'none'}; extra: ${ghosts.join(' ') || 'none'}; bad: ${badSem.join(' ') || 'none'}; changelog section ${hasSection}`);
  // Hub law 15: every change to any tracked file is logged; the newest changelog section is the current version.
  const trackedFiles = execFileSync('git', ['ls-files'], { cwd: MILLS, encoding: 'utf8', windowsHide: true }).split('\n').map((s) => s.trim()).filter(Boolean).filter((f) => !['CHANGELOG.md', 'versions.json'].includes(f));
  const changedFiles = PV ? trackedFiles.filter((f) => !same(f)) : [];
  const topHead = (/^## (.+)$/m.exec(changelog) || [, ''])[1];
  const topOk = new RegExp('^' + V.project.replace(/\./g, '\\.') + ' — \\d{4}-\\d{2}-\\d{2}$').test(topHead);
  const logged = !changedFiles.length || (SEMVER.test(V.project) && SEMVER.test(PV.project) && semCmp(V.project, PV.project) > 0);
  check('R133', 'law 15, the changelog: every tracked file that differs from the production checkout (code, page, README, tests, workflows …) comes with a higher project version than production\'s, and the newest CHANGELOG.md section is that version with its date',
    topOk && logged && /^### (Added|Changed|Fixed|Removed|Security)$/m.test(changelog),
    `top section "${topHead}", project ${V.project}${PV ? ' vs production ' + PV.project : ''}; changed vs production: ${changedFiles.slice(0, 12).join(' ') || 'none'}${changedFiles.length > 12 ? ' …' : ''}`);
  check('R124', '[static] themed scrollbars: thumb + hover built from the theme variables (accent + line), standard scrollbar-width / scrollbar-color plus the ::-webkit-scrollbar fallback, no hard-coded scrollbar colours',
    /--sb-thumb: color-mix\(in srgb, var\(--accent\)/.test(html) && /scrollbar-color: var\(--sb-thumb\) var\(--sb-track\)/.test(html) && /scrollbar-width: thin/.test(html)
    && /::-webkit-scrollbar-thumb \{ background: var\(--sb-thumb\)/.test(html) && /::-webkit-scrollbar-thumb:hover \{ background: var\(--sb-hover\)/.test(html) && !/scrollbar-color: #/.test(html));
  const runJs121 = fs.readFileSync(path.join(BRIDGE, 'run.js'), 'utf8'), pagesYml = fs.readFileSync(path.join(MILLS, '.github', 'workflows', 'pages.yml'), 'utf8');
  check('R121', '[static] named Cloudflare tunnel, prepared: run.js runs `cloudflared tunnel run <tunnelName>` and publishes https://<tunnelHostname> when configured (quick tunnel otherwise); the page accepts the deploy-time MILLS_HUB_HOST besides *.trycloudflare.com (CSP connect-src + hub address check); pages.yml injects it (empty = quick tunnels only)',
    /config\.tunnelName/.test(runJs121) && /'run', '--url'/.test(runJs121) && /config\.tunnelHostname/.test(runJs121) && /publishUrl\('https:\/\/' \+ host\)/.test(runJs121)
    && /__MILLS_HUB_HOST__; img-src/.test(csp) && /const HUB_HOST = /.test(html) && /HUB_HOST && location\.origin === HUB_HOST/.test(html)
    && /MILLS_HUB_HOST/.test(pagesYml) && /__MILLS_HUB_HOST__#\$HUBHOST#g/.test(pagesYml) && /https:\/\/\*\)/.test(pagesYml));
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
  const callsJs = fs.readFileSync(path.join(MILLS, 'web', 'calls.js'), 'utf8');
  check('R276', '[static] screen share: an audio choice first (none / tab-system audio / plus the microphone), getDisplayMedia asks for audio only when chosen, the mic is mixed in with an AudioContext into one track',
    /id="shareOpts"/.test(html) && /name="shareAudio" value="none" checked/.test(html) && /value="sys"/.test(html) && /value="sysmic"/.test(html) && /id="shareGo"/.test(html)
    && /audio: choice !== 'none'/.test(callsJs) && /createMediaStreamDestination\(\)/.test(callsJs) && /createMediaStreamSource\(new MediaStream\(\[call\.mic\]\)\)/.test(callsJs) && /gave no tab \/ system audio/.test(callsJs));
  check('R277', '[static] screen share: the share\'s sound is a 4th sender added inactive and turned on only for pages that signal sa (older pages never see it), its renegotiation comes from the offerer only, and it plays in the screen tile',
    /pc\.addTransceiver\('audio', \{ direction: 'inactive', streams: \[call\.screenStream\] \}\)/.test(callsJs) && /sa: 1, \.\.\.body/.test(callsJs) && /msg\.sa && peer\.offerer && msg\.d\.type === 'answer'/.test(callsJs)
    && /isScreenAudio\(pc, e\.transceiver\)/.test(callsJs) && /isScreenAudio\(peer\.pc, t\) \? call\.screenAudio : call\.mic/.test(callsJs) && /if \(!self\) applyPersonAudio\(t\)/.test(callsJs));
  check('R278', '[static] screen share: who is sharing shown in the call bar and on the tiles, stop also from the browser\'s own Stop sharing (track ended) and on leaving, all share audio released',
    /sharing a screen/.test(callsJs) && /call\.screen\.onended = \(\) => stopScreen\(\)/.test(callsJs) && /function dropShareAudio\(\)/.test(callsJs) && /call\.screen = null; dropShareAudio\(\)/.test(callsJs) && /\n  dropShareAudio\(\);\n  call\.mic = call\.cam/.test(callsJs)
    && /if \(p\.screen\) l\.append\(ic\('share', 'trail'\)\)/.test(callsJs) && /screen: !!call\.screen/.test(callsJs));
  check('R279', '[static] screen share: click a shared screen to enlarge it (big view, click again or Esc to shrink), and the browser differences are written down in calls.js',
    /t\.onclick = \(\) => t\.classList\.toggle\('big'\)/.test(callsJs) && /\.tile\.screenTile\.big \{ position: fixed/.test(html) && /querySelector\('\.screenTile\.big'\)/.test(callsJs)
    && /Chrome \/ Edge \(Linux\): audio only for a TAB/.test(callsJs) && /PipeWire/.test(callsJs) && /Firefox: screen \/ window \/ tab video; no tab or system audio/.test(callsJs) && /Safari/.test(callsJs));
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
    && /team-mcp\.js/.test(fs.readFileSync((RUST_ALONE ? path.join(SB, 'profiles', 't-admin', '.mcp-runtime-hub.json') : path.join(SB, '.mcp-runtime.json')), 'utf8')) && /mcp__team/.test(fs.readFileSync(path.join(SB, 'config.json'), 'utf8'))
    && /const authorOf = \(m\) => m\.as \|\| [^\n]*nickOf\(m\.from\)/.test(html), // the "<person> · Claude" label wins over any (server) nickname
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

  // R158: attachments. Chat: ciphertext uploaded first, then the message naming it; only its readers fetch it. Work: the
  // person's own uploads, which their Claude reads. Plus the page side (what shows how) and the auto-scroll setting.
  {
    const raw = (tok, p, body, headers = {}) => fetch(H + p, { method: body === undefined ? 'GET' : 'POST', headers: { Authorization: 'Bearer ' + tok, ...headers }, body });
    const ct = crypto.randomBytes(3000); // stands for an encrypted file: the hub only ever sees bytes like these
    const up = await raw(A, '/api/room/file', ct, { 'Content-Type': 'application/octet-stream' }), upId = (await up.json().catch(() => ({}))).id || '';
    const beforeSendOther = (await raw(U, '/api/room/file/' + upId)).status; // not sent yet: only its uploader
    const sentF = await call('/api/room/send', A, { ...(await sealConv(A, privA, 't-admin', 'a file r158', grpConv)), files: [upId] });
    const getU = await raw(U, '/api/room/file/' + upId), getUBuf = Buffer.from(await getU.arrayBuffer());
    const getD = (await raw(D, '/api/room/file/' + upId)).status; // not in that group
    const again = await call('/api/room/send', A, { ...(await sealConv(A, privA, 't-admin', 'same file again', grpConv)), files: [upId] });
    const upU = await raw(U, '/api/room/file', crypto.randomBytes(10), { 'Content-Type': 'application/octet-stream' }), upUId = (await upU.json().catch(() => ({}))).id;
    const stolen = await call('/api/room/send', A, { ...(await sealConv(A, privA, 't-admin', 'not mine', grpConv)), files: [upUId] });
    await call('/api/room/delete', A, { seq: sentF.data.seq });
    const afterDelete = (await raw(U, '/api/room/file/' + upId)).status, onDisk = fs.existsSync(path.join(SCRATCH, 'Chat', 'files', upId));
    const tooBig = await raw(A, '/api/room/file', Buffer.alloc(24 * 1024 * 1024 + 100), { 'Content-Type': 'application/octet-stream' }).then((r) => r.status).catch((e) => 'ERR ' + ((e.cause && (e.cause.code || e.cause.message)) || e.message));
    // Work: upload, name made safe, served back to its owner only (never as a type a browser would run), read by Claude
    const wUp = await raw(U, '/api/work/file', Buffer.from('# Notes\nhello r158\n'), { 'Content-Type': 'text/markdown', 'X-File-Name': encodeURIComponent('../../evil name.md') });
    const wMeta = await wUp.json().catch(() => ({}));
    const wGet = await raw(U, '/api/work/file/' + wMeta.id), wGetText = await wGet.text();
    const wOther = (await raw(A, '/api/work/file/' + wMeta.id)).status;
    const marker = '\n\n<!--vr-files ' + JSON.stringify([{ id: wMeta.id, n: wMeta.name, m: wMeta.mime, s: wMeta.size }]) + ' -->';
    const wRun = texts((await chat(U, 'look at my notes r158' + marker, { fresh: true })).events);
    const wOnly = texts((await chat(U, marker.trim(), { fresh: true })).events); // files only, no text
    const prefOn = await call('/api/prefs', U, { autoScroll: true }), prefOff = await call('/api/prefs', U, { autoScroll: false });
    const att = fs.readFileSync(path.join(MILLS, 'web', 'attach.js'), 'utf8'), page158 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R158', 'attachments: in the chat a file is encrypted in the browser and only its ciphertext uploaded; until it is sent only its uploader may fetch it; the message naming it makes it readable to exactly that message\'s readers (others 404), it can be sent once, never with someone else\'s file, and deleting the message deletes it; over 24 MB → 413. In Work a file goes to the person\'s own uploads (its name made safe), comes back only to them as an attachment (octet-stream + its real type in a header), and their Claude gets its path to read (text optional). The page: a + button and a tray on both message boxes (drop / paste too), pictures and video shown, text / markdown / code viewed with highlighting, anything else name + Download only (text only if no NUL byte and valid UTF-8); auto-scroll is a setting (off by default) that scrolling up switches off, with a "Newest" button',
      up.status === 200 && /^[a-f0-9]{24}$/.test(upId) && beforeSendOther === 404 && sentF.status === 200 && getU.status === 200 && getUBuf.equals(ct) && getD === 404
      && again.status === 400 && stolen.status === 400 && afterDelete === 404 && !onDisk && tooBig === 413
      && wUp.status === 200 && wMeta.name === 'evil name.md' && wMeta.mime === 'text/markdown' && wGet.status === 200 && wGet.headers.get('content-type') === 'application/octet-stream' && wGet.headers.get('x-vr-file-type') === 'text/markdown' && /hello r158/.test(wGetText) && wOther === 404
      && /Attached by the user: 1 file/.test(wRun) && /uploaded_files[\\/]\d{4}-\d\d-\d\d[\\/]evil name\.md/.test(wRun) && /look at my notes r158/.test(wRun) && /adddir=\S*uploaded_files/.test(wRun) && /no text: see the attached files/.test(wOnly)
      && prefOn.data.prefs.autoScroll === true && prefOff.data.prefs.autoScroll === false
      && /if \(b\.includes\(0\)\) return null/.test(att) && /fatal: true/.test(att) && /return 'binary'/.test(att) && /MD\.render\(body, fence \+ lang/.test(att) && /Not a picture, video or text/.test(att)
      && /id="roomAttach"/.test(page158) && /id="workAttach"/.test(page158) && /sealChatFiles/.test(page158) && /additionalData: FILE_AAD/.test(page158) && /<!--vr-files /.test(page158) && /id="setAutoScroll"/.test(page158) && /if \(up && prefs\(\)\.autoScroll\) setAutoScroll\(false\)/.test(page158) && /className = 'jumpNew'/.test(page158),
      `chat up ${up.status} pre ${beforeSendOther} send ${sentF.status} U ${getU.status} same ${getUBuf.equals(ct)} D ${getD} again ${again.status} stolen ${stolen.status} deleted ${afterDelete} disk ${onDisk} big ${tooBig}; work up ${wUp.status} ${JSON.stringify(wMeta)} get ${wGet.status} ${wGet.headers.get('x-vr-file-type')} other ${wOther}; run "${wRun.slice(0, 160)}"`);

    // R159: Files (everyone's library) and the link to their Linux account on the server
    const libU = (await call('/api/library', U)).data, libA = (await call('/api/library', A)).data;
    const wEntry = (libU.entries || []).find((e) => e.id === wMeta.id) || {};
    const onLinkedDisk = fs.readdirSync(path.join(FAKE_LINUX, 'home', 't-user', 'uploaded_files'), { recursive: true }).some((f) => String(f).endsWith('evil name.md'));
    // a chat file with its uploader's own seal: the message goes, the uploader keeps it (others lose it)
    const seal = 'aaaaaaaaaaaaaaaa.' + 'b'.repeat(40);
    const upS = await raw(A, '/api/room/file', crypto.randomBytes(500), { 'Content-Type': 'application/octet-stream', 'X-VR-Own-Seal': seal }), sId = (await upS.json().catch(() => ({}))).id;
    const sSent = await call('/api/room/send', A, { ...(await sealConv(A, privA, 't-admin', 'kept r159', grpConv)), files: [sId] });
    const sByU1 = (await raw(U, '/api/room/file/' + sId)).status;
    await call('/api/room/delete', A, { seq: sSent.data.seq });
    const sByU2 = (await raw(U, '/api/room/file/' + sId)).status, sByA = (await raw(A, '/api/room/file/' + sId)).status;
    const sEntry = ((await call('/api/library', A)).data.entries || []).find((e) => e.id === sId) || {};
    const sDel = await call('/api/library/delete', A, { id: sId }), sAfter = (await raw(A, '/api/room/file/' + sId)).status;
    const notMine = await call('/api/library/delete', A, { id: wMeta.id }); // someone else's file: not in their library
    const wDel = await call('/api/library/delete', U, { id: wMeta.id });
    const wGone = !fs.readdirSync(path.join(FAKE_LINUX, 'home', 't-user', 'uploaded_files'), { recursive: true }).some((f) => String(f).endsWith('evil name.md')), wServe = (await raw(U, '/api/work/file/' + wMeta.id)).status;
    const linkSh = fs.readFileSync(path.join(MILLS, 'deploy', 'link-linux-account.sh'), 'utf8'), lib159 = fs.readFileSync(path.join(MILLS, 'web', 'library.js'), 'utf8'), page159 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R159', 'Files: everyone\'s uploads in one library: a person linked to their Linux account on the server gets their Work uploads in ~/uploaded_files/<date>/ (theirs to work on), the library lists each with when, name / type / size, where it went (Work · project) and when it was last used; someone without a linked account keeps them in VibeRoller\'s area; a chat file carries its uploader\'s own seal (only their chat key opens its name and key), so when its message is deleted its readers lose it and the uploader still has it (marked "message deleted"), until they delete it from the library, for good; nobody deletes someone else\'s; [static] the link script needs no root and grants the hub that one folder only (ACLs), unlink takes it back; the Files screen opens, downloads, deletes, copies a chat file into uploaded_files, and shows the link status',
      libU.linux && libU.linux.linked === true && libU.linux.linuxUser === 't-user' && /uploaded_files$/.test(libU.linux.folder) && libA.linux && libA.linux.linked === false && onLinkedDisk
      && wEntry.kind === 'work' && wEntry.name === 'evil name.md' && wEntry.size > 0 && wEntry.at > 0 && wEntry.lastAccess > 0 && (wEntry.uses || []).some((x) => x.where === 'work' && x.project === 'VibeRoller') && /uploaded_files[\\/]\d{4}-\d\d-\d\d$/.test(wEntry.folder || '') && !wEntry.path
      && upS.status === 200 && sSent.status === 200 && sByU1 === 200 && sByU2 === 404 && sByA === 200 && sEntry.seal === seal && sEntry.where && sEntry.where.deleted > 0 && sEntry.where.conv === grpConv
      && sDel.status === 200 && sAfter === 404 && notMine.status === 404 && wDel.status === 200 && wGone && wServe === 404
      && /setfacl -m "u:\$HUB:x" "\$HOME"/.test(linkSh) && /setfacl -R -d -m/.test(linkSh) && /unlink/.test(linkSh) && !/sudo/.test(linkSh)
      && /function openOwn/.test(page159) && /'X-VR-Own-Seal': await sealOwn\(info\)/.test(page159) && /id="goFiles"/.test(page159) && /LIBRARY\.show\(\)/.test(page159)
      && /Copy to my uploaded_files/.test(lib159) && /\/api\/library\/delete/.test(lib159) && /ATTACH\.open\(it/.test(lib159)
      // the hub's own data never shows as a change in the live checkout (the auto-deploy refuses a checkout with changes)
      && ['bridge/uploads-index.json', 'bridge/uploads/x', 'bridge/ai-host-sessions.json', 'bridge/hub-signing.key', 'bridge/ai-node.status.json', 'Chat/files/x'].every((f) => spawnSync('git', ['check-ignore', '-q', f], { cwd: MILLS }).status === 0),
      `U linux ${JSON.stringify(libU.linux)}; A linked ${libA.linux && libA.linux.linked}; on disk ${onLinkedDisk}; work entry ${JSON.stringify({ k: wEntry.kind, n: wEntry.name, la: !!wEntry.lastAccess, uses: (wEntry.uses || []).length, f: wEntry.folder })}; chat U ${sByU1}/${sByU2} A ${sByA} entry ${JSON.stringify(sEntry.where || {})} del ${sDel.status} after ${sAfter}; notMine ${notMine.status}; work del ${wDel.status} gone ${wGone} serve ${wServe}`);
    // R161: edit, execute, files put in uploaded_files outside VibeRoller, files sent to Work from the library
    {
      const up = await (await raw(U, '/api/work/file', Buffer.from('echo one\n'), { 'Content-Type': 'text/x-shellscript', 'X-File-Name': 'run-r161.sh' })).json().catch(() => ({}));
      const saved = await raw(U, '/api/library/save?id=' + up.id, Buffer.from('echo two r161\n'), { 'Content-Type': 'application/octet-stream' });
      const back = await (await raw(U, '/api/work/file/' + up.id)).text();
      const saveOther = (await raw(A, '/api/library/save?id=' + up.id, Buffer.from('x'), { 'Content-Type': 'application/octet-stream' })).status;
      const saveBin = (await raw(U, '/api/library/save?id=' + up.id, Buffer.from([1, 0, 2]), { 'Content-Type': 'application/octet-stream' })).status;
      const exOn = await call('/api/library/exec', U, { id: up.id, on: true }), exOff = await call('/api/library/exec', U, { id: up.id, on: false }), exOther = await call('/api/library/exec', A, { id: up.id, on: true });
      const handDir = path.join(FAKE_LINUX, 'home', 't-user', 'uploaded_files', 'by-hand'); fs.mkdirSync(handDir, { recursive: true }); fs.writeFileSync(path.join(handDir, 'made-over-ssh-r161.py'), 'print(1)\n');
      const lib = (await call('/api/library', U)).data.entries || [], hand = lib.find((e) => e.name === 'made-over-ssh-r161.py') || {}, mine = lib.find((e) => e.id === up.id) || {};
      const handGet = await (await raw(U, '/api/work/file/' + hand.id)).text().catch(() => '');
      const handRun = texts((await chat(U, 'use it r161\n\n<!--vr-files ' + JSON.stringify([{ id: hand.id, n: hand.name, m: hand.mime, s: hand.size }]) + ' -->', { fresh: true })).events);
      const handDel = await call('/api/library/delete', U, { id: hand.id }), handGone = !fs.existsSync(path.join(handDir, 'made-over-ssh-r161.py'));
      await call('/api/library/delete', U, { id: up.id });
      const lib161 = fs.readFileSync(path.join(MILLS, 'web', 'library.js'), 'utf8'), page161 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
      check('R161', 'Files, complete: a Work file is edited in place (saved back, the owner only, text only), made executable and back (chmod, the owner only); a file put in uploaded_files outside VibeRoller (over SSH, by a Work run) shows in the library (type from its name, "disk"), opens, goes to Work as it is (its path handed to Claude) and deletes; [static] the Files screen has Edit (Ctrl+S), Executable, Use in Work / Run in Work, and Work sends library files with the message',
        saved.status === 200 && /echo two r161/.test(back) && saveOther === 404 && saveBin === 400
        && exOn.status === 200 && exOn.data.exec === true && exOff.data.exec === false && exOther.status === 404 && mine.size === 14 && mine.edited > 0
        && hand.disk === true && hand.mime === 'text/x-python' && /by-hand$/.test(hand.folder || '') && /print\(1\)/.test(handGet) && /by-hand[\\/]made-over-ssh-r161\.py/.test(handRun) && handDel.status === 200 && handGone
        && /function edit\(it\)/.test(lib161) && /\/api\/library\/save\?id=/.test(lib161) && /\/api\/library\/exec/.test(lib161) && /Run in Work/.test(lib161) && /function setWorkLibFiles/.test(page161) && /\.\.\.fromLib/.test(page161),
        `save ${saved.status} back "${back.trim()}" other ${saveOther} bin ${saveBin}; exec ${exOn.status}/${exOn.data && exOn.data.exec}/${exOff.data && exOff.data.exec} other ${exOther.status}; mine ${JSON.stringify({ s: mine.size, e: !!mine.edited })}; hand ${JSON.stringify({ d: hand.disk, m: hand.mime, f: hand.folder })} get "${handGet.slice(0, 20)}" run "${handRun.slice(0, 120)}" del ${handDel.status} gone ${handGone}`);
    }
  }
  const grpText = grpForU ? await read4(restored, grpForU).catch(() => null) : null;
  const addedInv = await call('/api/guilds/server/add', U, { server: gSid, add: ['t-admin'] }); // an invitation now…
  const added = addedInv.status === 200 ? await call('/api/guilds/invitation/accept', A, { server: gSid }) : addedInv; // …that they accept
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
    g0.servers.filter((s) => s.id !== 'seedtest').length === 1 && g0.servers[0].id === 'mills' && g0.servers[0].channels[0].conv === 'room' && gSv.status === 200 && !!gSid && chn.status === 200 && grp.status === 200 && !!grpConv
    && !aView.servers.some((s) => s.id === gSid) && aView.groups.some((g) => g.conv === grpConv)
    && chSent.status === 200 && chDisk.conv === chConv && Object.keys(chDisk.keys || {}).sort().join() === 't-user' && !chSeenByA
    && chOutsider.status === 400 && gBadConv.status === 400 && tyOutsider.status === 400 && tyMember.status === 200 && seenMember.status === 200 && seenOutsider.status === 400 && callOutsider.status === 400 && callMember.status === 200
    && grpSent.status === 200 && grpForU && grpText && grpText.t === 'group hello-r113' && !grpForD
    && added.status === 200 && aViewAfter.servers.some((s) => s.id === gSid) && chSent2.status === 200 && chText2 && chText2.t === 'now A sees-r113'
    && !!ansCh && ansChMsg && ansChMsg.conv === chConv && teamCh.status === 200
    && gDelOther.status === 400 && gDelOwn.status === 200 && gLeave.status === 200 && !gAfterLeave.groups.some((g) => g.conv === grpConv) && rmSv.status === 200,
    `server ${gSv.status}, channel ${chn.status}, send ${chSent.status} slots ${Object.keys(chDisk.keys || {}).join('+')}, A sees ${chSeenByA}, outsider ${chOutsider.status}/${tyOutsider.status}/${seenOutsider.status}/${callOutsider.status}, group ${grpSent.status} U "${grpText && grpText.t}" D ${grpForD}, after add "${chText2 && chText2.t}", unity ${!!ansCh} in ${ansChMsg && ansChMsg.conv}, delete ${gDelOther.status}/${gDelOwn.status}`);

  // Servers, Discord-shaped: invitations, roles + ranks, invite links (expiry / uses / temporary), password lock,
  // kick / ban, private + announcement channels, nicknames, audit log, ownership, the editable Mills server, groups.
  const gq = (tok) => call('/api/guilds', tok).then((r) => r.data);
  const inviteLink23 = (code) => 'https://example.invalid/Mills/?invite=' + code; // a pasted link works like the bare code
  const sv23 = (v, id) => (v.servers || []).find((s) => s.id === id);
  const mk23 = await call('/api/guilds/server', U, { name: 'Friends r123', color: '#123456', description: 'just us', members: ['t-admin'] });
  const s23 = mk23.data.result && mk23.data.result.id;
  const aInv = await gq(A);
  const aBefore = !sv23(aInv, s23) && (aInv.invitations || []).some((i) => i.server === s23);
  const acc23 = await call('/api/guilds/invitation/accept', A, { server: s23 });
  const updNo = await call('/api/guilds/server/update', A, { server: s23, name: 'hijack' });
  const updOk = await call('/api/guilds/server/update', U, { server: s23, name: 'Friends plus', description: 'just us r123' });
  const roleM = (await call('/api/guilds/role', U, { server: s23, name: 'Mods', color: '#ff0000', perms: ['KICK', 'CREATE_CHANNELS', 'NOT_A_PERM'] })).data.result;
  const roleI = (await call('/api/guilds/role', U, { server: s23, name: 'Inner', perms: [] })).data.result;
  const giveM = await call('/api/guilds/member/roles', U, { server: s23, user: 't-admin', roles: [roleM.id] });
  const kickOwner = await call('/api/guilds/member/kick', A, { server: s23, user: 't-user' });
  const pw23 = await call('/api/guilds/server/password', U, { server: s23, password: 'hunter-r123' });
  const iv23 = (await call('/api/guilds/invite', U, { server: s23, maxAge: 60, maxUses: 1 })).data.result;
  const leak23 = /"(salt|hash)"/.test(JSON.stringify(await gq(U))) || /"(salt|hash)"/.test(JSON.stringify(pw23.data));
  const pv23 = (await call('/api/guilds/invite/preview', D, { code: inviteLink23(iv23.code) })).data.result;
  const joinNoPw = await call('/api/guilds/invite/join', D, { code: iv23.code });
  const joinBadPw = await call('/api/guilds/invite/join', D, { code: iv23.code, password: 'nope' });
  const joinOk = await call('/api/guilds/invite/join', D, { code: iv23.code, password: 'hunter-r123' });
  const used23 = await call('/api/guilds/invite/preview', D, { code: iv23.code }); // max 1 use: gone
  const dIn = !!sv23(await gq(D), s23);
  const kickD = await call('/api/guilds/member/kick', A, { server: s23, user: 'mills', reason: 'r123 test' }); // Mods (rank 1) over a plain member
  const dOut = !sv23(await gq(D), s23);
  const ban23 = await call('/api/guilds/member/ban', U, { server: s23, user: 'mills', reason: 'r123 ban' });
  const iv23b = (await call('/api/guilds/invite', U, { server: s23, maxAge: 0, maxUses: 0, temporary: true })).data.result;
  const joinBanned = await call('/api/guilds/invite/join', D, { code: iv23b.code, password: 'hunter-r123' });
  const unban23 = await call('/api/guilds/member/unban', U, { server: s23, user: 'mills' });
  const joinTemp = await call('/api/guilds/invite/join', D, { code: iv23b.code, password: 'hunter-r123' });
  const tempListed = (sv23(await gq(U), s23) || {}).temp || [];
  // private + announcement channels
  const priv23 = await call('/api/guilds/channel', U, { server: s23, name: 'inner-room', roles: [roleI.id] });
  const news23 = await call('/api/guilds/channel', U, { server: s23, name: 'news', announce: true });
  const aSeesPriv = (sv23(await gq(A), s23) || { channels: [] }).channels.some((c) => c.id === 'inner-room');
  const aPrivSend = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'sneak r123', 'ch:' + s23 + '/inner-room'));
  const aNews = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'news by a r123', 'ch:' + s23 + '/news'));
  const uNews = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'news by owner r123', 'ch:' + s23 + '/news'));
  await call('/api/guilds/member/roles', U, { server: s23, user: 't-admin', roles: [roleM.id, roleI.id] });
  const aSeesPriv2 = (sv23(await gq(A), s23) || { channels: [] }).channels.some((c) => c.id === 'inner-room');
  const nick23 = await call('/api/guilds/member/nick', D, { server: s23, user: 'mills', nick: 'Dee r123' });
  const nickSeen = ((sv23(await gq(U), s23) || {}).nicks || {}).mills === 'Dee r123';
  const uView = sv23(await gq(U), s23) || {}, dView = sv23(await gq(D), s23) || {};
  const audit23 = (uView.audit || []).map((a) => a.what);
  const dNoAudit = dView.audit === undefined && dView.bans === undefined;
  const leaveOwner = await call('/api/guilds/server/leave', U, { server: s23 });
  const xfer = await call('/api/guilds/server/transfer', U, { server: s23, user: 't-admin' });
  const leaveAfter = await call('/api/guilds/server/leave', U, { server: s23 });
  const delOld = await call('/api/guilds/server/delete', U, { server: s23 });
  const delNew = await call('/api/guilds/server/delete', A, { server: s23 });
  // the Mills server: hub admins edit it; nobody invites to it
  const millsU = await call('/api/guilds/server/update', U, { server: 'mills', name: 'Not yours' });
  const millsA = await call('/api/guilds/server/update', A, { server: 'mills', name: 'Mills HQ r123', description: 'team r123' });
  const millsName = (sv23(await gq(U), 'mills') || {}).name;
  await call('/api/guilds/server/update', A, { server: 'mills', name: 'Mills', description: '' });
  const millsInv = await call('/api/guilds/invite', A, { server: 'mills' });
  // groups: owner removes / hands over; others cannot remove
  const g23 = (await call('/api/guilds/group', U, { name: 'three r123', members: ['t-admin', 'mills'] })).data.result;
  const gRmNo = await call('/api/guilds/group/remove', A, { group: g23.id, user: 'mills' });
  const gRmOk = await call('/api/guilds/group/remove', U, { group: g23.id, user: 'mills' });
  const gXfer = await call('/api/guilds/group/transfer', U, { group: g23.id, user: 't-admin' });
  await call('/api/guilds/group/leave', U, { group: g23.id });
  const gAfter = ((await gq(A)).groups || []).find((g) => g.id === g23.id);
  await call('/api/guilds/group/leave', A, { group: g23.id });
  const gs23 = fs.readFileSync(path.join(MILLS, 'web', 'guild-settings.js'), 'utf8');
  check('R123', 'servers like Discord: personal invitations to accept, server settings only with "manage server", roles with permissions + ranks (a moderator cannot kick the owner), password lock (hash never sent; no / wrong password refused), invite links with max uses + expiry + temporary membership, kick + ban (banned cannot rejoin) + unban, private channels only for their roles, announcement channels only for the announce permission, server nicknames, audit log only for managers, owner must hand over before leaving, only the owner deletes; the Mills server is edited by hub admins only and has no invites; groups: only the owner removes / hands over; [static] Server Settings tabs + join / invitation dialogs',
    mk23.status === 200 && aBefore && acc23.status === 200 && updNo.status === 400 && updOk.status === 200 && roleM && roleM.perms.join() === 'KICK_MEMBERS,CREATE_CHANNELS' && giveM.status === 200 && kickOwner.status === 400
    && pw23.status === 200 && !leak23 && pv23 && pv23.locked === true && pv23.members === 2 && joinNoPw.status === 400 && joinBadPw.status === 400 && joinOk.status === 200 && used23.status === 400 && dIn
    && kickD.status === 200 && dOut && ban23.status === 200 && joinBanned.status === 400 && unban23.status === 200 && joinTemp.status === 200 && tempListed.includes('mills')
    && priv23.status === 200 && news23.status === 200 && !aSeesPriv && aPrivSend.status === 400 && aNews.status === 403 && uNews.status === 200 && aSeesPriv2
    && nick23.status === 200 && nickSeen && audit23.includes('banned') && audit23.includes('kicked') && audit23.includes('joined') && dNoAudit
    && leaveOwner.status === 400 && xfer.status === 200 && leaveAfter.status === 200 && delOld.status === 400 && delNew.status === 200
    && millsU.status === 400 && millsA.status === 200 && millsName === 'Mills HQ r123' && millsInv.status === 400
    && gRmNo.status === 400 && gRmOk.status === 200 && gXfer.status === 200 && gAfter && gAfter.owner === 't-admin' && !gAfter.members.includes('t-user')
    && /'overview', 'Overview'/.test(gs23) && /'security', 'Security'/.test(gs23) && /'audit', 'Audit log'/.test(gs23) && /function drawJoin\(/.test(gs23) && /\?invite=/.test(gs23) && /<script src="guild-settings\.js">/.test(html),
    `make ${mk23.status}, invited ${aBefore} accept ${acc23.status}, update ${updNo.status}/${updOk.status}, role ${roleM && roleM.perms}, kick owner ${kickOwner.status}, pw ${pw23.status} leak ${leak23}, preview ${JSON.stringify(pv23)}, join ${joinNoPw.status}/${joinBadPw.status}/${joinOk.status} used ${used23.status}, kick ${kickD.status} out ${dOut}, ban ${ban23.status} rejoin ${joinBanned.status} unban ${unban23.status} temp ${joinTemp.status} ${tempListed}, priv ${priv23.status} seen ${aSeesPriv}→${aSeesPriv2} send ${aPrivSend.status}, news ${aNews.status}/${uNews.status}, nick ${nickSeen}, audit ${audit23.slice(0, 8)}, D audit ${dNoAudit}, leave ${leaveOwner.status} xfer ${xfer.status} leave ${leaveAfter.status} del ${delOld.status}/${delNew.status}, mills ${millsU.status}/${millsA.status} ${millsName} inv ${millsInv.status}, group ${gRmNo.status}/${gRmOk.status}/${gXfer.status} owner ${gAfter && gAfter.owner}`);

  // Discord's permission model: levels, timeouts, slowmode, read history, pins, manage messages, channel overwrites,
  // server mute in calls, only-grant-what-you-hold, welcome lines, paused invites, ban that deletes recent messages.
  const roomMsgs = async (tok, after) => ((await call('/api/room/messages?since=' + after, tok)).data.messages || []);
  const seq25 = async () => Math.max(0, ...(await roomMsgs(U, 0)).map((m) => m.seq));
  const mk25 = (await call('/api/guilds/server', U, { name: 'Perms r125', levels: true, members: ['t-admin'] })).data.result;
  const s25 = mk25.id, gen25 = 'ch:' + s25 + '/general';
  const v25 = () => gq(U).then((v) => sv23(v, s25) || {});
  const lv25 = await v25();
  const roleBy = (n) => (lv25.roles || []).find((r) => r.name === n) || {};
  const levelsOk = ['Super Admin', 'Admin', 'Moderator', 'Member'].every((n) => roleBy(n).id) && roleBy('Admin').position > roleBy('Moderator').position && roleBy('Moderator').position > roleBy('Member').position && roleBy('Super Admin').perms.includes('ADMINISTRATOR');
  await call('/api/guilds/invitation/accept', A, { server: s25 });
  await call('/api/guilds/member/roles', U, { server: s25, user: 't-admin', roles: [roleBy('Member').id] });
  // read history: neither @everyone nor Member may read history → only what arrives after joining; welcome line
  await call('/api/guilds/server/update', U, { server: s25, settings: { systemChannel: 'general', systemJoins: true }, everyonePerms: ['VIEW_CHANNEL', 'SEND_MESSAGES', 'CONNECT', 'SPEAK', 'CHANGE_NICKNAME'] });
  await call('/api/guilds/role/update', U, { server: s25, role: roleBy('Member').id, perms: ['VIEW_CHANNEL', 'SEND_MESSAGES', 'CONNECT', 'SPEAK'] });
  const before25 = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'before D r125', gen25));
  await call('/api/guilds/server/invite', U, { server: s25, add: ['mills'] }); await call('/api/guilds/invitation/accept', D, { server: s25 });
  const after25 = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'after D r125', gen25));
  const dSees = (await roomMsgs(D, 0)).filter((m) => m.conv === gen25).map((m) => m.seq);
  const welcome25 = (await roomMsgs(U, before25.data.seq)).some((m) => m.conv === gen25 && m.as === 'VibeRoller');
  const histOk = !dSees.includes(before25.data.seq) && dSees.includes(after25.data.seq);
  // timeout: A cannot post, type or call; ended → can again
  const to25 = await call('/api/guilds/member/timeout', U, { server: s25, user: 't-admin', minutes: 5, reason: 'r125' });
  const toSend = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'while timed out r125', gen25));
  const toCall = await call('/api/call/join', A, { conv: gen25, mic: true });
  const toKickOwner = await call('/api/guilds/member/timeout', A, { server: s25, user: 't-user', minutes: 5 });
  await call('/api/guilds/member/timeout', U, { server: s25, user: 't-admin', minutes: 0 });
  const toAfter = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'after timeout r125', gen25));
  // slowmode (owner bypasses)
  await call('/api/guilds/channel', U, { server: s25, name: 'slow', slowmode: 10 });
  const slowConv = 'ch:' + s25 + '/slow';
  const sl1 = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'slow 1 r125', slowConv));
  const sl2 = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'slow 2 r125', slowConv));
  const slU1 = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'owner 1 r125', slowConv)), slU2 = await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'owner 2 r125', slowConv));
  // pins: a Moderator (A) may pin, a Member (D) may not; manage messages: a Moderator deletes others' messages, a Member cannot
  const delNoMod = await call('/api/room/delete', D, { seq: after25.data.seq });
  await call('/api/guilds/member/roles', U, { server: s25, user: 't-admin', roles: [roleBy('Moderator').id] });
  const pinA = await call('/api/guilds/pin', A, { conv: gen25, seq: after25.data.seq });
  const pinD = await call('/api/guilds/pin', D, { conv: gen25, seq: after25.data.seq });
  const pinBad = await call('/api/guilds/pin', A, { conv: gen25, seq: before25.data.seq - 100000 });
  const pinsSeen = ((await v25()).channels.find((c) => c.id === 'general') || {}).pins || [];
  const delMod = await call('/api/room/delete', A, { seq: after25.data.seq });
  const delDm = await call('/api/room/delete', D, { seq: slU1.data.seq });
  // only grant what you hold: the Moderator cannot make an administrator role (no manage roles at all here) …
  const grantNo = await call('/api/guilds/role', A, { server: s25, name: 'sneaky', perms: ['ADMINISTRATOR'] });
  // channel overwrites: deny A sending in #slow, allow D to view a private channel
  const owSet = await call('/api/guilds/channel/update', U, { server: s25, channel: 'slow', overwrites: [{ type: 'member', id: 't-admin', allow: [], deny: ['SEND_MESSAGES'] }] });
  await sleep(10500); // past the slowmode, so the 403 below is the overwrite
  const owSend = await call('/api/room/send', A, await sealConv(A, privA, 't-admin', 'denied r125', slowConv));
  await call('/api/guilds/channel', U, { server: s25, name: 'hidden', overwrites: [{ type: 'role', id: 'everyone', allow: [], deny: ['VIEW_CHANNEL'] }, { type: 'member', id: 'mills', allow: ['VIEW_CHANNEL'], deny: [] }] });
  const hidV = (tok) => gq(tok).then((v) => ((sv23(v, s25) || { channels: [] }).channels || []).some((c) => c.id === 'hidden'));
  const hidD = await hidV(D), hidA = await hidV(A);
  // server mute in calls: the mic flag is held off for everyone to see
  await call('/api/guilds/member/mute', U, { server: s25, user: 't-admin', on: true });
  const mj = await call('/api/call/join', A, { conv: gen25, mic: true });
  const mjMe = ((mj.data.calls || {})[gen25] || []).find((p) => p.user === 't-admin') || {};
  await call('/api/call/leave', A, {});
  await call('/api/guilds/member/mute', U, { server: s25, user: 't-admin', on: false });
  // paused invites
  await call('/api/guilds/server/update', U, { server: s25, settings: { invitesPaused: true } });
  const ivPaused = await call('/api/guilds/invite', U, { server: s25 });
  // ban + delete the last hour of their messages
  const aLast = toAfter.data && toAfter.data.seq;
  const ban25 = await call('/api/guilds/member/ban', U, { server: s25, user: 't-admin', deleteSeconds: 3600 });
  const aGone = (await roomMsgs(U, 0)).find((m) => m.seq === aLast);
  const gs25 = fs.readFileSync(path.join(MILLS, 'web', 'guild-settings.js'), 'utf8'), gp25 = fs.readFileSync(path.join(BRIDGE, 'guild-perms.js'), 'utf8');
  await call('/api/guilds/server/delete', U, { server: s25 });
  check('R125', 'Discord permission model: permission levels Admin / Moderator / Member on a new server; without read history only messages after joining; welcome line by "Mills"; timeout blocks posting + calls (and nobody times out the owner); slowmode (owner bypasses); pins need "pin messages"; "manage messages" deletes others\' messages; nobody grants what they do not hold; channel overwrites (member deny send, private channel with one person allowed); server mute holds the mic flag off; paused invites; a ban deletes the last hour of their messages; [static] grouped permissions, Settings tab, timeout / mute / ban panels, overwrite editor',
    levelsOk && histOk && welcome25 && to25.status === 200 && toSend.status === 403 && toCall.status === 403 && toKickOwner.status === 400 && toAfter.status === 200
    && sl1.status === 200 && sl2.status === 403 && slU1.status === 200 && slU2.status === 200
    && pinA.status === 200 && pinD.status === 400 && pinBad.status === 400 && pinsSeen.some((p) => p.seq === after25.data.seq) && delNoMod.status === 403 && delMod.status === 200 && delDm.status === 403
    && grantNo.status === 400 && owSet.status === 200 && owSend.status === 403 && hidD && !hidA
    && mj.status === 200 && mjMe.mic === false && mjMe.held === 'muted' && ivPaused.status === 400 && ban25.status === 200 && ban25.data.result.deleted >= 1 && aGone && aGone.deleted === true
    && /\['Voice', \{/.test(gp25) && /MODERATE_MEMBERS/.test(gp25) && /function settings\(s\)/.test(gs25) && /'Time out'/.test(gs25) && /Mute in calls/.test(gs25) && /add an override for/.test(gs25) && /Built-in levels/.test(gs25),
    `levels ${levelsOk}, history ${histOk} welcome ${welcome25}, timeout ${to25.status} send ${toSend.status} call ${toCall.status} owner ${toKickOwner.status} after ${toAfter.status}, slow ${sl1.status}/${sl2.status} owner ${slU1.status}/${slU2.status}, pin ${pinA.status}/${pinD.status}/${pinBad.status} seen ${pinsSeen.length}, delete ${delNoMod.status}/${delMod.status}/${delDm.status}, grant ${grantNo.status}, overwrite ${owSet.status} send ${owSend.status}, hidden D ${hidD} A ${hidA}, mute ${mj.status} ${JSON.stringify(mjMe)}, paused ${ivPaused.status}, ban ${ban25.status} ${JSON.stringify(ban25.data.result)} gone ${aGone && aGone.deleted}`);

  // Built-in levels + co-owners: the ladder on every server (Mills too), Member for who joins, co-owners with owner
  // rights under the owner, handing over keeps the old owner as Co-owner, custom roles sit between levels.
  const s26 = (await call('/api/guilds/server', U, { name: 'Ladder r126', members: ['t-admin', 'mills'] })).data.result.id;
  await call('/api/guilds/invitation/accept', A, { server: s26 }); await call('/api/guilds/invitation/accept', D, { server: s26 });
  const v26 = (tok = U) => gq(tok).then((v) => sv23(v, s26) || {});
  const L = await v26();
  const rb = (n) => (L.roles || []).find((r) => r.name === n) || {};
  const ladder = ['Co-owner', 'Super Admin', 'Admin', 'Super Moderator', 'Moderator', 'Member'];
  const ladderOk = ladder.every((n, i) => rb(n).builtin && (i === 0 || rb(ladder[i - 1]).position > rb(n).position)) && rb('Co-owner').perms.includes('ADMINISTRATOR');
  const joinedMember = ['t-admin', 'mills'].every((u) => (L.memberRoles[u] || []).includes(rb('Member').id));
  const millsLadder = ((sv23(await gq(U), 'mills') || {}).roles || []).filter((r) => r.builtin).length === 6;
  const delBuiltin = await call('/api/guilds/role/delete', U, { server: s26, role: rb('Moderator').id });
  const moveBuiltin = await call('/api/guilds/role/update', U, { server: s26, role: rb('Admin').id, position: 45 });
  const permsSA = await call('/api/guilds/role/update', U, { server: s26, role: rb('Super Admin').id, perms: ['KICK_MEMBERS'] });
  const coOn = await call('/api/guilds/member/roles', U, { server: s26, user: 't-admin', roles: [rb('Member').id, rb('Co-owner').id] });
  const coRename = await call('/api/guilds/server/update', A, { server: s26, name: 'Ladder co r126' });
  const coPw = await call('/api/guilds/server/password', A, { server: s26, password: 'co-pass-r126' });
  const coKick = await call('/api/guilds/member/kick', A, { server: s26, user: 'mills' });
  const coKickOwner = await call('/api/guilds/member/kick', A, { server: s26, user: 't-user' });
  await call('/api/guilds/server/invite', A, { server: s26, add: ['mills'] }); await call('/api/guilds/invitation/accept', D, { server: s26 });
  const coMakeCo = await call('/api/guilds/member/roles', A, { server: s26, user: 'mills', roles: [rb('Member').id, rb('Co-owner').id] });
  const coMakeAdmin = await call('/api/guilds/member/roles', A, { server: s26, user: 'mills', roles: [rb('Member').id, rb('Super Admin').id] });
  const coDelete = await call('/api/guilds/server/delete', A, { server: s26 });
  const coTransfer = await call('/api/guilds/server/transfer', A, { server: s26, user: 'mills' });
  const custom = (await call('/api/guilds/role', A, { server: s26, name: 'Helpers r126' })).data.result;
  const moved26 = (await call('/api/guilds/role/update', A, { server: s26, role: custom.id, position: 20 })).data.result; // Moderator's spot → hops to 21
  const xfer26 = await call('/api/guilds/server/transfer', U, { server: s26, user: 't-admin' });
  const after26 = await v26(A), uRoles = (after26.memberRoles || {})['t-user'] || [];
  const oldOwnerCo = after26.owner === 't-admin' && after26.me.owner && uRoles.includes(rb('Co-owner').id) && !((after26.memberRoles || {})['t-admin'] || []).includes(rb('Co-owner').id);
  const exKick = await call('/api/guilds/member/kick', U, { server: s26, user: 't-admin' });
  const coLeave = await call('/api/guilds/server/leave', U, { server: s26 });
  await call('/api/guilds/server/delete', A, { server: s26 });
  check('R126', 'built-in levels on every server (Mills too): Co-owner > Super Admin > Admin > Super Moderator > Moderator > Member, cannot be deleted or reordered, Co-owner / Super Admin keep every permission; who joins becomes a Member; a co-owner (many allowed) has owner rights under the owner: changes settings + password, kicks, invites, hands out Super Admin, but cannot name co-owners, kick the owner, delete or hand over the server; custom roles start above Member and hop over the built-in spots; handing over keeps the old owner as Co-owner (who cannot kick the new owner)',
    ladderOk && joinedMember && millsLadder && delBuiltin.status === 400 && moveBuiltin.status === 400 && permsSA.status === 400
    && coOn.status === 200 && coRename.status === 200 && coPw.status === 200 && coKick.status === 200 && coKickOwner.status === 400 && coMakeCo.status === 400 && coMakeAdmin.status === 200 && coDelete.status === 400 && coTransfer.status === 400
    && custom && custom.position === 11 && moved26 && moved26.position === 21 && xfer26.status === 200 && oldOwnerCo && exKick.status === 400 && coLeave.status === 200,
    `ladder ${ladderOk} member ${joinedMember} mills ${millsLadder}, builtin del ${delBuiltin.status} move ${moveBuiltin.status} sa ${permsSA.status}, co ${coOn.status} rename ${coRename.status} pw ${coPw.status} kick ${coKick.status}/${coKickOwner.status} co→co ${coMakeCo.status} co→sa ${coMakeAdmin.status} delete ${coDelete.status} transfer ${coTransfer.status}, custom ${custom && custom.position}→${moved26 && moved26.position}, handover ${xfer26.status} old owner co ${oldOwnerCo}, kick new owner ${exKick.status}, leave ${coLeave.status}`);

  // Role-based features: admins + owners make coloured roles, people hold many, the highest coloured role colours the
  // name inside that server only, mentionable roles, self-assignable roles (everyday permissions only).
  const s27 = (await call('/api/guilds/server', U, { name: 'Colors r127', members: ['t-admin', 'mills'] })).data.result.id;
  await call('/api/guilds/invitation/accept', A, { server: s27 }); await call('/api/guilds/invitation/accept', D, { server: s27 });
  const v27 = async (tok = U) => sv23(await gq(tok), s27) || {};
  const b27 = (n) => v27().then((v) => (v.roles || []).find((r) => r.name === n || r.builtin === n) || {});
  const red = (await call('/api/guilds/role', U, { server: s27, name: 'Red r127', color: '#ff0000' })).data.result;
  const blue = (await call('/api/guilds/role', U, { server: s27, name: 'Blue r127', color: '#0000ff' })).data.result;
  await call('/api/guilds/role/update', U, { server: s27, role: blue.id, position: 12 });
  const mem27 = await b27('member'), adm27 = await b27('admin');
  const multi = await call('/api/guilds/member/roles', U, { server: s27, user: 't-admin', roles: [mem27.id, red.id, blue.id] });
  const giveAdmin = await call('/api/guilds/member/roles', U, { server: s27, user: 'mills', roles: [mem27.id, adm27.id] });
  const adminMakes = await call('/api/guilds/role', D, { server: s27, name: 'Green r127', color: '#00ff00' });
  const plainMakes = await call('/api/guilds/role', A, { server: s27, name: 'Nope r127' });
  const ment27 = await call('/api/guilds/role/update', U, { server: s27, role: red.id, mentionable: true });
  const self27 = (await call('/api/guilds/role', U, { server: s27, name: 'Gamers r127', color: '#aa00aa', selfAssign: true })).data.result;
  const selfBad = await call('/api/guilds/role', U, { server: s27, name: 'Bad r127', perms: ['KICK_MEMBERS'], selfAssign: true });
  const selfBuiltin = await call('/api/guilds/role/update', U, { server: s27, role: mem27.id, selfAssign: true });
  const pickOn = await call('/api/guilds/member/selfrole', A, { server: s27, role: self27 && self27.id, on: true });
  const pickNot = await call('/api/guilds/member/selfrole', A, { server: s27, role: adm27.id, on: true });
  const aRoles = ((await v27(A)).memberRoles || {})['t-admin'] || [];
  const pickOff = await call('/api/guilds/member/selfrole', A, { server: s27, role: self27 && self27.id, on: false });
  const aRoles2 = ((await v27(A)).memberRoles || {})['t-admin'] || [];
  const rl27 = (await v27()).roles || [], blueNow = rl27.find((r) => r.id === blue.id) || {}, redNow = rl27.find((r) => r.id === red.id) || {};
  const gj27 = fs.readFileSync(path.join(MILLS, 'web', 'guilds.js'), 'utf8'), gs27 = fs.readFileSync(path.join(MILLS, 'web', 'guild-settings.js'), 'utf8');
  await call('/api/guilds/server/delete', U, { server: s27 });
  check('R127', 'role features: owners and admins (manage roles) make coloured roles, plain members cannot; many roles per person; roles listed highest first so the highest coloured role colours the name, only inside that server (chat names, mentions, autocomplete, member column); mentionable roles (@Role highlights + notifies its holders); self-assignable roles members pick / drop themselves, limited to everyday permissions, never a built-in level or a role not marked pickable',
    multi.status === 200 && aRoles.includes(red.id) && aRoles.includes(blue.id) && giveAdmin.status === 200 && adminMakes.status === 200 && plainMakes.status === 400
    && blueNow.position > redNow.position && rl27.findIndex((r) => r.id === blue.id) < rl27.findIndex((r) => r.id === red.id) && ment27.status === 200 && redNow.mentionable === true
    && self27 && self27.selfAssign === true && selfBad.status === 400 && selfBuiltin.status === 400 && pickOn.status === 200 && pickNot.status === 400 && aRoles.includes(self27.id) && pickOff.status === 200 && !aRoles2.includes(self27.id)
    && /const roleColor = \(s, u\) => \{ const r = rolesOfUser\(s, u\)\.find/.test(gj27) && /\.sort\(\(a, b\) => b\.position - a\.position\)/.test(gj27) && /GUILDS\.colorIn\(convOfMsg\(m\)/.test(html) && /GUILDS\.roleByHandle\(currentConv, h\)/.test(html) && /GUILDS\.mentionsMyRole\(c, rec\.text\)/.test(html) && /mentionableRoles\(currentConv\)/.test(html)
    && /'Your roles'/.test(gs27) && /member\/selfrole/.test(gs27),
    `multi ${multi.status} ${aRoles.length} roles, admin makes ${adminMakes.status} plain ${plainMakes.status}, order blue ${blueNow.position} red ${redNow.position}, mentionable ${redNow.mentionable}, self ${self27 && self27.selfAssign} bad ${selfBad.status} builtin ${selfBuiltin.status} pick ${pickOn.status}/${pickNot.status} off ${pickOff.status}`);

  // Profiles (user settings everyone sees) kept apart from app settings (private, follow the person to every browser).
  const pf0 = (await call('/api/profile', U)).data, prefs0 = (await call('/api/prefs', U)).data.prefs || {};
  const pfSave = await call('/api/profile', U, { nick: prefs0.nick || '', color: prefs0.color ?? null, profile: { bio: 'hello r128\nsecond line ' + 'x'.repeat(400), pronouns: 'they/them', status: 'testing r128', location: 'Somewhere', links: ['https://example.org/r128'], accent: 3, timezone: 'Europe/Berlin' } });
  const pfBadLink = await call('/api/profile', U, { profile: { links: ['http://plain.example/r128'] } });
  const pfBadTz = await call('/api/profile', U, { profile: { timezone: 'Mars/Olympus' } });
  const seenByA28 = (((await call('/api/room/members', A)).data.members || []).find((m) => m.username === 't-user') || {});
  const pp = seenByA28.profile || {};
  const appSave = await call('/api/prefs', U, { theme: 'dracula', sound: { on: !((prefs0.sound || {}).on) } });
  const appBad = (await call('/api/prefs', U, { theme: '<script>' })).data.prefs || {};
  const prefsAfter = (await call('/api/prefs', U)).data.prefs || {};
  const pfAgain = await call('/api/profile', U, { profile: { status: 'still r128' } });
  const prefsAfter2 = (await call('/api/prefs', U)).data.prefs || {};
  const leak28 = /"(sound|tts|theme|audio|call|clips|autosave)"/.test(JSON.stringify(seenByA28));
  await call('/api/prefs', U, { theme: prefs0.theme || '', sound: { on: !!(prefs0.sound || {}).on } });
  await call('/api/profile', U, { profile: { bio: '', pronouns: '', status: '', location: '', links: [], accent: null, timezone: '' } });
  const pj28 = fs.readFileSync(path.join(MILLS, 'web', 'profile.js'), 'utf8');
  check('R128', 'profiles: a person sets bio (300 max), pronouns, status, location, https links, banner colour (default: follows their colour) and their local time; everyone signed in sees it in the member list; bad links / time zones refused; app settings (sounds, theme …) stay private, are saved separately (theme on the account, follows them to every browser) and a profile save never touches them; [static] Settings tabs Profile / App settings / Connections, live preview, profile card from a name with Message, the theme saved with the account',
    pf0.username === 't-user' && pf0.profile && pf0.profile.accent === null && pfSave.status === 200 && pfBadLink.status === 400 && pfBadTz.status === 400
    && pp.bio && pp.bio.length === 300 && pp.bio.startsWith('hello r128\nsecond line') && pp.pronouns === 'they/them' && pp.status === 'testing r128' && pp.links.join() === 'https://example.org/r128' && pp.accent === 3 && pp.timezone === 'Europe/Berlin'
    && appSave.status === 200 && appSave.data.prefs.theme === 'dracula' && appBad.theme === '' && prefsAfter.sound.on === !((prefs0.sound || {}).on) && pfAgain.status === 200 && prefsAfter2.theme === '' && prefsAfter2.sound.on === prefsAfter.sound.on && !leak28
    && /data-settab="profile"/.test(html) && /data-settab="app"/.test(html) && /data-settab="connections"/.test(html) && /data-setpane="connections"/.test(html) && /<script src="profile\.js">/.test(html)
    && /function cardFor\(/.test(pj28) && /'follow my colour'/.test(pj28) && /'Message'/.test(pj28) && /PROFILE\.show\(m\.via \|\| m\.from, ev\)/.test(html) && /saveThemeChoice\(/.test(html) && /theme: \$\('setTheme'\)\.value/.test(html),
    `get ${JSON.stringify(pf0.profile)}, save ${pfSave.status} badLink ${pfBadLink.status} badTz ${pfBadTz.status}, seen ${JSON.stringify(pp).slice(0, 160)}, app ${appSave.status} ${appSave.data.prefs && appSave.data.prefs.theme} bad "${appBad.theme}", after profile save theme "${prefsAfter2.theme}" sound ${prefsAfter2.sound && prefsAfter2.sound.on}, leak ${leak28}`);

  // Profile pictures: upload (type checked by its bytes, size capped), everyone sees it, replace, remove.
  const avDir = path.join(SCRATCH, 'Avatars');
  const pngBytes = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(40, 7)]);
  const jpgBytes = Buffer.concat([Buffer.from([0xff, 0xd8, 0xff, 0xe0]), Buffer.alloc(40, 9)]);
  const avPng = await call('/api/profile/avatar', U, { image: 'data:image/png;base64,' + pngBytes.toString('base64') });
  const avSeen = ((((await call('/api/room/members', A)).data.members || []).find((m) => m.username === 't-user') || {}).profile || {}).avatar || '';
  const pngFile = fs.existsSync(path.join(avDir, 't-user.png'));
  const avJpg = await call('/api/profile/avatar', U, { image: 'data:image/jpeg;base64,' + jpgBytes.toString('base64') });
  const swapped = !fs.existsSync(path.join(avDir, 't-user.png')) && fs.existsSync(path.join(avDir, 't-user.jpg'));
  const avSvg = await call('/api/profile/avatar', U, { image: 'data:image/svg+xml;base64,' + Buffer.from('<svg/>').toString('base64') });
  const avFake = await call('/api/profile/avatar', U, { image: 'data:image/png;base64,' + Buffer.from('not a picture at all').toString('base64') });
  const avBig = await call('/api/profile/avatar', U, { image: 'data:image/png;base64,' + Buffer.concat([pngBytes, Buffer.alloc(320 * 1024)]).toString('base64') });
  const avAnon = await fetch(H + '/api/profile/avatar', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
  const avRm = await call('/api/profile/avatar/remove', U, {});
  const avGone = ((((await call('/api/room/members', A)).data.members || []).find((m) => m.username === 't-user') || {}).profile || {}).avatar;
  const pj30 = fs.readFileSync(path.join(MILLS, 'web', 'profile.js'), 'utf8');
  check('R130', 'profile pictures: a person uploads one (shrunk to a 256 px square in their browser), the hub checks the type by its first bytes (PNG / JPEG / WebP only: no SVG, no disguised files) and caps the size, keeps one file per person outside the account folders (a new one replaces the old), everyone sees it in the member list; Remove deletes it; signed-out → 401; [static] upload / remove buttons, picture on the card, in the member column and next to messages; the folder is gitignored',
    avPng.status === 200 && avSeen.startsWith('data:image/png;base64,') && pngFile && avJpg.status === 200 && swapped && avSvg.status === 400 && avFake.status === 400 && avBig.status === 400 && avAnon.status === 401
    && avRm.status === 200 && avGone === '' && !fs.existsSync(path.join(avDir, 't-user.jpg'))
    && /function shrink\(/.test(pj30) && /toDataURL\('image\/webp'/.test(pj30) && /'Remove picture'/.test(pj30) && /PROFILE\.avatarOf\(m\.via \|\| m\.from\)/.test(html) && /PROFILE\.avatarOf\(u\)/.test(html)
    && /^Avatars\/$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')),
    `png ${avPng.status} seen ${avSeen.slice(0, 30)} file ${pngFile}, jpg ${avJpg.status} swapped ${swapped}, svg ${avSvg.status} fake ${avFake.status} big ${avBig.status} anon ${avAnon.status}, remove ${avRm.status} gone "${avGone}"`);

  // The host's AI switch (bridge/ai-host.off, the heart / knife host tools): the AI hosted here pauses, nothing else.
  fs.writeFileSync(path.join(SB, 'ai-host.off'), 'paused by r136');
  const offCheck = (await call('/api/check', U)).data;
  const offWork = (((await chat(U, 'while off r136', { fresh: true })).events || []).find((e) => e.type === 'bridge_error') || {}).text || ''; // the hub's own notice, not Claude text
  const offKey = (await call('/api/apikey/rotate', U, {})).data.key;
  const offV1 = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + offKey }, body: JSON.stringify({ model: 'x', messages: [{ role: 'user', content: 'hi r136' }] }) });
  const offAssist = await call('/api/assistant/chat', U, { message: 'hi r136' });
  const seqOff = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await sealConv(U, restored, 't-user', '@unity are you there r136', 'room', true));
  const offUnity = await waitAgent(U, restored, seqOff, (t) => /paused the AI/.test(t), 8000);
  const offChatOk = (await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'chat still works r136', 'room'))).status;
  const offBoardOk = (await call('/api/board', U)).status;
  fs.unlinkSync(path.join(SB, 'ai-host.off'));
  const onCheck = (await call('/api/check', U)).data;
  const onWork = texts((await chat(U, 'back on r136', { fresh: true })).events);
  await call('/api/apikey/revoke', U, {});
  const ht = (f) => fs.readFileSync(path.join(BRIDGE, 'host-tools', f), 'utf8');
  check('R136', 'the host\'s AI switch: with bridge/ai-host.off the AI hosted on this machine pauses (Work runs on the host\'s Claude, the API → 503, the Unity section → 503, Unity in the chat says it is paused) while chat and the board keep working and /api/check reports it; removing the file brings the AI back at once; [static] the heart & knife control app (vr-host.js, one shortcut with its own icon drawn by the build script on Windows, a launcher on Linux; the old heart / knife programs are removed, the Linux start / stop scripts call it) starts / stops the service when it is installed and otherwise starts the host or touches that switch; the service (AiHostService.cs) runs node ai-node.js from boot, restarts it, logs to ai-node.log; the installer registers it with start= auto and lets signed-in users start / stop it; the Linux unit + installer do the same; the build output is gitignored',
    offCheck.aiHost === false && /switched off right now/.test(offWork) && offV1.status === 503 && offAssist.status === 503 && !!offUnity && offChatOk === 200 && offBoardOk === 200
    && onCheck.aiHost === true && /echo: back on r136/.test(onWork)
    // since 3.6.0 the heart and the knife are one control app (vr-host.js): one shortcut with the heart & knife icon, the old programs removed
    && /heart-and-knife\.ico/.test(ht('build-host-tools.ps1')) && /VibeRoller AI Host\.lnk/.test(ht('build-host-tools.ps1')) && /vr-host\.js/.test(ht('build-host-tools.ps1')) && /'start-viberoller-ai-host\.exe', 'stop-viberoller-ai-host\.exe'/.test(ht('build-host-tools.ps1'))
    && /VibeRollerAIHost\.exe/.test(ht('build-host-tools.ps1')) && /System\.ServiceProcess\.dll/.test(ht('build-host-tools.ps1'))
    && /ServiceBase\.Run/.test(ht('AiHostService.cs')) && /ai-node\.js/.test(ht('AiHostService.cs')) && /ai-node\.log/.test(ht('AiHostService.cs')) && /OnStop/.test(ht('AiHostService.cs'))
    && /sc create %NAME% binPath=/.test(ht('install-ai-host-service.cmd')) && /start= auto/.test(ht('install-ai-host-service.cmd')) && /sc sdset/.test(ht('install-ai-host-service.cmd')) && /;;;IU\)/.test(ht('install-ai-host-service.cmd')) && /-Verb RunAs/.test(ht('install-ai-host-service.cmd'))
    && /WantedBy=multi-user\.target/.test(ht('viberoller-ai-host.service')) && /ExecStart=\/usr\/bin\/env node ai-node\.js/.test(ht('viberoller-ai-host.service')) && /systemctl enable/.test(ht('install-ai-host-service.sh')) && /polkit/.test(ht('install-ai-host-service.sh'))
    && /exec node vr-host\.js start/.test(ht('start-viberoller-host.sh')) && /exec node vr-host\.js stop/.test(ht('stop-viberoller-host.sh')) && /exec node vr-host\.js "\$@"/.test(ht('viberoller-host-control.sh'))
    && /^bridge\/ai-host\.off$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /^bridge\/host-tools\/\*\.exe$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')),
    `off: check ${offCheck.aiHost} work "${offWork.slice(0, 60)}" v1 ${offV1.status} assistant ${offAssist.status} unity ${offUnity ? 'said paused' : 'no reply'} chat ${offChatOk} board ${offBoardOk}; on: check ${onCheck.aiHost} work "${onWork.slice(0, 40)}"`);

  // AI nodes (bridge/ai-nodes.js + bridge/ai-node.js): this hub runs Unity's engines through a node process.
  const nodeList = ((await call('/api/check', U)).data.aiNodes || []);
  const nodeBad = await fetch(H + '/node/poll', { headers: { 'X-VR-Node': 'test-node', 'X-VR-Node-Token': 'wrong-secret-0123456789abcdef' } });
  const nodeUnknown = await fetch(H + '/node/poll', { headers: { 'X-VR-Node': 'nobody', 'X-VR-Node-Token': NODE_SECRET } });
  const nodeReplyStray = await fetch(H + '/node/reply', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Node': 'test-node', 'X-VR-Node-Token': NODE_SECRET }, body: JSON.stringify({ id: 'nope', status: 200 }) });
  const unityState = (await call('/api/assistant/state', U)).data || {};
  const nodeLog = fs.existsSync(path.join(SCRATCH, 'ai-node.log')) ? fs.readFileSync(path.join(SCRATCH, 'ai-node.log'), 'utf8') : '';
  const nodeSrc = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), hubNodeSrc = fs.readFileSync(path.join(BRIDGE, 'ai-nodes.js'), 'utf8');
  check('R137', 'AI nodes: a machine lends Unity\'s local engines to a hub running elsewhere by connecting OUT (long poll + reply, its own name + secret); this throwaway hub runs every Unity check through such a node; wrong secret / unknown node → 401, a stray reply → 404; the hub sends only an engine name + path (the node builds the address from its own list, so nothing else on its network is reachable); the knife (ai-host.off) pauses the node; /api/check lists the nodes',
    nodeList.some((n) => n.name === 'test-node' && n.online) && nodeBad.status === 401 && nodeUnknown.status === 401 && nodeReplyStray.status === 404 && unityState.local && unityState.local.up === true && unityState.local.model === true && /engines ollama, image, voice, speech/.test(nodeLog)
    && /base = ENGINES\[job\.engine\]/.test(nodeSrc) && /PAUSE_FILE/.test(nodeSrc) && /engine: target\.engine, path: target\.path/.test(hubNodeSrc) && /timingSafeEqual/.test(hubNodeSrc)
    && /^bridge\/ai-node\.json$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')),
    `nodes ${JSON.stringify(nodeList)}, bad ${nodeBad.status} unknown ${nodeUnknown.status} stray ${nodeReplyStray.status}, unity ${JSON.stringify(unityState.local || {})}, log "${nodeLog.split('\n')[0]}"`);

  { // No AI host (own block so its names never clash): the node goes away (Gee's PC off or paused) and everything else stays reachable, Unity says so at once.
  const assistantSay = async (tok, message) => { // one /api/assistant/chat run, all its events
    const t0 = Date.now(), r = await fetch(H + '/api/assistant/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + tok }, body: JSON.stringify({ message, client: { tz: 'Asia/Tokyo', locale: 'en-US' } }) });
    const events = (await r.text()).split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return {}; } });
    return { status: r.status, events, ms: Date.now() - t0 };
  };
  stopAiNode();
  await sleep(5500); // past aiNodeOfflineMs (4 s in this hub): the hub now counts the node as gone
  const offNodes = (await call('/api/check', U)).data.aiNodes || [];
  const offState = ((await call('/api/assistant/state', U)).data || {}).local || {};
  const offSay = await assistantSay(U, 'anyone there r138');
  const offErr = (offSay.events.find((e) => e.type === 'error') || {}).text || '';
  const seq38 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await call('/api/room/send', U, await sealConv(U, restored, 't-user', '@unity hello while off r138', 'room', true));
  const offUnity = await waitAgent(U, restored, seq38, (t) => /AI host is offline/.test(t), 12000);
  const offChat = (await call('/api/room/send', U, await sealConv(U, restored, 't-user', 'chat without an AI host r138', 'room'))).status;
  const offBoard = (await call('/api/board', U)).status, offNotes = (await call('/api/notes', U)).status, offWork = (await call('/api/work/current', U)).status;
  const offLogin = (await login('t-user', 'pw-t-user-0001', devB)).status;
  startAiNode();
  let backState = {};
  for (let i = 0; i < 60; i++) { backState = ((await call('/api/assistant/state', U)).data || {}).local || {}; if (backState.up && backState.model) break; await sleep(500); }
  check('R138', 'no AI host: when the node that lends Unity\'s engines goes away (the AI host is off or paused), the hub counts it offline within aiNodeOfflineMs, Unity\'s state shows her brain down, the Unity section answers at once with "AI host is offline" (no long wait), @unity in the chat posts the same plainly, while sending chat, the board, notes, Work\'s saved conversation and signing in all keep working; the node coming back brings Unity back without a restart',
    offNodes.some((n) => n.name === 'test-node' && n.online === false) && offState.up === false && offSay.status === 200 && /AI host is offline/.test(offErr) && offSay.ms < 15000
    && !!offUnity && offChat === 200 && offBoard === 200 && offNotes === 200 && offWork === 200 && offLogin === 200 && backState.up === true && backState.model === true,
    `offline listed ${offNodes.some((n) => n.online === false)}, state up ${offState.up}, unity section ${offSay.status} in ${offSay.ms} ms "${offErr.slice(0, 50)}", chat unity ${offUnity ? 'said offline' : 'no reply'}, chat ${offChat} board ${offBoard} notes ${offNotes} work ${offWork} login ${offLogin}; back: up ${backState.up} model ${backState.model}`);

  { // AI hosts as providers: identity (UUID + secret + nickname), the handshake, Settings → My AI hosts, remote Work runs,
    // computer access and local models through the host, caps, offline. The test host (startAiNode) belongs to t-user and
    // offers its "Claude" (the fake one) and a local model to t-admin.
    const hostHdr = (id, tok) => ({ 'Content-Type': 'application/json', 'X-VR-Host': id, 'X-VR-Host-Token': tok });
    let hostsNow = []; for (let i = 0; i < 40 && !hostsNow.some((h) => h.id === 'test-node' && h.online && h.providers.length); i++) { hostsNow = (await call('/api/check', U)).data.aiNodes || []; if (!hostsNow.some((h) => h.id === 'test-node' && h.online && h.providers.length)) await sleep(250); }
    const testHost = hostsNow.find((h) => h.id === 'test-node') || {};
    const helloBad = await fetch(H + '/node/hello', { method: 'POST', headers: hostHdr('test-node', 'wrong-secret-0123456789abcdef'), body: JSON.stringify({ nonce: 'a'.repeat(32) }) });
    const helloNoNonce = await fetch(H + '/node/hello', { method: 'POST', headers: hostHdr('test-node', NODE_SECRET), body: JSON.stringify({}) });
    const mine0 = await call('/api/hosts', A);
    const made = await call('/api/hosts/new', A, {});
    const newId = made.data.host ? made.data.host.id : '';
    const renamed = await call('/api/hosts/rename', A, { id: newId, nickname: 'brave-walrus-test' });
    const badNick = await call('/api/hosts/rename', A, { id: newId, nickname: 'Bad Name!' });
    const notMine = await call('/api/hosts/rename', U, { id: newId, nickname: 'stolen-yak' });
    const mine1 = await call('/api/hosts', A);
    // this host's config (what "Run on this PC" and the native AI host's download carry), for its owner only
    const cfgZip = await call('/api/hosts/config', A, { id: newId });
    const cfgJson = cfgZip.data.config || {};
    const cfgZipOther = await call('/api/hosts/config', U, { id: newId });
    // a hub-made host (UUID + secret from Settings) really connects: a second host program with that config
    const host2Dir = path.join(SB, 'host2'); fs.mkdirSync(host2Dir, { recursive: true }); for (const f of ['ai-node.js', 'host-relay.js']) fs.copyFileSync(path.join(SB, f), path.join(host2Dir, f));
    fs.writeFileSync(path.join(host2Dir, 'ai-node.json'), JSON.stringify({ ...cfgJson, providers: [], engines: {} }));
    const host2Log = fs.openSync(path.join(SCRATCH, 'host2.log'), 'a');
    const host2 = spawn(process.execPath, [path.join(host2Dir, 'ai-node.js')], { cwd: host2Dir, stdio: ['ignore', host2Log, host2Log], windowsHide: true });
    let h2 = null; for (let i = 0; i < 40; i++) { h2 = ((await call('/api/hosts', A)).data.mine || []).find((h) => h.id === newId); if (h2 && h2.online) break; await sleep(250); }
    try { if (WIN) execFileSync('taskkill', ['/pid', String(host2.pid), '/T', '/F'], { stdio: 'ignore' }); else host2.kill('SIGTERM'); } catch {}
    const host2Text = fs.readFileSync(path.join(SCRATCH, 'host2.log'), 'utf8');
    const removed = await call('/api/hosts/remove', A, { id: newId });
    const hubNodeSrc2 = fs.readFileSync(path.join(BRIDGE, 'ai-nodes.js'), 'utf8'), nodeSrc2 = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), html2 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R143', 'AI hosts: a host is a UUID + secret + nickname (adjective-animal); hello with the wrong secret → 401, without a nonce → 400; the handshake gives both sides the same e2e key (vr-… id) and the host tells the hub its user, providers (models, caps, allowed users) and engines; Settings → My AI hosts: new host (uuid, nickname), rename (bad nickname refused, only the owner), remove; that host\'s config (owner only), and a host run with it connects and shows online; [static] HKDF + AES-256-GCM on both sides, randomUUID ids, ai-hosts.json gitignored, the Settings panels exist',
      testHost.online === true && testHost.nickname === 'test-node' && /^vr-[0-9a-f]{24}$/.test(testHost.kid || '') && testHost.user === '' && (testHost.providers || []).join(',') === 'claude,codex,local' && (testHost.engines || []).length === 4
      && helloBad.status === 401 && helloNoNonce.status === 400 && mine0.status === 200 && mine0.data.mine.length === 0 && made.status === 200 && /^[0-9a-f-]{36}$/.test(newId) && /^[a-z]+-[a-z]+(-\d+)?$/.test(made.data.host.nickname) && made.data.host.user === 't-admin'
      && renamed.status === 200 && renamed.data.host.nickname === 'brave-walrus-test' && badNick.status === 400 && notMine.status === 400 && mine1.data.mine.length === 1
      && cfgZip.status === 200 && cfgJson.id === newId && cfgJson.token && cfgJson.token.length >= 24 && cfgJson.user === 't-admin' && cfgJson.hub === H && Array.isArray(cfgJson.providers) && cfgZipOther.status >= 400
      && !!h2 && h2.online === true && /^vr-[0-9a-f]{24}$/.test(h2.kid || '') && /connected to .* as "brave-walrus-test" \(t-admin\)/.test(host2Text) && removed.status === 200 && removed.data.mine.length === 0
      && /hkdfSync/.test(hubNodeSrc2) && /aes-256-gcm/.test(hubNodeSrc2) && /randomUUID/.test(hubNodeSrc2) && /hkdfSync/.test(nodeSrc2) && /aes-256-gcm/.test(nodeSrc2) && /^bridge\/ai-hosts\.json$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /id="hostBox"/.test(html2) && /id="srcBox"/.test(html2),
      `host ${JSON.stringify(testHost).slice(0, 160)}; hello bad ${helloBad.status} nonce ${helloNoNonce.status}; new ${made.status} ${JSON.stringify(made.data).slice(0, 120)}; rename ${renamed.status} bad ${badNick.status} other ${notMine.status}; config ${cfgZip.status} cfg id ok ${cfgJson.id === newId} other ${cfgZipOther.status}; host2 ${JSON.stringify(h2 || {}).slice(0, 80)} log "${host2Text.split('\n')[0].slice(0, 100)}"`);

    // Work with a host's Claude: the run happens HERE (this hub's worktree), only its model calls go to the host, which adds its login
    const remoteId = 'h:test-node/claude';
    const pickBad = await call('/api/model', A, { provider: 'h:test-node/codex', model: 'gpt-test' }); // not Claude → not for Work
    const pickNo = await call('/api/model', U, { provider: 'h:test-node/nope', model: '' });
    const pick = await call('/api/model', A, { provider: remoteId, model: 'claude-haiku-5-5' });
    await call('/api/workspace', A, { project: 'VibeRoller' }); // back from demo-app (R141): the run should land in the host's VibeRoller worktree
    const ckA = (await call('/api/check', A)).data;
    const r1 = await chat(A, 'where am I remote r144', { fresh: true });
    const t1 = texts(r1.events), info1 = (r1.events.find((e) => e.type === 'bridge_info') || {}).text || '';
    const sess1 = (r1.events.find((e) => e.type === 'system') || {}).session_id || '';
    const r2 = await chat(A, 'again r144'); // not fresh: resumed on the same host
    const sess2 = (r2.events.find((e) => e.type === 'system') || {}).session_id || '';
    const board1 = await chat(A, 'BOARDTOOL remote r144');
    const boardOut = (texts(board1.events).match(/BOARD:(\{.*?\})/) || [])[1] || '{}';
    const pcA = await chat(A, 'PCTOOL remote r144'); // the host shares AI only: the PC tools go to the person's own browser, never to the host
    const pcAOut = (texts(pcA.events).match(/PC:(\{.*\})/) || [])[1] || '{}';
    await call('/api/model', U, { provider: remoteId, model: 'claude-fable-5-1' }); // t-user is allowed to use it too
    const pcU = await chat(U, 'teammate run r144', { fresh: true });
    const pcUOut = texts(pcU.events);
    const slowR = chat(A, 'SLOW remote hold r144'); await sleep(900);
    const busyR = (await call('/api/status')).data.busy;
    const stopR = await call('/api/stop', A, {}); const slowDone = await slowR;
    const stoppedCode = (slowDone.events.find((e) => e.type === 'bridge_done') || {}).code;
    const usageA = (await call('/api/usage', A)).data;
    const usRun = await chat(A, 'USAGE remote r144', { fresh: true });
    const usageA2 = (await call('/api/usage', A)).data;
    // the relay from outside this machine, and with a made-up token: refused
    const relayBad = await fetch(H + '/llm/' + 'a'.repeat(48) + '/v1/messages', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    const relayOutside = await fetch(H + '/llm/' + 'a'.repeat(48) + '/v1/messages', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Forwarded-For': '8.8.8.8', 'CF-Connecting-IP': '8.8.8.8' }, body: '{}' });
    await chat(A, 'CAPTEST remote r144', { fresh: true });
    const capped = await chat(A, 'over the cap r144', { fresh: true });
    const capErr = (capped.events.find((e) => e.type === 'bridge_error') || {}).text || '';
    const ckCap = ((await call('/api/check', A)).data.providers || []).find((p) => p.id === remoteId) || {};
    await sleep(3500); // the fake window resets
    const afterCap = await chat(A, 'after the cap r144', { fresh: true });
    stopAiNode(); await sleep(5500);
    const offRun = await chat(A, 'host gone r144', { fresh: true });
    const offErr2 = (offRun.events.find((e) => e.type === 'bridge_error') || {}).text || '';
    const offProv = ((await call('/api/check', A)).data.providers || []).find((p) => p.id === remoteId) || {};
    startAiNode(); for (let i = 0; i < 60; i++) { if (((await call('/api/check', A)).data.aiNodes || []).some((h) => h.id === 'test-node' && h.online)) break; await sleep(250); }
    const aiNodeSrc = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), relaySrc = fs.readFileSync(path.join(BRIDGE, 'host-relay.js'), 'utf8');
    check('R144', 'a host shares its AI, not its computer: a person picks a host\'s Claude (not its local model / Codex for Work → 400; unknown → 400); Claude Code runs HERE in the person\'s own worktree on this hub (not on the host, no host login folder), with ANTHROPIC_BASE_URL on this hub\'s local relay; its model calls reach the provider through the host, which adds ITS login (the backend sees only the host\'s token, never the placeholder); a follow-up resumes the session; board + PC tools stay on this side (PC → the person\'s browser); a teammate allowed by the host can use it; Stop ends the run; /api/usage shows that host\'s windows (from the provider\'s usage headers); past its cap the hub refuses the run until the window resets; the relay refuses a made-up token and anyone from outside; with the host gone the run fails at once and the provider shows offline; [static] the host refuses work jobs unless its config says allowWork, and relays only to the two fixed provider addresses',
      pickBad.status === 400 && pickNo.status === 400 && pick.status === 200 && pick.data.provider === remoteId && ckA.provider === remoteId && ckA.effectiveProvider === remoteId
      && /work happens here on the server/.test(info1) && /cwd=.*worktrees[\\/]t-admin/.test(t1) && !/cfg=.*host-claude-login/.test(t1) && /relay=200:upstream ok/.test(t1) && /model=claude-haiku-5-5/.test(t1) && !!sess1 && sess1 === sess2
      && /"made":true/.test(boardOut) && /"claimed":true/.test(boardOut) && /"noted":true/.test(boardOut) && /fake-listing/.test(pcAOut) && /relay=200:upstream ok/.test(pcUOut) && /cwd=.*worktrees[\\/]t-user/.test(pcUOut)
      && busyR === true && stopR.status === 200 && slowDone.events.some((e) => e.type === 'bridge_done') && usageA.of === remoteId && usageA2.of === remoteId && usageA2.five_hour.source === 'claude' && Math.abs(usageA2.five_hour.utilization - 0.42) < 0.01 && /echo: USAGE/.test(texts(usRun.events))
      && relayBad.status === 401 && relayOutside.status === 401 && /cfg\.allowWork === true/.test(aiNodeSrc) && /https:\/\/api\.anthropic\.com/.test(relaySrc) && /https:\/\/chatgpt\.com\/backend-api\/codex/.test(relaySrc)
      && /cap/.test(capErr) && !!ckCap.capped && /echo: after the cap/.test(texts(afterCap.events)) && /offline/.test(offErr2) && offProv.online === false,
      `pick ${pick.status}/${pickBad.status}/${pickNo.status} eff ${ckA.effectiveProvider}; info "${info1}"; run "${t1.slice(0, 200)}"; sess ${sess1 === sess2}; board ${boardOut}; pcA ${pcAOut.slice(0, 60)}; teammate "${pcUOut.slice(0, 120)}"; busy ${busyR} stop ${stoppedCode}; usage ${usageA2.of} ${JSON.stringify(usageA2.five_hour)}; relay bad ${relayBad.status} outside ${relayOutside.status}; cap "${capErr.slice(0, 80)}" ${!!ckCap.capped}; after ${/echo: after/.test(texts(afterCap.events))}; off "${offErr2.slice(0, 60)}" ${offProv.online}`);

    // R147: a host's exact usage numbers (Anthropic's headers) are what people see, and a restart forgets nothing
    const uDir = fs.mkdtempSync(path.join(SCRATCH, 'usage-'));
    const mkUsage = () => require(path.join(BRIDGE, 'usage.js'))({ ROOT: uDir, config: {}, readJson: (f, d) => { try { return JSON.parse(fs.readFileSync(f, 'utf8')); } catch { return d; } }, writeJson: (f, v) => fs.writeFileSync(f, JSON.stringify(v)) });
    const u1 = mkUsage(), src = 'h:x/claude', in1h = Date.now() + 3600e3, in3d = Date.now() + 3 * 86400e3, acctU = { state: {} };
    u1.noteWindows(src, { five_hour: { utilization: 0.31, resetsAt: in1h, status: 'allowed' }, seven_day: { utilization: 0.64, resetsAt: in3d, status: 'allowed' } });
    u1.noteUsage(acctU, { type: 'rate_limit_event', rate_limit_info: { rateLimitType: 'five_hour', status: 'allowed_warning', resetsAt: Math.floor(in1h / 1000) } }, src); // Claude Code's event: no number
    const st1 = u1.status(acctU, src);
    await sleep(1300); // usage.json is written a second after a change
    const st2 = mkUsage().status(acctU, src); // a fresh start reads it back
    const nodesSrc = fs.readFileSync(path.join(BRIDGE, 'ai-nodes.js'), 'utf8'), serverSrc = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8');
    check('R147', 'accurate 5h / 7d: the windows an AI host\'s Claude reports in Anthropic\'s usage headers (both 5h and 7d) reach /api/usage; Claude Code\'s events without a number never wipe them (only the status follows); they are kept in usage.json, so a restart shows them at once; a missing number is unknown, never 0 %; the team tools\' hub_status reads the windows of the AI the person uses',
      usageA2.seven_day && usageA2.seven_day.source === 'claude' && Math.abs(usageA2.seven_day.utilization - 0.37) < 0.01
      && st1.five_hour.source === 'claude' && Math.abs(st1.five_hour.utilization - 0.31) < 0.001 && st1.five_hour.status === 'allowed_warning' && Math.abs(st1.seven_day.utilization - 0.64) < 0.001
      && Math.abs(st2.five_hour.utilization - 0.31) < 0.001 && Math.abs(st2.seven_day.utilization - 0.64) < 0.001 && st2.seven_day.resetsAt === in3d
      && /v == null \|\| v === '' \? NaN/.test(nodesSrc) && /onUsage: usage\.noteWindows, savedUsage: usage\.windows/.test(serverSrc) && /usageStatus: \(a\) => usage\.status\(a, usageSourceOf\(a\)\)/.test(serverSrc),
      `api 7d ${JSON.stringify(usageA2.seven_day)}; unit ${JSON.stringify(st1.five_hour)} / ${JSON.stringify(st1.seven_day)}; after restart ${JSON.stringify(st2.five_hour)} / ${JSON.stringify(st2.seven_day)}`);

    // R152: older AI hosts keep working, and hosts update themselves from the hub
    const oh = { 'X-VR-Host': 'old-node', 'X-VR-Host-Token': OLD_NODE_SECRET, 'Content-Type': 'application/json' };
    // a 2.x host: hello without caps, offering its Claude as "claude-cli"; it knows no login relays (404 "does not lend")
    const oldHello = await fetch(H + '/node/hello', { method: 'POST', headers: oh, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex'), user: 't-admin', providers: [{ id: 'claude', kind: 'claude-cli', label: 'Old Claude', models: ['claude-haiku-5-5'], allowedUsers: ['t-user'] }], engines: [], version: '2.0.0' }) });
    const oldHelloJ = await oldHello.json().catch(() => ({}));
    let probed = '';
    for (let i = 0; i < 12 && !probed; i++) {
      const pr = await fetch(H + '/node/poll', { headers: oh, signal: AbortSignal.timeout(30000) }).then((r) => r.json()).catch(() => ({}));
      for (const j of pr.jobs || []) {
        probed = String(j.engine) + ' ' + String(j.path);
        await fetch(H + '/node/reply', { method: 'POST', headers: oh, body: JSON.stringify({ id: j.id, status: 404, headers: { 'content-type': 'application/json' }, body: Buffer.from('{"error":"This host does not lend the ' + j.engine + ' engine."}').toString('base64') }) });
      }
    }
    await sleep(300);
    const oldProv = ((await call('/api/check', U)).data.providers || []).find((p) => p.id === 'h:old-node/claude') || {};
    const newProv = ((await call('/api/check', U)).data.providers || []).find((p) => p.id === 'h:test-node/claude') || {};
    // its Work would run on t-admin's computer: never for anyone else (4.1.0)
    await call('/api/model', U, { provider: 'h:old-node/claude', model: 'claude-haiku-5-5' });
    const notOwnerRun = await chat(U, 'run on their computer', { fresh: true });
    const notOwnerErr = (notOwnerRun.events.find((e) => e.type === 'bridge_error') || {}).text || '';
    await call('/api/model', U, { model: 'claude-opus-5-5', provider: 'host' });
    // updates: a signed manifest of the host's files as the hub runs them, and the files themselves
    const th = { 'X-VR-Host': 'test-node', 'X-VR-Host-Token': NODE_SECRET };
    const manR = await fetch(H + '/node/update', { headers: th }), man = await manR.json().catch(() => ({}));
    const sigOk = !!man.sig && man.sig === crypto.createHmac('sha256', NODE_SECRET).update(JSON.stringify({ version: man.version, commit: man.commit, files: man.files })).digest('hex');
    const aiEntry = (man.files || []).find((f) => f.name === 'ai-node.js') || {};
    const fileR = await fetch(H + '/node/update/file/ai-node.js', { headers: th }), fileB = Buffer.from(await fileR.arrayBuffer());
    const fileOk = fileR.status === 200 && crypto.createHash('sha256').update(fileB).digest('hex') === aiEntry.sha256 && aiEntry.sha256 === crypto.createHash('sha256').update(fs.readFileSync(path.join(SB, 'ai-node.js'))).digest('hex');
    const notHost = (await fetch(H + '/node/update/file/config.json', { headers: th })).status, sneaky = (await fetch(H + '/node/update/file/..%2Fauth.json', { headers: th })).status, anon = (await fetch(H + '/node/update')).status;
    const selfT = spawnSync(process.execPath, [path.join(SB, 'ai-node.js'), '--selftest', '--config', path.join(SB, 'ai-node.json')], { encoding: 'utf8', timeout: 30000 });
    // a real host updating itself end to end: its copy of host-relay.js differs from the hub's → download, selftest,
    // swap with a backup, exit 3; the next start reaches the hub and confirms the update
    const uh = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-hostupd-')), upd = { first: null, swapped: false, trial: false, backup: false, confirmed: false };
    try {
      for (const f of (man.files || [])) fs.copyFileSync(path.join(SB, f.name), path.join(uh, f.name));
      fs.appendFileSync(path.join(uh, 'host-relay.js'), '\n// an older copy\n');
      fs.writeFileSync(path.join(uh, 'ai-node.json'), JSON.stringify({ hub: H, id: 'upd-node', token: OLD_NODE_SECRET + '-upd', user: 't-admin', providers: [], engines: {} }));
      const runHost = (ms) => new Promise((resolve) => { const pr = spawn(process.execPath, [path.join(uh, 'ai-node.js')], { cwd: uh, stdio: ['ignore', 'pipe', 'pipe'] }); let out = ''; pr.stdout.on('data', (d) => { out += d; }); pr.stderr.on('data', (d) => { out += d; }); const t = setTimeout(() => pr.kill(), ms); pr.on('exit', (code) => { clearTimeout(t); resolve({ code, out }); }); });
      upd.first = await runHost(30000);
      upd.swapped = upd.first.code === 3 && fs.readFileSync(path.join(uh, 'host-relay.js'), 'utf8') === fs.readFileSync(path.join(SB, 'host-relay.js'), 'utf8');
      upd.trial = fs.existsSync(path.join(uh, '.update-trial.json')); upd.backup = /an older copy/.test(fs.readFileSync(path.join(uh, '.update-backup', 'host-relay.js'), 'utf8'));
      const second = await runHost(9000);
      upd.confirmed = /update to .* confirmed/.test(second.out) && !fs.existsSync(path.join(uh, '.update-trial.json')) && !fs.existsSync(path.join(uh, '.update-backup'));
      upd.first = upd.first.code + ' ' + upd.first.out.replace(/\s+/g, ' ').slice(-160);
    } catch (e) { upd.error = e.message; } finally { try { fs.rmSync(uh, { recursive: true, force: true }); } catch {} }
    const nodeSrc152 = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), bat = fs.readFileSync(path.join(BRIDGE, 'start-node.bat'), 'utf8'), nsh = fs.readFileSync(path.join(BRIDGE, 'start-node.sh'), 'utf8');
    check('R152', 'older AI hosts keep working: a 2.x host (no caps, its Claude as claude-cli) is asked once with a relay job to an address nobody forwards, answers 404 "does not lend", and from then on its Claude runs Work the old way (claude-cli-work) instead of failing; a current host says it can relay + update and is not probed; hosts update from the hub: GET /node/update is a manifest of the host files (sha256) signed with that host\'s own secret, each file is served byte-exact, nothing else is (config, ../) and nobody else gets it (401); the host tries a new version with --selftest before swapping, keeps a backup, puts it back after 3 starts that never reach the hub, never swaps launchers, and "autoUpdate": false switches it off; the launchers start the host again on exit 3 (update) and after a crash',
      oldHello.status === 200 && oldHelloJ.update && typeof oldHelloJ.update.version === 'string' && /^relay:claude \/v1\/__viberoller_probe$/.test(probed) && oldProv.remoteKind === 'claude-cli-work' && newProv.remoteKind === 'claude-relay'
      && /only its owner may work on that computer/.test(notOwnerErr)
      && manR.status === 200 && sigOk && fileOk && notHost === 404 && sneaky === 404 && anon === 401 && selfT.status === 0 && /selftest: .* loads/.test(selfT.stdout)
      && upd.swapped && upd.trial && upd.backup && upd.confirmed
      && /CAPS = \['relay', 'update'\]/.test(nodeSrc152) && /cfg\.autoUpdate !== false/.test(nodeSrc152) && /t\.starts <= 3/.test(nodeSrc152) && /selftest\(UPD\.stage\)/.test(nodeSrc152) && /timingSafeEqual\(got, want\)/.test(nodeSrc152)
      && /if errorlevel 3 goto run/.test(bat) && /if errorlevel 4 goto crashed/.test(bat) && /3\) ;;/.test(nsh) && /sleep 10/.test(nsh),
      `old hello ${oldHello.status} probed "${probed}" old ${oldProv.remoteKind} new ${newProv.remoteKind}; not owner "${notOwnerErr.slice(0, 80)}"; manifest ${manR.status} sig ${sigOk} files ${(man.files || []).map((f) => f.name).join(',')}; file ${fileR.status} ok ${fileOk}; other ${notHost}/${sneaky} anon ${anon}; selftest ${selfT.status} "${String(selfT.stdout || selfT.stderr).trim().slice(0, 120)}"; self-update ${JSON.stringify(upd)}`);

    // R157: the heart & knife control app (bridge/vr-host.js) and what the host does for it
    {
      const vh = (dir, ...a) => spawnSync(process.execPath, [path.join(dir, 'vr-host.js'), ...a], { cwd: dir, encoding: 'utf8', timeout: 30000 });
      const stFile = (() => { try { return JSON.parse(fs.readFileSync(path.join(SB, 'ai-node.status.json'), 'utf8')); } catch { return {}; } })();
      const stOut = vh(SB, 'status').stdout;
      // settings on a copy (a change to the live config would make the test host restart itself on it)
      const cdir = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-host-'));
      fs.copyFileSync(path.join(SB, 'vr-host.js'), path.join(cdir, 'vr-host.js')); fs.copyFileSync(path.join(SB, 'ai-node.json'), path.join(cdir, 'ai-node.json'));
      const sets = [vh(cdir, 'set', 'update.quietMinutes', '7'), vh(cdir, 'set', 'provider.claude.shareWithGroup', 'yes'), vh(cdir, 'set', 'provider.claude.allowedUsers', 'mills, sponge'), vh(cdir, 'set', 'engine.image.enabled', 'off'), vh(cdir, 'set', 'allowWork', 'off')];
      const badN = vh(cdir, 'set', 'update.quietMinutes', '999').status, badHub = vh(cdir, 'set', 'hub', 'http://evil.example').status, badKey = vh(cdir, 'set', 'nope', '1').status;
      const ccfg = JSON.parse(fs.readFileSync(path.join(cdir, 'ai-node.json'), 'utf8')), mode = fs.statSync(path.join(cdir, 'ai-node.json')).mode & 0o777;
      const listOut = vh(cdir, 'settings').stdout;
      fs.rmSync(cdir, { recursive: true, force: true });
      // pause / resume through the app
      vh(SB, 'pause'); const pausedFile = fs.existsSync(path.join(SB, 'ai-host.off')); vh(SB, 'resume'); const resumed = !fs.existsSync(path.join(SB, 'ai-host.off'));
      // restart and stop requests: the host leaves with 3 / 0 (its launcher or service starts it again after a restart)
      const waitExit = async () => { for (let i = 0; i < 40; i++) { if (aiNodeProc && aiNodeProc.exitCode !== null) return aiNodeProc.exitCode; await sleep(250); } return 'still running'; };
      const backOnline = async () => { stopAiNode(); startAiNode(); for (let i = 0; i < 60; i++) { if (((await call('/api/check', A)).data.aiNodes || []).some((h) => h.id === 'test-node' && h.online)) return true; await sleep(250); } return false; };
      vh(SB, 'restart', '--now'); const restartCode = await waitExit(); const back1 = await backOnline();
      vh(SB, 'stop', '--now'); const stopCode = await waitExit(); const back2 = await backOnline();
      // a hub over plain http on another machine: the host refuses to start
      const insecure = path.join(SCRATCH, 'insecure-node.json'); fs.writeFileSync(insecure, JSON.stringify({ hub: 'http://example.com', id: 'x', token: 'y'.repeat(30) }));
      const refused = spawnSync(process.execPath, [path.join(SB, 'ai-node.js'), '--config', insecure], { encoding: 'utf8', timeout: 10000 });
      // "@group": shared with the hub's team group (config hostGroup, here t-user), whatever names are in it
      const gh = { 'X-VR-Host': 'group-node', 'X-VR-Host-Token': OLD_NODE_SECRET + '-grp', 'Content-Type': 'application/json' };
      await fetch(H + '/node/hello', { method: 'POST', headers: { ...gh, 'X-VR-Host-Version': '3.6.0' }, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex'), providers: [{ id: 'grp', kind: 'claude-relay', label: 'Group Claude', models: ['m'], allowedUsers: ['@group'] }], engines: [], caps: ['relay', 'update'] }) });
      const seesU = ((await call('/api/check', U)).data.providers || []).some((p) => p.id === 'h:group-node/grp'), seesA = ((await call('/api/check', A)).data.providers || []).some((p) => p.id === 'h:group-node/grp');
      const host157 = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), app157 = fs.readFileSync(path.join(BRIDGE, 'vr-host.js'), 'utf8'), setup157 = fs.readFileSync(path.join(BRIDGE, 'vr-setup.js'), 'utf8');
      check('R157', 'the heart & knife control app: it reads the host\'s live status (running, connected to the hub, heartbeat, in use, updates) and changes every setting in ai-node.json (validated: a bad number, a plain-http hub, an unknown setting are refused; the file stays private), pauses / resumes, and asks the host to restart (exit 3) or stop (exit 0), which it does within seconds; the host refuses a plain-http hub on another machine, offers a provider to the hub\'s team group with "@group" (the hub expands it), and guards its engines (use, never manage); [static] start at boot: service / sign-in / off on Windows, system / user service / off on Linux; an installed package opens the control app',
        stFile.connected === true && stFile.pid > 0 && /Running\s+YES/.test(stOut) && /CONNECTED/.test(stOut)
        && sets.every((r) => r.status === 0) && ccfg.update.quietMinutes === 7 && ccfg.providers.find((p) => p.id === 'claude').shareWithGroup === true && ccfg.providers.find((p) => p.id === 'claude').allowedUsers.join(',') === 'mills,sponge'
        && !ccfg.engines.image && ccfg.enginesOff && !!ccfg.enginesOff.image && ccfg.allowWork === false && badN !== 0 && badHub !== 0 && badKey !== 0 && mode === 0o600 && /provider\.claude\.max5h/.test(listOut)
        && pausedFile && resumed && restartCode === 3 && back1 && stopCode === 0 && back2 && refused.status === 1 && /not https/.test(refused.stderr) && seesU && !seesA
        && /SEC\.engineGuard !== false/.test(host157) && /\/api\\\/\(delete\|pull\|push\|create\|copy\|blobs\)/.test(host157) && /\['service', 'sign-in', 'off'\]/.test(app157) && /\['service', 'user', 'off'\]/.test(app157) && /control\(HERE\)/.test(setup157),
        `status ${JSON.stringify({ c: stFile.connected, pid: stFile.pid })} out "${stOut.split('\n').slice(2, 6).join(' | ').slice(0, 200)}"; sets ${sets.map((r) => r.status).join('')} bad ${badN}/${badHub}/${badKey} mode ${mode.toString(8)}; pause ${pausedFile}/${resumed}; restart ${restartCode} back ${back1}; stop ${stopCode} back ${back2}; insecure ${refused.status}; group u ${seesU} a ${seesA}`);
    }

    // local models and the host's Claude through the API (bridge/openai.js → providers → the host)
    const keyR = await call('/api/apikey/rotate', A, {});
    const setLocal = await call('/api/apikey/settings', A, { provider: 'h:test-node/local', model: 'tiny-local' });
    const v1Models = await fetch(H + '/v1/models', { headers: { Authorization: 'Bearer ' + keyR.data.key } }).then((r) => r.json());
    const v1Local = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'tiny-local', messages: [{ role: 'user', content: 'through the host r145' }] }) }).then((r) => r.json());
    const v1LocalStream = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'tiny-local', stream: true, messages: [{ role: 'user', content: 'stream r145' }] }) }).then((r) => r.text());
    await call('/api/apikey/settings', A, { provider: remoteId, model: 'claude-fable-5-1' });
    const v1Claude = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'claude-fable-5-1', messages: [{ role: 'user', content: 'sandbox r145' }] }) }).then((r) => r.json());
    const v1ClaudeStream = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'claude-fable-5-1', stream: true, messages: [{ role: 'user', content: 'claude stream r145' }] }) }).then((r) => r.text());
    const setCodex = await call('/api/apikey/settings', A, { provider: 'h:test-node/codex', model: 'gpt-test' });
    const v1Codex = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'gpt-test', messages: [{ role: 'user', content: 'codex r145' }] }) }).then((r) => r.json());
    const codexStream = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + keyR.data.key }, body: JSON.stringify({ model: 'gpt-test', stream: true, messages: [{ role: 'user', content: 'codex stream r145' }] }) }).then((r) => r.text());
    const codexPieces = (codexStream.match(/"content":"[^"]+"/g) || []).length;
    const streamed = v1LocalStream.split('\n').filter((l) => l.startsWith('data: {')).map((l) => { try { return ((JSON.parse(l.slice(6)).choices || [])[0] || {}).delta?.content || ''; } catch { return ''; } }).join('');
    const streamPieces = (v1LocalStream.match(/"content":"[^"]+"/g) || []).length;
    const setOther = await call('/api/apikey/settings', U, { provider: 'h:test-node/local' }); // t-user is allowed; t-admin (the owner) sees it too
    const ownerSees = ((await call('/api/check', A)).data.providers || []).some((p) => p.id === 'h:test-node/local');
    await call('/api/apikey/revoke', A, {}); await call('/api/model', A, { provider: 'host', model: '' }); await call('/api/model', U, { provider: 'host', model: 'claude-opus-5-5' }); // back to what the restart check (R9) expects for t-user
    check('R145', 'the API through a host, streaming: its local model (OpenAI-compatible endpoint) answers plain and streamed piece by piece; its Claude login answers through the relay (Messages API, the Claude Code identity block first, the host\'s token only) plain and streamed; its ChatGPT / Codex login answers through the relay (Responses API, the host\'s token + account) plain and streamed; models listed; the owner and the allowed teammate may use them',
      keyR.status === 200 && setLocal.status === 200 && (v1Models.data || []).some((m) => m.id === 'tiny-local') && /oai says: through the host r145/.test(((v1Local.choices || [])[0] || {}).message?.content || '') && /oai says: stream r145/.test(streamed) && streamPieces >= 3 && /\[DONE\]/.test(v1LocalStream)
      && /upstream ok: sandbox r145 \[cc\]/.test(((v1Claude.choices || [])[0] || {}).message?.content || '') && /upstream/.test(v1ClaudeStream) && (v1ClaudeStream.match(/"content":"[^"]+"/g) || []).length >= 3
      && setCodex.status === 200 && /codex says: codex r145/.test(((v1Codex.choices || [])[0] || {}).message?.content || '') && codexPieces >= 3 && setOther.status === 200 && ownerSees === true,
      `models ${(v1Models.data || []).map((m) => m.id).join(',')}; local "${(((v1Local.choices || [])[0] || {}).message || {}).content || JSON.stringify(v1Local).slice(0, 80)}"; streamed ${streamPieces} pieces; claude "${((((v1Claude.choices || [])[0] || {}).message || {}).content || JSON.stringify(v1Claude).slice(0, 120)).slice(0, 120)}"; codex "${((((v1Codex.choices || [])[0] || {}).message || {}).content || JSON.stringify(v1Codex).slice(0, 120)).slice(0, 80)}" ${codexPieces} pieces`);
  }
  // The move kit: everything a person runs on the server / the old host, checked for shape (they cannot run here).
  const dk = (f) => fs.readFileSync(path.join(MILLS, 'deploy', f), 'utf8');
  const setupSh = dk('setup-server.sh'), restoreSh = dk('restore-data.sh'), packCmd = dk('pack-data.cmd'), packSh = dk('pack-data.sh'), deployYml = fs.readFileSync(path.join(MILLS, '.forgejo', 'workflows', 'deploy.yml'), 'utf8');
  const pagesYml = fs.readFileSync(path.join(MILLS, '.github', 'workflows', 'pages.yml'), 'utf8');
  const dataList = ['bridge/auth.json', 'bridge/profiles', 'Chat', 'Archive', 'Board', 'Notes', 'Avatars'];
  const autostart = fs.readFileSync(path.join(BRIDGE, 'autostart-windows.cmd'), 'utf8'), hostTool = fs.readFileSync(path.join(BRIDGE, 'host-tools', 'HostTool.cs'), 'utf8'), wtSrc = fs.readFileSync(path.join(BRIDGE, 'worktree.js'), 'utf8');
  const autoSh = dk('autodeploy.sh'), autoTimer = dk('viberoller-autodeploy.timer'), hubUnit = dk('viberoller-hub.service'), portal = dk('nginx-portal.conf'), localEx = dk('config.local.example.json');
  check('R139', '[static] the move kit for the real server layout (no root): deploy/setup-server.sh runs as the hub\'s account and makes the user-local Node + Claude Code, the live checkout, the server settings with a fresh node secret, the systemd USER services (hub + auto-deploy timer), the nginx site files in deploy-ready, and prints the root steps (linger, nginx, DNS + certificate) and the people steps; the auto-deploy fast-forwards main every minute, checks the scripts parse and asks for a graceful restart; pack-data (.cmd + .sh) packs exactly the data folders with the hub stopped, no code, and copies them by scp when given a host; restore-data.sh runs as the account, refuses anything but hub data and never overwrites existing accounts; the portal site is a plain front door (TLS, streaming unbuffered, /internal/ blocked, forged forwarding headers dropped; the sign-in is the protection, no inside / outside rule); deploy.yml only calls a key\'s one command; pages.yml runs on GitHub only; on the old host the autostart and the heart start the AI node once ai-node.json exists, the node has a single-instance lock, and worktree.js repairs worktree pointers after the host folder moves',
    /not root/.test(setupSh) && !/useradd/.test(setupSh) && /viberoller-hub\.service/.test(setupSh) && /viberoller-autodeploy\.timer/.test(setupSh) && /systemctl --user/.test(setupSh) && /deploy-ready/.test(setupSh) && /claude login/.test(setupSh) && /enable-linger/.test(setupSh) && /__NODE_SECRET__/.test(setupSh) && /nginx-portal\.conf/.test(setupSh)
    && /WantedBy=default\.target/.test(hubUnit) && /__BASE__\/node\/current\/bin\/node run\.js --no-tunnel/.test(hubUnit) && /UMask=0007/.test(hubUnit) && /projectsRoot/.test(localEx) && /__NODE_SECRET__/.test(localEx)
    && /--ff-only/.test(autoSh) && /node --check/.test(autoSh) && /bridge\/\.restart/.test(autoSh) && /git status --porcelain/.test(autoSh) && /OnUnitActiveSec=1min/.test(autoTimer)
    && !/vr_inside|droids/.test(portal) && /server_name portal\.unityailab\.com portal\.git\.unityailab\.com/.test(portal) && /proxy_pass http:\/\/127\.0\.0\.1:7777/.test(portal) && /CF-Connecting-IP ""/.test(portal) && /location \^~ \/internal\/ \{ return 404; \}/.test(portal) && /proxy_buffering off/.test(portal)
    && dataList.every((d) => packCmd.includes(d.replace('/', '\\')) && packSh.includes(d) && restoreSh.includes(d.split('/').pop())) && /api\/status/.test(packCmd) && /api\/status/.test(packSh) && /scp /.test(packCmd) && /scp /.test(packSh) && /Refusing to overwrite/.test(restoreSh) && /agent-key\.json/.test(restoreSh) && /systemctl --user stop viberoller-hub/.test(restoreSh) && /not root/.test(restoreSh)
    && /"\$\{USER\}@\$\{HOST\}" deploy/.test(deployYml) && !/sh -s/.test(deployYml) && /github\.server_url == 'https:\/\/github\.com'/.test(pagesYml)
    && /ai-node\.json/.test(autostart) && /start-node\.bat/.test(autostart) && /NodeRuns\(/.test(hostTool) && /start-node\.bat/.test(hostTool) && /LOCK_FILE/.test(nodeSrc) && /process\.exit\(2\)/.test(nodeSrc)
    && /function repair\(/.test(wtSrc) && /if \(!healthy\(dir\)\) git\(p\.repo, 'worktree', 'repair', dir\)/.test(wtSrc) && fs.existsSync(path.join(BRIDGE, 'start-node.bat')) && fs.existsSync(path.join(BRIDGE, 'start-node.sh')) && fs.existsSync(path.join(BRIDGE, 'ai-node.example.json')),
    'see the check text');
  }

  // The Download panel (the native programs only, for people with an account; the Rust hub's alone since 4.0.0), and the
  // page served by the hub itself.
  {
    const anon = await call('/api/downloads'), anonNative = await call('/api/native?app=vr-desktop&target=x86_64-unknown-linux-gnu');
    const dl = await call('/api/downloads', U);
    const items = dl.data.items || [];
    const unknown = await fetch(H + '/api/downloads/ai-host-windows.zip', { headers: { Authorization: 'Bearer ' + U } });
    const pubGone = await Promise.all(['/api/pub/versions', '/api/pub/desktop-linux.zip', '/api/pub/native?app=vr-desktop&target=x86_64-unknown-linux-gnu'].map((p) => fetch(H + p).then((r) => r.status)));
    const pubSigned = (await fetch(H + '/api/pub/versions', { headers: { Authorization: 'Bearer ' + U } })).status;
    const page = await (await fetch(H + '/')).text();
    const html = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), appJs = fs.readFileSync(path.join(MILLS, 'web', 'app.js'), 'utf8');
    check('R142', 'Download: only the native programs and only for people with an account (anonymous → 401 for the list and the desktop\'s release; the public /api/pub/* answers 410 to everyone; the old script packages are gone → 404); the Rust hub hands them out (nothing built here yet: an empty list), the JavaScript hub on its own says they come from the Rust hub (503); the page the hub serves says so (it then talks to its own origin, whatever name reached it); the Download button sits above Settings in the menu and in the app\'s user panel',
      anon.status === 401 && anonNative.status === 401 && pubGone.every((s) => s === 410) && pubSigned === 410
      && (RUST_HUB ? dl.status === 200 && Array.isArray(items) && items.every((it) => it.native === true && /^(app|host)-/.test(it.id)) && unknown.status === 404 : dl.status === 503 && unknown.status === 503)
      && !fs.existsSync(path.join(BRIDGE, 'downloads.js')) && !fs.existsSync(path.join(BRIDGE, 'desktop'))
      && /'hub' === 'hub'/.test(page) && !/__MILLS_SERVED__/.test(page) && /__MILLS_SERVED__/.test(html) && /SERVED_BY_HUB \|\|/.test(html)
      && /class="menuFoot menuDl"/.test(html) && html.indexOf('data-downloads') < html.indexOf('id="goSettings"') && /symbol id="i-download"/.test(html) && /openDownloads\(\)/.test(appJs) && /ic\('download'\)/.test(appJs)
      // Work's bar: Download right above Settings; Settings has its own Downloads tab, which every Download button opens
      && (() => { const rail = (html.match(/<nav id="rail">[\s\S]*?<\/nav>/) || [''])[0]; return rail.indexOf('data-downloads') > -1 && rail.indexOf('data-downloads') < rail.indexOf('data-settings'); })()
      && /data-settab="downloads"/.test(html) && /<div data-setpane="downloads" hidden>[\s\S]*id="dlBody"[\s\S]*id="appBox"/.test(html) && /PROFILE\.tab\('downloads'\)/.test(html) && !/id="downloads" class="modal"/.test(html),
      `anon ${anon.status} native ${anonNative.status} pub ${pubGone.join(',')} signed ${pubSigned}; list ${dl.status} ${items.map((x) => x.id + ':' + x.bytes).join(' ')}; old package ${unknown.status}; page served ${/'hub' === 'hub'/.test(page)}; menu order ${html.indexOf('data-downloads') < html.indexOf('id="goSettings"')}`);
  }

  { // The hand-off (bridge/handoff.js): the running hub packs its own data, pauses writes, and can turn this machine into an AI host.
    const hoJs = path.join(SB, 'handoff.js');
    const ho = (...a) => spawnSync(process.execPath, [hoJs, ...a], { cwd: SB, encoding: 'utf8' });
    const packed = ho('pack');
    const outFile = (packed.stdout.match(/→ (.+?\.tar\.gz)/) || [])[1] || '';
    // read the archive like tar would: gunzip, walk the 512-byte headers
    const tarNames = [];
    if (outFile && fs.existsSync(outFile)) {
      const buf = require('zlib').gunzipSync(fs.readFileSync(outFile)); let i = 0;
      while (i + 512 <= buf.length && buf[i] !== 0) {
        const name = buf.toString('utf8', i, i + 100).replace(/\0.*$/, ''), prefix = buf.toString('utf8', i + 345, i + 500).replace(/\0.*$/, ''), size = parseInt(buf.toString('ascii', i + 124, i + 136).replace(/\0.*$/, '').trim() || '0', 8);
        let sum = 0; for (let k = 0; k < 512; k++) sum += k >= 148 && k < 156 ? 32 : buf[i + k];
        const stored = parseInt(buf.toString('ascii', i + 148, i + 156).replace(/[\0 ]/g, ''), 8);
        tarNames.push((prefix ? prefix + '/' : '') + name + (sum === stored ? '' : ' (BAD CHECKSUM)'));
        i += 512 + Math.ceil(size / 512) * 512;
      }
      try { fs.unlinkSync(outFile); } catch {}
    }
    const maintOn = fs.existsSync(path.join(SB, '.maintenance'));
    const blockedSend = (await call('/api/room/send', U, { iv: 'x', ct: 'x', keys: {} })).status, blockedBoard = (await call('/api/board/create', A, { title: 'during the move', status: 'todo' })).status, blockedLogin = (await login('t-user', 'pw-t-user-0001', devA)).status;
    const stillRead = (await call('/api/board', U)).status, stillChat = (await chat(U, 'during the move r146', { fresh: true })).events.some((e) => e.type === 'result');
    const undone = ho('undo');
    const maintOff = !fs.existsSync(path.join(SB, '.maintenance'));
    const afterUndo = (await call('/api/board/create', A, { title: 'after the move r146', status: 'todo' })).status;
    const dry = ho('switch', '--dry-run');
    const noCfg = (() => { const keep = path.join(SB, 'ai-node.json'), tmp = keep + '.away'; fs.renameSync(keep, tmp); const r = ho('switch', '--dry-run'); fs.renameSync(tmp, keep); return r; })();
    // run.js in handed-off mode starts the AI host instead of the hub: a copy of the scratch bridge with the flag
    const SB2 = path.join(SCRATCH, 'bridge-handoff'); fs.mkdirSync(SB2, { recursive: true });
    for (const f of fs.readdirSync(SB)) if (/\.js$/.test(f) || f === 'config.json') fs.copyFileSync(path.join(SB, f), path.join(SB2, f));
    fs.writeFileSync(path.join(SB2, 'ai-node.json'), JSON.stringify({ hub: H, id: 'nobody-here', token: 'x'.repeat(32), engines: {} }));
    fs.writeFileSync(path.join(SB2, '.handoff'), 'test\n');
    const ho2Log = fs.openSync(path.join(SCRATCH, 'handoff-run.log'), 'a');
    const run2 = spawn(process.execPath, [path.join(SB2, 'run.js'), '--no-tunnel'], { cwd: SB2, stdio: ['ignore', ho2Log, ho2Log], windowsHide: true });
    let lockSeen = false; for (let i = 0; i < 24 && !lockSeen; i++) { await sleep(250); lockSeen = fs.existsSync(path.join(SB2, 'ai-node.lock')); }
    const hubStillOurs = (await call('/api/status')).data.pid === (await call('/api/status')).data.pid && (await call('/api/status')).status === 200;
    try { if (WIN) execFileSync('taskkill', ['/pid', String(run2.pid), '/T', '/F'], { stdio: 'ignore' }); else run2.kill('SIGTERM'); } catch {}
    await sleep(500);
    const run2Log = fs.readFileSync(path.join(SCRATCH, 'handoff-run.log'), 'utf8');
    const hoSrc = fs.readFileSync(path.join(BRIDGE, 'handoff.js'), 'utf8'), runSrc = fs.readFileSync(path.join(BRIDGE, 'run.js'), 'utf8'), cfgTools = (readJson(path.join(BRIDGE, 'config.json'), {}).allowedTools || []).join(',');
    check('R146', 'the hand-off: "handoff.js pack" packs exactly the hub\'s data (accounts, chat + key, board, notes…, no code) into a valid tar.gz (good checksums) and pauses writes: sign-in, chat sends and board changes answer 503 "moved" while reading and the Work run keep going; "undo" lifts it; "switch" refuses without an AI host config and (dry run) says what it does; run.js with bridge/.handoff starts the AI host instead of the hub and opens no tunnel (the hub here untouched); the flags land in the host folder\'s bridge, never a worktree\'s; a Work run may call handoff.js',
      packed.status === 0 && tarNames.includes('bridge/profiles/t-user/auth.json') && tarNames.includes('Chat/agent-key.json') && tarNames.some((n) => n.startsWith('Board/')) && !tarNames.some((n) => /BAD CHECKSUM|bridge\/server\.js|bridge\/config\.json|\.log$/.test(n))
      && maintOn && blockedSend === 503 && blockedBoard === 503 && blockedLogin === 503 && stillRead === 200 && stillChat && undone.status === 0 && maintOff && afterUndo === 200
      && dry.status === 0 && /dry run/.test(dry.stdout) && noCfg.status !== 0 && /ai-node\.json is missing/.test(noCfg.stderr)
      && lockSeen && /Handed off/.test(run2Log) && hubStillOurs && /HANDED_OFF\(\)/.test(runSrc) && /ai-node\.js/.test(runSrc) && /require\('\.\/worktree'\)/.test(hoSrc) && /path\.join\(REPO, 'bridge'\)/.test(hoSrc) && /Bash\(node bridge\/handoff\.js:\*\)/.test(cfgTools)
      && /^bridge\/\.handoff$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /^bridge\/\.maintenance$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')),
      `pack ${packed.status} ${tarNames.length} entries (${tarNames.slice(0, 4).join(', ')}…); maint ${maintOn} blocked ${blockedSend}/${blockedBoard}/${blockedLogin} read ${stillRead} chat ${stillChat}; undo ${undone.status} off ${maintOff} after ${afterUndo}; dry "${dry.stdout.split('\n')[0].slice(0, 60)}" nocfg ${noCfg.status}; run2 lock ${lockSeen} log "${run2Log.split('\n')[0].slice(0, 80)}"`);
  }

  // R148: Direct messages are their own place in the chat, not part of a server
  {
    const gj = fs.readFileSync(path.join(MILLS, 'web', 'guilds.js'), 'utf8'), page = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    const railSrc = (gj.match(/function renderRail\(\) \{[\s\S]*?\n  \}/) || [''])[0], chanSrc = (gj.match(/function renderChannels\(\) \{[\s\S]*?\n  \}/) || [''])[0];
    check('R148', 'Direct messages: the rail starts with a DM button (its own SVG icon i-dms, with an unread badge) above every server and a divider; it opens the Direct messages view: the one-to-one DMs, then the group DMs as DM channels, and the key fingerprints; a server shows only its channels (no groups, no DMs); opening a DM or group DM from anywhere switches to that view; the rail counts unread per server and on the DM button',
      /<symbol id="i-dms" viewBox="0 0 24 24">/.test(page) && railSrc.indexOf("'srv dms'") > -1 && railSrc.indexOf("'srv dms'") < railSrc.indexOf('for (const s of G.data.servers)') && /railSep/.test(railSrc) && /ic\('dms'\)/.test(railSrc) && /serverBadge = DM/.test(railSrc)
      && /if \(inDms\) return renderDms\(box\)/.test(chanSrc) && !/G\.data\.groups/.test(chanSrc) && /\['convList', 'grpList', 'keyList'\]/.test(chanSrc) && /Group DMs/.test(gj) && /glist\.append\(convButton\(g\.conv/.test(gj)
      && /<div id="convList"><\/div><div id="grpList"><\/div><div id="keyList"><\/div>/.test(page) && /\$\('keyList'\)\.append\(det\)/.test(page) && /GUILDS\.isDmConv\(conv\) \? GUILDS\.DM/.test(page) && /data-server-badge/.test(page) && /const isDmConv = \(c\) => \/\^\(dm\|grp\):\//.test(gj),
      `rail dms ${railSrc.indexOf("'srv dms'")} servers ${railSrc.indexOf('for (const s of G.data.servers)')}; channels groups ${/G\.data\.groups/.test(chanSrc)}`);
  }

  // R156: the web page updates itself: every 5 minutes (and on every answer) it compares builds; it saves the session
  // and hard-reloads with every cache busted
  {
    const served = await (await fetch(H + '/')).text();
    const ver = await call('/api/version', U), verHdr = (await fetch(H + '/api/version', { headers: { Authorization: 'Bearer ' + U } })).headers.get('x-vr-version');
    const anonVer = (await fetch(H + '/api/version')).status, ck156 = (await call('/api/check', U)).data;
    const page = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), notes156 = fs.readFileSync(path.join(MILLS, 'web', 'notes.js'), 'utf8'), host156 = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8');
    const build = ver.data.build || '';
    const tagged = [...served.matchAll(/<script src="([^"]+)"/g)].map((m) => m[1]);
    check('R156', 'the web page updates itself without anyone doing anything: the hub names its build (version + commit) on every signed-in answer and at /api/version (signed-in only), the page checks every 5 minutes; when the build changed it waits only for a streaming reply, a call or an unsaved file / note, saves the Work conversation + queue (only if Work was opened), the drafts and the screen, empties the service-worker caches, updates the worker and reloads under the new build\'s name, then restores it all; the hub serves every script and stylesheet under the build\'s name (cache busting) and the page itself with no-store; AI hosts look for updates every 5 minutes',
      ver.status === 200 && /^\d+\.\d+\.\d+(\+[0-9a-f]{7})?$/.test(build) && verHdr === build && ck156.build === build && anonVer === 401
      && tagged.length >= 5 && tagged.every((s) => s.endsWith('?v=' + encodeURIComponent(build))) && new RegExp('stylesheet" href="polish\\.css\\?v=').test(served)
      && /setInterval\(\(\) => \{ if \(me && token\) api\('\/api\/version'\)/.test(page) && /5 \* 60e3/.test(page) && /caches\.delete\(k\)/.test(page) && /r\.update\(\)/.test(page) && /searchParams\.set\('v', build\)/.test(page) && /location\.replace\(/.test(page)
      && /if \(workStarted\) \{ try \{ saveCurrent\(\); saveQueue\(\);/.test(page) && /function restoreAfterUpdate\(\)/.test(page) && /restoreAfterUpdate\(\); \/\/ the page just updated/.test(page) && /editorDirty = true/.test(page) && /dirty: \(\) => !!N\.dirty/.test(notes156)
      && /checkUpdate\(\); \}, UPDATE_CHECK_MS\)/.test(host156) && /checkMinutes, 1, 60, 5\)/.test(host156),
      `build ${build} hdr ${verHdr} check ${ck156.build} anon ${anonVer}; scripts ${tagged.slice(0, 3).join(' ')}`);
    // R173: one build per hub, whoever answers: every signed-in answer (the Rust hub's own and the ones of the
    // JavaScript hub behind it) names the build /api/check named, so a page never offers an update to another build
    // (Sponge saw "VibeRoller 4.1.0 is ready" on 5.x); the page drops ?v= from its address once loaded
    const hdrs173 = [];
    for (const p of ['/api/check', '/api/version', '/api/forgejo/me', '/api/board', '/api/notes/tree', '/api/projects', '/api/room/state', '/api/guilds']) {
      const r = await fetch(H + p, { headers: { Authorization: 'Bearer ' + U } }); hdrs173.push(`${p}=${r.status}:${r.headers.get('x-vr-version')}`);
    }
    const signed173 = hdrs173.filter((x) => !/=40[14]:/.test(x));
    const front173 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-hub', 'src', 'front.rs'), 'utf8');
    check('R173', 'one build per hub: every signed-in answer names the build /api/check named, the Rust hub\'s own answers and the JavaScript hub\'s behind it alike (a JavaScript hub that started apart is logged, the page is told the Rust hub\'s build); [static] the page drops ?v= from its address once loaded',
      signed173.length >= 4 && signed173.every((x) => x.endsWith(':' + build)) && /hub\.mods\.build\.note_back\(theirs, &mine\)/.test(front173)
      && /if \(u\.searchParams\.has\('v'\)\) \{ u\.searchParams\.delete\('v'\); history\.replaceState\(/.test(page),
      hdrs173.join(' '));
  }
  // ---- the full-Rust hub spec (team note plans/full-rust-hub.md): what the hub SHOULD do, checked before it is built.
  // spec() = SPEC (not built yet) until it passes; then it is a plain check. R174 FR1 · R176 FR3 · R177 FR4 · R178 FR5 ·
  // R179 FR6 here; R175 FR2 after the restart (R9).
  const tryFetch = async (u, h = {}) => { try { const r = await fetch(u, { headers: h }); return { status: r.status, j: await r.json().catch(() => ({})), h: r.headers }; } catch (e) { return { status: 0, j: { error: e.message }, h: new Map() }; } };
  if (RUST_HUB) { // R174 (FR1): a panic inside one request answers 500, the hub keeps its pid, a lock held during the panic recovers
    const pid0 = (await call('/api/status')).data.pid;
    const pan = await tryFetch(H + '/__vr-test/panic');
    const panLock = await tryFetch(H + '/__vr-test/panic?lock=gateway');
    await sleep(500);
    const st174 = await call('/api/status'), gw174 = await call('/api/admin/gateway/products', A);
    const log174 = fs.readFileSync(path.join(SCRATCH, 'hub.log'), 'utf8'), cargo174 = fs.readFileSync(path.join(MILLS, 'native', 'Cargo.toml'), 'utf8'), mod174 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-hub', 'src', 'modules', 'mod.rs'), 'utf8');
    check('R174', 'FR1 a bad request never takes the Rust hub down: a panic inside a request answers that request 500 ("failed inside the hub; it was logged") and the hub keeps its pid; a lock held during the panic (the gateway\'s products) still works afterwards; the hub log names the request; the test hook exists only with VR_TEST_HOOKS=1; [static] the release build unwinds, not aborts',
      pan.status === 500 && /failed inside the hub/.test(pan.j.error || '') && panLock.status === 500 && st174.status === 200 && !!pid0 && st174.data.pid === pid0 && gw174.status === 200
      && /a request failed inside the hub: GET \/__vr-test\/panic/.test(log174) && /VR_TEST_HOOKS/.test(mod174) && /^panic = "unwind"/m.test(cargo174),
      `panic ${pan.status} ${JSON.stringify(pan.j).slice(0, 80)} · in the gateway lock ${panLock.status} · status ${st174.status} pid ${pid0}→${st174.data.pid} · gateway ${gw174.status} · logged ${/failed inside the hub/.test(log174)} · unwind ${/^panic = "unwind"/m.test(cargo174)}`);
  } else skip('R174', 'FR1 a bad request never takes the Rust hub down', 'the Rust hub\'s: --rust-hub runs it');
  if (!WIN) { // R176 (FR3): kept builds per commit: native/keep-builds.sh keeps the current build + the 2 newest good ones
    const kb = path.join(MILLS, 'native', 'keep-builds.sh'), dist176 = path.join(SCRATCH, 'dist176');
    for (const c of ['c1', 'c2', 'c3', 'c4', 'c5']) { fs.mkdirSync(path.join(dist176, c), { recursive: true }); fs.writeFileSync(path.join(dist176, c, 'vr-hub'), c); }
    fs.writeFileSync(path.join(dist176, 'good'), 'c4\nc3\nc2\nc1\n'); // good builds, newest first; c5 = the current (not yet good)
    const r176 = fs.existsSync(kb) ? spawnSync('sh', [kb, dist176, 'c5', path.join(dist176, 'good')], { encoding: 'utf8', timeout: 30000 }) : { status: -1, stdout: '', stderr: 'no native/keep-builds.sh' };
    const left176 = fs.readdirSync(dist176).filter((d) => /^c\d$/.test(d)).sort();
    let cur176 = ''; try { cur176 = fs.readFileSync(path.join(dist176, 'current'), 'utf8').trim(); } catch { /* none */ }
    const build176 = fs.readFileSync(path.join(MILLS, 'native', 'build.sh'), 'utf8'), ad176 = fs.readFileSync(path.join(MILLS, 'deploy', 'autodeploy.sh'), 'utf8');
    // use-build.sh: the staged build c4 goes in service (its files copied over the top-level ones), current → c4, the kept set follows
    fs.writeFileSync(path.join(dist176, 'c4', 'BUILD.json'), '{"commit":"c4"}');
    const u176 = fs.existsSync(path.join(MILLS, 'native', 'use-build.sh')) ? spawnSync('sh', [path.join(MILLS, 'native', 'use-build.sh'), dist176, 'c4'], { encoding: 'utf8', timeout: 30000 }) : { status: -1, stderr: 'no native/use-build.sh' };
    let top176 = ''; try { top176 = fs.readFileSync(path.join(dist176, 'vr-hub'), 'utf8'); } catch { /* none */ }
    let cur176b = ''; try { cur176b = fs.readFileSync(path.join(dist176, 'current'), 'utf8').trim(); } catch { /* none */ }
    check('R176', 'FR3 kept builds per commit: sh native/keep-builds.sh <dist> <current> <good list> keeps the current build and the 2 newest good ones, deletes the rest, writes "current" (the current commit); sh native/use-build.sh <dist> <commit> puts a kept build in service (its files at <dist>/ itself, where run.js, vr-run and the Downloads read them; current follows); [static] build.sh stages builds under their commit and puts them in service through use-build.sh, the auto-deploy\'s rollback puts the last good build back in service with the checkout',
      r176.status === 0 && left176.join(',') === 'c3,c4,c5' && cur176 === 'c5' && u176.status === 0 && top176 === 'c4' && cur176b === 'c4' && /use-build\.sh/.test(build176) && /VR_STAGE_ONLY/.test(build176) && /use-build\.sh/.test(ad176),
      `keep exit ${r176.status} ${String(r176.stderr || '').trim().slice(0, 100)} · left ${left176.join(',')} · current ${cur176 || 'none'} · use exit ${u176.status} ${String(u176.stderr || '').trim().slice(0, 100)} top-level now ${top176 || 'none'} current ${cur176b || 'none'} · build.sh uses ${/use-build\.sh/.test(build176)} · autodeploy restores ${/use-build\.sh/.test(ad176)}`);
  }
  if (!WIN) { // R177 (FR4): the Rust supervisor vr-run: crash-loop → the previous good build; a hang → killed, the previous good build; exit 75 → started again
    const native = path.join(MILLS, 'native'), home177 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
    const env177 = { ...process.env, RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(home177, '.rustup'), CARGO_HOME: process.env.CARGO_HOME || path.join(home177, '.cargo'), CARGO_TARGET_DIR: NATIVE_TARGET };
    env177.PATH = path.join(env177.CARGO_HOME, 'bin') + path.delimiter + (env177.PATH || '');
    const vrRun = path.join(NATIVE_TARGET, 'release', 'vr-run');
    const built177 = fs.existsSync(path.join(native, 'crates', 'vr-run')) ? spawnSync('cargo', ['build', '--release', '-q', '-p', 'vr-run'], { cwd: native, env: env177, encoding: 'utf8', timeout: 1200000 }) : { status: -1, stderr: 'no crate native/crates/vr-run' };
    const fakeHub = path.join(SCRATCH, 'fake-hub.js'); // a stand-in build: answers /api/status with its commit; modes: ok | crash | hang; exit 75 when asked
    fs.writeFileSync(fakeHub, `const http = require('http'), fs = require('fs'); const commit = process.argv[2], port = Number(process.argv[process.argv.indexOf('--port') + 1]);
const mode = () => { try { return fs.readFileSync(process.argv[3], 'utf8').trim(); } catch { return 'ok'; } };
if (mode() === 'crash') process.exit(1);
http.createServer((req, res) => { if (mode() === 'hang') return; if (mode() === 'exit75') { res.end('{}'); return setTimeout(() => process.exit(75), 100); } res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ ok: true, hub: true, pid: process.pid, commit, serving: process.env.VR_SERVING || '', alone: process.argv.includes('--alone') })); }).listen(port, '127.0.0.1');\n`);
    const scenario = async (name, builds, cur, good, expect, change, withJs, extraArgs = []) => { // builds: commit → mode; cur = the newest; good = the good list newest first; withJs: a stand-in bridge/server.js (the JavaScript hub) in the root
      if (built177.status !== 0) return { name, ok: false, why: 'vr-run not built' };
      const dir = path.join(SCRATCH, 'dist177-' + name); fs.mkdirSync(dir, { recursive: true });
      const root0 = path.join(SCRATCH, 'root177-' + name, 'bridge'); fs.mkdirSync(root0, { recursive: true });
      if (withJs) fs.writeFileSync(path.join(root0, 'server.js'), `const http = require('http'); const port = Number(process.env.VR_BACK_PORT || process.env.MILLS_PORT || 0); if (!port) process.exit(1);
http.createServer((req, res) => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ ok: true, hub: true, pid: process.pid, commit: process.env.VR_BACK_PORT ? 'js-behind' : 'js-alone', serving: process.env.VR_SERVING || '' })); }).listen(port, '127.0.0.1');\n`);
      for (const [c, m] of Object.entries(builds)) { fs.mkdirSync(path.join(dir, c), { recursive: true }); fs.writeFileSync(path.join(dir, c, 'mode'), m); fs.writeFileSync(path.join(dir, c, 'vr-hub'), `#!/bin/sh\nexec "${process.execPath}" "${fakeHub}" ${c} "${path.join(dir, c, 'mode')}" "$@"\n`, { mode: 0o755 }); }
      fs.writeFileSync(path.join(dir, 'current'), cur + '\n'); fs.writeFileSync(path.join(dir, 'good'), good.join('\n') + '\n'); // current: a plain file naming the commit (keep-builds.sh writes it so)
      const port = await freePort(PORT + 300), root = root0;
      const log = fs.openSync(path.join(SCRATCH, `vr-run-${name}.log`), 'a');
      const proc = spawn(vrRun, ['--root', root, '--port', String(port), '--builds', dir, '--health-every', '1', '--health-timeout', '1', ...extraArgs], { stdio: ['ignore', log, log] });
      let serving = '';
      const who = async () => { try { const r = await fetch(`http://127.0.0.1:${port}/api/status`, { signal: AbortSignal.timeout(1500) }); const j = await r.json(); serving = j.serving || ''; return j.commit || ''; } catch { return ''; } };
      let seen = '', pid1 = 0; const t0 = Date.now();
      for (let i = 0; i < 100 && seen !== expect; i++) { await sleep(300); seen = await who(); }
      if (change) { const r0 = await fetch(`http://127.0.0.1:${port}/api/status`).then((r) => r.json()).catch(() => ({})); pid1 = r0.pid || 0; await change(dir); let again = ''; for (let i = 0; i < 100 && (!again || again === String(pid1)); i++) { await sleep(300); try { const r = await fetch(`http://127.0.0.1:${port}/api/status`, { signal: AbortSignal.timeout(1500) }); const j = await r.json(); again = String(j.pid || ''); seen = j.commit || ''; } catch { again = ''; } } }
      const fell = (() => { try { return JSON.parse(fs.readFileSync(path.join(root, '.fell-back'), 'utf8')); } catch { return null; } })();
      proc.kill('SIGTERM'); await sleep(500); try { proc.kill('SIGKILL'); } catch { /* gone */ }
      return { name, ok: seen === expect, seen, serving, ms: Date.now() - t0, fell };
    };
    const crash = await scenario('crash', { n1: 'crash', g1: 'ok', g0: 'ok' }, 'n1', ['g1', 'g0'], 'g1');
    const hang = await scenario('hang', { n1: 'hang', g1: 'ok', g0: 'ok' }, 'n1', ['g1', 'g0'], 'g1');
    const fine = await scenario('fine', { n1: 'ok', g1: 'ok' }, 'n1', ['g1'], 'n1');
    // the next health check makes the fake exit 75 and vr-run starts it again; it goes back to normal only once vr-run's log
    // says the restart was asked for (a fixed 1.5 s switch-back could miss a slow health check under load: flaky)
    const again = await scenario('exit75', { n1: 'ok', g1: 'ok' }, 'n1', ['g1'], 'n1', async (dir) => {
      const m = path.join(dir, 'n1', 'mode'), log = path.join(SCRATCH, 'vr-run-exit75.log'); fs.writeFileSync(m, 'exit75');
      for (let i = 0; i < 200; i++) { await sleep(300); let t = ''; try { t = fs.readFileSync(log, 'utf8'); } catch { /* not yet */ } if (/restart asked for/.test(t)) break; }
      fs.writeFileSync(m, 'ok');
    });
    // the first swap's last net: every kept Rust build is bad and bridge/server.js still exists → the JavaScript hub takes the port alone (as run.js did)
    const jsAlone = await scenario('jsalone', { n1: 'crash' }, 'n1', [], 'js-alone', null, true);
    const jsAloneNote = fs.existsSync(path.join(SCRATCH, 'root177-jsalone', 'bridge', '.announce')) && fs.readdirSync(path.join(SCRATCH, 'root177-jsalone', 'bridge', '.announce')).some((f) => /^js-alone-/.test(f));
    check('R177', 'FR4 the Rust supervisor vr-run (replacing run.js): starts the newest build ("current"); a build that crashes 3 times within a minute is marked bad and the previous good build takes over (bridge/.fell-back names both); a build that accepts connections but never answers the health check (/api/status every --health-every s, --health-timeout s, 3 misses) is killed and the previous good build takes over; a healthy build stays; exit 75 = the build is started again (restart on new code); while bridge/server.js still exists, every kept build failing hands the port to the JavaScript hub alone (the first swap\'s last net), said in .announce',
      built177.status === 0 && crash.ok && crash.fell && crash.fell.from === 'n1' && crash.fell.to === 'g1' && hang.ok && hang.fell && hang.fell.to === 'g1' && fine.ok && !fine.fell && again.ok && !again.fell && jsAlone.ok && jsAloneNote,
      built177.status !== 0 ? `vr-run does not build: ${String(built177.stderr || '').trim().split('\n').slice(-2).join(' ').slice(0, 160)}` : `crash → ${crash.seen || 'nobody'} in ${crash.ms} ms fell ${JSON.stringify(crash.fell)} · hang → ${hang.seen || 'nobody'} in ${hang.ms} ms fell ${JSON.stringify(hang.fell)} · fine → ${fine.seen} fell ${!!fine.fell} · exit 75 → ${again.seen} fell ${!!again.fell} · all bad + server.js → ${jsAlone.seen || 'nobody'} in ${jsAlone.ms} ms, said ${jsAloneNote}`);
    // R192 (T-157): the fallback chain and the frozen JavaScript hub. vr-run serves the newest build ("rust"), then the
    // previous good builds ("rust-fallback-1", "rust-fallback-2"), and only when all fail the JavaScript hub alone
    // ("javascript-fallback", also with --alone); /api/status says which. The real bridge/server.js, started alone like
    // the last resort starts it, still boots and serves the basics on the current tree (bridge/test/js-fallback.js).
    const deep192 = await scenario('deep', { n1: 'crash', g1: 'crash', g2: 'ok' }, 'n1', ['g1', 'g2'], 'g2');
    const aloneJs192 = await scenario('alonejs', { n1: 'crash', g1: 'crash' }, 'n1', ['g1'], 'js-alone', null, true, ['--alone']);
    const aloneOk192 = await scenario('alonefine', { n1: 'ok' }, 'n1', [], 'n1', null, true, ['--alone']);
    const js192 = await require('./js-fallback.js').probe({ portFrom: PORT + 340 });
    const keep192 = fs.readFileSync(path.join(MILLS, 'native', 'keep-builds.sh'), 'utf8');
    check('R192', 'T-157 the fallback chain: /api/status names what serves (rust → rust-fallback-1 → rust-fallback-2 → javascript-fallback, also with --alone); two previous good builds are kept (keep-builds.sh); the real JavaScript hub started alone boots and serves the basics (status, page, sign-in gate + sign-in, chat room, board, one Work run with the fake Claude, the AI host link)',
      built177.status === 0 && fine.serving === 'rust' && crash.serving === 'rust-fallback-1' && deep192.ok && deep192.serving === 'rust-fallback-2' && aloneJs192.ok && aloneJs192.serving === 'javascript-fallback' && aloneOk192.ok && aloneOk192.serving === 'rust' && /-ge 2/.test(keep192) && js192.ok,
      `serving: fine ${fine.serving} · crash ${crash.serving} · 3 deep ${deep192.seen}/${deep192.serving} · --alone all bad ${aloneJs192.seen}/${aloneJs192.serving} · --alone fine ${aloneOk192.serving} · JavaScript hub alone: ${js192.why}`);
    // R195: switching the live hub to the Rust hub alone is a config line and a graceful restart (deploy/full-rust-swap.md).
    // 2026-10-25 vr-run read rustHub.alone only when it started, so the graceful restart (exit 75) kept the JavaScript hub
    // running behind the Rust one and the swap needed a restart of the whole service.
    let r195 = { before: '', after: '', back: '', backBefore: false, backAfter: true, backBack: false };
    if (built177.status === 0) {
      const back195 = async (p) => { try { const r = await fetch(`http://127.0.0.1:${p}/api/status`, { signal: AbortSignal.timeout(1000) }); return (await r.json()).commit === 'js-behind'; } catch { return false; } };
      const front195 = async (p) => { try { const r = await fetch(`http://127.0.0.1:${p}/api/status`, { signal: AbortSignal.timeout(1000) }); return await r.json(); } catch { return {}; } };
      const dir = path.join(SCRATCH, 'dist195'), root = path.join(SCRATCH, 'root195', 'bridge'); fs.mkdirSync(path.join(dir, 'n1'), { recursive: true }); fs.mkdirSync(root, { recursive: true });
      fs.writeFileSync(path.join(root, 'server.js'), `const http = require('http'); const port = Number(process.env.VR_BACK_PORT || process.env.MILLS_PORT || 0); if (!port) process.exit(1);
http.createServer((req, res) => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ ok: true, hub: true, commit: process.env.VR_BACK_PORT ? 'js-behind' : 'js-alone' })); }).listen(port, '127.0.0.1');\n`);
      fs.writeFileSync(path.join(root, 'config.local.json'), JSON.stringify({ port: 1 }));
      fs.writeFileSync(path.join(dir, 'n1', 'mode'), 'ok'); fs.writeFileSync(path.join(dir, 'n1', 'vr-hub'), `#!/bin/sh\nexec "${process.execPath}" "${fakeHub}" n1 "${path.join(dir, 'n1', 'mode')}" "$@"\n`, { mode: 0o755 });
      fs.writeFileSync(path.join(dir, 'current'), 'n1\n');
      const port = await freePort(PORT + 360), backPort = await freePort(port + 2);
      const log = fs.openSync(path.join(SCRATCH, 'vr-run-r195.log'), 'a');
      const proc = spawn(vrRun, ['--root', root, '--port', String(port), '--back', String(backPort), '--builds', dir, '--health-every', '1', '--health-timeout', '1'], { stdio: ['ignore', log, log] });
      // (waits are "until", up to 60 s: several suites and builds may share the machine)
      let s = {}; for (let i = 0; i < 200 && !(s.commit === 'n1' && (r195.backBefore = await back195(backPort))); i++) { await sleep(300); s = await front195(port); }
      // a graceful restart of the build: it answers in exit-75 mode once (no pid, or the connection drops), and only then
      // goes back to normal, so a slow health check cannot miss the window (a fixed 1.5 s switch-back made this flaky)
      const restart195 = async (pidOld) => {
        const m = path.join(dir, 'n1', 'mode'); fs.writeFileSync(m, 'exit75');
        let x = {}; for (let i = 0; i < 200; i++) { await sleep(300); x = await front195(port); if (!x.pid || x.pid !== pidOld) break; }
        fs.writeFileSync(m, 'ok');
        for (let i = 0; i < 200 && !(x.pid && x.pid !== pidOld); i++) { await sleep(300); x = await front195(port); }
        return x;
      };
      r195.before = `${s.commit || 'nobody'}${s.alone ? ' --alone' : ''}`;
      fs.writeFileSync(path.join(root, 'config.local.json'), JSON.stringify({ port: 1, rustHub: { enabled: true, backPort, alone: true } }));
      s = await restart195(s.pid);
      await sleep(1500); // the JavaScript hub would have been started again by now
      r195.after = `${s.commit || 'nobody'}${s.alone ? ' --alone' : ''}`; r195.backAfter = await back195(backPort);
      // and back (the rollback in deploy/full-rust-swap.md): alone false + a graceful restart starts the JavaScript hub behind again
      fs.writeFileSync(path.join(root, 'config.local.json'), JSON.stringify({ port: 1, rustHub: { enabled: true, backPort, alone: false } }));
      s = await restart195(s.pid);
      for (let i = 0; i < 100 && !(r195.backBack = await back195(backPort)); i++) await sleep(300);
      r195.back = `${s.commit || 'nobody'}${s.alone ? ' --alone' : ''}`;
      proc.kill('SIGTERM'); await sleep(500); try { proc.kill('SIGKILL'); } catch { /* gone */ }
    }
    check('R195', 'switching to the Rust hub alone needs only the config line and a graceful restart: vr-run reads rustHub.alone again when a build asks for a restart (exit 75), starts the next build with --alone and stops the JavaScript hub behind it (it stays the last resort); setting it back to false and a graceful restart starts the JavaScript hub behind again (the rollback)',
      built177.status === 0 && r195.before === 'n1' && r195.backBefore && r195.after === 'n1 --alone' && !r195.backAfter && r195.back === 'n1' && r195.backBack,
      `before: ${r195.before}, JavaScript hub behind ${r195.backBefore} · after the restart: ${r195.after}, JavaScript hub behind ${r195.backAfter} · rolled back: ${r195.back || 'nobody'}, JavaScript hub behind ${r195.backBack}`);
  }
  if (!WIN) { // R194: the chain fills up the way the auto-deploy runs it (5.13.3: no build was ever marked good, so every deploy deleted the previous builds)
    const dist194 = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-r194-')), nat194 = path.join(MILLS, 'native'), seen194 = [];
    for (const c of ['c1', 'c2', 'c3', 'c4']) { // one deploy each: staged, tried, marked good, put in service
      fs.mkdirSync(path.join(dist194, c)); fs.writeFileSync(path.join(dist194, c, 'BUILD.json'), '{}'); fs.writeFileSync(path.join(dist194, c, 'vr-hub'), c);
      for (const s of ['mark-good.sh', 'use-build.sh']) spawnSync('sh', [path.join(nat194, s), dist194, c], { encoding: 'utf8', timeout: 30000 });
      seen194.push(fs.readdirSync(dist194).filter((d) => /^c\d$/.test(d)).sort().join(','));
    }
    const serves194 = fs.readFileSync(path.join(dist194, 'vr-hub'), 'utf8');
    fs.rmSync(dist194, { recursive: true, force: true });
    const ad194 = fs.readFileSync(path.join(MILLS, 'deploy', 'autodeploy.sh'), 'utf8');
    check('R194', 'the fallback chain fills up as deploys come: each tried build is marked good (native/mark-good.sh) before it goes in service, so after 4 deploys the current build and the 2 before it are kept; [static] the auto-deploy marks a build good after its trial, before use-build.sh',
      seen194.join(' / ') === 'c1 / c1,c2 / c1,c2,c3 / c2,c3,c4' && serves194 === 'c4' && /trial\.sh[^\n]*then sh '\$APP\/native\/mark-good\.sh'[^\n]*use-build\.sh/.test(ad194),
      `builds after each deploy: ${seen194.join(' / ')} · in service ${serves194}`);
  }
  // ---- presence + activity (team note plans/presence.md, T-259) and multi-session Work (plans/multi-session.md, T-283):
  // written as spec() first, plain checks since the Rust hub built them (T-259 5.14.0, T-284 / T-285 5.15.0). New routes are the Rust hub's.
  if (RUST_HUB) { // R196: presence per device (most active wins) × working / accepting work, the label, override, text with expiry
    const st196 = async (tok, u) => (((await call('/api/presence', tok)).data.people || []).find((p) => p.user === u)) || {};
    const keep196 = (tok, dev) => { let on = true; (async () => { let ver = -1; while (on) { const r = await call(`/api/room/messages?rev=0&wait=1&ver=${ver}&device=${dev}`, tok); if (r.status === 200 && r.data.ver !== undefined) ver = r.data.ver; else await sleep(300); } })(); return () => { on = false; }; };
    const seen = {}; let ok196 = false;
    const has196 = (await call('/api/presence', D)).status === 200;
    if (has196) {
      const stopA = keep196(D, 'r196a'), stopB = keep196(D, 'r196b');
      await sleep(400);
      await call('/api/presence', D, { device: 'r196a', idle: false, screen: 'work' }); await call('/api/presence', D, { device: 'r196b', idle: true });
      await sleep(200); seen.mixed = await st196(U, 'mills');
      await call('/api/presence', D, { device: 'r196a', idle: true }); await sleep(200); seen.bothIdle = await st196(U, 'mills');
      stopA(); await call('/api/presence', D, { device: 'r196a', gone: true }); await sleep(200); seen.oneGone = await st196(U, 'mills'); // (its poll loop stops first, so the page that closed does not poll again)
      await call('/api/presence', D, { device: 'r196b', idle: false });
      const slow = call('/api/chat', D, { message: 'SLOW r196' }); await sleep(800); seen.run = await st196(U, 'mills');
      await call('/api/presence/me', D, { acceptingWork: true }); await sleep(200); seen.runAccept = await st196(U, 'mills');
      await slow; await sleep(300); seen.accept = await st196(U, 'mills');
      seen.badOverride = (await call('/api/presence/me', D, { override: 'invisible' })).status; seen.badText = (await call('/api/presence/me', D, { text: 'x'.repeat(81) })).status;
      const ver0 = (await call('/api/room/messages?rev=0', U)).data.ver;
      const me = await call('/api/presence/me', D, { override: 'offline', overrideUntil: Date.now() + 1500, text: 'r196 heads down', textUntil: Date.now() + 1500 });
      seen.meReal = me.data.real; seen.off = await st196(U, 'mills');
      const room = (await call('/api/room/messages?rev=0', U)).data; seen.verMoved = room.ver !== ver0; seen.inRoom = (room.status || []).some((p) => p.user === 'mills');
      seen.oldList = (room.presence || []).find((p) => p.user === 'mills') || {};
      await sleep(2000); seen.back = await st196(U, 'mills'); seen.profile = ((await call('/api/profile', D)).data.profile || {}).status;
      await call('/api/presence/me', D, { override: 'auto', acceptingWork: false, text: '' });
      stopB(); await call('/api/presence', D, { device: 'r196b', gone: true }); await sleep(200); seen.allGone = await st196(U, 'mills');
      ok196 = seen.mixed.presence === 'online' && seen.mixed.label === 'Online' && seen.mixed.devices === 2 && seen.bothIdle.presence === 'away' && seen.bothIdle.label === 'Away' && seen.oneGone.presence === 'away'
        && seen.run.working === true && seen.run.label === 'Working' && seen.runAccept.label === 'Working + available' && seen.accept.working === false && seen.accept.label === 'Available for work'
        && seen.badOverride === 400 && seen.badText === 400 && seen.meReal === 'online' && seen.off.presence === 'offline' && seen.off.label === 'Offline' && !seen.off.text && seen.oldList.online === false
        && seen.verMoved && seen.inRoom && seen.back.presence !== 'offline' && !seen.back.text && seen.profile === '' && seen.allGone.presence === 'offline';
    }
    const l196 = (x) => x ? `${x.presence || '?'}/${x.label || '?'}` : '?';
    check('R196', 'presence and activity (plans/presence.md): a person is as present as their most active device (one active + one idle → online, both idle → away, the last one gone → offline); a run of theirs makes them Working; the accept-work switch makes them Available for work (Working + available during a run); appear offline shows Offline to everyone (also in the old presence list) while they see their real presence, and it ends at its until; the custom text (profile.status) ends at its until; bad override / too-long text → 400; a change reaches pages through the room long poll (status, ver)',
      ok196, has196 ? `mixed ${l196(seen.mixed)} (${seen.mixed.devices} devices) · both idle ${l196(seen.bothIdle)} · one gone ${l196(seen.oneGone)} · run ${l196(seen.run)} · run+accept ${l196(seen.runAccept)} · accept ${l196(seen.accept)} · bad ${seen.badOverride}/${seen.badText} · appear offline: others ${l196(seen.off)} text "${seen.off.text || ''}", old list online ${seen.oldList.online}, self real ${seen.meReal} · room ver moved ${seen.verMoved} status ${seen.inRoom} · after until ${l196(seen.back)} text "${seen.back.text || ''}" · all gone ${l196(seen.allGone)}` : 'GET /api/presence does not exist yet');
  } else skip('R196', 'presence and activity (plans/presence.md)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) await r488Check(); else skip('R488', 'the page shows everyone\'s status (web/presence.js, T-261)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R205-R209: time accounting (plans/presence.md section 6, T-263); the scratch hub's config has presence.workGapMs = 1500
    const day205 = (() => { const d = new Date(); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`; })();
    const time205 = async (tok, q = '') => (await call(`/api/presence/time${q}`, tok)).data || {};
    const today205 = async (tok, who) => (((await time205(tok, who ? `?user=${who}` : '')).days || []).find((x) => x.day === day205)) || { activeMs: 0, awayMs: 0, aiMs: 0, agentMs: 0, spans: [], events: [] };
    const act = (tok) => call('/api/presence', tok, { device: 'r205', active: true });
    await sleep(1800); // a quiet longer than the gap, so nothing before this extends into the test
    const b205 = await today205(U);
    await act(U); await sleep(700); await act(U); await sleep(2200); await act(U);
    const a205 = await today205(U);
    const dSpans = a205.spans.length - b205.spans.length, dActive = a205.activeMs - b205.activeMs;
    const last2 = a205.spans.slice(-2);
    const quiet = last2.length === 2 ? last2[1][0] - last2[0][1] : 0;
    check('R205', 'time accounting: input, then input within the gap, then a quiet longer than workGapMs, then input again makes two spans, and the quiet is not counted (active time = the spans only)',
      dSpans === 2 && quiet >= 2000 && dActive >= 600 && dActive < 1200, `spans +${dSpans} · quiet between ${quiet} ms · active +${dActive} ms (expect about 700)`);
    const b206 = await today205(D); const t206 = Date.now();
    await call('/api/presence', D, { device: 'r206', idle: true });
    await call('/api/chat', D, { message: 'SLOW r206' });
    await sleep(2200); // past the gap with the person idle; the tick has seen the run end
    const a206 = await today205(D);
    const dAi = a206.aiMs - b206.aiMs, dAgent = a206.agentMs - b206.agentMs, dAct = a206.activeMs - b206.activeMs;
    check('R206', 'time accounting: a run (SLOW) of a person who is idle (no input) adds its running time to aiMs (and agentMs, summed over sessions) and nothing to activeMs',
      dAi >= 2500 && dAi <= 7000 && dAgent >= dAi && dAct === 0, `ai +${dAi} ms · agent +${dAgent} ms · active +${dAct} ms`);
    const ev206 = (a206.events || []).filter((e) => e.at >= t206);
    check('R207', 'the activity log (events of the day, append-only): a run shows as an "ai" event with the number of working sessions (1, then 0) and a "status" event to Working and back',
      ev206.some((e) => e.kind === 'ai' && e.sessions >= 1) && ev206.some((e) => e.kind === 'ai' && e.sessions === 0) && ev206.some((e) => e.kind === 'status' && e.label === 'Working'),
      ev206.map((e) => `${e.kind}:${e.label || e.sessions}`).join(' '));
    const other = await time205(D, `?user=t-user&from=${day205}&to=${day205}`), own = await time205(U, `?from=${day205}&to=${day205}`);
    const od = (other.days || [])[0] || {}, ownd = (own.days || [])[0] || {};
    const bad = (await call('/api/presence/time?from=nope', U)).status, nobody = (await call('/api/presence/time?user=nobody-here', U)).status, anon = (await call('/api/presence/time')).status;
    check('R208', 'GET /api/presence/time: the team sees another person\'s totals (activeMs, awayMs, aiMs, agentMs) without their spans and events, a person sees their own record with both; a bad day is 400, an unknown person 404, not signed in 401; it names the workGapMs',
      typeof od.activeMs === 'number' && od.spans === undefined && od.events === undefined && Array.isArray(ownd.spans) && ownd.spans.length >= 2 && Array.isArray(ownd.events) && bad === 400 && nobody === 404 && anon === 401 && own.workGapMs === 1500,
      `other: ${Object.keys(od).join(',')} · own spans ${(ownd.spans || []).length} · ${bad}/${nobody}/${anon} · gap ${own.workGapMs}`);
    const f209 = path.join(SB, 'profiles', 't-user', 'activity', `${day205}.json`);
    const j209 = fs.existsSync(f209) ? JSON.parse(fs.readFileSync(f209, 'utf8')) : {};
    const page209 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R209', 'the day is kept in the person\'s own profiles/<user>/activity/<day>.json ({activeMs, awayMs, aiMs, agentMs, spans, events, updatedAt}); [static] the page sends active: true on its 15 s presence tick, at most once a minute, only when there was input',
      typeof j209.activeMs === 'number' && typeof j209.awayMs === 'number' && typeof j209.aiMs === 'number' && typeof j209.agentMs === 'number' && Array.isArray(j209.spans) && typeof j209.updatedAt === 'number'
        && /inputSince && Date\.now\(\) - sentActive >= 60000/.test(page209) && /body\.active = true/.test(page209), `file ${fs.existsSync(f209)} keys ${Object.keys(j209).join(',')}`);
  } else for (const id of ['R205', 'R206', 'R207', 'R208', 'R209']) skip(id, 'time accounting and the activity log (plans/presence.md section 6)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R230-R234: where a person works (T-260): project, branch, held task, from Work runs, the board and other sources
    const st230 = async (tok, u) => (((await call('/api/presence', tok)).data.people || []).find((p) => p.user === u)) || {};
    const wh230 = async (u = 'mills', tok = U) => (await st230(tok, u)).where;
    const keep230 = (tok, dev) => { let on = true; (async () => { let ver = -1; while (on) { const r = await call(`/api/room/messages?rev=0&wait=1&ver=${ver}&device=${dev}`, tok); if (r.status === 200 && r.data.ver !== undefined) ver = r.data.ver; else await sleep(300); } })(); return () => { on = false; }; };
    const stop230 = keep230(D, 'r230'); await sleep(400); await call('/api/presence', D, { device: 'r230', idle: false });
    const s230 = {};
    s230.before = await wh230();
    const slow = call('/api/chat', D, { message: 'SLOW r230' }); await sleep(800);
    s230.run = await wh230(); const rst = await st230(U, 'mills');
    await slow; await sleep(400); s230.after = await wh230();
    check('R230', 'where (plans/presence.md): a person with a running Work session shows where they work, { project, branch, since, source: "work" }, to the team in GET /api/presence, and it is gone when the run ends',
      !s230.before && s230.run && s230.run.source === 'work' && typeof s230.run.project === 'string' && s230.run.project.length > 0 && /^work\/mills/.test(s230.run.branch) && typeof s230.run.since === 'number' && rst.working === true && !s230.after,
      `before ${JSON.stringify(s230.before)} · run ${JSON.stringify(s230.run)} · after ${JSON.stringify(s230.after)}`);
    const t230 = (await call('/api/board/create', D, { title: 'where r230', status: 'todo' })).data.task || {};
    await call('/api/board/claim', D, { id: t230.id, minutes: 10 });
    s230.claim = await wh230();
    const slow2 = call('/api/chat', D, { message: 'SLOW r230b' }); await sleep(800); s230.both = await wh230();
    await slow2; await sleep(300);
    await call('/api/board/release', D, { id: t230.id }); await sleep(100); s230.released = await wh230();
    await call('/api/board/complete', D, { id: t230.id });
    check('R231', 'where: the board task a person holds a claim on shows as where.task { id, title } (source "board" on its own, added to the Work session\'s project / branch during a run) and goes when the claim is released',
      s230.claim && s230.claim.task && s230.claim.task.id === t230.id && s230.claim.task.title === 'where r230' && s230.claim.source === 'board' && s230.both && s230.both.source === 'work' && s230.both.task && s230.both.task.id === t230.id && s230.both.project && !s230.released,
      `claim ${JSON.stringify(s230.claim)} · run+claim ${JSON.stringify(s230.both)} · released ${JSON.stringify(s230.released)}`);
    const rep = { project: 'r232-proj', branch: 'feature/x', task: { id: 'T-232', title: 'reported' } };
    await call('/api/presence', D, { device: 'r232', source: 'jcode', where: rep });
    s230.jc = await wh230(); const jcRoom = ((await call('/api/room/messages?rev=0', U)).data.status || []).find((p) => p.user === 'mills') || {};
    await call('/api/presence', D, { device: 'r232', source: 'jcode', where: { project: 'r232-proj', branch: 'feature/y' } }); s230.jc2 = await wh230();
    await call('/api/presence', D, { device: 'r232', source: 'jcode', where: null }); s230.jcClear = await wh230();
    await call('/api/presence', D, { device: 'r232', source: 'jcode', where: rep }); await call('/api/presence', D, { device: 'r232', gone: true }); s230.jcGone = await wh230();
    check('R232', 'where: another source (Jcode PGM, the desktop app) reports POST /api/presence { device, source, where: { project, branch, task? } }: others see it with that source, in the room long poll\'s status too, a new report replaces it, where: null clears it and the device going away removes it',
      s230.jc && s230.jc.source === 'jcode' && s230.jc.project === 'r232-proj' && s230.jc.branch === 'feature/x' && s230.jc.task && s230.jc.task.id === 'T-232' && jcRoom.where && jcRoom.where.project === 'r232-proj' && s230.jc2 && s230.jc2.branch === 'feature/y' && !s230.jc2.task && !s230.jcClear && !s230.jcGone,
      `reported ${JSON.stringify(s230.jc)} · replaced ${JSON.stringify(s230.jc2)} · cleared ${JSON.stringify(s230.jcClear)} · gone ${JSON.stringify(s230.jcGone)}`);
    await call('/api/presence', D, { device: 'r232', source: 'jcode', where: rep });
    await call('/api/presence/me', D, { hideWhere: true }); await sleep(100); s230.hidden = await wh230(); const mine = await call('/api/presence/me', D); s230.mine = mine.data.where;
    await call('/api/presence/me', D, { hideWhere: false, override: 'offline' }); await sleep(100); s230.offline = await wh230(); const offMine = (await call('/api/presence/me', D)).data;
    await call('/api/presence/me', D, { override: 'auto' }); await sleep(100); s230.back = await wh230();
    await call('/api/presence', D, { device: 'r232', gone: true });
    check('R233', 'where: hideWhere hides project / branch / task from others (the person still sees their own where); appear offline hides it too; both are back when switched off',
      s230.hidden === undefined && s230.mine && s230.mine.project === 'r232-proj' && s230.offline === undefined && offMine.hideWhere === false && s230.back && s230.back.project === 'r232-proj',
      `hidden ${JSON.stringify(s230.hidden)} · own ${JSON.stringify(s230.mine)} · offline ${JSON.stringify(s230.offline)} · back ${JSON.stringify(s230.back)}`);
    await call('/api/presence', D, { device: 'r234', source: 'in valid!/' + 'x'.repeat(40), where: { project: 'p'.repeat(200), branch: 'b\u0001ranch', task: { id: 'T-1', title: 't'.repeat(500) } } });
    const long = await wh230(); const junk = [await call('/api/presence', D, { device: 'r234', where: 'nope' }), await call('/api/presence', D, { device: 'r234', where: {} })]; const afterJunk = await wh230();
    await call('/api/presence', D, { device: 'r234', gone: true }); stop230(); await call('/api/presence', D, { device: 'r230', gone: true });
    const page234 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R234', 'where: a source\'s name and text are cleaned and cut (source letters / digits / - _ up to 20, project 80, branch 120, task title 160, control characters dropped); junk or empty where clears it (still 200); [static] the page tooltip names "on <project> · <task>"',
      long && long.source.length <= 20 && /^[A-Za-z0-9_-]+$/.test(long.source) && long.project.length === 80 && long.branch === 'branch' && long.task.title.length === 160 && junk.every((r) => r.status === 200) && !afterJunk && /function whereText/.test(page234) && /' · on '/.test(page234),
      `source "${long && long.source}" project ${long && long.project.length} branch "${long && long.branch}" title ${long && long.task && long.task.title.length} · junk ${junk.map((r) => r.status)} · after ${JSON.stringify(afterJunk)}`);
  } else for (const id of ['R230', 'R231', 'R232', 'R233', 'R234']) skip(id, 'where a person works (T-260)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R250-R254 (T-264): presence for AIs and tools: the full status in the team tools, notify_when_available / notify_list / notify_cancel, the status event. t-user's Claude (fake "NOTIFYTOOL") asks about mills (D)
    const keep250 = (tok, dev) => { let on = true; (async () => { let ver = -1; while (on) { const r = await call(`/api/room/messages?rev=0&wait=1&ver=${ver}&device=${dev}`, tok); if (r.status === 200 && r.data.ver !== undefined) ver = r.data.ver; else await sleep(300); } })(); return () => { on = false; }; };
    const stop250 = keep250(D, 'r250'); await sleep(400); await call('/api/presence', D, { device: 'r250', idle: false });
    await call('/api/presence/me', D, { override: 'auto', acceptingWork: false, dnd: false, text: '' });
    const tools250 = async (calls, tok = U) => { const r = await chat(tok, 'NOTIFYTOOL ' + JSON.stringify(calls) + ' END', { fresh: true }); try { return JSON.parse((texts(r.events).match(/NOTIFY:(\[.*\])/) || [])[1]); } catch { return []; } };
    const seq250 = async () => Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
    const dms250 = async (since) => { const ms = ((await call('/api/room/messages?since=' + since, U)).data.messages || []).filter((m) => m.from === 'claude' && m.to === 't-user' && m.as); return Promise.all(ms.map(async (m) => ({ as: m.as, via: m.via, text: await roomRead(restored, m) }))); };
    const waitDm = async (since, re, ms = 6000) => { const end = Date.now() + ms; for (;;) { const hit = (await dms250(since)).find((d) => re.test(d.text)); if (hit || Date.now() > end) return hit; await sleep(300); } };
    const noDm = async (since, re, ms = 3000) => { await sleep(ms); return !(await dms250(since)).some((d) => re.test(d.text)); };
    const ok250 = (r) => !!r && r.ok === true;
    const s250 = {};
    // R250: the tools carry the whole status
    await call('/api/presence/me', D, { acceptingWork: true, text: 'r250 status' }); await sleep(300);
    { const [pr, hs] = await tools250([['team_presence', {}], ['hub_status', {}]]);
      const mp = ((pr && pr.result && pr.result.people) || []).find((p) => p.user === 'mills') || {}, mh = ((hs && hs.result && hs.result.people) || []).find((p) => p.user === 'mills') || {};
      const keys = ['presence', 'label', 'working', 'workingSessions', 'acceptingWork', 'dnd', 'text', 'since', 'state', 'screen', 'agoMin', 'user', 'name'];
      check('R250', 'T-264 the team tools carry the whole status: team_presence and hub_status give each person presence, label, working (+ workingSessions), acceptingWork, dnd, text, since (and where) next to the older state / screen / agoMin',
        ok250(pr) && ok250(hs) && keys.every((k) => k in mp && k in mh) && mp.presence === 'online' && mp.state === 'online' && mp.label === 'Available for work' && mp.acceptingWork === true && mp.working === false && mp.workingSessions === 0 && mp.dnd === false && mp.text === 'r250 status' && typeof mp.since === 'number' && mh.label === mp.label,
        `mills in team_presence ${JSON.stringify(mp)} · hub_status label ${mh.label}`); }
    // R251: "available": registered, listed, told in a private message from the asker's Claude when it holds, then gone from the list
    await call('/api/presence/me', D, { acceptingWork: false }); await sleep(1300);
    { const since = await seq250();
      const [ask, list1] = await tools250([['notify_when_available', { user: 'mills', what: 'available', message: 'r251 ping' }], ['notify_list', {}]]);
      await call('/api/presence/me', D, { acceptingWork: true });
      const dm = await waitDm(since, /is available now/);
      const [again, list2] = await tools250([['notify_when_available', { user: 'mills', what: 'available' }], ['notify_list', {}]]);
      s250.r251 = { ask, list1, dm, again, list2 };
      check('R251', 'T-264 notify_when_available: a request for "available" is remembered (notify_list shows it with its id), and once the person accepts work the asker is told in a private chat message from "<asker> · Claude" ("Mills is available now (asked by your Claude at HH:MM). Note: …"); the request is then gone; asking while it already holds answers at once (already: true) and registers nothing',
        ok250(ask) && ask.result.registered === true && !!ask.result.id && ok250(list1) && list1.result.watching.length === 1 && list1.result.watching[0].user === 'mills' && list1.result.watching[0].what === 'available'
        && !!dm && /^.+ · Claude$/.test(dm.as) && dm.via === 't-user' && /^.+ is available now \(asked by your Claude at \d\d:\d\d\)\. Note: r251 ping$/.test(dm.text)
        && ok250(again) && again.result.already === true && again.result.registered === false && ok250(list2) && list2.result.watching.length === 0,
        `ask ${JSON.stringify(ask && ask.result)} · told "${dm && dm.text}" as "${dm && dm.as}" · again ${JSON.stringify(again && again.result && { already: again.result.already })} · list after ${list2 && list2.result && list2.result.watching.length}`); }
    // R252: "back" (away → online), "free" (a run ends), "online" (appear offline ends)
    { await call('/api/presence/me', D, { acceptingWork: false, override: 'away' }); await sleep(1300);
      const sBack = await seq250(); const [aBack] = await tools250([['notify_when_available', { user: 'mills', what: 'back' }]]);
      await call('/api/presence/me', D, { override: 'auto' }); const dBack = await waitDm(sBack, /is back \(asked/);
      const slow = call('/api/chat', D, { message: 'SLOW r252' }); await sleep(900);
      const sFree = await seq250(); const [aFree] = await tools250([['notify_when_available', { user: 'mills', what: 'free' }]]);
      const noEarly = await noDm(sFree, /is free now/, 500); await slow; const dFree = await waitDm(sFree, /is free now \(not working\) \(asked/);
      await call('/api/presence/me', D, { override: 'offline' }); await sleep(1300);
      const sOn = await seq250(); const [aOn] = await tools250([['notify_when_available', { user: 'mills', what: 'online' }]]);
      await call('/api/presence/me', D, { override: 'auto' }); const dOn = await waitDm(sOn, /is online now \(asked/);
      check('R252', 'T-264 the other waits: "back" is told when someone who was away is online again, "free" when their run ends (not before), "online" when their appear offline ends; each is told once',
        ok250(aBack) && aBack.result.registered === true && !!dBack && ok250(aFree) && aFree.result.registered === true && noEarly && !!dFree && ok250(aOn) && aOn.result.registered === true && !!dOn,
        `back ${aBack && aBack.result && aBack.result.registered}/${!!dBack} · free ${aFree && aFree.result && aFree.result.registered}/early-silent ${noEarly}/${!!dFree} · online ${aOn && aOn.result && aOn.result.registered}/${!!dOn}`); }
    // R253: appear offline hides the change (nothing is told while it holds), and shows nothing the status does not show
    { await call('/api/presence/me', D, { acceptingWork: false, override: 'offline' }); await sleep(1300);
      const slow = call('/api/chat', D, { message: 'SLOW r253' }); await sleep(900);
      const since = await seq250();
      const [free, avail] = await tools250([['notify_when_available', { user: 'mills', what: 'free' }], ['notify_when_available', { user: 'mills', what: 'available' }]]);
      await slow;
      await call('/api/presence/me', D, { acceptingWork: true });
      const quiet = await noDm(since, /is available now/, 2800);
      await call('/api/presence/me', D, { override: 'auto' }); const dm = await waitDm(since, /is available now \(asked/);
      check('R253', 'T-264 appear offline: while it holds, "free" is answered from what others are shown (not working, though a run is going), a request for "available" stays quiet when the person starts accepting work, and the asker is told only once they appear again; the message says nothing beyond the status',
        ok250(free) && free.result.already === true && free.result.status.working === false && free.result.status.label === 'Offline' && !free.result.status.text && ok250(avail) && avail.result.registered === true && avail.result.label === 'Offline' && quiet && !!dm && !/Offline|working|r250/i.test(dm.text),
        `free ${JSON.stringify(free && free.result && { already: free.result.already, label: free.result.status && free.result.status.label })} · available ${avail && avail.result && avail.result.registered} · quiet while hidden ${quiet} · told after "${dm && dm.text}"`); }
    // R254: errors, the limit of five, cancel, privacy between askers, and the status event hook
    { await call('/api/presence/me', D, { acceptingWork: false, override: 'offline' }); await sleep(1300);
      const errs = await tools250([['notify_when_available', { user: 'nobody-here', what: 'available' }], ['notify_when_available', { user: 'mills', what: 'bogus' }], ['notify_when_available', { user: 't-user' }], ['notify_cancel', {}]]);
      const tadminOff = (((await call('/api/presence', U)).data.people || []).find((p) => p.user === 't-admin') || {}).presence === 'offline';
      const adds = await tools250([['notify_when_available', { user: 'mills', what: 'available' }], ['notify_when_available', { user: 'mills', what: 'back' }], ['notify_when_available', { user: 'mills', what: 'online' }], ['notify_when_available', { user: 't-admin', what: 'available' }], ['notify_when_available', { user: 't-admin', what: 'back' }], ['notify_when_available', { user: 't-admin', what: 'online' }]]);
      const regs = adds.filter((r) => ok250(r) && r.result.registered === true).length, errsAdd = adds.filter((r) => !ok250(r));
      const [lStranger] = await tools250([['notify_list', {}]], A);
      const [cOne, lAfterOne] = await tools250([['notify_cancel', { user: 'mills' }], ['notify_list', {}]]);
      const [cAll, lAfterAll] = await tools250([['notify_cancel', { all: true }], ['notify_list', {}]]);
      const lib = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-hcom-internal', 'src', 'chat.rs'), 'utf8');
      const nativeTools = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.map((t) => t.name);
      const jsTools = (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1));
      const names = ['notify_when_available', 'notify_list', 'notify_cancel'];
      check('R254', 'T-264 notify tools: an unknown person, an unknown "what", yourself and a cancel without a target are refused; at most five requests wait per asker (the sixth is refused); they are private to the asker (another person lists none); notify_cancel by user / all removes them; [static] the status change is an event (Chat::subscribe_status) that notify uses, and both MCP servers offer the three tools',
        errs.length === 4 && errs.every((r) => r.ok === false) && (!tadminOff || (regs === 5 && errsAdd.length === 1 && /5 people/.test(errsAdd[0].error)))
        && ok250(lStranger) && lStranger.result.watching.length === 0 && ok250(cOne) && cOne.result.cancelled >= 3 && ok250(lAfterOne) && lAfterOne.result.watching.every((w) => w.user !== 'mills')
        && ok250(cAll) && ok250(lAfterAll) && lAfterAll.result.watching.length === 0
        && /pub fn subscribe_status/.test(lib) && /subscribe_status\(\)/.test(lib.slice(lib.indexOf('pub fn start('))) && names.every((n) => nativeTools.includes(n) && jsTools.includes(n)),
        `refused ${errs.map((r) => r.ok)} · registered ${regs} refused ${errsAdd.map((r) => r.error)} (t-admin offline ${tadminOff}) · stranger lists ${lStranger && lStranger.result && lStranger.result.watching.length} · cancelled by user ${cOne && cOne.result && cOne.result.cancelled}, all ${cAll && cAll.result && cAll.result.cancelled} · left ${lAfterAll && lAfterAll.result && lAfterAll.result.watching.length}`); }
    await call('/api/presence/me', D, { override: 'auto', acceptingWork: false, text: '' });
    stop250(); await call('/api/presence', D, { device: 'r250', gone: true });
  } else for (const [id, l] of [['R250', 'the team tools carry the whole status'], ['R251', 'notify_when_available tells the asker'], ['R252', 'back / free / online waits'], ['R253', 'appear offline hides the change'], ['R254', 'notify tools: limits, cancel, privacy, the status event']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  // R480 to R484 (T-295 = T-294 · 1/6): agent-to-agent, spec() until T-296 … T-300 build it (team note plans/agent-talk.md
  // § 11). Each probes the routes of § 6 as the person; what is not there yet answers 404 and the check says SPEC.
  {
    const L480 = {
      R480: 'agent talk 2/6 (T-296): @t-user resolves to t-user\'s current session: an idle one is woken (the run sees the "[agent message · from @t-admin\'s agent" first line), a working one gets it queued and delivered when its reply ends; the thread exists on both sides with one id; a third person gets 404 on it; the audit has sent / delivered / read with from.run',
      R481: 'agent talk 3/6 (T-297): a run\'s agent_ask (wait: true) to another person is answered by that person\'s run through agent_reply and the asker\'s tool result carries the reply text and thread; wait: false gives {pending} and the reply arrives as a message in the asker\'s session; agents_list shows the person reachable with their state; both MCP lists have the six tools',
      R482: 'agent talk 4/6 (T-298): mute refuses a send (403 muted) and shows the attempt to the muter only; pause queues (202 queued) and unpause delivers; a hand-off is held by default, told in a private chat line, declined with a note or accepted (then the task is re-assigned to the receiver); DND changes nothing of delivery; the page has the thread panel and the held card',
      R483: 'agent talk 5/6 (T-299): a source: jcode session report makes @t-user/jcode resolvable; a send to it waits in GET /api/agents/inbox?source=jcode, read marks it, a reply from that side reaches the asker; the message has the same shape both ways (no Claude-only fields)',
      R484: 'agent talk 6/6 (T-300): an A→B→A→B echo is stopped by the loop detector (stopped, why loop) with one chat line to each person; the 21st message of a pair in an hour is 429 rate; a hand-off of someone else\'s task is held and, declined, leaves the assignee and refuses the receiving run\'s task_claim; a send right after a refused tool carries afterRefusal in the audit; an agent-started run counts in the receiver\'s ledger and time log',
    };
    if (RUST_HUB) {
      const ag = (p, tok, body) => call('/api/agents/' + p, tok, body);
      const built = (await ag('settings', U)).status === 200; // the whole feature is there, or none of it
      const seq480 = async () => Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
      // R480: wake an idle session, queue behind a working one, one thread both sides, audit
      let d480 = 'no /api/agents/* yet';
      if (built) {
        const idle = await ag('send', A, { to: 't-user', kind: 'ask', text: 'r480 what is the plan?', refs: { task: 'T-296' }, wait: false });
        let woke = null; for (let i = 0; i < 40 && !woke; i++) { const cur = (await call('/api/work/current', U)).data || {}; if ((cur.transcript || []).some((t) => /\[agent message · from @t-admin's agent/.test(t.text || ''))) woke = cur; else await sleep(250); }
        const slow = call('/api/chat', U, { message: 'SLOW r480' }); await sleep(700);
        const queued = await ag('send', A, { to: 't-user', kind: 'ask', text: 'r480 second, while busy', thread: idle.data.thread, wait: false }); await slow;
        let delivered = null; for (let i = 0; i < 40 && !delivered; i++) { const m = (await ag('thread?id=' + idle.data.thread, A)).data || {}; const x = (m.messages || []).find((q) => q.id === queued.data.id); if (x && ['delivered', 'read', 'answered'].includes(x.status)) delivered = x; else await sleep(250); }
        const mine = await ag('thread?id=' + idle.data.thread, U), other = await ag('thread?id=' + idle.data.thread, D);
        const audit = await ag('audit?since=0', A);
        const lines = (audit.data && audit.data.lines) || [];
        d480 = `send ${idle.status} ${JSON.stringify(idle.data).slice(0, 80)}; woke ${!!woke}; queued ${queued.status} ${queued.data && queued.data.status} -> ${delivered && delivered.status}; both sides ${mine.status}/${other.status}; audit ${lines.length}`;
        spec('R480', L480.R480, idle.status === 202 && !!woke && queued.status === 202 && queued.data.status === 'queued' && !!delivered && mine.status === 200 && (mine.data.messages || []).length >= 2 && other.status === 404
          && lines.some((l) => l.event === 'sent' && l.id === idle.data.id && typeof l.run === 'string') && lines.some((l) => l.event === 'delivered' && l.id === idle.data.id) && lines.some((l) => l.event === 'read' && l.id === idle.data.id), d480);
      } else spec('R480', L480.R480, false, d480);
      // R481: the tools (the fake Claude's NOTIFYTOOL calls team tools by name)
      const tools481 = async (calls, tok) => { const r = await chat(tok, 'NOTIFYTOOL ' + JSON.stringify(calls) + ' END', { fresh: true }); try { return JSON.parse((texts(r.events).match(/NOTIFY:(\[.*\])/) || [])[1]); } catch { return []; } };
      const nativeTools481 = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.map((t) => t.name);
      const jsTools481 = (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1));
      const six = ['agents_list', 'agent_ask', 'agent_handoff', 'agent_review', 'agent_report', 'agent_reply'];
      const listed = six.every((n) => nativeTools481.includes(n) && jsTools481.includes(n));
      if (built && listed) {
        const [lst, asked] = await tools481([['agents_list', {}], ['agent_ask', { to: 't-user', text: 'r481 reply with the word pelican', wait: true, replyBySec: 60 }]], A);
        const me481 = ((lst && lst.result && lst.result.agents) || []).find((x) => x.user === 't-user') || {};
        const [nowait] = await tools481([['agent_ask', { to: 't-user', text: 'r481 later', wait: false }]], A);
        let later = false; for (let i = 0; i < 40 && !later; i++) { const cur = (await call('/api/work/current', A)).data || {}; later = (cur.transcript || []).some((t) => /\[agent message · from @t-user's agent .* reply/.test(t.text || '')); if (!later) await sleep(250); }
        spec('R481', L480.R481, !!lst && lst.ok && me481.reachable === true && typeof me481.state === 'string' && !!asked && asked.ok && /pelican/.test(JSON.stringify(asked.result)) && !!asked.result.thread && !!nowait && nowait.ok && nowait.result.pending === true && later,
          `list ${JSON.stringify(me481)}; ask ${JSON.stringify(asked && asked.result).slice(0, 120)}; nowait ${JSON.stringify(nowait && nowait.result).slice(0, 80)}; reply later ${later}`);
      } else spec('R481', L480.R481, false, built ? `tools missing: ${six.filter((n) => !nativeTools481.includes(n) || !jsTools481.includes(n)).join(', ')}` : 'no /api/agents/* yet');
      // R482: the person's controls
      const html482 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
      const page482 = /\/api\/agents\/threads/.test(html482) && /\/api\/agents\/held/.test(html482) && /\/api\/agents\/settings/.test(html482);
      if (built) {
        await ag('settings', U, { muted: ['t-admin'] });
        const muted = await ag('send', A, { to: 't-user', kind: 'ask', text: 'r482 muted?', wait: false });
        const seen = (await ag('threads?with=t-admin', U)).data || {}, notSeen = (await ag('threads?with=t-user', A)).data || {};
        await ag('settings', U, { muted: [], paused: true });
        const paused = await ag('send', A, { to: 't-user', kind: 'ask', text: 'r482 paused', wait: false }); await sleep(600);
        const stillQueued = ((await ag('thread?id=' + (paused.data || {}).thread, A)).data || {}).messages || [];
        await ag('settings', U, { paused: false });
        let deliveredAfter = false; for (let i = 0; i < 40 && !deliveredAfter; i++) { const m = ((await ag('thread?id=' + (paused.data || {}).thread, A)).data || {}).messages || []; deliveredAfter = m.some((x) => x.id === paused.data.id && x.status !== 'queued' && x.status !== 'held'); if (!deliveredAfter) await sleep(250); }
        const t482 = (await call('/api/board/create', A, { title: 'r482 handoff task', status: 'todo', assignee: 't-admin' })).data.task || {};
        const s482 = await seq480();
        const held = await ag('send', A, { to: 't-user', kind: 'handoff', text: 'r482 take it', refs: { task: t482.id } });
        const dm = await (async () => { for (let i = 0; i < 24; i++) { const ms = ((await call('/api/room/messages?since=' + s482, U)).data.messages || []).filter((m) => m.to === 't-user'); if (ms.length) return ms[0]; await sleep(250); } return null; })();
        const declined = await ag('held', U, { id: held.data.id, accept: false, note: 'not now' });
        const stillAdmin = ((await call('/api/board', A)).data.tasks || []).find((t) => t.id === t482.id) || {};
        const held2 = await ag('send', A, { to: 't-user', kind: 'handoff', text: 'r482 take it, second try', refs: { task: t482.id } });
        const accepted = await ag('held', U, { id: held2.data.id, accept: true });
        const nowUser = ((await call('/api/board', A)).data.tasks || []).find((t) => t.id === t482.id) || {};
        await call('/api/presence/me', U, { dnd: true });
        const dnd = await ag('send', A, { to: 't-user', kind: 'ask', text: 'r482 dnd', wait: false });
        await call('/api/presence/me', U, { dnd: false });
        spec('R482', L480.R482, muted.status === 403 && (muted.data || {}).why === 'muted' && (seen.threads || []).some((t) => t.with === 't-admin') && !(notSeen.threads || []).some((t) => (t.lastWhy || '') === 'muted')
          && paused.status === 202 && paused.data.status === 'queued' && stillQueued.some((x) => x.id === paused.data.id && x.status === 'queued') && deliveredAfter
          && held.status === 202 && held.data.status === 'held' && !!dm && declined.status === 200 && stillAdmin.assignee === 't-admin' && accepted.status === 200 && nowUser.assignee === 't-user'
          && dnd.status === 202 && dnd.data.status !== 'refused' && page482,
          `muted ${muted.status} ${(muted.data || {}).why}; paused ${paused.status} ${(paused.data || {}).status} queued ${stillQueued.length} then delivered ${deliveredAfter}; held ${held.status} ${(held.data || {}).status} dm ${!!dm} declined ${declined.status} assignee ${stillAdmin.assignee} -> accepted ${accepted.status} ${nowUser.assignee}; dnd ${dnd.status}; page ${page482}`);
      } else spec('R482', L480.R482, false, `no /api/agents/* yet; page ${page482}`);
      // R483: an external (jcode) agent
      if (built) {
        const rep = await call('/api/work/sessions/report', U, { source: 'jcode', device: 'r483-box', sessions: [{ id: 'j-r483', state: 'working', title: 'r483 jcode', project: 'Jcode-PGM' }], ttlSec: 120 });
        const sent = await ag('send', A, { to: 't-user/jcode', kind: 'ask', text: 'r483 jcode, are you there?', wait: false });
        const inbox = await ag('inbox?source=jcode&device=r483-box', U);
        const got = ((inbox.data || {}).messages || []).find((m) => m.id === (sent.data || {}).id);
        const read = got ? await ag('read', U, { ids: [got.id] }) : { status: 0 };
        const rep2 = await ag('reply', U, { thread: (sent.data || {}).thread, text: 'r483 yes, from jcode' });
        let back = false; for (let i = 0; i < 40 && !back; i++) { const cur = (await call('/api/work/current', A)).data || {}; back = (cur.transcript || []).some((t) => /r483 yes, from jcode/.test(t.text || '')); if (!back) await sleep(250); }
        const keys = got ? Object.keys(got).sort().join(',') : '';
        spec('R483', L480.R483, rep.status === 200 && sent.status === 202 && (sent.data || {}).to && sent.data.to.source === 'jcode' && sent.data.to.session === 'j-r483' && !!got && read.status === 200 && rep2.status === 202 && back && /^at,from,hop,id,kind,parent,refs,replyBy,status,text,thread,to,why$/.test(keys),
          `report ${rep.status}; send ${sent.status} to ${JSON.stringify((sent.data || {}).to)}; inbox ${inbox.status} got ${!!got}; read ${read.status}; reply ${rep2.status} back ${back}; keys ${keys}`);
      } else spec('R483', L480.R483, false, 'no /api/agents/* yet');
      // R484: loops, rate, ownership, laundering mark, cost
      if (built) {
        const s484 = await seq480();
        const echo = await ag('send', A, { to: 't-user', kind: 'ask', text: 'ECHO r484 ping', wait: false }); // the fake Claude answers an ECHO with agent_reply of the same text, both sides, until stopped
        let stopped = null; for (let i = 0; i < 80 && !stopped; i++) { const m = (await ag('thread?id=' + (echo.data || {}).thread, A)).data || {}; if (m.status === 'stopped') stopped = m; else await sleep(250); }
        const told = ((await call('/api/room/messages?since=' + s484, A)).data.messages || []).filter((m) => /going in circles/.test(m.text || ''));
        const bursts = []; for (let i = 0; i < 21; i++) bursts.push((await ag('send', A, { to: 't-user', kind: 'report', text: 'r484 burst ' + i, wait: false })).status);
        const t484 = (await call('/api/board/create', D, { title: 'r484 mills task', status: 'todo', assignee: 'mills' })).data.task || {};
        const ho = await ag('send', A, { to: 't-user', kind: 'handoff', text: 'r484 not mine to give', refs: { task: t484.id } });
        const dec = await ag('held', U, { id: (ho.data || {}).id, accept: false });
        const still = ((await call('/api/board', A)).data.tasks || []).find((t) => t.id === t484.id) || {};
        const claim = await call('/api/board/claim', U, { id: t484.id, asAgent: true });
        const audit = ((await ag('audit?since=0', A)).data || {}).lines || [];
        const after = audit.some((l) => l.event === 'sent' && l.afterRefusal === true);
        const ledger = (await call('/api/usage', U)).data || {};
        spec('R484', L480.R484, !!stopped && stopped.why === 'loop' && (stopped.messages || []).length < 8 && told.length >= 1 && bursts.filter((x) => x === 429).length >= 1 && bursts[bursts.length - 1] === 429
          && ho.status === 202 && (ho.data || {}).status === 'held' && dec.status === 200 && still.assignee === 'mills' && claim.status !== 200 && typeof after === 'boolean' && ((ledger.answers || {}).turns || 0) >= 0,
          `loop ${stopped && stopped.why} after ${stopped && (stopped.messages || []).length}; told ${told.length}; rate ${bursts.filter((x) => x === 429).length}/21 refused; handoff ${ho.status} ${(ho.data || {}).status} declined ${dec.status} assignee ${still.assignee} claim ${claim.status}; afterRefusal seen ${after}`);
      } else spec('R484', L480.R484, false, 'no /api/agents/* yet');
    } else for (const id of Object.keys(L480)) skip(id, L480[id], 'the Rust hub\'s: --rust-hub runs it');
  }
  if (RUST_HUB) { // R245-R249 (T-238 = T-193 · 1/4): one start-work flow; the fake Forgejo (this runner) answers as tuser-fj and takes pushes over smart HTTP
    const PR238 = path.join(SCRATCH, 'projects'), FR = (fakeForgejo.repos = fakeForgejo.repos || new Map()), gitq238 = (dir, ...args) => spawnSync('git', args, { cwd: dir, encoding: 'utf8' });
    const cwd238 = (events) => ((texts(events).match(/cwd=(.+?)(?: \||$)/m) || [])[1] || '').trim();
    const same238 = (a, b) => !!a && !!b && path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();
    const sess238 = async (id) => ((await call('/api/work/sessions', U)).data.sessions || []).find((s) => s.id === id) || {};
    const html238 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    // R245: the repos list
    const noTok = await call('/api/projects/repos', U), anon = await call('/api/projects/repos?q=x', null);
    const conn = await call('/api/forgejo/connect', U, { token: 'fake-personal-token-1' });
    FR.set('tuser-fj/alpha-tools', { full_name: 'tuser-fj/alpha-tools', private: true, fork: false, mirror: false, description: 'Alpha tooling', clone_url: `http://127.0.0.1:${OLLAMA_PORT}/tuser-fj/alpha-tools.git` });
    FR.set('UnityAILab/shared-lib', { full_name: 'UnityAILab/shared-lib', private: false, fork: true, mirror: false, description: 'Shared library for the alpha team', clone_url: `http://127.0.0.1:${OLLAMA_PORT}/UnityAILab/shared-lib.git` });
    const allRepos = await call('/api/projects/repos', U), someRepos = await call('/api/projects/repos?q=shared', U), oddRepos = await call('/api/projects/repos?q=' + encodeURIComponent('--repo x  -- ; ls'), U);
    const alpha = ((allRepos.data || {}).repos || []).find((r) => r.name === 'tuser-fj/alpha-tools') || {}, shared = ((someRepos.data || {}).repos || [])[0] || {};
    check('R245', 'start work, clone from a list: GET /api/projects/repos?q= lists the Forgejo repos the person can see (name, private / fork, clone address, description), searched by words, asked of Forgejo as them through worktree.js repos; without their own saved token it says to save one (400); signed out is 401; no secret in the answer; words cannot become options',
      noTok.status === 400 && noTok.data.needsToken === true && /Forgejo token/.test(noTok.data.error) && anon.status === 401 && conn.status === 200
      && allRepos.status === 200 && allRepos.data.repos.length === 2 && alpha.private === true && alpha.url === `http://127.0.0.1:${OLLAMA_PORT}/tuser-fj/alpha-tools.git` && alpha.description === 'Alpha tooling'
      && someRepos.status === 200 && someRepos.data.repos.length === 1 && shared.name === 'UnityAILab/shared-lib' && shared.fork === true && oddRepos.status === 200 && !JSON.stringify([allRepos.data, someRepos.data]).includes('fake-personal-token-1'),
      `no token ${noTok.status} "${(noTok.data.error || '').slice(0, 50)}" anon ${anon.status} connect ${conn.status} · all ${allRepos.status} ${((allRepos.data || {}).repos || []).length} alpha ${JSON.stringify(alpha).slice(0, 120)} · q=shared ${((someRepos.data || {}).repos || []).length} ${shared.name} · odd ${oddRepos.status}`);
    // R246: a new empty project, private on Forgejo (checks first, then repo-create, new, publish)
    await call('/api/forgejo/disconnect', U, {});
    const nTok = await call('/api/projects/create', U, { name: 'start-new' }); const noTokLocal = fs.existsSync(path.join(PR238, 'start-new'));
    await call('/api/forgejo/connect', U, { token: 'fake-personal-token-1' });
    const bad = [await call('/api/projects/create', U, { name: '../evil' }), await call('/api/projects/create', U, { name: '_repo' }), await call('/api/projects/create', U, { name: 'a b' }), await call('/api/projects/create', U, {}), await call('/api/projects/create', U, { name: 'x'.repeat(65) })].map((x) => x.status);
    const taken = await call('/api/projects/create', U, { name: 'Demo-App' }), takenHub = await call('/api/projects/create', U, { name: 'viberoller' });
    FR.set('tuser-fj/taken-remote', { full_name: 'tuser-fj/taken-remote', private: false, fork: false, mirror: false, description: '', clone_url: '' });
    const takenFj = await call('/api/projects/create', U, { name: 'taken-remote' });
    const made238 = await call('/api/projects/create', U, { name: 'start-new', description: 'made by r246' });
    const repoDir = path.join(PR238, 'start-new', '_repo'), bareDir = path.join(SCRATCH, 'fjrepos', 'tuser-fj', 'start-new.git');
    const origin = gitq238(repoDir, 'remote', 'get-url', 'origin').stdout.trim(), remoteBranches = gitq238(bareDir, 'for-each-ref', '--format=%(refname:short)', 'refs/heads').stdout.trim().split('\n').sort().join(',');
    const again = await call('/api/projects/create', U, { name: 'Start-New' }), listed = ((await call('/api/projects', U)).data.projects || []).find((p) => p.name === 'start-new') || {};
    const fjRec = FR.get('tuser-fj/start-new') || {};
    check('R246', 'start work, a new empty project: POST /api/projects/create { name } checks first (bad name 400, a name taken here in any case or the hub\'s own 409, no saved Forgejo token 400 with nothing made, a repo of that name already on Forgejo 409 with no project made here), then makes the repo private on Forgejo as the person (repo-create), the local project (new) and publishes it: origin is Forgejo with main and develop pushed; the project is listed; again → 409',
      nTok.status === 400 && nTok.data.needsToken === true && !noTokLocal && bad.join() === '400,400,400,400,400' && taken.status === 409 && takenHub.status === 409
      && takenFj.status === 409 && !fs.existsSync(path.join(PR238, 'taken-remote')) && made238.status === 200 && made238.data.project === 'start-new' && made238.data.repo === 'tuser-fj/start-new' && made238.data.private === true && fjRec.private === true && fjRec.description === 'made by r246'
      && origin === `http://127.0.0.1:${OLLAMA_PORT}/tuser-fj/start-new.git` && remoteBranches === 'develop,main' && listed.remote === origin && again.status === 409,
      `no token ${nTok.status} local ${noTokLocal} · bad ${bad} · taken ${taken.status}/${takenHub.status} · on Forgejo ${takenFj.status} · made ${made238.status} ${JSON.stringify(made238.data).slice(0, 140)} · private ${fjRec.private} origin ${origin} remote branches ${remoteBranches} · again ${again.status}`);
    // R486 (T-249): a machine's read-only deploy key and a repo's visibility, through worktree.js as the person
    {
      const kdir = fs.mkdtempSync(path.join(SCRATCH, 'r486-')), pub = path.join(kdir, 'k.pub'), priv = path.join(kdir, 'k'), junk = path.join(kdir, 'junk.pub');
      fs.writeFileSync(pub, 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEr486r486r486r486r486r486r486r486r486r486r48 deploy@lab\n');
      fs.writeFileSync(priv, '-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAA\n-----END OPENSSH PRIVATE KEY-----\n');
      fs.writeFileSync(junk, 'hello there\n');
      // not spawnSync: the stand-in Forgejo answers from this same process, so a blocking call would wait for itself
      const wt = (...a) => new Promise((done) => {
        const c = spawn(process.execPath, [path.join(SB, 'worktree.js'), ...a], { cwd: SB, env: { ...process.env, FORGEJO_TOKEN: 'fake-personal-token-1', FORGEJO_URL: `http://127.0.0.1:${OLLAMA_PORT}` } });
        let stdout = '', stderr = ''; c.stdout.on('data', (d) => { stdout += d; }); c.stderr.on('data', (d) => { stderr += d; });
        const t = setTimeout(() => c.kill(), 30000); c.on('close', (status) => { clearTimeout(t); done({ status, stdout, stderr }); });
      });
      const add = await wt('deploy-key', 'tuser-fj/start-new', pub, 'lab server'), again = await wt('deploy-key', 'tuser-fj/start-new', pub, 'lab server');
      const privR = await wt('deploy-key', 'tuser-fj/start-new', priv), junkR = await wt('deploy-key', 'tuser-fj/start-new', junk), noRepo = await wt('deploy-key', 'tuser-fj/nonesuch', pub);
      const keys = (FR.get('tuser-fj/start-new') || {}).keys || [];
      const pubR = await wt('visibility', 'tuser-fj/start-new', 'public'), afterPub = (FR.get('tuser-fj/start-new') || {}).private;
      const privV = await wt('visibility', 'tuser-fj/start-new', 'private'), afterPriv = (FR.get('tuser-fj/start-new') || {}).private, oddV = await wt('visibility', 'tuser-fj/start-new', 'secret');
      check('R486', 'a machine gets a read-only deploy key on a repo (worktree.js deploy-key, as the person): added once, adding it again changes nothing, a private key or a file that is no SSH public key is refused before anything is sent, an unknown repo is an error; visibility makes a repo public or private and nothing else (a word other than private / public is refused); no token in any output',
        add.status === 0 && /Added the read-only deploy key "lab server"/.test(add.stdout) && again.status === 0 && /already has this key/.test(again.stdout) && keys.length === 1 && keys[0].read_only === true
        && privR.status !== 0 && /not an SSH public key/.test(privR.stderr) && junkR.status !== 0 && noRepo.status !== 0
        && pubR.status === 0 && afterPub === false && privV.status === 0 && /is now private/.test(privV.stdout) && afterPriv === true && oddV.status !== 0
        && ![add, again, privR, junkR, noRepo, pubR, privV, oddV].some((x) => (x.stdout + x.stderr).includes('fake-personal-token-1')),
        `add ${add.status} ${add.stdout.trim()} · again ${again.stdout.trim()} · keys ${keys.length} ro ${keys[0] && keys[0].read_only} · private key ${privR.status} ${privR.stderr.trim()} · junk ${junkR.status} · no repo ${noRepo.status} · public ${afterPub} private ${afterPriv} · odd ${oddV.status}`);
    }
    // R247: a clone start from the repos list; each start is a NEW session that lands in the person's own worktree at once
    const before = (await call('/api/work/sessions', U)).data, n0 = before.sessions.length;
    const cloned = await call('/api/projects', U, { url: `http://127.0.0.1:${OLLAMA_PORT}/tuser-fj/start-new.git`, name: 'start-copy' });
    const copyRepo = path.join(PR238, 'start-copy', '_repo'), readme = fs.existsSync(path.join(copyRepo, 'README.md'));
    const nonHttps = await call('/api/projects', U, { url: 'file:///nonexistent-r247' }), noHost = await call('/api/projects', U, { url: 'ftp://example.invalid/x.git' });
    const s247 = await call('/api/work/sessions/new', U, { project: 'start-copy', title: 'r247', background: true });
    const dir247 = (s247.data || {}).dir || '', after = (await call('/api/work/sessions', U)).data, sum247 = await sess238(s247.data.id);
    const run247 = await chat(U, 'where do I land', { session: s247.data.id });
    check('R247', 'start work, clone: a repo cloned from a Forgejo address (the repos list gives it; the person\'s own token is the login, private ones too) becomes a project with the repo\'s own branches; POST /api/work/sessions/new { project } opens a NEW session (one more in the list) and lands at once in the person\'s own worktree <project>/<user>.<id> on work/<user>.<id>, where its first run happens; other addresses than https / ssh are 400',
      cloned.status === 200 && cloned.data.project === 'start-copy' && readme && gitq238(copyRepo, 'rev-parse', '--abbrev-ref', 'HEAD').stdout.trim() === 'develop' && nonHttps.status === 400 && noHost.status === 400
      && s247.status === 200 && /^s-[0-9a-f]{12}$/.test(s247.data.id) && s247.data.project === 'start-copy' && same238(dir247, path.join(PR238, 'start-copy', 't-user.' + s247.data.id.slice(2))) && fs.existsSync(path.join(dir247, '.git')) && s247.data.branch === 'work/t-user.' + s247.data.id.slice(2)
      && after.sessions.length === n0 + 1 && sum247.project === 'start-copy' && sum247.noProject === false && same238(cwd238(run247.events), dir247),
      `clone ${cloned.status} ${JSON.stringify(cloned.data).slice(0, 60)} readme ${readme} · non-https ${nonHttps.status}/${noHost.status} · session ${s247.status} ${JSON.stringify(s247.data).slice(0, 160)} · list ${n0} → ${after.sessions.length} · run cwd ${cwd238(run247.events)}`);
    // R248: no project: the AI only, in the person's scratch folder, said so in the system prompt; no git for it
    const s248 = await call('/api/work/sessions/new', U, { none: true, title: 'r248', background: true }), id248 = (s248.data || {}).id;
    const scratch = path.join(PR238, '_scratch', 't-user'), sum248 = await sess238(id248);
    const run248 = await chat(U, 'where am I with no project', { session: id248 }), t248 = texts(run248.events);
    fs.writeFileSync(path.join(scratch, 'note.txt'), 'kept\n');
    const git248 = await call('/api/work/git?session=' + id248, U), home248 = await call('/api/work/sessions/home', U, { id: id248 });
    const names248 = ((await call('/api/projects', U)).data.projects || []).map((p) => p.name);
    const back248 = await call('/api/workspace', U, { session: id248, project: 'start-copy' }), sum248b = await sess238(id248);
    const none248 = await call('/api/workspace', U, { session: id248, none: true }), sum248c = await sess238(id248);
    await call('/api/work/sessions/delete', U, { id: id248 });
    check('R248', 'start work, no project: POST /api/work/sessions/new { none: true } opens a session whose project is "(none)" (noProject true) and whose run happens in the person\'s own scratch folder <projects>/_scratch/<user> (not a git worktree, never in the projects list), with a system prompt that says so and has no worktree law; the git panel and "merge home" say there is no project (400); /api/workspace switches a session to a project and back; deleting it keeps the scratch folder',
      s248.status === 200 && s248.data.noProject === true && s248.data.project === '(none)' && same238(s248.data.dir, scratch) && fs.existsSync(scratch) && sum248.project === '(none)' && sum248.noProject === true
      && same238(cwd238(run248.events), scratch) && /sysnone=true/.test(t248) && /syslaw=false/.test(t248) && !fs.existsSync(path.join(scratch, '.git')) && git248.status === 400 && git248.data.noProject === true && home248.status === 400 && !names248.includes('_scratch') && !names248.includes('(none)')
      && back248.status === 200 && back248.data.project === 'start-copy' && sum248b.noProject === false && sum248b.project === 'start-copy' && none248.status === 200 && none248.data.project === '(none)' && sum248c.noProject === true && fs.existsSync(path.join(scratch, 'note.txt')),
      `new ${s248.status} ${JSON.stringify(s248.data).slice(0, 140)} · run cwd ${cwd238(run248.events)} sysnone ${/sysnone=true/.test(t248)} syslaw ${/syslaw=true/.test(t248)} · git ${git248.status} home ${home248.status} · list ${names248} · switch ${back248.status} ${sum248b.project}/${sum248b.noProject} → ${none248.status} ${sum248c.noProject} · note kept ${fs.existsSync(path.join(scratch, 'note.txt'))}`);
    // R249: refusals, other people, and the page
    const unk = await call('/api/work/sessions/new', U, { project: 'no-such-project', background: true }), unkNone = await call('/api/work/sessions/new', U, { project: '../x', background: true });
    const hubS = await call('/api/work/sessions/new', U, { project: 'viberoller', background: true });
    const foreign = await call('/api/work/sessions', A), seenByA = (foreign.data.sessions || []).some((s) => [s247.data.id, hubS.data.id].includes(s.id));
    const hubWt = fs.existsSync(path.join(hubS.data.dir || '', '.git'));
    for (const id of [s247.data.id, (hubS.data || {}).id]) if (id) await call('/api/work/sessions/delete', U, { id });
    await call('/api/forgejo/disconnect', U, {});
    const repoCalls = (fakeForgejo.repoCalls || []).join(' | ');
    check('R249', 'start work, the rest: a start naming an unknown or bad project is 400 and opens nothing; the hub\'s own project starts (any case) in the person\'s worktree of it; nobody else sees the session; the Forgejo calls were made as the person only; the page has the Start work panel (project list, clone address box + repos search, new project name, no project) and sends ?session= for the session on screen',
      unk.status === 400 && unkNone.status === 400 && hubS.status === 200 && hubS.data.project === 'VibeRoller' && hubWt && !seenByA && /create tuser-fj\/start-new private/.test(repoCalls) && !/create (?!tuser-fj\/)/.test(repoCalls)
      && /id="startPanel"/.test(html238) && /id="startProjects"/.test(html238) && /id="startUrl"/.test(html238) && /id="startRepoQ"/.test(html238) && /id="startNewName"/.test(html238) && /id="startNoneGo"/.test(html238) && /id="startWork"/.test(html238) && /\/api\/projects\/repos\?q=/.test(html238) && /\/api\/projects\/create/.test(html238) && /'session=' \+ encodeURIComponent\(SESSION\)/.test(html238),
      `unknown ${unk.status}/${unkNone.status} · hub project ${hubS.status} ${hubS.data.project} ${hubS.data.dir} · seen by another ${seenByA} · Forgejo calls "${repoCalls.slice(0, 160)}"`);
  } else for (const id of ['R245', 'R246', 'R247', 'R248', 'R249', 'R486']) skip(id, 'one start-work flow (T-238), deploy keys (T-249)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R487 (T-190): the board's archive, for people, for Claude, and still readable by the JavaScript hub
    const nb487 = await call('/api/board/new', A, { name: 'R487 Board' }), B487 = 'r487-board'; // its own board: "archive all done" touches nobody else's checks
    const mk = async (title, extra) => (await call('/api/board/create', A, { title, board: B487, ...extra })).data.task.id;
    const a1 = await mk('archive one r487', { due: 'today' }), a2 = await mk('archive two r487'), open = await mk('archive open r487');
    await call('/api/board/complete', A, { id: a1 }); await call('/api/board/complete', A, { id: a2 });
    const openTry = await call('/api/board/archive', A, { id: open }), arch1 = await call('/api/board/archive', A, { id: a1 });
    const view1 = (await call('/api/board', U)).data;
    const got1 = (await call('/api/board/task?id=' + a1, U)).data.task || {};
    const found1 = (await call('/api/board/archived?q=' + encodeURIComponent('one r487'), U)).data;
    const f1 = (found1.tasks || []).find((t) => t.id === a1) || {};
    const rest = await call('/api/board/restore', U, { id: a1, status: 'review' }), view2 = (await call('/api/board', U)).data;
    const restAgain = await call('/api/board/restore', U, { id: a1 });
    await call('/api/board/complete', A, { id: a1 });
    const all = await call('/api/board/archive-done', A, { board: B487 }), view3 = (await call('/api/board', U)).data;
    const onView = (v, id) => (v.tasks || []).some((t) => t.id === id);
    const agent = JSON.parse((texts((await chat(A, 'BOARDARCHIVE please', { fresh: true })).events).match(/ARCH:(\{.*?\})/) || [])[1] || '{}');
    // the board MCP server offers the four tools
    const mcpList = await new Promise((done) => {
      const c = spawn(process.execPath, [path.join(BRIDGE, 'board-mcp.js')], { env: { ...process.env, MILLS_BRIDGE: 'http://127.0.0.1:1' } }); let o = '';
      c.stdout.on('data', (d) => { o += d; if (o.includes('\n')) { c.kill(); } }); c.on('close', () => { try { done(JSON.parse(o.split('\n')[0]).result.tools.map((t) => t.name)); } catch { done([]); } });
      c.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }) + '\n');
    });
    // the frozen JavaScript hub reads the same file: an archived task is a done one there, and its fields survive its write
    const jsDir = fs.mkdtempSync(path.join(SCRATCH, 'r487-js-'));
    fs.copyFileSync(path.join(SCRATCH, 'Board', 'board.json'), path.join(jsDir, 'board.json'));
    const jsBoard = require(path.join(BRIDGE, 'board.js'))({ dir: jsDir, send: () => {}, readBody: async () => ({}), accounts: { has: () => true }, displayOf: (u) => u });
    const jsGet = jsBoard.ops.get({ user: 't-admin', agent: false }, { id: a2 }), jsUpd = jsBoard.ops.update({ user: 't-admin', agent: false }, { id: a2, appendNote: 'from the JavaScript hub r487' });
    const jsDisk = (JSON.parse(fs.readFileSync(path.join(jsDir, 'board.json'), 'utf8')).tasks || []).find((t) => t.id === a2) || {};
    check('R487', 'the board\'s archive: only a done task can be archived; an archived task leaves the board but can still be read, is found in the archive by search with the day it will be deleted (30 days on), and is restored into the column asked for; "archive all done" empties the Done column; Claude archives, finds and restores through the board tools (task_archive, board_archived, task_restore, board_archive_done); the frozen JavaScript hub still reads the file (the task is done there) and keeps archivedAt when it writes',
      openTry.status === 400 && arch1.status === 200 && arch1.data.task.archivedAt > 0 && !onView(view1, a1) && onView(view1, a2) && view1.archive && view1.archive.archiveAfterHours === 48 && view1.archive.deleteAfterDays === 30 && view1.archive.count >= 1
      && nb487.status === 200 && got1.archivedAt === arch1.data.task.archivedAt && f1.deleteAt === f1.archivedAt + 30 * 86400e3 && (found1.tasks || []).every((t) => /one r487/.test(t.title))
      && rest.status === 200 && rest.data.task.status === 'review' && rest.data.task.archivedAt === null && onView(view2, a1) && restAgain.status === 400
      && all.status === 200 && [a1, a2].every((id) => all.data.archived.includes(id)) && !onView(view3, a1) && !onView(view3, a2) && !(view3.tasks || []).some((t) => t.status === 'done' && t.board === B487) && onView(view3, open)
      && agent.id && /not done/.test(agent.early || '') && agent.archivedAt > 0 && agent.onBoard === false && (agent.found || []).includes(agent.id) && agent.restored === 'todo'
      && ['task_archive', 'task_restore', 'board_archive_done', 'board_archived'].every((n) => mcpList.includes(n))
      && jsGet.status === 200 && jsGet.body.task.status === 'done' && jsUpd.status === 200 && jsDisk.archivedAt > 0 && /from the JavaScript hub r487/.test(jsDisk.body || ''),
      `open ${openTry.status} · archive ${arch1.status} on board ${onView(view1, a1)} info ${JSON.stringify(view1.archive)} · found ${JSON.stringify(found1.tasks && found1.tasks.map((t) => t.id))} delete ${f1.deleteAt - f1.archivedAt} · restore ${rest.status} ${rest.data.task && rest.data.task.status} again ${restAgain.status} · all ${JSON.stringify(all.data).slice(0, 80)} · agent ${JSON.stringify(agent)} · mcp ${mcpList.filter((n) => /archiv|restore/.test(n))} · js ${jsGet.status}/${jsUpd.status} kept ${jsDisk.archivedAt}`);
  } else skip('R487', 'the board\'s archive (T-190)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R490 (T-280): the planner re-assesses the board, as proposals only
    const mk = async (title, extra) => (await call('/api/board/create', A, { title, ...extra })).data.task.id;
    const base = await mk('engine r490 groundwork'); await call('/api/board/complete', A, { id: base });
    const waits = await mk('waits r490 on the groundwork', { status: 'backlog', labels: ['blocked', 'r490'], assignee: 't-admin', body: `**Blocked on ${base}.** based on it` });
    const still = await mk('still r490 blocked', { status: 'backlog', labels: ['blocked'], assignee: 't-admin', body: `Blocked on ${waits}.` });
    const lonely = await mk('nobody r490 owns this');
    const before = JSON.stringify(((await call('/api/board', A)).data.tasks || []).filter((t) => [waits, still, lonely].includes(t.id)).map((t) => [t.id, t.rev, t.status]));
    const got = JSON.parse((texts((await chat(A, 'PLANASSESS please', { fresh: true })).events).match(/ASSESS:(\{.*\})/) || [])[1] || '{}');
    const props = got.proposals || [], has = (kind, id) => props.find((p) => p.kind === kind && (p.ids || []).includes(id));
    const un = has('unblock', waits) || {};
    const after = JSON.stringify(((await call('/api/board', A)).data.tasks || []).filter((t) => [waits, still, lonely].includes(t.id)).map((t) => [t.id, t.rev, t.status]));
    check('R490', 'the planner re-assesses the board (plan_assess): a task marked blocked on something done is proposed for unblocking, with the change ready for the draft (to do, without the blocked label); one still waiting is not; a task without an owner is named; every proposal has a reason; nothing on the board changes',
      !!un.reason && un.change && un.change.id === waits && un.change.change.status === 'todo' && !(un.change.change.labels || []).includes('blocked') && (un.change.change.labels || []).includes('r490')
      && !has('unblock', still) && !!has('unowned', lonely) && props.every((p) => p.reason) && before === after,
      `${props.length} proposals · unblock ${JSON.stringify(un).slice(0, 200)} · still ${!!has('unblock', still)} · unowned ${!!has('unowned', lonely)} · unchanged ${before === after} ${got.error || ''}`);
  } else skip('R490', 'the planner re-assesses the board (T-280)', 'the Rust hub\'s: --rust-hub runs it');
  // R494 (T-323): deploy/setup-dev.sh installs the dev hub beside a live one (here a stand-in live base with a bare origin),
  // copying live's data but never its sign-in sessions, and never writing live's files except `pair`
  if (WIN) skip('R494', 'the dev hub install (deploy/setup-dev.sh)', 'a server script (sh): Linux only');
  else {
    const L = fs.mkdtempSync(path.join(SCRATCH, 'r494-')), app = path.join(L, 'app'), bare = path.join(L, 'origin.git');
    const g = (dir, ...a) => spawnSync('git', ['-c', 'user.name=r494', '-c', 'user.email=r494@test.invalid', ...a], { cwd: dir, encoding: 'utf8' });
    fs.mkdirSync(path.join(app, 'deploy'), { recursive: true }); fs.mkdirSync(path.join(app, 'bridge')); fs.mkdirSync(path.join(app, 'Board')); fs.mkdirSync(path.join(app, 'Notes'));
    fs.writeFileSync(path.join(app, 'versions.json'), '{ "project": "9.9.9" }\n');
    for (const f of ['setup-dev.sh', 'viberoller-dev-hub.service', 'viberoller-dev-autodeploy.service', 'viberoller-dev-autodeploy.timer']) fs.copyFileSync(path.join(MILLS, 'deploy', f), path.join(app, 'deploy', f));
    fs.writeFileSync(path.join(app, '.gitignore'), 'bridge/*.json\nBoard/\nNotes/\n');
    g(L, 'init', '-q', '--bare', '-b', 'main', bare); g(app, 'init', '-q', '-b', 'main'); g(app, 'add', '-A'); g(app, 'commit', '-q', '-m', 'r494');
    g(app, 'remote', 'add', 'origin', bare); g(app, 'push', '-q', 'origin', 'main'); g(app, 'push', '-q', 'origin', 'main:develop');
    const liveCfg = JSON.stringify({ port: 7777, publicUrl: 'https://portal.example', rustHub: { enabled: true, backPort: 7779, alone: true }, aiNodes: { pc: 'k' }, allowedOrigins: ['https://x.example'] }, null, 2) + '\n';
    fs.writeFileSync(path.join(app, 'bridge', 'config.local.json'), liveCfg);
    fs.writeFileSync(path.join(app, 'bridge', 'auth.json'), '{"users":{"ann":{"hash":"h"}}}'); fs.writeFileSync(path.join(app, 'bridge', 'front-sessions.json'), '{"tok":"ann"}');
    fs.writeFileSync(path.join(app, 'bridge', 'device-locks.json'), '{}'); fs.writeFileSync(path.join(app, 'Board', 'board.json'), '{"tasks":[{"id":"T-1"}]}'); fs.writeFileSync(path.join(app, 'Notes', 'n.md'), 'note r494');
    const env = { ...process.env, VR_LIVE_BASE: L, VR_DEV_UNITS: '', VR_DEV_URL: 'https://dev.example' };
    const run1 = spawnSync('sh', [path.join(app, 'deploy', 'setup-dev.sh')], { env, encoding: 'utf8' });
    const D = path.join(L, 'dev', 'app'), has = (p) => fs.existsSync(path.join(D, p));
    const cfg = (() => { try { return JSON.parse(fs.readFileSync(path.join(D, 'bridge', 'config.local.json'), 'utf8')); } catch { return {}; } })();
    const branch = g(D, 'rev-parse', '--abbrev-ref', 'HEAD').stdout.trim();
    fs.writeFileSync(path.join(D, 'Board', 'board.json'), '{"tasks":[{"id":"T-1"},{"id":"T-2"}]}'); // a change on dev
    const run2 = spawnSync('sh', [path.join(app, 'deploy', 'setup-dev.sh')], { env, encoding: 'utf8' });
    const kept = fs.readFileSync(path.join(D, 'Board', 'board.json'), 'utf8').includes('T-2'), liveSame = fs.readFileSync(path.join(app, 'bridge', 'config.local.json'), 'utf8') === liveCfg;
    const pair = spawnSync('sh', [path.join(app, 'deploy', 'setup-dev.sh'), 'pair'], { env, encoding: 'utf8' });
    const lc = JSON.parse(fs.readFileSync(path.join(app, 'bridge', 'config.local.json'), 'utf8')), dc = JSON.parse(fs.readFileSync(path.join(D, 'bridge', 'config.local.json'), 'utf8'));
    const secretMode = (() => { try { return (fs.statSync(path.join(L, 'sync.secret')).mode & 0o077) === 0; } catch { return false; } })();
    check('R494', 'the dev hub install (deploy/setup-dev.sh) clones develop beside live, copies live\'s data once (the board, notes, accounts) but never its sign-in sessions or device unlocks, makes dev\'s settings from live\'s (port 7778, instance dev, its own address, no AI hosts), leaves live\'s files alone, keeps what changed on dev when run again, and only `pair` turns the bridge on for both (one private secret, a graceful restart asked of each)',
      run1.status === 0 && branch === 'develop' && has('Board/board.json') && has('Notes/n.md') && has('bridge/auth.json') && !has('bridge/front-sessions.json') && !has('bridge/device-locks.json')
      && cfg.port === 7778 && cfg.instance === 'dev' && cfg.publicUrl === 'https://dev.example' && cfg.rustHub.backPort === 7781 && Object.keys(cfg.aiNodes || {}).length === 0 && !cfg.sync
      && run2.status === 0 && kept && liveSame
      && pair.status === 0 && lc.sync && lc.sync.peer === 'http://127.0.0.1:7778' && dc.sync && dc.sync.peer === 'http://127.0.0.1:7777' && lc.sync.secretFile === dc.sync.secretFile && secretMode
      && fs.existsSync(path.join(app, 'bridge', '.restart')) && fs.existsSync(path.join(D, 'bridge', '.restart')),
      `install ${run1.status} ${(run1.stdout + run1.stderr).trim().split('\n').slice(-2).join(' / ')} · branch ${branch} · sessions copied ${has('bridge/front-sessions.json')} · cfg ${JSON.stringify(cfg).slice(0, 160)} · again ${run2.status} kept ${kept} live unchanged ${liveSame} · pair ${pair.status} ${JSON.stringify(lc.sync)} ${JSON.stringify(dc.sync)} secret private ${secretMode}`);
  }
  if (RUST_HUB) { // R197: many Work sessions per person: concurrent runs, each in its own worktree; states with their precedence; open / hide / folders / delete (force-stops)
    const ss197 = async () => (await call('/api/work/sessions', U)).data;
    const one197 = async (id) => ((await ss197()).sessions || []).find((s) => s.id === id) || {};
    const cwd197 = (events) => ((texts(events).match(/cwd=(.+?)(?: \||$)/m) || [])[1] || '').trim();
    const has197 = (await call('/api/work/sessions', U)).status === 200;
    const seen = {}; let ok197 = false;
    if (has197) {
      const cur0 = (await ss197()).current;
      const a = (await call('/api/work/sessions/new', U, { title: 'r197 a', background: true })).data.id, b = (await call('/api/work/sessions/new', U, { title: 'r197 b', background: true })).data.id;
      seen.stillCurrent = (await ss197()).current === cur0;
      const ra = chat(U, 'SLOW r197 a', { session: a }), rb = chat(U, 'SLOW r197 b', { session: b });
      await sleep(900);
      const mid = await ss197(); seen.a = (mid.sessions || []).find((s) => s.id === a) || {}; seen.b = (mid.sessions || []).find((s) => s.id === b) || {}; seen.working = (mid.counts || {}).working;
      seen.again = (await call('/api/chat', U, { message: 'x', session: a })).status;
      seen.openBusy = (await call('/api/work/sessions/open', U, { id: b })).status; seen.openedB = (await ss197()).current === b; await call('/api/work/sessions/open', U, { id: cur0 });
      const [ea, eb] = await Promise.all([ra, rb]); await sleep(300);
      // each session its own worktree (Sponge: "each session gets its own worktree"): the runs happened in two folders on two session branches
      const wa = (await one197(a)).worktree || {}, wb = (await one197(b)).worktree || {};
      seen.cwdA = cwd197(ea.events); seen.cwdB = cwd197(eb.events); seen.brA = wa.branch || ''; seen.brB = wb.branch || '';
      seen.ownTrees = !!seen.cwdA && !!seen.cwdB && path.resolve(seen.cwdA) !== path.resolve(seen.cwdB) && !!wa.path && path.resolve(wa.path) === path.resolve(seen.cwdA)
        && /^work\/t-user\.[0-9a-f]+$/.test(seen.brA) && /^work\/t-user\.[0-9a-f]+$/.test(seen.brB) && seen.brA !== seen.brB;
      seen.aDone = (await one197(a)).state;
      await call('/api/work/draft', U, { session: a, draft: 'half typed r197' }); seen.aDraftUnseen = (await one197(a)).state;
      await call('/api/work/sessions/seen', U, { id: a }); seen.aDraft = (await one197(a)).state;
      await call('/api/work/draft', U, { session: a, draft: '' }); seen.aReady = (await one197(a)).state;
      await call('/api/work/current', U, { session: a, transcript: [{ cls: 'me', text: 'only in r197 a' }] });
      const curA = await call('/api/work/current?session=' + a, U), cur = await call('/api/work/current', U);
      seen.aOwn = curA.status === 200 && JSON.stringify(curA.data.transcript || []).includes('only in r197 a') && !JSON.stringify(cur.data.transcript || []).includes('only in r197 a');
      // closing = hiding: still listed (hidden), back with unhide
      await call('/api/work/sessions/hide', U, { id: a, hidden: true }); const hid = await one197(a); seen.hidden = hid.id === a && hid.hidden === true;
      await call('/api/work/sessions/hide', U, { id: a, hidden: false }); seen.unhidden = (await one197(a)).hidden === false;
      // folders: the person's own; deleting one moves its sessions out
      const f = (await call('/api/work/folders/new', U, { name: 'r197 folder' })).data.id;
      await call('/api/work/sessions/move', U, { id: b, folder: f }); seen.inFolder = !!f && (await one197(b)).folder === f;
      seen.othersFolders = JSON.stringify((await call('/api/work/sessions', D)).data.folders || []).includes('r197 folder');
      await call('/api/work/folders/delete', U, { id: f }); const bOut = await one197(b); seen.folderGone = bOut.id === b && !bOut.folder;
      // delete force-stops a running session
      const rc = chat(U, 'SLOW r197 c', { session: b }); await sleep(800);
      seen.del = (await call('/api/work/sessions/delete', U, { id: b })).status; const ec = await rc;
      seen.delStopped = !ec.events.some((e) => e.type === 'result'); seen.bGone = !((await ss197()).sessions || []).some((s) => s.id === b);
      await call('/api/work/sessions/delete', U, { id: a });
      ok197 = !!a && !!b && a !== b && seen.stillCurrent && seen.a.state === 'working' && seen.b.state === 'working' && seen.working >= 2 && seen.again === 409 && seen.openBusy === 200 && seen.openedB
        && seen.ownTrees && seen.aDone === 'done-unseen' && seen.aDraftUnseen === 'done-unseen' && seen.aDraft === 'draft' && seen.aReady === 'ready' && seen.aOwn
        && seen.hidden && seen.unhidden && seen.inFolder && !seen.othersFolders && seen.folderGone && seen.del === 200 && seen.delStopped && seen.bGone;
    }
    check('R197', 'many Work sessions per person (plans/multi-session.md): a new session in the background leaves the current one current; two sessions run at the same time (both "working", the counter says 2), each in its own worktree on its own work/<user>.<sid> branch, while a second message to the same session is still 409; opening another session is allowed while they run; a finished run is done-unseen until seen, which beats a draft; seen + draft → draft; no draft → ready; each session has its own conversation; closing hides (still listed, unhide brings it back); the person\'s own folders hold sessions and deleting one moves them out; delete force-stops a running session and removes it',
      ok197, has197 ? `a ${seen.a.state} b ${seen.b.state} counter ${seen.working} · same session again ${seen.again} · open while running ${seen.openBusy} (current ${seen.openedB}) · worktrees ${seen.cwdA || '?'} (${seen.brA || '?'}) vs ${seen.cwdB || '?'} (${seen.brB || '?'}) · after the run ${seen.aDone} · + draft ${seen.aDraftUnseen} · seen ${seen.aDraft} · no draft ${seen.aReady} · own conversation ${seen.aOwn} · hide ${seen.hidden} unhide ${seen.unhidden} · folder ${seen.inFolder} (seen by others ${seen.othersFolders}) folder deleted ${seen.folderGone} · delete while running ${seen.del} stopped ${seen.delStopped} gone ${seen.bGone}` : 'GET /api/work/sessions does not exist yet');
  } else skip('R197', 'many Work sessions per person (plans/multi-session.md)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R198: the limits: only working sessions count, 3 per provider on a host (5 per host) by default; over the limit → 429 with what limit
    const has198 = (await call('/api/work/sessions', U)).status === 200;
    const seen = {}; let ok198 = false;
    if (has198) {
      const ids = []; for (let i = 0; i < 4; i++) ids.push((await call('/api/work/sessions/new', U, { title: 'r198 ' + i, background: true })).data.id);
      const runs = ids.slice(0, 3).map((id) => call('/api/chat', U, { message: 'SLOW r198', session: id }));
      await sleep(900);
      const fourth = await call('/api/chat', U, { message: 'SLOW r198 four', session: ids[3] });
      seen.status = fourth.status; seen.limit = fourth.data.limit || {}; seen.error = fourth.data.error || '';
      seen.perHost = ((await call('/api/work/sessions', U)).data.counts || {}).perHost || {};
      await Promise.all(runs);
      for (const id of ids) await call('/api/work/sessions/delete', U, { id });
      ok198 = seen.status === 429 && seen.limit.max === 3 && seen.limit.used === 3 && !!seen.limit.provider && /3 of 3/.test(seen.error) && Object.values(seen.perHost).some((h) => h.used === 3);
    }
    check('R198', 'session limits (plans/multi-session.md § 6): with the defaults (5 per person per host, 3 per provider) a 4th working session on the same provider answers 429 (idle sessions cost nothing: the 4th exists, only its run is refused) with {error "… 3 of 3", limit {host, used, max, provider}}; /api/work/sessions counts what each host has running',
      ok198, has198 ? `4th run ${seen.status} "${seen.error}" limit ${JSON.stringify(seen.limit)} · perHost ${JSON.stringify(seen.perHost)}` : 'GET /api/work/sessions does not exist yet');
  } else skip('R198', 'session limits (plans/multi-session.md § 6)', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { if (!process.argv.includes('--only-r489')) await r489Check(); } else skip('R489', R489_LABEL, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R285 to R289 (T-289): other tools (Jcode PGM) report their sessions; the person's list shows them as `external`
    const s285 = {}, now285 = Date.now();
    const rep = (tok, body) => call('/api/work/sessions/report', tok, body);
    const ext = async (tok) => (await call('/api/work/sessions', tok)).data || {};
    const base = await ext(U);
    s285.r1 = await rep(U, { source: 'jcode', device: 'r285 laptop', sessions: [
      { id: 'jc-1', title: 'Fix the parser', state: 'working', project: 'viberoller', branch: 'work/x', host: 'laptop', updatedAt: now285 },
      { id: 'jc-2', title: 'Ask me', state: 'waiting', updatedAt: now285 - 5000 },
      { id: 'jc-3', title: 'Typed', state: 'draft', updatedAt: now285 - 9000 }] });
    const g1 = await ext(U);
    s285.list = (g1.external || []).filter((x) => x.source === 'jcode');
    const j1 = s285.list.find((x) => x.id === 'jc-1') || {};
    s285.fields = j1.title === 'Fix the parser' && j1.state === 'working' && j1.project === 'viberoller' && j1.branch === 'work/x' && j1.source === 'jcode' && typeof j1.updatedAt === 'number';
    s285.ew = (g1.counts || {}).externalWorking;
    check('R285', 'T-289 POST /api/work/sessions/report keeps another tool\'s sessions per person: GET /api/work/sessions then has `external` [{source, id, title, state, project, branch, updatedAt}] (newest first) and counts.externalWorking counts the working ones',
      s285.r1.status === 200 && s285.r1.data.accepted === 3 && s285.list.length === 3 && s285.fields && s285.list[0].id === 'jc-1' && s285.ew === 1, `report ${s285.r1.status} accepted ${s285.r1.data.accepted} · listed ${s285.list.length} · fields ${s285.fields} · externalWorking ${s285.ew}`);
    // R286: validation and cutting
    s285.bad = await rep(U, { source: 'jcode', device: 'r286', sessions: [
      { id: 'ok-1', title: 'x'.repeat(500), state: 'ready', project: 'p'.repeat(500), branch: 'b\nc', updatedAt: now285 },
      { id: 'bad-state', title: 'no', state: 'sleeping' }, { id: '', title: 'no id', state: 'idle' }, { title: 'no id at all', state: 'idle' }] });
    s285.noSource = await rep(U, { sessions: [] }); s285.noList = await rep(U, { source: 'jcode' }); s285.anon = await rep(null, { source: 'jcode', sessions: [] });
    const g2 = await ext(U); const o1 = (g2.external || []).find((x) => x.id === 'ok-1') || {};
    s285.cut = (o1.title || '').length === 120 && (o1.project || '').length === 80 && /^b ?c$/.test(o1.branch || '');
    s285.dropped = !(g2.external || []).some((x) => x.id === 'bad-state' || x.id === '');
    check('R286', 'T-289 the report is validated: only the six states are kept (others dropped), a session without an id is dropped, title / project / branch are cut to 120 / 80 / 80 on one line, no source or no list → 400, signed out → 401',
      s285.cut && s285.dropped && s285.bad.data.accepted === 1 && s285.noSource.status === 400 && s285.noList.status === 400 && s285.anon.status === 401, `cut ${s285.cut} · dropped ${s285.dropped} · accepted ${s285.bad.data.accepted} · no source ${s285.noSource.status} no list ${s285.noList.status} signed out ${s285.anon.status}`);
    // R287: per person, and a new report of the same source + device replaces the old one; another device is kept next to it
    const dOther = await ext(D);
    s285.rep2 = await rep(U, { source: 'jcode', device: 'r285 laptop', sessions: [{ id: 'jc-1', title: 'Now ready', state: 'ready', updatedAt: Date.now() }] });
    const g3 = await ext(U);
    const mine = (g3.external || []).filter((x) => x.source === 'jcode' && x.device === 'r285 laptop' || ['jc-1', 'jc-2', 'jc-3'].includes(x.id));
    s285.replaced = mine.length === 1 && mine[0].state === 'ready' && (g3.external || []).some((x) => x.id === 'ok-1');
    check('R287', 'T-289 reports are the person\'s own (another person\'s list has none of them), and a new report of the same source + device replaces the old one while another device\'s report stays',
      !(dOther.external || []).some((x) => ['jc-1', 'jc-2', 'jc-3', 'ok-1'].includes(x.id)) && s285.replaced && ((g3.counts || {}).externalWorking === 0), `others see ${(dOther.external || []).length} · replaced ${s285.replaced} · externalWorking ${(g3.counts || {}).externalWorking}`);
    // R288: a report that is not refreshed expires
    s285.short = await rep(U, { source: 'r288', device: 'x', ttlSec: 1, sessions: [{ id: 'gone-1', title: 'soon gone', state: 'working', updatedAt: Date.now() }] });
    const here = ((await ext(U)).external || []).some((x) => x.id === 'gone-1');
    await sleep(1600);
    const g4 = await ext(U); const stays = (g4.external || []).some((x) => x.id === 'ok-1');
    check('R288', 'T-289 a report that is not refreshed expires (ttlSec 1 here; 3 minutes by default), the other reports stay, and the expired working session no longer counts',
      here && !(g4.external || []).some((x) => x.id === 'gone-1') && stays && (g4.counts || {}).externalWorking === 0, `listed at first ${here} · after 1.6 s gone ${!(g4.external || []).some((x) => x.id === 'gone-1')} · others stay ${stays}`);
    // R289: nothing existing changes: the hub's own sessions and counts are the same, the old fields keep their shape
    check('R289', 'T-289 the report changes nothing of the hub\'s own list: sessions, folders and counts.working / waiting are as before, and a page that ignores `external` sees the same shape',
      JSON.stringify((await ext(U)).sessions.map((x) => x.id)) === JSON.stringify(base.sessions.map((x) => x.id)) && typeof (g4.counts || {}).working === 'number' && (g4.counts || {}).working === (base.counts || {}).working && Array.isArray(g4.folders) && typeof g4.current === 'string', `sessions ${(g4.sessions || []).length} vs ${(base.sessions || []).length} · working ${(g4.counts || {}).working}`);
  } else for (const [id, what] of [['R285', 'external session report'], ['R286', 'report validation'], ['R287', 'reports per person'], ['R288', 'report expiry'], ['R289', 'report leaves the list alone']]) skip(id, 'T-289 ' + what, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R300 to R304 (T-276, T-277, T-278): the planner's exact helpers as team tools: parse, existing, roles, owner, glossary
    const plan = async (calls) => { const r = await chat(U, 'NOTIFYTOOL ' + JSON.stringify(calls) + ' END', { fresh: true }); try { return JSON.parse((texts(r.events).match(/NOTIFY:(\[.*\])/) || [])[1]); } catch { return []; } };
    const excerpt = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-plan-internal', 'src', 'planner-example.md'), 'utf8');
    const shortList = 'Person M\n\n--- fable\n[Board Archiving]\nArchive done tasks after about 48 hours and delete the archived ones after 30 days.\n';
    const longList = '--- opus\n[Automated Task Board Archiving System]\n\nWe need the ability for the task boards to be able to archive tasks, and delete tasks that have been archived after ~30 days. Done tasks should auto archive after ~48 hours, but can be manually or AI archived.\n';
    const runOn = 'Hey, we need a board archive so the done tab stops filling up. Also we should have a right-click menu in the web version, plus maybe a queued messages panel. Oh and the board should show who is online.';
    const [pEx, pMerge, pRun, pEmpty] = await plan([['plan_parse', { text: excerpt }], ['plan_parse', { text: shortList + '\n' + longList }], ['plan_parse', { text: runOn }], ['plan_parse', { text: '   ' }]]);
    const ex = (pEx && pEx.ok && pEx.result.ideas) || [];
    const by = (t) => ex.find((i) => i.title.includes(t)) || {};
    const hw = by('Hyper Workflows Architecture').n;
    const blocked = ex.filter((i) => i.blockedOn.length);
    const mg = (pMerge && pMerge.ok && pMerge.result) || { ideas: [], counts: {} };
    const rn = (pRun && pRun.ok && pRun.result.ideas) || [];
    check('R300', 'T-276 plan_parse on an excerpt of the 2026-10-24 idea list gives its 11 ideas with the model tag, the person section owner and the source lines (hyper workflows, REA, queued panel …), the four "blocked on" ideas point at the hyper workflows idea, and only markers / role lines are left over; a short and a long version of one idea merge into one (longer wording kept, the other model tag noted, the person kept); run-on chat is split at each new wish; an empty input is an error',
      ex.length === 11 && by('Queued Messages').person === 'mills' && by('Queued Messages').model === 'opus' && by('Website Builder').model === 'fable' && by('REA Project').person === 'sponge' && hw > 0
        && blocked.length === 4 && blocked.every((b) => b.blockedOnIdeas.length === 1 && b.blockedOnIdeas[0] === hw) && by('Queued Messages').sourceSpan.from < by('Queued Messages').sourceSpan.to
        && pEx.result.leftovers.every((l) => /marker|role line/.test(l.reason)) && pEx.result.people.length === 3 && pEx.result.people.every((x) => x.known && !x.mismatch)
        && mg.ideas.length === 1 && mg.counts.merged === 1 && mg.ideas[0].title === 'Automated Task Board Archiving System' && mg.ideas[0].model === 'opus' && mg.ideas[0].person === 'mills' && ((mg.ideas[0].differs || [])[0] || {}).other === 'fable'
        && rn.length === 4 && /right-click/i.test(rn[1].title) && rn.every((i) => i.form === 'D') && !!pEmpty && pEmpty.ok === false,
      `ideas ${ex.length} · blocked ${blocked.length} on ${blocked.map((b) => b.blockedOnIdeas)} (hyper ${hw}) · people ${pEx && pEx.result && pEx.result.people.map((x) => x.person + (x.mismatch ? '!' : ''))} · merged ${mg.counts.merged}/${mg.ideas.length} · run-on ${rn.map((i) => i.title)} · empty ${pEmpty && pEmpty.ok}`);

    const mkTask = async (b) => ((await call('/api/board/create', U, b)).data.task || {}).id;
    const t301 = [await mkTask({ title: 'T-184 · 1/10: Hyper workflows spec: node-based independent system', status: 'todo', labels: 'T-184' }), await mkTask({ title: 'Queued messages sidebar panel', status: 'todo' }), await mkTask({ title: 'Presence and activity for the team', status: 'todo' })];
    await call('/api/board/complete', U, { id: t301[1] });
    const [e1, e2, e3, e4, e5] = await plan([['plan_existing', { title: 'Node-Based Hyper Workflows Architecture' }], ['plan_existing', { title: 'Hidable Queued Messages Sidebar Panel', words: 'view queued messages' }], ['plan_existing', { title: 'Zzyzx Quuxbar Wibblewobble' }], ['plan_existing', { title: '' }], ['plan_existing', { title: 'Queued messages sidebar panel', days: 0 }]]);
    const mt = (e) => ((e && e.ok && e.result.matches) || []);
    check('R301', 'T-276 plan_existing finds open tasks and recently done ones by word overlap (the open hyper workflows spec for the hyper workflows idea, the done queued panel with done: true, scored best first), nothing for words the board never used, an error for an empty title, and days: 0 leaves out a done task',
      mt(e1)[0] && mt(e1)[0].id === t301[0] && !mt(e1).some((m) => m.id === t301[2]) && mt(e2)[0] && mt(e2)[0].id === t301[1] && mt(e2)[0].done === true && mt(e2)[0].status === 'done' && mt(e2)[0].score >= 0.4 && mt(e3).length === 0 && e4 && e4.ok === false && !mt(e5).some((m) => m.id === t301[1]),
      `hyper → ${mt(e1).map((m) => m.id + ':' + m.score)} (want ${t301[0]}) · queued → ${mt(e2).map((m) => m.id + ':' + m.done)} (want ${t301[1]}) · nonsense ${mt(e3).length} · empty ${e4 && e4.ok} · days 0 → ${mt(e5).length}`);

    // roles: no note → built-in; a Roles section in plans/task-assignments.md; the roles file wins
    const rolesFile = JSON.stringify({ fallback: 't-user', people: [{ user: 't-user', name: 'Uma', initial: 'U', specialties: ['zebra herding'], keywords: ['zebra', 'stripes'] }, { user: 't-admin', name: 'Adam', initial: 'A', specialties: ['giraffe care'], keywords: ['giraffe'] }] });
    const assignNote = '# Assignments\n\n## Roles\n\n- **Sponge** (`sponge`): hub work.\n- **Ann** (`ann`): illustration, UI design.\n\n## Board rules\n\n1. x\n';
    for (const x of ['plans/roles.md', 'plans/task-assignments.md']) await call('/api/notes/delete', U, { path: x });
    const t302 = [await mkTask({ title: 'r302 zebra one', status: 'todo', assignee: 't-user' }), await mkTask({ title: 'r302 zebra two', status: 'doing', assignee: 't-user', priority: 1 }), await mkTask({ title: 'r302 zebra three', status: 'todo', assignee: 't-user' }), await mkTask({ title: 'r302 zebra four', status: 'todo', assignee: 't-user' })];
    const r = await plan([['plan_roles', {}], ['notes_write', { path: 'plans/task-assignments.md', text: assignNote }], ['plan_roles', {}], ['notes_write', { path: 'plans/roles.md', text: '# Roles\n\n```json\n' + rolesFile + '\n```\n' }], ['plan_roles', {}]]);
    const rr = (x) => (x && x.ok && x.result) || { people: [] };
    const wlOf = (x, u) => (rr(x).people.find((p) => p.user === u) || {}).workload || {};
    check('R302', 'T-277 plan_roles gives the team\'s roles and each person\'s open workload and says where they came from: built-in (sponge, mills, gee) when the Notes have nothing, the Roles section of plans/task-assignments.md when it exists, and plans/roles.md above both (its fallback, names and keywords); the workload counts a person\'s open tasks by status and priority, held ones apart (done ones not)',
      rr(r[0]).source === 'built-in' && rr(r[0]).people.map((p) => p.user).join() === 'sponge,mills,gee' && rr(r[0]).fallback === 'mills'
        && rr(r[2]).source === 'plans/task-assignments.md' && rr(r[2]).people.map((p) => p.user).join() === 'sponge,ann' && rr(r[2]).people[1].specialties.join() === 'illustration,UI design'
        && rr(r[4]).source === 'plans/roles.md' && rr(r[4]).people.map((p) => p.name).join() === 'Uma,Adam' && rr(r[4]).fallback === 't-user' && rr(r[4]).people[0].keywords.includes('zebra')
        && wlOf(r[4], 't-user').open >= 4 && wlOf(r[4], 't-user').byPriority['1'] >= 1 && wlOf(r[4], 't-user').active >= 4,
      `sources ${rr(r[0]).source} / ${rr(r[2]).source} / ${rr(r[4]).source} · people ${rr(r[0]).people.map((p) => p.user)} / ${rr(r[2]).people.map((p) => p.user)} / ${rr(r[4]).people.map((p) => p.name)} · t-user workload ${JSON.stringify(wlOf(r[4], 't-user'))}`);

    const o = await plan([['plan_roles', {}], ['plan_owner', { idea: { title: 'Anything at all', person: 'A' } }], ['plan_owner', { idea: { title: 'Zebra stripes research', body: '' } }], ['plan_owner', { idea: { title: 'Zebra giraffe parade' } }], ['plan_owner', { idea: { title: 'Qqq www eee', model: 'opus' } }], ['plan_owner', { ideas: [{ title: 'Giraffe feeding', model: 'fable', blockedOn: ['Zebra'] }, 'Zebra'] }], ['plan_owner', {}]]);
    const wl = (u) => ((rr(o[0]).people.find((p) => p.user === u) || {}).workload || {}).load;
    const lighter = wl('t-admin') < wl('t-user') ? 't-admin' : 't-user';
    const ow = (x) => (x && x.ok && x.result) || {};
    const many = (o[5] && o[5].ok && o[5].result.owners) || [];
    check('R303', 'T-277 plan_owner takes an explicit marker first (Person A → Adam), then role keywords (zebra → Uma, giraffe → Adam), a tie or no match by the lighter open workload (the second case says needsPick), is never empty, returns the model tag as a label and a priority (a blocked idea 3), several ideas at once, and an error without an idea',
      ow(o[1]).owner === 't-admin' && ow(o[1]).by === 'marker' && ow(o[2]).owner === 't-user' && ow(o[2]).by === 'role' && /zebra/.test(ow(o[2]).reason)
        && ow(o[3]).by === 'workload' && ow(o[3]).owner === lighter && ow(o[4]).owner === lighter && ow(o[4]).needsPick === true && ow(o[4]).labels.includes('opus') && ow(o[4]).priority === 2
        && many.length === 2 && many[0].owner === 't-admin' && many[0].priority === 3 && many[0].labels.includes('fable') && !!many[1].owner && !!ow(o[3]).reason && !!o[6] && o[6].ok === false,
      `marker ${ow(o[1]).owner}/${ow(o[1]).by} · zebra ${ow(o[2]).owner}/${ow(o[2]).by} · tie ${ow(o[3]).owner}/${ow(o[3]).by} (lighter ${lighter}: ${wl('t-admin')} vs ${wl('t-user')}) · none ${ow(o[4]).owner}/${ow(o[4]).by} pick ${ow(o[4]).needsPick} · many ${many.map((m) => m.owner + ':' + m.priority)} · empty ${o[6] && o[6].ok}`);
    for (const id of [...t301, ...t302]) await call('/api/board/delete', U, { id });
    for (const x of ['plans/roles.md', 'plans/task-assignments.md', 'plans/glossary.md']) await call('/api/notes/delete', U, { path: x });

    const glNote = '# Team glossary\n\n## Words\n\n- **Hyper workflows / hyper work:** an **independent system** of our own.\n- **"Blocked on X":** means **based on X**: built from it. (Sponge, 2026-10-24)\n\n## Decisions\n\n- **Everything private by default.** Built for the team first. (Sponge, 2026-10-24)\n';
    const pid304 = (await call('/api/work/sessions/new', U, { title: 'r304 planner', background: true, mode: 'planner' })).data.id;
    const nid304 = (await call('/api/work/sessions/new', U, { title: 'r304 normal', background: true })).data.id;
    const planRes = (sid) => chat(U, 'PLANNERPLAN please', { session: sid }).then((x) => { try { return JSON.parse((texts(x.events).match(/PLANRES:(\{.*\})/) || [])[1]); } catch { return {}; } });
    const [g0, , g1] = await plan([['plan_glossary', {}], ['notes_write', { path: 'plans/glossary.md', text: glNote }], ['plan_glossary', {}]]);
    const pr304 = await planRes(pid304), nr304 = await planRes(nid304);
    const gr = (x) => (x && x.ok && x.result) || { words: [], decisions: [] };
    const jsNames = (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1));
    const rsNames = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.map((t) => t.name);
    const five = ['plan_parse', 'plan_existing', 'plan_roles', 'plan_owner', 'plan_glossary'];
    check('R304', 'T-278 plan_glossary gives the words and decisions of plans/glossary.md (aliases, meaning, source; built-in minimum when the note is missing, said in its source); a planner session\'s prompt tells it to call plan_glossary, plan_roles, plan_parse, plan_existing and plan_owner in that order and that "blocked on" means based on, and its run may call them through the relay (a normal session too); [static] both team MCP servers (team-mcp.js, mcp-tools.json) list the five tools',
      /built-in/.test(gr(g0).source) && gr(g0).words.length >= 4 && gr(g1).source === 'plans/glossary.md' && gr(g1).words.length === 2 && gr(g1).words[0].aliases.join() === 'hyper workflows,hyper work' && /^means based on X/.test(gr(g1).words[1].meaning) && gr(g1).words[1].source === 'Sponge, 2026-10-24' && gr(g1).decisions[0].title === 'Everything private by default'
        && pr304.order === true && pr304.basedOn === true && pr304.glossary === 2 && pr304.ideas === 1 && pr304.owner === 'sponge' && nr304.glossary === 2 && nr304.ideas === 1 && nr304.order === false
        && five.every((t) => jsNames.includes(t) && rsNames.includes(t)),
      `glossary ${gr(g0).source} (${gr(g0).words.length} words) / ${gr(g1).source} (${gr(g1).words.length}) · planner prompt order ${pr304.order} basedOn ${pr304.basedOn} tools ${pr304.glossary}/${pr304.ideas}/${pr304.owner} · normal ${nr304.glossary}/${nr304.ideas}/${nr304.order} · js ${five.filter((t) => jsNames.includes(t)).length} rs ${five.filter((t) => rsNames.includes(t)).length}`);
    await call('/api/notes/delete', U, { path: 'plans/glossary.md' });
    for (const id of [pid304, nid304]) await call('/api/work/sessions/delete', U, { id });
  } else for (const [id, what] of [['R300', 'plan_parse'], ['R301', 'plan_existing'], ['R302', 'plan_roles'], ['R303', 'plan_owner'], ['R304', 'plan_glossary and the planner prompt']]) skip(id, 'T-276/277/278 ' + what, 'the Rust hub\'s: --rust-hub runs it');
  { // R310 to R314 (T-208): the person's own local Ollama, found by the page (browser) or the desktop app, listed as "Local (this computer)"
    const page208 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), main208 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-desktop', 'src', 'main.rs'), 'utf8');
    const blk208 = fs.readFileSync(path.join(MILLS, 'web', 'localai.js'), 'utf8'); // T-209: the probe lives in the shared module, not in the page
    let names208 = [], bad208 = [];
    try {
      const m = /const fromTags = (.*);\n/.exec(blk208), fromTags = new Function('return ' + m[1])();
      const tags = await (await fetch(`http://127.0.0.1:${OLLAMA_PORT}/api/tags`)).json(); // the suite's fake Ollama, as a browser would be answered
      names208 = fromTags(tags); bad208 = [fromTags(null), fromTags({ error: 'x' }), fromTags({ models: [{ size: 1 }, { model: 'b:1' }, 7] })];
    } catch (e) { names208 = ['ERR ' + e.message]; }
    check('R310', 'T-208 the page reads Ollama\'s /api/tags answer (the fake Ollama\'s here) into its model names, and an answer that is not Ollama\'s gives none instead of an error',
      names208.join() === 'unity-brain:latest' && bad208[0].length === 0 && bad208[1].length === 0 && bad208[2].join() === 'b:1', `names ${names208.join()} · odd ${JSON.stringify(bad208)}`);
    check('R311', 'T-208 the page asks 127.0.0.1:11434 /api/tags with a short timeout; a browser that is refused (CORS / private network) is told apart from "not running" by a no-cors ping, and only then is OLLAMA_ORIGINS explained (small hint, no error wall); when nothing runs, nothing is drawn',
      /LOCAL_OLLAMA = 'http:\/\/127\.0\.0\.1:11434'/.test(blk208) && /\/api\/tags/.test(blk208) && /abort\(\), 1500\)/.test(blk208) && /mode: 'no-cors'/.test(blk208) && /r\.blocked = true/.test(blk208) && /OLLAMA_ORIGINS="' \+ \(o\.origin \|\| location\.origin\)/.test(blk208) && /wrap\.hidden = true/.test(blk208) && /if \(!r\.found && !r\.blocked\) return;/.test(blk208) && !/sys\(|sysSettings\(|alert\(/.test(blk208.split('function line')[0]) && /<script src="localai\.js"><\/script>/.test(page208) && /LOCALAI\.line\(box, d\.providers/.test(page208) && !/LOCAL_OLLAMA|api\/tags/.test(page208) && /connect-src 'self' http:\/\/127\.0\.0\.1:11434 /.test(page208), 'static check of the module and the page');
    check('R312', 'T-208 the desktop app asks Ollama through its own program (command ollama_local, no CORS: OLLAMA_HOST or 127.0.0.1:11434) and the page uses it there; it also offers "Start this computer\'s AI host" (host_act start, or the set-up pointer) when none of the person\'s hosts lends a local model, and names where to lend it in a browser',
      /async fn ollama_local\(\)/.test(main208) && /probe_ollama_blocking/.test(main208) && /host_status, ollama_local, host_act/.test(main208) && /native\.call\('ollama_local'\)/.test(blk208) && /native\.call\('host_act', \{ what: 'start' \}\)/.test(blk208) && /Run on this PC/.test(blk208) && /My AI hosts/.test(blk208), 'static check of the page and the app');
    check('R313', 'T-208 the shared provider stays the default: the local list is display only (greyed, never a provider option the hub knows, no call to /api/model or /api/apikey/settings from the probe), and it is skipped when one of the person\'s own hosts already lends a local model',
      !/\/api\/model|\/api\/apikey|\/api\/providers/.test(blk208) && /op\.disabled = true/.test(blk208.slice(blk208.indexOf('Local (this computer) · not for Work'), blk208.indexOf('Local (this computer) · not for Work') + 500)) && /localOllama\.found && localOllama\.models\.length && !ownProvider\(provs, o && o\.user\)/.test(blk208) && /LOCALAI\.group\(sel, provs/.test(page208) && /p\.kind === 'host-openai' && p\.owner === /.test(blk208), 'static check of the page');
    const ck208 = (await call('/api/check', U)).data || {};
    check('R314', 'T-208 a person without a local Ollama sees nothing change: /api/check lists no provider for it (the page\'s probe adds nothing on the hub), and the default provider / model fields keep their shape',
      Array.isArray(ck208.providers) && !ck208.providers.some((p) => /local-ollama/.test(p.id || '')) && 'defaultProvider' in ck208 && Array.isArray(ck208.models), `providers ${(ck208.providers || []).length} · defaultProvider ${'defaultProvider' in ck208}`);
  }
  if (RUST_HUB) { // R315 (T-301): a person's own AI outside a Work run (Jcode PGM's planner) signs its board writes "<person> · Claude"
    const t315 = ((await call('/api/board/create', U, { title: 'agent write r315', status: 'todo', asAgent: true })).data || {}).task || {};
    const c315 = ((await call('/api/board/claim', U, { id: t315.id, asAgent: true })).data || {}).task || {};
    const p315 = ((await call('/api/board/create', U, { title: 'person write r315', status: 'todo' })).data || {}).task || {};
    const pc315 = ((await call('/api/board/claim', U, { id: p315.id })).data || {}).task || {};
    check('R315', 'T-301 asAgent on the board routes: the claim of a task written with asAgent is the person\'s agent ("<name> · Claude", agent true, key <user>#agent) and the field is not stored on the task; without it the write stays the person\'s own (older callers unchanged)',
      !!t315.id && !('asAgent' in t315) && c315.claim && c315.claim.agent === true && / · Claude$/.test(c315.claim.by || '') && /#agent$/.test(c315.claim.key || '') && pc315.claim && pc315.claim.agent === false && !/ · Claude$/.test(pc315.claim.by || ''),
      `agent claim ${JSON.stringify(c315.claim || null)} · person claim ${JSON.stringify(pc315.claim || null)}`);
    // leave the board as it was: the planner checks after this one weigh owners by their open workload
    for (const t of [t315, p315]) if (t.id) { await call('/api/board/release', U, { id: t.id, asAgent: t === t315 }); await call('/api/board/delete', U, { id: t.id }); }
  } else skip('R315', 'asAgent board writes', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R325 and R326 (T-309): linking a machine by device code, its key, the owner's list, the admin's count, revoke, re-link
    const mc = async (method, p, tok, body) => { // (call() does GET and POST only)
      try { const r = await fetch(H + p, { method, headers: { 'Content-Type': 'application/json', ...(tok ? { Authorization: 'Bearer ' + tok } : {}) }, body: body !== undefined ? JSON.stringify(body) : undefined }); return { status: r.status, data: await r.json().catch(() => ({})) }; } catch (e) { return { status: 0, data: { error: e.message } }; }
    };
    const newKey = () => { const k = crypto.generateKeyPairSync('ed25519'); return { priv: k.privateKey, pub: k.publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('hex') }; };
    const signReply = (k, id, rid, body) => crypto.sign(null, Buffer.from(`vr-machine-reply/1\n${id}\n${rid}\n${crypto.createHash('sha256').update(body).digest('hex')}`), k.priv).toString('hex');
    const mstart = (k, extra) => call('/api/device/start', null, { client: 'vr-machine', device: 'r325-box', publicKey: k.pub, machineId: 'mach-r325-aaaa', os: 'linux', app: 'vr-host', appVersion: '1.2.3', mode: 'persistent', folder: '/srv/r325/work', ...(extra || {}) });
    const mlink = async (owner, k, extra, replace) => { // start, approve (with the person's session), poll once: the token (or why not)
      const s = await mstart(k, extra), d = s.data || {};
      const ok = await call('/api/device/approve', owner, { userCode: d.userCode, ...(replace ? { replace } : {}) });
      const p = await call('/api/device/poll', null, { deviceCode: d.deviceCode });
      return { s, ok, p, token: (p.data || {}).token, id: (p.data || {}).deviceId, userCode: d.userCode };
    };
    const created = (await call('/api/admin/create', A, { username: 't-r325', display: 'R325' })).data || {};
    const X = created.password ? (await login('t-r325', created.password, 'device-r325-xxxxxxxxxx')).data.token : '';
    const listOf = async (t) => ((await call('/api/machines', t)).data || {}).machines || [];
    try {
      // ---- R325: linking
      const k1 = newKey();
      const noKey = await call('/api/device/start', null, { client: 'vr-machine', device: 'r325-box' });
      const s1 = await mstart(k1), d1 = s1.data || {};
      const peekNoSession = await call('/api/device/peek', null, { userCode: d1.userCode });
      const peekViaDev = await call('/api/device/peek', U, { userCode: 'BBBB-BBBB' });
      const peek = await call('/api/device/peek', X, { userCode: d1.userCode });
      const ap = await call('/api/device/approve', X, { userCode: d1.userCode });
      const p1 = await call('/api/device/poll', null, { deviceCode: d1.deviceCode });
      const tok = (p1.data || {}).token, id = (p1.data || {}).deviceId;
      const whoAmI = tok ? (await call('/api/check', tok)).data : {};
      const listX = await listOf(X), listU = await listOf(U), listA = await listOf(A);
      const viaMachine = tok ? await call('/api/machines', tok) : {};
      const devList = ((await call('/api/devices', X)).data || {}).devices || [];
      const raw = fs.existsSync(path.join(SB, 'device-tokens.json')) ? fs.readFileSync(path.join(SB, 'device-tokens.json'), 'utf8') : '';
      const body = JSON.stringify({ result: 'ok', n: 1 });
      const rep = (rid, b, sig) => call('/api/machine/reply', tok, { requestId: rid, body: b, sig });
      const good = await rep('rq1', body, signReply(k1, id, 'rq1', body));
      const wrongKey = await rep('rq1', body, signReply(newKey(), id, 'rq1', body));
      const tampered = await rep('rq1', body + ' ', signReply(k1, id, 'rq1', body));
      const otherReq = await rep('rq2', body, signReply(k1, id, 'rq1', body));
      const noSig = await rep('rq1', body, '');
      const polled = await call('/api/machine/poll', tok, { wait: 0 });
      const onlineX = (await listOf(X))[0] || {};
      const cnt = await call('/api/machines/count', A), cntU = await call('/api/machines/count', U), cntX = await call('/api/machines/count', X), cntNone = await call('/api/machines/count', null);
      const mine = (cnt.data.people || []).find((x) => x.user === 't-r325') || {};
      const renU = await mc('PATCH', '/api/machines/' + id, U, { name: 'stolen' }), renX = await mc('PATCH', '/api/machines/' + id, X, { name: 'Desk r325' }), renBad = await mc('PATCH', '/api/machines/' + id, X, { name: '  ' });
      const wide = await call('/api/machine/poll', tok, { wait: 0, scope: 'folders: /srv (read-write); shell (advisory)' });
      const afterRename = (await listOf(X))[0] || {};
      check('R325', 'T-309 link a machine by device code (client vr-machine): a start without a valid ed25519 key is refused (400); start names the default scope (one folder, read-write) and the approval screen (peek, own session only) shows os, app, key fingerprint and that scope; approving links it to the approving account only (its token is that account on /api/machine only: T-310 refuses it everywhere else, see R327); its replies are signed by its device key and the hub refuses another key, another body, another request and no signature; the owner\'s list has only their machines (kind, mode, online, scope in words, never a token or key), the admin sees counts and online numbers only (no name, os, scope), a machine token cannot manage machines; rename works for the owner only; the hub keeps only a hash of the token',
        noKey.status === 400 && s1.status === 200 && d1.kind === 'machine' && /^one folder: \/srv\/r325\/work \(read-write\)/.test((d1.defaultScope || {}).summary || '') && peekNoSession.status === 401 && [404, 429].includes(peekViaDev.status)
        && peek.status === 200 && peek.data.kind === 'machine' && peek.data.os === 'linux' && peek.data.app === 'vr-host' && /^[0-9a-f]{4}(-[0-9a-f]{4}){3}$/.test(peek.data.keyFingerprint || '') && /^one folder: /.test((peek.data.defaultScope || {}).summary || '') && Array.isArray(peek.data.replaceable) && !peek.data.replaceable.length
        && ap.status === 200 && p1.status === 200 && !!tok && !!id && p1.data.kind === 'machine' && !whoAmI.username
        && listX.length === 1 && listX[0].id === id && listX[0].kind === 'machine' && listX[0].mode === 'persistent' && listX[0].online === false && /^one folder: /.test(listX[0].scope) && !JSON.stringify(listX).includes(tok) && !JSON.stringify(listX).includes(k1.pub) && !listU.length && !listA.length
        && viaMachine.status === 403 && devList.some((x) => x.id === id && x.kind === 'machine') && raw.includes(crypto.createHash('sha256').update(tok).digest('hex')) && !raw.includes(tok)
        && good.status === 200 && good.data.owner === 't-r325' && good.data.machine === id && wrongKey.status === 403 && tampered.status === 403 && otherReq.status === 403 && noSig.status === 403
        && polled.status === 200 && polled.data.owner === 't-r325' && onlineX.online === true
        && cnt.status === 200 && cnt.data.machines >= 1 && mine.machines === 1 && mine.online === 1 && Object.keys(mine).sort().join() === 'machines,online,user' && !/Desk|linux|vr-host|scope|folder/.test(JSON.stringify(cnt.data)) && cntU.status === 403 && cntX.status === 403 && cntNone.status === 401
        && renU.status === 404 && renX.status === 200 && renBad.status === 400 && afterRename.name === 'Desk r325' && wide.status === 200 && (await listOf(X))[0].scope.startsWith('folders: /srv') && (await listOf(X))[0].scopeIsDefault === false,
        `start ${s1.status}/nokey ${noKey.status} scope "${(d1.defaultScope || {}).summary}" · peek ${peek.status} ${peek.data.keyFingerprint} · approve ${ap.status} poll ${p1.status} as ${whoAmI.username} · list X ${listX.length} U ${listU.length} A ${listA.length} via machine ${viaMachine.status} · reply good ${good.status}/wrong key ${wrongKey.status}/body ${tampered.status}/request ${otherReq.status}/none ${noSig.status} · online ${onlineX.online} · count ${cnt.status} ${JSON.stringify(mine)} U ${cntU.status} X ${cntX.status} · rename U ${renU.status} X ${renX.status} bad ${renBad.status}`);

      // ---- R326: unlink, revoke on a password change, re-link
      const inflight = (async () => { const t0 = Date.now(); const r = await call('/api/machine/poll', tok, { wait: 20 }); return { r, ms: Date.now() - t0 }; })();
      await sleep(600);
      const delU = await mc('DELETE', '/api/machines/' + id, U), delViaMachine = await mc('DELETE', '/api/machines/' + id, tok);
      const stillThere = (await listOf(X)).length;
      const delX = await mc('DELETE', '/api/machines/' + id, X);
      const held = await inflight;
      const afterPoll = await call('/api/machine/poll', tok, { wait: 0 }), afterReply = await rep('rq9', body, signReply(k1, id, 'rq9', body)), afterApi = await call('/api/check', tok);
      const afterList = await listOf(X), afterTwice = await mc('DELETE', '/api/machines/' + id, X);
      const unknown = await call('/api/machine/poll', 'f'.repeat(64), { wait: 0 });
      const machineSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-net-internal', 'src', 'machine', 'device.rs'), 'utf8');
      // a password change revokes the machines (and the person's other devices); "keep my machines linked" keeps them
      const k2 = newKey(), m2 = await mlink(X, k2, { machineId: 'mach-r325-bbbb' }), m3 = await mlink(X, k2, { machineId: 'mach-r325-cccc' });
      const pollOf = async (t) => (await call('/api/machine/poll', t, { wait: 0 }));
      const before = [(await pollOf(m2.token)).status, (await pollOf(m3.token)).status];
      const pw1 = await call('/api/admin/password', A, { username: 't-r325' });
      const afterPw = [await pollOf(m2.token), await pollOf(m3.token)];
      const X2 = (await login('t-r325', pw1.data.password, 'device-r325-xxxxxxxxxx')).data.token;
      const afterPwList = await listOf(X2);
      const m4 = await mlink(X2, k2, { machineId: 'mach-r325-dddd' });
      const pw2 = await call('/api/admin/password', A, { username: 't-r325', keepMachines: true });
      const kept = await pollOf(m4.token), keptSession = await call('/api/check', X2);
      const X3 = (await login('t-r325', pw2.data.password, 'device-r325-xxxxxxxxxx')).data.token;
      const keptList = await listOf(X3);
      // re-link after a wiped store: the same machine id is offered "replace old record", the old token ends, the scope is the default again
      const k5 = newKey(), old = await mlink(X3, k5, { machineId: 'mach-r325-eeee', device: 'r325-old' });
      await mc('PATCH', '/api/machines/' + old.id, X3, { name: 'My old desk', paused: true });
      await call('/api/machine/poll', old.token, { wait: 0, scope: 'folders: / (read-write); shell' });
      const k6 = newKey(), s6 = await mstart(k6, { machineId: 'mach-r325-eeee', device: 'r325-old', folder: '/srv/r325/new' }), d6 = s6.data || {};
      const peek6 = await call('/api/device/peek', X3, { userCode: d6.userCode });
      const stranger = await call('/api/device/approve', X3, { userCode: d6.userCode, replace: 'not-mine' });
      const offerU = await call('/api/device/peek', U, { userCode: d6.userCode });
      const ap6 = await call('/api/device/approve', X3, { userCode: d6.userCode, replace: old.id });
      const p6 = await call('/api/device/poll', null, { deviceCode: d6.deviceCode });
      const list6 = await listOf(X3);
      const oldNow = await call('/api/machine/poll', old.token, { wait: 0 });
      const newNow = await call('/api/machine/poll', (p6.data || {}).token, { wait: 0 });
      // no replace: both records exist until one is unlinked
      const k7 = newKey(), twin = await mlink(X3, k7, { machineId: 'mach-r325-eeee', device: 'r325-old' });
      const list7 = await listOf(X3);
      const so = await call('/api/admin/signout', A, { username: 't-r325' }), afterSignout = await pollOf(m4.token);
      const n6 = list6.find((x) => x.id === (p6.data || {}).deviceId) || {};
      const bits326 = [
        delU.status === 404, delViaMachine.status === 403, stillThere === 1, delX.status === 200, held.r.status === 401, held.r.data.revoked === true, held.ms < 5000,
        afterPoll.status === 401, afterPoll.data.error === 'revoked', afterReply.status === 401, afterReply.data.revoked === true, afterApi.status === 401, !afterList.length, afterTwice.status === 404, unknown.status === 401, !unknown.data.revoked,
        /fn handle_answer\(/.test(machineSrc), /fn store_is_owner_only_and_wipes\(/.test(machineSrc),
        m2.p.status === 200, m3.p.status === 200, before.join() === '200,200', pw1.status === 200, afterPw.every((x) => x.status === 401 && x.data.revoked === true), !afterPwList.length,
        m4.p.status === 200, pw2.status === 200, kept.status === 200, keptSession.status === 401, keptList.length === 1, keptList[0] && keptList[0].id === m4.id, so.status === 200, afterSignout.status === 401, afterSignout.data.revoked === true,
        s6.status === 200, peek6.status === 200, peek6.data.replaceable.length === 1, peek6.data.replaceable[0].id === old.id, peek6.data.replaceable[0].sameMachineId === true, stranger.status === 400, (offerU.status === 200 && !(offerU.data.replaceable || []).length),
        ap6.status === 200, p6.status === 200, (p6.data || {}).replaced === old.id, list6.length === 2, !list6.some((x) => x.id === old.id),
        n6.name === 'My old desk', /^one folder: \/srv\/r325\/new \(read-write\)/.test(n6.scope || ''), n6.scopeIsDefault === true, n6.paused === false, (n6.replaced || []).length === 1,
        oldNow.status === 401, oldNow.data.revoked === true, newNow.status === 200,
        twin.p.status === 200, list7.length === 3, list7.filter((x) => x.machineId === 'mach-r325-eeee').length === 2,
      ];
      check('R326', 'T-309 revoke and re-link: Unlink from the web (the owner\'s page only; not another person, not a machine token) closes a held poll within moments and the token is invalid at once for poll, reply and the API, with the answer "revoked" (401) that makes the machine wipe its token and key (LinkStore::handle_answer, unit-tested; an ordinary 401 or a 500 wipes nothing); an unknown token is a plain 401; a password change revokes the account\'s machine tokens, unless the request says keepMachines (they stay, the sessions still go); "sign out everywhere" revokes them; re-link after a wiped store: the same machine id is offered "replace old record" (only the account\'s own records, else 400), replacing keeps the name and revokes the old token, the new link starts at the default scope (the new folder, unpaused), and without replacing both records exist',
        bits326.every(Boolean),
        `${bits326.map((b, i) => (b ? '' : 'FAILED #' + i + ' ')).join('')}unlink U ${delU.status} machine ${delViaMachine.status} X ${delX.status} · held poll ${held.r.status} ${held.r.data.error} after ${held.ms} ms · after: poll ${afterPoll.status} reply ${afterReply.status} api ${afterApi.status} list ${afterList.length} · password: before ${before} after ${afterPw.map((x) => x.status)} list ${afterPwList.length} · keepMachines: machine ${kept.status} session ${keptSession.status} list ${keptList.length} · signout ${afterSignout.status} · offer ${JSON.stringify(peek6.data.replaceable)} · stranger ${stranger.status} · replaced ${(p6.data || {}).replaced} list ${list6.length} (${list6.map((x) => x.name + ' ' + x.scope.slice(0, 24)).join(' | ')}) · old ${oldNow.status} new ${newNow.status} · twin: list ${list7.length}`);
    } finally {
      if (created.password) await call('/api/admin/delete', A, { username: 't-r325' });
    }
  } else { skip('R325', 'linked machines: link by device code', 'the Rust hub\'s: --rust-hub runs it'); skip('R326', 'linked machines: revoke and re-link', 'the Rust hub\'s: --rust-hub runs it'); }
  // R327 and R338 (T-310, linked machines 4/8): who may reach a linked machine, and compatibility. The hub half runs here (the Rust hub's);
  // the unit tests, the check calls and the JavaScript hub's half are in the cargo block further down (r327hub / r338hub carry the results).
  let r327hub = null, r338hub = null;
  if (RUST_HUB) {
    const mc = async (method, p, tok, body) => { try { const r = await fetch(H + p, { method, headers: { 'Content-Type': 'application/json', ...(tok ? { Authorization: 'Bearer ' + tok } : {}) }, body: body !== undefined ? JSON.stringify(body) : undefined }); return { status: r.status, data: await r.json().catch(() => ({})) }; } catch (e) { return { status: 0, data: {} }; } };
    const hostHdr = (id, tok) => ({ 'Content-Type': 'application/json', 'X-VR-Host': id, 'X-VR-Host-Token': tok });
    const mkHost = async (token) => { const made = await call('/api/hosts/new', token, {}); const id = (made.data.host || {}).id || ''; const cj = (await call('/api/hosts/config', token, { id })).data.config || {}; return { id, token: cj.token, nick: (made.data.host || {}).nickname || '' }; };
    const enc = (spec) => 'MACHTOOL=' + Buffer.from(JSON.stringify(spec)).toString('hex');
    const mach = (r) => { const l = texts(r.events).split('\n').find((x) => x.startsWith('MACH:')); try { return JSON.parse(l.slice(5)); } catch { return []; } };
    const listPc = (extra) => ({ route: 'pc-tool', body: { name: 'list', args: { path: '' }, ...(extra || {}) } });
    const o = {}; const d = [];
    const created = (await call('/api/admin/create', A, { username: 't-r327', display: 'R327' })).data || {};
    const X = created.password ? (await login('t-r327', created.password, 'device-r327-xxxxxxxxxx')).data.token : '';
    const watchStop = { v: false }; let watcher = Promise.resolve(); let hostStub = null;
    try {
      // ---- 1. a machine token reaches only /api/machine/poll|reply|unlink (403 everywhere else); an ordinary device token is unchanged
      const kp = crypto.generateKeyPairSync('ed25519'); const pub = kp.publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('hex');
      const ms = (await call('/api/device/start', null, { client: 'vr-machine', device: 'r327-box', publicKey: pub, machineId: 'mach-r327-aaaa', os: 'linux', app: 'vr-host', appVersion: '1.2.3', mode: 'persistent', folder: '/srv/r327/work' })).data || {};
      await call('/api/device/approve', X, { userCode: ms.userCode });
      const mp = (await call('/api/device/poll', null, { deviceCode: ms.deviceCode })).data || {}; const tokM = mp.token, idM = mp.deviceId;
      const ds = (await call('/api/device/start', null, { client: 'jcode', device: 'r327-laptop' })).data || {};
      await call('/api/device/approve', X, { userCode: ds.userCode });
      const tokD = ((await call('/api/device/poll', null, { deviceCode: ds.deviceCode })).data || {}).token;
      const gets = ['/api/check', '/api/machines', '/api/devices', '/api/work/current', '/api/board', '/api/notes/list', '/v1/models', '/api/machine/other', '/api/usage'];
      const posts = [['/api/chat', { message: 'hi' }], ['/api/work/share', { conv: 'room' }], ['/api/devices/revoke', { id: idM }], ['/api/hosts/new', {}], ['/api/device/approve', { userCode: 'AAAA-BBBB' }], ['/api/logout', {}], ['/api/pc-result', { id: 'x', ok: true }], ['/api/admin/create', { username: 't-r327-x' }]];
      const viaM = []; for (const p of gets) viaM.push((await mc('GET', p, tokM)).status); for (const [p, b] of posts) viaM.push((await mc('POST', p, tokM, b)).status);
      const mPoll = await mc('POST', '/api/machine/poll', tokM, { wait: 0 });
      const dChk = await mc('GET', '/api/check', tokD), dList = await mc('GET', '/api/devices', tokD), dBoard = await mc('GET', '/api/board', tokD);
      const stillLinked = ((await mc('GET', '/api/machines', X)).data.machines || []).some((m) => m.id === idM);
      o.scope = viaM.every((s) => s === 403) && mPoll.status === 200 && mPoll.data.owner === 't-r327' && dChk.status === 200 && dChk.data.username === 't-r327' && dList.status === 200 && dBoard.status === 200 && stillLinked;
      d.push(`scope: machine token ${viaM.join('/')} poll ${mPoll.status}; device token check ${dChk.status} devices ${dList.status} board ${dBoard.status}`);

      // ---- 2. a watcher sees no machine / pc event and cannot answer one. (No host of U's is online yet: the call goes to the page.)
      await call('/api/work/share', U, { conv: 'room' });
      const wlog = []; let wcur = 0, wsince = '';
      watcher = (async () => { while (!watchStop.v) {
        const w = await call('/api/work/watch?user=t-user&after=' + wcur + '&since=' + encodeURIComponent(wsince), A); const dd = w.data || {};
        for (const e of dd.events || []) wlog.push(e); if (typeof dd.after === 'number') wcur = dd.after; if (dd.since) wsince = dd.since; if (dd.stopped || w.status !== 200) break;
      } })();
      let ownerSaw = '', ownerRun = '', wrongAnswer = null, waitedOnOwner = false, finished = false;
      await (async () => {
        const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + U }, body: JSON.stringify({ message: 'PCTOOL r327 watcher', fresh: true }) });
        const reader = r.body.getReader(), dec = new TextDecoder(); let buf = '';
        for (;;) {
          const { value, done } = await reader.read(); if (done) break; buf += dec.decode(value, { stream: true }); let i;
          while ((i = buf.indexOf('\n')) >= 0) {
            const line = buf.slice(0, i); buf = buf.slice(i + 1); if (!line.trim()) continue; let ev = {}; try { ev = JSON.parse(line); } catch { /* */ }
            if (ev.type === 'bridge_run') ownerRun = ev.id;
            if (ev.type === 'pc_tool') (async () => { wrongAnswer = await call('/api/pc-result', A, { id: ev.id, ok: true, result: 'watcher-answer' }); await sleep(600); waitedOnOwner = !finished; await call('/api/pc-result', U, { id: ev.id, ok: true, result: 'owner-answer' }); })();
            if (ev.type === 'assistant') ownerSaw += JSON.stringify(ev.message);
          }
        }
        finished = true;
      })();
      await sleep(300);
      const attachA = await call('/api/work/attach?run=' + encodeURIComponent(ownerRun) + '&after=0', A);
      await call('/api/work/share', U, { conv: '' }); watchStop.v = true; await Promise.race([watcher, sleep(5000)]);
      const wjson = JSON.stringify(wlog);
      o.watch = /owner-answer/.test(ownerSaw) && !/watcher-answer/.test(ownerSaw) && waitedOnOwner && wrongAnswer && wrongAnswer.status === 200 && wlog.length > 0 && wlog.every((e) => ['user', 'claude', 'tool', 'sys'].includes(e.t) && !('type' in e)) && !/pc_tool|machine_/.test(wjson) && !!ownerRun && attachA.status === 200 && !(attachA.data.events || []).length;
      d.push(`watch: ${wlog.length} log lines (${[...new Set(wlog.map((e) => e.t))].join('+')}) pc_tool in log ${/pc_tool|machine_/.test(wjson)}; the watcher's answer was ${waitedOnOwner ? 'ignored (the run kept waiting for the owner)' : 'NOT ignored'}; run saw ${/owner-answer/.test(ownerSaw) ? 'the owner\'s answer' : ownerSaw.slice(0, 120)}; attach by the watcher ${(attachA.data.events || []).length} events`);

      // ---- 3. the owner's own (older) AI host: pc engine, no `machine` cap, not even `signed-pc`. A planner session's run never reaches it; a run
      //         started on U's behalf by someone else never reaches it (nor the browser folder); U's own run does.
      const hostU = await mkHost(U); const got = [];
      const ac = new AbortController(); hostStub = ac;
      const hi = await (await fetch(H + '/node/hello', { method: 'POST', headers: hostHdr(hostU.id, hostU.token), body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex'), user: '', providers: [], engines: [], pc: true, version: '1.0.0', os: 'linux', caps: ['relay'] }) })).json().catch(() => ({}));
      (async () => { while (!ac.signal.aborted) {
        const pr = await fetch(H + '/node/poll', { headers: hostHdr(hostU.id, hostU.token), signal: ac.signal }).then((r) => r.json()).catch(() => ({}));
        for (const j of pr.jobs || []) { got.push(j); await fetch(H + '/node/reply', { method: 'POST', headers: hostHdr(hostU.id, hostU.token), body: JSON.stringify({ id: j.id, status: 200, headers: { 'content-type': 'application/json' }, body: Buffer.from(JSON.stringify({ ok: true, result: 'host-listing ' + hostU.id })).toString('base64') }) }).catch(() => {}); }
      } })();
      for (let i = 0; i < 40; i++) { const hs = (await call('/api/check', U)).data.aiNodes || []; if (hs.some((x) => x.id === hostU.id && x.online)) break; await sleep(250); }
      const planSid = (await call('/api/work/sessions/new', U, { title: 'r327 planner', background: true, mode: 'planner' })).data.id;
      const n0 = got.length;
      const pr1 = await chat(U, 'PLANNERPC PLANNERMACHINE=' + hostU.id + ' please', { session: planSid }), pr2 = await chat(U, 'PLANNERPC please', { session: planSid });
      const planMsg = (r) => { try { return JSON.parse((texts(r.events).match(/PLANNER:(\{.*\})/) || [])[1]).pc || ''; } catch { return ''; } };
      const n1 = got.length;
      const behalf = mach(await chat(U, enc([listPc(), listPc({ machine: hostU.id }), listPc({ machine: hostU.nick }), { route: 'board', body: { name: 'board_list', args: {} } }]), { startedBy: 't-admin', fresh: true }));
      const n2 = got.length;
      const own = mach(await chat(U, enc([listPc(), listPc({ machine: hostU.id })]), { fresh: true }));
      const n3 = got.length;
      const isListing = (r) => r && r.status === 200 && r.body && r.body.ok === true && r.body.result === 'host-listing ' + hostU.id;
      o.planner = /Planner mode: PC tools are not available/.test(planMsg(pr1)) && /Planner mode: PC tools are not available/.test(planMsg(pr2)) && n1 === n0;
      o.behalf = behalf.length === 4 && [0, 1, 2].every((i) => behalf[i].status === 403 && behalf[i].body.ok === false && /on someone else's behalf/.test(JSON.stringify(behalf[i].body))) && behalf[3].status === 200 && n2 === n1;
      o.own = own.length === 2 && isListing(own[0]) && isListing(own[1]) && n3 === n2 + 2;
      d.push(`planner: "${planMsg(pr1).slice(0, 50)}" / "${planMsg(pr2).slice(0, 50)}" jobs to the host ${n1 - n0}; on behalf: ${behalf.map((r) => r.status).join('/')} jobs ${n2 - n1}; own run: ${own.map((r) => r.status).join('/')} jobs ${n3 - n2}`);
      // the same on behalf with no host: the page's folder is not reached either (no pc_tool event leaves, the answer is at once)
      ac.abort(); await call('/api/hosts/remove', U, { id: hostU.id });
      const behalfB = mach(await chat(U, enc([listPc()]), { startedBy: 't-admin', fresh: true }));
      const ownB = await chat(U, 'PCTOOL r327 own page', { fresh: true });
      o.behalfBrowser = behalfB.length === 1 && behalfB[0].status === 403 && /fake-listing/.test(texts(ownB.events));
      d.push(`browser: on behalf ${behalfB.map((r) => r.status).join('/')}, own run ${/fake-listing/.test(texts(ownB.events))}`);

      // ---- compatibility (R338): the hub's hello answer announces `machine` (an older host never mentions it and still gets its pc jobs, above),
      //      a scope document with a higher v at the poll boundary is refused whole, a readable one gives the words
      o.hello = !!hi.hubKey && hi.machine && hi.machine.v === 1;
      const listM = async () => ((await mc('GET', '/api/machines', X)).data.machines || []).find((m) => m.id === idM) || {};
      const before = await listM();
      const v2 = await mc('POST', '/api/machine/poll', tokM, { wait: 0, scopeDoc: { v: 2, preset: 'folder', paths: [{ path: '/srv/r327/newer', access: 'rw' }], futureThing: true } });
      const v2text = await mc('POST', '/api/machine/poll', tokM, { wait: 0, scopeDoc: JSON.stringify({ v: 9 }), scope: 'folders: /srv/typed-by-the-machine (read-write)' });
      const junk = await mc('POST', '/api/machine/poll', tokM, { wait: 0, scopeDoc: 'not json' });
      const afterV2 = await listM();
      const v1 = await mc('POST', '/api/machine/poll', tokM, { wait: 0, scopeDoc: { v: 1, preset: 'folder', paths: [{ path: '/srv/r327/other', access: 'rw' }], futureKey: [1] }, scope: 'folders: /srv/typed-by-the-machine (read-write)' });
      const afterV1 = await listM();
      o.scopeDoc = v2.status === 200 && /needs a newer hub/.test(v2.data.scopeRefused || '') && /needs a newer hub/.test(v2text.data.scopeRefused || '') && !!junk.data.scopeRefused && afterV2.scope === before.scope && afterV2.scopeIsDefault === true
        && v1.status === 200 && v1.data.scopeRefused === null && /^one folder: \/srv\/r327\/other/.test(afterV1.scope || '') && !/typed-by-the-machine/.test(afterV1.scope || '') && afterV1.scopeIsDefault === false;
      d.push(`scope doc: v2 ${v2.status} "${(v2.data.scopeRefused || '').slice(0, 60)}"; kept "${(afterV2.scope || '').slice(0, 40)}"; v1 → "${(afterV1.scope || '').slice(0, 40)}"; hello machine ${JSON.stringify(hi.machine)}`);
      // the source: no presence / idle check on the machine path (the owner's run just goes on while they are away, Q7) and the sentence is in the prompt
      const workSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'work.rs'), 'utf8'), hostsSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'ai_hosts.rs'), 'utf8');
      const toolBody = workSrc.slice(workSrc.indexOf('// pc-tool: the person'), workSrc.indexOf('/// Everything a person may pick')).replace(/\/\/.*$/gm, ''); // the pc path, code only
      const mfBody = hostsSrc.slice(hostsSrc.indexOf('pub fn machine_for('), hostsSrc.indexOf('pub async fn pc_tool(')).replace(/\/\/.*$/gm, '');
      o.away = !/presence|idle|is_present|\.away|unattended/i.test(toolBody + mfBody) && /what you did, why you chose it over the other options, and what \{name\} can change/.test(workSrc) && !/unattended/i.test(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-hub', 'src', 'modules', 'machines.rs'), 'utf8').replace(/\/\/.*$/gm, ''));
      d.push(`away: no presence check on the machine path ${!/presence|idle|is_present|\.away|unattended/i.test(toolBody + mfBody)}`);
    } finally {
      watchStop.v = true; if (hostStub) hostStub.abort();
      await call('/api/work/share', U, { conv: '' });
      if (created.password) await call('/api/admin/delete', A, { username: 't-r327' });
    }
    r327hub = { ok: ['scope', 'watch', 'planner', 'behalf', 'own', 'behalfBrowser', 'away'].every((k) => o[k] === true), o, detail: d.join(' · ') };
    r338hub = { ok: ['own', 'hello', 'scopeDoc'].every((k) => o[k] === true), o, detail: d.join(' · ') };
  }
  // R316 to R320 (T-303): RE containers, the manager of throw-away copies of the base image. A tiny local image (reTestSetup),
  // the real Podman; every container this block starts is removed again, whatever happens
  if (RE_T.ok) {
    const prefix = RE_T.prefix, tok = { u: U, d: D, a: A };
    const ctrs = () => (podman(['ps', '-a', '--filter', 'name=' + prefix, '--format', '{{.Names}}'], RE_T.env).stdout || '').split('\n').filter(Boolean).sort();
    const re = (p, t, body) => call('/api/re/' + p, t, body);
    const reList = async (t) => ((await re('list', t)).data || {});
    const reSt = async (t, id) => ((await re('status?id=' + id, t)).data || {}).sandbox || {};
    const exch = (id) => path.join(SCRATCH, 'Re', id);
    const started = []; // every copy this block made, for the last clean-up
    try {
      // R316: at most three run, the rest wait in order, a freed slot starts the next in line
      const mk = async (t, purpose) => { const r = await re('start', t, { purpose }); const s = (r.data || {}).sandbox || {}; if (s.id) started.push(s.id); return { status: r.status, s, error: (r.data || {}).error }; };
      const s1 = await mk(U, 'r316 one'), s2 = await mk(U, 'r316 two'), s3 = await mk(D, 'r316 three');
      const q1 = await mk(A, 'r316 four'), q2 = await mk(U, 'r316 five');
      const l316 = await reList(U);
      const names316 = ctrs();
      const stop1 = await re('stop', U, { id: s1.s.id });
      let q1after = {}; for (let i = 0; i < 40; i++) { q1after = await reSt(A, q1.s.id); if (q1after.state === 'running') break; await sleep(250); }
      const q2after = await reSt(U, q2.s.id);
      const q2stop = await re('stop', U, { id: q2.s.id }); // leaving the queue: it never starts
      const names316b = ctrs();
      check('R316', 'T-303 RE containers: at most 3 copies run at once (a fourth and fifth wait: state queued, place 1 and 2, no container made for them); stopping a running one starts the next in line (FIFO), the one behind moves up, and a queued copy can leave the queue; anyone on the team may start one (no approval)',
        [s1, s2, s3].every((x) => x.status === 200 && x.s.state === 'running') && q1.s.state === 'queued' && q1.s.queuePosition === 1 && q2.s.state === 'queued' && q2.s.queuePosition === 2
        && l316.running === 3 && l316.queued === 2 && l316.max === 3 && (l316.sandboxes || []).length === 5 && names316.length === 3 && names316.every((n) => n.startsWith(prefix))
        && stop1.status === 200 && stop1.data.sandbox.state === 'gone' && q1after.state === 'running' && q2after.state === 'queued' && q2after.queuePosition === 1
        && q2stop.status === 200 && names316b.length === 3 && !names316b.includes(prefix + s1.s.id),
        `started ${[s1, s2, s3].map((x) => x.s.state)} queued ${q1.s.queuePosition}/${q2.s.queuePosition} (${q1.error || ''}${q2.error || ''}) list ${l316.running}+${l316.queued}/${l316.max} containers ${names316.length} · after stop q1 ${q1after.state} q2 ${q2after.state}/${q2after.queuePosition} containers ${names316b.length}`);

      // R317: data in, a command in the copy, data out (text, binary and the raw download); no network, no host mount, a non-root user, one capability
      const id2 = s2.s.id;
      const bin = Buffer.from([0, 1, 2, 250, 255, 77, 90, 144, 0, 3]);
      const p1 = await re('put', U, { id: id2, path: 'sample/hello.txt', text: 'hello re' });
      const p2 = await re('put', U, { id: id2, path: 'bin.dat', base64: bin.toString('base64') });
      const x1 = await re('exec', U, { id: id2, cmd: 'cat /work/in/sample/hello.txt | tr a-z A-Z > /work/out/up.txt; cp /work/in/bin.dat /work/out/bin.dat; id -u; ls /sys/class/net; ls /work; grep CapEff /proc/self/status; echo err >&2' });
      const x2 = await re('exec', U, { id: id2, cmd: ['sh', '-c', 'echo $0 $1', 'a', 'b'] });
      const x3 = await re('exec', U, { id: id2, cmd: 'sleep 30', timeoutSec: 1 });
      const g1 = await re('get', U, { id: id2, path: 'up.txt' });
      const g2 = await re('get', U, { id: id2, path: 'out/bin.dat' });
      const raw = await fetch(H + '/api/re/get?id=' + id2 + '&path=bin.dat', { headers: { Authorization: 'Bearer ' + U } });
      const rawBuf = Buffer.from(await raw.arrayBuffer());
      const esc = [await re('put', U, { id: id2, path: '../escape.txt', text: 'x' }), await re('put', U, { id: id2, path: '/etc/cron.d/x', text: 'x' }), await re('get', U, { id: id2, path: '../../etc/passwd' }), await re('get', U, { id: id2, path: '/etc/passwd' }), await re('put', U, { id: id2, path: 'in/../../x', text: 'x' })];
      const st2 = (await re('status?id=' + id2, U)).data || {};
      const out1 = (x1.data.stdout || '').split('\n');
      check('R317', 'T-303 RE containers: put / exec / get round trip: a text and a binary file go in (/work/in), a command in the copy makes results in /work/out (it runs as a non-root user, no network but lo, nothing of the host mounted, only the ptrace capability), they come out unchanged (JSON and raw download), stderr is kept apart, a runaway command is stopped at its timeout, and a path that leaves the exchange folder (.., absolute) is refused with 400',
        p1.status === 200 && p1.data.path === '/work/in/sample/hello.txt' && p2.status === 200 && p2.data.size === bin.length
        && x1.status === 200 && x1.data.code === 0 && out1[0] === '1000' && out1[1] === 'lo' && out1[2] === 'in' && out1[3] === 'out' && /CapEff:\t0000000000080000/.test(x1.data.stdout) && x1.data.stderr.trim() === 'err'
        && x2.data.code === 0 && x2.data.stdout.trim() === 'a b' && x3.data.timedOut === true
        && g1.status === 200 && g1.data.text === 'HELLO RE' && g2.status === 200 && g2.data.base64 === bin.toString('base64') && raw.status === 200 && rawBuf.equals(bin)
        && esc.every((e) => e.status === 400) && !fs.existsSync(path.join(SCRATCH, 'Re', 'escape.txt')) && (st2.files || []).some((f) => f.area === 'out' && f.path === 'up.txt'),
        `put ${p1.status}/${p2.status} exec ${x1.status} ${JSON.stringify(x1.data).slice(0, 220)} · argv ${JSON.stringify(x2.data.stdout)} · timeout ${x3.data.timedOut} · get ${g1.data.text}/${g2.status} raw ${raw.status} ${rawBuf.length} · escapes ${esc.map((e) => e.status)}`);

      // R318: stop removes the container and its exchange folder entirely
      const dir2 = exch(id2), hadDir = fs.existsSync(path.join(dir2, 'in', 'bin.dat')) && fs.existsSync(path.join(dir2, 'out', 'up.txt'));
      const stop2 = await re('stop', U, { id: id2 });
      const after2 = [await re('exec', U, { id: id2, cmd: 'true' }), await re('get', U, { id: id2, path: 'up.txt' })];
      const gone = (await reList(U)).sandboxes || [];
      check('R318', 'T-303 RE containers: stopping a copy deletes it entirely: its container is gone from the engine, its exchange folder (what went in and what came out, even files the copy\'s own user made) is deleted, it is off the list, and using it afterwards is refused (409/404); nothing survives',
        hadDir && stop2.status === 200 && stop2.data.deleted === true && !ctrs().includes(prefix + id2) && !fs.existsSync(dir2) && !gone.some((s) => s.id === id2) && after2.every((r) => r.status === 404 || r.status === 409),
        `had ${hadDir} stop ${stop2.status} container ${ctrs().includes(prefix + id2)} folder ${fs.existsSync(dir2)} after ${after2.map((r) => r.status)}`);

      // R319: someone else's copy: listed, readable (status), but never stopped / used by another person (403)
      const mine = await mk(U, 'r319 mine');
      await re('put', U, { id: mine.s.id, path: 'keep.txt', text: 'keep me' });
      const other = [await re('stop', D, { id: mine.s.id }), await re('exec', D, { id: mine.s.id, cmd: 'id' }), await re('put', D, { id: mine.s.id, path: 'x', text: 'x' }), await re('get', D, { id: mine.s.id, path: 'keep.txt' })];
      const dList = await reList(D), dSee = (dList.sandboxes || []).find((s) => s.id === mine.s.id), dStatus = await re('status?id=' + mine.s.id, D);
      const stillMine = await re('exec', U, { id: mine.s.id, cmd: 'echo still here' });
      // the team tools: an agent is its person (same owner rules); an unknown tool is refused
      check('R319', 'T-303 RE containers: another person cannot stop, exec, put to or get from someone\'s copy (403 each, nothing changed) while the list and status show it to everyone (read-only); the owner still can; the list says who owns which',
        mine.status === 200 && other.every((r) => r.status === 403) && !!dSee && dSee.owner === 't-user' && dSee.purpose === 'r319 mine' && dStatus.status === 200 && dStatus.data.sandbox.state === 'running' && stillMine.status === 200 && stillMine.data.stdout.trim() === 'still here',
        `other ${other.map((r) => r.status)} listed ${!!dSee} status ${dStatus.status} owner exec ${stillMine.status}`);

      // R320: a hub restart adopts the running copy back (owner, purpose, files kept), never kills it; an admin may stop any copy
      const pid0 = (await call('/api/status')).data.pid, before = ctrs();
      fs.writeFileSync(path.join(SB, '.restart'), 'test');
      let pid1 = pid0; for (let i = 0; i < 120 && (pid1 === pid0 || !pid1); i++) { await sleep(250); pid1 = (await call('/api/status')).data.pid; }
      let l320 = {}, mine2 = null; for (let i = 0; i < 40 && !mine2; i++) { l320 = await reList(U); mine2 = (l320.sandboxes || []).find((s) => s.id === mine.s.id); if (!mine2) await sleep(500); }
      const got = await re('get', U, { id: mine.s.id, path: 'in/keep.txt' }), dOther = await re('exec', D, { id: mine.s.id, cmd: 'id' });
      const stillRuns = await re('exec', U, { id: mine.s.id, cmd: 'echo after restart' });
      const during = ctrs();
      const adminStop = await re('stop', A, { id: mine.s.id });
      check('R320', 'T-303 RE containers: after a hub restart the running copy is adopted back (same id, owner, purpose; its files and its container untouched; listed as adopted), the owner rules still hold (403 for others), and an admin may stop any copy',
        !!pid1 && pid1 !== pid0 && !!mine2 && mine2.adopted === true && mine2.state === 'running' && mine2.owner === 't-user' && mine2.purpose === 'r319 mine' && before.includes(prefix + mine.s.id)
        && got.status === 200 && got.data.text === 'keep me' && dOther.status === 403 && stillRuns.status === 200 && stillRuns.data.stdout.trim() === 'after restart' && adminStop.status === 200 && !ctrs().includes(prefix + mine.s.id) && !fs.existsSync(exch(mine.s.id)),
        `pid ${pid0} -> ${pid1} adopted ${mine2 && mine2.adopted} ${mine2 && mine2.state} get ${got.status} other ${dOther.status} exec ${stillRuns.status} admin stop ${adminStop.status}`);
    } finally {
      // whatever happened: every copy is stopped (their owners', through the hub), then any container left is removed with the engine
      for (const id of started) for (const t of [A]) await re('stop', t, { id });
      const left = ctrs(); if (left.length) podman(['rm', '-f', '-t', '0', ...left], RE_T.env);
    }
  } else for (const id of ['R316', 'R317', 'R318', 'R319', 'R320']) skip(id, 'RE containers (T-303: cap of 3 + queue, put / exec / get, stop deletes everything, owner rules, restart adoption)', RE_T.why);
  // R341 to R345 (T-254): REA's MCP tools inside a container, through three proxy tools (re_skill, re_tools, re_call). The test image
  // carries a fake MCP stdio server (native/crates/pgm-sandbox-internal/tests/fake-mcp.sh, plain sh) and a two-file skill folder,
  // and the hub's re.mcpCommand / re.skillDir point at them: the real 2.25 GB image is not needed
  if (RE_T.ok) {
    const prefix = RE_T.prefix;
    const ctrs = () => (podman(['ps', '-a', '--filter', 'name=' + prefix, '--format', '{{.Names}}'], RE_T.env).stdout || '').split('\n').filter(Boolean).sort();
    const re = (p, t, body) => call('/api/re/' + p, t, body);
    const tools = async (token, calls) => { const r = await chat(token, 'NOTIFYTOOL ' + JSON.stringify(calls) + ' END', { fresh: true }); try { return JSON.parse((texts(r.events).match(/NOTIFY:(\[.*\])/) || [])[1]); } catch { return []; } };
    const ok = (x) => (x && x.ok && x.result) || {};
    const fakeProcs = (id) => { const x = spawnSync('podman', [...(RE_T.env.pre || []), 'exec', prefix + id, 'sh', '-c', 'grep -la "[m]cp.sh" /proc/[0-9]*/cmdline 2>/dev/null | wc -l'], { encoding: 'utf8', env: { ...process.env, ...(RE_T.env.env || {}) } }); return parseInt((x.stdout || '').trim(), 10); };
    const hostClients = (id) => (spawnSync('ps', ['-eo', 'args'], { encoding: 'utf8' }).stdout || '').split('\n').filter((l) => l.includes('exec -i -w /work ' + prefix + id)).length;
    const started = [];
    let id = '';
    try {
      const st = await re('start', U, { purpose: 'r341 rea tools' });
      id = ((st.data || {}).sandbox || {}).id || ''; if (id) started.push(id);
      const hint = (st.data || {}).hint || '';
      const before = id ? fakeProcs(id) : -1; // nothing of REA runs until the first use
      const [t0, t1, t2, t3, t4] = await tools(U, [['re_tools', { id }], ['re_tools', { id, filter: 'FAIL' }], ['re_tools', { id, tool: 'echo' }], ['re_tools', { id, tool: 'nonesuch' }], ['re_tools', { id: 'zzzzzzzz' }]]);
      const after = fakeProcs(id);
      check('R341', 'T-254 re_tools lists REA\'s tools from inside the container: re_start\'s answer says to call re_skill and re_tools next, nothing of REA runs in the copy before the first use, the first re_tools starts its MCP server in the copy (one process) and answers the fake\'s three tool names (names only, with the server\'s name), filter gives the matching ones with the first line of their description, tool gives one tool\'s full schema, an unknown tool or copy is an error',
        st.status === 200 && /re_skill/.test(hint) && /re_tools/.test(hint) && /re_call/.test(hint) && before === 0 && after === 1
        && ok(t0).count === 3 && ok(t0).of === 3 && ok(t0).server && ok(t0).server.name === 'fake-rea' && ok(t0).names.join() === 'echo,big,nope' && !ok(t0).tools
        && ok(t1).count === 1 && ok(t1).tools[0].name === 'nope' && ok(t1).tools[0].description === 'Always fails.' && ok(t2).tool && ok(t2).tool.inputSchema.type === 'object' && t3 && t3.ok === false && /nonesuch/.test(t3.error) && t4 && t4.ok === false,
        `hint ${/re_skill/.test(hint)}/${/re_tools/.test(hint)} before ${before} after ${after} · list ${JSON.stringify(ok(t0)).slice(0, 200)} · filter ${ok(t1).count} · tool ${!!ok(t2).tool} · bad ${t3 && t3.error} / ${t4 && t4.error}`);

      const args = { path: '/work/in/a "quoted".exe', n: [1, 2, 3], deep: { k: 'v', u: 'héllo' } };
      const [c1, c2, c3, c4] = await tools(U, [['re_call', { id, tool: 'echo', args }], ['re_call', { id, tool: 'echo' }], ['re_call', { id, tool: 'nope', args: {} }], ['re_call', { id, tool: 'nonesuch', args: {} }]]);
      const bigR = await re('call', U, { id, tool: 'big', args: {} });
      const head = (((bigR.data || {}).content || [])[0] || {}).text || '';
      const full = (bigR.data || {}).savedTo ? await re('get', U, { id, path: bigR.data.savedTo.replace('/work/', '') }) : {};
      const txt1 = (((ok(c1).content || [])[0]) || {}).text || '', txt2 = (((ok(c2).content || [])[0]) || {}).text || '';
      const pidOf = (t) => (t.match(/pid=(\d+)/) || [])[1];
      let echoed = null; try { echoed = JSON.parse(txt1.slice(txt1.indexOf('args=') + 5)); } catch {}
      check('R342', 'T-254 re_call round-trips: the arguments (quotes, nesting, non-ASCII) reach REA\'s tool exactly and its result comes back unchanged (its untrusted-content marker kept), two calls are served by the same long-lived MCP process (same pid, one process in the copy), an isError result passes through as REA gave it, an unknown tool is an error, and a result over the cap (100000 characters) comes back as a head with truncated, its size and a path in /work/out/mcp-results/ where the whole is saved and can be taken out with re_get',
        c1 && c1.ok === true && /^\[untrusted content\] pid=/.test(txt1) && JSON.stringify(echoed) === JSON.stringify(args) && pidOf(txt1) && pidOf(txt1) === pidOf(txt2) && fakeProcs(id) === 1
        && c3.ok === true && ok(c3).isError === true && /it failed/.test(JSON.stringify(ok(c3).content)) && c4.ok === false && /Unknown tool/.test(c4.error)
        && bigR.status === 200 && bigR.data.truncated === true && bigR.data.bytes > 100000 && head.startsWith('[untrusted content]') && head.length <= 40000 && /^\/work\/out\/mcp-results\/big-\d+\.json$/.test(bigR.data.savedTo || '') && full.status === 200 && ((full.data || {}).size || 0) === bigR.data.bytes,
        `echo ${JSON.stringify(echoed)} pids ${pidOf(txt1)}/${pidOf(txt2)} procs ${fakeProcs(id)} · nope ${ok(c3).isError} · unknown ${c4 && c4.error} · big ${bigR.status} ${bigR.data && bigR.data.truncated} ${bigR.data && bigR.data.bytes} head ${head.length} saved ${bigR.data && bigR.data.savedTo} ${(full.data || {}).size}`);

      const skillFx = fs.readFileSync(path.join(RE_T.dir, 'fake-rea', 'skill', 'SKILL.md'), 'utf8'), refFx = fs.readFileSync(path.join(RE_T.dir, 'fake-rea', 'skill', 'references', 'environments.md'), 'utf8');
      const [s0, s1, s2, s3] = await tools(U, [['re_skill', { id }], ['re_skill', { id, file: 'references/environments.md' }], ['re_skill', { id, file: '../../etc/passwd' }], ['re_skill', { id, file: 'references/none.md' }]]);
      check('R343', 'T-254 re_skill returns REA\'s skill text from inside the container: SKILL.md by default, with the list of the other files (references/…), or one of them by path; a path that leaves the skill folder or does not exist is an error (nothing of the host is read)',
        s0 && s0.ok === true && ok(s0).file === 'SKILL.md' && ok(s0).text === skillFx && (ok(s0).files || []).join() === 'SKILL.md,references/environments.md'
        && ok(s1).file === 'references/environments.md' && ok(s1).text === refFx && s2 && s2.ok === false && !/root:/.test(JSON.stringify(s2)) && s3 && s3.ok === false,
        `default ${ok(s0).file} ${(ok(s0).text || '').length}b files ${(ok(s0).files || []).join()} · ref ${ok(s1).text === refFx} · escape ${s2 && s2.ok} · missing ${s3 && s3.ok}`);

      // owner only: another person (even an admin) is refused, by the routes (403) and by the tools; the owner still can
      const other = [await re('tools?id=' + id, D), await re('skill?id=' + id, D), await re('call', D, { id, tool: 'echo', args: {} }), await re('tools?id=' + id, A), await re('call', A, { id, tool: 'echo', args: {} })];
      const [o1, o2, o3] = await tools(D, [['re_tools', { id }], ['re_call', { id, tool: 'echo', args: {} }], ['re_skill', { id }]]);
      const mine = await re('call', U, { id, tool: 'echo', args: { again: 1 } });
      check('R344', 'T-254 REA\'s tools are the owner\'s only: another person gets 403 from the routes (tools, skill, call) and an error from the three tools, an admin too, nothing of the copy is revealed to them; the owner still can',
        other.every((r) => r.status === 403) && [o1, o2, o3].every((x) => x && x.ok === false && /owner/.test(x.error) && !/fake-rea|echo/.test(x.error)) && mine.status === 200 && /again/.test(JSON.stringify(mine.data)),
        `routes ${other.map((r) => r.status)} tools ${[o1, o2, o3].map((x) => x && x.ok)} owner ${mine.status}`);

      // stop: the MCP session ends with the copy, nothing is left running (container, host-side client), nothing answers afterwards
      const clientsBefore = hostClients(id), procsBefore = fakeProcs(id);
      const stop = await tools(U, [['re_stop', { id }]]);
      const left = [await re('call', U, { id, tool: 'echo', args: {} }), await re('tools?id=' + id, U), await re('skill?id=' + id, U)];
      await sleep(300);
      const planner = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'work.rs'), 'utf8').match(/\("team", "re_start"[^=]*=>/);
      const names = ['re_skill', 're_tools', 're_call'], jsNames = (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1));
      const rsNames = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.map((t) => t.name);
      const jsRe = new RegExp(fs.readFileSync(path.join(BRIDGE, 'team.js'), 'utf8').match(/\/\^(re_\([^$]*\))\$\/\.test\(name\)/)[1]);
      check('R345', 'T-254 stopping the copy ends its MCP session and leaves nothing running: before the stop one MCP server in the copy and one exec client on the host, after it the container is gone and no client process is left, REA\'s tools and skill answer 404 / 409 afterwards; [static] both team MCP servers list re_skill, re_tools and re_call, the JavaScript hub answers them "not available", and a planner session may not use them',
        clientsBefore === 1 && procsBefore === 1 && stop[0] && stop[0].ok === true && !ctrs().includes(prefix + id) && hostClients(id) === 0 && left.every((r) => r.status === 404 || r.status === 409)
        && names.every((n) => jsNames.includes(n) && rsNames.includes(n) && jsRe.test(n)) && !!planner && names.every((n) => planner[0].includes('"' + n + '"')),
        `before ${clientsBefore}/${procsBefore} stop ${stop[0] && stop[0].ok} container ${ctrs().includes(prefix + id)} clients ${hostClients(id)} after ${left.map((r) => r.status)} · names js ${names.map((n) => jsNames.includes(n))} rs ${names.map((n) => rsNames.includes(n))} planner ${!!planner}`);
    } finally {
      for (const x of started) await re('stop', A, { id: x });
      const left = ctrs(); if (left.length) podman(['rm', '-f', '-t', '0', ...left], RE_T.env);
    }
  } else for (const id of ['R341', 'R342', 'R343', 'R344', 'R345']) skip(id, 'REA tools inside a container (T-254: re_tools, re_call, re_skill, owner only, closes on stop)', RE_T.why);
  // R485 (T-255): a workflow's `rea` node is REA through the same RE containers as the re_* tools (no second way in): the
  // run starts its owner's copy, puts a file in, calls REA's tool, takes the file out, and the copy is gone afterwards
  const L485 = 'a workflow with a rea node runs a REA step as its owner: a copy of the RE image starts, the file goes in, REA\'s tool answers with the run\'s arguments, the file comes back out, and no container is left afterwards; the owner\'s RE list is empty again';
  if (RE_T.ok) {
    const ctrs = () => (podman(['ps', '-a', '--filter', 'name=' + RE_T.prefix, '--format', '{{.Names}}'], RE_T.env).stdout || '').split('\n').filter(Boolean);
    const hw = (p, tok, body) => call('/api/hyperwork/' + p, tok, body);
    const before = ctrs().length;
    const doc485 = { schema: 1, name: 'r485-rea', nodes: [{ id: 's', kind: 'start' },
      { id: 'r', kind: 'rea', config: { tool: 'echo', args: { sample: 'r485 {{prompt}}' }, files: [{ path: 'in/note.txt', text: 'note for {{prompt}}' }], get: ['in/note.txt'], purpose: 'r485 workflow' } },
      { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'r.args' }, { id: 'e2', from: 'r.text', to: 'e.result' }] };
    const sv = await hw('save', U, { doc: doc485 }), wid = (sv.data || {}).id;
    const pub = await hw('publish', U, { id: wid, message: 'r485' });
    const go = await hw('run', U, { id: wid, prompt: 'app.bin' }), run = (go.data || {}).run;
    let m = {}; for (let i = 0; i < 240 && run; i++) { m = (await hw('run?id=' + run, U)).data || {}; if (m.status && m.status !== 'running') break; await sleep(250); }
    const log = run ? (await fetch(`${H}/api/hyperwork/log?id=${run}`, { headers: { Authorization: 'Bearer ' + U } }).then((r) => r.text()).catch(() => '')).split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return {}; } }) : [];
    const end = log.filter((e) => e.type === 'node_end' && e.node === 'r').pop() || {};
    const outFile = (((end.outputs || {}).files || {})['in/note.txt'] || {}).text;
    const mine = ((await call('/api/re/list', U)).data.sandboxes || []).filter((s) => s.owner === 't-user' && s.state !== 'gone');
    const result = String(m.result || '');
    check('R485', L485, sv.status === 200 && pub.status === 200 && go.status === 202 && m.status === 'ok' && /\[untrusted content\]/.test(result) && /"sample": ?"r485 app\.bin"/.test(result)
      && outFile === 'note for app.bin' && ctrs().length === before && mine.length === 0,
      `save ${sv.status} publish ${pub.status} run ${go.status} → ${m.status} ${JSON.stringify(m.error || '')} · result ${result.slice(0, 160)} · file out ${JSON.stringify(outFile)} · containers ${before} → ${ctrs().length} · own copies left ${mine.length}`);
    for (const left of ctrs().slice(before)) podman(['rm', '-f', '-t', '0', left], RE_T.env);
  } else skip('R485', L485, RE_T.why);
  if (RUST_HUB && !WIN) { // R200: a session's worktree follows the worktree law (it builds on the person's work/<user>) and its work comes home
    const wt200 = path.join(SB, 'worktree.js'), own200 = path.join(SCRATCH, 'worktrees', 't-user');
    const run200 = (...args) => spawnSync(process.execPath, [wt200, ...args], { cwd: SB, encoding: 'utf8', timeout: 60000 });
    // the hub's background freshness check (worktree.js auto, started with each run) runs git in these same worktrees, so
    // a commit here can meet its index.lock: wait for that lock only (and say so), never for anything else
    const g200 = (dir, ...args) => { let r; for (let i = 0; i < 20; i++) { r = spawnSync('git', ['-c', 'user.name=r200', '-c', 'user.email=r200@test.invalid', ...args], { cwd: dir, encoding: 'utf8' }); if (r.status === 0 || !/index\.lock/.test(r.stderr || '')) break; spawnSync('sleep', ['0.25']); } return r; };
    const commit200 = (dir, file, text, msg) => { fs.writeFileSync(path.join(dir, file), text); const a = g200(dir, 'add', file), c = g200(dir, 'commit', '-q', '-m', msg); return a.status === 0 && c.status === 0 ? 'ok' : `failed: ${((c.stderr || a.stderr) || '').trim().slice(0, 120)}`; };
    const seen = {}; let ok200 = false;
    const id = (await call('/api/work/sessions/new', U, { title: 'r200', background: true })).data.id;
    if (id) {
      await chat(U, 'r200 first run', { session: id }); // makes the session's worktree
      const sm = (((await call('/api/work/sessions', U)).data.sessions) || []).find((x) => x.id === id) || {};
      const dir = (sm.worktree || {}).path || '', hex = id.slice(2);
      if (dir && fs.existsSync(dir)) {
        seen.c1 = commit200(dir, 'r200-session.txt', 'from the session\n', 'r200 session work');
        const c1 = run200('check', 't-user', '--session', hex); seen.fresh = c1.status; seen.freshText = (c1.stdout || c1.stderr).trim();
        seen.c2 = commit200(own200, 'r200-own.txt', 'from my own worktree\n', 'r200 own work');
        const c2 = run200('check', 't-user', '--session', hex); seen.stale = c2.status; seen.staleText = (c2.stdout || c2.stderr).trim();
        const up = run200('update', 't-user', '--session', hex); seen.update = up.status; seen.hasOwn = fs.existsSync(path.join(dir, 'r200-own.txt'));
        seen.after = run200('check', 't-user', '--session', hex).status;
        const home = await call('/api/work/sessions/home', U, { id }); seen.home = home.status; seen.homeText = home.data.text || home.data.error || '';
        seen.came = fs.existsSync(path.join(own200, 'r200-session.txt'));
        ok200 = seen.fresh === 0 && /1 commit\(s\) not in work\/t-user/.test(seen.freshText) && seen.stale === 3 && /behind work\/t-user/.test(seen.staleText) && seen.update === 0 && seen.hasOwn && seen.after === 0 && seen.home === 200 && seen.came;
      } else seen.freshText = 'no session worktree at ' + dir;
      await call('/api/work/sessions/delete', U, { id });
    }
    check('R200', 'a session\'s own worktree follows the worktree law: worktree.js check --session says FRESH with its own commit not yet in work/<user>, STALE (exit 3) once the person\'s work/<user> moved on, update --session brings that in; the session menu\'s "Merge into my work" (/api/work/sessions/home) merges the session\'s commits into work/<user>',
      ok200, `commits: session ${seen.c1 || '?'}, own ${seen.c2 || '?'} · fresh ${seen.fresh} "${(seen.freshText || '').slice(0, 160)}" · stale ${seen.stale} "${(seen.staleText || '').slice(0, 120)}" · update ${seen.update} brought own ${seen.hasOwn} · after ${seen.after} · home ${seen.home} "${(seen.homeText || '').slice(0, 120)}" came ${seen.came}`);
  } else skip('R200', 'a session\'s worktree follows the worktree law', 'the Rust hub\'s (and not on Windows): --rust-hub runs it');
  if (RUST_HUB) { // R225-R229: git at a glance on the Work page (T-240): the person's own worktree of the open session
    const own225 = path.join(SCRATCH, 'worktrees', 't-user');
    const gitq = (dir, ...args) => spawnSync('git', ['-c', 'user.name=r225', '-c', 'user.email=r225@test.invalid', ...args], { cwd: dir, encoding: 'utf8' });
    const gs = (q = '') => call('/api/work/git' + q, U);
    await call('/api/work/git', U); // makes the person's own worktree if there is none yet
    fs.writeFileSync(path.join(own225, 'r225-new.txt'), 'new file\n'); fs.writeFileSync(path.join(own225, 'r225-other.txt'), 'other file\n');
    const st1 = await gs(); const f1 = ((st1.data || {}).files || []).find((f) => f.path === 'r225-new.txt') || {};
    const sid225 = (await call('/api/work/sessions/new', U, { title: 'r225', background: true })).data.id;
    const noWt = await gs('?session=' + sid225);
    await chat(U, 'r225 first run', { session: sid225 }); // makes the session's worktree
    const ss1 = await gs('?session=' + sid225); const none = await gs('?session=s-000000000000');
    check('R225', 'git at a glance: GET /api/work/git shows the person\'s own worktree of the first session (branch, base, ahead / behind as numbers, changed files with their state: an untracked file is unstaged) and, with ?session=, that session\'s own worktree on work/<user>.<id> against work/<user> (exists false before its first run); an unknown session is 404',
      st1.status === 200 && st1.data.branch === 'work/t-user' && st1.data.kind === 'own' && st1.data.working === false && typeof st1.data.ahead === 'number' && typeof st1.data.behind === 'number' && f1.state === 'untracked' && f1.staged === false && f1.unstaged === true
        && noWt.status === 200 && noWt.data.exists === false && ss1.status === 200 && ss1.data.kind === 'session' && ss1.data.branch === 'work/t-user.' + sid225.slice(2) && ss1.data.base === 'work/t-user' && ss1.data.exists === true && none.status === 404,
      `own ${st1.status} ${st1.data.branch} ahead ${st1.data.ahead} behind ${st1.data.behind} file ${JSON.stringify(f1)} · session before run exists ${noWt.data.exists}, after ${ss1.data.branch} base ${ss1.data.base} · unknown ${none.status}`);
    const stg = await call('/api/work/git/stage', U, { files: ['r225-new.txt'] }); const fs1 = (stg.data.files || []).find((f) => f.path === 'r225-new.txt') || {};
    const uns = await call('/api/work/git/unstage', U, { files: ['r225-new.txt'] }); const fu1 = (uns.data.files || []).find((f) => f.path === 'r225-new.txt') || {};
    const bad = [await call('/api/work/git/stage', U, { files: ['../outside'] }), await call('/api/work/git/stage', U, { files: ['-A'] }), await call('/api/work/git/stage', U, { files: [] }), await call('/api/work/git/stage', U, { files: ['/etc/passwd'] })].map((x) => x.status);
    check('R226', 'stage / unstage: POST /api/work/git/stage puts a file in the index (staged true), unstage takes it out again; a path outside the worktree, an option, an absolute path or no file at all is 400',
      stg.status === 200 && fs1.staged === true && uns.status === 200 && fu1.staged === false && fu1.unstaged === true && bad.every((x) => x === 400), `stage ${stg.status} staged ${fs1.staged} · unstage ${uns.status} staged ${fu1.staged} · bad paths ${bad.join(',')}`);
    const noMsg = await call('/api/work/git/commit', U, { message: '  ' }); const noStaged = await call('/api/work/git/commit', U, { message: 'r225 nothing' });
    await call('/api/work/git/stage', U, { files: ['r225-new.txt'] });
    const cm = await call('/api/work/git/commit', U, { message: 'r225: the panel commits' });
    const log = gitq(own225, 'log', '-1', '--format=%s|%ae|%an').stdout.trim(); const left = ((await gs()).data.files || []).map((f) => f.path);
    check('R227', 'commit: only what is staged is committed, as the person (author email <login>@viberoller.local, from their Forgejo record when they have one); no message or nothing staged is 400 and changes nothing; the unstaged file stays a change',
      noMsg.status === 400 && noStaged.status === 400 && cm.status === 200 && /^Committed on work\/t-user/.test(cm.data.text) && log.startsWith('r225: the panel commits|') && /@viberoller\.local\|/.test(log) && left.includes('r225-other.txt') && !left.includes('r225-new.txt'),
      `no message ${noMsg.status}, nothing staged ${noStaged.status}, commit ${cm.status} "${cm.data.text}" · last ${log} · still changed ${left.join(',')}`);
    const b409 = {}; fs.writeFileSync(path.join(ss1.data.dir || '', 'r228.txt'), 'r228\n'); const slow225 = chat(U, 'SLOW r225 hold', { session: sid225 }); await sleep(700);
    const S = { session: sid225 };
    const wk = await gs('?session=' + sid225); b409.stage = (await call('/api/work/git/stage', U, { ...S, files: ['r228.txt'] })).status; b409.commit = (await call('/api/work/git/commit', U, { ...S, message: 'x' })).status; b409.merge = (await call('/api/work/git/merge', U, S)).status; b409.update = (await call('/api/work/git/update', U, S)).status;
    const other = (await gs()).data.working; await call('/api/stop', U, S); await slow225;
    const after409 = (await call('/api/work/git/stage', U, { ...S, files: ['r228.txt'] })).status;
    check('R228', 'while that session\'s run is working every git action (stage, commit, update, merge) is 409 and the status says working; another session of the same person is not held by it; afterwards it works again',
      wk.data.working === true && Object.values(b409).every((x) => x === 409) && other === false && after409 === 200, `working ${wk.data.working} · ${JSON.stringify(b409)} · other session working ${other} · after ${after409}`);
    // a session's worktree: commit through the panel, update from work/<user>, "merge" = into work/<user> (worktree.js home); the person's own: worktree.js merge
    fs.writeFileSync(path.join(ss1.data.dir || '', 'r229-session.txt'), 'from the session\n');
    await call('/api/work/git/stage', U, { session: sid225, all: true }); const sc = await call('/api/work/git/commit', U, { session: sid225, message: 'r229: session work' });
    await call('/api/work/git/stage', U, { all: true }); await call('/api/work/git/commit', U, { message: 'r229: own work moves on' });
    const sa = await gs('?session=' + sid225);
    const su = await call('/api/work/git/update', U, { session: sid225 }); const sa2 = await gs('?session=' + sid225);
    const sm = await call('/api/work/git/merge', U, { session: sid225 }); const came = fs.existsSync(path.join(own225, 'r229-session.txt'));
    const pm = await call('/api/work/git/merge', U, {});
    const foreign = (await call('/api/work/git?session=' + sid225, A)).status;
    await call('/api/work/sessions/delete', U, { id: sid225 });
    check('R229', 'update and merge go only through worktree.js: a session\'s own worktree updates from work/<user> (behind 1 → 0, its commit stays) and "merge" brings its commit into work/<user> (the file arrives in the person\'s worktree); the person\'s own worktree "merge" answers with the tool\'s own words (Forgejo first, versioning law); another person never reaches this person\'s session (404)',
      sc.status === 200 && sa.data.behind >= 1 && sa.data.ahead === 1 && su.status === 200 && sa2.data.behind === 0 && sa2.data.ahead >= 1 && sm.status === 200 && came && typeof pm.data.text === 'string' && /Merged|Nothing to merge|Not merged|CONFLICT|Forgejo|develop/.test(pm.data.text) && foreign === 404,
      `session commit ${sc.status} · behind/ahead ${sa.data.behind}/${sa.data.ahead} · update ${su.status} → ${sa2.data.behind}/${sa2.data.ahead} · merge ${sm.status} "${(sm.data.text || '').slice(0, 90)}" came ${came} · own merge ${pm.status} "${(pm.data.text || '').slice(0, 140)}" · other person on this session ${foreign}`);
  } else { for (const [id, what] of [['R225', 'git at a glance: status of the own / session worktree'], ['R226', 'git panel: stage / unstage'], ['R227', 'git panel: commit as the person'], ['R228', 'git panel: refused (409) while the session works'], ['R229', 'git panel: update / merge only through worktree.js']]) skip(id, what, 'the Rust hub\'s: --rust-hub runs it'); }
  if (RUST_HUB) { // R201: a PC tool call reaches the page of the session that asked, not another running session of the same person
    const b201 = (await call('/api/work/sessions/new', U, { title: 'r201', background: true })).data.id;
    let txt201 = '', slow201 = null;
    if (b201) {
      slow201 = call('/api/chat', U, { message: 'SLOW r201 first session' }); await sleep(700); // the first session runs meanwhile, with no browser answering its PC calls (the old routing sent them there)
      const pc = await Promise.race([chat(U, 'PCTOOL r201', { session: b201 }), sleep(30000).then(() => ({ events: [] }))]);
      txt201 = texts(pc.events);
      await slow201; await call('/api/work/sessions/delete', U, { id: b201 });
    }
    check('R201', 'a PC tool call of a session\'s run reaches that session\'s page (its MCP relay names the session) while another session of the same person runs',
      /fake-listing: a\.txt/.test(txt201), `answer "${txt201.slice(0, 160)}"`);
  } else skip('R201', 'a PC tool call reaches the asking session', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R202 (T-288): a run that was refused a tool leaves its session waiting on its person (yellow) until they answer; presence counts working sessions
    const c202 = (await call('/api/work/sessions/new', U, { title: 'r202', background: true })).data.id;
    const one202 = async () => ((((await call('/api/work/sessions', U)).data.sessions) || []).find((x) => x.id === c202)) || {};
    const seen = {};
    if (c202) {
      await chat(U, 'DENIED r202', { session: c202 }); const w = await one202(); seen.state = w.state; seen.kind = (w.waiting || {}).kind; seen.tools = ((w.waiting || {}).tools || []).join(',');
      const a = call('/api/chat', U, { message: 'SLOW r202 first' }), b = chat(U, 'SLOW r202 answer', { session: c202 }); await sleep(900);
      seen.count = ((((await call('/api/presence', D)).data.people) || []).find((p) => p.user === 't-user') || {}).workingSessions;
      await Promise.all([a, b]); const after = await one202(); seen.after = after.state; seen.cleared = !after.waiting;
      await call('/api/work/sessions/delete', U, { id: c202 });
    }
    check('R202', 'T-288 a run that stopped short of a tool it was not allowed (permission_denials) leaves its session "waiting" (yellow, which beats red) with what it needs; the next message answers it (no longer waiting); presence shows how many sessions of a person work at once (workingSessions); [static] the Work page says what was refused and sounds it',
      /ev\.permission_denials/.test(fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8')) && seen.state === 'waiting' && seen.kind === 'permission' && seen.tools === 'Bash' && seen.count === 2 && seen.after === 'done-unseen' && seen.cleared,
      `after the refusal ${seen.state} (${seen.kind}: ${seen.tools}) · two running: workingSessions ${seen.count} · after answering ${seen.after}, cleared ${seen.cleared}`);
  } else skip('R202', 'a refused tool leaves the session waiting', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R203: a share shows the session that is shared (not always the first one); sharing another moves it
    const c203 = (await call('/api/work/sessions/new', U, { title: 'r203', background: true })).data.id;
    const seen = {};
    if (c203) {
      const sh = await call('/api/work/share', U, { conv: 'room', transcript: [], session: c203 }); seen.share = sh.data.session;
      await chat(U, 'r203 in the shared session', { session: c203 });
      await chat(U, 'r203 in the first session');
      const log = (await call('/api/work/watch?user=t-user&after=0', D)).data;
      const texts203 = (log.events || []).map((e) => e.text).join(' | ');
      seen.has = /r203 in the shared session/.test(texts203); seen.leak = /r203 in the first session/.test(texts203); seen.session = log.session;
      await call('/api/work/share', U, { conv: '' }); await call('/api/work/sessions/delete', U, { id: c203 });
    }
    check('R203', 'a shared Work session shows the session that was shared (POST /api/work/share {session}): its lines reach the watchers, another session\'s do not; the watch answer names it',
      seen.share === c203 && seen.has && !seen.leak && seen.session === c203, `shared ${seen.share} · watcher sees it ${seen.has}, the first session\'s ${seen.leak} · watch names ${seen.session}`);
  } else skip('R203', 'a share shows the shared session', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB && NATIVE_BIN.host) { // R280-R284 (T-290 = T-282 · 8/8): several people, two native hosts, many sessions: limits, restarts, a share and an older page, in one run
    const until280 = async (fn, ms = 20000) => { const end = Date.now() + ms; let v; while (Date.now() < end) { v = await fn(); if (v) return v; await sleep(100); } return v; };
    const list280 = async (tok) => (await call('/api/work/sessions', tok)).data || {};
    const sum280 = async (tok, id) => ((await list280(tok)).sessions || []).find((x) => x.id === id) || {};
    const used280 = async (tok, h) => ((((await list280(tok)).counts || {}).perHost) || {})[h.id] || {};
    const working280 = (tok, h, n) => until280(async () => ((await used280(tok, h)).used || 0) >= n);
    const hostSt280 = async (tok, h) => (((await call('/api/hosts/status', tok)).data || {}).hosts || []).find((x) => x.id === h.id) || null;
    const online280 = (tok, h, pred = () => true) => until280(async () => { const x = await hostSt280(tok, h); return x && x.online && x.sessionLimits && pred(x) ? x : null; }, 40000);
    const pid280 = async () => (await call('/api/status')).data.pid;
    const kill280 = (p) => { try { if (WIN) execFileSync('taskkill', ['/pid', String(p.pid), '/T', '/F'], { stdio: 'ignore' }); else p.kill('SIGKILL'); } catch {} };
    const ran280 = (r) => !!r && r.status === 200 && r.events.some((e) => e.type === 'result') && /relay=200:upstream ok/.test(texts(r.events)); // it finished, and its model call went through its host
    const sysId280 = (r) => ((r.events.find((e) => e.type === 'system') || {}).session_id) || '';
    const within280 = (p, ms) => Promise.race([p, sleep(ms).then(() => ({ status: 0, events: [], stuck: true }))]);
    const go280 = (tok, id, tag) => chat(tok, 'SLOW r280 ' + tag, { session: id });
    const prov280 = (h, p) => `h:${h.id}/${p}`;
    const mine280 = []; // [token, id] of every session this block made: all deleted at the end
    const mkSession = async (tok, h, p, title) => { const id = (await call('/api/work/sessions/new', tok, { title, background: true, provider: prov280(h, p), model: 'claude-haiku-5-5' })).data.id; if (id) mine280.push([tok, id]); return id; };
    const mkHost = async (tok, who, name) => {
      const made = (await call('/api/hosts/new', tok, {})).data.host || {}, c = (await call('/api/hosts/config', tok, { id: made.id })).data.config || {};
      const dir = path.join(SCRATCH, 'r280-' + name); fs.mkdirSync(dir, { recursive: true });
      const cfgf = path.join(dir, 'ai-node.json'), base = `http://127.0.0.1:${OLLAMA_PORT}`;
      const prov = (id) => ({ id, kind: 'claude-relay', label: `R280 ${name} ${id}`, models: ['claude-haiku-5-5'], allowedUsers: ['t-user', 't-admin', 'mills'], configDir: path.join(SCRATCH, 'host-claude-login'), baseUrl: base + '/anthropic' });
      fs.writeFileSync(cfgf, JSON.stringify({ hub: H, id: made.id, token: c.token, user: c.user || who, autoUpdate: false, engines: {}, providers: [prov('c1'), prov('c2')] }), { mode: 0o600 });
      const out = fs.openSync(path.join(dir, 'out.log'), 'a');
      const h = { id: made.id, tok, dir, exit: null, start() { h.exit = null; h.proc = spawn(NATIVE_BIN.host, ['host', '--config', cfgf], { cwd: dir, stdio: ['ignore', out, out], windowsHide: true }); h.proc.on('exit', () => { h.exit = true; }); } };
      h.start(); return h;
    };
    const S = {}, hosts280 = [];
    let mySt = null;
    try {
      const h1 = await mkHost(A, 't-admin', 'one'), h2 = await mkHost(D, 'mills', 'two'); hosts280.push(h1, h2);
      S.up = !!(await online280(A, h1)) && !!(await online280(D, h2));
      const offered = async () => { for (const tok of [A, U, D]) { const ps = ((await call('/api/check', tok)).data.providers || []).map((p) => p.id); for (const h of [h1, h2]) for (const p of ['c1', 'c2']) if (!ps.includes(prov280(h, p))) return false; } return true; };
      S.offered = !!(await until280(offered, 30000));
      // the first (primary) session of t-user is what an older page means: it works through host one, provider c2
      mySt = (await call('/api/check', U)).data;
      await call('/api/model', U, { provider: prov280(h1, 'c2'), model: 'claude-haiku-5-5' });
      const primary = ((await list280(U)).sessions || []).find((x) => x.primary) || {};

      // ---- 1. everyone at once: t-user 5 on host one (3 on c1 + 2 on c2) and 3 on host two (c1); the owners go beyond the caps on their own hosts
      const uA = [], uB = [], uC = [], aOwn = [], dOwn = [];
      for (let i = 0; i < 3; i++) uA.push(await mkSession(U, h1, 'c1', 'r280 u-one-c1 ' + i));
      for (let i = 0; i < 2; i++) uB.push(await mkSession(U, h1, 'c2', 'r280 u-one-c2 ' + i));
      for (let i = 0; i < 4; i++) uC.push(await mkSession(U, h2, 'c1', 'r280 u-two-c1 ' + i)); // the fourth only to be refused
      const uX = await mkSession(U, h1, 'c2', 'r280 u-one-c2 refused');
      for (let i = 0; i < 4; i++) aOwn.push(await mkSession(A, h1, 'c1', 'r280 a ' + i));
      for (let i = 0; i < 4; i++) dOwn.push(await mkSession(D, h2, 'c1', 'r280 d ' + i));
      const dOther = await mkSession(D, h1, 'c2', 'r280 d on one');
      const wave = [...uA.map((id, i) => go280(U, id, 'ua' + i)), ...uB.map((id, i) => go280(U, id, 'ub' + i)), ...uC.slice(0, 3).map((id, i) => go280(U, id, 'uc' + i)),
        ...aOwn.map((id, i) => go280(A, id, 'a' + i)), ...dOwn.map((id, i) => go280(D, id, 'd' + i)), go280(D, dOther, 'dx')];
      S.reached = !!(await working280(U, h1, 5)) && !!(await working280(U, h2, 3));
      S.ownersReached = !!(await working280(A, h1, 4)) && !!(await working280(D, h2, 4));
      const r4 = await call('/api/chat', U, { message: 'SLOW r280 over the provider cap', session: uC[3] });
      const r6 = await call('/api/chat', U, { message: 'SLOW r280 over the host cap', session: uX });
      const old6 = await call('/api/chat', U, { message: 'SLOW r280 an older page, over the host cap' });
      const perHost = ((await list280(U)).counts || {}).perHost || {};
      const l4 = r4.data.limit || {}, l6 = r6.data.limit || {};
      S.provCap = r4.status === 429 && l4.provider === prov280(h2, 'c1') && l4.used === 3 && l4.max === 3 && l4.host === h2.id && /3 of 3/.test(r4.data.error || '');
      S.hostCap = r6.status === 429 && !l6.provider && l6.used === 5 && l6.max === 5 && l6.host === h1.id && /5 of 5/.test(r6.data.error || '');
      S.oldCap = old6.status === 429 && /5 of 5/.test(old6.data.error || '');
      S.perHost = [(perHost[h1.id] || {}).used, (perHost[h2.id] || {}).used].join('/');
      S.refusedIdle = [(await sum280(U, uC[3])).state, (await sum280(U, uX)).state, (await sum280(U, primary.id)).state !== 'working'].join('/');
      const done = await Promise.all(wave);
      S.waveOk = done.every(ran280); S.waveBad = done.map((r, i) => (ran280(r) ? '' : i + ':' + r.status)).filter(Boolean).join(',');

      // ---- 1b. the owner of a host is limited on it only when that host says so (the limits control)
      const lim = (tok, h, limits) => call('/api/hosts/control', tok, { id: h.id, action: 'limits', limits });
      S.notOwner = (await lim(A, h2, { perProvider: 1 })).status;
      S.off = (await lim(D, h2, { perUser: 5, perProvider: 1, exceptions: {}, ownerBypass: false })).status;
      S.offSeen = !!(await online280(D, h2, (x) => x.sessionLimits.ownerBypass === false && x.sessionLimits.perProvider === 1));
      const b1 = go280(D, dOwn[0], 'b1'); await working280(D, h2, 1);
      const bRefused = await call('/api/chat', D, { message: 'SLOW r280 owner, bypass off', session: dOwn[1] });
      S.ownerLimited = bRefused.status === 429 && (bRefused.data.limit || {}).provider === prov280(h2, 'c1') && (bRefused.data.limit || {}).max === 1;
      S.b1 = ran280(await b1);
      S.on = (await lim(D, h2, { perUser: 5, perProvider: 1, exceptions: {}, ownerBypass: true })).status;
      S.onSeen = !!(await online280(D, h2, (x) => x.sessionLimits.ownerBypass === true && x.sessionLimits.perProvider === 1));
      const b2 = go280(D, dOwn[0], 'b2'); await working280(D, h2, 1);
      const b3 = go280(D, dOwn[1], 'b3'); await working280(D, h2, 2);
      S.ownerFree = ran280(await b2) && ran280(await b3);

      // ---- 2. a hub restart while runs go: it waits for them (no new run meanwhile), then every session is as it was and the hosts are back with their limits
      const U1 = uA[0], U2 = uC[0], D1 = dOwn[2], kept = [[U, U1], [U, uB[0]], [U, U2], [D, D1]];
      const tr = (id) => [{ cls: 'me', text: 'r280 asked ' + id }, { cls: 'claude', text: 'r280 answered ' + id }, { cls: 'tool', text: 'r280 tool ' + id }];
      for (const [tok, id] of kept) { await call('/api/work/current', tok, { session: id, transcript: tr(id) }); await call('/api/work/queue', tok, { session: id, queue: ['r280 queued a ' + id, 'r280 queued b ' + id], sendNow: 'r280 now ' + id }); }
      const snap = async () => { const o = {}; for (const [tok, id] of kept) { const c = (await call('/api/work/current?session=' + id, tok)).data; o[id] = JSON.stringify([c.transcript, c.queue, c.sendNow]); } return o; };
      const view = async (tok) => (((await list280(tok)).sessions) || []).map((x) => x.id + '|' + ((x.worktree || {}).path || '') + '|' + x.provider + '|' + x.title).sort().join('\n');
      const before = await snap(), viewU = await view(U), viewD = await view(D), viewA = await view(A);
      const wtOf = async (ids) => (await Promise.all(ids.map((id) => sum280(U, id)))).map((x) => (x.worktree || {}).path).filter((p) => p && fs.existsSync(p)).length;
      const wt0 = await wtOf([U1, U2]);
      await call('/api/work/share', U, { conv: 'room', transcript: [], session: U1 });
      const w0 = (await call('/api/work/watch?user=t-user&after=0', D)).data; S.watch0 = w0.session === U1 && !w0.stopped;
      const pid0 = await pid280();
      const m1 = go280(U, U1, 'restart shared'), m2 = go280(U, U2, 'restart two'), m3 = go280(D, D1, 'restart d');
      await working280(U, h1, 1); await working280(U, h2, 1); await working280(D, h2, 1);
      S.watchRun = !!(await until280(async () => (((await call('/api/work/watch?user=t-user&after=0', D)).data.events) || []).some((e) => /restart shared/.test(e.text || '')), 10000));
      spawnSync(process.execPath, [path.join(SB, 'restart.js')], { cwd: SB });
      S.held = [(await call('/api/chat', A, { message: 'r280 held' })).status, (await call('/api/chat', U, { message: 'r280 held', session: uA[1] })).status].join('/');
      const mids = await Promise.all([m1, m2, m3]); S.midRuns = mids.every(ran280);
      let pid1 = pid0; for (let i = 0; i < 240 && (pid1 === pid0 || !pid1); i++) { await sleep(250); pid1 = await pid280(); }
      S.restarted = !!pid1 && pid1 !== pid0;
      S.watchStopped = (await call('/api/work/watch?user=t-user&after=0', D)).data.stopped === true;
      const after = await snap();
      S.sameConv = Object.keys(before).every((k) => before[k] === after[k]);
      S.sameList = [await view(U) === viewU, await view(D) === viewD, await view(A) === viewA].join('/');
      S.wtKept = wt0 === 2 && (await wtOf([U1, U2])) === 2;
      S.hostsBack = !!(await online280(A, h1)) && !!(await online280(D, h2, (x) => x.sessionLimits.perProvider === 1 && x.sessionLimits.ownerBypass === true && x.sessionLimits.perUser === 5));
      S.offered2 = !!(await until280(offered, 30000));
      // a share lives in the hub's memory: the person shares the session again and the watcher follows it
      await call('/api/work/share', U, { conv: 'room', transcript: [], session: U1 });
      const p1 = await chat(U, 'r280 after restart shared', { session: U1 }), p2 = await chat(U, 'r280 after restart two', { session: U2 }), p3 = await chat(D, 'r280 after restart d', { session: D1 });
      S.afterRuns = ran280(p1) && ran280(p2) && ran280(p3);
      S.sameClaude = !!sysId280(p1) && sysId280(p1) === sysId280(mids[0]) && sysId280(p2) === sysId280(mids[1]) && sysId280(p3) === sysId280(mids[2]);
      const w2 = (await call('/api/work/watch?user=t-user&after=0', D)).data, wt2 = (w2.events || []).map((e) => e.text).join(' | ');
      S.watchAgain = w2.session === U1 && /after restart shared/.test(wt2) && !/after restart two/.test(wt2);
      await call('/api/work/share', U, { conv: '' });

      // ---- 3. a host restart while runs go: its run ends cleanly, nothing stays "working", a run while it is down says so, runs go through once it is back
      const E1 = uA[1], E2 = uC[1];
      const k1 = go280(U, E1, 'host dies'), k2 = go280(U, E2, 'other host');
      await working280(U, h1, 1); await working280(U, h2, 1);
      kill280(h1.proc); S.exited = !!(await until280(async () => h1.exit === true, 15000));
      const [d1, d2] = await Promise.all([within280(k1, 60000), within280(k2, 60000)]);
      S.hostRun = !d1.stuck && d1.status === 200 && d1.events.some((e) => e.type === 'bridge_done'); S.otherHostRun = ran280(d2);
      S.idle = !!(await until280(async () => (await sum280(U, E1)).state !== 'working' && !(await used280(U, h1)).used, 15000));
      S.goesOffline = !!(await until280(async () => { const x = await hostSt280(A, h1); return x && !x.online; }, 30000));
      const off = await chat(U, 'r280 while it is down', { session: E1 });
      S.downRun = off.status === 200 && off.events.some((e) => e.type === 'bridge_error' && /offline/.test(e.text || '')) && !!(await until280(async () => !(await used280(U, h1)).used, 5000));
      h1.start(); S.backUp = !!(await online280(A, h1));
      const again = await chat(U, 'r280 after the host restart', { session: E1 });
      const aRun = await chat(A, 'r280 owner after the host restart', { session: aOwn[0] });
      S.afterHost = ran280(again) && ran280(aRun);

      // ---- 4. an older page (no session in its calls) keeps the first session while others run: it sees it, runs it, stops it, the others are untouched
      await call('/api/work/current', U, { transcript: [{ cls: 'me', text: 'r280 first session marker' }] });
      await call('/api/work/queue', U, { queue: ['r280 first session queue'], sendNow: null });
      await call('/api/model', U, { provider: prov280(h2, 'c2'), model: 'claude-haiku-5-5' });
      const oOther = go280(U, uA[2], 'other while old'); await working280(U, h1, 1);
      const oOld = chat(U, 'SLOW r280 older page runs the first session'); await working280(U, h2, 1);
      const cur = (await call('/api/work/current', U)).data, curOther = (await call('/api/work/current?session=' + uA[2], U)).data;
      S.oldSees = JSON.stringify(cur.transcript || []).includes('first session marker') && (cur.queue || []).join() === 'r280 first session queue' && !JSON.stringify(curOther.transcript || []).includes('first session marker');
      const att = (await call('/api/work/attach?run=&after=0', U)).data || {};
      S.oldAttach = att.busy === true && (att.events || []).some((e) => e.type === 'bridge_run');
      S.oldBusy = (await call('/api/chat', U, { message: 'x' })).status;
      S.states = [(await sum280(U, primary.id)).state, (await sum280(U, uA[2])).state].join('/');
      S.oldStop = (await call('/api/stop', U, {})).status;
      S.oldStopped = !(await oOld).events.some((e) => e.type === 'result');
      S.otherLeft = (await sum280(U, uA[2])).state;
      S.otherRan = ran280(await oOther);
    } catch (e) { S.err = String((e && e.stack) || e).slice(0, 400); }
    // put everything back
    await call('/api/work/share', U, { conv: '' });
    for (const h of hosts280) if (h.proc) kill280(h.proc);
    if (mySt) await call('/api/model', U, { provider: mySt.provider || 'host', model: mySt.model || 'claude-opus-5-5' });
    await call('/api/work/current', U, { transcript: [] }); await call('/api/work/queue', U, { queue: [], sendNow: null });
    for (const [tok, id] of mine280) await call('/api/work/sessions/delete', tok, { id });
    for (const h of hosts280) await call('/api/hosts/remove', h.tok, { id: h.id });
    const E = S.err ? ' · ERROR ' + S.err : '';
    check('R280', 'T-290 three people (t-admin, t-user, mills) on two native hosts, up to 5 sessions each, SLOW turns at once (17 runs): t-user\'s fifth working session on a host is fine (3 on one provider + 2 on another) and the sixth is 429 naming the host (5 of 5), a fourth on one provider is 429 naming the provider (3 of 3), an older page (no session) is told the same, refused sessions stay idle, the counters say 5 and 3; each host\'s owner runs 4 on one provider on their own host (beyond 3); the owner is limited once that host sets ownerBypass off (the limits control: 429 at its provider cap, nobody else may set it) and free again when it is on',
      !S.err && S.up && S.offered && S.reached && S.ownersReached && S.provCap && S.hostCap && S.oldCap && S.perHost === '5/3' && S.refusedIdle === 'ready/ready/true' && S.waveOk
      && S.notOwner === 403 && S.off === 200 && S.offSeen && S.ownerLimited && S.b1 && S.on === 200 && S.onSeen && S.ownerFree,
      `hosts up ${S.up}/${S.offered} · all running ${S.reached}/${S.ownersReached} · provider cap ${S.provCap} · host cap ${S.hostCap} · older page ${S.oldCap} · counters ${S.perHost} · refused stay ${S.refusedIdle} · runs ok ${S.waveOk} (${S.waveBad}) · limits control: not owner ${S.notOwner}, off ${S.off}/${S.offSeen}, owner refused ${S.ownerLimited}, ran ${S.b1}, on ${S.on}/${S.onSeen}, owner free ${S.ownerFree}${E}`);
    check('R281', 'T-290 a hub restart asked while sessions of three people run on both hosts: it waits for the runs (they all finish, through their hosts) and no new run starts meanwhile (503, also for another session), then comes back with every session as it was (same list, same worktrees, each conversation, queue and "Send now"), both hosts reconnect on their own with the limits they announced, and the sessions go on in the same Claude conversation',
      !S.err && S.held === '503/503' && S.midRuns && S.restarted && S.sameConv && S.sameList === 'true/true/true' && S.wtKept && S.hostsBack && S.offered2 && S.afterRuns && S.sameClaude,
      `held ${S.held} · runs finished ${S.midRuns} · new pid ${S.restarted} · conversations ${S.sameConv} · lists ${S.sameList} · worktrees ${S.wtKept} · hosts back ${S.hostsBack}/${S.offered2} · runs after ${S.afterRuns} · same Claude session ${S.sameClaude}${E}`);
    check('R282', 'T-290 a host killed while runs go: its run ends cleanly (a finished stream, the session not "working", no slot held), the other host\'s run is not disturbed, a run while it is down answers that it is offline (and holds no slot), and once it is back runs go through again (for t-user and for the host\'s owner)',
      !S.err && S.exited && S.hostRun && S.otherHostRun && S.idle && S.goesOffline && S.downRun && S.backUp && S.afterHost,
      `killed ${S.exited} · its run ended ${S.hostRun} · other host ${S.otherHostRun} · idle ${S.idle} · offline ${S.goesOffline} · run while down ${S.downRun} · back ${S.backUp} · runs after ${S.afterHost}${E}`);
    check('R283', 'T-290 a shared Work session is watched by another person (mills) while it runs and across a hub restart: the watcher sees the shared session\'s lines as they are sent, the restart ends the share (it lives in the hub\'s memory) and the watcher is told it stopped, and after the owner shares the same session again the watcher sees it again, with the new lines of that session and none of the owner\'s other sessions',
      !S.err && S.watch0 && S.watchRun && S.watchStopped && S.watchAgain,
      `watch at once ${S.watch0} · sees the run ${S.watchRun} · stopped after the restart ${S.watchStopped} · follows it again ${S.watchAgain}${E}`);
    check('R284', 'T-290 an older page (calls without a session) keeps meaning the first session while other sessions run: it reads that conversation and queue (not another session\'s), runs it (attach finds the run), a second message is 409, Stop ends only that run, and the other session of the same person finishes untouched',
      !S.err && S.oldSees && S.oldAttach && S.oldBusy === 409 && S.states === 'working/working' && S.oldStop === 200 && S.oldStopped && S.otherLeft === 'working' && S.otherRan,
      `sees its own ${S.oldSees} · attach ${S.oldAttach} · second message ${S.oldBusy} · states ${S.states} · stop ${S.oldStop} ended without an answer ${S.oldStopped} · the other still ${S.otherLeft}, finished ${S.otherRan}${E}`);
  } else for (const [id, what] of [['R280', 'three people on two native hosts: caps and owners'], ['R281', 'a hub restart mid-run keeps every session'], ['R282', 'a host restart mid-run'], ['R283', 'a shared session across a restart'], ['R284', 'an older page next to the sessions']]) skip(id, what + ' (T-290)', 'the Rust hub\'s and the native host\'s: --rust-hub with a built vr-host runs it');
  if (RUST_HUB) { // R210-R214 (T-245 = T-195 · 1/4): the AI asks its person (team note plans/question-protocol.md); the fake Claude's "ASKTOOL" calls ask_user like the team MCP relay
    const askOf = (events) => { const t = texts(events).split('\n').find((l) => l.startsWith('ASK:')); try { return JSON.parse(t.slice(4)); } catch { return {}; } };
    const sessionsOf = async () => ((await call('/api/work/sessions', U)).data.sessions) || [];
    const primary = async () => (await sessionsOf()).find((x) => x.primary) || {};
    // R210: a series reaches the page as an event, the session waits (yellow), the answers come back as one tool result
    { let q = null, during = null, seenAfter = null; const seen = {};
      const r = await chat(U, 'ASKTOOL r210', { onEvent: (ev) => { if (ev.type === 'bridge_question') { q = ev; (async () => { during = await primary(); seen.stranger = (await call('/api/work/answer', D, { id: ev.id, answers: [] })).status; seen.go = (await call('/api/work/answer', U, { id: ev.id, answers: [{ id: 'which', picked: ['left', 'right'] }, { id: 'what', picked: ['b', 'zzz'], other: 'ignored' }] })).status; })(); } } });
      const res = askOf(r.events).result || {}; seenAfter = await primary();
      const ans = res.answers || [];
      check('R210', 'T-245 ask_user: a question series reaches the page as a bridge_question event (kinds, options, recommended defaults), its session is "waiting" (kind question, ref = the question) while it is open, and the answers come back to the AI as one tool result (what was picked, cut to what each question allows, by: user); then the event bridge_answered closes it and the session is no longer waiting; another person cannot answer it',
        !!q && q.questions.length === 2 && q.questions[0].kind === 'single' && q.questions[0].default[0] === 'right' && q.questions[1].kind === 'checklist' && q.questions[1].other === false && q.timeoutSec === 600
        && during.state === 'waiting' && during.waiting.kind === 'question' && during.waiting.ref === q.id
        && seen.stranger === 404 && seen.go === 200 && res.answered === true && res.by === 'user' && ans[0].picked.join() === 'left' && ans[1].picked.join() === 'b' && !ans[1].other && ans[1].labels.join() === 'b'
        && r.events.some((e) => e.type === 'bridge_answered' && e.id === q.id && e.by === 'user') && seenAfter.state !== 'waiting' && !seenAfter.waiting,
        `event ${q ? q.questions.map((x) => x.kind + ':' + (x.default || []).join('+')).join(' / ') + ' timeout ' + q.timeoutSec : 'none'} · during ${during && during.state}/${during && during.waiting && during.waiting.kind} · stranger ${seen.stranger}, answer ${seen.go} · result ${JSON.stringify(res).slice(0, 200)} · after ${seenAfter.state}`); }
    // R211: nobody answers: after the timeout the recommended answers are used, and the AI is told so
    { const t0 = Date.now(); const r = await chat(U, 'ASKTOOL TIMEOUT r211'); const took = Date.now() - t0, res = askOf(r.events).result || {}, ans = res.answers || [], p = await primary();
      check('R211', 'T-245 an unanswered question times out (timeoutSec, default 10 minutes): the recommended answers are used (by: default, timedOut: true) and the AI is told so, the page gets bridge_answered by default, and the session is no longer waiting',
        res.answered === true && res.by === 'default' && res.timedOut === true && ans[0].picked.join() === 'right' && ans[1].picked.join() === 'a,c' && ans.every((x) => x.by === 'default')
        && r.events.some((e) => e.type === 'bridge_answered' && e.by === 'default') && took >= 1800 && took < 20000 && !p.waiting,
        `result ${JSON.stringify(res).slice(0, 220)} · took ${took} ms · waiting after ${JSON.stringify(p.waiting)}`); }
    // R212: a question belongs to the session whose run asked, so a person with two sessions running is asked in the right one
    { const sid = (await call('/api/work/sessions/new', U, { title: 'r212', background: true })).data.id; const seen = {}; let q = null;
      if (sid) {
        const first = call('/api/chat', U, { message: 'SLOW r212 first session' }); await sleep(600);
        const r = await chat(U, 'ASKTOOL r212', { session: sid, onEvent: (ev) => { if (ev.type === 'bridge_question') { q = ev; (async () => {
          const list = (await call('/api/work/questions?session=' + sid, U)).data.questions || []; seen.listed = list.length === 1 && list[0].id === ev.id;
          seen.otherSession = ((await call('/api/work/questions?session=none', U)).data.questions || []).length;
          const all = await sessionsOf(); seen.child = (all.find((x) => x.id === sid) || {}).state; seen.first = (all.find((x) => x.primary) || {}).state;
          seen.answer = (await call('/api/work/answer', U, { id: ev.id, defaults: true })).status; })(); } } });
        await first; const res = askOf(r.events).result || {};
        seen.by = res.by; seen.session = q && q.session;
        await call('/api/work/sessions/delete', U, { id: sid });
      }
      check('R212', 'T-245 a question is asked in the session whose run asked it (its MCP relay names the session): its event carries the session id, GET /api/work/questions lists it for that session only, that session is "waiting" while another session of the same person is "working" (not waiting); {defaults: true} answers with the recommended answers',
        !!sid && seen.session === sid && seen.listed && seen.otherSession === 0 && seen.child === 'waiting' && seen.first === 'working' && seen.answer === 200 && seen.by === 'default',
        `event session ${seen.session} (asked in ${sid}) · listed ${seen.listed}, other session lists ${seen.otherSession} · states: asking ${seen.child}, other ${seen.first} · answer ${seen.answer} by ${seen.by}`); }
    // R213: a call longer than one HTTP request is held (the relay polls), and "I am on it" (activity) starts the timeout over
    { let q = null; const seen = {};
      const r = await chat(U, 'ASKTOOL POLL r213', { onEvent: (ev) => { if (ev.type === 'bridge_question') { q = ev; (async () => {
        await sleep(1500); const act = await call('/api/work/answer', U, { id: ev.id, activity: true }); seen.act = act.status; seen.dl = act.data.deadline;
        await sleep(1700); seen.go = (await call('/api/work/answer', U, { id: ev.id, answers: [{ id: 'which', picked: ['right'] }] })).status; })(); } } });
      const res = askOf(r.events).result || {}, ans = res.answers || [];
      check('R213', 'T-245 a question may wait longer than one tool call is held: the hub answers {pending, id} after pollSec and the MCP relay asks again with {poll: id}; the answer still arrives as the tool result (picked what was sent, the unanswered question took its recommended answer: by mixed); {activity: true} says the person is on it and restarts the timeout (a new deadline)',
        !!q && seen.act === 200 && seen.dl > q.deadline && seen.go === 200 && res.answered === true && res.by === 'mixed' && !res.timedOut && ans[0].by === 'user' && ans[1].by === 'default' && ans[1].picked.join() === 'a,c',
        `activity ${seen.act} deadline ${seen.dl} (was ${q && q.deadline}) · answer ${seen.go} · result ${JSON.stringify(res).slice(0, 200)}`); }
    // R214: a stopped run ends its open question; the tool is offered by the JavaScript and the native MCP servers and the page shows the event
    { let q = null; const seen = {};
      await chat(U, 'ASKTOOL r214', { onEvent: (ev) => { if (ev.type === 'bridge_question') { q = ev; (async () => { await sleep(400); seen.during = (await primary()).state; await call('/api/stop', U, {}); })(); } } });
      await sleep(300); const p = await primary(); seen.after = p.state; seen.waiting = p.waiting; seen.open = ((await call('/api/work/questions', U)).data.questions || []).length;
      seen.late = q ? (await call('/api/work/answer', U, { id: q.id, defaults: true })).status : 0;
      const nativeTools = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.map((t) => t.name);
      const jsTools = (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1));
      const page = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
      check('R214', 'T-245 stopping the run ends its open question (the session is no longer waiting, nothing is left open, a late answer finds no question: 404); [static] ask_user is offered by team-mcp.js and by the native host MCP (mcp-tools.json), and the Work page shows bridge_question and sends the answer to /api/work/answer',
        !!q && seen.during === 'waiting' && seen.after !== 'waiting' && !seen.waiting && seen.open === 0 && seen.late === 404 && jsTools.includes('ask_user') && nativeTools.includes('ask_user') && /bridge_question/.test(page) && /\/api\/work\/answer/.test(page),
        `while open ${seen.during} · after the stop ${seen.after} waiting ${JSON.stringify(seen.waiting)} · open ${seen.open} · late answer ${seen.late} · js tool ${jsTools.includes('ask_user')} native tool ${nativeTools.includes('ask_user')}`); }
  } else for (const [id, l] of [['R210', 'a question series reaches the page and the answers come back'], ['R211', 'an unanswered question takes the recommended answers'], ['R212', 'a question is asked in the session that asked'], ['R213', 'a long question polls; activity restarts the timeout'], ['R214', 'a stopped run ends its question']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R240-R244 (T-246 / T-248): the question UI's page, the person's "when the AI asks" setting, the activity rule, the alert prefs
    const askOf = (events) => { const t = texts(events).split('\n').find((l) => l.startsWith('ASK:')); try { return JSON.parse(t.slice(4)); } catch { return {}; } };
    const askOnce = async (msg, after) => { let q = null; const seen = {};
      const r = await chat(U, msg, { onEvent: (ev) => { if (ev.type === 'bridge_question') { q = ev; (async () => { try { await after(ev, seen); } catch {} seen.go = (await call('/api/work/answer', U, { id: ev.id, defaults: true })).status; })(); } } });
      return { q, seen, res: askOf(r.events).result || {} }; };
    // R240: the settings are saved (clamped, add-only: the rest of the prefs stays)
    const nickBefore = ((await call('/api/prefs', U)).data.prefs || {}).nick;
    const sv = await call('/api/prefs', U, { ask: { auto: true, minutes: 2, activity: false }, sound: { ask: 55 }, tts: { ask: true } });
    const pf = (await call('/api/prefs', U)).data.prefs || {};
    const big = (await call('/api/prefs', U, { ask: { minutes: 99999 } })).data.prefs.ask, small = (await call('/api/prefs', U, { ask: { minutes: 0 } })).data.prefs.ask;
    await call('/api/prefs', U, { ask: { minutes: 2 } });
    check('R240', 'T-248 the "when the AI asks" settings are saved per person (ask: auto, minutes, activity; the alert sound volume sound.ask; tts.ask), minutes are kept between 1 and 1440, and the other preferences are left as they were',
      sv.status === 200 && pf.ask && pf.ask.auto === true && pf.ask.minutes === 2 && pf.ask.activity === false && pf.sound.ask === 55 && pf.tts.ask === true && big.minutes === 1440 && small.minutes >= 1 && pf.nick === nickBefore,
      `saved ${JSON.stringify(pf.ask)} sound.ask ${pf.sound && pf.sound.ask} tts.ask ${pf.tts && pf.tts.ask} · 99999 -> ${big.minutes}, 0 -> ${small.minutes}`);
    // R241: a question with no timeoutSec of its own waits as long as the person's setting says
    { const t = await askOnce('ASKTOOL r241', async () => {});
      check('R241', 'T-248 a question that has no timeoutSec of its own waits the minutes of the person\'s setting (2 minutes: timeoutSec 120, a deadline 120 s after it was asked), not the 10-minute default',
        !!t.q && t.q.timeoutSec === 120 && t.q.deadline - t.q.at === 120000 && t.res.answered === true, `timeoutSec ${t.q && t.q.timeoutSec} deadline-at ${t.q && t.q.deadline - t.q.at}`); }
    // R242: turned off = never; the tool's own timeoutSec still wins over the setting
    { await call('/api/prefs', U, { ask: { auto: false } });
      const off = await askOnce('ASKTOOL r242', async () => {});
      const t0 = Date.now(); const own = await chat(U, 'ASKTOOL TIMEOUT r242'); const took = Date.now() - t0, ownRes = askOf(own.events).result || {};
      await call('/api/prefs', U, { ask: { auto: true, minutes: 3 } });
      check('R242', 'T-248 with "use the recommended after N minutes" turned off the question waits until answered (timeoutSec 0, no deadline); a question that brings its own timeoutSec still times out by it',
        !!off.q && off.q.timeoutSec === 0 && off.q.deadline === null && off.res.answered === true
        && ownRes.timedOut === true && ownRes.by === 'default' && took < 20000, `off: timeoutSec ${off.q && off.q.timeoutSec} deadline ${off.q && off.q.deadline} · own timeout: ${ownRes.timedOut} after ${took} ms`); }
    // R243: any activity restarts the wait from the person's setting (the page sends it for a click, a key, a focus)
    { const t = await askOnce('ASKTOOL r243', async (ev, seen) => { await sleep(1300); const a = await call('/api/work/answer', U, { id: ev.id, activity: true }); seen.act = a.status; seen.dl = a.data.deadline; });
      check('R243', 'T-248 activity on a question restarts the wait from the person\'s setting (3 minutes): the new deadline is 180 s from the activity, later than the first one',
        !!t.q && t.q.timeoutSec === 180 && t.seen.act === 200 && t.seen.dl > t.q.deadline && t.seen.dl - Date.now() > 170000 && t.seen.dl - Date.now() <= 180000, `first ${t.q && t.q.deadline - t.q.at} ms, after activity +${t.seen.dl - t.q.at} ms from the ask`); }
    await call('/api/prefs', U, { ask: { auto: true, minutes: 10, activity: true }, sound: { ask: 80 }, tts: { ask: false } });
    // R244: the page (static: no browser here)
    { const page = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
      check('R244', 'T-246/T-248 [static] the Work page shows a question series one at a time (Back / Next / Done, radio, checkboxes, checklist with select all / none and a filter, own answer, recommended marked, countdown, Use the recommended), restores it after a reload (GET /api/work/questions, sessionStorage), the "User action required" tone has its own kind and volume in Settings, a "When the AI asks" settings group, speech and the desktop notification respect do-not-disturb, and any activity is sent at most every 15 s',
        /function showQuestion/.test(page) && /'Next'/.test(page) && /'Done'/.test(page) && /Select all/.test(page) && /Select none/.test(page) && /Filter the list/.test(page) && /qrec/.test(page) && /Use the recommended/.test(page) && /restoreQuestions/.test(page) && /api\/work\/questions/.test(page) && /sessionStorage/.test(page)
        && /ask: \[0, 0\.95\]/.test(page) && /id="volAsk"/.test(page) && /id="setAskMin"/.test(page) && /When the AI asks/.test(page) && /User action required/.test(page) && /if \(dndOn\(\)\) return;\s*\n\s*if \(P\.sound\.on\) playTone\('ask'/.test(page) && /NATIVE\.call\('notify', \{ title: 'User action required'/.test(page) && /now - lastPoke < 15000/.test(page),
        'page checked'); }
  } else for (const [id, l] of [['R240', 'the question settings are saved'], ['R241', 'a question waits the person\'s minutes'], ['R242', 'off = never; the tool\'s own timeout wins'], ['R243', 'activity restarts the wait'], ['R244', 'the question UI on the page']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R255-R259 (T-239): the person's project index, pin / hide, remove my worktree, admin delete (to the trash)
    const PR = path.join(SCRATCH, 'projects'), TRASH = path.join(PR, '_trash');
    const gg = (cwd, ...a) => spawnSync('git', a, { cwd, encoding: 'utf8' });
    const mine = async (tok, q = '') => (await call('/api/projects/mine' + q, tok)).data;
    const row = (d, n) => (d.projects || []).find((p) => p.name === n) || {};
    const profU = path.join(SB, 'profiles', 't-user', 'projects.json');
    await call('/api/projects', A, { name: 'idx-one' }); await call('/api/projects', A, { name: 'idx-two' });
    await call('/api/workspace', U, { project: 'idx-one' }); await call('/api/workspace', A, { project: 'idx-one' });
    // R255: the index
    const u1 = await mine(U), a1 = await mine(A), fj = await mine(U, '?forgejo=1');
    check('R255', 'T-239 GET /api/projects/mine is the person\'s index: VibeRoller first-class, a project they work in is "mine" with its worktree (branch, dirty, unmerged count) and when they last used it, one they have not touched is listed under discover, another person\'s worktree shows in "othersWorktrees"; Forgejo repos are only listed on request and say so when no token is saved',
      row(u1, 'VibeRoller').hub === true && row(u1, 'idx-one').mine === true && row(u1, 'idx-one').lastUsed > 0 && row(u1, 'idx-one').worktree && row(u1, 'idx-one').worktree.branch === 'work/t-user' && row(u1, 'idx-one').worktree.dirty === false
      && row(u1, 'idx-two').mine === false && u1.discover.includes('idx-two') && !u1.discover.includes('idx-one') && (row(a1, 'idx-one').othersWorktrees || []).includes('t-user') && (row(u1, 'idx-one').othersWorktrees || []).includes('t-admin')
      && u1.forgejo === undefined && fj.forgejo && fj.forgejo.available === false && Array.isArray(fj.forgejo.repos) && u1.admin === false && a1.admin === true,
      `idx-one ${JSON.stringify(row(u1, 'idx-one')).slice(0, 220)} discover ${JSON.stringify(u1.discover)} forgejo ${JSON.stringify(fj.forgejo).slice(0, 90)}`);
    // R256: pin / hide, per person, stored add-only
    const pin = await call('/api/projects/pin', U, { project: 'idx-one', on: true }), hide = await call('/api/projects/hide', U, { project: 'idx-two', on: true });
    const u2 = await mine(U), a2 = await mine(A), bad = await call('/api/projects/pin', U, { project: 'nope-x' });
    const stored1 = JSON.parse(fs.readFileSync(profU, 'utf8'));
    await call('/api/projects/hide', U, { project: 'idx-two', on: false });
    const stored2 = JSON.parse(fs.readFileSync(profU, 'utf8')), u3 = await mine(U);
    check('R256', 'T-239 pin and hide are the person\'s own (another person\'s list is unchanged), a pinned project sorts first, a hidden one is flagged and leaves discover, they are kept in <profile>/projects.json (add-only: showing again keeps the entry and the last-used time), and an unknown project → 404',
      pin.status === 200 && hide.status === 200 && u2.projects[0].name === 'idx-one' && row(u2, 'idx-one').pinned === true && row(u2, 'idx-two').hidden === true && !u2.discover.includes('idx-two') && row(a2, 'idx-one').pinned === false && row(a2, 'idx-two').hidden === false
      && bad.status === 404 && stored1.projects['idx-one'].pinned === true && stored1.projects['idx-one'].usedAt > 0 && stored1.projects['idx-two'].hidden === true && stored2.projects['idx-two'] && stored2.projects['idx-two'].hidden === false && stored2.projects['idx-one'].usedAt === stored1.projects['idx-one'].usedAt && row(u3, 'idx-two').hidden === false,
      `first ${u2.projects[0].name} pinned ${row(u2, 'idx-one').pinned} A sees pinned ${row(a2, 'idx-one').pinned} stored ${JSON.stringify(stored2.projects).slice(0, 140)} bad ${bad.status}`);
    // R257: remove MY worktree
    const dU = path.join(PR, 'idx-one', 't-user'), dA = path.join(PR, 'idx-one', 't-admin'), repo1 = path.join(PR, 'idx-one', '_repo');
    fs.writeFileSync(path.join(dU, 'mine.txt'), 'committed\n'); gg(dU, 'add', '-A'); gg(dU, '-c', 'user.name=t', '-c', 'user.email=t@x', 'commit', '-q', '-m', 'my commit');
    fs.writeFileSync(path.join(dU, 'wip.txt'), 'uncommitted\n');
    const slow = chat(U, 'SLOW r257 hold the project'); await sleep(900);
    const busy = await call('/api/projects/remove-worktree', U, { project: 'idx-one' });
    await slow;
    const hubRm = await call('/api/projects/remove-worktree', U, { project: 'VibeRoller' }), noWt = await call('/api/projects/remove-worktree', U, { project: 'idx-two' });
    const rm = await call('/api/projects/remove-worktree', U, { project: 'idx-one' });
    const refs = gg(repo1, 'for-each-ref', '--format=%(refname)', 'refs/viberoller/removed/t-user/').stdout.trim().split('\n').filter(Boolean);
    const kept = refs[0] ? gg(repo1, 'ls-tree', '-r', '--name-only', refs[0]).stdout : '';
    const again = await call('/api/projects/remove-worktree', U, { project: 'idx-one' }), u4 = await mine(U);
    check('R257', 'T-239 "remove my worktree": refused (409) while one of their sessions works in the project; VibeRoller → 400, no worktree → 409; otherwise their uncommitted file is committed, the unmerged branch is kept as refs/viberoller/removed/<user>/<stamp> (both files on it), the worktree folder and work/<user> go, another person\'s worktree of the project is untouched, and the project says "no worktree of mine"',
      busy.status === 409 && /working/.test(busy.data.error || '') && hubRm.status === 400 && noWt.status === 409 && rm.status === 200 && /kept as refs\/viberoller\/removed\/t-user\//.test(rm.data.message || '') && !fs.existsSync(dU)
      && refs.length >= 1 && /mine\.txt/.test(kept) && /wip\.txt/.test(kept) && gg(repo1, 'rev-parse', '--verify', '--quiet', 'refs/heads/work/t-user').status !== 0 && fs.existsSync(path.join(dA, '.git')) && again.status === 409 && row(u4, 'idx-one').mine === false,
      `busy ${busy.status} ${busy.data.error} · hub ${hubRm.status} · none ${noWt.status} · removed ${rm.status} ${(rm.data.message || rm.data.error || '').slice(0, 120)} · refs ${refs.join(',')} · kept files ${kept.trim().replace(/\n/g, ',')}`);
    // R258: delete a whole project: admin only, nobody else's worktree, moved to the trash
    const wt258 = (...a) => spawnSync(process.execPath, [path.join(SB, 'worktree.js'), ...a], { cwd: SB, encoding: 'utf8' });
    wt258('--project', 'idx-two', 'ensure', 't-user');
    const delU = await call('/api/projects/delete', U, { project: 'idx-two' }), delOthers = await call('/api/projects/delete', A, { project: 'idx-two' });
    await call('/api/projects/remove-worktree', U, { project: 'idx-two' });
    const delHub = await call('/api/projects/delete', A, { project: 'VibeRoller' }), delNone = await call('/api/projects/delete', A, { project: 'nope-x' });
    fs.writeFileSync(path.join(dA, 'admin-note.txt'), 'admin work\n');
    const del = await call('/api/projects/delete', A, { project: 'idx-two' }), del1 = await call('/api/projects/delete', A, { project: 'idx-one' });
    const trashed = fs.existsSync(TRASH) ? fs.readdirSync(TRASH) : [];
    const t1 = trashed.find((n) => n.startsWith('idx-one-')) || '', t2 = trashed.find((n) => n.startsWith('idx-two-')) || '';
    const a3 = await mine(A);
    await call('/api/workspace', U, { project: 'VibeRoller' }); await call('/api/workspace', A, { project: 'VibeRoller' });
    check('R258', 'T-239 deleting a project: only an admin (403), never VibeRoller (400), an unknown one 404, refused (409) while another person still has a worktree in it (naming them); when it is only the admin\'s own, the whole folder is MOVED to <projectsRoot>/_trash/<name>-<stamp> (nothing erased: the repo and the admin\'s uncommitted file are in it), the answer says so, and it leaves the list',
      delU.status === 403 && delOthers.status === 409 && /t-user/.test(delOthers.data.error || '') && delHub.status === 400 && delNone.status === 404 && del.status === 200 && del1.status === 200 && /not deleted/.test(del.data.message || '')
      && !fs.existsSync(path.join(PR, 'idx-two')) && !fs.existsSync(path.join(PR, 'idx-one')) && !!t1 && !!t2 && fs.existsSync(path.join(TRASH, t2, '_repo', '.git')) && fs.existsSync(path.join(TRASH, t1, 't-admin', 'admin-note.txt')) && !row(a3, 'idx-one').name && !row(a3, 'idx-two').name,
      `user ${delU.status} others ${delOthers.status} ${delOthers.data.error} · hub ${delHub.status} none ${delNone.status} · two ${del.status} one ${del1.status} · trash ${trashed.join(',')}`);
    // R259: the page (static)
    const page259 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R259', 'T-239 [static] Settings has a "My projects" section (id projBox) that lists the index from /api/projects/mine and offers Pin / Hide, Remove my worktree and (admins) Delete project, each destructive one asking twice, shows what the trash does, and lists the Forgejo repos only when asked',
      /id="projBox"/.test(page259) && /function renderMyProjects/.test(page259) && /\/api\/projects\/mine/.test(page259) && /\/api\/projects\/pin/.test(page259) && /\/api\/projects\/hide/.test(page259) && /\/api\/projects\/remove-worktree/.test(page259) && /\/api\/projects\/delete/.test(page259) && /Remove my worktree/.test(page259) && /Delete project/.test(page259) && /List my Forgejo repos/.test(page259) && /nothing is erased/.test(page259), 'page checked');
  } else for (const [id, l] of [['R255', 'the person\'s project index'], ['R256', 'pin and hide'], ['R257', 'remove my worktree'], ['R258', 'delete a project (admin, to the trash)'], ['R259', 'the My projects section on the page']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R215 to R219 (T-262): presence on the task board. mills (D) holds claims; t-user (U) looks; T-Admin (A) makes tasks
    const find215 = async (tok, id) => ((await call('/api/board', tok)).data.tasks || []).find((t) => t.id === id) || {};
    const keep215 = (tok, dev) => { let on = true; (async () => { let ver = -1; while (on) { const r = await call(`/api/room/messages?rev=0&wait=1&ver=${ver}&device=${dev}`, tok); if (r.status === 200 && r.data.ver !== undefined) ver = r.data.ver; else await sleep(300); } })(); return () => { on = false; }; };
    const seen = {};
    const t215 = (await call('/api/board/create', A, { title: 'r215 presence on a card', assignee: 'mills' })).data.task;
    if (t215) {
      await call('/api/presence/me', D, { override: 'auto', acceptingWork: false, text: '' });
      await call('/api/presence', D, { device: 'r215x', gone: true });
      await call('/api/board/claim', D, { id: t215.id, minutes: 30 });
      // R215: the live status of the holder and the assignee on the card (the board view and one task), seen by another person
      const stopA = keep215(D, 'r215a'); await sleep(500);
      await call('/api/presence', D, { device: 'r215a', idle: false });
      const card = await find215(U, t215.id), one = (await call('/api/board/task?id=' + t215.id, U)).data.task || {};
      seen.card = card; seen.one = one;
      // R216: the claim holder goes offline (appear offline) with the claim held: the card is marked; back online, it is not
      await call('/api/presence/me', D, { override: 'offline' }); await sleep(200);
      seen.off = await find215(U, t215.id); const self216 = await find215(D, t215.id);
      await call('/api/presence/me', D, { override: 'auto' }); await sleep(200);
      seen.on = await find215(U, t215.id);
      // R217: the assign picker data: people with available (accepting work and not offline), and what each holds
      await call('/api/presence/me', D, { acceptingWork: true }); await sleep(200);
      const pe = async (tok) => ((await call('/api/board', tok)).data.people || []).find((p) => p.user === 'mills') || {};
      seen.av = await pe(U);
      await call('/api/presence/me', D, { override: 'offline' }); await sleep(200); seen.avOff = await pe(U);
      await call('/api/presence/me', D, { override: 'auto', acceptingWork: false }); await sleep(200); seen.avNo = await pe(U);
      // R218: a list / one task as the board tools give them (the same operation) carries presence
      const list = (await call('/api/board', U)).data; seen.everyone = (list.people || []).length >= 3;
      // R219: a person whose run works while they are offline is not a stale claim: Working, not holderOffline
      stopA(); await call('/api/presence', D, { device: 'r215a', gone: true }); await sleep(300);
      seen.gone = await find215(U, t215.id);
      const slow = call('/api/chat', D, { message: 'SLOW r215' }); await sleep(800);
      seen.work = await find215(U, t215.id); await slow;
      await call('/api/board/release', D, { id: t215.id }); await call('/api/board/delete', A, { id: t215.id });
      seen.self = self216;
    }
    const h = (c) => (c && c.holder) || {};
    check('R215', 'T-262 a card shows the live status of whoever holds its claim (holder: user, presence, label, since) and of its assignee (assigneeStatus), in the board view and in one task',
      !!t215 && h(seen.card).user === 'mills' && h(seen.card).presence === 'online' && h(seen.card).label === 'Online' && typeof h(seen.card).since === 'number' && (seen.card.assigneeStatus || {}).user === 'mills' && h(seen.one).user === 'mills' && (seen.one.assigneeStatus || {}).label === 'Online',
      `holder ${JSON.stringify(h(seen.card))} · assignee ${(seen.card && seen.card.assigneeStatus || {}).label} · one task holder ${h(seen.one).user}`);
    check('R216', 'T-262 a claim whose holder is offline is marked (claim.holderOffline and holderOffline on the card) so it can be taken over; back online, the mark is gone',
      !!t215 && seen.off.holderOffline === true && (seen.off.claim || {}).holderOffline === true && h(seen.off).presence === 'offline' && !seen.on.holderOffline && !(seen.on.claim || {}).holderOffline && !seen.card.holderOffline,
      `offline: ${seen.off && seen.off.holderOffline} (${h(seen.off).label}) · online again: ${seen.on && seen.on.holderOffline}`);
    check('R217', 'T-262 the board view lists people for the assign picker: available = accepting work and not offline (also what they hold and have assigned); appearing offline or not accepting makes them not available',
      !!t215 && seen.av.available === true && seen.av.label !== 'Offline' && seen.avOff.available === false && seen.avOff.presence === 'offline' && seen.avNo.available === false && Array.isArray(seen.av.holding) && seen.av.holding.includes(t215.id) && seen.av.assignedOpen >= 1,
      `available ${seen.av && seen.av.available} (${seen.av && seen.av.label}) · appearing offline ${seen.avOff && seen.avOff.available} · not accepting ${seen.avNo && seen.avNo.available} · holding ${JSON.stringify(seen.av && seen.av.holding)}`);
    check('R218', 'T-262 the board tools (board_list, task_get: the same operations) carry presence, so an AI can check before assigning; [static] the tools go through board_op_presence and say so in their descriptions',
      !!t215 && seen.everyone === true && /board_op_presence\(self,/.test(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-hub', 'src', 'modules', 'traits.rs'), 'utf8')) && /holderOffline/.test(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')),
      `people on the board answer ${seen.everyone}`);
    check('R219', 'T-262 a holder whose own run is working while they are offline shows Working and the claim is not marked stale; the person sees their own card with their real status',
      !!t215 && h(seen.gone).presence === 'offline' && seen.gone.holderOffline === true && h(seen.work).working === true && h(seen.work).label === 'Working' && !seen.work.holderOffline && !!seen.self.holder,
      `offline, idle: ${h(seen.gone).label} stale ${seen.gone && seen.gone.holderOffline} · running: ${h(seen.work).label} stale ${seen.work && seen.work.holderOffline}`);
  } else for (const [id, what] of [['R215', 'a card shows its holder\'s live status'], ['R216', 'a claim whose holder is offline is marked'], ['R217', 'the assign picker lists who is available'], ['R218', 'the board tools carry presence'], ['R219', 'a working holder is not a stale claim']]) skip(id, what, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R235 to R239 (T-236): device-code sign-in for tools (Jcode PGM): per-device tokens, approve / refuse / revoke
    const s235 = {};
    const start = async (extra) => (await call('/api/device/start', null, { client: 'jcodepgm', device: 'r235 laptop', ...(extra || {}) }));
    const st = await start();
    const sd = st.data || {};
    s235.shape = st.status === 200 && typeof sd.deviceCode === 'string' && sd.deviceCode.length >= 48 && /^[A-Z]{4}-[A-Z]{4}$/.test(sd.userCode || '') && sd.expiresIn === 600 && sd.interval === 5 && /device=/.test(sd.verificationUrl || '');
    s235.p1 = await call('/api/device/poll', null, { deviceCode: sd.deviceCode });
    s235.p2 = await call('/api/device/poll', null, { deviceCode: sd.deviceCode }); // at once again: too fast
    s235.wrong = await call('/api/device/approve', U, { userCode: 'BBBB-BBBB' });
    s235.nosess = await call('/api/device/approve', null, { userCode: sd.userCode });
    s235.ok = await call('/api/device/approve', D, { userCode: sd.userCode.toLowerCase() });
    s235.again = await call('/api/device/approve', U, { userCode: sd.userCode });
    s235.p3 = await call('/api/device/poll', null, { deviceCode: sd.deviceCode });
    const tok = (s235.p3.data || {}).token;
    s235.chk = tok ? await call('/api/check', tok) : {};
    s235.board = tok ? await call('/api/board', tok) : {};
    s235.notes = tok ? await call('/api/notes', tok) : {};
    s235.p4 = await call('/api/device/poll', null, { deviceCode: sd.deviceCode }); // a delivered code is spent
    // R236: the token acts as its person, is its own thing (not the page session), cannot approve more devices
    s235.viaDev = tok ? await call('/api/device/approve', tok, { userCode: 'BBBB-BBBB' }) : {};
    s235.sessionStill = (await call('/api/check', D)).status;
    // R237: devices list: own only; another person sees none and cannot revoke it
    s235.listD = await call('/api/devices', D); s235.listU = await call('/api/devices', U);
    const dev = ((s235.listD.data || {}).devices || [])[0] || {};
    s235.other = await call('/api/devices/revoke', U, { id: dev.id });
    s235.stillWorks = tok ? (await call('/api/check', tok)).status : 0;
    s235.listJson = JSON.stringify(s235.listD.data || {});
    const keep = fs.existsSync(path.join(SB, 'device-tokens.json')) ? fs.readFileSync(path.join(SB, 'device-tokens.json'), 'utf8') : '';
    s235.hashed = !!tok && keep.includes(crypto.createHash('sha256').update(tok).digest('hex')) && !keep.includes(tok) && !keep.includes(sd.deviceCode);
    s235.revoke = await call('/api/devices/revoke', D, { id: dev.id });
    s235.after = tok ? await call('/api/check', tok) : {}; s235.afterBoard = tok ? await call('/api/board', tok) : {};
    s235.listAfter = await call('/api/devices', D);
    // R238: refusal and expiry
    const s2 = (await start()).data;
    await call('/api/device/poll', null, { deviceCode: s2.deviceCode });
    s235.deny = await call('/api/device/deny', D, { userCode: s2.userCode });
    s235.dpoll = await call('/api/device/poll', null, { deviceCode: s2.deviceCode });
    const s3 = (await start({ expiresIn: 1 })).data; await sleep(1500);
    s235.exp = await call('/api/device/poll', null, { deviceCode: s3.deviceCode });
    s235.expApprove = await call('/api/device/approve', D, { userCode: s3.userCode });
    s235.junk = await call('/api/device/poll', null, { deviceCode: 'nope' });
    // R239: secrets stay secrets: only a hash of the token is kept, the list never shows it, and the page has the box
    s235.page = /\/api\/device\/approve/.test(fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8')) && /api\/devices\/revoke/.test(fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'));
    const s = s235;
    check('R235', 'T-236 device sign-in: start gives a long device code, a short user code, interval and expiry; polling says pending (428), polling too fast says slow_down (429); a wrong code is refused (404); approving needs a signed-in session (401); after the person approves, the poll gives a NEW token for that person (and the code is spent)',
      s.shape && s.p1.status === 428 && s.p1.data.pending === true && s.p2.status === 429 && s.p2.data.error === 'slow_down' && s.wrong.status === 404 && s.nosess.status === 401 && s.ok.status === 200 && s.ok.data.client === 'jcodepgm' && s.again.status === 409 && s.p3.status === 200 && typeof tok === 'string' && tok.length >= 48 && s.p3.data.username === 'mills' && tok !== D && s.p4.status === 410,
      `start ${st.status} shape ${s.shape} · pending ${s.p1.status} · fast ${s.p2.status} · wrong ${s.wrong.status} · no session ${s.nosess.status} · approve ${s.ok.status} · twice ${s.again.status} · poll ${s.p3.status} ${s.p3.data && s.p3.data.username} · spent ${s.p4.status}`);
    check('R236', 'T-236 the device token works as its person on the API (/api/check, the board, the notes) while the page session goes on; but it cannot approve another device (403)',
      s.chk.status === 200 && (s.chk.data.user || s.chk.data.username || '') !== '' && s.board.status === 200 && s.notes.status === 200 && s.viaDev.status === 403 && s.sessionStill === 200,
      `check ${s.chk.status} · board ${s.board.status} · notes ${s.notes.status} · approve with a device token ${s.viaDev.status} · page session ${s.sessionStill}`);
    check('R237', 'T-236 Settings lists a person\'s own devices (name, client, created, lastUsed; never the token), another person sees none and cannot revoke it (404); revoking stops the token at once on every route (401) and it leaves the list',
      s.listD.status === 200 && (s.listD.data.devices || []).length === 1 && dev.name === 'r235 laptop' && dev.client === 'jcodepgm' && !!dev.created && dev.current === false && !s.listJson.includes(tok) && (s.listU.data.devices || []).length === 0 && s.other.status === 404 && s.stillWorks === 200 && s.revoke.status === 200 && s.after.status === 401 && s.afterBoard.status === 401 && (s.listAfter.data.devices || []).length === 0,
      `own ${(s.listD.data.devices || []).length} · other ${(s.listU.data.devices || []).length} · other revokes ${s.other.status} · revoke ${s.revoke.status} · token after ${s.after.status}/${s.afterBoard.status} · list after ${(s.listAfter.data.devices || []).length}`);
    check('R238', 'T-236 a refused sign-in: the poll says denied (403) and gives no token; a request past its time says expired (410) and cannot be approved; an unknown device code is 410 too',
      s.deny.status === 200 && s.deny.data.approved === false && s.dpoll.status === 403 && s.dpoll.data.denied === true && !s.dpoll.data.token && s.exp.status === 410 && s.exp.data.expired === true && s.expApprove.status === 404 && s.junk.status === 410,
      `deny ${s.deny.status} · poll ${s.dpoll.status} · expired ${s.exp.status} · approve expired ${s.expApprove.status} · unknown ${s.junk.status}`);
    check('R239', 'T-236 only a hash of a device token is kept (device-tokens.json holds neither the token nor the device code); [static] the page has the user-code box (approve) and the Signed-in devices list with Revoke',
      s.hashed && s.page, `hashed ${s.hashed} · page ${s.page}`);
  } else for (const [id, what] of [['R235', 'device sign-in: start, pending, slow_down, approve, token'], ['R236', 'a device token works as its person'], ['R237', 'own devices listed, revoke stops the token'], ['R238', 'a refused or expired device sign-in'], ['R239', 'device tokens kept hashed; the page has the box']]) skip(id, what, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R270-R274 (T-250): the Products section of the Download panel: the team's product releases through the hub, with the hub's own Forgejo token
    const s270 = {};
    s270.anon = await call('/api/releases'); s270.anonFile = await call('/api/releases/file?product=fake-prod&tag=v2.0.0&asset=fake-prod-windows-x86_64.zip');
    s270.list = await call('/api/releases', U);
    const prods = (s270.list.data || {}).products || [], fp = prods.find((x) => x.product === 'fake-prod') || {}, gone = prods.find((x) => x.product === 'fake-gone') || {};
    const fetchFile = (q, tok) => fetch(H + '/api/releases/file?' + q, { headers: tok ? { Authorization: 'Bearer ' + tok } : {} });
    const fr = await fetchFile('product=fake-prod&tag=v2.0.0&asset=fake-prod-windows-x86_64.zip', U), got = Buffer.from(await fr.arrayBuffer());
    s270.file = { status: fr.status, same: got.equals(FAKE_ASSET), cd: fr.headers.get('content-disposition') || '', len: fr.headers.get('content-length') };
    s270.unlisted = (await fetchFile('product=fake-prod&tag=v2.0.0&asset=..%2F..%2Fetc%2Fpasswd', U)).status;
    s270.unknownProduct = (await fetchFile('product=nope&tag=v2.0.0&asset=x', U)).status;
    s270.signedOutFile = (await fetchFile('product=fake-prod&tag=v2.0.0&asset=fake-prod-windows-x86_64.zip', '')).status;
    const everything = JSON.stringify(s270.list.data) + [...fr.headers.entries()].join(',');
    const hubLog = fs.existsSync(path.join(SCRATCH, 'hub.log')) ? fs.readFileSync(path.join(SCRATCH, 'hub.log'), 'utf8') : '';
    const listCalls = () => fakeForgejo.relCalls.filter((c) => /^\/api\/v1\/repos\/UnityAILab\/fake-prod\/releases/.test(c.url)).length;
    const before = listCalls(); await call('/api/releases', U); const cached = listCalls() === before && before === 1;
    check('R270', 'T-250 the releases catalogue lists the fake Forgejo\'s releases for a signed-in account: product, label, versions newest first (a draft is left out), date, notes, assets with name, size and the platform guessed from the name (Windows, Linux, macOS); a product whose repo cannot be read says so and the others still list',
      s270.list.status === 200 && fp.label === 'Fake Product' && (fp.releases || []).map((x) => x.version).join() === 'v2.0.0,v1.0.0' && fp.releases[0].date === '2026-10-20T10:00:00Z' && fp.releases[0].notes === 'Second release notes'
      && fp.releases[0].assets.map((x) => x.platform).join() === 'Windows,Linux' && fp.releases[0].assets[0].size === FAKE_ASSET.length && fp.releases[1].assets[0].platform === 'macOS' && (gone.releases || []).length === 0 && !!gone.error,
      `list ${s270.list.status} · ${prods.map((x) => x.product + ':' + (x.releases || []).length).join(' ')} · platforms ${((fp.releases || [{ assets: [] }])[0].assets || []).map((x) => x.platform).join()}`);
    check('R271', 'T-250 a release file streams through the hub with exactly the right bytes (300 KB), its length and a Content-Disposition naming the file',
      s270.file.status === 200 && s270.file.same && /attachment; filename="fake-prod-windows-x86_64\.zip"/.test(s270.file.cd) && Number(s270.file.len) === FAKE_ASSET.length,
      `status ${s270.file.status} · same bytes ${s270.file.same} · ${s270.file.cd} · length ${s270.file.len}`);
    check('R272', 'T-250 a signed-out request is refused (401) for the catalogue and for a file; a file that is not in the catalogue (a path trick, an unknown product) is 404 and nothing is asked of Forgejo for it',
      s270.anon.status === 401 && s270.anonFile.status === 401 && s270.signedOutFile === 401 && s270.unlisted === 404 && s270.unknownProduct === 404 && !fakeForgejo.relCalls.some((c) => /passwd|nope/.test(c.url)),
      `signed out ${s270.anon.status}/${s270.anonFile.status}/${s270.signedOutFile} · unlisted ${s270.unlisted} · unknown product ${s270.unknownProduct}`);
    check('R273', 'T-250 the hub\'s Forgejo token never appears in any answer (catalogue, file headers) or in the hub log, yet Forgejo got it on every call (a private repo answers 404 without it)',
      !everything.includes('fake-forgejo-token') && !hubLog.includes('fake-forgejo-token') && fakeForgejo.relCalls.length > 0 && fakeForgejo.relCalls.every((c) => c.auth === 'token fake-forgejo-token'),
      `answers clean ${!everything.includes('fake-forgejo-token')} · log clean ${!hubLog.includes('fake-forgejo-token')} · calls with the token ${fakeForgejo.relCalls.filter((c) => c.auth === 'token fake-forgejo-token').length}/${fakeForgejo.relCalls.length}`);
    const html270 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R274', 'T-250 [static] the Download panel has a Products section reading /api/releases and downloading through /api/releases/file (the person\'s platform first); the catalogue is cached (a second look makes no new call to Forgejo)',
      /\/api\/releases'/.test(html270) && /\/api\/releases\/file\?/.test(html270) && /dlProducts/.test(html270) && cached,
      `page ${/dlProducts/.test(html270)} · cached ${cached} (${before} call)`);
  } else for (const [id, what] of [['R270', 'releases catalogue'], ['R271', 'a release file streams'], ['R272', 'signed out refused'], ['R273', 'token never in an answer'], ['R274', 'Products section + cache']]) skip(id, what, '--rust-hub only');
  if (RUST_HUB) { // R295-R299 (T-244): co-view: the sharer's page says which view it shows, viewers follow it in their own page
    const DM = 'dm:t-admin|t-user', cvPoll = async (tok, after = 0) => (await call(`/api/coview/poll?after=${after}&ver=-1`, tok)).data;
    const s295 = {};
    s295.start = await call('/api/coview/start', A, { conv: DM, screen: 'menu' });
    s295.outsiderJoin = await call('/api/coview/join', D, { sharer: 't-admin' });
    s295.outsiderSees = (await cvPoll(D)).available;
    s295.outsiderStart = await call('/api/coview/start', D, { conv: DM });
    s295.outsiderView = await call('/api/coview/view', D, { screen: 'board' });
    s295.join = await call('/api/coview/join', U, { sharer: 't-admin' });
    const ver0 = (await cvPoll(U)).ver;
    const waiting = call(`/api/coview/poll?wait=1&after=0&ver=${ver0}`, U); // hangs until the sharer's page shows something new
    const t0 = Date.now(); await new Promise((r) => setTimeout(r, 400));
    s295.v1 = await call('/api/coview/view', A, { screen: 'board', item: { kind: 'board-task', id: 'T-77' }, scroll: 0.25 });
    s295.woke = (await waiting).data; s295.wokeMs = Date.now() - t0;
    s295.v2 = await call('/api/coview/view', A, { screen: 'notes', item: { kind: 'note', id: 'plans/next.md' } });
    s295.same = await call('/api/coview/view', A, { screen: 'notes', item: { kind: 'note', id: 'plans/next.md' } });
    s295.v3 = await call('/api/coview/view', A, { screen: 'shell' });
    s295.badScreen = await call('/api/coview/view', A, { screen: 'admin' });
    s295.viewerView = await call('/api/coview/view', U, { screen: 'cal' });
    s295.uAll = (await cvPoll(U)).mine; s295.uTail = (await cvPoll(U, s295.v2.data.seq)).mine;
    const seqs = (s295.uAll.views || []).map((v) => v.seq), screens = (s295.uAll.views || []).map((v) => v.screen);
    check('R295', 'T-244 a sharer posts what the page shows and a viewer\'s waiting poll wakes with it, then gets every view in order (and only the ones after what it already has); showing the same thing twice adds nothing',
      s295.start.status === 200 && s295.join.status === 200 && s295.woke.mine && s295.woke.mine.view.screen === 'board' && s295.woke.mine.view.item.id === 'T-77' && s295.woke.mine.view.scroll === 0.25 && s295.wokeMs >= 300 && s295.wokeMs < 5000
      && s295.same.data.seq === s295.v2.data.seq && seqs.join() === [1, 2, 3, 4].join() && screens.join() === 'menu,board,notes,shell' && s295.uTail.views.map((v) => v.screen).join() === 'shell',
      `woke after ${s295.wokeMs} ms · seqs ${seqs.join()} · screens ${screens.join()} · tail ${(s295.uTail.views || []).map((v) => v.screen).join()}`);
    check('R296', 'T-244 someone outside the conversation cannot join, see, start or steer a co-view (403 / 400 / 409, and it is not even listed); an unknown screen is refused; a viewer who does not lead cannot post the view (403)',
      s295.outsiderJoin.status === 403 && (s295.outsiderSees || []).length === 0 && s295.outsiderStart.status === 400 && s295.outsiderView.status === 409 && s295.badScreen.status === 400 && s295.viewerView.status === 403,
      `join ${s295.outsiderJoin.status} · listed ${(s295.outsiderSees || []).length} · start ${s295.outsiderStart.status} · view ${s295.outsiderView.status} · bad screen ${s295.badScreen.status} · viewer posts ${s295.viewerView.status}`);
    const s297 = {};
    s297.untaken = await call('/api/coview/take', U, {}); // not invited yet
    s297.stranger = await call('/api/coview/handover', U, { to: 't-user' }); // a viewer is not the sharer
    s297.invite = await call('/api/coview/invite', A, { to: 't-user' });
    s297.invited = (await cvPoll(U)).mine.invited;
    s297.take = await call('/api/coview/take', U, {});
    s297.uView = await call('/api/coview/view', U, { screen: 'cal' });
    s297.aView = await call('/api/coview/view', A, { screen: 'room' });
    s297.aState = (await cvPoll(A)).mine;
    s297.back = await call('/api/coview/handover', A, { to: 't-admin' });
    s297.backState = (await cvPoll(U)).mine;
    check('R297', 'T-244 take-over moves the lead: a viewer cannot take it uninvited, the sharer invites, the viewer accepts and then leads (the sharer\'s own posts are refused and the sharer sees the viewer as lead); the sharer can hand it back and the viewer follows again',
      s297.untaken.status === 403 && s297.stranger.status === 403 && s297.invite.status === 200 && s297.invited === true && s297.take.status === 200 && s297.take.data.mine.lead === 't-user' && s297.uView.status === 200 && s297.aView.status === 403
      && s297.aState.lead === 't-user' && s297.aState.isLead === false && s297.aState.view.screen === 'cal' && s297.back.status === 200 && s297.backState.lead === 't-admin' && s297.backState.isLead === false,
      `untaken ${s297.untaken.status} · invite ${s297.invite.status} · take ${s297.take.status} lead ${(s297.take.data.mine || {}).lead} · old lead posts ${s297.aView.status} · back ${s297.backState.lead}`);
    const s299 = {};
    s299.secretConv = await call('/api/coview/view', A, { screen: 'room', item: { kind: 'conv', id: 'dm:mills|t-admin', text: 'SECRET-BODY' } });
    s299.u1 = await call('/api/coview/poll?after=0&ver=-1', U);
    s299.a1 = await call('/api/coview/poll?after=0&ver=-1', A);
    s299.work = await call('/api/coview/view', A, { screen: 'shell', item: { kind: 'work-session', id: 'mills' } });
    s299.u2 = await call('/api/coview/poll?after=0&ver=-1', U);
    s299.note = await call('/api/coview/view', A, { screen: 'notes', item: { kind: 'note', id: 'private/x.md', text: 'SECRET-BODY', body: 'SECRET-BODY' } });
    s299.u3 = await call('/api/coview/poll?after=0&ver=-1', U);
    const raw = JSON.stringify([s299.u1.data, s299.u2.data, s299.u3.data]);
    const hiddenConv = s299.u1.data.mine.views.find((v) => v.seq === s299.secretConv.data.seq), hiddenWork = s299.u2.data.mine.views.find((v) => v.seq === s299.work.data.seq), noteItem = s299.u3.data.mine.views.find((v) => v.seq === s299.note.data.seq);
    check('R299', 'T-244 nothing a viewer could not read is carried: a private conversation they are not in, or another person\'s Work session that is not shared with them, reaches them only as "hidden" (no id, no name); an item carries just its kind and id, never content; the sharer still sees their own',
      s299.secretConv.status === 200 && hiddenConv.item.hidden === true && !hiddenConv.item.id && hiddenWork.item.hidden === true && !hiddenWork.item.id && !raw.includes('dm:mills|t-admin') && !raw.includes('SECRET-BODY')
      && Object.keys(noteItem.item).sort().join() === 'id,kind' && s299.a1.data.mine.views.find((v) => v.seq === s299.secretConv.data.seq).item.id === 'dm:mills|t-admin',
      `conv ${JSON.stringify(hiddenConv.item)} · work ${JSON.stringify(hiddenWork.item)} · note item keys ${Object.keys(noteItem.item).join()} · leaked ${raw.includes('SECRET-BODY') || raw.includes('dm:mills|t-admin')}`);
    const s298 = {};
    s298.leave = await call('/api/coview/stop', U, {});
    s298.afterLeave = (await cvPoll(A)).mine;
    s298.rejoin = await call('/api/coview/join', U, { sharer: 't-admin' });
    s298.end = await call('/api/coview/stop', A, {});
    s298.uState = await cvPoll(U); s298.uPost = await call('/api/coview/view', U, { screen: 'board' });
    s298.joinEnded = await call('/api/coview/join', U, { sharer: 't-admin' });
    check('R298', 'T-244 stop ends it: a viewer who stops leaves (the sharer sees nobody following, the co-view goes on); when the sharer ends it every viewer\'s page is told (no co-view, nothing listed), nothing can be posted and nobody can join it any more',
      s298.leave.status === 200 && s298.afterLeave.viewers.length === 0 && s298.rejoin.status === 200 && s298.end.status === 200 && s298.uState.mine === null && (s298.uState.available || []).length === 0 && s298.uPost.status === 409 && s298.joinEnded.status === 404,
      `viewers after leave ${s298.afterLeave.viewers.length} · after end: mine ${JSON.stringify(s298.uState.mine)} listed ${(s298.uState.available || []).length} · post ${s298.uPost.status} · join ${s298.joinEnded.status}`);
  } else for (const [id, what] of [['R295', 'a sharer\'s views reach a viewer in order'], ['R296', 'a non-member cannot join'], ['R297', 'take-over moves the lead'], ['R298', 'stop ends a co-view'], ['R299', 'nothing a viewer could not read is carried']]) skip(id, what, '--rust-hub only');
  { // R260 (T-241): "Reopen what I had open after a reload": a per-person pref, absent = on, kept with the other prefs; the page honours it
    const html260 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    const c0 = (await call('/api/check', U)).data || {};
    const off = await call('/api/prefs', U, { reopen: false }), afterOff = (await call('/api/check', U)).data || {};
    const other = await call('/api/prefs', U, { autoScroll: false }), kept = (await call('/api/check', U)).data || {}; // another pref saved: reopen stays off
    const on = await call('/api/prefs', U, { reopen: true });
    const page = /id="setReopen"/.test(html260) && /prefs\(\)\.reopen/.test(html260) && /reopen: \$\('setReopen'\)\.checked/.test(html260) && /reopen: p\.reopen !== false/.test(html260);
    check('R260', 'T-241 "reopen what I had open after a reload" is a per-person setting: absent means on; saving false returns and keeps false (also after another setting is saved); saving true turns it on again; [static] the Settings checkbox exists and the page uses it for the Work conversation and the chat',
      (c0.prefs || {}).reopen === undefined && off.status === 200 && (off.data.prefs || {}).reopen === false && (afterOff.prefs || {}).reopen === false && (kept.prefs || {}).reopen === false && (on.data.prefs || {}).reopen === true && page,
      `default ${(c0.prefs || {}).reopen} · off ${(off.data.prefs || {}).reopen} · kept ${(kept.prefs || {}).reopen} · on ${(on.data.prefs || {}).reopen} · page ${page}`);
  }
  if (RUST_HUB) { // R261 (T-251): the Downloads list carries a `platform` hint from the User-Agent; the page puts the person's own platform first
    const ua = async (s) => ((await call('/api/downloads', U, undefined, { 'User-Agent': s })).data || {}).platform;
    const got = [await ua('Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120'), await ua('Mozilla/5.0 (X11; Linux x86_64) Firefox/121'), await ua('Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) Safari/605'), await ua('Mozilla/5.0 (Linux; Android 14) Chrome/120'), await ua('curl/8.0')];
    const html261 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    const page = /Other platforms/.test(html261) && /d\.platform/.test(html261) && /dataset\.dlMine/.test(html261);
    check('R261', 'T-251 /api/downloads says which platform the asker\'s User-Agent names (windows, linux, mac, android; empty when it says nothing); [static] the Downloads panel shows the person\'s own platform first and the others under "Other platforms"',
      got.join(',') === 'windows,linux,mac,android,' && page, `hints ${JSON.stringify(got)} · page ${page}`);
  } else skip('R261', 'the Downloads platform hint and own-platform-first panel', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R265 to R269 (T-275): planner mode in Work: a session kind whose tools only plan, enforced by the tools and by the hub
    const probe = (sid, msg) => chat(U, msg, { session: sid }).then((r) => { const m = (texts(r.events).match(/PLANNER:(\{.*\})/) || [])[1]; try { return JSON.parse(m); } catch { return {}; } });
    const readNote = async (p) => (await call('/api/notes/read?path=' + encodeURIComponent(p), U)).data || {};
    const bad = await call('/api/work/sessions/new', U, { title: 'r265 bad', background: true, mode: 'nonsense' });
    const pid = (await call('/api/work/sessions/new', U, { title: 'r265 planner', background: true, mode: 'planner' })).data.id;
    const nid = (await call('/api/work/sessions/new', U, { title: 'r268 normal', background: true })).data.id;
    const listed = ((await call('/api/work/sessions', U)).data.sessions || []);
    const sumP = listed.find((x) => x.id === pid) || {}, sumN = listed.find((x) => x.id === nid) || {};
    const pr = await probe(pid, 'PLANNERPROBE please');
    await call('/api/work/sessions/rename', U, { id: pid, title: 'r265 renamed' }); await call('/api/work/sessions/pin', U, { id: pid });
    const sumP2 = (((await call('/api/work/sessions', U)).data.sessions || []).find((x) => x.id === pid)) || {};
    const toolsOf = (s) => String(s || '').split(',');
    check('R265', 'T-275 a planner session\'s run gets no Edit / Write / NotebookEdit and no pc server in its config: POST /api/work/sessions/new {mode: "planner"} shows mode "planner" in the list (a normal one "work", an unknown mode 400, renaming / pinning never changes it); its run has only the board + team servers, Read / Glob / Grep allowed, Edit / Write / NotebookEdit / mcp__pc denied, the permission mode default and the planner segment in its prompt',
      bad.status === 400 && sumP.mode === 'planner' && sumN.mode === 'work' && sumP2.mode === 'planner'
        && pr.servers === 'board+team' && ['Read', 'Glob', 'Grep'].every((t) => toolsOf(pr.allowed).includes(t)) && !toolsOf(pr.allowed).some((t) => /^(Edit|Write|NotebookEdit|MultiEdit)$|mcp__pc/.test(t))
        && ['Edit', 'Write', 'NotebookEdit', 'mcp__pc'].every((t) => toolsOf(pr.denied).includes(t)) && pr.perm === 'default' && pr.planPrompt === true,
      `bad ${bad.status} · modes ${sumP.mode}/${sumN.mode}/${sumP2.mode} · servers ${pr.servers} · allowed ${pr.allowed} · denied ${pr.denied} · perm ${pr.perm} · prompt ${pr.planPrompt}`);
    const inside = await readNote('plans/planner-inside-r265.md'), outside = await readNote('planner-outside-r265.md'), tricky = await readNote('planner-tricky-r265.md');
    check('R266', 'T-275 a notes_write outside plans/ is refused for a planner session\'s run (plans/../x too) while inside plans/ it is written (the board\x27s own writes are the planner\x27s draft flow since T-279: task_create is refused with "Use plan_draft", see R305)',
      /under plans\//.test(pr.outside || '') && /under plans\//.test(pr.tricky || '') && pr.inside === 'written' && /Use plan_draft: the person approves the plan first/.test(pr.board || '') && /plan r265/.test(inside.text || '') && !outside.text && !tricky.text,
      `outside: ${pr.outside} · tricky: ${pr.tricky} · inside: ${pr.inside} (${(inside.text || '').slice(0, 12)}) · board ${pr.board}`);
    await call('/api/notes/delete', U, { path: 'plans/planner-inside-r265.md' }); // the normal session writes it again below
    const pcr = await probe(pid, 'PLANNERPC please');
    check('R267', 'T-275 the hub refuses a pc tool from a planner session\'s run even when it is asked for directly (nothing goes to the page; the answer comes at once)',
      /Planner mode: PC tools are not available/.test(pcr.pc || ''), `pc: ${pcr.pc}`);
    const nr = await probe(nid, 'PLANNERPROBE please');
    check('R268', 'T-275 a normal session is unchanged: its run has the pc, board and team servers, nothing denied, no planner prompt, a permission mode of the person\'s own, and notes_write outside plans/ works',
      nr.servers === 'board+pc+team' && !nr.denied && nr.planPrompt === false && nr.outside === 'written' && nr.inside === 'written',
      `servers ${nr.servers} · denied "${nr.denied}" · perm ${nr.perm} · prompt ${nr.planPrompt} · outside ${nr.outside} · inside ${nr.inside}`);
    const pg = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R269', 'T-275 [static] the page has "New planner session" next to New chat, a visible Planner badge, a way back to a normal session, and its requests carry the planner session',
      /id="newPlanner"[^>]*>New planner session/.test(pg) && /id="plannerBadge"/.test(pg) && /id="leavePlanner"/.test(pg) && /mode: 'planner'/.test(pg) && /planSession/.test(pg), 'index.html');
    for (const x of ['planner-outside-r265.md', 'plans/planner-inside-r265.md', 'planner-tricky-r265.md']) await call('/api/notes/delete', U, { path: x });
  } else for (const [id, l] of [['R265', 'a planner session\'s run gets no Edit / Write and no pc server'], ['R266', 'a notes_write outside plans/ is refused for a planner session, inside works'], ['R267', 'a pc tool is refused for a planner session'], ['R268', 'a normal session is unchanged'], ['R269', 'the page has New planner session and the Planner badge']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R305 to R309 (T-279 / T-281): a planner session drafts, the person approves, the HUB writes; the fake Claude's "PLANDRAFT" calls plan_draft through the team relay
    const planSid = (await call('/api/work/sessions/new', U, { title: 'r305 planner', background: true, mode: 'planner' })).data.id;
    const boardTasks = async () => (await call('/api/board', U)).data.tasks || [];
    const taskOf = async (id) => (await call('/api/board/task?id=' + id, U)).data.task || {};
    const noteText = async (p) => ((await call('/api/notes/read?path=' + encodeURIComponent(p), U)).data || {}).text || '';
    const planOut = (events) => { const m = (texts(events).match(/PLAN:(\{.*\})/) || [])[1]; try { return JSON.parse(m); } catch { return {}; } };
    const sessionNow = async () => ((await call('/api/work/sessions', U)).data.sessions || []).find((x) => x.id === planSid) || {};
    const planRun = (msg, onPlan) => chat(U, msg, { session: planSid, onEvent: (ev) => { if (ev.type === 'bridge_plan') onPlan(ev).catch((e) => console.log('plan handler: ' + e.message)); } });
    const planAnswer = (ev, body, tok = U) => call('/api/work/plan/answer', tok, { id: ev.id, session: ev.session, ...body });
    const roomSeq = async () => Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
    const summaries = async (after) => { const out = []; for (const m of (await call('/api/room/messages?since=' + after, U)).data.messages || []) if (m.from === 'claude' && m.as && !m.to) out.push(await roomRead(restored, m) || ''); return out.filter((t) => /Plan "/.test(t)); };
    const parts = {}; const part = (id, label, pass, detail) => { parts[id] = { label, pass, detail }; };
    const exId = (await call('/api/board/create', U, { title: 'existing r305', labels: 'keep', priority: 4 })).data.task.id;
    const mine = (re) => boardTasks().then((l) => l.filter((t) => re.test(t.title)));
    // R305 + R306 + R309: a run with direct board writes, a draft that gets a change request, then a second draft that is rejected
    const A = {}; let n305 = 0;
    const rA = await planRun('PLANDRAFT PLANDIRECT PLANTWICE PLANTOPIC=r305 PLANCHANGE=' + exId, async (ev) => {
      n305++;
      if (n305 === 1) {
        A.ev = ev; A.during = await sessionNow(); A.listed = (await call('/api/work/plans?session=' + planSid, U)).data.plans || [];
        A.boardDuring = (await mine(/Alpha r305|Beta r305|Gamma r305|planner direct r305/)).length; A.noteDuring = await noteText('plans/plan-r305-original.md');
        A.stranger = (await planAnswer(ev, { approve: 'all' }, D)).status; A.strangerSees = ((await call('/api/work/plans', D)).data.plans || []).length;
        A.empty = (await planAnswer(ev, { approve: false, revise: true })).status; A.unknown = (await call('/api/work/plan/answer', U, { id: 'p-nope', approve: 'all' })).status;
        A.revise = (await planAnswer(ev, { approve: false, revise: true, comment: 'split beta' })).status;
      } else { A.ev2 = ev; A.reject = (await planAnswer(ev, { approve: false, comment: 'not now' })).status; await sleep(150); A.late = (await planAnswer(ev, { approve: 'all' })).status; }
    });
    await sleep(600); // the handler of the second draft finishes its late answer
    const repA = planOut(rA.events), resA = (repA.result || {}).result || {};
    check('R305', 'T-279 in a planner session the AI\'s own task_create / task_update are refused ("Use plan_draft: the person approves the plan first") while board_list stays allowed, and nothing reaches the board before an approval',
      /Use plan_draft: the person approves the plan first/.test(repA.direct || '') && /Use plan_draft: the person approves the plan first/.test(repA.directUpdate || '') && repA.listOk === true && A.boardDuring === 0 && (await mine(/planner direct r305/)).length === 0,
      `create: ${repA.direct} · update: ${repA.directUpdate} · board_list ok ${repA.listOk} · drafted tasks on the board while waiting ${A.boardDuring}`);
    const dr = (A.ev || {}).draft || {};
    check('R306', 'T-279 plan_draft makes the session waiting (kind plan, ref = the draft) and shows the draft on the page as a bridge_plan event (parents with owner / priority / status / blocked on, sub-tasks, the existing-task change, not planned, the source note path); GET /api/work/plans lists it for a page that was away; the draft is saved in the profile; the source note is not written before the answer',
      A.during && A.during.state === 'waiting' && A.during.waiting && A.during.waiting.kind === 'plan' && A.during.waiting.ref === (A.ev || {}).id && (A.ev || {}).session === planSid
      && (dr.parents || []).length === 3 && dr.parents[0].owner === 't-user' && dr.parents[0].priority === 1 && dr.parents[0].subtasks.length === 2 && dr.parents[1].status === 'backlog' && dr.parents[1].blockedOn === 'Alpha r305'
      && (dr.existingChanges || [])[0] && dr.existingChanges[0].id === exId && (dr.notPlanned || []).length === 1 && dr.sourceNote === 'plans/plan-r305-original.md' && dr.sourceChars > 20 && !('sourceText' in dr)
      && A.listed.length === 1 && A.listed[0].id === A.ev.id && !A.noteDuring,
      `waiting ${JSON.stringify(A.during && A.during.waiting)} (${A.during && A.during.state}) · parents ${(dr.parents || []).length} · source ${dr.sourceNote} · listed ${(A.listed || []).length} · note before the answer "${(A.noteDuring || '').slice(0, 10)}"`);
    const ex1 = await taskOf(exId);
    const ansEvents = rA.events.filter((e) => e.type === 'bridge_plan_answered').map((e) => e.outcome);
    check('R309', 'T-279 a change request ("change …", text required) goes back to the AI as the tool result and it drafts again; a reject writes nothing (no tasks, no source note, the existing task unchanged); another person cannot answer or see the draft (404, none listed); an unknown draft is 404; the answered draft cannot be answered again; the session is no longer waiting; the page gets bridge_plan_answered each time',
      A.stranger === 404 && A.strangerSees === 0 && A.empty === 400 && A.unknown === 404 && A.revise === 200 && repA.first === 'split beta' && A.reject === 200 && [404, 409].includes(A.late) && resA.rejected === true && resA.approved === false
      && (await mine(/Alpha r305|Beta r305|Gamma r305/)).length === 0 && !(await noteText('plans/plan-r305-original.md')) && !(await noteText('plans/plan-r305-plan.md')) && (ex1.labels || []).join() === 'keep'
      && ansEvents.join() === 'change,rejected' && !(await sessionNow()).waiting && !!A.ev2 && A.ev2.id !== A.ev.id,
      `stranger ${A.stranger}/${A.strangerSees} · empty change ${A.empty} · unknown ${A.unknown} · revise ${A.revise} (AI got "${repA.first}") · reject ${A.reject} · late ${A.late} · outcomes ${ansEvents} · existing labels ${(ex1.labels || []).join()}`);
    // R307 + R308: approve two of three parents (the existing-task change waits for "all"), then approve all (blocked, change, source note appended)
    const seqB = await roomSeq(); let evB = null; const B = {};
    const rB = await planRun('PLANDRAFT PLANTOPIC=r307 PLANCHANGE=' + exId, async (ev) => { evB = ev; B.status = (await planAnswer(ev, { approve: [0, 2, 9], comment: 'only these' })).status; });
    const repB = planOut(rB.events), resB = (repB.result || {}).result || {}; const pB = resB.parents || [];
    const alpha = pB.find((p) => p.index === 0) || {}, gamma = pB.find((p) => p.index === 2) || {};
    const alphaT = await taskOf(alpha.id), buildT = await taskOf(((alpha.subtasks || [])[1] || {}).id), specT = await taskOf(((alpha.subtasks || [])[0] || {}).id), gammaSub = await taskOf(((gamma.subtasks || [])[0] || {}).id);
    const srcB = await noteText('plans/plan-r307-original.md'); const ex2 = await taskOf(exId);
    check('R307', 'T-279 approve some: only the ticked parents and their sub-tasks are written by the hub as "<person> · Claude" (5 tasks, the unticked parent and an out-of-range index ignored), sub-tasks named "T-<parent> · n/m: title" with the parent id as a label and their own owner / priority, the source note holds the person\'s words word for word, the tool result lists the new ids (created, parents, partial), the existing-task change waits for "all"',
      B.status === 200 && resB.partial === true && (resB.created || []).length === 5 && pB.length === 2 && !(await mine(/Beta r307/)).length
      && alphaT.title === 'Alpha r307' && alphaT.assignee === 't-user' && alphaT.priority === 1 && (alphaT.labels || []).includes('viberoller') && /Claude/.test(alphaT.createdBy || '') && alphaT.body.includes('plans/plan-r307-original.md') && alphaT.body.includes(`${(alpha.subtasks[0] || {}).id} · 1/2: spec (t-user)`)
      && specT.title === `${alpha.id} · 1/2: spec` && (specT.labels || []).includes(alpha.id) && specT.assignee === 't-user' && buildT.title === `${alpha.id} · 2/2: build` && buildT.assignee === 'mills' && buildT.priority === 3 && (buildT.labels || []).includes(alpha.id)
      && gammaSub.title === `${gamma.id} · 1/1: only` && (gammaSub.labels || []).includes(gamma.id)
      && srcB.includes('Idea one: the alpha.\n\nIdea two: the beta, blocked on the alpha. (r307 1)') && (resB.changes || []).length === 0 && (ex2.labels || []).join() === 'keep' && (repB.result.result.created || []).join() === (resB.created || []).join(),
      `status ${B.status} · created ${(resB.created || []).join()} · alpha "${alphaT.title}" ${alphaT.assignee} P${alphaT.priority} · sub "${specT.title}" / "${buildT.title}" ${buildT.assignee} · gamma sub "${gammaSub.title}" · source has words ${srcB.includes('Idea two: the beta')} · existing labels ${(ex2.labels || []).join()}`);
    const sumB = await summaries(seqB); const planNoteB = await noteText('plans/plan-r307-plan.md');
    const seqC = await roomSeq(); const C = {};
    const rC = await planRun('PLANDRAFT PLANTOPIC=r307 PLANCHANGE=' + exId, async (ev) => { C.status = (await planAnswer(ev, { approve: 'all' })).status; });
    const repC = planOut(rC.events), resC = (repC.result || {}).result || {}; const pC = resC.parents || [];
    const betaT = await taskOf((pC.find((p) => p.index === 1) || {}).id), alphaC = pC.find((p) => p.index === 0) || {}; const ex3 = await taskOf(exId);
    const srcC = await noteText('plans/plan-r307-original.md'), planNoteC = await noteText('plans/plan-r307-plan.md'); const sumC = await summaries(seqC);
    part('R308a', 'T-279 approve all: 3 parents + 4 sub-tasks, a parent that is blocked goes to backlog with the label blocked and names the new id of its blocker, the existing-task change is applied (labels, priority, a signed note), the source note is appended with the date instead of replaced (both wordings kept)',
      C.status === 200 && resC.partial === false && (resC.created || []).length === 7 && betaT.status === 'backlog' && (betaT.labels || []).includes('blocked') && betaT.body.includes(`**Blocked on:** ${alphaC.id} (Alpha r307)`)
      && (resC.changes || []).length === 1 && resC.changes[0].ok === true && (ex3.labels || []).join() === 'planned-r307' && ex3.priority === 2 && /Planned change: the plan touches it/.test(ex3.body || '')
      && (srcC.match(/\(r307 1\)/g) || []).length === 2 && /## 20\d\d-\d\d-\d\d/.test(srcC),
      `status ${C.status} · created ${(resC.created || []).length} · beta ${betaT.status} labels ${(betaT.labels || []).join()} · blocked line ${/Blocked on:\*\* T-\d+ \(Alpha/.test(betaT.body || '')} · change ${JSON.stringify(resC.changes)} · existing ${(ex3.labels || []).join()} P${ex3.priority} · source copies ${(srcC.match(/\(r307 1\)/g) || []).length}`);
    const c308b = { once: sumB.length === 1 && sumC.length === 1, posted: resB.summaryPosted === true && resC.summaryPosted === true, sumIds: !!sumB[0] && sumB[0].includes(alpha.id) && sumB[0].includes(gamma.id),
      sumSource: /Source: plans\/plan-r307-original\.md/.test(sumB[0] || ''), blocked: /Blocked: .*waits for/.test(sumC[0] || ''), waits: /Waits for .*:/.test(sumC[0] || ''),
      noteB: planNoteB.includes(`| ${alpha.id} |`) && !planNoteB.includes(`| ${alphaC.id} |`), noteC: planNoteC.includes(`| ${alpha.id} |`) && planNoteC.includes(`| ${alphaC.id} |`), // the table cell, not a substring: the note's own header names T-281, which holds T-28 table: /\| Task \| Title \| Owner \|/.test(planNoteC),
      owners: Object.keys(resB.newTasksFor || {}).sort().join() === 'mills,t-user', millsBuild: !!(resB.newTasksFor && resB.newTasksFor.mills && resB.newTasksFor.mills.some((t) => t.id === buildT.id)),
      userFour: !!(resC.newTasksFor && resC.newTasksFor['t-user'] && resC.newTasksFor['t-user'].length >= 4), planNote: resB.planNote === 'plans/plan-r307-plan.md' };
    part('R308b', 'T-281 after the write the plan summary is posted once to the team chat as "<person> · Claude" (who got what, what is blocked, what waits for Mills, the source and plan notes), the owner map is appended to plans/<topic>-plan.md with every approval, and the result hands each owner a "new tasks for you" list',
      Object.values(c308b).every(Boolean),
      `failing ${Object.keys(c308b).filter((k) => !c308b[k]).join(',') || 'none'} · summaries ${sumB.length}/${sumC.length} · posted ${resB.summaryPosted}/${resC.summaryPosted} · plan note has both approvals ${planNoteC.includes(alpha.id) && planNoteC.includes(alphaC.id)} · owners ${Object.keys(resB.newTasksFor || {}).sort()} · first summary "${(sumB[0] || '').replace(/\n/g, ' | ').slice(0, 160)}"`);
    const pgPlan = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    part('R308c', 'T-279 [static] the page draws bridge_plan as a draft card (a parents table with checkboxes, Approve all / Approve N of M, Change…, Reject) posting to /api/work/plan/answer, closes it on bridge_plan_answered, and re-reads GET /api/work/plans after a reload; both MCP servers list plan_draft',
      /ev\.type === 'bridge_plan'/.test(pgPlan) && /bridge_plan_answered/.test(pgPlan) && /\/api\/work\/plan\/answer/.test(pgPlan) && /Approve all/.test(pgPlan) && /Change…/.test(pgPlan) && /Reject/.test(pgPlan) && /restorePlans/.test(pgPlan) && /\/api\/work\/plans/.test(pgPlan)
      && (fs.readFileSync(path.join(BRIDGE, 'team-mcp.js'), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).some((m) => /plan_draft/.test(m)) && JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'mcp-tools.json'), 'utf8')).team.some((t) => t.name === 'plan_draft'), 'index.html, team-mcp.js, mcp-tools.json');
    check('R308', 'T-279 / T-281 approve all, then the hand-off: ' + ['R308a', 'R308b', 'R308c'].map((k) => parts[k].label.replace(/^T-2\d\d /, '')).join(' ;; '), ['R308a', 'R308b', 'R308c'].every((k) => parts[k].pass), ['R308a', 'R308b', 'R308c'].map((k) => (parts[k].pass ? '' : k + ' FAILED: ') + parts[k].detail).join(' || '));
    for (const x of ['plans/plan-r305-original.md', 'plans/plan-r307-original.md', 'plans/plan-r307-plan.md']) await call('/api/notes/delete', U, { path: x });
    await call('/api/work/sessions/delete', U, { id: planSid });
  } else for (const [id, l] of [['R305', 'a planner session\'s direct task_create is refused'], ['R306', 'plan_draft makes the session waiting and shows the draft'], ['R307', 'approve some writes only those parents'], ['R308', 'approve all, blocked, changes, source appended'], ['R309', 'a change request, a reject, another person']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB) { // R199: an answer the browser may keep (Unity's pictures: private, max-age) never names the build. 2026-10-25 a page
    // showed a picture cached under 5.13.2 after 5.13.3 went live, read its X-VR-Version and said "5.13.2 is running"
    const img199 = path.join(SCRATCH, 'Images', 'room'); fs.mkdirSync(img199, { recursive: true });
    fs.writeFileSync(path.join(img199, 'r199-test.png'), Buffer.from('89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da63f8ffff3f0005fe02fe0d4c2bba0000000049454e44ae426082', 'hex'));
    const pic199 = await fetch(H + '/api/assistant/image?ref=room%2Fr199-test', { headers: { Authorization: 'Bearer ' + U } }).catch(() => null);
    const chk199 = await fetch(H + '/api/check', { headers: { Authorization: 'Bearer ' + U } }).catch(() => null);
    const hd = (r, k) => (r && r.headers.get(k)) || '';
    const page199 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R199', 'an answer the browser may keep (Unity\'s pictures: cache-control private, max-age) does not name the hub\'s build, so a copy kept from an older build never makes a page think that build is running; a normal answer (no-store) still names it; [static] the page reads the build only from answers that are not cacheable',
      !!pic199 && pic199.status === 200 && /max-age=\d+/.test(hd(pic199, 'cache-control')) && !hd(pic199, 'x-vr-version') && !!chk199 && !!hd(chk199, 'x-vr-version') && /cacheable\(res\)/.test(page199),
      `picture ${pic199 ? pic199.status : 'no answer'} cache "${hd(pic199, 'cache-control')}" build "${hd(pic199, 'x-vr-version')}" · /api/check build "${hd(chk199, 'x-vr-version')}" · page checks ${/cacheable\(res\)/.test(page199)}`);
    fs.rmSync(path.join(img199, 'r199-test.png'), { force: true });
  } else skip('R199', 'a cacheable answer does not name the build', 'the Rust hub\'s: --rust-hub runs it');
  if (RUST_HUB && !WIN) { // R178 (FR5): deploy/trial.sh tries a build on a side port against a copy of the data before it is swapped in
    const tr = path.join(MILLS, 'deploy', 'trial.sh');
    const before178 = fs.readdirSync(require('os').tmpdir()).filter((d) => /^vr-trial-/.test(d)).length;
    const ok178 = fs.existsSync(tr) ? spawnSync('sh', [tr, RUST_HUB, SCRATCH], { encoding: 'utf8', timeout: 90000 }) : { status: -1, stdout: '', stderr: 'no deploy/trial.sh' };
    const t178 = Date.now(), bad178 = fs.existsSync(tr) ? spawnSync('sh', [tr, '/bin/false', SCRATCH], { encoding: 'utf8', timeout: 90000 }) : { status: -1, stdout: '', stderr: '' };
    const badMs = Date.now() - t178, left178 = fs.readdirSync(require('os').tmpdir()).filter((d) => /^vr-trial-/.test(d)).length - before178;
    const ad178 = fs.readFileSync(path.join(MILLS, 'deploy', 'autodeploy.sh'), 'utf8');
    check('R178', 'FR5 a new build is tried before it is swapped in: sh deploy/trial.sh <vr-hub> <checkout> copies the settings, data and page to a private temporary root, starts the build there on a free port, expects /api/status 200 naming the checkout\'s version, / 200 and /__vr-front naming the front within 30 s, stops it, removes the copy: exit 0; a build that cannot start: exit 1 within 35 s; [static] the auto-deploy stages a build, tries it and only then puts it in service',
      ok178.status === 0 && bad178.status === 1 && badMs < 35000 && left178 <= 0 && /trial\.sh/.test(ad178) && /VR_STAGE_ONLY=1/.test(ad178),
      `good build exit ${ok178.status} ${String(ok178.stdout || ok178.stderr || '').trim().split('\n').pop().slice(0, 120)} · /bin/false exit ${bad178.status} in ${badMs} ms · copies left ${Math.max(0, left178)} · autodeploy runs it ${/trial\.sh/.test(ad178)}`);
  }

  // R155: updates need an account too (4.0.0): the desktop app updates itself with the session its hub page hands it
  {
    const target = WIN ? 'x86_64-pc-windows-gnu' : 'x86_64-unknown-linux-gnu';
    const anonRel = (await fetch(H + `/api/native?app=vr-desktop&target=${target}`)).status, anonFile = (await fetch(H + `/api/native/file?app=vr-desktop&target=${target}&name=vr-desktop`)).status;
    const signedRel = (await fetch(H + `/api/native?app=vr-desktop&target=${target}`, { headers: { Authorization: 'Bearer ' + U } })).status;
    const nd = path.join(MILLS, 'native', 'crates', 'vr-desktop');
    const upd = fs.readFileSync(path.join(nd, 'src', 'update.rs'), 'utf8'), dmain = fs.readFileSync(path.join(nd, 'src', 'main.rs'), 'utf8'), dset = fs.readFileSync(path.join(nd, 'src', 'settings.rs'), 'utf8'), dbuild = fs.readFileSync(path.join(nd, 'build.rs'), 'utf8');
    const html = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R155', 'updates need an account: the desktop app\'s release and its files answer 401 without a session (signed in: the Rust hub has it, 404 while nothing is built; the JavaScript hub alone: 503); [static] the hub\'s page hands the app its session on sign-in and clears it on sign-out (set_session, allowed for the hub\'s page), the app keeps it private and only for its own hub, asks /api/native with it (the public route only for a hub before 4.0.0), and its background update waits for someone to be signed in',
      anonRel === 401 && anonFile === 401 && signedRel === (RUST_HUB ? 404 : 503)
      && /\/api\/native\{q\}/.test(upd) && /with_token\(hub, session\)/.test(upd) && /\/api\/pub\/native\{q\}/.test(upd)
      && /fn set_session\(/.test(dmain) && /allow-set-session/.test(dmain) && /"set_session"/.test(dbuild) && /hub_session\.is_empty\(\) \{ continue; \}/.test(dmain)
      && /pub hub_session: String/.test(dset) && /if next\.hub != s\.hub \{ next\.hub_session\.clear\(\); \}/.test(dset) && /hub_session: String::new\(\), \.\.s/.test(dset)
      && /NATIVE\.call\('set_session', \{ token \}\)/.test(html) && /NATIVE\.call\('set_session', \{ token: '' \}\)/.test(html),
      `anon release ${anonRel} file ${anonFile}; signed ${signedRel}`);
  }

  // R149: the server's auto-deploy restarts the hub only for a really new main (production bug 2026-10-19: commits made
  // on the server and not on Forgejo made it "deploy" the same commit every minute, and every AI host lost its line)
  {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-autodeploy-')), g = (cwd, ...a) => spawnSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', ...a], { cwd, encoding: 'utf8' });
    const bare = path.join(tmp, 'origin.git'), seed = path.join(tmp, 'seed'), app = path.join(tmp, 'app');
    const ad = fs.readFileSync(path.join(MILLS, 'deploy', 'autodeploy.sh'), 'utf8');
    let ahead = '?', fresh = '?', runnable = true;
    try {
      g(tmp, 'init', '-q', '--bare', '-b', 'main', bare); g(tmp, 'clone', '-q', bare, seed);
      fs.mkdirSync(path.join(seed, 'bridge')); fs.mkdirSync(path.join(seed, 'deploy'));
      fs.writeFileSync(path.join(seed, 'bridge', 'a.js'), '1;\n'); fs.writeFileSync(path.join(seed, 'versions.json'), '{ "project": "9.9.9" }\n'); fs.writeFileSync(path.join(seed, 'deploy', 'autodeploy.sh'), ad);
      fs.writeFileSync(path.join(seed, '.gitignore'), 'bridge/.restart\n');
      g(seed, 'add', '-A'); g(seed, 'commit', '-qm', 'one'); g(seed, 'push', '-q', 'origin', 'HEAD:main');
      g(tmp, 'clone', '-q', '-b', 'main', bare, app);
      fs.writeFileSync(path.join(app, 'bridge', 'b.js'), '2;\n'); g(app, 'add', '-A'); g(app, 'commit', '-qm', 'made on the server');
      const run = () => spawnSync('sh', [path.join(app, 'deploy', 'autodeploy.sh'), app], { encoding: 'utf8' });
      const r1 = run(); if (r1.error) runnable = false;
      ahead = `${r1.status} restart ${fs.existsSync(path.join(app, 'bridge', '.restart'))}`;
      g(app, 'reset', '-q', '--hard', 'origin/main');
      fs.writeFileSync(path.join(seed, 'bridge', 'c.js'), '3;\n'); g(seed, 'add', '-A'); g(seed, 'commit', '-qm', 'new on forgejo'); g(seed, 'push', '-q', 'origin', 'HEAD:main');
      const r2 = run(); fresh = `${r2.status} restart ${fs.existsSync(path.join(app, 'bridge', '.restart'))}`;
    } finally { try { fs.rmSync(tmp, { recursive: true, force: true }); } catch {} }
    check('R149', 'auto-deploy: a server checkout that is ahead of Forgejo main (commits made on the server) is left alone (no "deployed", no restart), while a main that really has something new still asks for one; it never restarts the hub every minute for the same commit',
      /git merge-base --is-ancestor "\$REMOTE" "\$LOCAL" && exit 0/.test(ad) && (!runnable || (ahead === '0 restart false' && fresh === '0 restart true')),
      `${runnable ? '' : 'sh not available, source check only; '}ahead → ${ahead}; new main → ${fresh}`);
  }

  // R151: a deploy that does not come up rolls itself back; the data is snapshotted first and never touched
  {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-rollback-')), g = (cwd, ...a) => spawnSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', ...a], { cwd, encoding: 'utf8' });
    const bare = path.join(tmp, 'origin.git'), seed = path.join(tmp, 'seed'), app = path.join(tmp, 'app'), statusFile = path.join(tmp, 'status.txt');
    const ad = fs.readFileSync(path.join(MILLS, 'deploy', 'autodeploy.sh'), 'utf8');
    const runnable = !spawnSync('sh', ['-c', 'true']).error && !spawnSync('curl', ['--version']).error;
    const port = await freePort(47610);
    // a stand-in hub: answers /api/status with the commit written in status.txt, or 500 when it says "down"
    const fake = spawn(process.execPath, ['-e', `require('http').createServer((q, s) => { const t = require('fs').readFileSync(${JSON.stringify(statusFile)}, 'utf8').trim(); if (t === 'down') { s.writeHead(500); return s.end(); } s.end(JSON.stringify({ ok: true, hub: true, commit: t })); }).listen(${port}, '127.0.0.1')`], { stdio: 'ignore' });
    const out = {};
    try {
      if (!runnable) throw new Error('skip');
      g(tmp, 'init', '-q', '--bare', '-b', 'main', bare); g(tmp, 'clone', '-q', bare, seed);
      for (const d of ['bridge', 'deploy']) fs.mkdirSync(path.join(seed, d));
      fs.writeFileSync(path.join(seed, 'bridge', 'a.js'), '1;\n'); fs.writeFileSync(path.join(seed, 'bridge', 'config.json'), JSON.stringify({ port }) + '\n');
      fs.writeFileSync(path.join(seed, 'versions.json'), '{ "project": "1.0.0" }\n'); fs.writeFileSync(path.join(seed, 'deploy', 'autodeploy.sh'), ad);
      fs.writeFileSync(path.join(seed, '.gitignore'), 'bridge/.restart\nbridge/.rolled-back\nbridge/state.json\n');
      const push = (file, text, ver) => { fs.writeFileSync(path.join(seed, 'bridge', file), text); fs.writeFileSync(path.join(seed, 'versions.json'), `{ "project": "${ver}" }\n`); g(seed, 'add', '-A'); g(seed, 'commit', '-qm', ver); g(seed, 'push', '-q', 'origin', 'HEAD:main'); return g(seed, 'rev-parse', 'HEAD').stdout.trim(); };
      g(seed, 'add', '-A'); g(seed, 'commit', '-qm', 'one'); g(seed, 'push', '-q', 'origin', 'HEAD:main');
      g(tmp, 'clone', '-q', '-b', 'main', bare, app);
      fs.writeFileSync(path.join(app, 'bridge', 'state.json'), '{"precious":true}\n'); // the hub's data: gitignored
      const head = () => g(app, 'rev-parse', 'HEAD').stdout.trim(), restartAsked = () => fs.existsSync(path.join(app, 'bridge', '.restart'));
      const run = () => spawnSync('sh', [path.join(app, 'deploy', 'autodeploy.sh'), app], { encoding: 'utf8' });
      const hubRestarts = (to) => { fs.rmSync(path.join(app, 'bridge', '.restart'), { force: true }); fs.writeFileSync(statusFile, to); };
      const one = head(); fs.writeFileSync(statusFile, one);
      await sleep(400); // the stand-in is listening
      // a good release: deployed, snapshot taken, verified once the hub runs it
      const good = push('b.js', '2;\n', '1.1.0');
      const d1 = run(); out.deployed = head() === good && restartAsked() && /deployed/.test(d1.stdout);
      out.snapshot = fs.readdirSync(path.join(tmp, 'snapshots')).some((f) => /^data-.*\.tar\.gz$/.test(f));
      hubRestarts(good); const v1 = run(); out.verified = /verified/.test(v1.stdout) && fs.readFileSync(path.join(tmp, 'deploy-state', 'last-good'), 'utf8').trim() === good;
      // a broken release: the hub never comes back → after 3 runs it is rolled back to the good one
      const bad = push('c.js', '3;\n', '1.2.0');
      run(); hubRestarts('down');
      const w1 = run(), w2 = run(); // two misses: still waiting, still on the new one
      out.waited = !/ROLLED BACK/.test(w1.stdout + w2.stdout) && head() === bad;
      const rb = run();
      out.rolledBack = /ROLLED BACK/.test(rb.stdout) && head() === good && restartAsked() && fs.existsSync(path.join(app, 'bridge', '.rolled-back'));
      out.dataKept = fs.readFileSync(path.join(app, 'bridge', 'state.json'), 'utf8') === '{"precious":true}\n';
      hubRestarts(good); const again = run(); out.notRetried = head() === good && !/deployed/.test(again.stdout);
      // the fix moves main on: deployed (a fast-forward from the good one)
      const fix = push('c.js', '4;\n', '1.2.1'); const d3 = run(); out.fixDeployed = head() === fix && /deployed/.test(d3.stdout);
      // a release that does not even parse is never started: it stays on the running one
      const broken = push('d.js', 'this is not javascript (\n', '1.3.0'); hubRestarts(fix); run(); // (verify the fix first)
      const d4 = run(); out.parseRefused = head() === fix && /node --check failed/.test(d4.stdout) && fs.readFileSync(path.join(tmp, 'deploy-state', 'bad'), 'utf8').trim() === broken;
    } catch (e) { if (e.message !== 'skip') out.error = e.message; }
    finally { fake.kill(); try { fs.rmSync(tmp, { recursive: true, force: true }); } catch {} }
    const srv151 = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8');
    check('R151', 'deploys never break the hub: the auto-deploy snapshots the data first, asks for a graceful restart and checks the hub comes up on the new commit (/api/status reports it, locally only); a release that does not come up is rolled back to the last good commit by itself (the data untouched, the chat told on the next start) and not retried until main moves on; one that does not parse never starts; a later fix deploys normally; [static] the hub announces a rollback and keeps running after an uncaught error',
      (!runnable || (out.deployed && out.snapshot && out.verified && out.waited && out.rolledBack && out.dataKept && out.notRetried && out.fixDeployed && out.parseRefused))
      && /VR_AUTODEPLOY_COPY=1 exec sh/.test(ad) && /commit: RUNNING_COMMIT/.test(srv151) && /\.rolled-back/.test(srv151) && /process\.on\('uncaughtException'/.test(srv151),
      runnable ? JSON.stringify(out) : 'sh / curl not available: source checks only');
  }

  // Host-set owners + co-owners (bridge/server-owners.json), applied once at start; the Mills server's named owner.
  const seedV = (await gq(U)).servers || [], seedS = seedV.find((s) => s.id === 'seedtest') || {}, millsS = seedV.find((s) => s.id === 'mills') || {};
  const seedCo = (seedS.roles || []).find((r) => r.builtin === 'coowner') || {};
  const seedOwnerOk = seedS.owner === 't-admin' && ((seedS.memberRoles || {})['t-user'] || []).includes(seedCo.id) && !((seedS.memberRoles || {})['t-admin'] || []).includes(seedCo.id);
  const seedApplied = (() => { try { const g = JSON.parse(fs.readFileSync(path.join(SCRATCH, 'Chat', 'guilds.json'), 'utf8')); return !!(g.seeds && g.seeds.seedtest && g.seeds.mills); } catch { return false; } })();
  const seedAudit = ((sv23(await gq(A), 'seedtest') || {}).audit || []).some((a) => a.by === 'host' && /owner t-admin/.test(a.detail || ''));
  const coCanEdit = await call('/api/guilds/server/update', U, { server: 'seedtest', description: 'co-owner edit r134' });
  const coCannotDelete = await call('/api/guilds/server/delete', U, { server: 'seedtest' });
  const millsOwnerShown = millsS.owner === 't-admin';
  const gs34 = fs.readFileSync(path.join(MILLS, 'web', 'guild-settings.js'), 'utf8'), gu34 = fs.readFileSync(path.join(MILLS, 'bridge', 'guilds.js'), 'utf8');
  await call('/api/guilds/server/delete', A, { server: 'seedtest' });
  check('R134', 'host-set owners: bridge/server-owners.json (gitignored) gives a server its owner and co-owners at start, once (remembered in guilds.json, so later Server Settings changes stick), skipping unknown accounts, with an audit line by "host"; the Mills server can have a named owner (hub admins keep owner rights), shown to everyone, and can be handed on but never deleted; a seeded co-owner edits settings but cannot delete',
    seedOwnerOk && seedApplied && seedAudit && coCanEdit.status === 200 && coCannotDelete.status === 400 && millsOwnerShown
    && /db\.seeds\[id\] === key/.test(gu34) && /if \(s\.everyone\) return; \/\/ the VibeRoller server is everyone's/.test(gs34) && /^bridge\/server-owners\.json$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')),
    `seedtest owner ${seedS.owner} co ${JSON.stringify((seedS.memberRoles || {})['t-user'])}, applied ${seedApplied}, audit ${seedAudit}, co edit ${coCanEdit.status} delete ${coCannotDelete.status}, mills owner "${millsS.owner}"`);

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
    /id="sessToggle"/.test(html) && /Saved conversations/.test(html) && />Save session<\/button>/.test(html) && !/>Archive<\/button>/.test(html) && /id="setAutosave"/.test(html)
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
  // R473 (T-293): the brain is not on the host's Ollama: Unity answers on a stand-in chat model (never the embedder) and
  // the state says which; with two hosts lending Ollama, the hub asks the one whose models include the brain
  const L473 = 'Unity\'s brain follows the host that has it (T-293): when the AI host\'s Ollama has no unity-brain, she answers on a stand-in chat model (an embedding model is never picked) and /api/assistant/state says so (local.standin); when the brain is back she is on it again; with two hosts lending Ollama the hub asks the one whose hello lists unity-brain, not the other; the page tells the stand-in; a native host without the brain says in its log that no Modelfile was found';
  if (RUST_HUB) {
    fakeOllamaNoBrain = true;
    const sm = await uchat(U, 'MODEL?'), st473 = (await call('/api/assistant/state', U)).data.local || {};
    fakeOllamaNoBrain = false;
    const back = await uchat(U, 'MODEL?'), st473b = (await call('/api/assistant/state', U)).data.local || {};
    const page473 = /st\.local\.standin/.test(fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'));
    // a second host whose Ollama has no brain (a native host, OLLAMA_HOST at a fake with one chat model): the brain's host is still the one asked
    let pref = 'no native build', nbLog = '';
    const why473 = buildNative();
    if (!why473) {
      const nb = require('http').createServer((req, res) => {
        let body = ''; req.on('data', (c) => { body += c; });
        req.on('end', () => {
          const j = (o) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(o)); };
          if (req.url === '/api/version') return j({ version: 'fake-nobrain' });
          if (req.url === '/api/tags') return j({ models: [{ name: 'plain-chat:latest' }] });
          if (req.url === '/api/show') return j({ capabilities: ['completion'] });
          if (req.url !== '/api/chat') { res.writeHead(404); return res.end(); }
          const b = JSON.parse(body || '{}'), user = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' };
          res.writeHead(200, { 'Content-Type': 'application/x-ndjson' });
          res.write(JSON.stringify({ message: { role: 'assistant', content: 'nobrain says: ' + user.content }, done: false }) + '\n'); res.end(JSON.stringify({ done: true }) + '\n');
        });
      });
      await new Promise((r) => nb.listen(0, '127.0.0.1', r));
      const ndir = path.join(SCRATCH, 'nobrain-host'); fs.mkdirSync(ndir, { recursive: true });
      const nexe = path.join(ndir, 'vr-host' + (WIN ? '.exe' : '')), ncfg = path.join(ndir, 'ai-node.json');
      fs.copyFileSync(NATIVE_BIN.host, nexe); fs.chmodSync(nexe, 0o755);
      fs.writeFileSync(ncfg, JSON.stringify({ hub: H, id: 'nobrain-node', token: OLD_NODE_SECRET + '-nb', user: 't-admin', autoUpdate: false, providers: [], engines: {} }), { mode: 0o600 });
      const nlog = path.join(ndir, 'out.log'), nfd = fs.openSync(nlog, 'a');
      const nproc = spawn(nexe, ['run', '--config', ncfg], { cwd: ndir, stdio: ['ignore', nfd, nfd], env: { ...process.env, OLLAMA_HOST: `127.0.0.1:${nb.address().port}` } });
      let up473 = null; for (let i = 0; i < 80 && !up473; i++) { up473 = ((await call('/api/check', A)).data.aiNodes || []).find((x) => x.id === 'nobrain-node' && x.online && (x.engines || []).includes('ollama')) || null; if (!up473) await sleep(250); }
      const asked = []; for (let i = 0; i < 3; i++) asked.push((await uchat(U, 'MODEL?')).final);
      try { nproc.kill(); } catch {}
      await new Promise((r) => nb.close(r));
      // the hub counts the second host offline within aiNodeOfflineMs; the checks after this one need Unity's engines back on the first
      for (let i = 0; i < 60; i++) { const h = ((await call('/api/check', A)).data.aiNodes || []).find((x) => x.id === 'nobrain-node'); if (!h || !h.online) break; await sleep(250); }
      nbLog = (() => { try { return fs.readFileSync(nlog, 'utf8'); } catch { return ''; } })();
      pref = up473 ? (asked.every((a) => a === 'model is unity-brain') ? 'ok' : 'asked: ' + asked.join(' | ')) : 'the second host never came up';
    }
    check('R473', L473, sm.final === 'model is plain-chat:latest' && st473.up === true && st473.model === false && st473.standin === 'plain-chat:latest'
      && back.final === 'model is unity-brain' && st473b.model === true && st473b.standin === '' && page473 && (pref === 'ok' || pref === 'no native build') && (pref === 'no native build' || /no unity-brain and no Modelfile for it was found/.test(nbLog)),
      `stand-in: "${sm.final || sm.err}" state ${JSON.stringify(st473)}; back: "${back.final || back.err}" ${JSON.stringify(st473b)}; page ${page473}; two hosts: ${pref}${why473 ? ' (' + why473 + ')' : ''}; log ${nbLog.split('\n').filter((l) => /Modelfile|Ollama/.test(l)).join(' | ').slice(0, 300)}`);
  } else skip('R473', L473, 'the Rust hub\'s Unity: --rust-hub runs it');
  const ua0 = ((await call('/api/usage', U)).data.answers || {});
  const t1 = await uchat(U, 'TASK buy regress milk');
  const ua1 = ((await call('/api/usage', U)).data.answers || {});
  if (RUST_HUB) { // R186: Unity's local-brain answer (three model rounds: the tool call, the spoken line, the closing text) is one turn in the ledger under `ollama`
    const o0 = (ua0.by && ua0.by.ollama) || {}, o1 = (ua1.by && ua1.by.ollama) || {};
    check('R186', 'Unity\'s answers in the same ledger: the local brain\'s answer (Ollama\'s prompt_eval_count / eval_count summed over its rounds) is noted once under `answers.by.ollama` (no cache numbers), next to the API answers',
      (o1.turns || 0) - (o0.turns || 0) === 1 && (o1.input || 0) - (o0.input || 0) === 300 && (o1.output || 0) - (o0.output || 0) === 30 && (o1.cached || 0) === (o0.cached || 0), `ollama ${JSON.stringify(o0)} -> ${JSON.stringify(o1)}`);
  } else skip('R186', 'Unity answers through the usage ledger', 'the Rust hub\'s: --rust-hub runs it');
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
  { // R171: admins make other admins (the founders), never remove themselves; the default account's flag stays its own
    const sum = async () => ((await call('/api/admin/accounts', A)).data.accounts || []);
    const notAdm = await call('/api/admin/set-admin', U, { username: 't-user', admin: true });
    const mk = await call('/api/admin/set-admin', A, { username: 't-user', admin: true });
    const uSees = await call('/api/admin/accounts', U), uPw = await call('/api/admin/unlock', U, { username: 'nobody-r171' }); // past the admin check (no such account), nothing changed
    const self = await call('/api/admin/set-admin', A, { username: 't-admin', admin: false });
    const local = path.join(SB, 'config.local.json'), hadLocal = fs.existsSync(local), before = hadLocal ? fs.readFileSync(local) : null;
    if (hadLocal) fs.chmodSync(local, 0o600);
    const dflt = (await sum()).find((x) => !['t-admin', 't-user'].includes(x.username)) || {};
    const mkD = await call('/api/admin/set-admin', A, { username: dflt.username, admin: true });
    const afterD = await sum(), localMode = fs.statSync(local).mode & 0o777;
    const leak = afterD.filter((x) => x.admin).map((x) => x.username).sort().join(',');
    await call('/api/admin/set-admin', A, { username: dflt.username, admin: false });
    if (hadLocal) fs.writeFileSync(local, before);
    const rm = await call('/api/admin/set-admin', A, { username: 't-user', admin: false });
    const uAfter = await call('/api/admin/accounts', U);
    check('R171', 'Admin → Make admin / Remove admin: non-admin 403; the new admin can use Admin at once (both hubs); nobody removes their own admin; the default account\'s flag goes into config.local.json (its permissions kept) and never makes the profiles admins; removed again → 403',
      notAdm.status === 403 && mk.status === 200 && mk.data.admin === true && uSees.status === 200 && uPw.status !== 403 && uPw.status < 500 && self.status === 400
      && mkD.status === 200 && mkD.data.admin === true && leak === [dflt.username, 't-admin', 't-user'].sort().join(',') && (!hadLocal || localMode === 0o600)
      && rm.status === 200 && rm.data.admin === false && uAfter.status === 403 && /set-admin/.test(html),
      `${notAdm.status} ${mk.status} ${uSees.status}/${uPw.status} self ${self.status} default ${dflt.username} ${mkD.status} admins ${leak} mode ${localMode.toString(8)} rm ${rm.status} after ${uAfter.status}`);
  }
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

  // Work state (bridge/work-state.js) set before the restart, checked after it (R135)
  const ws0 = (await call('/api/work/current', U)).data;
  const wsSave = await call('/api/work/current', U, { transcript: [{ cls: 'me', text: 'hello r135' }, { cls: 'claude', text: 'answer r135' }, { cls: 'tool', text: 'Read file r135' }, { cls: 'bogus', text: 'dropped' }], archiveId: 'a-r135', archiveTitle: 'Title r135' });
  const wsQueue = await call('/api/work/queue', U, { queue: ['first queued r135', '', 'second queued r135'], sendNow: 'send now r135' });
  const wsBad = await call('/api/work/queue', U, { queue: 'nope' });
  const wsOther = (await call('/api/work/current', A)).data;
  const wsFile = fs.existsSync(path.join(SB, 'profiles', 't-user', 'work-current.json'));
  { // R169: a page that reloads during a Work reply joins it again (the Rust hub's GET /api/work/attach), and the page
    // never reloads itself in front of you, never stays black, sends the retired GitHub Pages copy to the hub (4.1.4)
    const page169 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    const stat = /async function followRun\(saved, inSend(, gen0)?\)/.test(page169) && /\/api\/work\/attach\?run=/.test(page169) && /run: curRun/.test(page169) && /curRun && !runEnded\) await followRun\(curRun, true(, gen)?\)/.test(page169)
      && /if \(document\.visibilityState === 'visible'\) \{ updateBar\(pageUpdate\.build\); return; \}/.test(page169) && /function bootTrouble\(msg\)/.test(page169) && /location\.replace\(HUB_HOST \+ '\/'\)/.test(page169);
    if (RUST_HUB) {
      const ctl = new AbortController();
      fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + U }, body: JSON.stringify({ message: 'SLOW attach-r169' }), signal: ctl.signal }).then((r) => r.body && r.body.getReader().read()).catch(() => {});
      await sleep(1000); ctl.abort(); // the page goes away mid-reply (a reload): the reply goes on in the hub
      const mid = (await call('/api/work/attach?run=&after=0', U)).data || {};
      let d = mid, all = [...(d.events || [])];
      for (let i = 0; i < 20 && d.busy; i++) { d = (await call(`/api/work/attach?run=${d.run}&after=${d.next}`, U)).data || {}; all.push(...(d.events || [])); }
      const again = (await call(`/api/work/attach?run=${mid.run}&after=${d.next}`, U)).data || {};
      const none = (await call('/api/work/attach?run=&after=0', U)).data || {};
      const kept = (await call('/api/work/current', U)).data || {}; // (R135 checks this conversation after the restart below: put back as it was)
      const cur = (await call('/api/work/current', U, { transcript: [{ cls: 'me', text: 'x' }], run: { id: mid.run, n: 3 } })).status === 200 && ((await call('/api/work/current', U)).data.run || {}).n === 3;
      await call('/api/work/current', U, { transcript: kept.transcript || [], archiveId: kept.archiveId, archiveTitle: kept.archiveTitle });
      check('R169', 'a Work reply survives a reload: the page that went away mid-reply joins the run again (everything so far, then each event to its end: its id first, the answer, the end), afterwards there is nothing more and no run, and the transcript keeps how far into the run it goes; [static] the page follows a run after a reload or a broken stream, offers an update while you look at it (it takes it in the background), never stays black, and sends the retired GitHub Pages copy to the hub',
        mid.busy === true && /^[0-9a-f]+$/.test(mid.run || '') && (all[0] || {}).type === 'bridge_run' && all.some((e) => e.type === 'result' && /attach-r169/.test(e.result || '')) && all.some((e) => e.type === 'bridge_done') && d.busy === false
        && (again.events || []).length === 0 && again.busy === false && none.run === '' && cur && stat,
        `mid busy ${mid.busy} run ${mid.run}; events ${all.map((e) => e.type).join(',')}; end busy ${d.busy}; again ${(again.events || []).length}/${again.busy}; none "${none.run}"; current keeps run ${cur}; static ${stat}`);
    } else check('R169', 'the page follows a Work run after a reload, offers updates instead of reloading in front of you, never stays black, sends the GitHub Pages copy to the hub [static; joining a run is the Rust hub\'s: --rust-hub runs it]', stat, `static ${stat}`);
  }
  { // R347 to R349 (T-319): "Done in 0s" four times in a row after Send now
    const results = (evs) => evs.filter((e) => e.type === 'result'), uuids = (evs) => results(evs).map((e) => e.uuid);
    const sidOf = async (title) => (await call('/api/work/sessions/new', U, { title, background: true })).data.id;
    // Send now: a run is stopped, then the next message goes (as the page does when the stream ends)
    const slow = chat(U, 'SLOW sendnow-r347'); await sleep(1000);
    await call('/api/stop', U, {}); const e1 = (await slow).events;
    const e2 = (await chat(U, 'after sendnow r347')).events;
    // a queue of three, one after the other
    const q = []; for (let i = 1; i <= 3; i++) q.push((await chat(U, 'queued ' + i + ' r347')).events);
    // the same result sent three times (one uuid), and three empty turns (three uuids)
    const dup = (await chat(U, 'DUPRESULT r347')).events;
    let replay = null;
    if (RUST_HUB) { const rid = (dup.find((e) => e.type === 'bridge_run') || {}).id || ''; replay = ((await call('/api/work/attach?run=' + rid + '&after=0', U)).data || {}).events || []; }
    const multi = (await chat(U, 'MULTIRESULT r347')).events;
    // a page that joins the run again afterwards sees the same
    // two sessions at once: each stream has its own result only
    const multiSess = (await call('/api/work/sessions', U)).status === 200; // sessions are the Rust hub's
    const sa = multiSess ? await sidOf('r347 a') : '', sb = multiSess ? await sidOf('r347 b') : '';
    const [ra, rb] = multiSess ? await Promise.all([chat(U, 'SLOW two-A-r347', { session: sa }), chat(U, 'DUPRESULT two-B-r347', { session: sb })]) : [{ status: 0, events: [] }, { status: 0, events: [] }];
    const own = (r, tag) => results(r.events).length === 1 && results(r.events).every((e) => new RegExp(tag).test(e.result || ''));
    const sane = results(e1).length <= 1 && results(e2).length === 1 && q.every((e) => results(e).length === 1) && results(dup).length === 1 && new Set(uuids(multi)).size === results(multi).length;
    check('R347', 'a run says each result once: after Send now (the stopped run has at most one, the next exactly one), three queued messages (one each), the same result delivered three times (one on the wire, and one when the run is joined again), three different empty turns (each kept, with its own uuid), and two sessions at once (each stream only its own, one result)',
      sane && (!replay || results(replay).length === 1) && (!multiSess || (own(ra, 'two-A-r347') && own(rb, 'two-B-r347'))),
      `send now ${results(e1).length}/${results(e2).length}; queue ${q.map((e) => results(e).length)}; dup ${results(dup).length}; multi ${results(multi).length}; replay ${replay ? results(replay).length : 'n/a'}; sessions ${results(ra.events).length}/${results(rb.events).length} (status ${ra.status}/${rb.status})`);
    // the page's logic, run on its own source (a browser cannot run here)
    const page347 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    const m = page347.match(/\/\/ <t319>[^\n]*\n([\s\S]*?)\/\/ <\/t319>/);
    let P = null; try { P = m ? require('vm').runInNewContext(m[1] + '; ({ sysCollapse, sysLabel, resultNote })') : null; } catch {}
    let lines = null;
    if (P) { // four identical status lines in a row: one with x4; a different one starts a new line
      let st = null; const out = []; for (const t of ['Using A', 'Using A', 'Using A', 'Using A', 'Done']) { st = P.sysCollapse(st, t, false); if (st.merged) out[out.length - 1] = P.sysLabel(st); else out.push(P.sysLabel(st)); }
      lines = out;
    }
    const wired = /runRes\.seen\.has\(rkey\)/.test(page347) && /runRes\.el\.textContent = note\.text/.test(page347) && /runRes = \{ id: '', seen: new Set\(\)/.test(page347) && /function sys\(text, bad\)[\s\S]{0,200}sysCollapse\(/.test(page347);
    check('R348', 'the page shows one Done line per run: a result seen before (same uuid, a replay or a second stream) is skipped, the next result of the same run updates that line (with a turn count), a new run starts its own; identical status lines in a row collapse into one with a ×N count [static: the page\'s own functions run on their source]',
      !!P && JSON.stringify(lines) === JSON.stringify(['Using A ×4', 'Done']) && P.sysLabel({ text: 'x', n: 1 }) === 'x' && wired, `lines ${JSON.stringify(lines)}; wired ${wired}`);
    let why = null;
    if (P) {
      const hv = (o) => ({ v: 1, kind: 'result', error: false, text: 'ok', subtype: 'success', durationMs: 1, ...o });
      const o = (x) => ({ model: 'Opus 5.5', turns: 1, out: 0, ...x });
      why = [P.resultNote(hv({}), o({ out: 1 })).text, P.resultNote(hv({}), o({ interrupted: true })).text, P.resultNote(hv({}), o({ stopped: true })).text, P.resultNote(hv({ subtype: 'error_during_execution' }), o({})).text,
        P.resultNote(hv({ text: '' }), o({ stderr: 'boom\nno conversation found' })).text, P.resultNote(hv({ text: '' }), o({})).text, P.resultNote(hv({}), o({ out: 1, turns: 3, model: '' })).text, P.resultNote(hv({ error: true, text: 'bad' }), o({})).text];
    }
    check('R349', 'a run that ended at once without output says why: interrupted by Send now, you stopped it, the result subtype, the stderr tail, nothing to resume; a normal reply says "Done in Ns · model" (and the turns when there are several); an error says what it was [static: the page\'s own function run on its source]',
      !!why && why[0] === 'Done in 0s · Opus 5.5' && why[1] === 'Ended at once: interrupted by Send now' && why[2] === 'Ended at once: you stopped it' && why[3] === 'Ended at once: error_during_execution'
      && why[4] === 'Ended at once: boom no conversation found' && why[5] === 'Ended at once: nothing to resume or answer' && why[6] === 'Done in 0s · 3 turns' && why[7] === 'Claude stopped with an error: bad', `${JSON.stringify(why)}`);
  }
  { // R350 to R354 (T-320): screen sharing in calls. The hub half runs on the real hub; the page half runs web/calls.js on its own source with
    // stubs (a browser cannot run here), so the share logic is exercised for real: double click, leaving while the picker is open, the browser's
    // "Stop sharing" bar, the tile rule, the quiet re-join, a quick leave + join, and the sa gating between an old page and a new one.
    const roomOf = async (tok) => (((await call('/api/call/poll?after=0&ver=-1', tok)).data.calls || {}).room || []);
    const whoU = async () => (await roomOf(A)).find((p) => p.user === 't-user') || {};
    await call('/api/call/leave', A, {}); await call('/api/call/leave', U, {});
    await call('/api/call/join', U, { conv: 'room', cam: false, mic: true, screen: true });
    const u1 = await whoU();
    await call('/api/call/join', A, { conv: 'room', cam: false, mic: true });
    await call('/api/call/join', U, { conv: 'room', cam: false, mic: true, screen: true }); // the page's quiet re-join after a hub restart: the share flag comes with it
    const u2 = await whoU();
    await call('/api/call/leave', U, {}); await sleep(30);
    await call('/api/call/join', U, { conv: 'room', cam: false, mic: true });
    const u3 = await whoU();
    const bigSig = (n) => ({ iv: toB64(rand(12)), ct: toB64(rand(n)) });
    const sigOk = await call('/api/call/signal', A, { to: 't-user', conv: 'room', ...bigSig(150000) }), sigBig = await call('/api/call/signal', A, { to: 't-user', conv: 'room', ...bigSig(150003) });
    await call('/api/call/leave', A, {}); await call('/api/call/leave', U, {});
    check('R350', 'T-320 calls, hub side: a join can carry the share flag (the page re-joins with it after a hub restart), a re-join keeps the person\'s `since` and a leave + join makes a new one (the page rebuilds the connection from that), and a signal of 200000 characters goes through while 200004 is refused',
      u1.screen === true && u2.screen === true && !!u1.since && u1.since === u2.since && u3.since && u3.since !== u1.since && u3.screen === false && sigOk.status === 200 && sigBig.status === 400,
      `screen ${u1.screen}/${u2.screen}/${u3.screen}; since same ${u1.since === u2.since} new ${u3.since !== u1.since}; signal ${sigOk.status}/${sigBig.status}`);

    const callsSrc = fs.readFileSync(path.join(MILLS, 'web', 'calls.js'), 'utf8'), vm = require('vm');
    const hm = callsSrc.match(/\/\/ <t320>[^\n]*\n([\s\S]*?)\/\/ <\/t320>/);
    let H = null; try { H = hm ? vm.runInNewContext(hm[1] + '; ({ shareErrorText, shareTileWanted, peerNeedsRebuild, retryableSignal })') : null; } catch {}
    const er = (name, message) => ({ name, message: message || name });
    check('R351', 'T-320 the page says why a share failed (refused / cancelled, nothing to share and the Linux portal, capture error, rejected options, plain http), a share tile shows while the hub says the person shares and the track has not ended (also while it is muted), a person who left and joined again gets a new connection unless the hub restarted, and a lost set-up signal is retried only when it may be transient [the helpers run on their own source]',
      !!H && /cancelled or blocked/.test(H.shareErrorText(er('NotAllowedError'), false)) && /not allowed to capture/.test(H.shareErrorText(er('NotAllowedError'), true)) && /portal/.test(H.shareErrorText(er('NotFoundError'), false))
      && /could not be captured \(boom\)/.test(H.shareErrorText(er('NotReadableError', 'boom'), false)) && /did not accept/.test(H.shareErrorText(er('TypeError', 'bad'), false)) && /secure address/.test(H.shareErrorText(null, false, false)) && /^Screen share: odd$/.test(H.shareErrorText(er('X', 'odd'), false))
      && H.shareTileWanted(true, { readyState: 'live', muted: true }) && !H.shareTileWanted(false, { readyState: 'live' }) && !H.shareTileWanted(true, null) && !H.shareTileWanted(true, { readyState: 'ended' })
      && H.peerNeedsRebuild('a', 'b', false) && !H.peerNeedsRebuild('a', 'b', true) && !H.peerNeedsRebuild('a', 'a', false) && !H.peerNeedsRebuild(null, 'b', false)
      && H.retryableSignal(new Error('Both people must be in this call.')) && H.retryableSignal(new TypeError('Failed to fetch')) && !H.retryableSignal(new Error('Bad signal.')), `helpers ${!!H}`);

    const wired = [/e\.track\.onunmute = \(\) => syncShareTile\(user\); e\.track\.onended = \(\) => syncShareTile\(user\)/, /peer\.shareStream\.addTrack\(e\.track\)/, /syncShareTile\(p\.user\); \}/, /screen: !!call\.screen \}\) \}\); \} catch/, /if \(call\.screen \|\| call\.sharing\) return/,
      /s\.getTracks\(\)\.forEach\(\(t\) => t\.stop\(\)\)/, /for \(let n = 0; n < 4; n\+\+\)/, /try \{ await onSignal\(s\); \}/, /function askApp\(\)/, /\n  askApp\(\);\n  \$\('callScreen'\)\.hidden/, /connectionState === 'disconnected' && peer\.offerer/, /peerNeedsRebuild\(old\.since, p\.since, ourMoved\)/].map((re) => re.test(callsSrc));
    check('R352', 'T-320 [static] calls.js wiring: the remote share tile follows the hub flag (no hide on mute), the share audio is kept on the peer, the re-join carries the share flag, no second capture while the picker is open and a capture that arrives after leaving is stopped, set-up signals are retried and a bad one does not cost the rest, the app answer redraws the buttons, an offerer restarts ICE on "disconnected", a quick leave + join rebuilds the connection; the old hide-on-mute is gone',
      wired.every(Boolean) && !/e\.track\.onmute = hide/.test(callsSrc), `wired ${wired.map((x) => (x ? 1 : 0)).join('')}`);

    // the page's own code, run with stubs
    const { webcrypto } = require('crypto');
    const mk = () => { const kept = {}; return new Proxy(function () {}, { get: (t, k) => (k === 'then' ? undefined : k in kept ? kept[k] : (kept[k] = mk())), set: (t, k, v) => { kept[k] = v; return true; }, apply: () => mk() }); }; // any element, any call
    const posts = [], els = {}, pend = [];
    class MS { constructor(t = []) { this.t = [...t]; } getTracks() { return this.t; } getAudioTracks() { return this.t.filter((x) => x.kind === 'audio'); } getVideoTracks() { return this.t.filter((x) => x.kind === 'video'); } addTrack(x) { this.t.push(x); } removeTrack(x) { this.t = this.t.filter((y) => y !== x); } }
    class PC { constructor() { this.ts = []; this.closed = false; } addTransceiver() { return {}; } getTransceivers() { return this.ts; } close() { this.closed = true; } }
    const ctx = { console, setTimeout, clearTimeout, setInterval: () => 0, Promise, JSON, Date, Math, Uint8Array, Map, Set, Array, Object, String, Number, Error, Symbol, MediaStream: MS, RTCPeerConnection: PC, RTCRtpReceiver: {}, crypto: webcrypto, CSS: { escape: (x) => x }, enc, dec, toB64, fromB64,
      $: (id) => els[id] || (els[id] = mk()), json: async (p, o) => { posts.push([p, o && o.body ? JSON.parse(o.body) : null]); return { calls: {}, iceServers: [] }; },
      store: { get: () => null, set() {} }, prefs: () => ({ audio: {}, call: { quality: 'medium', volume: 100 }, sound: {}, tts: {} }), me: { username: 'alice' }, nickOf: (u) => u, colorOf: () => '#000', ic: () => mk(), label() {}, layout: { show: {} },
      sysRoom() {}, playTone() {}, speak() {}, sleep: (ms) => new Promise((r) => setTimeout(r, ms)), convLabel: (c) => c, currentConv: 'room', chatPriv: {}, token: '', base: '', memberOf: () => null, changedKeys: new Set(), trustedPub: () => true, sharedWith: async () => null,
      document: { createElement: () => mk(), addEventListener() {}, querySelector: () => null }, addEventListener() {}, navigator: { mediaDevices: { getDisplayMedia: () => new Promise((res) => pend.push(res)) } }, isSecureContext: true };
    ctx.window = ctx;
    let S = null, e353 = '';
    try { S = vm.runInNewContext(callsSrc + '\n;({ call, startShare, onCallState, onSignal, syncShareTile })', ctx); } catch (e) { e353 = 'load: ' + e.message; }
    const track = (kind) => ({ kind, readyState: 'live', enabled: true, stopped: false, stop() { this.stopped = true; this.readyState = 'ended'; } });
    const res = {};
    if (S) try {
      S.call.conv = 'room';
      // a double click while the picker is open starts one capture only; the picture goes out and the hub is told
      const p1 = S.startShare('none'), p2 = S.startShare('none'); res.oneCapture = pend.length === 1;
      const s1 = new MS([track('video')]); pend[0](s1); await p1; await p2;
      res.sharing = S.call.screen === s1.t[0] && posts.some(([p, b]) => p === '/api/call/state' && b.screen === true) && S.call.screenStream.getTracks().includes(s1.t[0]);
      // the browser's "Stop sharing" bar ends the track: the state follows
      const vt = S.call.screen; posts.length = 0; vt.onended(); await ctx.sleep(30);
      res.stoppedByBar = S.call.screen === null && vt.stopped && posts.some(([p, b]) => p === '/api/call/state' && b.screen === false) && !S.call.screenStream.getTracks().includes(vt);
      // leaving while the picker is open: the capture that arrives later is stopped (its light goes off), nothing is shared
      pend.length = 0; S.call.conv = 'room'; const p3 = S.startShare('none'); S.call.conv = null; const s2 = new MS([track('video')]); pend[0](s2); await p3;
      res.leftFirst = s2.t[0].stopped === true && S.call.screen === null && S.call.sharing === false;
      // a refusal in the app and a missing source say different things
      S.call.conv = 'room'; ctx.navigator.mediaDevices.getDisplayMedia = () => Promise.reject(er('NotFoundError')); await S.startShare('none'); res.msg = /nothing to share/.test(String(els.callMsg.textContent)); res.free = S.call.sharing === false;
      // the share tile rule on a peer's track
      const log = []; ctx.showScreenTile = (u) => log.push('show:' + u); ctx.removeScreenTile = (u) => log.push('rm:' + u);
      const pr = { shareTrack: { readyState: 'live', muted: true }, shareStream: {} }; S.call.peers.set('bob', pr);
      S.call.state = { room: [{ user: 'bob', screen: true }] }; S.syncShareTile('bob');
      S.call.state = { room: [{ user: 'bob', screen: false }] }; S.syncShareTile('bob');
      S.call.state = { room: [{ user: 'bob', screen: true }] }; pr.shareTrack = { readyState: 'ended' }; S.syncShareTile('bob');
      res.tile = JSON.stringify(log) === JSON.stringify(['show:bob', 'rm:bob', 'rm:bob']); S.call.peers.delete('bob');
      // the quiet re-join after a restart carries what we share
      S.call.screen = track('video'); S.call.state = {}; posts.length = 0;
      await S.onCallState({ room: [{ user: 'bob', since: 'b1' }] });
      res.rejoin = posts.some(([p, b]) => p === '/api/call/join' && b.screen === true);
      // bob left and joined again between two polls (our own since unchanged): a new connection; the hub restarted (ours moved too): kept
      await S.onCallState({ room: [{ user: 'alice', since: 'a1' }, { user: 'bob', since: 'b1' }] });
      const first = S.call.peers.get('bob');
      await S.onCallState({ room: [{ user: 'alice', since: 'a1' }, { user: 'bob', since: 'b2' }] });
      const second = S.call.peers.get('bob');
      await S.onCallState({ room: [{ user: 'alice', since: 'a2' }, { user: 'bob', since: 'b3' }] });
      res.rebuild = first.pc.closed === true && second !== first && S.call.peers.get('bob') === second && second.pc.closed === false;
    } catch (e) { e353 = String(e && e.stack || e); }
    check('R353', 'T-320 the share flow, calls.js run with stubs: a double click opens one picker, the picture goes out and the hub is told; the browser\'s Stop sharing bar ends it and the hub is told; a capture that arrives after leaving is stopped; a refusal says what it was; a peer\'s share tile shows while the hub says they share (muted or not) and goes with the flag or the end of the track; the quiet re-join carries the share flag; a quick leave + join rebuilds the connection but a hub restart does not',
      !!S && Object.values(res).length === 9 && Object.values(res).every(Boolean), e353 || JSON.stringify(res));

    // sa gating: an up-to-date page's answer turns the share-audio line on (one more offer); an older page's answer leaves it inactive
    const saRes = {};
    if (S) try {
      const key = await webcrypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']); ctx.pairKey = async () => key;
      const mkT = (kind, dir) => ({ direction: dir, receiver: { track: { kind } }, sender: { setStreams() {}, replaceTrack: async () => {} } });
      const fpc = { signalingState: 'stable', remoteDescription: null, setRemoteDescription: async function (d) { this.remoteDescription = d; }, addIceCandidate: async () => {}, getTransceivers() { return this.ts; }, ts: [mkT('audio', 'sendrecv'), mkT('video', 'sendrecv'), mkT('video', 'sendrecv'), mkT('audio', 'inactive')] };
      const fpeer = { pc: fpc, user: 'bob', offerer: true, pendingIce: [] }; S.call.peers.set('bob', fpeer); S.call.conv = 'room';
      const sig = async (body) => { const iv = webcrypto.getRandomValues(new Uint8Array(12)); const ct = await webcrypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, enc.encode(JSON.stringify({ conv: 'room', from: 'bob', to: 'alice', ...body }))); return { conv: 'room', from: 'bob', iv: toB64(iv), ct: toB64(ct) }; };
      await S.onSignal(await sig({ d: { type: 'answer', sdp: 'x' } }));
      saRes.oldPage = fpc.ts[3].direction === 'inactive' && !fpeer.saOn;
      await S.onSignal(await sig({ sa: 1, d: { type: 'answer', sdp: 'x' } }));
      saRes.newPage = fpc.ts[3].direction === 'sendrecv' && fpeer.saOn === true && fpc.ts[2].direction === 'sendrecv';
    } catch (e) { saRes.err = String(e && e.stack || e); }
    check('R354', 'T-320 old page and new page together: the share-audio line stays inactive after an answer from a page that does not say sa, and is turned on (one more offer) after an answer that does; the page\'s own signal code ran on a real AES-GCM message',
      saRes.oldPage === true && saRes.newPage === true, JSON.stringify(saRes));
  }
  { // R170 (5.1.0): PGM Gateway: an admin registers a product (its secret only in a private file), the product's signed
    // requests (pgm-net-internal's product link) check a VibeRoller login (the sign-in limits apply), link that person,
    // and use the AI hosts / providers as them; unsigned, replayed, re-pathed, proxied, unlinked and non-admin all refused
    // [static] the admin's Gateway products box (index.html) and the board kit's options (web/board.js, shared with the
    // products through pgm-libs): VibeRoller sets none, so it keeps its Claude buttons
    const page170 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), board170 = fs.readFileSync(path.join(MILLS, 'web', 'board.js'), 'utf8');
    const stat170 = /async function loadGateway\(\)/.test(page170) && /id="gwBox" hidden/.test(page170) && /adminCall\('gateway\/products\/remove'/.test(page170)
      && /Object\.assign\(\{ ai: true, product: 'VibeRoller' \}, window\.BOARD_OPTS \|\| \{\}\)/.test(board170) && !/BOARD_OPTS\s*=/.test(page170)
      // the page as a product's shell (5.2.0): PRODUCT from the placeholder, everything stored under NS, the base path, only its screens
      && /const PRODUCT = \(\(\) => \{ const v = '__MILLS_PRODUCT__';/.test(page170) && /const NS = PRODUCT \? PRODUCT\.id \+ '\.' : '';/.test(page170) && /const TOKEN_KEY = NS \+ 'mills\.token';/.test(page170)
      && /localStorage\.getItem\(NS \+ k\)/.test(page170) && /indexedDB\.open\(NS \+ 'mills', 1\)/.test(page170) && !/localStorage\.(get|set)Item\('mills/.test(page170) && !/sessionStorage\.(get|set)Item\('/.test(page170)
      && /return location\.origin \+ \(PRODUCT \? PRODUCT\.base : ''\);/.test(page170) && /if \(PRODUCT && !\['menu', 'admin', \.\.\.PRODUCT\.screens\]\.includes\(name\)\) name = 'menu';/.test(page170)
      && (await call('/')).status === 200 && /__MILLS_PRODUCT__/.test(await fetch(H + '/').then((r) => r.text())); // the hub leaves the placeholder: PRODUCT stays null here
    if (RUST_HUB) {
      const notAdmin = await call('/api/admin/gateway/products', U, { id: 'prodsuit', name: 'PGM Prod Suit' });
      const made = await call('/api/admin/gateway/products', A, { id: 'prodsuit', name: 'PGM Prod Suit', ai: true });
      const badId = await call('/api/admin/gateway/products', A, { id: 'Bad Id', name: 'x' });
      const secFile = path.join(SB, 'gateway', 'prodsuit.secret');
      const secret = fs.existsSync(secFile) ? fs.readFileSync(secFile, 'utf8').trim() : '';
      const mode = fs.existsSync(secFile) ? (fs.statSync(secFile).mode & 0o777) : 0;
      const listed = (await call('/api/admin/gateway/products', A)).data || {};
      const sign = (method, pq, user, body, ts = Date.now(), nonce = crypto.randomBytes(16).toString('hex'), sec = secret) => {
        const sha = crypto.createHash('sha256').update(body || '').digest('hex');
        const sig = crypto.createHmac('sha256', sec).update(`pgm-product-v1\n${method}\n${pq}\n${ts}\n${nonce}\n${user}\n${sha}`).digest('hex');
        return { 'x-pgm-product': 'prodsuit', 'x-pgm-ts': String(ts), 'x-pgm-nonce': nonce, 'x-pgm-user': user, 'x-pgm-sig': sig };
      };
      const gw = async (method, pq, user, bodyObj, hs) => { const body = bodyObj === undefined ? '' : JSON.stringify(bodyObj); const r = await fetch(H + pq, { method, headers: { 'Content-Type': 'application/json', ...(hs || sign(method, pq, user, body)) }, body: body || undefined }); return { status: r.status, text: await r.text() }; };
      const status = await gw('GET', '/internal/gateway/status', '');
      const unsigned = await fetch(H + '/internal/gateway/status').then((r) => r.status);
      const replayHs = sign('GET', '/internal/gateway/status', '');
      const once = await gw('GET', '/internal/gateway/status', '', undefined, replayHs), twice = await gw('GET', '/internal/gateway/status', '', undefined, replayHs);
      const moved = await gw('GET', '/internal/gateway/identity/profile', 't-user', undefined, sign('GET', '/internal/gateway/status', 't-user'));
      const proxied = await gw('GET', '/internal/gateway/status', '', undefined, { ...sign('GET', '/internal/gateway/status', ''), 'X-Forwarded-For': '198.51.100.7' });
      const old = await gw('GET', '/internal/gateway/status', '', undefined, sign('GET', '/internal/gateway/status', '', '', Date.now() - 120000));
      const wrongKey = await gw('GET', '/internal/gateway/status', '', undefined, sign('GET', '/internal/gateway/status', '', '', Date.now(), crypto.randomBytes(16).toString('hex'), 'x'.repeat(64)));
      const beforeLink = await gw('GET', '/internal/gateway/identity/profile', 't-user');
      const aiBefore = await gw('POST', '/internal/gateway/v1/chat/completions', 't-user', { messages: [{ role: 'user', content: 'gw-r170' }] });
      const wrongPw = await gw('POST', '/internal/gateway/identity/verify', '', { username: 't-user', password: 'not-it-r170', ip: '198.51.100.70' });
      const verified = await gw('POST', '/internal/gateway/identity/verify', '', { username: 'T-User', password: 'pw-t-user-0001', ip: '198.51.100.71' });
      // the same limits as a sign-in: five wrong passwords for a name from an address block it there
      for (let i = 0; i < 5; i++) await gw('POST', '/internal/gateway/identity/verify', '', { username: 't-admin', password: 'guess-r170-' + i, ip: '198.51.100.72' });
      const blocked = await gw('POST', '/internal/gateway/identity/verify', '', { username: 't-admin', password: 'pw-t-admin-0001', ip: '198.51.100.72' });
      const profile = await gw('GET', '/internal/gateway/identity/profile', 't-user');
      const provs = await gw('GET', '/internal/gateway/ai/providers', 't-user');
      const ai = await gw('POST', '/internal/gateway/v1/chat/completions', 't-user', { messages: [{ role: 'user', content: 'gw-r170' }] });
      const aiText = (((JSON.parse(ai.text || '{}').choices || [])[0] || {}).message || {}).content || '';
      const linksFile = path.join(SB, 'gateway', 'links', 'prodsuit.json');
      const links = fs.existsSync(linksFile) ? JSON.parse(fs.readFileSync(linksFile, 'utf8')) : {};
      // a product served through the hub at /p/<id>/ (5.2.0): a stand-in product on a port of its own
      const fakeProd = require('http').createServer((req, res) => { let body = ''; req.on('data', (c) => { body += c; }); req.on('end', () => { res.writeHead(200, { 'content-type': 'application/json', 'x-prod': 'yes' }); res.end(JSON.stringify({ path: req.url, method: req.method, body, h: { host: req.headers.host, xff: req.headers['x-forwarded-for'], prefix: req.headers['x-forwarded-prefix'], proto: req.headers['x-forwarded-proto'], vr: req.headers['x-vr-user'] || '', pgm: req.headers['x-pgm-user'] || '', gw: req.headers['x-pgm-gateway'] || '', auth: req.headers.authorization || '' } })); }); });
      const prodPort = await freePort(PORT + 40); await new Promise((r) => fakeProd.listen(prodPort, '127.0.0.1', r));
      const withPort = await call('/api/admin/gateway/products', A, { id: 'fakeprod', name: 'Fake product', port: prodPort });
      const badPort = await call('/api/admin/gateway/products', A, { id: 'badport', name: 'x', port: 70000 });
      const noSlash = await fetch(H + '/p/fakeprod', { redirect: 'manual' });
      const viaGet = await fetch(H + '/p/fakeprod/a/b?x=1', { headers: { 'x-vr-user': 't-admin', 'x-pgm-user': 't-admin', 'x-forwarded-for': '203.0.113.9', Authorization: 'Bearer abc' } });
      const viaGetBody = await viaGet.json().catch(() => ({}));
      const viaPost = await fetch(H + '/p/fakeprod/api/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{"u":1}' }).then((r) => r.json()).catch(() => ({}));
      const viaInternal = await fetch(H + '/p/fakeprod/internal/gateway/status').then((r) => r.status);
      const viaFront = await fetch(H + '/p/fakeprod/__front/x').then((r) => r.status);
      const unknownProd = await fetch(H + '/p/nothere/').then((r) => r.status);
      const noPortProd = await fetch(H + '/p/prodsuit/').then((r) => r.status);
      await new Promise((r) => fakeProd.close(r));
      const prodDown = await fetch(H + '/p/fakeprod/').then((r) => r.status);
      await call('/api/admin/gateway/products/remove', A, { id: 'fakeprod' });
      const served = withPort.status === 200 && (withPort.data.product || {}).path === '/p/fakeprod/' && badPort.status === 400 && noSlash.status === 302 && noSlash.headers.get('location') === '/p/fakeprod/'
        && viaGet.status === 200 && viaGet.headers.get('x-prod') === 'yes' && viaGetBody.path === '/a/b?x=1' && viaGetBody.h.prefix === '/p/fakeprod' && viaGetBody.h.vr === '' && viaGetBody.h.pgm === '' && viaGetBody.h.auth === 'Bearer abc' && viaGetBody.h.xff === '127.0.0.1' && /^\d+\.\d+\.\d+/.test(viaGetBody.h.gw)
        && viaPost.method === 'POST' && viaPost.body === '{"u":1}' && viaPost.path === '/api/login' && viaInternal === 404 && viaFront === 404 && unknownProd === 404 && noPortProd === 404 && prodDown === 503;
      // R474 (T-207, spec 15): Hyper Workflows through the gateway for a product's people: the same paths as /api/hyperwork/,
      // as the person the product signed for (linked: t-user; not linked: t-admin → 404); the log streams; run-only honoured
      {
        // a product's person designs through the gateway too (Hyper Prod Man's editor): save + publish a workflow of their own
        const doc474 = { schema: 1, name: 'r474-flow', nodes: [{ id: 's', kind: 'start' }, { id: 'p', kind: 'prompt', config: { user: 'gateway {{inputs.prompt}}' } }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'p.in' }, { id: 'e2', from: 'p.text', to: 'e.result' }] };
        const gwSave = await gw('POST', '/internal/gateway/hyperwork/save', 't-user', { doc: doc474 });
        const gwId = (() => { try { return JSON.parse(gwSave.text).id || ''; } catch { return ''; } })();
        const gwPub = gwId ? await gw('POST', '/internal/gateway/hyperwork/publish', 't-user', { id: gwId, message: 'r474' }) : { status: 0, text: '' };
        const gwList = await gw('GET', '/internal/gateway/hyperwork/list?q=r474', 't-user');
        const gwItems = (() => { try { return JSON.parse(gwList.text).items || []; } catch { return []; } })();
        const notLinked = await gw('GET', '/internal/gateway/hyperwork/list', 't-admin');
        const gwCmd = await gw('POST', '/internal/gateway/hyperwork/command', 't-user', { line: '/hyperwork r474-flow through the gateway', project: 'VibeRoller' });
        const gwRun = (() => { try { return JSON.parse(gwCmd.text).run || ''; } catch { return ''; } })();
        let gwLines = [];
        if (gwRun) { try { const pq = `/internal/gateway/hyperwork/log?id=${gwRun}&from=0&follow=1`; const r = await fetch(H + pq, { headers: sign('GET', pq, 't-user', ''), signal: AbortSignal.timeout(60000) }); gwLines = (await r.text()).split('\n').filter(Boolean).map((l) => JSON.parse(l)); } catch (e) { gwLines = [{ error: e.message }]; } }
        const gwMeta = gwRun ? await gw('GET', '/internal/gateway/hyperwork/run?id=' + gwRun, 't-user') : { status: 0, text: '' };
        const ro = await (async () => { const pq = '/internal/gateway/hyperwork/save', body = JSON.stringify({ doc: { schema: 1, name: 'r474-ro', nodes: [{ id: 's', kind: 'start' }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'e.result' }] } }); const r = await fetch(H + pq, { method: 'POST', headers: { 'Content-Type': 'application/json', 'x-vr-client': 'run-only', ...sign('POST', pq, 't-user', body) }, body }); return r.status; })();
        const gwUnknown = await gw('GET', '/internal/gateway/hyperwork/zzz', 't-user');
        const gwTypes = gwLines.map((e) => e.type);
        check('R474', 'T-207 Hyper Workflows through the gateway (spec 15): a product\'s signed request as a linked person saves and publishes a workflow (POST hyperwork/save, hyperwork/publish), lists it (GET hyperwork/list), runs a /hyperwork line (POST hyperwork/command → 202) against a project, follows the run (hyperwork/log streams NDJSON to run_end) and reads it (hyperwork/run); someone the product has not linked gets 404; a product that says x-vr-client: run-only cannot save (403); an unknown path is 404',
          gwSave.status === 200 && !!gwId && gwPub.status === 200 && gwList.status === 200 && gwItems.some((i) => i.name === 'r474-flow') && notLinked.status === 404 && gwCmd.status === 202 && !!gwRun && gwTypes[0] === 'run_start' && gwTypes[gwTypes.length - 1] === 'run_end' && gwMeta.status === 200 && /"status":"ok"/.test(gwMeta.text) && ro === 403 && gwUnknown.status === 404,
          `save ${gwSave.status} publish ${gwPub.status}; list ${gwList.status} ${gwItems.map((i) => i.name).join(',')}; not linked ${notLinked.status}; command ${gwCmd.status} ${gwCmd.text.slice(0, 80)}; log ${gwTypes.join(' ')}; run ${gwMeta.status} ${gwMeta.text.slice(0, 80)}; run-only save ${ro}; unknown ${gwUnknown.status}`);
      }
      // a new secret: the old one stops at once; removing the product ends its links
      await call('/api/admin/gateway/products', A, { id: 'prodsuit', name: 'PGM Prod Suit', ai: true });
      const oldSecret = await gw('GET', '/internal/gateway/status', '');
      const removed = await call('/api/admin/gateway/products/remove', A, { id: 'prodsuit' });
      const gone = !fs.existsSync(secFile) && !fs.existsSync(linksFile);
      const ignored = spawnSync('git', ['check-ignore', '-q', 'bridge/gateway/products.json'], { cwd: MILLS }).status === 0;
      check('R170', 'PGM Gateway: an admin registers a product (secret only in an owner-only file, never in an answer); its signed requests check a VibeRoller login (the sign-in limits apply), link that person, and reach the AI hosts / providers as them; unsigned, replayed, moved to another path, proxied, too old, wrong secret, not linked, not admin, a renewed secret and a removed product are all refused; its data stays out of git',
        notAdmin.status === 403 && made.status === 200 && (made.data.product || {}).id === 'prodsuit' && !JSON.stringify(made.data).includes(secret) && badId.status === 400 && /^[0-9a-f]{64}$/.test(secret) && (process.platform === 'win32' || mode === 0o600)
        && (listed.products || []).some((p) => p.id === 'prodsuit' && p.ai === true) && !JSON.stringify(listed).includes(secret)
        && status.status === 200 && /PGM Gateway/.test(status.text) && unsigned === 403 && once.status === 200 && twice.status === 403 && moved.status === 403 && proxied.status === 403 && old.status === 403 && wrongKey.status === 403
        && beforeLink.status === 404 && aiBefore.status === 404 && wrongPw.status === 401 && verified.status === 200 && /"username":"t-user"/.test(verified.text) && blocked.status === 429
        && profile.status === 200 && /"username":"t-user"/.test(profile.text) && provs.status === 200 && /"providers":\[/.test(provs.text) && !/apiKey|configDir|baseUrl/.test(provs.text)
        && ai.status === 200 && /gw-r170/.test(aiText) && !!links['t-user'] && !links['t-admin']
        && oldSecret.status === 403 && removed.status === 200 && gone && ignored && stat170 && served,
        `static ${stat170}; served ${served} (${withPort.status} ${JSON.stringify(withPort.data).slice(0, 100)} badPort ${badPort.status} noSlash ${noSlash.status} ${noSlash.headers.get('location')} get ${viaGet.status} ${JSON.stringify(viaGetBody).slice(0, 300)} post ${JSON.stringify(viaPost).slice(0, 120)} internal ${viaInternal} front ${viaFront} unknown ${unknownProd} noPort ${noPortProd} down ${prodDown}); notAdmin ${notAdmin.status} made ${made.status} ${JSON.stringify(made.data).slice(0, 120)} badId ${badId.status} secret ${secret.length} mode ${mode.toString(8)}; status ${status.status} unsigned ${unsigned} once ${once.status} twice ${twice.status} moved ${moved.status} proxied ${proxied.status} old ${old.status} wrongKey ${wrongKey.status}; before ${beforeLink.status}/${aiBefore.status} wrongPw ${wrongPw.status} verified ${verified.status} ${verified.text.slice(0, 80)} blocked ${blocked.status}; profile ${profile.status} provs ${provs.status} ${provs.text.slice(0, 120)}; ai ${ai.status} "${aiText.slice(0, 80)}" ${ai.text.slice(0, 120)}; links ${JSON.stringify(links).slice(0, 80)}; oldSecret ${oldSecret.status} removed ${removed.status} gone ${gone} ignored ${ignored}`);
    } else check('R170', 'PGM Gateway: product data stays out of git; [static] the admin\'s products box, the board kit\'s options (VibeRoller keeps its Claude buttons) [the gateway is the Rust hub\'s: --rust-hub runs it]', spawnSync('git', ['check-ignore', '-q', 'bridge/gateway/products.json'], { cwd: MILLS }).status === 0 && stat170, `static ${stat170}`);
  }
  { // R346 (T-317): PGM Gateway carries tools: a product's chat completion with `tools` reaches the provider as it is, the provider's
    // tool call comes back as tool_calls (finish_reason tool_calls), the follow-up with the assistant's call and a tool message
    // gets the final text; streamed too; a request without tools is the old one; a provider that cannot carry tools says 400
    if (RUST_HUB) {
      const made = await call('/api/admin/gateway/products', A, { id: 'toolsprod', name: 'Tools product', ai: true });
      const secFile = path.join(SB, 'gateway', 'toolsprod.secret');
      const secret = fs.existsSync(secFile) ? fs.readFileSync(secFile, 'utf8').trim() : '';
      const gw = async (method, pq, user, bodyObj) => {
        const body = bodyObj === undefined ? '' : JSON.stringify(bodyObj), ts = Date.now(), nonce = crypto.randomBytes(16).toString('hex');
        const sha = crypto.createHash('sha256').update(body).digest('hex');
        const sig = crypto.createHmac('sha256', secret).update(`pgm-product-v1\n${method}\n${pq}\n${ts}\n${nonce}\n${user}\n${sha}`).digest('hex');
        const r = await fetch(H + pq, { method, headers: { 'Content-Type': 'application/json', 'x-pgm-product': 'toolsprod', 'x-pgm-ts': String(ts), 'x-pgm-nonce': nonce, 'x-pgm-user': user, 'x-pgm-sig': sig }, body: body || undefined });
        const text = await r.text(); let data = {}; try { data = JSON.parse(text); } catch {}
        return { status: r.status, text, data };
      };
      const chatPath = '/internal/gateway/v1/chat/completions';
      const verified = await gw('POST', '/internal/gateway/identity/verify', '', { username: 't-admin', password: 'pw-t-admin-0001', ip: '198.51.100.146' });
      // an OpenAI-compatible provider of the admin's (the way R112 adds one: a host of theirs says hello for a moment)
      const th = (await call('/api/hosts/new', A, {})).data.host || {}, thc = (await call('/api/hosts/config', A, { id: th.id })).data.config || {};
      await fetch(H + '/node/hello', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Host': th.id, 'X-VR-Host-Token': thc.token }, body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex') }) });
      const prov = (await call('/api/providers/add', A, { kind: 'openai', label: 'R346 endpoint', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, apiKey: 'k', models: 'fake-gpt', shared: false })).data;
      await call('/api/hosts/remove', A, { id: th.id });
      await call('/api/apikey/rotate', A, {});
      const setP = await call('/api/apikey/settings', A, { provider: prov.provider && prov.provider.id, model: 'fake-gpt' });
      const tools = [{ type: 'function', function: { name: 'lookup', description: 'Look a thing up', parameters: { type: 'object', properties: { q: { type: 'string' }, n: { type: 'number' } } } } }];
      const before = fakeOaiTools.length;
      const plain = await gw('POST', chatPath, 't-admin', { messages: [{ role: 'user', content: 'plain-r346' }] });
      const plainText = (((plain.data.choices || [])[0] || {}).message || {}).content || '';
      const first = await gw('POST', chatPath, 't-admin', { messages: [{ role: 'user', content: 'find it' }], tools, tool_choice: 'auto' });
      const c0 = (first.data.choices || [])[0] || {}, call0 = ((c0.message || {}).tool_calls || [])[0] || {};
      const second = await gw('POST', chatPath, 't-admin', { tools, messages: [{ role: 'user', content: 'find it' }, { role: 'assistant', content: null, tool_calls: [call0] }, { role: 'tool', tool_call_id: call0.id, content: 'forty-two' }] });
      const c1 = (second.data.choices || [])[0] || {};
      const streamed = await gw('POST', chatPath, 't-admin', { stream: true, messages: [{ role: 'user', content: 'find it' }], tools });
      const evs = streamed.text.split('\n').filter((l) => l.startsWith('data: ') && l !== 'data: [DONE]').map((l) => { try { return JSON.parse(l.slice(6)); } catch { return {}; } });
      const sCall = evs.map((e) => ((e.choices || [])[0] || {}).delta || {}).flatMap((d) => d.tool_calls || [])[0] || {};
      const sFinish = evs.map((e) => ((e.choices || [])[0] || {}).finish_reason).filter(Boolean).pop();
      const bad = await gw('POST', chatPath, 't-admin', { messages: [{ role: 'user', content: 'x' }], tools: [{ type: 'function' }] });
      const gotTools = fakeOaiTools.slice(before);
      await call('/api/apikey/revoke', A, {}); // back to the hub's own default (a Claude CLI answers text only)
      const noTools = await gw('POST', chatPath, 't-admin', { messages: [{ role: 'user', content: 'x' }], tools });
      const textStill = await gw('POST', chatPath, 't-admin', { messages: [{ role: 'user', content: 'gw-r346' }] });
      await call('/api/providers/remove', A, { id: prov.provider && prov.provider.id });
      await call('/api/admin/gateway/products/remove', A, { id: 'toolsprod' });
      check('R346', 'T-317 PGM Gateway carries tools: a product\'s chat completion with tools and tool_choice reaches an OpenAI-compatible provider as it is (and a request without tools is the old text one); the provider\'s call comes back as message.tool_calls (id, name, arguments as JSON text) with finish_reason tool_calls and no content; the follow-up with the assistant\'s call and a tool message gets the final text (finish_reason stop) and the provider saw the tool message with the same id; streamed, the call arrives in delta.tool_calls and the stream ends with finish_reason tool_calls and [DONE]; a malformed tool is 400; a provider that cannot carry tools (a Claude CLI) answers 400 "cannot carry tools" while text still works',
        made.status === 200 && verified.status === 200 && setP.status === 200
        && plain.status === 200 && /oai says: plain-r346/.test(plainText)
        && first.status === 200 && c0.finish_reason === 'tool_calls' && c0.message.content === null && call0.type === 'function' && call0.id === 'call_r346' && call0.function.name === 'lookup' && JSON.parse(call0.function.arguments).q === 'r346'
        && second.status === 200 && c1.finish_reason === 'stop' && /oai says: tool said forty-two/.test((c1.message || {}).content || '')
        && gotTools.length === 3 && gotTools[0].tools[0].function.name === 'lookup' && gotTools[0].tool_choice === 'auto' && gotTools[1].messages[2].role === 'tool' && gotTools[1].messages[2].tool_call_id === 'call_r346' && gotTools[1].messages[1].tool_calls[0].id === 'call_r346'
        && streamed.status === 200 && sCall.function && sCall.function.name === 'lookup' && sCall.index === 0 && sFinish === 'tool_calls' && /data: \[DONE\]/.test(streamed.text)
        && bad.status === 400 && noTools.status === 400 && /cannot carry tools/.test(noTools.text) && textStill.status === 200 && /gw-r346/.test(textStill.text),
        `made ${made.status} verified ${verified.status} set ${setP.status}; plain ${plain.status} ${plainText.slice(0, 60)}; first ${first.status} ${first.text.slice(0, 300)}; second ${second.status} ${second.text.slice(0, 200)}; provider got ${gotTools.length}; stream ${streamed.status} ${streamed.text.slice(0, 300)}; bad ${bad.status}; noTools ${noTools.status} ${noTools.text.slice(0, 120)}; text ${textStill.status}`);
    } else skip('R346', 'PGM Gateway carries tools (T-317)', 'the gateway is the Rust hub\'s: --rust-hub runs it');
  }
  // Persistence & restart (scratch restart.js: graceful flag only)
  const authBefore = sha(path.join(SB, 'profiles', 't-admin', 'auth.json'));
  const pid1 = (await call('/api/status')).data.pid;
  spawnSync(process.execPath, [path.join(SB, 'restart.js')], { cwd: SB });
  let pid2 = pid1;
  for (let i = 0; i < 60 && (pid2 === pid1 || !pid2); i++) { await sleep(250); pid2 = (await call('/api/status')).data.pid; }
  const after = await call('/api/check', U);
  const lockedOut = (await login('t-user', 'pw-t-user-0001', 'device-eeeeeeeeeeeeeeee')).status;
  check('R9', 'graceful restart: new pid, token kept, lock + model kept, flag consumed', pid2 && pid2 !== pid1 && after.status === 200 && after.data.model === 'claude-opus-5-5'
    && lockedOut === 403 && !fs.existsSync(path.join(SB, '.restart')), // (devA joined t-user's devices in R168)
    `pid ${pid1}→${pid2} check ${after.status} model ${after.data && after.data.model} new device ${lockedOut} flag ${fs.existsSync(path.join(SB, '.restart'))}`);
  if (RUST_HUB) { // R175 (FR2): an unreadable data file never stops the hub starting: moved aside as .bad-<time> (kept), defaults, said in the team chat
    const pf175 = path.join(SB, 'gateway', 'products.json'), garbage = '{"products": garbage';
    fs.mkdirSync(path.dirname(pf175), { recursive: true }); fs.writeFileSync(pf175, garbage);
    const seq175 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
    const pidA = (await call('/api/status')).data.pid;
    spawnSync(process.execPath, [path.join(SB, 'restart.js')], { cwd: SB });
    let pidB = pidA;
    for (let i = 0; i < 80 && (pidB === pidA || !pidB); i++) { await sleep(250); try { pidB = (await call('/api/status')).data.pid; } catch { pidB = 0; } }
    const bad175 = fs.readdirSync(path.dirname(pf175)).filter((f) => /^products\.json\.bad-\d{8}-\d{6}$/.test(f));
    const kept175 = bad175.length === 1 && fs.readFileSync(path.join(path.dirname(pf175), bad175[0]), 'utf8') === garbage;
    let fresh175 = false; try { fresh175 = !!JSON.parse(fs.readFileSync(pf175, 'utf8')); } catch { fresh175 = !fs.existsSync(pf175); }
    let said175 = false; // posted as VibeRoller by the chat's notice loop, 3 s after the start: sealed to every member, read here as t-user
    for (let i = 0; i < 40 && !said175; i++) {
      await sleep(500);
      for (const m of ((await call('/api/room/messages?since=' + seq175, U)).data.messages || []).filter((m) => m.from === 'claude' && !m.to)) { const t = await roomRead(restored, m); if (t && t.includes('products.json.bad-')) { said175 = true; break; } }
    }
    const gw175 = await call('/api/admin/gateway/products', A);
    check('R175', 'FR2 bad data never stops the hub starting: an unreadable data file (here bridge/gateway/products.json) is moved aside as <file>.bad-<yyyymmdd-hhmmss> byte for byte, the hub starts with defaults for that part (the gateway answers with no products), nothing overwrites the original silently, and the team chat says what happened',
      !!pidB && pidB !== pidA && kept175 && fresh175 && gw175.status === 200 && said175,
      `restart ${pidA}→${pidB} · moved aside ${bad175.join(',') || 'no'} kept ${kept175} · fresh ${fresh175} · gateway ${gw175.status} · said in chat ${said175}`);
  }
  // R154: a hub restart keeps every AI host's session: an older host that never says hello again (the "old-node" played
  // in R152, a 2.x host) still offers its Claude right after the restart, the way it did before it; a host the hub has
  // no session for is asked to say hello again (428 for hosts that send their version, 401 for older ones: they re-hello
  // on it); every poll answer carries the hub's heartbeat
  {
    const provs = ((await call('/api/check', U)).data.providers || []);
    const oldAfter = provs.find((p) => p.id === 'h:old-node/claude') || {}, newAfter = provs.find((p) => p.id === 'h:test-node/claude') || {};
    const nh = { 'X-VR-Host': 'nohello-node', 'X-VR-Host-Token': OLD_NODE_SECRET + '-nh' };
    const askNew = await fetch(H + '/node/poll', { headers: { ...nh, 'X-VR-Host-Version': '3.2.0' } }), askOld = await fetch(H + '/node/poll', { headers: nh });
    const legacyNoHello = await fetch(H + '/node/poll', { headers: { 'X-VR-Node': 'nohello-node', 'X-VR-Node-Token': OLD_NODE_SECRET + '-nh' }, signal: AbortSignal.timeout(1500) }).then((r) => r.status).catch(() => 'held'); // a 1.x host: keeps long-polling
    const sessFile = (() => { try { return JSON.parse(fs.readFileSync(path.join(SB, 'ai-host-sessions.json'), 'utf8')); } catch { return {}; } })();
    const ign = spawnSync('git', ['check-ignore', '-q', 'bridge/ai-host-sessions.json'], { cwd: MILLS }).status;
    const nodes154 = fs.readFileSync(path.join(BRIDGE, 'ai-nodes.js'), 'utf8'), host154 = fs.readFileSync(path.join(BRIDGE, 'ai-node.js'), 'utf8'), team154 = fs.readFileSync(path.join(BRIDGE, 'team.js'), 'utf8'), page154 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
    check('R154', 'going live never cuts a host off: after a hub restart an older host that does not say hello again still offers its Claude (session kept in the gitignored ai-host-sessions.json, the 2.x routing too), a current one too; a host without a session is asked to hello again (428 with its version, 401 without), a 1.x host (no hello ever) keeps polling; the poll answer is the hub\'s heartbeat (time, version, commit) and the host reconnects in 1-5 s, re-hellos on 428, follows a new hub version, and only restarts / updates after 3 quiet minutes; hub_status lists the AI hosts up / down; [static] the page reloads itself onto a new hub version when nothing is in progress',
      pid2 !== pid1 && oldAfter.remoteKind === 'claude-cli-work' && !!newAfter.id && askNew.status === 428 && askOld.status === 401 && legacyNoHello === 'held'
      && !!(sessFile['old-node'] && sessFile['old-node'].hello && /^[0-9a-f]{64}$/.test(sessFile['old-node'].key)) && ign === 0
      && /beat: beat\(\)/.test(nodes154) && /r\.status === 428/.test(host154) && /Math\.min\(RECONNECT_MAX_MS, 1000 \* misses\)/.test(host154) && /quiet = \(ms = UPDATE_QUIET_MS\)/.test(host154) && /maxDelaySeconds, 1, 60, 5\)/.test(host154) && /quietMinutes, 1, 60, 3\)/.test(host154) && /aiHosts/.test(team154)
      && /X-VR-Version/.test(fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8')) && /function pageUpdateAvailable\(build, path\)/.test(page154) && /last\.build === build && Date\.now\(\) - last\.at < 10 \* 60e3/.test(page154) && /sessionStorage\.setItem\(UPDATE_LAST/.test(page154) && /hubBuild !== \(me\.build \|\| me\.version\)/.test(page154),
      `old ${oldAfter.remoteKind} new ${!!newAfter.id}; no session: ${askNew.status} / ${askOld.status}; 1.x ${legacyNoHello}; sessions file ${Object.keys(sessFile).join(',')}; ignored ${ign}`);
  }
  // R163 / R164: the native (Rust) programs, built here when this machine has a Rust toolchain (else skipped)
  {
    const L163 = 'native core (native/crates/vr-core) against the JavaScript side, both ways: the AI host session key id + envelope (Rust seals → Node opens, Node seals → Rust opens) and the update HMAC as bridge/ai-nodes.js makes them; the chat as the page does it with WebCrypto: a page message (format 4) opened by Rust, Rust messages (formats 3 and 4) opened by the page, the uploader\'s own seal and a chat file opened by Rust';
    const L164 = 'the native AI host (vr-host): without a running host "status" exits 3; "set" checks like the control app (a plain-http hub refused), keeps unknown keys and leaves the config private; "settings" lists every setting; its MCP servers offer exactly the JavaScript servers\' tools; the hub hands out a signed release of the binary (Ed25519 with the release key + the host\'s HMAC; nobody else gets it); "run" supervises it: it connects, writes its status file, updates itself from that release when asked (checked, self-tested, swapped, restarted on it, confirmed once it reached the hub), pauses / resumes, stops when asked';
    const why = buildNative();
    if (why) { skip('R163', L163, why); skip('R164', L164, why); }
    else {
      const io = await require('./native-interop').run(NATIVE_BIN.interop);
      check('R163', L163, io.ok, io.ok ? Object.keys(io.parts).join(', ') : io.why);
      const rdir = path.join(SCRATCH, 'rust-host'); fs.mkdirSync(rdir, { recursive: true });
      const rexe = path.join(rdir, 'vr-host' + (WIN ? '.exe' : '')), rcfg = path.join(rdir, 'ai-node.json'), rsecret = OLD_NODE_SECRET + '-rs';
      fs.copyFileSync(NATIVE_BIN.host, rexe); fs.chmodSync(rexe, 0o755);
      fs.writeFileSync(rcfg, JSON.stringify({ hub: H, id: 'rust-node', token: rsecret, user: 't-admin', providers: [{ id: 'local', kind: 'openai', label: 'Rust host local', baseUrl: `http://127.0.0.1:${OLLAMA_PORT}/oai`, models: ['tiny-local'] }], engines: {}, keepMe: 'r164' }), { mode: 0o644 });
      const vh = (...a) => spawnSync(rexe, [...a, '--config', rcfg], { encoding: 'utf8', timeout: 30000 });
      const rstatus = () => { try { return JSON.parse(fs.readFileSync(path.join(rdir, 'ai-node.status.json'), 'utf8')); } catch { return {}; } };
      const stNot = vh('status'), setBad = vh('set', 'hub', 'http://evil.example'), setOk = vh('set', 'update.checkMinutes', '7');
      const cfgAfter = JSON.parse(fs.readFileSync(rcfg, 'utf8')), cfgMode = WIN ? 0o600 : fs.statSync(rcfg).mode & 0o777;
      const listed = vh('settings').stdout;
      const tools = (() => { try { return JSON.parse(vh('tools').stdout); } catch { return {}; } })();
      const jsTools = (f) => (fs.readFileSync(path.join(BRIDGE, f), 'utf8').match(/\{ name: '([a-z_]+)'/g) || []).map((m) => m.slice(9, -1)).sort().join(',');
      const toolsSame = [['pc', 'pc-tools-mcp.js'], ['board', 'board-mcp.js'], ['team', 'team-mcp.js']].every(([k, f]) => (tools[k] || []).slice().sort().join(',') === jsTools(f));
      // a newer build in the scratch hub's native/dist: the same program with bytes appended (ELF / PE ignore trailing data)
      const target = WIN ? 'x86_64-pc-windows-gnu' : 'x86_64-unknown-linux-gnu', dist = path.join(SCRATCH, 'native', 'dist', target);
      fs.mkdirSync(dist, { recursive: true });
      fs.writeFileSync(path.join(dist, path.basename(rexe)), Buffer.concat([fs.readFileSync(NATIVE_BIN.host), Buffer.from('\nr164-newer-build\n')]), { mode: 0o755 });
      fs.writeFileSync(path.join(dist, 'BUILD.json'), JSON.stringify({ version: '99.0.0', commit: 'c'.repeat(40) }));
      const rh = { 'X-VR-Host': 'rust-node', 'X-VR-Host-Token': rsecret };
      const man = await fetch(H + `/node/update/bin?app=vr-host&target=${target}`, { headers: rh }), manJ = await man.json().catch(() => ({}));
      const manAnon = (await fetch(H + `/node/update/bin?app=vr-host&target=${target}`)).status;
      const pub = crypto.createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(TEST_RELEASE_KEY.public, 'hex')]), format: 'der', type: 'spki' });
      const sigOk = !!manJ.manifest && crypto.verify(null, Buffer.from(manJ.manifest), pub, Buffer.from(manJ.sig || '', 'hex'));
      const hmacOk = !!manJ.manifest && manJ.hmac === crypto.createHmac('sha256', rsecret).update(manJ.manifest).digest('hex');
      const rlog = fs.openSync(path.join(rdir, 'out.log'), 'a');
      const rproc = spawn(rexe, ['run', '--config', rcfg], { cwd: rdir, stdio: ['ignore', rlog, rlog] });
      let rExit = null; rproc.on('exit', (c) => { rExit = c; });
      const hostUp = async () => { for (let i = 0; i < 80; i++) { const h = ((await call('/api/check', A)).data.aiNodes || []).find((x) => x.id === 'rust-node'); if (h && h.online && h.version) return h; await sleep(250); } return null; };
      const up1 = await hostUp();
      let st1 = rstatus(); for (let i = 0; i < 20 && st1.connected !== true; i++) { await sleep(250); st1 = rstatus(); } // rewritten within a second
      const stRun = vh('status');
      vh('update');
      let swapped = false; for (let i = 0; i < 160 && !swapped; i++) { await sleep(250); try { swapped = fs.readFileSync(rexe).includes('r164-newer-build'); } catch {} }
      let confirmed = false; for (let i = 0; i < 120 && !confirmed; i++) { await sleep(250); confirmed = swapped && !fs.existsSync(path.join(rdir, '.update-trial.json')) && /confirmed/.test(rstatus().lastUpdate || ''); }
      const up2 = await hostUp();
      vh('pause'); let paused = false; for (let i = 0; i < 40 && !paused; i++) { await sleep(250); paused = rstatus().paused === true; }
      vh('resume'); let resumed = false; for (let i = 0; i < 40 && !resumed; i++) { await sleep(250); resumed = rstatus().paused === false; }
      vh('stop', '--now'); for (let i = 0; i < 60 && rExit === null; i++) await sleep(250);
      if (rExit === null) { try { rproc.kill(); } catch {} }
      const outLog = (() => { try { return fs.readFileSync(path.join(rdir, 'out.log'), 'utf8'); } catch { return ''; } })();
      check('R164', L164,
        stNot.status === 3 && setBad.status === 1 && setOk.status === 0 && cfgAfter.update && cfgAfter.update.checkMinutes === 7 && cfgAfter.keepMe === 'r164' && cfgMode === 0o600
        && /provider\.local\.max5h/.test(listed) && /security\.engineGuard/.test(listed) && toolsSame
        && man.status === 200 && manAnon === 401 && sigOk && hmacOk && /"version":"99\.0\.0"/.test(manJ.manifest || '')
        && !!up1 && st1.connected === true && st1.native === true && stRun.status === 0 && /CONNECTED/.test(stRun.stdout)
        && swapped && confirmed && !!up2 && paused && resumed && rExit === 0,
        `status ${stNot.status}; set ${setBad.status}/${setOk.status} mode ${cfgMode.toString(8)} kept ${cfgAfter.keepMe}; tools ${toolsSame}; manifest ${man.status} anon ${manAnon} sig ${sigOk} hmac ${hmacOk}; up ${!!up1} (${up1 && up1.version}) status ${st1.connected}/${st1.native} cmd ${stRun.status}; swapped ${swapped} confirmed ${confirmed} (${rstatus().lastUpdate}); back ${!!up2}; pause ${paused} resume ${resumed}; exit ${rExit}; log "${outLog.replace(/\s+/g, ' ').slice(-300)}"`);
    }
    // R182: the native host finds a local Ollama by itself (no engine, no provider configured) and offers it; a missing
    // Codex login is named in its log at hello
    if (!why) {
      const L182 = 'the native AI host picks up a local Ollama by itself: with nothing configured for it, a host whose machine runs Ollama (OLLAMA_HOST) offers it as the engine and as a private provider "ollama" with the models Ollama lists; its owner sees it (kind host-openai), others do not; the API answers through it; a Codex login the host cannot read is named in its log with the way to fix it; the page lists ChatGPT / Codex and local models greyed out as not for Work';
      const odir = path.join(SCRATCH, 'ollama-host'); fs.mkdirSync(odir, { recursive: true });
      const oexe = path.join(odir, 'vr-host' + (WIN ? '.exe' : '')), ocfg = path.join(odir, 'ai-node.json');
      fs.copyFileSync(NATIVE_BIN.host, oexe); fs.chmodSync(oexe, 0o755);
      fs.writeFileSync(ocfg, JSON.stringify({ hub: H, id: 'ollama-node', token: OLD_NODE_SECRET + '-ol', user: 't-admin', autoUpdate: false,
        providers: [{ id: 'codex', kind: 'codex-relay', label: 'R182 Codex', models: ['gpt-5.5'], codexHome: path.join(odir, 'no-codex-here') }], engines: {} }), { mode: 0o600 });
      const olog = path.join(odir, 'out.log'), ofd = fs.openSync(olog, 'a');
      const oproc = spawn(oexe, ['run', '--config', ocfg], { cwd: odir, stdio: ['ignore', ofd, ofd], env: { ...process.env, OLLAMA_HOST: `127.0.0.1:${OLLAMA_PORT}` } });
      let seen = null; for (let i = 0; i < 80 && !seen; i++) { seen = ((await call('/api/check', A)).data.providers || []).find((p) => p.id === 'h:ollama-node/ollama') || null; if (!seen) await sleep(250); }
      const otherSees = ((await call('/api/check', U)).data.providers || []).some((p) => p.id === 'h:ollama-node/ollama');
      const key = (await call('/api/apikey/rotate', A, {})).data.key;
      await call('/api/apikey/settings', A, { provider: 'h:ollama-node/ollama', model: 'unity-brain:latest' });
      const ans = await fetch(H + '/v1/chat/completions', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + key }, body: JSON.stringify({ model: 'unity-brain:latest', messages: [{ role: 'user', content: 'local r182' }] }) }).then((r) => r.json()).catch(() => ({}));
      await call('/api/apikey/revoke', A, {});
      try { oproc.kill(); } catch {}
      const outText = (() => { try { return fs.readFileSync(olog, 'utf8'); } catch { return ''; } })();
      const page182 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
      check('R182', L182,
        !!seen && seen.kind === 'host-openai' && (seen.models || []).includes('unity-brain:latest') && !otherSees
        && /oai says: local r182/.test(((ans.choices || [])[0] || {}).message?.content || '')
        && /found Ollama at http:\/\/127\.0\.0\.1:\d+ with 1 model/.test(outText) && /no Codex login found at .*no-codex-here.*cli_auth_credentials_store/.test(outText)
        && /const NOT_WORK = \(p\) =>/.test(page182) && /not for Work: pick it in Settings → API access/.test(page182) && /'host-codex': 'AI host · ChatGPT \/ Codex'/.test(page182),
        `seen ${seen && seen.kind} ${seen && (seen.models || []).join(',')}; others ${otherSees}; answer ${JSON.stringify(ans).slice(0, 120)}; log ${outText.split('\n').filter((l) => /Ollama|Codex/.test(l)).join(' | ').slice(0, 300)}`);
    }
    // R183: a second copy waits instead of looping, and a control command from a download reaches the installed host
    if (!why && !WIN) {
      const L183 = 'the native host on a machine with two copies: `vr-host run` in a folder whose host already runs waits for it (says so once, does not exit, so systemd does not restart it every 5 s); a control command run from a download that is not running (status, update) acts on the installed host that runs and says so; an automatic update waits at most 30 minutes for quiet minutes (LAWS.md)';
      const home = path.join(SCRATCH, 'r183-home'), inst = path.join(home, '.local', 'share', 'viberoller', 'ai-host'), dl = path.join(SCRATCH, 'r183-download');
      fs.mkdirSync(inst, { recursive: true }); fs.mkdirSync(dl, { recursive: true });
      const cfg183 = JSON.stringify({ hub: H, id: 'r183-node', token: 'r183-token-0123456789abcdef', user: 't-admin', providers: [], engines: {}, autoUpdate: false });
      for (const d of [inst, dl]) { fs.copyFileSync(NATIVE_BIN.host, path.join(d, 'vr-host')); fs.chmodSync(path.join(d, 'vr-host'), 0o755); fs.writeFileSync(path.join(d, 'ai-node.json'), cfg183, { mode: 0o600 }); }
      fs.writeFileSync(path.join(inst, 'ai-node.lock'), String(process.pid)); // "a host runs there": this test process holds its lock
      const env183 = { ...process.env, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config') };
      const runLog = path.join(inst, 'run.out'), fd183 = fs.openSync(runLog, 'a');
      const sup = spawn(path.join(inst, 'vr-host'), ['run', '--config', path.join(inst, 'ai-node.json')], { cwd: inst, env: env183, stdio: ['ignore', fd183, fd183] });
      let supExit = null; sup.on('exit', (c) => { supExit = c; });
      await sleep(4000);
      const stillUp = supExit === null; try { sup.kill(); } catch {}
      const supOut = (() => { try { return fs.readFileSync(runLog, 'utf8'); } catch { return ''; } })();
      const st = spawnSync(path.join(dl, 'vr-host'), ['status'], { cwd: dl, env: env183, encoding: 'utf8', timeout: 30000 });
      const upd = spawnSync(path.join(dl, 'vr-host'), ['update'], { cwd: dl, env: env183, encoding: 'utf8', timeout: 30000 });
      const askedInstalled = fs.existsSync(path.join(inst, '.update-now')), askedDownload = fs.existsSync(path.join(dl, '.update-now'));
      const runner183 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'host', 'runner.rs'), 'utf8'), laws183 = fs.readFileSync(path.join(MILLS, 'LAWS.md'), 'utf8');
      check('R183', L183,
        stillUp && (supOut.match(/another AI host runs in this folder/g) || []).length === 1 && !/already running \(pid \d+\); nothing to do\.[\s\S]*already running/.test(supOut)
        && /acting on it/.test(st.stderr) && new RegExp(inst.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).test(st.stdout) && /RUNNING|running|pid/i.test(st.stdout)
        && askedInstalled && !askedDownload && /Asked it to look for an update now/.test(upd.stdout)
        && /UPDATE_MAX_WAIT_MS: u64 = 30 \* 60_000/.test(runner183) && /after an automatic update has waited 30 minutes/.test(laws183),
        `waits ${stillUp} (${supOut.trim().split('\n').slice(-2).join(' | ').slice(0, 160)}); status ${st.stderr.trim().slice(0, 100)} / ${st.stdout.split('\n')[0]}; update asked installed ${askedInstalled} download ${askedDownload}`);
    }
    // R184: vr-host is one place for everything: install puts it on the PATH (once), doctor lists the checks, setup follows
    if (!why && !WIN) {
      const L184 = 'vr-host install from a download (a scratch HOME, systemd untouched) installs into ~/.local/share/viberoller/ai-host, links ~/.local/bin/vr-host to it and writes the ~/.profile block once (a second install still once); `vr-host doctor --json` lists the checks (config, private, copy, boot, path ok, hub, updates); `vr-host path` says it is already on the PATH; `vr-host setup` brings VERSION.txt to the program\'s version; the control app has its wizard, providers, doctor screens (static)';
      const home = path.join(SCRATCH, 'r184-home'), dl = path.join(SCRATCH, 'r184-download'), inst = path.join(home, '.local', 'share', 'viberoller', 'ai-host');
      fs.mkdirSync(home, { recursive: true }); fs.mkdirSync(dl, { recursive: true });
      fs.copyFileSync(NATIVE_BIN.host, path.join(dl, 'vr-host')); fs.chmodSync(path.join(dl, 'vr-host'), 0o755);
      fs.writeFileSync(path.join(dl, 'ai-node.json'), JSON.stringify({ hub: H, id: 'r184-node', token: 'r184-token-0123456789abcdef', user: 't-admin', providers: [], engines: {}, autoUpdate: false }), { mode: 0o600 });
      fs.writeFileSync(path.join(dl, 'VERSION.txt'), '0.0.1-old\n');
      const env184 = { ...process.env, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), VR_HOST_NO_BOOT: '1' };
      const run184 = (exe, args, env = env184) => spawnSync(exe, args, { cwd: path.dirname(exe), env, encoding: 'utf8', timeout: 60000 });
      const i1 = run184(path.join(dl, 'vr-host'), ['install']);
      const i2 = run184(path.join(dl, 'vr-host'), ['install']);
      const link = path.join(home, '.local', 'bin', 'vr-host');
      const linkTarget = (() => { try { return fs.realpathSync(link); } catch { return ''; } })();
      const profile = (() => { try { return fs.readFileSync(path.join(home, '.profile'), 'utf8'); } catch { return ''; } })();
      const onPath = { ...env184, PATH: path.join(home, '.local', 'bin') + path.delimiter + process.env.PATH };
      const doc = run184(path.join(inst, 'vr-host'), ['doctor', '--json'], onPath);
      const checks = (() => { try { return JSON.parse(doc.stdout); } catch { return []; } })();
      const byId = (id) => checks.find((c) => c.id === id) || {};
      const pth = run184(path.join(inst, 'vr-host'), ['path'], onPath);
      fs.writeFileSync(path.join(inst, 'VERSION.txt'), '0.0.1-old\n');
      const ver = run184(path.join(inst, 'vr-host'), ['version']).stdout.trim().split(/\s+/)[0];
      const setup = run184(path.join(inst, 'vr-host'), ['setup'], onPath);
      const verTxt = (() => { try { return fs.readFileSync(path.join(inst, 'VERSION.txt'), 'utf8').trim(); } catch { return ''; } })();
      const tui184 = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-host', 'src', 'tui.rs'), 'utf8');
      check('R184', L184,
        i1.status === 0 && i2.status === 0 && fs.existsSync(path.join(inst, 'vr-host')) && linkTarget === fs.realpathSync(path.join(inst, 'vr-host'))
        && (profile.match(/# added by VibeRoller AI Host \(vr-host\)/g) || []).length === 1 && /vr-host: linked/.test(i1.stdout)
        && doc.status === 0 && ['config', 'private', 'copy', 'boot', 'path', 'hub', 'updates'].every((id) => checks.some((c) => c.id === id))
        && byId('config').ok === true && byId('private').ok === true && byId('path').ok === true && byId('copy').ok === true
        && /already on PATH/.test(pth.stdout) && ver && verTxt === ver && /VERSION\.txt now says/.test(setup.stdout)
        && /Screen::Setup/.test(tui184) && /Screen::Providers/.test(tui184) && /Screen::Doctor/.test(tui184),
        `install ${i1.status}/${i2.status} "${(i1.stdout + i1.stderr).replace(/\s+/g, ' ').slice(0, 260)}"; link ${linkTarget}; blocks ${(profile.match(/added by VibeRoller/g) || []).length}; doctor ${doc.status} ${checks.map((c) => c.id + ':' + c.ok).join(',')}; path "${pth.stdout.trim()}"; version ${ver} txt ${verTxt}; setup "${setup.stdout.trim().slice(0, 160)}"`);
    }
    // R167: the Downloads hand out the native programs (pre-built), and they install themselves (the Rust hub's alone)
    if (!why && RUST_HUB) {
      const target = WIN ? 'x86_64-pc-windows-gnu' : 'x86_64-unknown-linux-gnu', dist = path.join(SCRATCH, 'native', 'dist', target), ext = WIN ? '.exe' : '';
      fs.mkdirSync(dist, { recursive: true });
      fs.copyFileSync(NATIVE_BIN.host, path.join(dist, 'vr-host' + ext)); // (R164 left a "newer" one here: the real build again)
      fs.writeFileSync(path.join(dist, 'vr-desktop' + ext), 'fake desktop r167'); fs.writeFileSync(path.join(dist, 'vr-hostw' + ext), 'fake hostw r167');
      fs.writeFileSync(path.join(dist, 'BUILD.json'), JSON.stringify({ version: '3.99.0', commit: 'd'.repeat(40) }));
      const osName = WIN ? 'windows' : 'linux';
      const raw = (tok, p) => fetch(H + p, { headers: { Authorization: 'Bearer ' + tok } });
      const dlAll = (await call('/api/downloads', A)).data || {}, dl = dlAll.items || [];
      const appZip = await raw(A, `/api/downloads/app-${osName}.zip`), appEntries = appZip.status === 200 ? unzipAll(Buffer.from(await appZip.arrayBuffer())) : [];
      const hostsNow = (await call('/api/hosts', A)).data.mine || [];
      const myHost = hostsNow[0] || (await call('/api/hosts/new', A, {})).data.host;
      const hostZip = await raw(A, `/api/downloads/host-${osName}.zip?host=${encodeURIComponent(myHost.id)}`), hostEntries = hostZip.status === 200 ? unzipAll(Buffer.from(await hostZip.arrayBuffer())) : [];
      const pubApp = (await fetch(H + `/api/pub/app-${osName}.zip`)).status, anonApp = (await fetch(H + `/api/downloads/app-${osName}.zip`)).status;
      // the desktop app's own release, signed in (what it updates itself from)
      const rel = await raw(A, `/api/native?app=vr-desktop&target=${target}`), relJson = rel.status === 200 ? await rel.json() : {};
      // the downloaded AI host installs itself (a scratch home), its config with it, private
      const unz = path.join(SCRATCH, 'r167-download'); fs.mkdirSync(unz, { recursive: true });
      for (const e of hostEntries) { const f = path.join(unz, e.name.split('/').slice(1).join('/')); fs.writeFileSync(f, e.data); if (/vr-host/.test(e.name)) fs.chmodSync(f, 0o755); }
      const home167 = path.join(SCRATCH, 'r167-home'); fs.mkdirSync(home167, { recursive: true });
      const inst = spawnSync(path.join(unz, 'vr-host' + ext), ['install'], { encoding: 'utf8', timeout: 60000, env: { ...process.env, VR_HOST_NO_BOOT: '1', HOME: home167, LOCALAPPDATA: path.join(home167, 'AppData', 'Local'), USERPROFILE: home167 } });
      const instDir = WIN ? path.join(home167, 'AppData', 'Local', 'VibeRoller', 'AIHost') : path.join(home167, '.local', 'share', 'viberoller', 'ai-host');
      const instCfg = (() => { try { return JSON.parse(fs.readFileSync(path.join(instDir, 'ai-node.json'), 'utf8')); } catch { return {}; } })();
      const instMode = WIN ? 0o600 : (() => { try { return fs.statSync(path.join(instDir, 'ai-node.json')).mode & 0o777; } catch { return 0; } })();
      const again = spawnSync(path.join(instDir, 'vr-host' + ext), ['install'], { encoding: 'utf8', timeout: 60000, env: { ...process.env, VR_HOST_NO_BOOT: '1', HOME: home167, LOCALAPPDATA: path.join(home167, 'AppData', 'Local'), USERPROFILE: home167 } });
      check('R167', 'the Downloads hand out the native programs, pre-built: the native app (vr-desktop + the AI host) and the native AI host (with that person\'s own host config inside when asked) for this machine\'s system, to people with an account only (anonymous → 401, the old public route → 410); the list names the native releases, and the desktop app\'s signed release is there for a signed-in app; the downloaded AI host installs itself ("vr-host install": the standard folder, its config kept and private) and an installed copy knows it is the installed one',
        dl.some((i) => i.id === `app-${osName}` && i.native && i.group === 'native') && dl.some((i) => i.id === `host-${osName}` && i.group === 'native-host')
        && appEntries.some((e) => e.name === 'VibeRoller/vr-desktop' + ext) && appEntries.some((e) => e.name === 'VibeRoller/vr-host' + ext) && appEntries.some((e) => /VibeRoller\/README\.txt$/.test(e.name))
        && hostEntries.some((e) => e.name === 'VibeRollerAIHost/vr-host' + ext) && hostEntries.some((e) => e.name === 'VibeRollerAIHost/ai-node.json' && JSON.parse(e.data.toString()).id === myHost.id)
        && pubApp === 410 && anonApp === 401 && Array.isArray(dlAll.native) && rel.status === 200 && typeof relJson.sig === 'string'
        && inst.status === 0 && fs.existsSync(path.join(instDir, 'vr-host' + ext)) && instCfg.id === myHost.id && instMode === 0o600 && /installed AI host/.test(again.stdout),
        `items ${dl.map((i) => i.id).join(',')}; app ${appZip.status} ${appEntries.map((e) => e.name).join(',')}; host ${hostZip.status} ${hostEntries.map((e) => e.name).join(',')}; pub ${pubApp} anon ${anonApp}; native ${Array.isArray(dlAll.native)} release ${rel.status} ${Object.keys(relJson).join(',')}; install ${inst.status} "${String((inst.stdout || '') + (inst.stderr || '') + (inst.error || '')).trim().slice(0, 200)}" cfg ${instCfg.id === myHost.id} mode ${instMode.toString(8)}; again "${String(again.stdout || again.error || '').trim().slice(0, 100)}"`);
    } else skip('R167', 'the Downloads hand out the native programs, pre-built, and they install themselves', why || 'the Rust hub\'s alone: --rust-hub runs it');
    // R165: the desktop app (Tauri) starts on Linux, headless, against this hub
    const L165 = 'the desktop app (vr-desktop, Tauri) starts on Linux (headless, Xvfb): its Tools window (its own page) loads, runs and reaches the app with the Tools permissions; its main window shows this hub\'s page, which sees the app (window.__TAURI__ → NATIVE) and is allowed the hub capability (app_info, notify) and shows the Tools tile; [static] the hub\'s page is never given the commands that touch the computer (files, terminal, runs, git, proxy, settings), and the Tools window\'s page is the app\'s own';
    const dsk = why ? { why } : buildDesktop();
    if (dsk.why) skip('R165', L165, dsk.why);
    else {
      const sm = spawnSync('sh', [path.join(MILLS, 'native', 'smoke-desktop.sh'), dsk.bin, H], { encoding: 'utf8', timeout: 180000 });
      const mainSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-desktop', 'src', 'main.rs'), 'utf8');
      const hubCap = (/CapabilityBuilder::new\("hub"\)[\s\S]*?\)\)\?;/.exec(mainSrc) || [''])[0];
      const toolsCap = JSON.parse(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-desktop', 'capabilities', 'tools.json'), 'utf8'));
      const forbidden = ['fs-', 'term-', 'run-', 'allow-git', 'proxy-', 'settings-', 'pick-folder', 'update-'];
      if (sm.status === 77) skip('R165', L165, sm.stdout.trim());
      else check('R165', L165,
        sm.status === 0 && /SMOKE smoke-ok/.test(sm.stdout) && new RegExp('SMOKE smoke-main 127\\.0\\.0\\.1:' + PORT + ' \\S+ tools-tile true share-field boolean').test(sm.stdout)
        && !!hubCap && !forbidden.some((f) => hubCap.includes(f)) && /window\("main"\)/.test(hubCap) && toolsCap.windows.join() === 'tools' && !toolsCap.remote
        && /WebviewUrl::App\("tools\.html/.test(mainSrc),
        `smoke ${sm.status} "${(sm.stdout + sm.stderr).replace(/\s+/g, ' ').slice(0, 400)}"; hub capability clean ${!!hubCap && !forbidden.some((f) => hubCap.includes(f))}`);
      // R290 (T-243): screen sharing in the app's webview is granted to the hub's own page only
      const capSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'vr-desktop', 'src', 'capture.rs'), 'utf8');
      const toolsBuilder = (/WebviewWindowBuilder::new\(app, "tools"[^\n]*/g.exec(mainSrc) || [''])[0];
      const home290 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
      const ut = sm.status === 77 ? { status: 77 } : spawnSync('sh', ['-c', `export RUSTUP_HOME="${home290}/.rustup" CARGO_HOME="${home290}/.cargo" CARGO_TARGET_DIR="${NATIVE_TARGET}" OWNER_HOME="${home290}"; export PATH="$CARGO_HOME/bin:$PATH"; . ./tauri-env.sh; cargo test --release -q -p vr-desktop capture 2>&1`], { cwd: path.join(MILLS, 'native'), encoding: 'utf8', timeout: 1800000 });
      const L290 = 'screen sharing in the desktop app is granted to the hub\x27s own page only (permission handler, unit tests, app_info.screenShare)';
      if (sm.status === 77) skip('R290', L290, sm.stdout.trim());
      else check('R290', 'screen sharing in the desktop app: the main window (the hub\'s page) answers the webview\'s permission requests through one handler that grants capture (screen, microphone, camera) only to the hub\'s own origin and refuses it to any other page, the Tools window has no such handler so it is never granted, Linux turns the WebKitGTK media stream on for the main window only, app_info carries screenShare / screenShareWhy / screenShareAudio (a boolean, read by the hub\'s page in the smoke), and the origin / permission unit tests pass; real capture needs a desktop session and is not exercised here',
        /\.on_permission_request\(/.test(mainSrc) && /capture::decide\(/.test(mainSrc) && !/on_permission_request/.test(toolsBuilder)
        && /capture::enable_in_webview\(&main_win\)/.test(mainSrc) && /set_enable_media_stream\(true\)/.test(capSrc) && /capture::capability\(\)/.test(mainSrc)
        && /screenShare/.test(capSrc) && /is_hub_origin\(page, hub\)/.test(capSrc) && ut.status === 0 && /3 passed; 0 failed|test result: ok\. 3 passed/.test(ut.stdout || ''),
        `cargo test ${ut.status} "${String(ut.stdout || ut.stderr || '').replace(/\s+/g, ' ').slice(-200)}"`);
    }
  }
  // R321, R322, R340 (T-307, linked machines 1/8): the library core pgm-net-internal::machine (scope model + evaluator,
  // path canonicaliser, safety floor), pure logic: its unit tests run once (Linux real temp dirs and symlinks; the Windows
  // rules are pure-logic tests that run on any host)
  {
    const mdir = path.join(MILLS, 'native', 'crates', 'pgm-net-internal', 'src', 'machine');
    const home321 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
    const env321 = { ...process.env, RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(home321, '.rustup'), CARGO_HOME: process.env.CARGO_HOME || path.join(home321, '.cargo'), CARGO_TARGET_DIR: NATIVE_TARGET };
    env321.PATH = path.join(env321.CARGO_HOME, 'bin') + path.delimiter + (env321.PATH || '');
    const ct = spawnSync('cargo', ['test', '-p', 'pgm-net-internal', 'machine::'], { cwd: path.join(MILLS, 'native'), env: env321, encoding: 'utf8', timeout: 1800000 });
    const L321 = 'linked machines core (T-307): scope model + evaluator in pgm-net-internal::machine::scope: Scope parses the 5.3 JSON, folder is the default, unknown keys are ignored, a newer v is refused whole, a bad or missing file allows nothing, every preset (folder, folders, read-only, full, full-ro, deny-list, no-shell, no-network, unrestricted, custom) has table-driven tests, deny beats allow, the more specific entry wins, full hides raw devices and keeps /sys and /proc/sys read-only, grants expire on a passed-in clock';
    const L322 = 'linked machines core (T-307): the one path canonicaliser machine::paths: .., symlink escape, dangling links, \\\\?\\, UNC, another drive, C:\\x vs c:/X, alternate data streams, reserved names, and the write-through-a-swapped-link case (O_NOFOLLOW + openat re-check on Linux) all deny; the Windows rules are pure logic tested on any host';
    const L340 = 'linked machines core (T-307): the safety floor machine::floor: under unrestricted (and every preset) the machine\'s token, device key, scope file and audit log are never readable, writable, deletable or listable, also through symlinks; fetch never sends them; the default deny list on full covers .env, *.pem, id_*, ~/.ssh and hub.json (glob matching)';
    const L331 = 'linked machines audit log (T-312): machine::audit is append-only and hash-chained: every allow and deny leaves one line, an edited, removed, reordered or tail-cut line breaks verify(), rotation keeps the chain across files, file contents are never in it (only a digest), the hub copy has no digest or byte count and only the owner may read it (an admin may not)';
    const L332 = 'linked machines guards (T-312): a KILL file or machine stop stops jobs and the link, survives a restart and only a local action clears it (a remote message cannot); over-limit calls per tool class are refused with a retry-after; elevate without the machine\'s approval fails and never runs the command, an approved one runs once; the app refuses to serve as root / Administrator';
    const L333 = 'linked machines scope changes (T-312): a widening proposal from the web stays pending and changes nothing until the machine approves, a narrowing applies at once and is audited with the old and new scope hash, a grant expires on time and its jobs are told to stop, and no tool can read or change the scope file or its neighbours (floor, under every preset)';
    const L327 = 'linked machines who may reach a machine (T-310): a linked machine\'s device token reaches only /api/machine/poll|reply|unlink and is refused (403) on every other route while an ordinary device token is unchanged; a watcher of a shared Work session gets no pc_tool / machine_* event (the share log holds only text lines), cannot answer a call (its /api/pc-result finds nothing, the run waits for the owner) and sees nothing through attach; a planner session\'s machine / pc call is refused and no job reaches the host; the one rule (machine_rule) is run account = machine owner AND the run started by that person: a run started on the owner\'s behalf by someone else (Hyper Workflows, T-294: Work::set_started_by) gets 403 for machine, host pc and the browser folder, the owner\'s own run works; nothing refuses a run because the owner is away (no grant, no presence check) and the prompt says "report what I did, why, what you can change"';
    const L338 = 'linked machines compatibility (T-310): an older AI host (no machine cap, not even signed-pc) still serves its pc engine and is shown "one folder (older app)"; the hub\'s hello answer announces machine v1 and a newer app against an older hub (no announcement, the frozen JavaScript hub) stays an AI host; a scope document with a higher v (or unreadable) arriving in a machine\'s poll is refused whole at the hub (nothing kept, the typed words ignored) while a readable one gives the words; the frozen JavaScript hub keeps the browser PC-folder tools and has no machine routes';
    if (ct.error) { for (const [id, l] of [['R327', L327], ['R338', L338], ['R321', L321], ['R322', L322], ['R340', L340], ['R331', L331], ['R332', L332], ['R333', L333], ['R324', 'signed requests and the machine\'s own check (T-308): machine::sign + the host pin']]) skip(id, l, 'no Rust toolchain here (' + ct.error.code + ')'); }
    else {
      const out321 = String(ct.stdout || '') + String(ct.stderr || '');
      const okN = (re) => (out321.match(new RegExp('^test machine::' + re + '[^ ]* \\.\\.\\. ok$', 'gm')) || []).length;
      const detail321 = `cargo test ${ct.status} scope ${okN('scope::')} paths ${okN('paths::')} floor ${okN('floor::')} integration ${okN('tests::')} ${/FAILED|panicked/.test(out321) ? 'FAILURES' : ''}`;
      const src321 = ['mod.rs', 'scope.rs', 'paths.rs', 'floor.rs'].every((f) => fs.existsSync(path.join(mdir, f)));
      check('R321', L321, ct.status === 0 && src321 && okN('scope::') >= 12, detail321);
      check('R322', L322, ct.status === 0 && okN('paths::') >= 8, detail321);
      check('R340', L340, ct.status === 0 && okN('floor::') >= 4 && okN('tests::') >= 4, detail321);
      // R324 (T-308, linked machines 2/8): signed requests and the machine's own check: machine::sign (hub key, envelope, verifier) and the
      // host's pin of the hub key (host/pin.rs: an older hub that signs nothing still works). The hub's per-run secret unit tests ride along.
      const ct323 = spawnSync('cargo', ['test', '-p', 'pgm-ai-internal', '--', 'host::pin::', 'work::tests::a_run_is_found', 'work::tests::the_deprecated', 'work::tests::a_hosts_job_token'], { cwd: path.join(MILLS, 'native'), env: env321, encoding: 'utf8', timeout: 1800000 });
      const out2 = String(ct323.stdout || '') + String(ct323.stderr || '');
      const ok2 = (name) => new RegExp('^test ' + name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ' \\.\\.\\. ok$', 'm').test(out2);
      const signTests = ['a_good_request_passes_and_names_the_actor', 'bad_signature_other_key_and_tampering_are_refused', 'a_lying_hub_stub_is_refused', 'a_request_for_another_owner_or_by_another_actor_is_refused', 'expired_future_and_too_long_requests_are_refused', 'a_repeated_nonce_is_refused_and_the_cache_is_bounded', 'the_tool_and_the_arguments_are_bound', 'the_hub_key_is_made_once_and_kept_owner_only'];
      const pinTests = ['no_key_announced_means_unsigned_jobs_work_as_before', 'a_pinned_key_demands_a_signature_for_the_hosts_owner', 'a_different_key_later_is_not_taken_and_a_hello_again_keeps_the_replay_cache'];
      const signOk = signTests.filter((t) => new RegExp('^test machine::sign::tests::' + t + ' \\.\\.\\. ok$', 'm').test(out321));
      const pinOk = pinTests.filter((t) => ok2('host::pin::tests::' + t));
      const src324 = fs.existsSync(path.join(mdir, 'sign.rs')) && /pub mod sign;/.test(fs.readFileSync(path.join(mdir, 'mod.rs'), 'utf8'));
      check('R324', 'signed requests and the machine\'s own check (T-308): one ed25519 hub key made once in the hub\'s data folder (owner-only file), a signed envelope {owner, actor{account, session, run}, tool, args digest, nonce, issuedAt, expiresAt}, and the machine-side verifier refuses a bad signature, a request signed by another key (a lying hub stub), an expired or future request, a repeated nonce (bounded replay cache), an owner that is not its own link, actor.account != owner, another machine id, another tool or other arguments than the ones signed; the host pins the hub\'s key from its hello (an older hub that announces none keeps working unsigned, a different key later is not taken, an unsigned job is refused once pinned)',
        ct.status === 0 && ct323.status === 0 && src324 && signOk.length === signTests.length && pinOk.length === pinTests.length && /signed-pc/.test(fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-net-internal', 'src', 'hostlink.rs'), 'utf8')),
        `cargo test ${ct.status} / ${ct323.status}: sign ${signOk.length}/${signTests.length}, pin ${pinOk.length}/${pinTests.length} ${/FAILED|panicked/.test(out2) ? 'FAILURES' : ''}`);
      // R331, R332, R333 (T-312, linked machines 6/8): the same cargo run covers the guards
      const detail331 = `cargo test ${ct.status} audit ${okN('audit::')} kill ${okN('kill::')} limits ${okN('limits::')} elevate ${okN('elevate::')} changes ${okN('changes::')} ${/FAILED|panicked/.test(out321) ? 'FAILURES' : ''}`;
      const src331 = ['audit.rs', 'kill.rs', 'limits.rs', 'elevate.rs', 'changes.rs'].every((f) => fs.existsSync(path.join(mdir, f)));
      check('R331', L331, ct.status === 0 && src331 && okN('audit::') >= 6, detail331);
      check('R332', L332, ct.status === 0 && src331 && okN('kill::') >= 4 && okN('limits::') >= 3 && okN('elevate::') >= 5, detail331);
      check('R333', L333, ct.status === 0 && src331 && okN('changes::') >= 7, detail331);
      // R327 and R338 (T-310): the unit tests of the rule, the watcher filter and the compatibility module, plus the hub's half from above
      const ct310 = spawnSync('cargo', ['test', '-p', 'pgm-net-internal', '-p', 'pgm-ai-internal', '-p', 'pgm-identity-internal', '--', 'compat::', 'work::tests::', 'devices::'], { cwd: path.join(MILLS, 'native'), env: env321, encoding: 'utf8', timeout: 1800000 });
      const out310 = String(ct310.stdout || '') + String(ct310.stderr || '');
      const t310 = (name) => new RegExp('^test ' + name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ' \\.\\.\\. ok$', 'm').test(out310);
      const units327 = ['work::tests::a_machine_call_needs_the_owner_to_be_the_account_and_the_starter', 'work::tests::watchers_see_no_machine_event_and_answer_none_and_a_planner_has_no_machine', 'work::tests::a_planner_may_only_write_notes_under_plans_and_never_use_pc_tools'];
      const units338 = ['compat::tests::an_older_hub_offers_no_machine_and_a_newer_app_stays_an_ai_host', 'compat::tests::an_older_app_keeps_its_pc_engine_and_gets_no_machine_job', 'compat::tests::a_scope_with_a_higher_v_is_refused_whole_at_the_polls_boundary'];
      const u327 = units327.filter(t310), u338 = units338.filter(t310);
      if (!RUST_HUB) skip('R327', L327, 'the Rust hub\'s: --rust-hub runs it');
      else check('R327', L327, ct310.status === 0 && u327.length === units327.length && !!r327hub && r327hub.ok, `cargo test ${ct310.status}: units ${u327.length}/${units327.length} ${/FAILED|panicked/.test(out310) ? 'FAILURES' : ''} · ${r327hub ? (r327hub.ok ? '' : 'FAILED ' + Object.entries(r327hub.o).filter(([, v]) => v !== true).map(([k]) => k).join(',') + ' · ') + r327hub.detail : 'no hub run'}`);
      // the frozen JavaScript hub's half (bridge/test/js-fallback.js): the browser PC tools answer, the machine routes are absent, hello announces no machine
      const js338 = await require('./js-fallback.js').probe({ portFrom: PORT + 380 });
      check('R338', L338, ct310.status === 0 && u338.length === units338.length && js338.ok && js338.steps.pcBrowser === true && js338.steps.noMachines === true && js338.steps.hostStaysAiHost === true && (!RUST_HUB || (!!r338hub && r338hub.ok)),
        `cargo test ${ct310.status}: units ${u338.length}/${units338.length} · JavaScript hub alone: ${js338.why.slice(0, 160)} · ${RUST_HUB ? (r338hub ? (r338hub.ok ? 'hub ok' : 'hub FAILED ' + Object.entries(r338hub.o).filter(([, v]) => v !== true).map(([k]) => k).join(',')) : 'no hub run') : 'plain mode: the Rust hub\'s half is skipped'}`);
    }
  }
  // R323 (T-308, linked machines 2/8, the Rust hub's): the per-run secret and the owner check on the hub (plans/linked-machines.md 4.3)
  if (!RUST_HUB) skip('R323', 'per-run MCP secret and the owner check for machines (the Rust hub\'s; --rust-hub runs it)', 'the Rust hub is not in front: run with --rust-hub');
  else await r323Check();
  // R488 (T-261, Mills' UI for plans/presence.md): what the page shows, driven over HTTP against the scratch hub, and the page's own
  // presence script evaluated in node (its pure helpers: one shape per label, do not disturb, the hover card's lines, nothing on a hub without presence)
  async function r488Check() {
    const got = {}; let P = null;
    const page = await fetch(H + '/').then((r) => r.text()).catch(() => '');
    const js = await fetch(H + '/presence.js').then(async (r) => (r.ok ? r.text() : '')).catch(() => '');
    got.loads = /<script src="presence\.js(\?v=[^"]*)?"><\/script>/.test(page) && js.length > 0; // the hub adds ?v=<build> to every script
    const ctx488 = require('vm').createContext({}); // no document, no page helpers: the script must still load (the web kit's pages differ)
    try { P = require('vm').runInContext(js + '\n;PRESENCE', ctx488); } catch (e) { got.vmErr = e.message; }
    const shapes = P ? P.LABELS.map((l) => P.shapeOf({ label: l })) : [];
    got.shapes = shapes.join(',');
    got.distinct = shapes.length === 6 && shapes.every(Boolean) && new Set(shapes).size === 6;
    got.dnd = P ? P.shapeOf({ label: 'Online', dnd: true }) : ''; got.dndOffline = P ? P.shapeOf({ label: 'Offline', dnd: true }) : '';
    got.unknown = P ? P.shapeOf(undefined) + '|' + P.shapeOf({ label: 'Something new' }) : 'x';
    if (P) { // a hub without presence: no status in the long poll, and GET /api/presence fails (404) → nothing is known and nothing throws
      const before = P.state.ok; P.update(undefined); P.update({ error: 'Unknown endpoint' }); let threw = false;
      ctx488.json = () => Promise.reject(new Error('Unknown endpoint')); // the page's json() on a 404
      try { await P.start(); } catch { threw = true; }
      got.degrades = !before && !P.state.ok && !threw && Object.keys(P.state.map).length === 0;
    }
    // the hub: mills sets a status; t-user sees it (GET /api/presence and the room long poll); the page's helpers read it
    const st = async (tok, u) => (((await call('/api/presence', tok)).data.people || []).find((p) => p.user === u)) || {};
    let on = true; (async () => { let ver = -1; while (on) { const r = await call(`/api/room/messages?rev=0&wait=1&ver=${ver}&device=r488`, D); if (r.status === 200 && r.data.ver !== undefined) ver = r.data.ver; else await sleep(300); } })();
    await sleep(400); await call('/api/presence', D, { device: 'r488', idle: false, screen: 'room' });
    const me = await call('/api/presence/me', D, { dnd: true, dndUntil: Date.now() + 600000, text: 'r488 reviewing', acceptingWork: true, hideWhere: true });
    got.me = me.status === 200 && me.data.real === 'online' && me.data.dnd === true && me.data.hideWhere === true && me.data.dndUntil > Date.now();
    await sleep(200);
    const seen = await st(U, 'mills');
    got.other = seen.label === 'Available for work' && seen.dnd === true && seen.text === 'r488 reviewing' && seen.acceptingWork === true && seen.hideWhere === undefined && seen.real === undefined && seen.where === undefined;
    got.otherShape = P ? P.shapeOf(seen) : '';
    const lines = P ? P.describe(seen, Date.now()) : [];
    got.card = lines.map((l) => l.k).join(',');
    const room = (await call('/api/room/messages?rev=0', U)).data, inRoom = (room.status || []).find((p) => p.user === 'mills') || {};
    got.room = inRoom.label === 'Available for work' && inRoom.dnd === true && inRoom.text === 'r488 reviewing';
    await call('/api/presence/me', D, { override: 'away', overrideUntil: Date.now() + 600000, dnd: false });
    await sleep(200); const away = await st(U, 'mills'); got.away = away.label + '/' + (P ? P.shapeOf(away) : '');
    const card = P ? P.describe({ user: 'x', presence: 'away', label: 'Working', working: true, since: Date.now() - 20 * 60000, text: 'deep in it', where: { project: 'Proj', branch: 'work/x', task: { id: 'T-1', title: 'Do it' } }, dnd: true }, Date.now()) : [];
    got.working = card.map((l) => l.t).join(' | ');
    const workingOk = /^Working · away \| for 20 min, since .+ \| “deep in it” \| on Proj \(work\/x\) · T-1 Do it \| Do not disturb/.test(got.working);
    await call('/api/presence/me', D, { override: 'auto', dnd: false, text: '', acceptingWork: false, hideWhere: false });
    on = false; await call('/api/presence', D, { device: 'r488', gone: true });
    check('R488', 'the page shows everyone\'s status (web/presence.js, T-261): the hub serves the presence script and the page loads it; it gives each of the six labels its own shape (colour-blind safe) and do not disturb its own (but not over Offline); on a hub without presence it knows nothing and throws nothing; a status set with POST /api/presence/me (do not disturb, text, accepting work, hide where) reaches another person through GET /api/presence and the room long poll without the private fields, the page draws it as the do-not-disturb shape with a hover card of label, since, text and dnd, appear away draws the moon, and a working person away reads "Working · away" with since, text, where and dnd',
      got.loads && got.distinct && got.dnd === 'minus' && got.dndOffline === 'ring' && got.unknown === '|' && got.degrades && got.me && got.other && got.otherShape === 'minus' && /^label,since,text,dnd(,devices)?$/.test(got.card) && got.room && got.away === 'Away/moon' && workingOk,
      `loads ${got.loads}${got.vmErr ? ' vm: ' + got.vmErr : ''} · shapes ${got.shapes} · dnd ${got.dnd}/${got.dndOffline} · unknown "${got.unknown}" · degrades ${got.degrades} · me ${got.me} · other ${got.other} (${seen.label}, dnd ${seen.dnd}, "${seen.text}") shape ${got.otherShape} card ${got.card} · room ${got.room} · away ${got.away} · working card "${got.working}"`);
  }
  // R489 (T-287): the Work session list. The hub side it relies on, over HTTP, then the panel script as the page gets it,
  // its pure helpers run here (state → colour / shape / meaning, the groups, the counter and room, the one menu).
  async function r489Check() {
    const ss = async (q) => (await call('/api/work/sessions' + (q || ''), U)).data || {};
    const one = async (id, q) => ((await ss(q)).sessions || []).find((s) => s.id === id) || {};
    const seen = {};
    const primary = (await ss()).primary, a = (await call('/api/work/sessions/new', U, { background: true })).data.id;
    seen.untitled = (await one(a)).title === '';
    await call('/api/work/sessions/rename', U, { id: a, title: '  r489   named\n' }); seen.title = (await one(a)).title;
    await call('/api/work/sessions/pin', U, { id: a, pinned: true }); seen.pinned = (await one(a)).pinned === true;
    await call('/api/work/sessions/pin', U, { id: a, pinned: false }); seen.unpinned = (await one(a)).pinned === false;
    // a message waiting in a session (e.g. refused with 429) is in the list, so the page sends it when a slot frees
    await call('/api/work/queue', U, { session: a, queue: ['r489 waits', 'r489 next'], sendNow: null }); seen.queued = (await one(a)).queued;
    await call('/api/work/queue', U, { session: a, queue: [] }); seen.unqueued = (await one(a)).queued === undefined;
    const run = chat(U, 'SLOW r489', { session: a }); await sleep(900);
    const mid = await ss('?archived=1'); seen.working = ((mid.sessions || []).find((s) => s.id === a) || {}).state; seen.counter = (mid.counts || {}).working;
    seen.room = Object.values((mid.counts || {}).perHost || {}).some((h) => h.used >= 1 && h.max >= 1);
    await run; await sleep(300); seen.after = (await one(a)).state;
    const f = (await call('/api/work/folders/new', U, { name: 'r489 f' })).data.id;
    await call('/api/work/folders/rename', U, { id: f, name: 'r489 g', collapsed: true });
    const fl = ((await ss()).folders || []).find((x) => x.id === f) || {}; seen.folder = fl.name === 'r489 g' && fl.collapsed === true;
    await call('/api/work/folders/delete', U, { id: f });
    seen.archive = (await call('/api/work/sessions/archive', U, { id: a })).status;
    seen.outOfList = !((await ss()).sessions || []).some((s) => s.id === a); seen.inArchived = (await one(a, '?archived=1')).archived === true;
    await call('/api/work/sessions/open', U, { id: a }); seen.back = (await one(a)).archived === false && (await ss()).current === a;
    await call('/api/work/sessions/open', U, { id: primary }); await call('/api/work/sessions/delete', U, { id: a });
    // the page loads the panel script, and the hub serves it
    const page = await (await fetch(H + '/')).text(), js = await fetch(H + '/sessions.js'), src = js.status === 200 ? await js.text() : '';
    seen.served = /<script src="sessions\.js(\?[^"]*)?"><\/script>/.test(page) && js.status === 200 && /javascript/.test(js.headers.get('content-type') || '') && !/id="openArchive"/.test(page) && /id="sessPanel"/.test(page);
    let S = null; try { const ctx = { window: {} }; require('vm').runInNewContext(src, ctx); S = ctx.window.SESSIONS; } catch (e) { seen.load = e.message; }
    if (S) {
      const hue = (hex) => { const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255), mx = Math.max(r, g, b), mn = Math.min(r, g, b), d = mx - mn; if (!hex || d < 0.12) return { grey: true }; const h = mx === r ? ((g - b) / d) % 6 : mx === g ? (b - r) / d + 2 : (r - g) / d + 4; return { h: (h * 60 + 360) % 360 }; };
      const v = Object.fromEntries(['waiting', 'working', 'done-unseen', 'draft', 'idle', 'ready'].map((st) => [st, S.stateView({ state: st })]));
      const dots = ['waiting', 'working', 'done-unseen', 'draft', 'idle'], H5 = Object.fromEntries(dots.map((k) => [k, hue(v[k].color)]));
      seen.colours = H5.waiting.h >= 40 && H5.waiting.h <= 60 && H5.working.h >= 255 && H5.working.h <= 290 && (H5['done-unseen'].h >= 345 || H5['done-unseen'].h <= 10) && H5.draft.h >= 200 && H5.draft.h <= 235 && H5.idle.grey === true;
      seen.shapes = new Set(dots.map((k) => v[k].shape)).size === 5 && dots.every((k) => v[k].shape) && !v.ready.shape && !v.ready.color;
      seen.meanings = Object.entries(v).every(([k, x]) => x.state === k && x.title.startsWith(x.name + ': ') && x.title.length > x.name.length + 6);
      const silent = S.stateView({ state: 'ready', ended: { why: 'silent' } }), stalled = S.stateView({ state: 'idle', ended: { why: 'stalled' } }), perm = S.stateView({ state: 'waiting', waiting: { kind: 'permission', tools: ['Bash'] } });
      seen.stall = silent.state === 'idle' && /sent nothing/.test(silent.title) && stalled.state === 'idle' && /stopped answering/.test(stalled.title) && /Bash/.test(perm.title) && S.stateView({ state: 'working', ended: { why: 'silent' } }).state === 'working';
      const now = new Date(2026, 9, 27, 12).getTime(), H1 = 3600e3;
      const list = [{ id: 't1', lastActivityAt: now - H1 }, { id: 'e1', lastActivityAt: now - 30 * H1 }, { id: 'p1', pinned: true, lastActivityAt: now - 50 * H1 }, { id: 'f1', folder: 'fa', lastActivityAt: now - 2 * H1 },
        { id: 'h1', hidden: true, title: 'secret plan', lastActivityAt: now }, { id: 'a1', archived: true, lastActivityAt: now }, { id: 't2', lastActivityAt: now - 0.5 * H1 }, { id: 'x1', folder: 'gone', lastActivityAt: now - 40 * H1 }];
      const folders = [{ id: 'fb', name: 'Empty', order: 1 }, { id: 'fa', name: 'Mine', order: 0 }];
      const gs = S.groupSessions(list, folders, { now }), key = (g) => g.key + '=' + g.items.map((x) => x.id).join(',');
      seen.groups = gs.map(key).join(' ');
      const found = S.groupSessions(list, folders, { now, q: 'SECRET' });
      seen.grouped = seen.groups === 'pinned=p1 f:fa=f1 f:fb= today=t2,t1 earlier=e1,x1 hidden=h1 archived=a1' && gs.find((g) => g.key === 'hidden').collapsed === true && gs.find((g) => g.key === 'archived').collapsed === true
        && found.map(key).join(' ') === 'hidden=h1' && found[0].collapsed === false;
      seen.counterText = S.counterText({ working: 3, waiting: 1 }) + ' | ' + S.counterText({ working: 1, waiting: 2 });
      const provs = [{ id: 'h:gee1/claude', owner: 'gee', label: 'Claude' }, { id: 'h:my1/claude', owner: 't-user', label: 'Claude' }];
      const name = (h) => S.hostName(h, provs, 't-user', (u) => (u === 'gee' ? 'Gee' : u));
      const room = S.roomText({ gee1: { used: 3, max: 5, maxPerProvider: 3, providers: { 'h:gee1/claude': 3 } }, my1: { used: 1 }, hub: { used: 5, max: 5 } }, name);
      seen.room2 = room.map((r) => r.text + (r.full ? '!' : '')).join(', ');
      const ids = (s, c) => S.menuItems(s, c).map((x) => x.id).join(',');
      const m = S.menuItems({ id: 's-1', worktree: { branch: 'work/x.1' }, folder: 'fa' }, { folders });
      seen.menu = ids({ id: 's-1', worktree: { branch: 'work/x.1' } }, { folders }) === 'open,rename,pin,move,share,home,link,hide,archive,delete'
        && ids({ id: 's-0', primary: true, worktree: {} }, {}) === 'open,rename,pin,move,share,link,hide,archive'
        && ids({ id: 's-2', archived: true, hidden: true, pinned: true }, { shared: true }) === 'open,rename,pin,move,share,link,hide,delete'
        && S.menuItems({ id: 's-2', hidden: true, pinned: true }, { shared: true }).filter((x) => ['pin', 'share', 'hide'].includes(x.id)).map((x) => x.label).join('|') === 'Unpin|Stop sharing|Unhide'
        && m.find((x) => x.id === 'delete').danger === true && m.find((x) => x.id === 'move').sub.map((x) => x.id + (x.on ? '*' : '')).join(',') === 'move:fb,move:fa*,move:new,move:none'
        && S.folderMenuItems({ collapsed: true }).map((x) => x.label).join('|') === 'Rename…|Expand|Delete folder…';
    }
    const ok = seen.untitled && seen.title === 'r489 named' && seen.pinned && seen.unpinned && seen.queued === 2 && seen.unqueued && seen.working === 'working' && seen.counter >= 1 && seen.room && seen.after === 'done-unseen'
      && seen.folder && seen.archive === 200 && seen.outOfList && seen.inArchived && seen.back && seen.served && !!S && seen.colours && seen.shapes && seen.meanings && seen.stall && seen.grouped
      && seen.counterText === '3 working · 1 needs you | 1 working · 2 need you' && seen.room2 === 'Gee\'s host 3/5, Hub 5/5!' && seen.menu;
    check('R489', R489_LABEL, ok, JSON.stringify(seen));
  }
  async function r323Check() {
    const hostHdr = (id, tok) => ({ 'Content-Type': 'application/json', 'X-VR-Host': id, 'X-VR-Host-Token': tok });
    const mkHost = async (token) => { const made = await call('/api/hosts/new', token, {}); const id = (made.data.host || {}).id || ''; const cj = (await call('/api/hosts/config', token, { id })).data.config || {}; return { id, token: cj.token, nick: (made.data.host || {}).nickname || '' }; };
    const hostA = await mkHost(A), hostU = await mkHost(U);
    const enc = (spec) => 'MACHTOOL=' + Buffer.from(JSON.stringify(spec)).toString('hex');
    const mach = (r) => { const l = texts(r.events).split('\n').find((x) => x.startsWith('MACH:')); try { return JSON.parse(l.slice(5)); } catch { return []; } };
    const runIdOf = (r) => ((r.events || []).find((e) => e.type === 'bridge_run') || {}).id || '';
    const listPc = (extra) => ({ route: 'pc-tool', body: { name: 'list', args: { path: '' } }, ...(extra || {}) });
    // 1. before any machine is online: the owner's browser folder still answers (today's fallback), and a named machine that is the
    //    owner's own but offline says so (it does not fall back); another person's machine is refused offline too
    const fb = await chat(U, 'PCTOOL r323 browser fallback');
    const early = mach(await chat(U, enc([listPc({ body: { name: 'list', args: { path: '' }, machine: hostU.id } }), listPc({ body: { name: 'list', args: {}, machine: hostA.id } }), listPc({ body: { name: 'list', args: {}, machine: 'no-such-machine-r323' } })])));
    // 2. two machines online (played by the test: hello with pc + signed-pc, a held poll, an answer for every job)
    const keyOf = {}; const got = { [hostA.id]: [], [hostU.id]: [] }; const ac = new AbortController();
    for (const h of [hostA, hostU]) {
      const hi = await (await fetch(H + '/node/hello', { method: 'POST', headers: hostHdr(h.id, h.token), body: JSON.stringify({ nonce: crypto.randomBytes(16).toString('hex'), user: '', providers: [], engines: [], pc: true, version: '9.9.9', os: 'linux', caps: ['relay', 'signed-pc'] }) })).json().catch(() => ({}));
      keyOf[h.id] = { key: hi.hubKey || '', kid: hi.hubKid || '' };
      (async () => { while (!ac.signal.aborted) {
        const pr = await fetch(H + '/node/poll', { headers: hostHdr(h.id, h.token), signal: ac.signal }).then((r) => r.json()).catch(() => ({}));
        for (const j of pr.jobs || []) { got[h.id].push(j); await fetch(H + '/node/reply', { method: 'POST', headers: hostHdr(h.id, h.token), body: JSON.stringify({ id: j.id, status: 200, headers: { 'content-type': 'application/json' }, body: Buffer.from(JSON.stringify({ ok: true, result: 'host-listing ' + h.id })).toString('base64') }) }).catch(() => {});
        }
      } })();
    }
    for (let i = 0; i < 40; i++) { const hs = (await call('/api/check', U)).data.aiNodes || []; if ([hostA, hostU].every((h) => hs.some((x) => x.id === h.id && x.online))) break; await sleep(250); }
    const legacyU = crypto.createHmac('sha256', WORK_SECRET).update('t-user').digest('hex');
    // U's run: unnamed goes to U's own machine; naming A's machine (by id, by nickname, inside args) is 403; a header or session that disagrees with
    // the run record is 403; a wrong secret is not ours; the old shared secret still works (deprecated) and the owner check still applies to it
    const rU = await chat(U, enc([
      listPc(),                                                                              // 0 own machine, unnamed
      listPc({ body: { name: 'list', args: {}, machine: hostA.id } }),                       // 1 A's machine by id
      listPc({ body: { name: 'list', args: {}, machine: hostA.nick } }),                     // 2 by nickname
      listPc({ body: { name: 'list', args: { path: '', machine: hostA.id } } }),             // 3 named inside args
      listPc({ account: 't-admin' }),                                                       // 4 the run's secret with someone else's account header
      listPc({ body: { name: 'list', args: {}, session: 'someone-elses-session' } }),        // 5 a session that is not this run's
      listPc({ secret: 'not-a-secret-r323' }),                                              // 6 a wrong secret
      listPc({ account: null }),                                                            // 7 no account header at all: the record says who
      listPc({ body: { name: 'list', args: {}, machine: hostU.nick } }),                     // 8 own machine by nickname
      { route: 'board', body: { name: 'board_list', args: {} } },                            // 9 other tools on the run's secret
      listPc({ secret: legacyU, account: 't-user' }),                                       // 10 the deprecated shared secret: still works
      listPc({ secret: legacyU, account: 't-user', body: { name: 'list', args: {}, machine: hostA.id } }), // 11 ... and is owner-checked
      listPc({ secret: legacyU, account: 't-admin' }),                                      // 12 the old secret is one account's: not as another
    ]));
    // A (the admin)'s run: names U's machine = 403 (an admin is another person); its own machine works, by id and unnamed
    const rA = await chat(A, enc([
      listPc({ body: { name: 'list', args: {}, machine: hostU.id } }),                       // 0 admin names U's machine
      listPc({ body: { name: 'list', args: {}, machine: hostU.nick } }),                     // 1 by nickname
      listPc(),                                                                              // 2 own machine, unnamed
      listPc({ body: { name: 'list', args: {}, machine: hostA.id } }),                       // 3 own machine by id
    ]));
    ac.abort();
    const u = mach(rU), a = mach(rA);
    const isListing = (r, id) => r && r.status === 200 && r.body && r.body.ok === true && r.body.result === 'host-listing ' + id;
    const notOurs = (r) => r && r.status !== 200 && !(r.body && r.body.ok === true);
    const forbidden = (r) => r && r.status === 403 && r.body && r.body.ok === false;
    // what the machines received: only requests for their own owner, each signed by the hub's key with the run as the actor
    const spki = (hex) => crypto.createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(hex, 'hex')]), format: 'der', type: 'spki' });
    const verify = (h, j) => { try {
      const sr = JSON.parse(j.headers['x-vr-machine-sig'] || ''), env = JSON.parse(sr.payload);
      const sigOk = crypto.verify(null, Buffer.concat([Buffer.from('vr-machine-request-v1\n'), Buffer.from(sr.payload)]), spki(keyOf[h.id].key), Buffer.from(sr.sig, 'hex'));
      return { sigOk, env, digestOk: env.argsDigest === crypto.createHash('sha256').update(Buffer.from(j.body || '', 'base64')).digest('hex'), toolOk: env.tool === String(j.path).replace(/^\//, ''), life: env.expiresAt - env.issuedAt, kid: sr.kid };
    } catch { return { sigOk: false, env: {} }; } };
    const vA = got[hostA.id].map((j) => verify(hostA, j)), vU = got[hostU.id].map((j) => verify(hostU, j));
    const nonces = new Set([...vA, ...vU].map((v) => v.env.nonce));
    const idA = runIdOf(rA), idU = runIdOf(rU);
    const signedOk = (v, owner, runId, h) => v.sigOk && v.digestOk && v.toolOk && v.env.v === 1 && v.env.owner === owner && v.env.actor && v.env.actor.account === owner && v.env.machine === h.id && v.life === 60 && v.kid === keyOf[h.id].kid && (runId ? v.env.actor.run === runId : true);
    // the hub's key: made once, in the data folder, owner-only, the same one every hello announces
    const keyFile = path.join(SB, 'hub-signing.key');
    const keyMode = fs.existsSync(keyFile) ? fs.statSync(keyFile).mode & 0o777 : 0;
    // clean up: the test machines go away (the later checks must not find a PC host for these accounts)
    await call('/api/hosts/remove', A, { id: hostA.id }); await call('/api/hosts/remove', U, { id: hostU.id });
    const detail323 = `browser fallback ${/fake-listing/.test(texts(fb.events))}; offline: ${early.map((r) => r.status).join('/')}; U run: ${u.map((r) => r.status).join('/')}; A run: ${a.map((r) => r.status).join('/')}; machine A got ${got[hostA.id].length} (signed ${vA.filter((v) => v.sigOk).length}), machine U got ${got[hostU.id].length} (signed ${vU.filter((v) => v.sigOk).length}); key mode ${keyMode.toString(8)}`;
    check('R323', 'per-run MCP secret and the owner check for machines (T-308): a Work run gets its OWN secret in its MCP config, the hub takes the account and session from the run record (a disagreeing account header or session is 403, the account header is not needed, a wrong secret is not accepted); a run naming ANOTHER person\'s machine (by id, by nickname or inside the arguments) gets 403 with no fallback, and so does an admin\'s run; the owner\'s own machine answers (signed by the hub\'s one ed25519 key for the owner with the run as actor, digest of the arguments, 60 s life, a fresh nonce each), the owner\'s browser-folder fallback still works when no machine is named, a named machine that is the owner\'s but offline says so instead of falling back; the old shared secret (an older run config) still works, deprecated, and is owner-checked too; machines only ever received requests for their own owner; the hub key file is owner-only',
      /fake-listing: a\.txt/.test(texts(fb.events)) && early.length === 3 && early[0].status === 503 && /offline/.test(JSON.stringify(early[0].body)) && forbidden(early[1]) && early[2].status === 404
      && Object.values(keyOf).every((k) => /^[0-9a-f]{64}$/.test(k.key) && k.kid) && keyOf[hostA.id].key === keyOf[hostU.id].key
      && u.length === 13 && isListing(u[0], hostU.id) && forbidden(u[1]) && forbidden(u[2]) && forbidden(u[3]) && forbidden(u[4]) && forbidden(u[5]) && notOurs(u[6]) && isListing(u[7], hostU.id) && isListing(u[8], hostU.id)
      && u[9].status === 200 && u[9].body.ok === true && isListing(u[10], hostU.id) && forbidden(u[11]) && notOurs(u[12])
      && a.length === 4 && forbidden(a[0]) && forbidden(a[1]) && isListing(a[2], hostA.id) && isListing(a[3], hostA.id)
      && got[hostA.id].length === 2 && got[hostU.id].length === 4 // U: probes 0, 7, 8 and the legacy 10; A: 2 and 3
      && vA.every((v) => signedOk(v, 't-admin', idA, hostA)) && vU.every((v, i) => signedOk(v, 't-user', i < 3 ? idU : '', hostU)) && nonces.size === vA.length + vU.length && !!idA && !!idU
      && keyMode === 0o600,
      detail323);
  }
  // R328, R329, R330 (T-311, linked machines 5/8): the machine's tools, over the wire, with a REAL machine runner (`vr-host machine`, the
  // library's runner) linked by device code to this scratch hub, called through a Work run's pc-tool relay like the AI does.
  if (!RUST_HUB) { for (const [id, l] of [['R328', 'machine tools: files (the pc_* mapping) and machine_info'], ['R329', 'machine tools: shell, jobs and processes'], ['R330', 'machine tools: big-file transfer by ticket']]) skip(id, l, 'the Rust hub\'s: --rust-hub runs it'); }
  else if (WIN) { for (const [id, l] of [['R328', 'machine tools: files and machine_info'], ['R329', 'machine tools: shell, jobs and processes'], ['R330', 'machine tools: big-file transfer by ticket']]) skip(id, l, 'the shell and process checks use sh and /proc; run on Linux'); }
  else await machineToolsCheck();
  // R337 (T-314, linked machines 8/8): the My machines panel and the in-use bar, with a REAL machine runner linked to this scratch hub
  if (RUST_HUB && !WIN) await r337Check();
  else skip('R337', 'the My machines panel lists only the owner\'s machines and the Work bar says what a run uses', RUST_HUB ? 'the run uses sh (shell_run); run on Linux' : 'the Rust hub\'s: --rust-hub runs it');
  // R334, R335, R336, R339 (T-313, linked machines 7/8): the apps use the library: the service install and the state folder check
  // (one template, a systemd user unit and a per-user Windows task, .sh and .cmd twins), pc_job retired for the library's door, vr-desktop
  // links this computer, jcodepgm machine, and the cross-test. Windows can't be built or run here: its half is marked pending.
  {
    const rd = (...p) => fs.readFileSync(path.join(MILLS, ...p), 'utf8');
    const cratesDir = path.join(MILLS, 'native', 'crates');
    const rsFiles = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? (e.name === 'target' ? [] : rsFiles(path.join(dir, e.name))) : e.name.endsWith('.rs') ? [path.join(dir, e.name)] : []));
    const home334 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
    const env334 = { ...process.env, RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(home334, '.rustup'), CARGO_HOME: process.env.CARGO_HOME || path.join(home334, '.cargo'), CARGO_TARGET_DIR: NATIVE_TARGET };
    env334.PATH = path.join(env334.CARGO_HOME, 'bin') + path.delimiter + (env334.PATH || '');
    const L334 = 'linked machines service install (T-313): ONE template (pgm-net-internal::machine::service) makes a systemd USER unit (Restart=on-failure, WantedBy=default.target, no User=, never root, no restart after exit 3 / 4) and a per-user Windows Scheduled Task (logon trigger, LeastPrivilege, restart on failure, no administrator, no SYSTEM) with .sh and .cmd twins; `vr-host machine install | uninstall` write and remove them; the machine refuses to start (exit 4) when the state folder, or the token / key / scope in it, is group- or world-readable (Linux) or has a broad ACL (Windows, the icacls helper)';
    const L335 = 'linked machines, the apps use the library (T-313): the host-side pc_job is gone, the AI host\'s PC role is the library\'s Machine (scope + the one canonicaliser, audit, kill switch; an older hub that signs nothing is still served as the owner), the hub still relays the pc engine to older hosts, no copy of the canonicaliser exists outside pgm-net-internal (the desktop app\'s local file paths use it too), vr-desktop links this computer and serves it while open or as the persistent service, only the Tools window may call those commands (the hub\'s page, scoped to the hub origin, cannot), and the machine parity R328 stays green';
    const L336 = 'linked machines, Jcode PGM (T-313): `jcodepgm machine link | run | install | uninstall | scope | stop | start | status` on the mirrored library, run against a fake hub (link by device code, signed calls served, unsigned / other-key / outside-folder refused, revoked wipe, state folder check, local scope edits audited, kill switch), the credential files owner-only, the library mirrored byte for byte from this repo';
    const L339 = 'linked machines cross-test (T-313): the library tests R321 to R333 and R340 run on Linux (machine:: unit tests), the Windows path rules are pure logic that run on any host, and the Windows run is documented for Gee in the pgm-net-internal README; the Windows run itself is PENDING';
    const why334 = buildNative();
    const ct334 = spawnSync('cargo', ['test', '-p', 'pgm-net-internal', '-p', 'pgm-ai-internal', '--', 'machine::', 'the_pc_role_is_the_librarys_machine'], { cwd: path.join(MILLS, 'native'), env: env334, encoding: 'utf8', timeout: 1800000 });
    if (ct334.error) { for (const [id, l] of [['R334', L334], ['R335', L335], ['R339', L339]]) skip(id, l, 'no Rust toolchain here (' + ct334.error.code + ')'); }
    else {
      const out334 = String(ct334.stdout || '') + String(ct334.stderr || '');
      const okT = (name) => new RegExp('^test ' + name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ' \\.\\.\\. ok$', 'm').test(out334);
      const nOk = (out334.match(/^test machine::[^ ]+ \.\.\. ok$/gm) || []).length;
      const svcTests = ['the_systemd_unit_is_a_user_unit_that_restarts_on_failure_and_never_runs_as_root', 'the_windows_task_is_per_user_least_privilege_and_restarts', 'sh_and_cmd_twins_exist_and_the_sh_refuses_root', 'install_writes_the_unit_and_uninstall_removes_it', 'a_group_or_world_readable_state_folder_or_secret_is_refused'];
      const svcOk = svcTests.filter((t) => okT('machine::service::tests::' + t));
      const runnerSrc = rd('native', 'crates', 'pgm-net-internal', 'src', 'machine', 'runner.rs'), hostSrc = rd('native', 'crates', 'vr-host', 'src', 'machine.rs');
      const srcOk334 = (runnerSrc.match(/check_state_dir\(&o\.state_dir\)/g) || []).length >= 2 && /fn install_sh|install_cmd/.test(rd('native', 'crates', 'pgm-net-internal', 'src', 'machine', 'service.rs')) && /"install" \| "uninstall"/.test(hostSrc) && /EXIT_REFUSED/.test(hostSrc);
      // the real program: generated files, then the refusal
      let live = { ok: false, why: why334 || 'not run' };
      const isRoot = typeof process.getuid === 'function' && process.getuid() === 0;
      if (!why334 && !WIN && !isRoot) {
        const d = fs.mkdtempSync(path.join(SCRATCH || require('os').tmpdir(), 'r334-')), state = path.join(d, 'state'), out = path.join(d, 'out');
        const vr = (...a) => spawnSync(NATIVE_BIN.host, ['machine', ...a], { encoding: 'utf8', timeout: 60000, env: { ...process.env, HOME: d, XDG_CONFIG_HOME: path.join(d, 'cfg') } });
        const inst = vr('install', '--files-only', '--state', state, '--out', out);
        const names = ['install.sh', 'uninstall.sh', 'install.cmd', 'uninstall.cmd', 'vibe-roller-machine.service', 'vibe-roller-machine.task.xml'];
        const have = names.filter((n) => fs.existsSync(path.join(out, n)));
        const rdo = (n) => (fs.existsSync(path.join(out, n)) ? fs.readFileSync(path.join(out, n), 'utf8') : '');
        const unit = rdo('vibe-roller-machine.service'), xml = rdo('vibe-roller-machine.task.xml'), cmd = rdo('install.cmd'), sh = rdo('install.sh');
        const shn = spawnSync('sh', ['-n', path.join(out, 'install.sh')], { encoding: 'utf8' });
        const unitOk = /^Restart=on-failure$/m.test(unit) && /^WantedBy=default\.target$/m.test(unit) && !/^User=/m.test(unit) && !/multi-user\.target/.test(unit) && /^RestartPreventExitStatus=3 4$/m.test(unit) && /machine run --state /.test(unit) && /^UMask=0077$/m.test(unit);
        const taskOk = /<LogonTrigger>/.test(xml) && /<RunLevel>LeastPrivilege<\/RunLevel>/.test(xml) && /<RestartOnFailure>/.test(xml) && !/HighestAvailable|LocalSystem|S-1-5-18/.test(xml);
        const twinsOk = /schtasks \/Create/.test(cmd) && !/sc create|LocalSystem/.test(cmd) && /\r\n/.test(cmd) && sh.startsWith('#!/bin/sh') && /id -u/.test(sh) && shn.status === 0;
        // the refusal: linked state (record + token), folder open to the group / the world, then a group- or world-readable token in a private folder
        fs.mkdirSync(state, { recursive: true });
        fs.writeFileSync(path.join(state, 'link.json'), JSON.stringify({ owner: 'ann', machine: 'mach-x', hub: 'http://127.0.0.1:9' })); fs.writeFileSync(path.join(state, 'token'), 'tok');
        fs.chmodSync(path.join(state, 'link.json'), 0o600); fs.chmodSync(path.join(state, 'token'), 0o600);
        const refuse = [];
        for (const [dirMode, tokMode] of [[0o755, 0o600], [0o750, 0o600], [0o700, 0o640], [0o700, 0o604]]) {
          fs.chmodSync(state, dirMode); fs.chmodSync(path.join(state, 'token'), tokMode);
          const r = vr('run', '--state', state);
          refuse.push(r.status === 4 && /will not start/.test(r.stderr || '') && /chmod (700|600)/.test(r.stderr || ''));
        }
        live = { ok: inst.status === 0 && have.length === names.length && unitOk && taskOk && twinsOk && refuse.every(Boolean), why: `install ${inst.status} files ${have.length}/${names.length} unit ${unitOk} task ${taskOk} twins ${twinsOk} refusal ${refuse.map((b) => (b ? 'exit4' : 'NO')).join('/')}` };
      } else if (WIN) live = { ok: true, why: 'the live shell checks run on Linux; the Windows run is in the README' };
      else if (isRoot) live = { ok: true, why: 'running as root: `machine install` refuses root by design, the live check is skipped (the unit tests cover the text)' };
      check('R334', L334, ct334.status === 0 && svcOk.length === svcTests.length && srcOk334 && live.ok, `cargo test ${ct334.status}: service tests ${svcOk.length}/${svcTests.length}; ${live.why}; Windows: the ACL test and the task are written but not run here (no Windows)`);

      // ---- R335
      const allRs = rsFiles(cratesDir), outside = allRs.filter((f) => !f.includes(path.join('crates', 'pgm-net-internal') + path.sep));
      const oldPc = allRs.filter((f) => /\bfn pc_job\b/.test(fs.readFileSync(f, 'utf8')));
      const copies = outside.filter((f) => { const t = fs.readFileSync(f, 'utf8'); return /contains\(&"\.\."\)/.test(t) || (/go above the (shared|projects) folder/.test(t) && !/canonicalize_in/.test(t)); }).map((f) => path.relative(MILLS, f));
      const runnerAi = rd('native', 'crates', 'pgm-ai-internal', 'src', 'host', 'runner.rs'), aiHosts = rd('native', 'crates', 'pgm-ai-internal', 'src', 'ai_hosts.rs');
      const pcRole = /async fn machine_job\(/.test(runnerAi) && /machine::exec::Machine/.test(runnerAi) && /\.handle\(&tool, &body, &sig\)/.test(runnerAi) && /\.run_as\(/.test(runnerAi) && !/read_dir\(&full\)|remove_dir_all\(&full\)/.test(runnerAi) && /h\.track\(machine_job\(h2, job\)\)/.test(runnerAi);
      const hubKeeps = /self\.relay\(host, "pc", &format!\("\/\{name\}"\)/.test(aiHosts) && fs.existsSync(path.join(cratesDir, 'pgm-net-internal', 'src', 'compat.rs')) && /an_older_app_keeps_its_pc_engine/.test(rd('native', 'crates', 'pgm-net-internal', 'src', 'compat.rs'));
      const deskMain = rd('native', 'crates', 'vr-desktop', 'src', 'main.rs'), deskMachine = rd('native', 'crates', 'vr-desktop', 'src', 'machine.rs'), deskLocal = rd('native', 'crates', 'vr-desktop', 'src', 'local.rs');
      const hubCapSrc = (/CapabilityBuilder::new\("hub"\)[\s\S]*?\)\)\?;/.exec(deskMain) || [''])[0];
      const toolsCap = JSON.parse(rd('native', 'crates', 'vr-desktop', 'capabilities', 'tools.json')), buildRs = rd('native', 'crates', 'vr-desktop', 'build.rs');
      const machCmds = ['machine_status', 'machine_link', 'machine_act', 'machine_service'];
      const deskOk = machCmds.every((c) => new RegExp('fn ' + c + '\\(').test(deskMain) && buildRs.includes('"' + c + '"') && toolsCap.permissions.includes('allow-' + c.replace(/_/g, '-')) && deskMain.includes(c)) && hubCapSrc.length > 50 && !/machine/.test(hubCapSrc) && /\.remote\(format!\("\{hub\}\/\*"\)\)/.test(hubCapSrc)
        && /Runner::open/.test(deskMachine) && /runner::link/.test(deskMachine) && /Unit::new/.test(deskMachine) && /option_env!\("VR_APP_NAME"\)/.test(deskMachine) && /fn cli\(/.test(deskMachine) && /machine::cli\(/.test(deskMain) && /canonicalize_in/.test(deskLocal)
        && /is_hub_origin\(page, hub\)/.test(rd('native', 'crates', 'vr-desktop', 'src', 'capture.rs')) && /mode = "while open"/.test(deskMachine);
      const r328 = results.find((r) => r.id === 'R328');
      const parityOk = RUST_HUB ? !!r328 && r328.status === 'PASS' : true;
      const cdesk = why334 ? { status: 77 } : spawnSync('sh', ['-c', `export RUSTUP_HOME="${env334.RUSTUP_HOME}" CARGO_HOME="${env334.CARGO_HOME}" CARGO_TARGET_DIR="${NATIVE_TARGET}" OWNER_HOME="${home334}"; export PATH="$CARGO_HOME/bin:$PATH"; . ./tauri-env.sh; [ "$HAVE_TAURI_LINUX" = 1 ] || exit 77; cargo test --release -q -p vr-desktop -- machine local 2>&1`], { cwd: path.join(MILLS, 'native'), encoding: 'utf8', timeout: 1800000 });
      const deskTests = cdesk.status === 77 ? 'desktop unit tests not run (no WebKitGTK sysroot)' : `desktop unit tests ${cdesk.status}`;
      check('R335', L335, ct334.status === 0 && okT('host::runner::tests::the_pc_role_is_the_librarys_machine') && oldPc.length === 0 && copies.length === 0 && pcRole && hubKeeps && deskOk && parityOk && (cdesk.status === 77 || cdesk.status === 0),
        `pc_job in the source: ${oldPc.length ? oldPc.map((f) => path.relative(MILLS, f)).join(',') : 'none'}; canonicaliser copies outside pgm-net-internal: ${copies.join(',') || 'none'} (the frozen JavaScript host keeps its own); pc role ${pcRole}; hub still relays pc ${hubKeeps}; desktop ${deskOk}; ${deskTests}; R328 parity ${RUST_HUB ? (r328 ? r328.status : 'missing') : 'runs with --rust-hub'}`);

      // ---- R339
      const readme = rd('native', 'crates', 'pgm-net-internal', 'README.md');
      const winDoc = /Running the linked-machines tests on Windows \(R339\)/.test(readme) && /cargo test -p pgm-net-internal machine::/.test(readme) && /vr-host\.exe machine install --files-only/.test(readme) && /schtasks \/Query/.test(readme);
      const winTests = (fs.readFileSync(path.join(cratesDir, 'pgm-net-internal', 'src', 'machine', 'paths.rs'), 'utf8').match(/fn w\(|Flavor::Windows/g) || []).length;
      check('R339', L339, ct334.status === 0 && nOk >= 80 && !/FAILED|panicked/.test(out334) && winDoc && winTests >= 5,
        `Linux: cargo test ${ct334.status}, ${nOk} machine:: tests ok; Windows path rules are pure logic (${winTests} Windows-flavour uses in paths.rs) and run here; Windows run: PENDING (Gee: the README commands; the cfg(windows) ACL test and the Scheduled Task were never run)`);
    }

    // ---- R336 (Jcode PGM): its tests live in the Jcode PGM repo; here, the files, the mirror and the test list
    const jc = [process.env.JCODE_PGM_DIR, path.join(MILLS, '..', 'jc-' + path.basename(MILLS).replace(/^vr-/, '')), path.join(MILLS, '..', 'Jcode-PGM'), '/srv/apps/sharedwork/Jcode-PGM'].filter(Boolean).find((d) => fs.existsSync(path.join(d, 'crates', 'jcode-machine', 'src', 'tests.rs')));
    if (!jc) skip('R336', L336, 'no Jcode PGM checkout with crates/jcode-machine beside this repo (JCODE_PGM_DIR=<dir> points at one); its tests: cargo test -p jcode-machine');
    else {
      const jr = (...p) => fs.readFileSync(path.join(jc, ...p), 'utf8');
      const mirrorDir = path.join(jc, 'pgm', 'libs', 'pgm-net-internal', 'src'), srcDir = path.join(MILLS, 'native', 'crates', 'pgm-net-internal', 'src');
      const same = (rel) => { try { return fs.readFileSync(path.join(mirrorDir, rel)).equals(fs.readFileSync(path.join(srcDir, rel))); } catch { return false; } };
      const rels = ['client.rs', ...fs.readdirSync(path.join(srcDir, 'machine')).map((f) => path.join('machine', f))];
      const stale = rels.filter((r) => !same(r));
      const tj = jr('crates', 'jcode-machine', 'src', 'tests.rs'), lj = jr('crates', 'jcode-machine', 'src', 'lib.rs'), cj = jr('src', 'cli', 'machine.rs'), aj = jr('src', 'cli', 'args.rs');
      const names = ['link_then_run_serves_signed_calls_and_refuses_the_rest', 'unlinked_on_the_web_wipes_the_token_and_exits_3', 'a_state_folder_others_can_read_is_refused_before_any_call', 'scope_shows_and_edits_locally_and_the_audit_names_this_terminal', 'stop_ends_all_access_until_start_here', 'the_default_hub_is_the_one_hub_login_signed_in_to', 'the_service_is_jcodepgm_machine_run_as_a_user_unit_and_a_per_user_task'];
      const hasTests = names.filter((n) => tj.includes('fn ' + n));
      const cli = /enum MachineCommand/.test(aj) && ['Link', 'Run', 'Install', 'Uninstall', 'Scope', 'Stop', 'Start', 'Status'].every((v) => new RegExp('\\b' + v + '\\b').test(aj.slice(aj.indexOf('enum MachineCommand')))) && /MachineCommand::Scope/.test(cj) && /Command::Machine/.test(jr('src', 'cli', 'dispatch.rs'));
      const wiring = /fn fake_hub\(/.test(tj) && /mode\(&state\), 0o700/.test(tj) && /0o600/.test(tj) && /pgm_net_internal::machine::runner::\{self, Ended, LinkRecord, Options, Runner\}/.test(lj) && /LocalApproval::from_machine\(APPROVER\)/.test(lj) && /jcode_hub_login::load/.test(lj) && /pgm-net-internal = \{ path = "\.\.\/\.\.\/pgm\/libs\/pgm-net-internal" \}/.test(jr('crates', 'jcode-machine', 'Cargo.toml'));
      let ran = '';
      let jt = { status: 0 };
      if (process.env.VR_RUN_JCODE_TESTS === '1') { jt = spawnSync('cargo', ['test', '-p', 'jcode-machine'], { cwd: jc, env: { ...env334, CARGO_TARGET_DIR: path.join(jc, 'target') }, encoding: 'utf8', timeout: 1800000 }); ran = ` cargo test -p jcode-machine ${jt.status}`; }
      check('R336', L336, stale.length === 0 && hasTests.length === names.length && cli && wiring && jt.status === 0,
        `Jcode PGM at ${jc}: mirror stale files ${stale.join(',') || 'none'}; fake-hub tests ${hasTests.length}/${names.length}; CLI ${cli}; wiring ${wiring};${ran || ' the tests run with: cargo test -p jcode-machine (VR_RUN_JCODE_TESTS=1 runs them here)'}`);
    }
  }
  async function machineToolsCheck() {
    const mdir = fs.mkdtempSync(path.join(SCRATCH, 'r328-'));
    const procs = [], cleanups = [];
    const encSpec = (spec) => 'MACHTOOL=' + Buffer.from(JSON.stringify(spec)).toString('hex');
    const machOf = (r) => { const l = texts(r.events).split('\n').find((x) => x.startsWith('MACH:')); try { return JSON.parse(l.slice(5)); } catch { return []; } };
    const pc = (name, args, machine) => ({ route: 'pc-tool', body: { name, args: args || {}, ...(machine ? { machine } : {}) } });
    const via = async (specs) => machOf(await chat(U, encSpec(specs)));       // one Work run, the calls in order
    const one = async (name, args, machine) => (await via([pc(name, args, machine)]))[0] || { status: 0, body: {} };
    const okB = (r) => !!(r && r.body && r.body.ok === true), res = (r) => (r && r.body ? r.body.result : undefined), errOf = (r) => String((r && r.body && r.body.error) || '');
    const tree = (dir, rel = '') => { const o = {}; for (const e of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) { const p = rel ? rel + '/' + e.name : e.name; if (e.isDirectory()) { o[p + '/'] = 1; Object.assign(o, tree(dir, p)); } else o[p] = fs.readFileSync(path.join(dir, p), 'utf8'); } return o; };
    const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
    const mcall = (method, p, tok, body) => fetch(H + p, { method, headers: { 'Content-Type': 'application/json', ...(tok ? { Authorization: 'Bearer ' + tok } : {}) }, body: body !== undefined ? JSON.stringify(body) : undefined }).then(async (r) => ({ status: r.status, data: await r.json().catch(() => ({})) })).catch((e) => ({ status: 0, data: { error: e.message } }));
    const linkMachine = (tag, approver, name) => new Promise((resolve) => {
      const state = path.join(mdir, tag + '-state'), home = path.join(mdir, tag + '-home'); fs.mkdirSync(home, { recursive: true });
      const p = spawn(NATIVE_BIN.host, ['machine', 'link', '--hub', H, '--folder', home, '--state', state, '--name', name], { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, VR_MACHINE_ALLOW_ROOT: '1' } });
      let out = '', approved = false;
      p.stdout.on('data', (d) => { out += d; const m = /Code: ([A-Z]{4}-[A-Z]{4})/.exec(out); if (m && !approved) { approved = true; call('/api/device/approve', approver, { userCode: m[1] }); } });
      p.stderr.on('data', (d) => { out += d; });
      p.on('close', (code) => { let id = ''; try { id = JSON.parse(fs.readFileSync(path.join(state, 'link.json'), 'utf8')).machine; } catch {} resolve({ code, out, state, home: fs.realpathSync(home), tag, id, name }); });
    });
    const runMachine = (m) => { const log = fs.openSync(path.join(mdir, m.tag + '.log'), 'a'); const p = spawn(NATIVE_BIN.host, ['machine', 'run', '--state', m.state], { stdio: ['ignore', log, log], env: { ...process.env, VR_MACHINE_ALLOW_ROOT: '1' } }); procs.push(p); m.proc = p; m.exited = new Promise((r) => p.once('exit', (c) => r(c))); return p; };
    const setScope = (m, doc) => { fs.writeFileSync(path.join(m.state, 'scope.json'), JSON.stringify({ v: 1, ...doc })); };
    const folderScope = (m, extra) => ({ preset: 'folder', paths: [{ path: m.home, access: 'rw' }], ...(extra || {}) });
    const waitOnline = async (tok, ids) => { for (let i = 0; i < 80; i++) { const ms = (await call('/api/machines', tok)).data.machines || []; if (ids.every((id) => ms.some((x) => x.id === id && x.online))) return true; await sleep(250); } return false; };
    const tokOf = (m) => fs.readFileSync(path.join(m.state, 'token'), 'utf8').trim();
    const created = (await call('/api/admin/create', A, { username: 't-r330', display: 'R330' })).data || {};
    const X = created.password ? (await login('t-r330', created.password, 'device-r330-xxxxxxxxxx')).data.token : '';
    const detail = {};
    try {
      // 0. with no machine and no host linked, the browser folder answers as it always did
      const fbBefore = await chat(U, 'PCTOOL r328 before any machine');
      const fallbackBefore = /fake-listing/.test(texts(fbBefore.events));
      const [m1, m2, m2b, m3] = await Promise.all([linkMachine('m1', U, 'r328-box'), linkMachine('m2', U, 'r330-box'), linkMachine('m2b', U, 'r330-other'), linkMachine('m3', X, 'r330-x')]);
      detail.linked = [m1, m2, m2b, m3].map((m) => m.code + ':' + (m.id ? 'id' : 'noid')).join(',');
      runMachine(m1);
      const online = await waitOnline(U, [m1.id]);
      // a real pc_job host (the AI host's own five tools) on its own folder, for the parity
      const madeHost = await call('/api/hosts/new', U, {}), hostId = (madeHost.data.host || {}).id || '', hcfg = (await call('/api/hosts/config', U, { id: hostId })).data.config || {};
      const hdir = path.join(mdir, 'host'), hhome = path.join(mdir, 'host-home'); fs.mkdirSync(hdir, { recursive: true }); fs.mkdirSync(hhome, { recursive: true });
      fs.writeFileSync(path.join(hdir, 'ai-node.json'), JSON.stringify({ hub: H, id: hostId, token: hcfg.token, user: 't-user', autoUpdate: false, engines: {}, providers: [], pc: { folder: hhome } }), { mode: 0o600 });
      const hlog = fs.openSync(path.join(mdir, 'host.log'), 'a');
      const hostProc = spawn(NATIVE_BIN.host, ['host', '--config', path.join(hdir, 'ai-node.json')], { cwd: hdir, stdio: ['ignore', hlog, hlog] }); procs.push(hostProc);
      for (let i = 0; i < 60; i++) { const hs = (await call('/api/check', U)).data.aiNodes || []; if (hs.some((x) => x.id === hostId && x.online)) break; await sleep(250); }

      // ===== R328: files through the machine = files through pc_job; ro refuses writes; machine_info =====
      const ops = [['list', {}], ['mkdir', { path: 'sub/deep' }], ['write', { path: 'sub/a.txt', content: 'héllo wörld' }], ['write', { path: 'top.txt', content: 'x' }], ['list', {}], ['list', { path: 'sub' }], ['read', { path: 'sub/a.txt' }], ['read', { path: 'nope.txt' }],
        ['delete', { path: 'sub/deep' }], ['delete', { path: 'nothing' }], ['write', { path: '', content: 'z' }], ['delete', { path: '' }], ['write', { path: '../escape.txt', content: 'no' }], ['read', { path: '/etc/hostname' }], ['list', { path: './sub/' }], ['write', { path: 'sub/./b.txt', content: '' }], ['list', { path: 'sub' }]];
      const hostRes = await via(ops.map(([n, a]) => pc(n, a, hostId))), machRes = await via(ops.map(([n, a]) => pc(n, a, m1.id)));
      const sameOp = ops.map((o, i) => { const h = hostRes[i], m = machRes[i]; if (!h || !m || h.status !== m.status) return false; if (okB(h) !== okB(m)) return false; if (okB(h)) return res(h) === res(m); return o[1].path === '' ? errOf(h) === errOf(m) : true; });
      const treesEqual = JSON.stringify(tree(hhome)) === JSON.stringify(tree(m1.home));
      detail.parity = sameOp.map((b, i) => b ? '' : ops[i][0] + '#' + i).filter(Boolean).join(',') || 'all same';
      const noEscape = !fs.existsSync(path.join(mdir, 'escape.txt'));
      const info = await one('machine_info', {}, m1.id), inf = res(info) || {};
      // read-only: reads work, every change is refused and nothing changes; the write tools are not even offered
      setScope(m1, { preset: 'folders', paths: [{ path: m1.home, access: 'ro' }] });
      const ro = await via([pc('read', { path: 'top.txt' }, m1.id), pc('write', { path: 'n.txt', content: 'x' }, m1.id), pc('delete', { path: 'top.txt' }, m1.id), pc('mkdir', { path: 'dro' }, m1.id), pc('machine_info', {}, m1.id), pc('transfer_put', { path: 'p.bin', from: '0'.repeat(32) }, m1.id)]);
      const roInfo = res(ro[4]) || {};
      const roKept = fs.existsSync(path.join(m1.home, 'top.txt')) && !fs.existsSync(path.join(m1.home, 'n.txt')) && !fs.existsSync(path.join(m1.home, 'dro'));
      setScope(m1, folderScope(m1));
      const auditFile = path.join(m1.state, 'machine-audit.jsonl'), auditText = fs.existsSync(auditFile) ? fs.readFileSync(auditFile, 'utf8') : '';
      const auditLines = auditText.split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return {}; } });
      const stMode = fs.statSync(m1.state).mode & 0o777, tokMode = fs.statSync(path.join(m1.state, 'token')).mode & 0o777;
      const card = ((await call('/api/machines', U)).data.machines || []).find((x) => x.id === m1.id) || {};
      // done with the pc_job host: it must not answer the later fallback checks
      await call('/api/hosts/remove', U, { id: hostId }); hostProc.kill();
      check('R328', 'T-311 machine tools, files: a machine linked by device code and run by the library\'s runner answers mcp__pc__list / read / write / delete / mkdir through a Work run exactly as today\'s pc_job does for the same folder (same results for success, a refusal where pc_job refuses, the same files afterwards; paths are relative to the selected folder; nothing written outside it), a read-only scope reads but refuses write / delete / mkdir and does not offer the write tools, machine_info says os, arch, the scope in words, the tools it offers and the shell\'s limits, every call leaves an audit line (denials too, never file contents) and the state folder is owner-only; mcp__pc__* still works with no machine linked (the browser folder)',
        fallbackBefore && [m1, m2, m2b, m3].every((m) => m.code === 0 && m.id) && online && !!hostId && hostRes.length === ops.length && machRes.length === ops.length && sameOp.every(Boolean) && treesEqual && noEscape
        && okB(info) && inf.os === 'linux' && typeof inf.arch === 'string' && /^one folder: /.test(inf.scope || '') && inf.access === 'read-write' && ['machine_info', 'list', 'read', 'write', 'delete', 'mkdir', 'shell_run', 'job_start', 'job_read', 'job_stop', 'proc_kill', 'transfer_get', 'transfer_put'].every((t) => (inf.tools || []).includes(t)) && /^advisory/.test((inf.shell || {}).confinement || '') && (inf.shell || {}).elevation === 'never'
        && okB(ro[0]) && res(ro[0]) === 'x' && [1, 2, 3].every((i) => !okB(ro[i])) && roKept && roInfo.access === 'read-only' && !(roInfo.tools || []).includes('write') && (roInfo.tools || []).includes('read') && !okB(ro[5])
        && auditLines.length >= 20 && auditLines.some((l) => l.decision === 'deny') && auditLines.some((l) => l.decision === 'allow') && auditLines.every((l) => l.hash && l.actor) && !auditText.includes('héllo') && stMode === 0o700 && tokMode === 0o600 && /^(one folder|folders): /.test(card.scope || ''),
        `${detail.linked} · parity ${detail.parity} · trees ${treesEqual} · ro ${ro.map((r) => okB(r) ? 'ok' : 'no').join('/')} · audit ${auditLines.length} lines (deny ${auditLines.some((l) => l.decision === 'deny')}, noactor ${auditLines.filter((l) => !l.actor || !l.hash).length}) · fb ${fallbackBefore} online ${online} state ${stMode.toString(8)} token ${tokMode.toString(8)} card "${card.scope}" tools ${(inf.tools || []).join()} access ${inf.access} elev ${(inf.shell || {}).elevation} conf ${(inf.shell || {}).confinement} ro ${roInfo.access} ${(roInfo.tools || []).join()}`);

      // ===== R329: shell, jobs, processes =====
      const home1 = m1.home;
      setScope(m1, folderScope(m1, { tools: { shell: { enabled: true, timeoutSec: 6, maxOutputKB: 1 } }, limits: { concurrentJobs: 2 } }));
      const sh = await via([pc('shell_run', { cmd: 'pwd; echo err >&2; exit 3' }, m1.id), pc('mkdir', { path: 'sub' }, m1.id), pc('shell_run', { cmd: 'pwd; touch made.txt', cwd: 'sub' }, m1.id),
        pc('shell_run', { cmd: 'pwd', cwd: '..' }, m1.id), pc('shell_run', { cmd: 'pwd', cwd: '/' }, m1.id), pc('shell_run', { cmd: 'pwd', cwd: '/etc' }, m1.id), pc('shell_run', { cmd: 'pwd', cwd: 'sub/../..' }, m1.id), pc('shell_run', { cmd: 'pwd', cwd: home1 }, m1.id),
        pc('shell_run', { cmd: "head -c 100000 /dev/zero | tr '\\0' x", timeoutSec: 99 }, m1.id), pc('shell_run', { cmd: '   ' }, m1.id)]);
      const tShort = Date.now(); const slow1 = await one('shell_run', { cmd: 'sleep 30', timeoutSec: 1 }, m1.id); const msShort = Date.now() - tShort;
      const tCap = Date.now(); const slow2 = await one('shell_run', { cmd: 'sleep 30' }, m1.id); const msCap = Date.now() - tCap;
      // jobs: run A starts a long one; "reload" = other runs read it by id; a pause from the web and back (the link drops and returns); then stop
      const jobA = await one('job_start', { cmd: 'echo $$ > jobpid.txt; echo first; sleep 2; echo second; sleep 120', timeoutSec: 300 }, m1.id), jid = (res(jobA) || {}).jobId || '';
      await sleep(900);
      const rd1 = await one('job_read', { jobId: jid }, m1.id), r1 = res(rd1) || {};
      const pausedNow = await mcall('PATCH', '/api/machines/' + m1.id, U, { paused: true });
      const whilePaused = await one('job_read', { jobId: jid }, m1.id);
      await mcall('PATCH', '/api/machines/' + m1.id, U, { paused: false });
      await waitOnline(U, [m1.id]); await sleep(1800);
      const rd2 = await one('job_read', { jobId: jid, since: r1.next || 0 }, m1.id), r2 = res(rd2) || {};
      const listed = res(await one('job_read', {}, m1.id)) || {};
      const jobB = await one('job_start', { cmd: 'sleep 100' }, m1.id), jidB = (res(jobB) || {}).jobId || '';
      const jobC = await one('job_start', { cmd: 'sleep 100' }, m1.id); // over concurrentJobs: 2
      const jpid = Number((fs.existsSync(path.join(home1, 'jobpid.txt')) ? fs.readFileSync(path.join(home1, 'jobpid.txt'), 'utf8') : '0').trim());
      const stopA = await one('job_stop', { jobId: jid }, m1.id); await sleep(300);
      const jaDead = !alive(jpid);
      // processes: an unrelated one is untouchable, a job's own can be killed
      const other = spawn('sleep', ['300'], { stdio: 'ignore' }); cleanups.push(() => { try { other.kill(); } catch {} });
      const pl = res(await one('proc_list', {}, m1.id)) || {};
      const killOther = await one('proc_kill', { pid: other.pid }, m1.id), otherAlive = alive(other.pid);
      const bPid = ((res(await one('job_read', { jobId: jidB }, m1.id)) || {}).pid) || 0;
      const killOwn = await one('proc_kill', { pid: bPid }, m1.id); await sleep(500);
      const bDead = bPid > 1 && !alive(bPid);
      const killInit = await one('proc_kill', { pid: 1 }, m1.id);
      // no-shell: the shell tools disappear, files stay
      setScope(m1, folderScope(m1, { preset: 'no-shell', tools: { shell: { enabled: false } } }));
      const ns = await via([pc('machine_info', {}, m1.id), pc('shell_run', { cmd: 'echo hi' }, m1.id), pc('job_start', { cmd: 'echo hi' }, m1.id), pc('list', {}, m1.id)]);
      const nsTools = (res(ns[0]) || {}).tools || [];
      setScope(m1, folderScope(m1));
      // revoke: a running job dies with the link, the runner wipes its token and key and exits
      const jobR = await one('job_start', { cmd: 'echo $$ > revpid.txt; sleep 300' }, m1.id); await sleep(500);
      const rpid = Number((fs.existsSync(path.join(home1, 'revpid.txt')) ? fs.readFileSync(path.join(home1, 'revpid.txt'), 'utf8') : '0').trim());
      const rAlive = alive(rpid);
      const unlink = await mcall('DELETE', '/api/machines/' + m1.id, U);
      const exitCode = await Promise.race([m1.exited, sleep(12000).then(() => 'timeout')]);
      await sleep(300);
      const rDead = rpid > 1 && !alive(rpid), tokenGone = !fs.existsSync(path.join(m1.state, 'token')) && !fs.existsSync(path.join(m1.state, 'device-key'));
      const afterRevoke = await one('shell_run', { cmd: 'echo hi' }, m1.id);
      const fbAfter = await chat(U, 'PCTOOL r329 after the machine is gone');
      const fallbackAfter = /fake-listing/.test(texts(fbAfter.events));
      const shellSrc = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-net-internal', 'src', 'machine', 'exec.rs'), 'utf8');
      check('R329', 'T-311 machine tools, shell / jobs / processes: shell_run runs in the selected folder or a folder inside the scope (cwd .., /, /etc and sub/../.. are refused, an absolute path inside the folder works), has a timeout (the call\'s own, never above the scope\'s) and an output cap (truncated is said), says its confinement is advisory and is never run as root; no-shell removes shell_run / job_* (and says so) while files work; job_start / job_read / job_stop keep a job on the machine across runs, a pause from the web and back, and a reload (read by job id, the list finds it), refuse past concurrentJobs, and a stopped job\'s process is gone; proc_kill with kill own-children kills a job\'s process and refuses an unrelated one and pid 1; an unlink from the web ends running jobs, the runner wipes its token and key and exits, and then the browser folder answers mcp__pc__* again',
        okB(sh[0]) && res(sh[0]).stdout.trim() === home1 && res(sh[0]).stderr.trim() === 'err' && res(sh[0]).exit === 3 && /advisory/.test(res(sh[0]).note) && okB(sh[2]) && res(sh[2]).stdout.trim() === path.join(home1, 'sub') && fs.existsSync(path.join(home1, 'sub', 'made.txt'))
        && [3, 4, 5, 6].every((i) => !okB(sh[i])) && okB(sh[7]) && res(sh[7]).stdout.trim() === home1 && okB(sh[8]) && res(sh[8]).truncated === true && res(sh[8]).stdout.length <= 1024 && !okB(sh[9])
        && okB(slow1) && res(slow1).timedOut === true && msShort < 9000 && okB(slow2) && res(slow2).timedOut === true && msCap < 12000 && msCap >= 4000
        && !!jid && okB(rd1) && r1.running === true && /first/.test(r1.output || '') && !/second/.test(r1.output || '') && pausedNow.status === 200 && !okB(whilePaused) && /paused/.test(errOf(whilePaused))
        && okB(rd2) && r2.running === true && /second/.test(r2.output || '') && !/first/.test(r2.output || '') && (listed.jobs || []).some((j) => j.jobId === jid) && !!jidB && !okB(jobC) && /running already/.test(errOf(jobC))
        && jpid > 1 && okB(stopA) && res(stopA).state === 'stopped' && jaDead
        && !okB(killOther) && /not started by a tool/.test(errOf(killOther)) && otherAlive && (pl.processes || []).some((p) => p.pid === other.pid && p.ownChild === false) && bPid > 1 && okB(killOwn) && res(killOwn).ownChild === true && bDead && !okB(killInit)
        && nsTools.includes('list') && ['shell_run', 'job_start', 'job_read', 'job_stop'].every((t) => !nsTools.includes(t)) && !okB(ns[1]) && /does not offer shell_run/.test(errOf(ns[1])) && !okB(ns[2]) && okB(ns[3])
        && okB(jobR) && rpid > 1 && rAlive && unlink.status === 200 && exitCode === 3 && rDead && tokenGone && !okB(afterRevoke) && fallbackAfter && /fn is_elevated\(/.test(shellSrc),
        `shell ${sh.map((r) => okB(r) ? 'ok' : 'no').join('/')} · timeout ${msShort} ms / cap ${msCap} ms · job ${jid ? 'started' : 'NOT started'} reads ${r1.running}/${r2.running} paused ${errOf(whilePaused).slice(0, 40)} · killOther ${errOf(killOther).slice(0, 60)} · revoke: unlink ${unlink.status} exit ${exitCode} dead ${rDead} wiped ${tokenGone} fallback ${fallbackAfter}`);

      // ===== R330: big-file transfer by ticket =====
      runMachine(m2); await waitOnline(U, [m2.id]);
      const SIZE = 512 << 20, big = path.join(m2.home, 'big.bin');
      { const fd = fs.openSync(big, 'w'); fs.ftruncateSync(fd, SIZE); const stamp = (at, s) => fs.writeSync(fd, Buffer.from(s.repeat(64)), 0, 64 * s.length, at); stamp(0, 'head-of-the-file;'); stamp(SIZE / 2, 'middle-of-the-file;'); stamp(SIZE - 2048, 'end-of-the-file;'); fs.closeSync(fd); }
      const shaOf = (file) => new Promise((resolve, reject) => { const h = crypto.createHash('sha256'); fs.createReadStream(file, { highWaterMark: 1 << 20 }).on('data', (d) => h.update(d)).on('end', () => resolve(h.digest('hex'))).on('error', reject); });
      const wantSha = await shaOf(big);
      const hubPid = (() => { try { for (const d of fs.readdirSync('/proc')) { if (!/^\d+$/.test(d)) continue; try { if (fs.readFileSync('/proc/' + d + '/cmdline', 'utf8').split('\0')[0] === process.env.VR_HUB_BIN) return +d; } catch {} } } catch {} return 0; })();
      const mem = () => { try { const s = fs.readFileSync('/proc/' + hubPid + '/status', 'utf8'); return { rss: +(/VmRSS:\s+(\d+)/.exec(s) || [])[1] * 1024, hwm: +(/VmHWM:\s+(\d+)/.exec(s) || [])[1] * 1024 }; } catch { return null; } };
      const mem0 = mem();
      const tStart = Date.now();
      const first = await one('transfer_get', { path: 'big.bin', maxBytes: 100 << 20, ticketSecs: 10 }, m2.id), f1 = res(first) || {};
      const tFirstDone = Date.now(); // the ticket (10 s) was issued no later than this
      const partFile = path.join(SB, 'transfers', 't-user', (f1.id || 'x') + '.part');
      const partSize = fs.existsSync(partFile) ? fs.statSync(partFile).size : -1;
      const tk = f1.ticket || 'no-ticket', st = (tok, ticket, body) => mcall('POST', '/api/machine/transfer/' + ticket + '/status', tok, body);
      const goodBody = { path: 'big.bin', key: f1.id };
      const rOther = await st(tokOf(m3), tk, goodBody), rSibling = await st(tokOf(m2b), tk, goodBody), rPath = await st(tokOf(m2), tk, { path: 'other.bin', key: f1.id }), rPage = await st(U, tk, goodBody), rNone = await st(null, tk, goodBody), rBogus = await st(tokOf(m2), 'f'.repeat(48), goodBody);
      const rGood = await st(tokOf(m2), tk, goodBody);
      const rChunkOther = await fetch(H + '/api/machine/transfer/' + tk + '/chunk?offset=' + (100 << 20) + '&key=' + f1.id + '&path=big.bin', { method: 'POST', headers: { Authorization: 'Bearer ' + tokOf(m3), 'Content-Type': 'application/octet-stream' }, body: Buffer.alloc(16) }).then((r) => r.status);
      const withinTicket = Date.now() - tStart < 9500;
      await sleep(Math.max(0, 11000 - (Date.now() - tStart), 10500 - (Date.now() - tFirstDone)));
      const rLate = await st(tokOf(m2), tk, goodBody);
      // resume: the same call again carries on at the byte the hub holds, and ends with the sha256 of the whole file
      const second = await one('transfer_get', { path: 'big.bin' }, m2.id), f2 = res(second) || {};
      const hubFile = path.join(SB, 'transfers', 't-user', f2.id || 'x');
      const hubSha = fs.existsSync(hubFile) ? await shaOf(hubFile) : '';
      // the owner downloads it from the hub, streamed; Range works; another account has none of it
      const dl = await fetch(H + '/api/transfers/' + f2.id, { headers: { Authorization: 'Bearer ' + U } });
      const dh = crypto.createHash('sha256'); let dn = 0; for await (const c of dl.body) { dh.update(c); dn += c.length; }
      const rng = await fetch(H + '/api/transfers/' + f2.id, { headers: { Authorization: 'Bearer ' + U, Range: 'bytes=0-63' } });
      const rngBuf = Buffer.from(await rng.arrayBuffer()), rngBad = await fetch(H + '/api/transfers/' + f2.id, { headers: { Authorization: 'Bearer ' + U, Range: 'bytes=' + (SIZE + 5) + '-' } });
      const dlOther = await mcall('GET', '/api/transfers/' + f2.id, X), listU = ((await call('/api/transfers', U)).data.transfers || []).map((t) => t.id), listX = ((await call('/api/transfers', X)).data.transfers || []).map((t) => t.id);
      // the other way: the owner uploads to the hub, the machine pulls it down in pieces with a resume, verified, then put in place
      const UP = 200 << 20, upFile = path.join(mdir, 'up.bin');
      { const fd = fs.openSync(upFile, 'w'); fs.ftruncateSync(fd, UP); fs.writeSync(fd, Buffer.from('upload-start;'.repeat(40)), 0, 520, 0); fs.writeSync(fd, Buffer.from('upload-end;'.repeat(40)), 0, 440, UP - 440); fs.closeSync(fd); }
      const upSha = await shaOf(upFile);
      const upload = async (tok, file, size, id) => { const fh = fs.openSync(file, 'r'); const buf = Buffer.alloc(4 << 20); let off = 0; while (off < size) { const n = fs.readSync(fh, buf, 0, buf.length, off); const r = await fetch(H + '/api/transfers/' + id + '/chunk?offset=' + off, { method: 'POST', headers: { Authorization: 'Bearer ' + tok, 'Content-Type': 'application/octet-stream' }, body: buf.subarray(0, n) }); if (r.status !== 200) break; off += n; } fs.closeSync(fh); return off; };
      const slot = (await call('/api/transfers', U, { name: 'up.bin' })).data || {};
      const uploaded = await upload(U, upFile, UP, slot.id);
      const upDone = await mcall('POST', '/api/transfers/' + slot.id + '/done', U, {});
      const upBad = await fetch(H + '/api/transfers/' + slot.id + '/chunk?offset=0', { method: 'POST', headers: { Authorization: 'Bearer ' + U, 'Content-Type': 'application/octet-stream' }, body: Buffer.alloc(8) }).then((r) => r.status);
      const put1 = await one('transfer_put', { path: 'inbox/up.bin', from: slot.id, maxBytes: 50 << 20 }, m2.id), p1 = res(put1) || {};
      const partOnMachine = path.join(m2.home, 'inbox', 'up.bin.vr-part'), partMachineSize = fs.existsSync(partOnMachine) ? fs.statSync(partOnMachine).size : -1;
      const put2 = await one('transfer_put', { path: 'inbox/up.bin', from: slot.id }, m2.id), p2 = res(put2) || {};
      const landed = path.join(m2.home, 'inbox', 'up.bin'), landedSha = fs.existsSync(landed) ? await shaOf(landed) : '';
      const putOut = await one('transfer_put', { path: '../evil.bin', from: slot.id }, m2.id), evilThere = fs.existsSync(path.join(m2.home, '..', 'evil.bin')) || fs.existsSync(path.join(m2.home, '..', 'evil.bin.vr-part'));
      const xFile = path.join(mdir, 'x.bin'); fs.writeFileSync(xFile, 'only for t-r330');
      const slotX = (await call('/api/transfers', X, { name: 'x.bin' })).data || {}; await upload(X, xFile, 15, slotX.id); await mcall('POST', '/api/transfers/' + slotX.id + '/done', X, {});
      const putOfOthers = await one('transfer_put', { path: 'steal.bin', from: slotX.id }, m2.id);
      // the scope's maxFileMB bounds the transfer too
      setScope(m2, folderScope(m2, { limits: { maxFileMB: 100 } }));
      const tooBig = await one('transfer_get', { path: 'big.bin' }, m2.id);
      setScope(m2, folderScope(m2));
      const mem1 = mem();
      const srcT = fs.readFileSync(path.join(MILLS, 'native', 'crates', 'pgm-ai-internal', 'src', 'machine_transfer.rs'), 'utf8');
      check('R330', `T-311 big-file transfer by ticket: the hub issues a ticket bound to account, machine, path and direction (5 minutes at most; a call may ask for less); a sparse ${SIZE >> 20} MB file is pulled from the machine in chunks (the hub writes them straight to disk, its memory stays bounded), a call that stops early resumes at the byte the hub holds, the end is checked by sha256 and the owner downloads it streamed (Range works, another account sees nothing); a ${UP >> 20} MB upload from the owner goes down to the machine the other way with a resume and a sha256 check before it is put in place; the ticket is refused for another account's machine, another machine of the same account, another path, a page token, no token and after it expired; the scope's maxFileMB, path rules and another account's file are refused too`,
        !!f1.ticket && f1.complete === false && f1.received === (100 << 20) && partSize === (100 << 20) && f1.resumedFrom === 0 && withinTicket
        && rOther.status === 403 && rSibling.status === 403 && rPath.status === 403 && rPage.status === 401 && rNone.status === 401 && rBogus.status === 403 && rGood.status === 200 && rGood.data.received === (100 << 20) && rChunkOther === 403 && rLate.status === 410
        && f2.complete === true && f2.resumedFrom === (100 << 20) && f2.size === SIZE && f2.sha256 === wantSha && hubSha === wantSha && fs.statSync(hubFile).size === SIZE && !fs.existsSync(partFile)
        && dl.status === 200 && dn === SIZE && dh.digest('hex') === wantSha && rng.status === 206 && rngBuf.toString().startsWith('head-of-the-file;') && rngBuf.length === 64 && rngBad.status === 416 && dlOther.status === 404 && listU.includes(f2.id) && !listX.includes(f2.id)
        && uploaded === UP && upDone.status === 200 && upDone.data.sha256 === upSha && upDone.data.size === UP && upBad === 409
        && okB(put1) && p1.complete === false && p1.received === (50 << 20) && partMachineSize === (50 << 20) && okB(put2) && p2.complete === true && p2.resumedFrom === (50 << 20) && p2.sha256 === upSha && landedSha === upSha && !fs.existsSync(partOnMachine)
        && !okB(putOut) && !evilThere && !okB(putOfOthers) && !fs.existsSync(path.join(m2.home, 'steal.bin')) && !okB(tooBig) && /maxFileMB|more than the allowed/.test(errOf(tooBig))
        && !!hubPid && !!mem0 && !!mem1 && mem1.hwm - mem0.hwm < 160 * (1 << 20) && mem1.rss - mem0.rss < 160 * (1 << 20)
        && /pub const TICKET_SECS: i64 = 300/.test(srcT) && /fn a_ticket_is_bound_to_machine_account_path_and_five_minutes/.test(srcT),
        `${SIZE >> 20} MB: first call received ${(f1.received || 0) >> 20} MB, resumed from ${(f2.resumedFrom || 0) >> 20} MB, sha ${f2.sha256 === wantSha} · refusals other-account ${rOther.status} sibling ${rSibling.status} path ${rPath.status} page ${rPage.status} none ${rNone.status} late ${rLate.status} (within ticket ${withinTicket}) · download ${dl.status} ${dn >> 20} MB range ${rng.status} · put ${UP >> 20} MB: ${(p1.received || 0) >> 20}+${((p2.size || 0) - (p2.resumedFrom || 0)) >> 20} MB sha ${landedSha === upSha} · hub memory ${mem0 && mem1 ? `hwm ${(mem0.hwm >> 20)} → ${(mem1.hwm >> 20)} MB, rss ${(mem0.rss >> 20)} → ${(mem1.rss >> 20)} MB` : 'n/a'}`);
    } finally {
      for (const f of cleanups) f();
      for (const p of procs) { try { p.kill('SIGKILL'); } catch {} }
      for (const tok of [U, X]) { if (!tok) continue; for (const t of ((await call('/api/transfers', tok)).data.transfers || [])) await mcall('DELETE', '/api/transfers/' + t.id, tok); }
      for (const mm of ((await call('/api/machines', U)).data.machines || [])) if (/^r3(28|30)-/.test(mm.name || '')) await mcall('DELETE', '/api/machines/' + mm.id, U);
      if (created.password) await call('/api/admin/delete', A, { username: 't-r330' });
      try { fs.rmSync(mdir, { recursive: true, force: true }); } catch {}
    }
  }
  // R337: the owner's list over HTTP (scope in words, mode, status, in-use while a run uses it), nobody else sees or manages it, the
  // run's one transcript line (a plain bridge_info, so an older page shows it as text) and the page's panel script, its pure helpers
  // (scope words, bar text, red or not, the dot's shape) run in node, and the bar's state driven by the run's events.
  async function r337Check() {
    const L337 = 'the My machines panel lists only the owner\'s machines and the Work bar says what a run uses (T-314): the owner\'s list shows a linked machine with its scope in words (one folder: <its folder>, read-write), its mode and online status, and an empty in-use line; another person (and an admin) sees none of it and cannot rename, pause or unlink it; while the owner\'s run uses it the card says which session uses it and afterwards nothing; the run\'s first machine call adds exactly one plain transcript line "your machine › <name>: <scope> · session …" that any page shows, a run that uses no machine gets none; the page loads web/machines.js with the panel and the bar, whose helpers say "Using the opened folder <name>" for a folder (the browser\'s or the machine\'s), "Using your PC" only for the whole system or unrestricted, red only then, a dot with a different shape for online, offline and paused, and the bar follows the run (on at the first use, the wider place wins, off when the run ends)';
    const mdir = fs.mkdtempSync(path.join(SCRATCH, 'r337-')), procs = [];
    const home = path.join(mdir, 'home'), state = path.join(mdir, 'state'); fs.mkdirSync(home, { recursive: true });
    const mcall = (method, p, tok, body) => fetch(H + p, { method, headers: { 'Content-Type': 'application/json', ...(tok ? { Authorization: 'Bearer ' + tok } : {}) }, body: body !== undefined ? JSON.stringify(body) : undefined }).then(async (r) => ({ status: r.status, data: await r.json().catch(() => ({})) })).catch((e) => ({ status: 0, data: { error: e.message } }));
    const pc = (name, args, machine) => ({ route: 'pc-tool', body: { name, args: args || {}, machine } });
    const enc = (spec) => 'MACHTOOL=' + Buffer.from(JSON.stringify(spec)).toString('hex');
    const listOf = async (tok) => (await call('/api/machines', tok)).data.machines;
    const created = (await call('/api/admin/create', A, { username: 't-r337', display: 'R337' })).data || {};
    const Y = created.password ? (await login('t-r337', created.password, 'device-r337-xxxxxxxxxx')).data.token : '';
    const o = {}, detail = {};
    let id = '';
    try {
      // link a machine of t-user by device code (the library's runner), with its folder scope and a shell, and run it
      const linked = await new Promise((resolve) => {
        const p = spawn(NATIVE_BIN.host, ['machine', 'link', '--hub', H, '--folder', home, '--state', state, '--name', 'r337-box'], { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, VR_MACHINE_ALLOW_ROOT: '1' } });
        let out = '', approved = false;
        p.stdout.on('data', (d) => { out += d; const m = /Code: ([A-Z]{4}-[A-Z]{4})/.exec(out); if (m && !approved) { approved = true; call('/api/device/approve', U, { userCode: m[1] }); } });
        p.stderr.on('data', (d) => { out += d; });
        p.on('close', (code) => resolve({ code, out }));
      });
      try { id = JSON.parse(fs.readFileSync(path.join(state, 'link.json'), 'utf8')).machine; } catch {}
      o.linked = linked.code === 0 && !!id;
      const realHome = fs.realpathSync(home);
      fs.writeFileSync(path.join(state, 'scope.json'), JSON.stringify({ v: 1, preset: 'folder', paths: [{ path: realHome, access: 'rw' }], tools: { shell: { enabled: true, timeoutSec: 30 } } }));
      const log = fs.openSync(path.join(mdir, 'machine.log'), 'a');
      procs.push(spawn(NATIVE_BIN.host, ['machine', 'run', '--state', state], { stdio: ['ignore', log, log], env: { ...process.env, VR_MACHINE_ALLOW_ROOT: '1' } }));
      let card = {};
      for (let i = 0; i < 80; i++) { card = ((await listOf(U)) || []).find((x) => x.id === id) || {}; if (card.online && /^one folder: /.test(card.scope || '')) break; await sleep(250); }
      detail.card = `${card.name} · ${card.scope} · ${card.mode} · online ${card.online} · inUse ${JSON.stringify(card.inUse)}`;
      o.ownerCard = card.name === 'r337-box' && (card.scope || '').startsWith('one folder: ' + realHome + ' (read-write)') && ['persistent', 'while open'].includes(card.mode) && card.online === true && card.paused === false && Array.isArray(card.inUse) && card.inUse.length === 0 && !('token' in card) && !('publicKey' in card);
      // another person, and an admin, see none of it and cannot manage it
      const yList = await listOf(Y), aList = await listOf(A);
      const yRen = await mcall('PATCH', '/api/machines/' + id, Y, { name: 'stolen' }), yPause = await mcall('PATCH', '/api/machines/' + id, Y, { paused: true }), yDel = await mcall('DELETE', '/api/machines/' + id, Y), aDel = await mcall('DELETE', '/api/machines/' + id, A);
      const after = ((await listOf(U)) || []).find((x) => x.id === id) || {};
      o.othersSeeNone = Array.isArray(yList) && yList.length === 0 && Array.isArray(aList) && !aList.some((x) => x.id === id || x.name === 'r337-box') && [yRen, yPause, yDel, aDel].every((r) => r.status === 404) && after.name === 'r337-box' && after.paused === false;
      detail.others = `Y ${JSON.stringify(yList)} A ${aList && aList.length} · ${[yRen, yPause, yDel, aDel].map((r) => r.status).join('/')}`;
      // the owner's run uses it: the card says so while the run goes (Y still sees nothing), and not after it
      let seenUse = null, yDuring = null, stop = false;
      const watcher = (async () => { while (!stop) { const c = ((await listOf(U)) || []).find((x) => x.id === id) || {}; if (!seenUse && (c.inUse || []).length) { seenUse = c.inUse; yDuring = await listOf(Y); } await sleep(150); } })();
      const run = await chat(U, enc([pc('shell_run', { cmd: 'sleep 3; echo r337' }, id), pc('list', {}, id)]));
      stop = true; await watcher;
      const res = (() => { const l = texts(run.events).split('\n').find((x) => x.startsWith('MACH:')); try { return JSON.parse(l.slice(5)); } catch { return []; } })();
      const uses = run.events.filter((e) => e.type === 'bridge_info' && /^your machine › /.test(e.text || ''));
      const runId = (run.events.find((e) => e.type === 'bridge_run') || {}).id;
      const afterRun = ((await listOf(U)) || []).find((x) => x.id === id) || {};
      o.inUse = !!seenUse && seenUse.length === 1 && seenUse[0].run === runId && typeof seenUse[0].session === 'string' && Array.isArray(yDuring) && yDuring.length === 0 && Array.isArray(afterRun.inUse) && afterRun.inUse.length === 0;
      o.calls = res.length === 2 && res.every((r) => r.body && r.body.ok === true);
      o.oneLine = uses.length === 1 && uses[0].text.startsWith('your machine › r337-box: one folder: ' + realHome + ' (read-write)') && / · session \S+$/.test(uses[0].text) && uses[0].machine && uses[0].machine.id === id;
      const plain = await chat(U, 'r337 a run that uses no machine');
      o.noLineWithout = plain.status === 200 && !plain.events.some((e) => e.type === 'bridge_info' && /^your machine/.test(e.text || ''));
      detail.run = `use ${JSON.stringify(seenUse)} run ${runId} · calls ${res.map((r) => r.body && r.body.ok).join('/')} · lines ${uses.map((e) => e.text).join(' | ')} · after ${JSON.stringify(afterRun.inUse)}`;
      // the page: it loads web/machines.js (served by the hub), has the panel and the bar, and the helpers behave
      const page = await fetch(H + '/').then((r) => r.text()).catch(() => ''), js = await fetch(H + '/machines.js').then((r) => (r.ok ? r.text() : '')).catch(() => '');
      o.served = /<script src="machines\.js(\?v=[^"]*)?"><\/script>/.test(page) && page.includes('id="machBox"') && page.includes('id="useBar"') && js.includes('const MACHINES');
      const ctx = { $: () => null, rootHandle: { name: 'proj' } };
      let M = null; try { M = require('vm').runInNewContext(js + '\n;MACHINES', ctx); } catch (e) { detail.vm = e.message; }
      if (M) {
        const full = 'folders: the whole system (read-write); shell (advisory)', unr = 'unrestricted: no limits except the safety floor, shell', one = card.scope, many = 'folders: /a (read-write), /b (read-only); no shell';
        const st = [M.statusOf({ online: true }), M.statusOf({ online: false }), M.statusOf({ online: true, paused: true })];
        const bars = { browser: M.barText({ kind: 'browser', folder: 'proj' }), mine: M.barText({ kind: 'machine', name: 'r337-box', scope: one }), full: M.barText({ kind: 'machine', name: 'box', scope: full }), unr: M.barText({ kind: 'machine', name: 'box', scope: unr }), many: M.barText({ kind: 'machine', name: 'box', scope: many }) };
        o.helpers = M.wide(full) && M.wide(unr) && M.wide('folders: the whole system (read-only); no shell') && !M.wide(one) && !M.wide(many) && !M.wide('') && !M.wide('one folder (picked on the machine), read-write; shell (advisory)')
          && bars.browser.startsWith('Using the opened folder proj') && bars.mine.startsWith('Using the opened folder ' + realHome + ' on r337-box') && bars.full.startsWith('Using your PC box') && bars.unr.startsWith('Using your PC box') && !/your PC/.test(bars.many + bars.mine + bars.browser)
          && new Set(st.map((x) => x.glyph)).size === 3 && new Set(st.map((x) => x.cls)).size === 3 && M.inUseLine(card) === '' && M.inUseLine({ inUse: [{ session: 's2', run: 'r' }] }) === 'Now in use by: session s2' && M.parse(one).folder === realHome;
        detail.bars = JSON.stringify(bars);
        // the bar follows the run's events: a run starts (off), the browser folder answers (folder), a wide machine (it wins), the run ends (off)
        const seq = [];
        for (const ev of [{ type: 'bridge_run', id: 'r1' }, { type: 'pc_tool', id: 'c1' }, { type: 'bridge_info', text: 'your machine › box: ' + full, machine: { id: 'm', name: 'box', scope: full } }, { type: 'pc_tool', id: 'c2' }, { type: 'bridge_done' }]) { M.onEvent(ev); const u = M.using(); seq.push(u ? u.kind + (M.wide(u.scope) ? '!' : '') : '-'); }
        o.barFollows = seq.join(',') === '-,browser,machine!,machine!,-';
        detail.seq = seq.join(',');
      }
    } finally {
      for (const p of procs) { try { p.kill('SIGKILL'); } catch {} }
      if (id) await mcall('DELETE', '/api/machines/' + id, U);
      if (created.password) await call('/api/admin/delete', A, { username: 't-r337' });
      try { fs.rmSync(mdir, { recursive: true, force: true }); } catch {}
    }
    const keys = ['linked', 'ownerCard', 'othersSeeNone', 'inUse', 'calls', 'oneLine', 'noLineWithout', 'served', 'helpers', 'barFollows'];
    check('R337', L337, keys.every((k) => o[k] === true), `${keys.filter((k) => o[k] !== true).map((k) => 'FAILED ' + k).join(', ')} · ${Object.entries(detail).map(([k, v]) => k + ': ' + v).join(' · ')}`);
  }
  // R401 to R406 (T-199): the Hyper Workflows library (native/crates/pgm-workflow-internal): one `cargo test` run, its
  // tests named r401_ ... r406_ map back to the checks (a check passes when every test of its id ran and passed)
  {
    const home401 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
    const native401 = path.join(MILLS, 'native');
    const L401 = {
      R401: 'workflow library: a document (section 13 shape, loop fixed) round-trips with its unknown fields, an older schema is migrated on read, a newer one is refused with a message',
      R402: 'workflow library: each validation rule of 3.3 has a case (two starts, a bad edge type, an unfed required input, an input with two edges, an unreachable node as a warning, an unknown kind, a missing law id)',
      R403: 'workflow library: a cycle without a loop node is an error, the same cycle through a loop with a limit is valid, a loop with no limit is an error',
      R404: 'workflow library: a sub-graph with matching input / output ports is valid, mismatched ports or a sub-graph containing itself are errors, depth 9 is refused',
      R405: 'workflow library: diff(v1, v2) names an added node, a removed node, a changed config key and a rewired edge',
      R406: 'workflow library: a bad json file in the store is moved aside as *.bad-<time> and the listing goes on',
    };
    const MIN401 = { R401: 4, R402: 10, R403: 3, R404: 4, R405: 1, R406: 2 };
    if (!fs.existsSync(path.join(native401, 'crates', 'pgm-workflow-internal'))) for (const id of Object.keys(L401)) skip(id, L401[id], 'no crate native/crates/pgm-workflow-internal');
    else {
      const ut = spawnSync('sh', ['-c', `export RUSTUP_HOME="${home401}/.rustup" CARGO_HOME="${home401}/.cargo" CARGO_TARGET_DIR="${NATIVE_TARGET}"; export PATH="$CARGO_HOME/bin:$PATH"; command -v cargo >/dev/null || exit 77; cargo test --release --color never -p pgm-workflow-internal 2>&1`], { cwd: native401, encoding: 'utf8', timeout: 1800000 });
      const out401 = String(ut.stdout || '') + String(ut.stderr || '');
      if (ut.status === 77) for (const id of Object.keys(L401)) skip(id, L401[id], 'no Rust toolchain here (cargo not found)');
      else {
        const seen = {};
        for (const m of out401.matchAll(/^test (r40\d)_\S+ \.\.\. (ok|FAILED)/gm)) { const id = m[1].toUpperCase(); (seen[id] = seen[id] || []).push(m[2]); }
        const tail = out401.replace(/\s+/g, ' ').slice(-160);
        for (const id of Object.keys(L401)) check(id, L401[id], ut.status === 0 && (seen[id] || []).length >= MIN401[id] && seen[id].every((s) => s === 'ok'), `${(seen[id] || []).length} test(s) ${(seen[id] || []).filter((s) => s === 'ok').length} ok; cargo test ${ut.status}${ut.status === 0 ? '' : ' "' + tail + '"'}`);
      }
    }
  }
  // R407 to R423 (T-200, T-201): the Hyper Workflows engine and its built-in kinds (native/crates/
  // pgm-workflow-engine-internal), all with its fake host and no network: one `cargo test` run, its tests named
  // r407_ ... r423_ map back to the checks as R401-R406 do
  {
    const home407 = (() => { try { return require('os').userInfo().homedir; } catch { return process.env.HOME || ''; } })();
    const native407 = path.join(MILLS, 'native');
    const L407 = {
      R407: 'workflow engine: a prompt through start -> prompt -> end gives the fake AI\'s answer as the result; the log has run_start, node_start / node_end per node, run_end, with gapless seq',
      R408: 'workflow engine: parallel lines run at the same time and join all gives a list in line order; join first cancels the others; maxParallel 2 never lets three run',
      R409: 'workflow engine: a switch fires one port only, the nodes on the other ports are logged skipped, a join ignores skipped lines',
      R410: 'workflow engine: a loop until stops when met; one that never is stops at maxIterations and leaves by its limit port, or ends gave-up when that is not wired; each round is its own log entry',
      R411: 'workflow engine: a node that fails twice then works runs three attempts with backoff entries; after max it leaves by its error port; no wire: onError fail ends failed, skip goes on with a warning',
      R412: 'workflow engine: an approve node ends its worker (the run waits), a snapshot is written, an answer wakes it; a timeout follows onTimeout and never approves unless the designer chose it',
      R413: 'workflow engine: a run killed mid-way resumes from its log without running finished nodes again; a non-idempotent node that was mid-flight pauses for a person',
      R414: 'workflow engine: maxNodes, maxSeconds and maxCost each end a run limit with their name',
      R415: 'workflow engine: cancel stops running nodes through their cancel handle and ends cancelled; no node starts after it',
      R416: 'workflow engine: a panic in one node ends that node with engine.panic, and the engine and other runs go on',
      R417: 'workflow engine: a run\'s log replayed with a host that refuses all calls gives the same result',
      // T-201: the built-in kinds (tests/kinds.rs), same crate, same cargo run
      R418: 'workflow kinds: prompt / chat / summarise: templates fill from wires and saved prompts, a missing value is an error, a bad json answer is asked again; chat stops at maxTurns and at stopWhen; summarise keeps the keep items and cuts long texts',
      R419: 'workflow kinds: tool calls only what the scope allows (engine.tool-denied); a custom http call maps status and a json path; a secret is never in the log and a prompt cannot read one',
      R420: 'workflow kinds: script (the transform language) computes, has no file / network / clock function, and stops at its step limit; other languages are refused',
      R421: 'workflow kinds: model choice is the node\'s, then its section\'s (inner first), then the workflow default; fallback is tried when a model is unreachable',
      R422: 'workflow kinds: sub-workflow runs a pinned child with both logs linked and refuses past the depth limit; agent waits for the final answer; orchestrate plans, fans out, reviews within maxRounds and picks workers with a pgm-orchestrator profile',
      R423: 'workflow kinds: jev picks one choice port; low confidence follows onLowConfidence (default, error, second opinion, ask a person); off-shape is retried then an error; votes take the majority; through openrouter, ollama and host; the decision-model slot (T-210): a scope\'s decision model, JEV through OpenRouter then a local model, a node\'s own model first',
      // T-202: laws and saved prompts (tests/laws.rs, the store side in tests/service.rs)
      R425: 'workflow laws: a law and a prompt are saved, grouped (in two groups at once), left standalone, listed and deleted; deleting a group keeps its members; every save is a new version file; only the scope\'s admins (and lawEditors) change a team\'s',
      R426: 'workflow laws: scope defaults, workflow, sections (outer to inner) and node give the laws in that order with no duplicates; the right laws reach the right node and nowhere else; node_start names their ids and versions',
      R427: 'workflow laws: a lower level adds a law and takes one away (-law-x), but cannot remove a lockable: false law of a higher level',
      R428: 'workflow laws: a law check (regex, expr, jev) that fails follows its onFail: retry (once, told which law), error, or warn',
      R429: 'workflow prompts: {{prompt:name}} resolves a saved prompt; a pinned one (name@1) does not change when the prompt is edited, a latest one does; an unknown name is a validation error',
      R430: 'workflow laws: an export bundle carries the laws, groups and prompts the workflow uses (and nothing else); an import into another scope checks them and brings them along',
      // T-207: the /hyperwork command line (the one parser for the page, HPM and jcode)
      R460: 'hyperwork command: /hyperwork name prompt with --project / --on / --version (and --x=y, /hw), a still-typed name, bad options refused; a name that matches nothing runs nothing and gets the nearest names',
      // T-203: custom node kinds (tests/custom.rs, the store side in tests/service.rs)
      R431: 'custom kinds: "new node from selection" makes a graph kind whose ports are the selection\'s way in and out, saved in the person\'s own scope, and the rewired workflow runs as before',
      R432: 'custom kinds: undeclared access is denied at run time (an http kind with network: [] gets engine.denied, nothing is sent); a script kind stops at its step limit; a crash is the node\'s error; a sandboxed program is refused on this runner',
      R433: 'custom kinds: publishing runs the kind\'s tests and refuses a failing one; a version is published once; a built-in cannot be replaced',
      R434: 'custom kinds: a workflow pinned to major 1 never picks up 2.x; a stored kind whose hash no longer matches is refused (422 engine.kind-hash)',
      R435: 'custom kinds: author-only runs for its author alone; a review by someone not the author (an admin or lawEditor) makes it reviewed; a sandboxed program needs two; blocked can be neither added nor run',
      R436: 'custom kinds: a team kind is fetched by another runner (jcode) with its hash, installed only when the hash matches, and runs there',
      // T-206: the AI side chat (tests/service.rs)
      R454: 'side chat: ask answers about a workflow and may return a patch (the editor\'s commands); asking writes nothing',
      R455: 'side chat: a patch is for the person\'s draft only: asking never saves, publishes or runs',
      R456: 'side chat: a patch is only the editor\'s drawing commands; anything else (publish, run) is refused as a problem, and nothing happens',
      R457: 'side chat: the AI is told the node kinds the person may use; a patch that breaks validation comes with its problems, not applied',
      R458: 'side chat: with no AI reachable it says so (noAi) and the editor still works',
      // T-186: triggers (tests/triggers.rs, a clock of the test's own)
      R466: 'triggers: timers every N minutes / hours / days and the board\'s repeats (nightly at 02:00, monthly month-end aware); a run when due, once; a triggered run says so (sender trigger)',
      R467: 'triggers: no double run after a restart (the next time is saved first) or across two hubs on one store (a lease; the other takes over when it lapses)',
      R468: 'triggers: a time missed while the hub was down runs once (marked missed) or is skipped, then back on schedule',
      R469: 'triggers: a Forgejo / GitHub webhook, signed with the trigger\'s secret, starts a run with its payload; a wrong or missing signature is 401; an event it did not ask for starts nothing; the secret is never listed',
      R470: 'triggers: a named event from this server starts every unpaused event trigger of that name with its payload',
      R471: 'triggers: standing workflows list their last runs and next time, pause / resume, only the owner changes them, and a failed run sends the owner a chat alert',
    };
    const MIN407 = { R407: 2, R408: 7, R409: 2, R410: 4, R411: 4, R412: 3, R413: 2, R414: 1, R415: 1, R416: 1, R417: 1, R418: 5, R419: 2, R420: 1, R421: 1, R422: 4, R423: 5, R425: 1, R426: 2, R427: 1, R428: 1, R429: 2, R430: 1, R460: 3,
      R431: 2, R432: 1, R433: 2, R434: 2, R435: 1, R436: 1, R454: 1, R455: 1, R456: 1, R457: 1, R458: 1, R466: 1, R467: 1, R468: 1, R469: 1, R470: 1, R471: 1 };
    if (!fs.existsSync(path.join(native407, 'crates', 'pgm-workflow-engine-internal'))) for (const id of Object.keys(L407)) skip(id, L407[id], 'no crate native/crates/pgm-workflow-engine-internal');
    else {
      const ut = spawnSync('sh', ['-c', `export RUSTUP_HOME="${home407}/.rustup" CARGO_HOME="${home407}/.cargo" CARGO_TARGET_DIR="${NATIVE_TARGET}"; export PATH="$CARGO_HOME/bin:$PATH"; command -v cargo >/dev/null || exit 77; cargo test --release --color never -p pgm-workflow-engine-internal -p pgm-workflow-command-internal 2>&1`], { cwd: native407, encoding: 'utf8', timeout: 1800000 });
      const out407 = String(ut.stdout || '') + String(ut.stderr || '');
      if (ut.status === 77) for (const id of Object.keys(L407)) skip(id, L407[id], 'no Rust toolchain here (cargo not found)');
      else {
        const seen = {};
        // a test may stand for several checks: r454_r455_... counts for both
        for (const m of out407.matchAll(/^test (?:[\w:]+::)?((?:r4\d\d_)+)\S* \.\.\. (ok|FAILED)/gm)) for (const x of m[1].match(/r4\d\d/g)) { const id = x.toUpperCase(); (seen[id] = seen[id] || []).push(m[2]); }
        const tail = out407.replace(/\s+/g, ' ').slice(-160);
        for (const id of Object.keys(L407)) check(id, L407[id], ut.status === 0 && (seen[id] || []).length >= MIN407[id] && seen[id].every((s) => s === 'ok'), `${(seen[id] || []).length} test(s) ${(seen[id] || []).filter((s) => s === 'ok').length} ok; cargo test ${ut.status}${ut.status === 0 ? '' : ' "' + tail + '"'}`);
      }
    }
  }
  // R437 to R446 (T-204): Hyper Workflows' store and run API, /api/hyperwork/ (the Rust hub's alone: the frozen
  // JavaScript hub has none of it, R445). The library's own tests (tests/service.rs) run with the cargo block above;
  // these go over HTTP, as the person: config hyperworkTeams makes team-core of t-admin + t-user, mills is outside it
  {
    const L437 = {
      R437: 'hyperwork routes: save, get, publish (a message needed, errors refused), versions, diff, restore round trip; a stale etag gets 409',
      R438: 'hyperwork routes: a viewer cannot save (403), an editor cannot publish (403), a run-only client (jcode) cannot save, publish, delete or change rights (403 each) and can list, get the published version and run',
      R439: 'hyperwork routes: a team workflow is seen by a team member and not by someone outside the team (404 on get, list and run); a personal scope is private; the same name in two scopes lists twice, tagged; promote copies with history',
      R440: 'hyperwork routes: run pins the published version and runs it through the hub\'s own AI path; log?follow=1 streams its events to run_end; a page that reloads attaches again from a seq; runs lists it',
      R441: 'hyperwork routes: a run with preflight problems (needsProject without a project) answers 422 with the problems and starts nothing',
      R442: 'hyperwork routes: an approval workflow shows waiting, someone else cannot answer it, /api/hyperwork/answer wakes it [the Work question protocol\'s bridge_question + POST /api/work/answer is T-204\'s later part, with 5.17.0\'s ask_user]',
      R443: 'hyperwork routes: a hub restart in the middle of a run: the run resumes from its log after the restart and says so (run_resume)',
      R444: 'hyperwork routes: a secret (config hyperworkSecrets) used in a workflow\'s HTTP call never appears in the run log or any route\'s answer',
      R445: 'hyperwork routes: the frozen JavaScript hub answers 404 on /api/hyperwork/ (no mirror); the Rust hub lists the module',
      R446: 'hyperwork routes: list?since=<rev> gives only what changed [the gateway serving the same routes and mirroring the home hub is T-204\'s later part]',
      R461: 'hyperwork in the page: /hyperwork (and /hw) in the Work box\'s commands, a type-ahead from /api/hyperwork/list (grey = cannot run, with its reason), the line sent to POST /api/hyperwork/command with the session\'s project, the run followed from its log (steps, the answer, Approve / Reject); the command route runs a workflow by its line [HPM\'s and jcode\'s /hyperwork are T-207\'s later parts]',
      R462: 'hyperwork command: a workflow that needs a project refuses to start without one; the line\'s --project (else the page\'s project) is the run\'s project',
      R465: 'hyperwork team tools: a Work run\'s AI lists workflows (hyperwork_list) and sends a prompt through one (hyperwork_run) as the person whose run it is; someone else\'s personal workflow is refused [the session policy switch is T-207\'s later part]',
      R472: 'hyperwork triggers on the hub: a webhook trigger made in /api/hyperwork/trigger is started by POST /hooks/hyperwork/<id> signed as Forgejo does (no session), refused unsigned; an internal event (this machine, its secret) starts an event trigger; a triggered run that fails sends its owner a private chat alert from "Hyper Workflows"; the list shows the runs',
    };
    const hw = (p, tok, body, hdr) => call('/api/hyperwork/' + p, tok, body, hdr);
    // the page's part is the kit module web/hyperwork.js (one copy with Hyper Prod Man and pgm-libs) with the page's hooks
    const html461 = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), mod461 = fs.readFileSync(path.join(MILLS, 'web', 'hyperwork.js'), 'utf8');
    const page461 = /\{ cmd: '\/hyperwork', desc: /.test(html461) && /\{ cmd: '\/hw', desc: /.test(html461) && /HyperworkUI\.create\(\{/.test(html461) && /project: \(\) => \(me && me\.project\) \|\| ''/.test(html461) && /id="hwMenu" hidden/.test(html461) && /<script src="hyperwork\.js"><\/script>/.test(html461)
      && /function typeahead\(\)/.test(mod461) && /\/api\/hyperwork\/list\?q=/.test(mod461) && /\/api\/hyperwork\/command', \{ method: 'POST', body: JSON\.stringify\(\{ line, project: /.test(mod461) && /\/api\/hyperwork\/log\?id=/.test(mod461) && /\/api\/hyperwork\/answer/.test(mod461) && /cannot run: /.test(mod461)
      && (() => { try { const M = require(path.join(MILLS, 'web', 'hyperwork.js')); return M.nameQuery('/hyperwork te') === 'te' && M.nameQuery('/hw ') === '' && M.nameQuery('hello') === null && M.isCommand('/hw notes go') && !M.isCommand('/model x') && M.renderEvent({ type: 'run_end', status: 'ok', result: 'done it' }).kind === 'done' && M.renderEvent({ type: 'wait', node: 'a', what: 'approval' }).kind === 'wait' && M.renderEvent({ type: 'node_end', status: 'ok', kind: 'prompt', node: 'p' }).kind === 'step'; } catch { return false; } })();
    if (!RUST_HUB) {
      const js1 = await hw('list', U), js2 = await hw('run', U, { name: 'x', prompt: 'y' });
      check('R445', L437.R445, js1.status === 404 && js2.status === 404, `JavaScript hub: list ${js1.status}, run ${js2.status}`);
      for (const id of Object.keys(L437).filter((x) => x !== 'R445')) skip(id, L437[id], 'the Rust hub\'s: --rust-hub runs it');
    } else {
      const doc = (name, extra = {}) => ({ schema: 1, name, nodes: [{ id: 's', kind: 'start' }, { id: 'p', kind: 'prompt', config: { user: 'hyperwork {{inputs.prompt}}' } }, { id: 'e', kind: 'end' }],
        edges: [{ id: 'e1', from: 's.prompt', to: 'p.in' }, { id: 'e2', from: 'p.text', to: 'e.result' }], ...extra });
      const until = async (run, tok = A) => { let m = {}; for (let i = 0; i < 240; i++) { m = (await hw('run?id=' + run, tok)).data || {}; if (m.status && m.status !== 'running') break; await sleep(250); } return m; };
      const follow = async (run, tok, from = 0) => { try { const r = await fetch(`${H}/api/hyperwork/log?id=${run}&from=${from}&follow=1`, { headers: { Authorization: 'Bearer ' + tok }, signal: AbortSignal.timeout(60000) }); return (await r.text()).split('\n').filter(Boolean).map((l) => JSON.parse(l)); } catch (e) { return [{ error: e.message }]; } };
      const JC = { 'X-VR-Client': 'jcode' };
      // R437
      const sv = await hw('save', A, { doc: doc('r437-notes'), scope: 'team-core' }), id = sv.data.id;
      const g1 = await hw('get?id=' + id, A), p0 = await hw('publish', A, { id }), p1 = await hw('publish', A, { id, message: 'first' });
      const sv2 = await hw('save', A, { id, doc: doc('r437-notes', { description: 'second' }), etag: sv.data.etag });
      const stale = await hw('save', A, { id, doc: doc('r437-notes'), etag: sv.data.etag });
      const p2 = await hw('publish', A, { id, message: 'second' }), vers = await hw('versions?id=' + id, A), df = await hw(`diff?id=${id}&a=1&b=2`, A), rst = await hw('restore', A, { id, version: 1 });
      const broken = await hw('save', A, { doc: { schema: 1, name: 'r437-broken', nodes: [{ id: 's', kind: 'start' }, { id: 's2', kind: 'start' }], edges: [] } }), pb = await hw('publish', A, { id: broken.data.id, message: 'x' });
      check('R437', L437.R437, sv.status === 200 && g1.status === 200 && p0.status === 400 && p1.data.version === 1 && sv2.status === 200 && stale.status === 409 && p2.data.version === 2 && (vers.data.versions || []).length === 2 && df.status === 200 && rst.status === 200 && pb.status === 422,
        `save ${sv.status} get ${g1.status} publish ${p0.status}/${p1.data.version}/${p2.data.version} stale ${stale.status} versions ${(vers.data.versions || []).length} diff ${df.status} restore ${rst.status} broken ${pb.status}`);
      // R438
      const viewerSave = await hw('save', U, { id, doc: doc('r437-notes') });
      await hw('perms', A, { id, perms: { edit: ['t-user'] } });
      const editorSave = await hw('save', U, { id, doc: doc('r437-notes') }), editorPub = await hw('publish', U, { id, message: 'x' });
      const jc = [await hw('save', A, { id, doc: doc('r437-notes') }, JC), await hw('publish', A, { id, message: 'x' }, JC), await hw('delete', A, { id }, JC), await hw('perms', A, { id, perms: {} }, JC)].map((r) => r.status);
      const jcList = await hw('list', A, undefined, JC), jcGet = await hw('get?id=' + id, A, undefined, JC), jcRun = await hw('run', A, { id, prompt: 'from jcode' }, JC);
      check('R438', L437.R438, viewerSave.status === 403 && editorSave.status === 200 && editorPub.status === 403 && jc.every((s) => s === 403) && jcList.status === 200 && (jcList.data.items || []).some((i) => i.id === id && i.rights.edit === false) && jcGet.status === 200 && jcGet.data.version === 2 && jcRun.status === 202,
        `viewer save ${viewerSave.status}, editor save ${editorSave.status} publish ${editorPub.status}; run-only writes ${jc.join('/')}, list ${jcList.status} get ${jcGet.status} v${jcGet.data.version} run ${jcRun.status}`);
      // R439
      const outGet = await hw('get?id=' + id, D), outList = await hw('list', D), outRun = await hw('run', D, { id, prompt: 'x' });
      const mine = await hw('save', A, { doc: doc('r437-notes') }); await hw('publish', A, { id: mine.data.id, message: 'mine' });
      const peek = await hw('get?id=' + mine.data.id, U), both = await hw('list?q=r437-no', A);
      const promo = await hw('promote', A, { id: mine.data.id, scope: 'company' }), promoVers = promo.data.id ? await hw('versions?id=' + promo.data.id, D) : { data: {} };
      const shown = (both.data.items || []).map((i) => i.shown);
      check('R439', L437.R439, outGet.status === 404 && !(outList.data.items || []).some((i) => i.id === id) && outRun.status === 404 && peek.status === 404 && shown.includes('r437-notes') && shown.includes('core/r437-notes') && promo.status === 200 && (promoVers.data.versions || []).length === 1,
        `outside: get ${outGet.status} run ${outRun.status} listed ${(outList.data.items || []).some((i) => i.id === id)}; personal peek ${peek.status}; shown ${shown.join(', ')}; promote ${promo.status}, company sees ${(promoVers.data.versions || []).length} version(s)`);
      // R440 (and T-201's run through the hub: the prompt node answers through the hub's own AI path)
      const r440 = await hw('run', A, { id, prompt: 'r440' });
      await hw('save', A, { id, doc: doc('r437-notes', { description: 'third' }) }); await hw('publish', A, { id, message: 'third' });
      const lines = await follow(r440.data.run, A), m440 = await until(r440.data.run), again = await follow(r440.data.run, A, 3), runs = await hw('runs?mine=1', A);
      const ty = lines.map((e) => e.type);
      check('R440', L437.R440, r440.status === 202 && ty[0] === 'run_start' && ty[ty.length - 1] === 'run_end' && lines[0].workflow && lines[0].workflow.version === 2 && m440.status === 'ok' && !!m440.result && again.length > 0 && again[0].seq === 3 && (runs.data.runs || []).some((x) => x.id === r440.data.run),
        `run ${r440.status}; streamed ${ty.join(' ')}; pinned v${lines[0] && lines[0].workflow && lines[0].workflow.version}; status ${m440.status} result ${JSON.stringify(m440.result || m440.error).slice(0, 120)}; reattach from ${again[0] && again[0].seq}`);
      // R441
      const np = await hw('save', A, { doc: doc('r441-needs', { needsProject: true }) }); await hw('publish', A, { id: np.data.id, message: 'x' });
      const r441 = await hw('run', A, { id: np.data.id, prompt: 'x' }), runs441 = await hw('runs?workflow=' + np.data.id, A);
      check('R441', L437.R441, r441.status === 422 && (r441.data.problems || []).some((p) => p.code === 'engine.no-project') && (runs441.data.runs || []).length === 0, `run ${r441.status} ${JSON.stringify(r441.data.problems || []).slice(0, 120)}; runs ${(runs441.data.runs || []).length}`);
      // R442
      const ap = await hw('save', A, { doc: { schema: 1, name: 'r442-ship', nodes: [{ id: 's', kind: 'start' }, { id: 'a', kind: 'approve', config: { timeoutSec: 600 } }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'a.in' }, { id: 'e2', from: 'a.approved', to: 'e.result' }] } });
      await hw('publish', A, { id: ap.data.id, message: 'x' });
      const r442 = await hw('run', A, { id: ap.data.id, prompt: 'ship v5' }), w442 = await until(r442.data.run);
      const notMine = await hw('answer', U, { run: r442.data.run, node: 'a', answers: { approved: true } }), ok442 = await hw('answer', A, { run: r442.data.run, node: 'a', answers: { approved: true } });
      const d442 = await until(r442.data.run);
      check('R442', L437.R442, w442.status === 'waiting' && (w442.waiting || [])[0] && w442.waiting[0].key === 'a' && [403, 404].includes(notMine.status) && ok442.status === 200 && d442.status === 'ok' && d442.result === 'ship v5',
        `first ${w442.status} on ${JSON.stringify(w442.waiting || []).slice(0, 80)}; t-user answers ${notMine.status}; owner ${ok442.status}; then ${d442.status} ${JSON.stringify(d442.result)}`);
      // R443: a run that waits 4 s is cut by a graceful restart (a workflow run does not hold the hub busy) and resumes
      const wt = await hw('save', A, { doc: { schema: 1, name: 'r443-wait', nodes: [{ id: 's', kind: 'start' }, { id: 'w', kind: 'wait', config: { seconds: 4 } }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'w.in' }, { id: 'e2', from: 'w.out', to: 'e.result' }] } });
      await hw('publish', A, { id: wt.data.id, message: 'x' });
      const pid443 = (await call('/api/status')).data.pid;
      const r443 = await hw('run', A, { id: wt.data.id, prompt: 'after the restart' });
      await sleep(800);
      fs.writeFileSync(path.join(SB, '.restart'), 'test');
      let pid2 = pid443; for (let i = 0; i < 80 && (pid2 === pid443 || !pid2); i++) { await sleep(250); pid2 = (await call('/api/status')).data.pid; }
      for (let i = 0; i < 40 && (await call('/api/status')).status !== 200; i++) await sleep(250);
      const m443 = await until(r443.data.run), l443 = await follow(r443.data.run, A);
      check('R443', L437.R443, pid2 && pid2 !== pid443 && m443.status === 'ok' && m443.result === 'after the restart' && l443.some((e) => e.type === 'run_resume'),
        `pid ${pid443} -> ${pid2}; ${m443.status} ${JSON.stringify(m443.result || m443.error).slice(0, 100)}; resumed ${l443.some((e) => e.type === 'run_resume')}`);
      // R444
      const sec = await hw('save', A, { doc: { schema: 1, name: 'r444-call', nodes: [{ id: 's', kind: 'start' }, { id: 't', kind: 'tool', config: { http: { url: `http://127.0.0.1:${OLLAMA_PORT}/r444`, headers: { authorization: 'token {{secret:R444_TOKEN}}' }, expect: { status: [200, 404] } } } }, { id: 'e', kind: 'end' }],
        edges: [{ id: 'e1', from: 's.inputs', to: 't.args' }, { id: 'e2', from: 't.result', to: 'e.result' }] } });
      await hw('publish', A, { id: sec.data.id, message: 'x' });
      const r444 = await hw('run', A, { id: sec.data.id, prompt: 'x' }), m444 = await until(r444.data.run), l444 = await follow(r444.data.run, A), all444 = JSON.stringify([m444, l444, (await hw('runs', A)).data]);
      const file444 = (() => { try { return fs.readFileSync(path.join(SCRATCH, 'Hyperwork', 'runs', r444.data.run, 'log.ndjson'), 'utf8'); } catch { return ''; } })();
      check('R444', L437.R444, r444.status === 202 && l444.length > 2 && !all444.includes('r444-sekrit-token-77') && !!file444 && !file444.includes('r444-sekrit-token-77'),
        `run ${r444.status} ${m444.status}; ${l444.length} events; secret in answers ${all444.includes('r444-sekrit-token-77')}, in the file ${file444.includes('r444-sekrit-token-77')} (${file444.length} bytes)`);
      // R445
      const front = await call('/__vr-front');
      check('R445', L437.R445, (front.data.modules || []).includes('hyperwork') && (await hw('list', A)).status === 200 && (await hw('list')).status === 401, `modules ${(front.data.modules || []).join(' ')}`);
      // R446
      const rev = (await hw('list', A)).data.rev;
      const ch = await hw('save', A, { doc: doc('r446-new') });
      const since = await hw('list?since=' + rev, A);
      check('R446', L437.R446, ch.status === 200 && (since.data.items || []).length === 1 && since.data.items[0].id === ch.data.id, `since ${rev}: ${(since.data.items || []).map((i) => i.name).join(', ')}`);
      // R461 / R462: the command line (as the page sends it)
      const ai = await hw('save', A, { doc: doc('r461-notes') }); await hw('publish', A, { id: ai.data.id, message: 'x' });
      const c1 = await hw('command', A, { line: '/hyperwork r461-notes write it', project: 'VibeRoller' }), c1m = c1.data.run ? await until(c1.data.run) : {};
      const c2 = await hw('command', A, { line: '/hyperwork r461-nots write it' }), c3 = await hw('command', A, { line: '/hyperwork r461-notes --on moon x' });
      check('R461', L437.R461, page461 && c1.status === 202 && c1m.status === 'ok' && c2.status === 404 && (c2.data.nearest || []).includes('r461-notes') && c3.status === 400,
        `page ${page461}; command ${c1.status} ${c1m.status}; unknown ${c2.status} nearest ${JSON.stringify(c2.data.nearest)}; bad --on ${c3.status}`);
      const c4 = await hw('command', A, { line: '/hyperwork r441-needs x' }), c5 = await hw('command', A, { line: '/hyperwork r441-needs --project Jcode-PGM x', project: 'VibeRoller' }), c6 = await hw('command', A, { line: '/hyperwork r441-needs x', project: 'VibeRoller' });
      const p5 = c5.data.run ? (await follow(c5.data.run, A))[0] || {} : {}, p6 = c6.data.run ? (await follow(c6.data.run, A))[0] || {} : {};
      check('R462', L437.R462, c4.status === 422 && c5.status === 202 && p5.project === 'Jcode-PGM' && c6.status === 202 && p6.project === 'VibeRoller', `none ${c4.status}; --project ${c5.status} ${p5.project}; the page's ${c6.status} ${p6.project}`);
      // R465: a Work run's AI (the fake Claude's HYPERWORK) uses the team tools
      const t465 = await chat(A, 'HYPERWORK r461-notes', { fresh: true }), o465 = JSON.parse((texts(t465.events).match(/HW:(\{.*\})/) || [])[1] || '{}');
      const u465 = await chat(U, 'HYPERWORK r461-notes', { fresh: true }), q465 = JSON.parse((texts(u465.events).match(/HW:(\{.*\})/) || [])[1] || '{}');
      const mine465 = o465.run && o465.run.run ? (await hw('run?id=' + o465.run.run, A)).data : {};
      check('R465', L437.R465, Array.isArray(o465.listed) && o465.listed.includes('r461-notes') && o465.ok === true && o465.run.status === 'ok' && mine465.owner === 't-admin' && q465.ok === false,
        `listed ${JSON.stringify(o465.listed)}; run ${JSON.stringify(o465.run).slice(0, 160)}; owner ${mine465.owner}; t-user ${q465.ok} ${JSON.stringify(q465.run).slice(0, 80)}`);
      // R472: triggers through the hub
      const crypto472 = require('crypto');
      const wh = await hw('trigger', A, { workflow: ai.data.id, kind: 'webhook', webhook: { events: ['pull_request'] }, prompt: 'review {{event.pull_request.title}}', title: 'r472 hook' });
      const body472 = JSON.stringify({ action: 'opened', pull_request: { title: 'R472 PR' } });
      const post472 = (headers) => fetch(H + '/hooks/hyperwork/' + wh.data.id, { method: 'POST', headers: { 'Content-Type': 'application/json', ...headers }, body: body472 }).then(async (x) => ({ status: x.status, data: await x.json().catch(() => ({})) })).catch((e) => ({ status: 0, data: { error: e.message } }));
      const signed = await post472({ 'X-Forgejo-Event': 'pull_request', 'X-Forgejo-Signature': crypto472.createHmac('sha256', wh.data.secret || 'x').update(body472).digest('hex') });
      const unsigned = await post472({ 'X-Forgejo-Event': 'pull_request' });
      const m472 = signed.data.run ? await until(signed.data.run) : {};
      const ev = await hw('trigger', A, { workflow: ai.data.id, kind: 'event', event: { name: 'r472-deployed' }, prompt: 'deployed {{event.v}}' });
      const sendEvent = (secret) => fetch(H + '/internal/hyperwork/event', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Event-Secret': secret }, body: JSON.stringify({ name: 'r472-deployed', payload: { v: '9.9.9' } }) }).then(async (x) => ({ status: x.status, data: await x.json().catch(() => ({})) }));
      const evOk = await sendEvent('r472-event-secret-0123456789'), evBad = await sendEvent('wrong-secret-0000000000');
      const mEv = (evOk.data.runs || [])[0] ? await until(evOk.data.runs[0]) : {};
      // a workflow that fails (an HTTP call to a host the hub does not allow), started by an event: its owner hears of it
      const failing = await hw('save', A, { doc: { schema: 1, name: 'r472-fails', nodes: [{ id: 's', kind: 'start' }, { id: 't', kind: 'tool', config: { http: { url: 'https://not-allowed.example/x' } } }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.inputs', to: 't.args' }, { id: 'e2', from: 't.result', to: 'e.result' }] } });
      await hw('publish', A, { id: failing.data.id, message: 'x' });
      await hw('trigger', A, { workflow: failing.data.id, kind: 'event', event: { name: 'r472-breaks' }, title: 'r472 breaks' });
      const seq472 = Math.max(0, ...((await call('/api/room/messages?since=0', A)).data.messages || []).map((m) => m.seq));
      const evFail = await fetch(H + '/internal/hyperwork/event', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-VR-Event-Secret': 'r472-event-secret-0123456789' }, body: JSON.stringify({ name: 'r472-breaks', payload: {} }) }).then((x) => x.json()).catch(() => ({}));
      const mFail = (evFail.runs || [])[0] ? await until(evFail.runs[0]) : {};
      let alert472 = null;
      for (let i = 0; i < 40 && !alert472; i++) { alert472 = ((await call('/api/room/messages?since=' + seq472, A)).data.messages || []).find((m) => m.as === 'Hyper Workflows' && m.to === 't-admin') || null; if (!alert472) await sleep(250); }
      const list472 = (await hw('triggers?workflow=' + ai.data.id, A)).data.triggers || [], hook472 = list472.find((t) => t.id === wh.data.id) || {};
      check('R472', L437.R472, wh.status === 200 && !!wh.data.secret && signed.status === 202 && m472.status === 'ok' && unsigned.status === 401 && ev.status === 200 && evOk.status === 200 && mEv.status === 'ok' && evBad.status === 403
        && mFail.status === 'failed' && !!alert472 && (hook472.runs || []).length === 1 && hook472.webhook && hook472.webhook.secret === undefined,
        `hook ${wh.status}; signed ${signed.status} ${m472.status} ${JSON.stringify(m472.result).slice(0, 60)}; unsigned ${unsigned.status}; event ${evOk.status} ${mEv.status}, wrong secret ${evBad.status}; failing ${mFail.status}, alert ${!!alert472}; listed runs ${(hook472.runs || []).length}`);
    }
  }
  // R447 to R453 (T-205): the editor foundation (web/editor/core.js, view.js, the workflow plugin, a stub page-elements
  // plugin, the Workflows screen). The logic checks run in Node in both suite modes (bridge/test/editor-check.js, with a
  // fake DOM for the view and a stub hub for the screen); R451 also goes through the real Rust hub under --rust-hub.
  {
    const L447 = {
      R447: 'editor core (web/editor/core.js: pure logic, no DOM): add, move, resize, snap to the grid and to another item, box select, align, group / ungroup; undo x n then redo x n returns the same document; a continuous drag is one undo step',
      R448: 'editor core: copy / paste across two documents keeps ids unique; a connected selection keeps the wires inside it and drops the wires to the outside; a group travels with its members',
      R449: 'editor core: the property panel is built from a json schema (also drawn by the view); an invalid value shows the schema\'s message and does not write',
      R450: 'editor core: entering a sub-graph and leaving it again changes the view, not the document or its history; the breadcrumb shows the path (and the view draws it)',
      R451: 'editor plugins: a tool registers item kinds (the workflow one, and a stub "page elements" one for T-191); core.js names neither and works alone; the workflow plugin\'s ports, types and document round trip follow the library; the Workflows screen opens the list, then the editor (save, test run with the node overlay, the side chat); with --rust-hub the plugin\'s calls match the hub: its kinds equal the hub\'s, save then open round-trips a document through /api/hyperwork/, validate, a test run reports node states, the side chat, triggers, duplicate, delete, publish',
      R452: 'editor core: an AI patch (a list of commands) applies as one undo step; a bad patch is refused whole and a rejected proposal leaves no trace in the history',
    };
    const ec = await require('./editor-check.js').run();
    let hub451 = { ok: true, detail: '' };
    if (!RUST_HUB) hub451.detail = ' (the hub half of R451 runs with --rust-hub)';
    else {
      const WF = require(path.join(MILLS, 'web', 'editor', 'kinds-workflow.js')), CORE = require(path.join(MILLS, 'web', 'editor', 'core.js'));
      const request = async (method, p, body) => {
        const r = await fetch(H + p, { method, headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + A }, body: body === undefined || method === 'GET' ? undefined : JSON.stringify(body) });
        const text = await r.text(); let data = null; if (!/\/log\?/.test(p)) { try { data = JSON.parse(text); } catch { data = null; } }
        return { status: r.status, data, text };
      };
      const cl = WF.client(request), why = [], need = (name, ok, more) => { if (!ok) why.push(name + (more ? ' [' + more + ']' : '')); };
      try {
        // the hub's kinds and the plugin's static copy agree (names, ports, types, required, many)
        const hubKinds = await cl.kinds(), port = (p) => [p.name, p.type, !!p.required, !!p.many].join(':');
        const sig = (m) => m.kind + '|' + (m.ports.in || []).map(port).join(',') + '|' + (m.ports.out || []).map(port).join(',');
        const hubBuilt = hubKinds.filter((k) => k.trust === 'built-in'), sa = new Set(WF.STATIC_MANIFESTS.map(sig)), sb = new Set(hubBuilt.map(sig));
        need('the static kinds equal the hub\'s built-ins', [...sa].every((x) => sb.has(x)) && [...sb].every((x) => sa.has(x)), 'static-only ' + [...sa].filter((x) => !sb.has(x)).join(' ; ') + ' hub-only ' + [...sb].filter((x) => !sa.has(x)).join(' ; '));
        // build a document in the editor, save it, open it again
        const reg = CORE.createRegistry(); WF.install(reg, hubKinds);
        const ed = CORE.createEditor({ registry: reg, doc: WF.fromWorkflow(WF.blank('r451-edit', { extra: { labels: ['r451cat'] } })), idPrefix: 'n', linkPrefix: 'e' });
        need('edit: disconnect the blank\'s wire', ed.exec({ type: 'disconnect', id: 'e1' }).ok);
        const added = ed.exec({ type: 'add', kind: 'prompt', id: 'n3', x: 220, y: 100, props: { user: 'hyperwork {{inputs.prompt}}', label: 'Answer' } });
        need('edit: add a prompt node', added.ok, added.error);
        const c1 = ed.exec({ type: 'connect', from: 'n1.prompt', to: 'n3.in' }), c2 = ed.exec({ type: 'connect', from: 'n3.text', to: 'n2.result' });
        need('edit: wire start to prompt to end', c1.ok && c2.ok, (c1.error || '') + (c2.error || ''));
        const sv = await cl.save(null, ed.toJSON(), { scope: 'team-core' });
        need('save: a new workflow in team-core, an etag, no errors', !!sv.id && !!sv.etag && !sv.problems.some((p) => p.level === 'error'), JSON.stringify(sv.problems).slice(0, 200));
        const opened = await cl.open(sv.id), mine = WF.toWorkflow(ed.toJSON()).json, back = WF.toWorkflow(opened.doc).json;
        const norm = (j) => CORE.canon({ nodes: j.nodes, edges: j.edges, name: j.name, labels: j.labels });
        need('open: the saved document comes back with the same nodes, wires, positions, config and name', norm(mine) === norm(back) && opened.draft === true && opened.etag === sv.etag && opened.rights.edit === true, norm(mine).slice(0, 160) + ' <> ' + norm(back).slice(0, 160));
        need('open: the editor loads it again (same items and wires)', Object.keys(opened.doc.items).sort().join() === Object.keys(ed.doc.items).sort().join() && Object.keys(opened.doc.links).sort().join() === Object.keys(ed.doc.links).sort().join());
        const again = await cl.save(sv.id, opened.doc, { etag: opened.etag }); need('save again with the etag', again.id === sv.id && again.etag !== opened.etag);
        let stale = 0; try { await cl.save(sv.id, opened.doc, { etag: opened.etag }); } catch (e) { stale = e.status; } need('a stale etag is refused (409), the editor says so', stale === 409, 'status ' + stale);
        // validate: a good document, and one the hub refuses
        const good = await cl.validate(ed.toJSON()); need('validate: no errors for the good document', !good.some((p) => p.level === 'error'), JSON.stringify(good).slice(0, 160));
        const broken = CORE.createEditor({ registry: reg, doc: WF.fromWorkflow(WF.blank('r451-bad')), idPrefix: 'n', linkPrefix: 'e' }); broken.exec({ type: 'delete', ids: ['n2'] });
        const badp = await cl.validate(broken.toJSON()); need('validate: a document with no end has the hub\'s error (end.missing)', badp.some((p) => p.code === 'end.missing' && p.level === 'error'), JSON.stringify(badp).slice(0, 160));
        // a test run of the draft, with the node states from the log
        const until = async (run) => { let m = {}; for (let i = 0; i < 240; i++) { m = await cl.runMeta(run); if (m.status && m.status !== 'running') break; await sleep(250); } return m; };
        const run = await cl.run(sv.id, { prompt: 'r451 run', draft: true }); need('a test run starts (a run id, the hub as runner)', /^hr-/.test(run.run) && run.runner === 'hub', JSON.stringify(run));
        const watched = []; const w = cl.watch(run.run, (ev) => watched.push(ev), { pollMs: 150 }); await Promise.race([w.done, sleep(60000)]); w.stop();
        const meta = await until(run.run), logged = await cl.log(run.run, 0), ov = WF.overlay(); ov.apply.all(logged);
        need('the live watch and the log give the same events, ending with run_end', watched.length === logged.length && watched.length > 3 && watched[watched.length - 1].type === 'run_end' && JSON.stringify(watched.map((e) => e.seq)) === JSON.stringify(logged.map((e) => e.seq)), watched.length + ' vs ' + logged.length);
        need('the overlay reports each node\'s state (start, prompt, end: ok) and the run (ok, outcome, result)', ov.nodes.n1 && ov.nodes.n1.state === 'ok' && ov.nodes.n3 && ov.nodes.n3.state === 'ok' && ov.nodes.n2 && ov.nodes.n2.state === 'ok' && ov.run.status === 'ok' && !!ov.run.outcome && ov.run.result !== null && meta.status === 'ok', JSON.stringify(ov.marks()).slice(0, 240) + ' run ' + ov.run.status);
        need('the marks carry text for the view and the run is over', ov.marks().n3 && /ok/.test(ov.marks().n3.text) && ov.ended());
        // a run that fails: the overlay shows the node in error (a template with no value)
        const bad = CORE.createEditor({ registry: reg, doc: WF.fromWorkflow(WF.blank('r451-fail')), idPrefix: 'n', linkPrefix: 'e' }); bad.exec({ type: 'disconnect', id: 'e1' });
        bad.exec({ type: 'add', kind: 'prompt', id: 'n3', x: 220, y: 100, props: { user: '{{inputs.nothing_here}}' } }); bad.exec({ type: 'connect', from: 'n1.prompt', to: 'n3.in' }); bad.exec({ type: 'connect', from: 'n3.text', to: 'n2.result' });
        const sv2 = await cl.save(null, bad.toJSON(), { scope: 'team-core' }), run2 = await cl.run(sv2.id, { prompt: 'x', draft: true }); await until(run2.run);
        const ov2 = WF.overlay(); ov2.apply.all(await cl.log(run2.run, 0)); need('a failing node shows as error with its message, the run as failed', ov2.nodes.n3 && ov2.nodes.n3.state === 'error' && !!ov2.nodes.n3.error && ov2.run.status === 'failed', JSON.stringify(ov2.nodes.n3) + ' ' + ov2.run.status);
        // the side chat: asks with the document as it is now; answers have text and a patch array (no AI reachable is an answer too)
        const ask = await cl.ask(ed.toJSON(), 'what does this workflow do?'); need('the side chat answers with text and a patch list (writes nothing)', typeof ask.text === 'string' && Array.isArray(ask.patch), JSON.stringify(ask).slice(0, 160));
        const chat = WF.chat(cl, ed); const before = JSON.stringify(ed.history()); await chat.send('explain it'); need('the chat records the question and an answer, and the editor history is untouched', chat.messages.length === 2 && JSON.stringify(ed.history()) === before);
        // triggers
        const tr = await cl.triggerSave(WF.triggerBody(sv.id, { kind: 'event', name: 'r451-evt', prompt: 'go', title: 'r451 trigger' })); need('a trigger is added', !!tr.id, JSON.stringify(tr));
        let list = await cl.triggers(sv.id); need('the trigger list shows it, with a readable summary', list.some((t) => t.id === tr.id && /r451-evt/.test(WF.triggerSummary(t))), JSON.stringify(list).slice(0, 160));
        await cl.triggerPause(tr.id); list = await cl.triggers(sv.id); need('pause shows in the list', list.find((t) => t.id === tr.id).paused === true);
        await cl.triggerResume(tr.id); await cl.triggerDelete(tr.id); list = await cl.triggers(sv.id); need('remove takes it away', !list.some((t) => t.id === tr.id));
        // publish, then the list: search, categories, duplicate, delete
        const pub = await cl.publish(sv.id, 'r451 first').catch((e) => ({ error: e.message })); need('publish through the plugin gives version 1', pub.version === 1, JSON.stringify(pub));
        const items = await cl.list(), mineRow = items.find((i) => i.id === sv.id);
        need('the list has the workflow with its version and categories; filter and categories work on it', !!mineRow && mineRow.version === 1 && WF.categories(items).some((c) => c.id === 'r451cat') && WF.filterList(items, { q: 'r451-ed' }).some((i) => i.id === sv.id) && WF.filterList(items, { category: 'r451cat' }).every((i) => (i.labels || []).includes('r451cat')) && WF.filterList(items, { q: 'no-such-thing-zz' }).length === 0, JSON.stringify(mineRow).slice(0, 160));
        const cname = WF.copyName('r451-edit', items.map((i) => i.name)), dup = await cl.duplicate(sv.id, cname), dupOpen = await cl.open(dup.id);
        need('duplicate saves a copy under a new name in the same scope', !!dup.id && dup.id !== sv.id && dupOpen.meta.name === cname && dupOpen.meta.scope === 'team-core' && (dupOpen.json.nodes || []).length === 3, JSON.stringify(dupOpen.meta).slice(0, 160));
        await cl.remove(dup.id); const gone = await cl.list(); need('delete removes it from the list', !gone.some((i) => i.id === dup.id) && gone.some((i) => i.id === sv.id));
      } catch (e) { why.push('threw: ' + e.message + ' ' + String(e.stack).split('\n')[1]); }
      hub451 = { ok: !why.length, detail: why.length ? '; hub: ' + why.join('; ') : '; hub: kinds, save / open round trip, validate, test run + overlay, chat, triggers, publish, list, duplicate, delete all match' };
    }
    for (const id of ['R447', 'R448', 'R449', 'R450', 'R452']) check(id, L447[id], ec[id].ok, ec[id].detail);
    check('R451', L447.R451, ec.R451.ok && hub451.ok, ec.R451.detail + hub451.detail);
    skip('R453', 'editor core: the same fixture (a document and a list of user commands) gives the same result on the web build and the native build', 'there is no native build of editor-core yet (the Rust port, pgm-editor, comes after the web page; open question 11); the web build\'s checks R447 to R452 are the results a native build is held to');
  }
  const ws1 = (await call('/api/work/current', U)).data;
  await call('/api/work/queue', U, { queue: [], sendNow: null }); await call('/api/work/current', U, { transcript: [] });
  check('R135', 'Work state survives restarts: the conversation on screen (me / claude / tool entries, its saved-session id + title) and the queue + a pending "Send now" are kept per person on the hub (their profile folder), come back unchanged after a hub restart, and are nobody else\'s; bad input refused, empty messages dropped; [static] every queue change is saved, opening Work restores the conversation and the queue and starts the queue running, New chat clears it',
    wsSave.status === 200 && wsQueue.status === 200 && wsBad.status === 400 && wsFile && (ws0.transcript || []).length === 0 && (wsOther.transcript || []).length === 0 && (wsOther.queue || []).length === 0
    && pid2 !== pid1 && (ws1.transcript || []).map((t) => t.cls + ':' + t.text).join('|') === 'me:hello r135|claude:answer r135|tool:Read file r135' && ws1.archiveId === 'a-r135' && ws1.archiveTitle === 'Title r135'
    && (ws1.queue || []).join('|') === 'first queued r135|second queued r135' && ws1.sendNow === 'send now r135'
    && /function saveQueue\(\)/.test(html) && /x\.onclick = \(\) => \{[^\n]*saveQueue\(\)/.test(html) && /next\.el\.remove\(\); saveQueue\(\); send\(next\.message\)/.test(html) && /sendNowMessage = text;[^\n]*saveQueue\(\)/.test(html)
    && /restoreFolder\(\); restoreWork\(\);/.test(html) && /function restoreWork\(/.test(html) && /function record\(cls, text\) \{ transcript\.push\(\{ cls, text \}\); saveCurrentSoon\(\); \}/.test(html) && /transcript = items; saveCurrentSoon\(\)/.test(html),
    `after restart (pid ${pid1}→${pid2}): ${JSON.stringify(ws1).slice(0, 220)}`);

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

  if (RUST_HUB) { // R179 (FR6): who answers each route; the list of routes still answered by the JavaScript hub must shrink to none
    // Every /api/ path the JavaScript hub names (bridge/server.js: `p === '…' && req.method === '…'`, and the modules it
    // routes to), probed as the admin with the method the JavaScript hub expects (GET; a POST with {} where the path is
    // POST-only, since a GET on it answers 404 from whoever owns it). Who answers is what counts, not the status. Routes
    // that act (restart, clear, delete, sign-out, passwords, stop, handoff …) are never probed. Last in the suite: a
    // probe that changed something (presence, typing, seen) disturbs nothing after it.
    const srv179 = fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8');
    const methods179 = new Map();
    for (const m of srv179.matchAll(/p === '(\/api\/[a-z0-9/_-]+)'(?: && req\.method === '([A-Z]+)')?/g)) methods179.set(m[1], m[2] || (methods179.get(m[1]) || 'GET'));
    for (const m of srv179.matchAll(/p\.startsWith\('(\/api\/[a-z0-9/_-]+)'\)/g)) if (!methods179.has(m[1] + 'probe')) methods179.set(m[1].endsWith('/') ? m[1] + 'probe' : m[1], 'GET');
    const js179 = ['files.js', 'notes.js', 'linux-accounts.js', 'forgejo.js', 'guilds.js', 'calls.js', 'shares.js', 'archive.js', 'user-profile.js', 'handoff.js', 'host-relay.js', 'usage.js', 'providers.js', 'board.js', 'assistant.js', 'ai-nodes.js', 'ai-nodes-front.js', 'openai.js'].map((f) => { try { return fs.readFileSync(path.join(BRIDGE, f), 'utf8'); } catch { return ''; } }).join('\n');
    for (const m of js179.matchAll(/'(\/api\/[a-z0-9/_-]+)'/g)) if (!methods179.has(m[1])) methods179.set(m[1], 'ANY');
    const skip179 = /(logout|restart|\/clear|\/delete|\/stop$|password|create|signout|unlock|allow-device|set-admin|handoff|\/remove|disconnect|library\/exec|workspace|\/kill|\/reset|\/forget|unity-tools|room\/send|room\/file|work\/file|\/exec)/;
    const probe = async (p, method, body) => { try { const r = await fetch(H + p, { method, headers: { Authorization: 'Bearer ' + A, 'Content-Type': 'application/json' }, body: method === 'POST' ? JSON.stringify(body || {}) : undefined }); return { status: r.status, by: r.headers.get('x-vr-by') || '-' }; } catch (e) { return { status: 0, by: '-' }; } };
    const by179 = [];
    for (const [p, method] of [...methods179.entries()].sort()) {
      if (skip179.test(p) || p.includes('/pub/')) continue;
      if (p.endsWith('/') || p.endsWith('/probe') || p.startsWith('/api/v1/') || p === '/api/show' || p === '/api/tags') continue; // prefixes, and Forgejo's / Ollama's API paths quoted in forgejo.js / assistant.js (not hub routes)
      let r = await probe(p, method === 'POST' ? 'POST' : 'GET');
      if (method === 'ANY' && r.by === 'node' && [404, 405].includes(r.status)) r = await probe(p, 'POST'); // the modules' routes: the method is not in the source line
      by179.push({ p, method: method === 'POST' ? 'POST' : method === 'ANY' && r.status !== 404 ? 'POST?' : 'GET', ...r });
    }
    const node179 = by179.filter((x) => x.by === 'node').map((x) => x.p), unmarked179 = by179.filter((x) => x.by === '-').map((x) => `${x.p}(${x.status})`), broken179 = by179.filter((x) => x.status >= 500 || !x.status).map((x) => `${x.p}(${x.status})`);
    check('R179', 'FR6 retire the JavaScript hub: every /api/ answer says who answered (x-vr-by: rust | node) and no route the JavaScript hub names is still answered by it (probed with the method it expects; routes that act are never probed)',
      unmarked179.length === 0 && node179.length === 0 && broken179.length === 0,
      `${by179.length} routes probed · still node (${node179.length}): ${node179.join(' ') || 'none'} · unmarked: ${unmarked179.join(' ') || 'none'} · broken: ${broken179.join(' ') || 'none'}`);
  }

  check('R11', 'account password files unchanged (scratch and production)', sha(path.join(SB, 'profiles', 't-admin', 'auth.json')) === authBefore
    && prodAccountFiles().every((f) => prodBefore[f] === sha(f)));

  // Ship hygiene (real repo, read-only)
  const terms = fs.existsSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'))
    ? fs.readFileSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'), 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#')) : [];
  const generic = ['[0-9a-f]{32}', '"hash"\\s*:\\s*"[0-9a-f]', 'C:\\\\\\\\Users\\\\\\\\', 'trycloudflare\\.com/', 'password\\s*[:=]\\s*"[^"]{6,}'];
  const pattern = [...generic, ...terms.map((t) => '\\b' + t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b')].join('|');
  // web/vendor holds third-party libraries (checked by R102 against their published hashes), not our text
  const gg = spawnSync('git', ['grep', '-n', '-I', '-i', '-E', pattern, '--', '.', ':!bridge/test/regress.js', ':!web/vendor', ':!native/Cargo.lock', ':!native/containers/*/Containerfile', ':!native/containers/*/*requirements.txt', ':!native/containers/*/*/package-lock.json'], { cwd: MILLS, encoding: 'utf8' }); // Cargo.lock and the container pins (Containerfile digests / download checksums, pip and npm locks): published checksums, not ours
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
  } catch (e) { if (e.stopAfter) console.log(e.message); else { console.log('RUNNER ERROR: ' + e.stack); code = 2; results.push({ id: 'RUNNER', label: 'runner crashed; every check after the last PASS above never ran', status: 'FAIL', detail: e.message }); } }
  stopHub();
  reTestCleanup();
  if (ollamaSrv) ollamaSrv.close();
  await sleep(800);
  const failed = results.filter((r) => r.status === 'FAIL'), skipped = results.filter((r) => r.status === 'SKIP'), pending = results.filter((r) => r.status === 'SPEC');
  console.log(`\n${results.length - failed.length - skipped.length - pending.length} passed, ${failed.length} failed, ${skipped.length} skipped${pending.length ? `, ${pending.length} spec pending (not built yet: ${pending.map((r) => r.id).join(' ')})` : ''}.`);
  if (failed.length) code = code || 1;
  if (SCRATCH && !KEEP) { try { fs.rmSync(SCRATCH, { recursive: true, force: true }); } catch (e) { console.log('Could not delete scratch folder: ' + e.message); } }
  else if (SCRATCH) console.log('Scratch kept at ' + SCRATCH);
  process.exit(code);
})();
