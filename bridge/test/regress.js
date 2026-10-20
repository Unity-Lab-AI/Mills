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
      if (req.url === '/oai/chat/completions') {
        const b = JSON.parse(body || '{}'), last = [...(b.messages || [])].reverse().find((m) => m.role === 'user') || { content: '' }, sys = (b.messages || []).find((m) => m.role === 'system');
        const text = 'oai says: ' + last.content + (sys ? ' [sys ' + sys.content.length + ']' : '');
        if (b.stream) { res.writeHead(200, { 'Content-Type': 'text/event-stream' }); for (const w of text.split(' ')) res.write('data: ' + JSON.stringify({ model: b.model, choices: [{ delta: { content: w + ' ' } }] }) + '\n\n'); return res.end('data: [DONE]\n\n'); }
        return j({ id: 'x', model: b.model, choices: [{ message: { role: 'assistant', content: text } }], usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 } });
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
        line({ content: `final sys-has-name=${/(VibeRoller|Mills) username "t-(user|admin)"/.test(sys)} tz=${(/\(([A-Za-z_]+\/[A-Za-z_]+)\)/.exec(sys) || [])[1] || ''}` });
      }
      res.end(JSON.stringify({ done: true }) + '\n');
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
  // what the Download panel packs (bridge/downloads.js): the AI node's launchers, example config and host tools
  for (const f of ['ai-node.example.json', 'start-node.bat', 'start-node.sh']) fs.copyFileSync(path.join(BRIDGE, f), path.join(SB, f));
  fs.cpSync(path.join(BRIDGE, 'host-tools'), path.join(SB, 'host-tools'), { recursive: true, filter: (s) => !/\.(exe|ico)$/i.test(s) });
  fs.cpSync(path.join(BRIDGE, 'desktop'), path.join(SB, 'desktop'), { recursive: true }); // the Desktop packages' setup scripts
  fs.cpSync(path.join(MILLS, 'web'), path.join(SCRATCH, 'web'), { recursive: true });
  for (const f of ['versions.json', 'CHANGELOG.md']) if (fs.existsSync(path.join(MILLS, f))) fs.copyFileSync(path.join(MILLS, f), path.join(SCRATCH, f)); // semver (law 14)
  PORT = await freePort(7799);
  H = `http://127.0.0.1:${PORT}`;
  const real = readJson(path.join(BRIDGE, 'config.json'), {});
  // The scratch copy is its own git repo (main + develop) so per-account worktrees work there.
  const g = (...a) => execFileSync('git', a, { cwd: SCRATCH, stdio: 'ignore', windowsHide: true });
  fs.writeFileSync(path.join(SCRATCH, '.gitignore'), ['bridge/profiles/', 'bridge/profiles-deleted/', 'bridge/*.json', 'bridge/.restart', 'Chat/', 'Archive/', 'worktrees/', 'projects/', 'upstream.git/', 'host-pc/', 'host-claude-login/', 'host-codex/', 'Api/', 'bridge/host2/', 'bridge-handoff/', 'rust-host/', 'native/', 'handoff-run.log', 'hub.log', 'ai-node.log', 'host2.log', 'bridge/ai-node.lock', ''].join('\n'));
  g('init', '-q', '-b', 'main'); g('config', 'user.email', 'regress@example.invalid'); g('config', 'user.name', 'regress');
  g('add', '-A'); g('commit', '-q', '-m', 'scratch'); g('branch', 'develop');
  await startFakeOllama();
  const cfg = { ...real, assistant: { ...(real.assistant || {}), ollamaUrl: `http://127.0.0.1:${OLLAMA_PORT}`, imageUrl: `http://127.0.0.1:${OLLAMA_PORT}`, voiceUrl: `http://127.0.0.1:${OLLAMA_PORT}`, speechUrl: `http://127.0.0.1:${OLLAMA_PORT}`, searchUrl: `http://127.0.0.1:${OLLAMA_PORT}/search`, youtubeUrl: `http://127.0.0.1:${OLLAMA_PORT}/results`, via: 'node' }, aiNodes: { 'test-node': NODE_SECRET, 'old-node': OLD_NODE_SECRET, 'upd-node': OLD_NODE_SECRET + '-upd', 'nohello-node': OLD_NODE_SECRET + '-nh', 'group-node': OLD_NODE_SECRET + '-grp', 'rust-node': OLD_NODE_SECRET + '-rs' }, hostGroup: ['t-user'], linuxPasswdFile: path.join(FAKE_LINUX, 'passwd'), aiNodeOfflineMs: 4000, port: PORT, tunnel: false, gistId: '', worktreeRoot: path.join(SCRATCH, 'worktrees'), projectsRoot: path.join(SCRATCH, 'projects'), projectsLocalClone: true, claudeCommand: process.execPath, claudeArgs: [path.join(SB, 'test', 'fake-claude.js')] };
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
}
// The throwaway hub runs Unity's engines THROUGH an AI node (bridge/ai-node.js, its own process), the way the hub on
// the server will: every Unity check below then also tests the node path end to end (R137).
const NODE_SECRET = 'test-node-secret-0123456789abcdef';
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
const NATIVE_TARGET = path.join(MILLS, 'native', 'target', 'regress');
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
  const r = spawnSync('cargo', ['build', '--release', '-q', '-p', 'vr-host', '-p', 'vr-core', '--examples', '--bins'], { cwd: path.join(MILLS, 'native'), env, encoding: 'utf8', timeout: 1200000 });
  return (nativeBuilt = buildResult(r));
}
function buildResult(r) {
  if (r.error) return 'no Rust toolchain here (' + r.error.code + ')';
  return r.status === 0 && fs.existsSync(NATIVE_BIN.host) ? '' : 'cargo build failed: ' + String(r.stderr || '').trim().split('\n').slice(-3).join(' ');
}
const RUST_HOST = (() => {
  if (!process.argv.includes('--rust-host')) return '';
  const why = buildNative();
  if (why) { console.error('--rust-host: ' + why); process.exit(2); }
  return NATIVE_BIN.host;
})();
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
  const PAGE_SCRIPTS = ['attach.js', 'library.js', 'calls.js', 'guilds.js', 'guild-settings.js', 'profile.js', 'notes.js', 'app.js'];
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
  const otherTouch = await call('/api/providers/update', A, { id: cli.provider.id, shared: true });
  const shareIt = await call('/api/providers/update', U, { id: cli.provider.id, shared: true });
  const nowSeenByA = (await call('/api/providers', A)).data.providers.map((p) => p.id);
  const rmOther = await call('/api/providers/remove', A, { id: cli.provider.id });
  const rm = await call('/api/providers/remove', U, { id: cli.provider.id });
  const ck7b = (await call('/api/check', U)).data;
  await call('/api/providers/remove', U, { id: oai.provider.id }); await call('/api/apikey/revoke', U, {}); await call('/api/model', U, { model: 'claude-opus-5-5', provider: 'host' });
  check('R112', 'AI providers: the host Claude is always listed; a person adds a Claude Code login (own folder) and an OpenAI endpoint (private or shared; bad kind / key refused; secrets never listed); a private one is invisible to others; picking the login runs Work with CLAUDE_CONFIG_DIR set and that model; an OpenAI provider is refused for Work; the API key answers through the endpoint provider (plain + streaming, system passed); only the owner shares / removes; removal clears the person\'s choice',
    // (the test AI host's own providers, "h:…", are listed too: they survive the early hub restart since 3.3.0)
    prov0.providers.filter((p) => !String(p.id).startsWith('h:')).length === 1 && prov0.providers[0].id === 'host' && badKind.status === 400 && badAnth.status === 400 && cli.provider && oai.provider && !secretLeak
    && !seenByA.includes(cli.provider.id) && seenByA.includes(oai.provider.id)
    && pickCli.status === 200 && pickCli.data.provider === cli.provider.id && new RegExp('cfg=' + cfgDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).test(texts(cliRun.events)) && /model=claude-sonnet-5-5/.test(texts(cliRun.events)) && ck7.provider === cli.provider.id && (ck7.providers || []).some((p) => p.id === cli.provider.id)
    && pickOai.status === 400 && setOai.status === 200 && setOai.data.provider === oai.provider.id && oaiModels.status === 200 && /fake-mini/.test(oaiModels.text)
    && oaiChat.status === 200 && /^oai says: through-r112 \[sys 3\]/.test(oaiText) && /flow-r112/.test(oaiStreamed)
    && otherTouch.status === 400 && shareIt.status === 200 && nowSeenByA.includes(cli.provider.id) && rmOther.status === 400 && rm.status === 200 && ck7b.provider === 'host'
    && /id="provBox"/.test(html) && /\/api\/providers\/add/.test(html) && /o\.value = p\.id \+ '\|' \+ m/.test(html),
    `providers at start ${prov0.providers.map((p) => p.id).join(',')}; cli ${pickCli.status} run "${texts(cliRun.events).slice(-120)}", oai work ${pickOai.status}, api via oai ${oaiChat.status} "${oaiText.slice(0, 50)}", stream "${oaiStreamed.slice(0, 40)}", share ${otherTouch.status}/${shareIt.status}, remove ${rmOther.status}/${rm.status}`);
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
  {
    // R162: sessions kept as hashes; the page's scripts pinned by hash; every route's failure answered
    const hs = JSON.parse(fs.readFileSync(path.join(SB, 'hub-state.json'), 'utf8')), keys = Object.keys(hs.sessions || {});
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
    [/default-src 'none'/.test(csp) && /connect-src 'self' https:\/\/\*\.trycloudflare\.com https:\/\/api\.github\.com __MILLS_HUB_HOST__;/.test(csp) && /object-src 'none'/.test(csp) && /img-src 'self' https: data: blob:/.test(csp) && !/script-src[^;]*https:/.test(csp), 'CSP (images over https only; scripts never)'],
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
  const fjEnvOf = (events) => ((texts(events).match(/fj=([^\s|]*)/) || [])[1] || '');
  const fjRunU = fjEnvOf((await chat(U, 'forgejo env r122', { fresh: true })).events), fjRunA = fjEnvOf((await chat(A, 'forgejo env r122', { fresh: true })).events);
  const fjOff = (await call('/api/forgejo/disconnect', U, {})).data;
  const fjRunU2 = fjEnvOf((await chat(U, 'forgejo env r122 after', { fresh: true })).events);
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
  const modsOnDisk = [...fs.readdirSync(BRIDGE).filter((f) => f.endsWith('.js')).map((f) => 'bridge/' + f), ...fs.readdirSync(path.join(MILLS, 'web')).filter((f) => /\.(js|css)$/.test(f) || f === 'index.html').map((f) => 'web/' + f)];
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
      && ['bridge/uploads-index.json', 'bridge/uploads/x', 'bridge/ai-host-sessions.json', 'bridge/ai-node.status.json', 'Chat/files/x'].every((f) => spawnSync('git', ['check-ignore', '-q', f], { cwd: MILLS }).status === 0),
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
    // the package with this host's config inside, for its owner only
    const cfgZip = await fetch(H + '/api/downloads/ai-host-linux.zip?host=' + newId, { headers: { Authorization: 'Bearer ' + A } });
    const cfgZipBuf = Buffer.from(await cfgZip.arrayBuffer());
    const cfgEntry = cfgZip.status === 200 ? unzipAll(cfgZipBuf).find((e) => e.name.endsWith('/ai-node.json')) : null;
    const cfgJson = cfgEntry ? JSON.parse(cfgEntry.data.toString('utf8')) : {};
    const cfgZipOther = await fetch(H + '/api/downloads/ai-host-linux.zip?host=' + newId, { headers: { Authorization: 'Bearer ' + U } });
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
    check('R143', 'AI hosts: a host is a UUID + secret + nickname (adjective-animal); hello with the wrong secret → 401, without a nonce → 400; the handshake gives both sides the same e2e key (vr-… id) and the host tells the hub its user, providers (models, caps, allowed users) and engines; Settings → My AI hosts: new host (uuid, nickname), rename (bad nickname refused, only the owner), remove; the AI Host download with ?host= carries that host\'s config (owner only), and a host run from it connects and shows online; [static] HKDF + AES-256-GCM on both sides, randomUUID ids, ai-hosts.json gitignored, the Settings panels exist',
      testHost.online === true && testHost.nickname === 'test-node' && /^vr-[0-9a-f]{24}$/.test(testHost.kid || '') && testHost.user === '' && (testHost.providers || []).join(',') === 'claude,codex,local' && (testHost.engines || []).length === 4
      && helloBad.status === 401 && helloNoNonce.status === 400 && mine0.status === 200 && mine0.data.mine.length === 0 && made.status === 200 && /^[0-9a-f-]{36}$/.test(newId) && /^[a-z]+-[a-z]+(-\d+)?$/.test(made.data.host.nickname) && made.data.host.user === 't-admin'
      && renamed.status === 200 && renamed.data.host.nickname === 'brave-walrus-test' && badNick.status === 400 && notMine.status === 400 && mine1.data.mine.length === 1
      && cfgZip.status === 200 && cfgJson.id === newId && cfgJson.token && cfgJson.token.length >= 24 && cfgJson.user === 't-admin' && cfgJson.hub === H && Array.isArray(cfgJson.providers) && cfgZipOther.status === 500
      && !!h2 && h2.online === true && /^vr-[0-9a-f]{24}$/.test(h2.kid || '') && /connected to .* as "brave-walrus-test" \(t-admin\)/.test(host2Text) && removed.status === 200 && removed.data.mine.length === 0
      && /hkdfSync/.test(hubNodeSrc2) && /aes-256-gcm/.test(hubNodeSrc2) && /randomUUID/.test(hubNodeSrc2) && /hkdfSync/.test(nodeSrc2) && /aes-256-gcm/.test(nodeSrc2) && /^bridge\/ai-hosts\.json$/m.test(fs.readFileSync(path.join(MILLS, '.gitignore'), 'utf8')) && /id="hostBox"/.test(html2) && /id="srcBox"/.test(html2),
      `host ${JSON.stringify(testHost).slice(0, 160)}; hello bad ${helloBad.status} nonce ${helloNoNonce.status}; new ${made.status} ${JSON.stringify(made.data).slice(0, 120)}; rename ${renamed.status} bad ${badNick.status} other ${notMine.status}; zip ${cfgZip.status} cfg id ok ${cfgJson.id === newId} other ${cfgZipOther.status}; host2 ${JSON.stringify(h2 || {}).slice(0, 80)} log "${host2Text.split('\n')[0].slice(0, 100)}"`);

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
      && manR.status === 200 && sigOk && fileOk && notHost === 404 && sneaky === 404 && anon === 401 && selfT.status === 0 && /selftest: .* loads/.test(selfT.stdout)
      && upd.swapped && upd.trial && upd.backup && upd.confirmed
      && /CAPS = \['relay', 'update'\]/.test(nodeSrc152) && /cfg\.autoUpdate !== false/.test(nodeSrc152) && /t\.starts <= 3/.test(nodeSrc152) && /selftest\(UPD\.stage\)/.test(nodeSrc152) && /timingSafeEqual\(got, want\)/.test(nodeSrc152)
      && /if errorlevel 3 goto run/.test(bat) && /if errorlevel 4 goto crashed/.test(bat) && /3\) ;;/.test(nsh) && /sleep 10/.test(nsh),
      `old hello ${oldHello.status} probed "${probed}" old ${oldProv.remoteKind} new ${newProv.remoteKind}; manifest ${manR.status} sig ${sigOk} files ${(man.files || []).map((f) => f.name).join(',')}; file ${fileR.status} ok ${fileOk}; other ${notHost}/${sneaky} anon ${anon}; selftest ${selfT.status} "${String(selfT.stdout || selfT.stderr).trim().slice(0, 120)}"; self-update ${JSON.stringify(upd)}`);

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

  // The Download panel (Vibe Roller Desktop + Vibe Roller AI Host, Windows + Linux), and the page served by the hub itself.
  {
    const anon = await call('/api/downloads');
    const dl = await call('/api/downloads', U);
    const items = dl.data.items || [];
    const zipRes = await fetch(H + '/api/downloads/ai-host-windows.zip', { headers: { Authorization: 'Bearer ' + U } });
    const zipBuf = Buffer.from(await zipRes.arrayBuffer());
    const unknown = await fetch(H + '/api/downloads/nope.zip', { headers: { Authorization: 'Bearer ' + U } });
    const entries = unzipAll(zipBuf), names = entries.map((e) => e.name);
    const linuxZip = Buffer.from(await (await fetch(H + '/api/downloads/desktop-linux.zip', { headers: { Authorization: 'Bearer ' + U } })).arrayBuffer());
    const linuxFiles = unzipAll(linuxZip);
    const page = await (await fetch(H + '/')).text();
    const html = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8'), appJs = fs.readFileSync(path.join(MILLS, 'web', 'app.js'), 'utf8');
    check('R142', 'Download: signed-in people get the four packages (Vibe Roller Desktop + Vibe Roller AI Host, Windows + Linux) listed with sizes and as real zip files (PK header, attachment, every file of the AI host inside, the hub\'s own address in them); anonymous → 401; unknown → 404; the page the hub serves says so (it then talks to its own origin, whatever name reached it); the Download button sits above Settings in the menu and in the app\'s user panel',
      anon.status === 401 && dl.status === 200 && items.length === 4 && ['desktop-windows', 'desktop-linux', 'ai-host-windows', 'ai-host-linux'].every((id) => items.some((it) => it.id === id && it.bytes > 1000 && /\.zip$/.test(it.file)))
      && zipRes.status === 200 && zipBuf.slice(0, 4).equals(Buffer.from([0x50, 0x4b, 0x03, 0x04])) && /attachment; filename="VibeRollerAIHost-windows\.zip"/.test(zipRes.headers.get('content-disposition') || '') && zipRes.headers.get('content-type') === 'application/zip'
      && ['VibeRollerAIHost/ai-node.js', 'VibeRollerAIHost/common.js', 'VibeRollerAIHost/worktree.js', 'VibeRollerAIHost/pc-tools-mcp.js', 'VibeRollerAIHost/board-mcp.js', 'VibeRollerAIHost/team-mcp.js', 'VibeRollerAIHost/ai-node.example.json', 'VibeRollerAIHost/start-node.bat', 'VibeRollerAIHost/host-tools/HostTool.cs', 'VibeRollerAIHost/host-tools/AiHostService.cs', 'VibeRollerAIHost/host-tools/build-host-tools.ps1', 'VibeRollerAIHost/host-tools/build.cmd', 'VibeRollerAIHost/host-tools/install-ai-host-service.cmd', 'VibeRollerAIHost/README.txt'].every((n) => names.includes(n)) && !names.some((n) => n.endsWith('/ai-node.json'))
      && linuxZip.slice(0, 4).equals(Buffer.from([0x50, 0x4b, 0x03, 0x04])) && linuxFiles.length === 3 && linuxFiles.filter((e) => !e.name.endsWith('VERSION.txt')).every((e) => e.data.toString('utf8').includes(H)) && entries.some((e) => e.name.endsWith('README.txt') && e.data.toString('utf8').includes(H))
      && entries.some((e) => e.name.endsWith('ai-node.js') && e.data.equals(fs.readFileSync(path.join(BRIDGE, 'ai-node.js')))) && unknown.status === 404
      && /'hub' === 'hub'/.test(page) && !/__MILLS_SERVED__/.test(page) && /__MILLS_SERVED__/.test(html) && /SERVED_BY_HUB \|\|/.test(html)
      && /class="menuFoot menuDl"/.test(html) && html.indexOf('data-downloads') < html.indexOf('id="goSettings"') && /symbol id="i-download"/.test(html) && /openDownloads\(\)/.test(appJs) && /ic\('download'\)/.test(appJs)
      // Work's bar: Download right above Settings; Settings has its own Downloads tab, which every Download button opens
      && (() => { const rail = (html.match(/<nav id="rail">[\s\S]*?<\/nav>/) || [''])[0]; return rail.indexOf('data-downloads') > -1 && rail.indexOf('data-downloads') < rail.indexOf('data-settings'); })()
      && /data-settab="downloads"/.test(html) && /<div data-setpane="downloads" hidden>[\s\S]*id="dlBody"[\s\S]*id="appBox"/.test(html) && /PROFILE\.tab\('downloads'\)/.test(html) && !/id="downloads" class="modal"/.test(html),
      `anon ${anon.status} list ${dl.status} ${items.map((x) => x.id + ':' + x.bytes).join(' ')} zip ${zipRes.status} ${names.length} entries (${names.join(' ')}); ai-node same ${entries.some((e) => e.name.endsWith('ai-node.js') && e.data.equals(fs.readFileSync(path.join(BRIDGE, 'ai-node.js'))))}; linux ${linuxFiles.length} files, hub named ${linuxFiles.filter((e) => e.data.toString('utf8').includes(H)).length}; unknown ${unknown.status}; page served ${/'hub' === 'hub'/.test(page)}; menu order ${html.indexOf('data-downloads') < html.indexOf('id="goSettings"')}`);
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
  }

  // R155: every download is its own installer, launcher and updater (Linux paths run for real here; Windows: source)
  {
    const sh = (cmd) => spawnSync('sh', ['-c', cmd], { encoding: 'utf8' });
    const canRun = !sh('true').error && sh('command -v curl').status === 0;
    const canUnpack = sh('command -v unzip || command -v python3 || command -v bsdtar').status === 0;
    const pubVers = await fetch(H + '/api/pub/versions').then((r) => r.json()).catch(() => ({}));
    const pubDesk = await fetch(H + '/api/pub/desktop-linux.zip'), pubDeskBuf = Buffer.from(await pubDesk.arrayBuffer());
    const pubHost = (await fetch(H + '/api/pub/ai-host-linux.zip')).status, pubOdd = (await fetch(H + '/api/pub/auth.json')).status;
    const write = (buf, dir) => { for (const e of unzipAll(buf)) { const f = path.join(dir, e.name); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, e.data); if (/\.sh$/.test(e.name)) fs.chmodSync(f, 0o755); } };
    const out = {};
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-setup-home-'));
    try {
      if (!canRun) throw new Error('skip');
      const env = { ...process.env, HOME: home, PATH: '/usr/bin:/bin' + (process.env.PATH ? ':' + path.dirname(process.execPath) : '') };
      const run = (file, a = []) => spawnSync(file.endsWith('.js') ? process.execPath : 'sh', [file, ...a], { encoding: 'utf8', env, input: '', timeout: 60000 });
      // ---- Desktop (Linux) ----
      const pkg = path.join(home, 'Downloads'); write(pubDeskBuf, pkg);
      const dsh = path.join(pkg, 'VibeRollerDesktop', 'viberoller-desktop.sh'), STDD = path.join(home, '.local', 'share', 'viberoller', 'desktop');
      out.dStatus = /No other install found/.test(run(dsh, ['--status']).stdout);
      run(dsh, ['--install', '--yes']);
      const vOf = (d) => { try { return fs.readFileSync(path.join(d, 'VERSION.txt'), 'utf8').trim(); } catch { return ''; } };
      out.dInstalled = vOf(STDD) === pubVers['desktop-linux'] && fs.existsSync(path.join(home, '.local', 'share', 'applications', 'viberoller-desktop.desktop')) && fs.existsSync(path.join(home, '.local', 'bin', 'viberoller-desktop'));
      fs.writeFileSync(path.join(STDD, 'VERSION.txt'), '0.0.1+old\n'); // an older install …
      const dUpd = run(dsh); // … the downloaded package finds it and updates it
      out.dUpdated = vOf(STDD) === pubVers['desktop-linux'] && /updated to/.test(dUpd.stdout);
      if (canUnpack) { // the installed copy updates itself from the hub at its next start
        fs.writeFileSync(path.join(STDD, 'VERSION.txt'), '0.0.1+old\n');
        run(path.join(home, '.local', 'bin', 'viberoller-desktop'));
        out.dSelf = vOf(STDD) === pubVers['desktop-linux'];
      } else out.dSelf = 'no unzip / python3 here';
      // ---- AI Host (Linux) ----
      const hostZip = Buffer.from(await (await fetch(H + '/api/downloads/ai-host-linux.zip', { headers: { Authorization: 'Bearer ' + U } })).arrayBuffer());
      const hpkg = path.join(home, 'HostDownload'); write(hostZip, hpkg);
      const setup = path.join(hpkg, 'VibeRollerAIHost', 'vr-setup.js'), STDH = path.join(home, '.local', 'share', 'viberoller', 'ai-host');
      out.hStatus = /No other install found/.test(run(setup, ['--status']).stdout);
      const hInst = run(setup, ['--install', '--yes']);
      out.hInstalled = vOf(STDH) === pubVers['ai-host-linux'] && fs.existsSync(path.join(STDH, 'ai-node.js')) && fs.existsSync(path.join(STDH, '.vr-installed')) && /No ai-node\.json/.test(hInst.stdout);
      fs.writeFileSync(path.join(STDH, 'VERSION.txt'), '0.0.1+old\n'); fs.writeFileSync(path.join(STDH, 'ai-node.json'), '{"mine":true}\n'); fs.writeFileSync(path.join(STDH, 'ai-node.js'), '// stale-copy-r155\n');
      const hUpd = run(setup, ['--update']);
      out.hUpdated = vOf(STDH) === pubVers['ai-host-linux'] && fs.readFileSync(path.join(STDH, 'ai-node.json'), 'utf8') === '{"mine":true}\n' && !/stale-copy-r155/.test(fs.readFileSync(path.join(STDH, 'ai-node.js'), 'utf8')) && /Found an earlier install/.test(hUpd.stdout);
      // a host that runs from a git checkout is never overwritten
      fs.rmSync(STDH, { recursive: true, force: true });
      const gitHost = path.join(home, 'VibeRollerAIHost'); fs.mkdirSync(path.join(gitHost, '.git'), { recursive: true }); fs.writeFileSync(path.join(gitHost, 'ai-node.js'), '// git checkout\n');
      const hGit = run(setup, ['--yes']);
      out.hGitKept = /git checkout/.test(hGit.stdout) && fs.readFileSync(path.join(gitHost, 'ai-node.js'), 'utf8') === '// git checkout\n' && !fs.existsSync(STDH);
    } catch (e) { if (e.message !== 'skip') out.error = e.message; }
    finally { try { fs.rmSync(home, { recursive: true, force: true }); } catch {} }
    const ps1 = fs.readFileSync(path.join(BRIDGE, 'desktop', 'vr-desktop.ps1'), 'utf8'), setupSrc = fs.readFileSync(path.join(BRIDGE, 'vr-setup.js'), 'utf8'), dl155 = fs.readFileSync(path.join(BRIDGE, 'downloads.js'), 'utf8');
    check('R155', 'every download is its own installer, launcher and updater, Windows and Linux: each package carries VERSION.txt (the hub\'s version + a fingerprint of its contents) and /api/pub/versions names them (public, as are the Desktop packages; the AI Host packages are not); the Desktop finds no install → installs (menu entry, ~/.local/bin), finds an older one → updates it, and an installed copy updates itself from the hub at its next start; the AI Host setup installs into the standard folder, updates an older install keeping its ai-node.json, and never overwrites a host that runs from a git checkout; [static] the Windows Desktop (PowerShell) finds earlier installs through its folder and its shortcuts, installs or runs portable, updates itself; the AI Host setup finds the service\'s folder, holds a running launcher back, and starts a host its old launcher let stop',
      /^\d+\.\d+\.\d+\+[0-9a-f]{8}$/.test(pubVers['desktop-linux'] || '') && /^\d+\.\d+\.\d+\+[0-9a-f]{8}$/.test(pubVers['ai-host-windows'] || '') && pubDesk.status === 200 && pubDeskBuf.slice(0, 2).toString() === 'PK' && pubHost === 404 && pubOdd === 404
      && (!canRun || (out.dStatus && out.dInstalled && out.dUpdated && out.dSelf !== false && out.hStatus && out.hInstalled && out.hUpdated && out.hGitKept))
      && /function Find-Installs/.test(ps1) && /function Self-Update/.test(ps1) && /\[switch\]\$Portable/.test(ps1) && /api\/pub\/versions/.test(ps1) && /Expand-Archive/.test(ps1)
      && /sc', \['qc', 'VibeRollerAIHost'\]/.test(setupSrc) && /held\.push\(r\)/.test(setupSrc) && /followRestart/.test(setupSrc) && /publicRoute/.test(dl155),
      canRun ? JSON.stringify(out) : 'sh / curl not here: source checks only');
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

  // Work state (bridge/work-state.js) set before the restart, checked after it (R135)
  const ws0 = (await call('/api/work/current', U)).data;
  const wsSave = await call('/api/work/current', U, { transcript: [{ cls: 'me', text: 'hello r135' }, { cls: 'claude', text: 'answer r135' }, { cls: 'tool', text: 'Read file r135' }, { cls: 'bogus', text: 'dropped' }], archiveId: 'a-r135', archiveTitle: 'Title r135' });
  const wsQueue = await call('/api/work/queue', U, { queue: ['first queued r135', '', 'second queued r135'], sendNow: 'send now r135' });
  const wsBad = await call('/api/work/queue', U, { queue: 'nope' });
  const wsOther = (await call('/api/work/current', A)).data;
  const wsFile = fs.existsSync(path.join(SB, 'profiles', 't-user', 'work-current.json'));
  // Persistence & restart (scratch restart.js: graceful flag only)
  const authBefore = sha(path.join(SB, 'profiles', 't-admin', 'auth.json'));
  const pid1 = (await call('/api/status')).data.pid;
  spawnSync(process.execPath, [path.join(SB, 'restart.js')], { cwd: SB });
  let pid2 = pid1;
  for (let i = 0; i < 60 && (pid2 === pid1 || !pid2); i++) { await sleep(250); pid2 = (await call('/api/status')).data.pid; }
  const after = await call('/api/check', U);
  check('R9', 'graceful restart: new pid, token kept, lock + model kept, flag consumed', pid2 && pid2 !== pid1 && after.status === 200 && after.data.model === 'claude-opus-5-5'
    && (await login('t-user', 'pw-t-user-0001', devA)).status === 403 && !fs.existsSync(path.join(SB, '.restart')));
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
      && /X-VR-Version/.test(fs.readFileSync(path.join(BRIDGE, 'server.js'), 'utf8')) && /function pageUpdateAvailable\(build\)/.test(page154) && /hubBuild !== \(me\.build \|\| me\.version\)/.test(page154),
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
