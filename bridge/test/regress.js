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
'use strict';

const fs = require('fs');
const os = require('os');
const net = require('net');
const path = require('path');
const crypto = require('crypto');
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
const { allProfiles, readJson } = require(path.join(BRIDGE, 'common.js'));
function prodAccountFiles() {
  return allProfiles().flatMap((p) => {
    const dir = p ? path.join(BRIDGE, 'profiles', p) : BRIDGE;
    return [path.join(dir, 'auth.json'), ...(p ? [path.join(dir, 'config.json')] : [])];
  });
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

let SCRATCH, SB, PORT, H, hub = null;
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
  const cfg = { ...real, port: PORT, tunnel: false, gistId: '', worktreeRoot: path.join(SCRATCH, 'worktrees'), claudeCommand: process.execPath, claudeArgs: [path.join(SB, 'test', 'fake-claude.js')] };
  fs.writeFileSync(path.join(SB, 'config.json'), JSON.stringify(cfg, null, 2));
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
  const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }, body: JSON.stringify({ message, fresh: !!opts.fresh }) });
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
const rand = (n) => globalThis.crypto.getRandomValues(new Uint8Array(n));
const enc = new TextEncoder(), dec = new TextDecoder();
const toB64 = (b) => Buffer.from(b).toString('base64'), fromB64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));
const ECDH = { name: 'ECDH', namedCurve: 'P-256' };
async function pwKey(pw, salt) { const b = await subtle.importKey('raw', enc.encode(pw), 'PBKDF2', false, ['deriveKey']); return subtle.deriveKey({ name: 'PBKDF2', salt, iterations: 310000, hash: 'SHA-256' }, b, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']); }
async function lock(priv, pw) { const salt = rand(16), iv = rand(12); const ct = await subtle.encrypt({ name: 'AES-GCM', iv }, await pwKey(pw, salt), await subtle.exportKey('pkcs8', priv)); return { salt: toB64(salt), iv: toB64(iv), ct: toB64(ct) }; }
async function unlock(b, pw) { const p = await subtle.decrypt({ name: 'AES-GCM', iv: fromB64(b.iv) }, await pwKey(pw, fromB64(b.salt)), fromB64(b.ct)); return subtle.importKey('pkcs8', p, ECDH, true, ['deriveKey']); }
const shared = async (priv, jwk) => subtle.deriveKey({ name: 'ECDH', public: await subtle.importKey('jwk', { kty: 'EC', crv: 'P-256', x: jwk.x, y: jwk.y }, ECDH, false, []) }, priv, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);

// ---------- the checklist ----------
async function run() {
  const devA = 'device-aaaaaaaaaaaaaaaa', devB = 'device-bbbbbbbbbbbbbbbb', devC = 'device-cccccccccccccccc';

  // Hub & sign-in
  const scripts = fs.readdirSync(BRIDGE).filter((f) => f.endsWith('.js')).map((f) => path.join(BRIDGE, f)).concat(fs.readdirSync(__dirname).map((f) => path.join(__dirname, f)));
  const bad = scripts.filter((f) => spawnSync(process.execPath, ['--check', f]).status !== 0);
  let pageOk = true;
  const html = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
  try { new Function(html.slice(html.indexOf('<script>') + 8, html.lastIndexOf('</script>'))); } catch { pageOk = false; }
  check('R1', 'every bridge script passes node --check; page script parses', !bad.length && pageOk, bad.map((f) => path.basename(f)).join(', '));

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
  const agentMember = ((await call('/api/room/members', U)).data.members || []).find((m) => m.username === 'claude');
  check('R35a', 'room agent listed in members (agent: true, public key)', !!(agentMember && agentMember.agent && agentMember.pub && agentMember.pub.x));
  const greet = await waitAgent(U, restored, 0, (t) => /just signed in/.test(t), 30000);
  check('R38', 'sign-in greeting from the agent (Unity) that the person decrypts', !!greet && !!agentMember && agentMember.display === 'Unity');
  await sleep(2500); // let any queued greeting run finish
  const seq0 = Math.max(0, ...((await call('/api/room/messages?since=0', U)).data.messages || []).map((m) => m.seq));
  await roomSend(U, restored, 'just chatting, nobody named');
  await sleep(3000);
  const quiet = !(await agentMsgs(U, restored, seq0)).length;
  await roomSend(U, restored, '@claude ping-r36');
  const ans = await waitAgent(U, restored, seq0, (t) => t.includes('ping-r36'));
  const ansA = ans && (await agentMsgs(A, privA, seq0)).find((x) => x.seq === ans.seq);
  await roomSend(A, privA, '@unity ping-unity');
  const viaUnity = await waitAgent(A, privA, seq0, (t) => t.includes('ping-unity'));
  check('R36', 'unnamed message → no reply; @claude → one reply every member decrypts; @unity also calls it', quiet && !!ans && !!(ansA && ansA.text) && !!viaUnity);
  await sleep(1500);
  const pidR = (await call('/api/status')).data.pid;
  await roomSend(U, restored, '@claude SLOW ping-r37');
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
  const cwdOf = (events) => ((texts(events).match(/cwd=(.+)$/m) || [])[1] || '').trim();
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
  await slow2;
  let pid4 = midPid;
  for (let i = 0; i < 60 && (pid4 === pid3 || !pid4); i++) { await sleep(250); pid4 = (await call('/api/status')).data.pid; }
  check('R10', 'restart waits for an active run, then happens', midPid === pid3 && pid4 && pid4 !== pid3);

  check('R11', 'account password files unchanged (scratch and production)', sha(path.join(SB, 'profiles', 't-admin', 'auth.json')) === authBefore
    && prodAccountFiles().every((f) => prodBefore[f] === sha(f)));

  // Ship hygiene (real repo, read-only)
  const terms = fs.existsSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'))
    ? fs.readFileSync(path.join(MILLS, 'docs', 'hygiene-terms.txt'), 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#')) : [];
  const generic = ['[0-9a-f]{32}', '"hash"\\s*:\\s*"[0-9a-f]', 'C:\\\\\\\\Users\\\\\\\\', 'trycloudflare\\.com/', 'password\\s*[:=]\\s*"[^"]{6,}'];
  const pattern = [...generic, ...terms.map((t) => '\\b' + t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\b')].join('|');
  const gg = spawnSync('git', ['grep', '-n', '-I', '-i', '-E', pattern, '--', '.', ':!bridge/test/regress.js'], { cwd: MILLS, encoding: 'utf8' });
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
  const prodPort = readJson(path.join(BRIDGE, 'config.json'), {}).port || 7777;
  let prod = null; try { prod = await (await fetch(`http://127.0.0.1:${prodPort}/api/status`)).json(); } catch {}
  check('R33', `production: ${prodPort} answers, no leftover old port 7778`, !!(prod && prod.ok) && !(await listening(7778)), prod ? `pid ${prod.pid}, supervised ${prod.supervised}` : 'not answering');
  const accts = allProfiles();
  const intact = accts.every((p) => { const a = readJson(path.join(p ? path.join(BRIDGE, 'profiles', p) : BRIDGE, 'auth.json'), null); return a && a.salt && a.hash; });
  check('R34', `production: every account (${accts.length}) still has an intact password file`, intact, 'sign-in itself is proven by the host (law 6), not by this runner');
  const tracked = spawnSync('git', ['ls-files', 'bridge/profiles', 'bridge/auth.json', 'bridge/state.json', 'bridge/hub-state.json'], { cwd: MILLS, encoding: 'utf8' }).stdout.trim();
  const configs = accts.filter(Boolean).every((p) => fs.existsSync(path.join(BRIDGE, 'profiles', p, 'config.json')));
  check('R35', 'account files are untracked (git cannot delete them on merge) and every account has its config.json', !tracked && configs, tracked || (configs ? '' : 'a profile is missing config.json'));
}

(async () => {
  let code = 0;
  try {
    await setup();
    console.log(`Throwaway hub: ${SCRATCH} on port ${PORT} (no tunnel). Production is only read.\n`);
    if (!(await startHub())) throw new Error('throwaway hub did not start; see ' + path.join(SCRATCH, 'hub.log'));
    await run();
  } catch (e) { console.log('RUNNER ERROR: ' + e.stack); code = 2; }
  stopHub();
  await sleep(800);
  const failed = results.filter((r) => r.status === 'FAIL'), skipped = results.filter((r) => r.status === 'SKIP');
  console.log(`\n${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped.`);
  if (failed.length) code = code || 1;
  if (SCRATCH && !KEEP) { try { fs.rmSync(SCRATCH, { recursive: true, force: true }); } catch (e) { console.log('Could not delete scratch folder: ' + e.message); } }
  else if (SCRATCH) console.log('Scratch kept at ' + SCRATCH);
  process.exit(code);
})();
