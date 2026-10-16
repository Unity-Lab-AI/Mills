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
  const html = fs.readFileSync(path.join(MILLS, 'web', 'index.html'), 'utf8');
  try { new Function(html.slice(html.indexOf('<script>') + 8, html.lastIndexOf('</script>'))); } catch (e) { pageOk = false; pageErr = 'page: ' + e.message; }
  if (spawnSync(process.execPath, ['--check', path.join(MILLS, 'web', 'md.js')]).status !== 0) bad.push('md.js');
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
    tts: { on: true, volume: 400, rate: 10, voice: 'x'.repeat(300), scope: 'weird' }, clips: { volume: -5, autoplay: 1 },
    audio: { noise: false, echo: 1 }, call: { volume: 250, quality: 'ultra' }, autosave: 999 });
  const ttsOk = pr.data.prefs && pr.data.prefs.tts && pr.data.prefs.tts.on === true && pr.data.prefs.tts.volume === 100 && pr.data.prefs.tts.rate === 50
    && pr.data.prefs.tts.voice.length === 120 && pr.data.prefs.tts.scope === 'all'
    && pr.data.prefs.clips && pr.data.prefs.clips.volume === 0 && pr.data.prefs.clips.autoplay === true
    && pr.data.prefs.audio && pr.data.prefs.audio.noise === false && pr.data.prefs.audio.echo === true
    && pr.data.prefs.call && pr.data.prefs.call.volume === 100 && pr.data.prefs.call.quality === 'medium' && pr.data.prefs.autosave === 120;
  const prBad = await call('/api/prefs', A, { color: 99 });
  const memP = ((await call('/api/room/members', A)).data.members || []).find((m) => m.username === 't-user');
  check('R42', 'settings: nickname cleaned, colour kept, volumes / speech settings clamped, bad colour → none; others see nick + colour', pr.status === 200 && ttsOk && pr.data.prefs.nick === 'Tee Nick'
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
  check('R61', 'message format v3: decrypts with HKDF + authenticated sender/recipient/id, relabelled recipient fails, padded to a bucket, duplicate / bad id → 400, edit keeps its id, agent reads and answers in v3',
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
  check('R62', 'room long poll: held while nothing changes, answered right after a send (not on a timer), stale version answered at once',
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
  check('R63', 'format v4: text deflated before encryption (smaller than the text), clip only via /api/room/blob for its readers (not in the list), clip intact, v3 edit of a clip → 400, v4 caption edit keeps the clip, agent reads v4',
    sent4.status === 200 && !!list4 && !list4.blob && list4.blobLen > 0 && p4 && p4.t === longText + 'clip-r63' && fromB64(list4.ct).length < longText.length
    && blobU.status === 200 && blobD.status === 404 && !!audioBack && audioBack.equals(audio) && ed4bad.status === 400 && ed4.status === 200 && stillBlob.status === 200 && !!ans4,
    `send ${sent4.status}, body ${list4 ? fromB64(list4.ct).length : '-'} B for ${longText.length} B text, blob U ${blobU.status} / D ${blobD.status}, edit ${ed4bad.status}/${ed4.status}, agent ${!!ans4}`);

  // Hardening headers on every hub answer.
  const hr = await fetch(H + '/api/status');
  check('R64', 'hub answers carry nosniff, no-referrer, frame DENY; API answers are no-store',
    hr.headers.get('x-content-type-options') === 'nosniff' && hr.headers.get('referrer-policy') === 'no-referrer' && hr.headers.get('x-frame-options') === 'DENY' && /no-store/.test(hr.headers.get('cache-control') || ''));

  // Sign-in hashing is async now: parallel wrong guesses must still hit the 5-attempt limit.
  const burst = await Promise.all(Array.from({ length: 12 }, () => call('/api/login', null, { username: 'ghost-r65', password: 'wrong', deviceId: 'x'.repeat(32) })));
  const n401 = burst.filter((r) => r.status === 401).length, n429 = burst.filter((r) => r.status === 429).length;
  const statusDuring = await call('/api/status');
  check('R65', 'parallel wrong sign-ins: at most 5 are checked, the rest → 429; the hub stays responsive', n401 <= 5 && n401 + n429 === 12 && statusDuring.status === 200, `${n401}×401, ${n429}×429`);

  const csp = (/<meta http-equiv="Content-Security-Policy" content="([^"]+)"/.exec(html) || [])[1] || '';
  const missing66 = [
    [/default-src 'none'/.test(csp) && /connect-src 'self' https:\/\/\*\.trycloudflare\.com https:\/\/api\.github\.com;/.test(csp) && /object-src 'none'/.test(csp), 'CSP'],
    [/HUB_URL_RE = \/\^https:\\\/\\\/\[a-z0-9-\]\+\\\.trycloudflare\\\.com\$\//.test(html) && /store\.set\('mills\.hub'/.test(html), 'hub address pinned + cached'],
    [/function trustedPub\(/.test(html) && /changedKeys\.has\(m\.username\)\) continue; \/\/ never encrypt to an unverified key/.test(html) && /Trust new key/.test(html), 'key pinning'],
    [/streams: \[call\.local\]/.test(html) && /sender\.setStreams\(call\.local\)/.test(html) && /e\.streams && e\.streams\[0\]/.test(html), 'call lip-sync streams'],
    [/sendSignal\(user, \{ cs: peer\.iceOut\.splice\(0\) \}\)/.test(html), 'ICE batching'],
    [/&wait=1&ver=' \+ roomVer/.test(html) && !/setInterval\(\(\) => \{ pollRoom\(\)/.test(html), 'room long poll, no 3 s timer'],
    [/new CompressionStream\('deflate-raw'\)/.test(html) && /\/api\/room\/blob\?seq=/.test(html), 'v4 deflate + clip on play'],
  ].filter(([ok]) => !ok).map(([, n]) => n);
  const mdc = await require('./md-check.js').run();
  const mdSrc = fs.readFileSync(path.join(MILLS, 'web', 'md.js'), 'utf8');
  const mdStatic = !/\.innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(mdSrc) && /securityLevel: 'strict'/.test(mdSrc) && /htmlLabels: false/.test(mdSrc)
    && /<script src="md\.js"><\/script>/.test(html) && /MD\.render\(el, text\)/.test(html) && /MD\.render\(el, text, \{ mention:/.test(html);
  check('R67', 'markdown renderer (web/md.js): headings, emphasis, links (safe only), lists / tasks, tables, quotes / alerts, code with colouring + Copy, mermaid on demand, math, footnotes, mentions; raw HTML and javascript: links stay text; never assigns HTML; used by both chats',
    !mdc.failed.length && mdStatic, mdc.failed.length ? mdc.failed.join(' | ') : (mdStatic ? `${mdc.total} cases` : 'static check failed'));
  // Custom SVG icons instead of emoji: nothing the page shows may be an emoji (code comments and the
  // regex that strips the old "⚙ " from saved sessions are not shown), and every icon used exists.
  const EMOJI = /[\u{1F000}-\u{1FAFF}\u{2600}-\u{27BF}\u{2B00}-\u{2BFF}\u{23E9}-\u{23FA}\u{25B6}\u{27F3}\u{21A9}\u{21AA}\u{2713}\u{2715}\u{25A0}]/u;
  const shown = (src) => src.split('\n').map((l) => l.replace(/^\s*\/\/.*$/, '').replace(/\s\/\/ .*$/, '').replace(/<!--.*?-->/g, '')).filter((l) => !/const toolText/.test(l));
  const emojiLines = [...shown(html).map((l, k) => [l, 'index.html:' + (k + 1)]), ...shown(fs.readFileSync(path.join(MILLS, 'web', 'md.js'), 'utf8')).map((l, k) => [l, 'md.js:' + (k + 1)])]
    .filter(([l]) => EMOJI.test(l) && !/^\s*\*|\/\*/.test(l)).map(([, at]) => at);
  const symbols = new Set([...html.matchAll(/<symbol id="i-([\w-]+)"/g)].map((m) => m[1]));
  const code68 = shown(html).join('\n');
  const usedIcons = new Set([...code68.matchAll(/href="#i-([\w-]+)"/g), ...code68.matchAll(/\bic\('([\w-]+)'/g), ...code68.matchAll(/\blabel\([^,]+, '([\w-]+)'/g)].map((m) => m[1])
    .concat(['reply', 'edit', 'trash', 'stop', 'send', 'talk', 'image', 'mic', 'mic-off', 'speaker', 'speaker-off', 'folder', 'file'])); // via add(), dictLabel(), ternaries, md.js
  const missingIcons = [...usedIcons].filter((n) => !symbols.has(n));
  const serverEmoji = /mirror\(acct, 'tool', '⚙/.test(fs.readFileSync(path.join(MILLS, 'bridge', 'server.js'), 'utf8'));
  check('R68', '[static] no emoji shown anywhere on the site (custom SVG icon sprite instead), every icon used exists in the sprite, hub tool lines carry no emoji',
    !emojiLines.length && !missingIcons.length && symbols.size >= 30 && !serverEmoji, [emojiLines.slice(0, 8).join(' '), missingIcons.join(' ')].filter(Boolean).join(' | ') || `${symbols.size} icons`);
  check('R66', '[static] CSP, hub address pinned to the tunnel, key pinning (TOFU + Trust new key), call lip-sync + ICE batching, room long poll, v4 deflate + clips fetched on play', !missing66.length, missing66.join(', '));
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
  const camState = await call('/api/call/state', A, { conv: 'room', cam: true, mic: false });
  const afterCam = (((await call('/api/call/poll?after=0&ver=-1', U)).data.calls || {}).room || []).find((p) => p.user === 't-admin');
  await call('/api/call/leave', A, {});
  const afterLeave = (((await call('/api/call/poll?after=0&ver=-1', U)).data.calls || {}).room || []).map((p) => p.user);
  check('R50', 'calls: presence per conversation, private calls only for their two people, one call at a time, signals relayed opaquely and only inside the call, long-poll wakes on a signal, cam/mic state, leave',
    jA.status === 200 && Array.isArray(jA.data.iceServers) && ((pollU.calls || {}).room || []).some((p) => p.user === 't-admin') && ((pollD.calls || {}).room || []).some((p) => p.user === 't-admin')
    && dmJoin.status === 200 && dmIntruder.status === 400 && badConv.status === 400 && !dmSeenByD && !(stU.calls || {})['dm:t-admin|t-user']
    && sigToD.status === 409 && sig.status === 200 && !!got1 && got1.ct === opaque.ct && waited >= 600 && waited < 20000
    && camState.status === 200 && afterCam && afterCam.cam === true && afterCam.mic === false && !afterLeave.includes('t-admin') && afterLeave.includes('t-user'),
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
  check('R69', 'usage sync: a limit event without numbers keeps the last utilization + reset time; a percentage (55) is read as 55 %; the page shows the 5h / 7d % from the same number as the fill, re-reads usage live during a reply and every 15 s, and empties a window once it resets',
    uPartial.five_hour && uPartial.five_hour.utilization === 0.42 && uPartial.five_hour.resetsAt === uAfter.five_hour.resetsAt
    && uPartial.seven_day && uPartial.seven_day.source === 'claude' && uPartial.seven_day.utilization === 0.55
    && /<span class="uPctS">/.test(html) && /function windowUse\(/.test(html) && /refreshUsageSoon\(\)/.test(html) && /if \(left === 0\) return \{ left, pct: 0/.test(html) && /15000\); \/\/ the limits are shared/.test(html),
    JSON.stringify({ five: uPartial.five_hour, seven: uPartial.seven_day }).slice(0, 220));
  check('R53', '[static] usage bar above the Claude messages: 5h hh:mm:ss, 7d dd/hh/mm, colour scale green → orange → yellow → red, purple when out',
    /id="usageBar"/.test(html) && html.indexOf('id="usageBar"') < html.indexOf('id="log"') && /\[0, \[46, 204, 113\]\], \[33, \[255, 159, 26\]\], \[66, \[255, 212, 59\]\], \[100, \[255, 59, 59\]\]/.test(html)
    && /OUT_COLOR = 'rgb\(160, 90, 255\)'/.test(html) && /pad2\(Math\.floor\(s \/ 3600\)\) \+ ':'/.test(html) && /'d ' \+ pad2/.test(html));
  check('R54', '[static] calls: speaking ring from a local level meter, per-person volume + mute kept only in this browser, Mute / Unmute button with M shortcut',
    /\.tile\.speaking \{/.test(html) && /createAnalyser\(\)/.test(html) && /addMeter\(user, e\.track\)/.test(html) && /mills\.vol\.' \+ u/.test(html)
    && /mills\.mute\.' \+ user/.test(html) && /className = 'pVol'/.test(html) && /micOn \? 'mic-off' : 'mic', micOn \? 'Mute' : 'Unmute'/.test(html) && /M = mute \/ unmute yourself/.test(html));
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
  await call('/api/work/share', A, { conv: 'dm:t-admin|t-user', transcript: [] });
  const dmD = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', D)).data, dmU = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', U)).data;
  const selfWatch = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', A)).data;
  await call('/api/work/share', A, { conv: '' });
  const afterStop = (await call('/api/work/watch?user=t-admin&after=0&ver=-1&since=', U)).data;
  check('R58', 'shared Work: listed for the conversation (not to yourself), seeded transcript, live run wakes watchers, typing + queue live, context reaches own Claude but not watchers, private share only for that person, stop',
    shA.status === 200 && shareListU.some((s) => s.user === 't-admin' && s.conv === 'room') && !shareListA.some((s) => s.user === 't-admin')
    && seedOk && liveRun && waited58 >= 500 && ranA.status === 200 && wa3.live && wa3.live.typing === 'typing-r58' && (wa3.live.queue || [])[0] === 'queued-r58'
    && texts(ctxRun.events).includes('CTX-SUMMARY-R58') && !ctxMirrored && dmD.stopped === true && !dmU.stopped && selfWatch.stopped === true && afterStop.stopped === true,
    `seed ${seedOk}, live ${liveRun} (${waited58} ms), dm D ${dmD.stopped}/U ${!dmU.stopped}`);
  check('R58b', '[static] share selector, Join work banner, watch-only windows with live typing + queue, summaries in Work messages',
    /id="shareSel"/.test(html) && /id="shareBanner"/.test(html) && /function openWatch\(/.test(html) && /\/api\/work\/watch\?user=/.test(html)
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
  } catch (e) { console.log('RUNNER ERROR: ' + e.stack); code = 2; results.push({ id: 'RUNNER', label: 'runner crashed; every check after the last PASS above never ran', status: 'FAIL', detail: e.message }); }
  stopHub();
  await sleep(800);
  const failed = results.filter((r) => r.status === 'FAIL'), skipped = results.filter((r) => r.status === 'SKIP');
  console.log(`\n${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped.`);
  if (failed.length) code = code || 1;
  if (SCRATCH && !KEEP) { try { fs.rmSync(SCRATCH, { recursive: true, force: true }); } catch (e) { console.log('Could not delete scratch folder: ' + e.message); } }
  else if (SCRATCH) console.log('Scratch kept at ' + SCRATCH);
  process.exit(code);
})();
