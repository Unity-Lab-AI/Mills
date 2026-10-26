#!/usr/bin/env node
// The JavaScript hub as the last fallback (T-157, LAWS.md "The JavaScript hub stays as the last fallback"): starts
// bridge/server.js ALONE the way vr-run's last resort does (node server.js --no-tunnel, MILLS_PORT, no Rust hub, no
// other hub), in a private temporary copy with its own data and the fake Claude, and probes the basics: /api/status,
// the page, the sign-in gate and sign-in, the chat room, the board, Work with Claude (one run), the AI host link
// (/node/hello). Used by bridge/test/regress.js (R192); runnable alone: `node bridge/test/js-fallback.js`.
'use strict';
const fs = require('fs');
const os = require('os');
const net = require('net');
const path = require('path');
const { spawn, spawnSync } = require('child_process');

const BRIDGE = path.join(__dirname, '..');
const MILLS = path.join(BRIDGE, '..');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const freePort = (from) => new Promise((resolve) => { const s = net.createServer(); s.once('error', () => resolve(freePort(from + 1))); s.listen(from, '127.0.0.1', () => s.close(() => resolve(from))); });

async function probe(opts = {}) {
  const out = { steps: {}, ok: false, why: '' };
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vr-jsfb-'));
  const B = path.join(root, 'bridge');
  let proc = null;
  try {
    fs.mkdirSync(path.join(B, 'test'), { recursive: true });
    for (const f of fs.readdirSync(BRIDGE)) if ((f.endsWith('.js') && f !== 'master-logins.js') || f === 'cloudflared-quick.yml') fs.copyFileSync(path.join(BRIDGE, f), path.join(B, f));
    fs.copyFileSync(path.join(__dirname, 'fake-claude.js'), path.join(B, 'test', 'fake-claude.js'));
    fs.cpSync(path.join(MILLS, 'web'), path.join(root, 'web'), { recursive: true });
    fs.cpSync(path.join(BRIDGE, 'host-tools'), path.join(B, 'host-tools'), { recursive: true, filter: (s) => !/\.(exe|ico)$/i.test(s) });
    for (const f of ['versions.json', 'CHANGELOG.md']) if (fs.existsSync(path.join(MILLS, f))) fs.copyFileSync(path.join(MILLS, f), path.join(root, f));
    const port = await freePort(opts.portFrom || 7950);
    const H0 = 'http://127.0.0.1:' + port; // the fake Claude's PC relay calls this hub (its MCP config names the config port 7777 on purpose)
    const node = 'probe-node', secret = 'probe-node-secret-0123456789abcdef';
    fs.writeFileSync(path.join(B, 'config.json'), JSON.stringify({ port: 7777, tunnel: false, gistId: '', rustHub: false, aiNodes: { [node]: secret }, worktreeRoot: path.join(root, 'worktrees'), projectsRoot: path.join(root, 'projects'), projectsLocalClone: true, claudeCommand: process.execPath, claudeArgs: [path.join(B, 'test', 'fake-claude.js')] })); // port 7777 on purpose: MILLS_PORT must win (the fallback takes the port vr-run serves)
    fs.writeFileSync(path.join(root, '.gitignore'), ['bridge/profiles/', 'bridge/*.json', 'Chat/', 'Archive/', 'worktrees/', 'projects/', ''].join('\n'));
    const g = (...a) => spawnSync('git', a, { cwd: root, stdio: 'ignore', windowsHide: true });
    g('init', '-q', '-b', 'main'); g('config', 'user.email', 'regress@example.invalid'); g('config', 'user.name', 'regress'); g('add', '-A'); g('commit', '-q', '-m', 'scratch'); g('branch', 'develop');
    const sp = (args) => spawnSync(process.execPath, [path.join(B, 'set-password.js'), ...args], { cwd: B, encoding: 'utf8' });
    sp(['pw-default-0001']);
    fs.mkdirSync(path.join(B, 'profiles', 't-admin'), { recursive: true });
    fs.writeFileSync(path.join(B, 'profiles', 't-admin', 'config.json'), JSON.stringify({ user: 'Tadmin', admin: true }));
    sp(['--profile', 't-admin', 'pw-t-admin-0001']);

    const logFile = path.join(root, 'hub.log');
    const log = fs.openSync(logFile, 'a');
    proc = spawn(process.execPath, [path.join(B, 'server.js'), '--no-tunnel'], { cwd: B, stdio: ['ignore', log, log], windowsHide: true, env: { ...process.env, MILLS_SUPERVISED: '1', MILLS_PORT: String(port), VR_SERVING: 'javascript-fallback', FAKE_CLAUDE_BRIDGE: H0 } });
    const H = `http://127.0.0.1:${port}`;
    const get = async (p, token, headers = {}) => { try { const r = await fetch(H + p, { headers: { ...(token ? { Authorization: 'Bearer ' + token } : {}), ...headers } }); return { status: r.status, data: await r.json().catch(() => ({})), type: r.headers.get('content-type') || '' }; } catch (e) { return { status: 0, data: {}, type: '' }; } };
    const post = async (p, body, headers = {}) => { try { const r = await fetch(H + p, { method: 'POST', headers: { 'Content-Type': 'application/json', ...headers }, body: JSON.stringify(body) }); return { status: r.status, data: await r.json().catch(() => ({})) }; } catch (e) { return { status: 0, data: {} }; } };
    let st = { status: 0, data: {} };
    for (let i = 0; i < 60 && st.status !== 200; i++) { st = await get('/api/status'); if (st.status !== 200) await sleep(250); }
    out.steps.status = st.status === 200 && st.data.ok === true && st.data.serving === 'javascript-fallback' && typeof st.data.version === 'string' && st.data.version !== '';
    const page = await fetch(H + '/').then(async (r) => ({ status: r.status, type: r.headers.get('content-type') || '', len: (await r.text()).length })).catch(() => ({ status: 0, type: '', len: 0 }));
    out.steps.page = page.status === 200 && /html/.test(page.type) && page.len > 1000;
    const gate = await get('/api/room/messages?rev=0');
    const li = await post('/api/login', { username: 'T-Admin', password: 'pw-t-admin-0001', deviceId: 'probe-device-0001' });
    const T = li.data.token;
    out.steps.signin = gate.status === 401 && li.status === 200 && !!T;
    const room = await get('/api/room/messages?rev=0', T);
    out.steps.room = room.status === 200;
    const board = await get('/api/board', T);
    out.steps.board = board.status === 200;
    // Work with Claude: one run, the fake Claude answers
    let events = [];
    try {
      const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + T }, body: JSON.stringify({ message: 'hello from the fallback probe', fresh: true }) });
      if (r.status === 200) { const txt = await r.text(); events = txt.split('\n').filter((l) => l.trim()).map((l) => { try { return JSON.parse(l); } catch { return {}; } }); }
      out.chatStatus = r.status;
    } catch (e) { out.chatStatus = 0; }
    out.steps.work = out.chatStatus === 200 && events.some((e) => e.type === 'system') && events.some((e) => e.type === 'assistant');
    // T-310 (linked machines 9, R338): this hub keeps the browser PC-folder tools exactly as they are (the page answers the run's
    // pc_tool through /api/pc-result) and has no machine features: the machine routes are not here ("not available on this hub")
    let pcText = '';
    try {
      const r = await fetch(H + '/api/chat', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + T }, body: JSON.stringify({ message: 'PCTOOL fallback probe', fresh: true }) });
      if (r.status === 200) {
        const reader = r.body.getReader(), dec = new TextDecoder(); let buf = '';
        for (;;) {
          const { value, done } = await reader.read(); if (done) break;
          buf += dec.decode(value, { stream: true }); let i;
          while ((i = buf.indexOf('\n')) >= 0) {
            const line = buf.slice(0, i); buf = buf.slice(i + 1); if (!line.trim()) continue;
            let ev = {}; try { ev = JSON.parse(line); } catch { /* not json */ }
            if (ev.type === 'pc_tool') post('/api/pc-result', { id: ev.id, ok: true, result: 'fake-listing: a.txt' }, { Authorization: 'Bearer ' + T });
            if (ev.type === 'assistant') pcText += JSON.stringify(ev.message);
          }
        }
      }
    } catch (e) { /* the step fails below */ }
    out.steps.pcBrowser = /fake-listing: a\.txt/.test(pcText); out.pcText = pcText.slice(0, 300);
    const mach = await get('/api/machines', T), machPoll = await post('/api/machine/poll', { wait: 0 }, { Authorization: 'Bearer ' + T });
    out.steps.noMachines = mach.status !== 200 && !mach.data.machines && machPoll.status !== 200 && !machPoll.data.owner;
    const hello = await post('/node/hello', { nonce: 'a'.repeat(32) }, { 'X-VR-Host': node, 'X-VR-Host-Token': secret });
    const helloBad = await post('/node/hello', { nonce: 'a'.repeat(32) }, { 'X-VR-Host': node, 'X-VR-Host-Token': 'wrong-secret-0123456789abcdef' });
    out.steps.host = hello.status === 200 && !!hello.data.serverNonce && helloBad.status === 401;
    out.helloMachine = hello.data.machine; // the frozen hub never announces `machine` (an app stays an AI host against it)
    out.steps.hostStaysAiHost = hello.data.machine === undefined;
    out.ok = Object.values(out.steps).every(Boolean);
    out.why = Object.entries(out.steps).map(([k, v]) => `${k} ${v ? 'ok' : 'FAILED'}`).join(' · ') + (out.ok ? '' : ` · log tail: ${(fs.existsSync(logFile) ? fs.readFileSync(logFile, 'utf8') : '').trim().split('\n').slice(-3).join(' | ').slice(0, 300)}`);
  } catch (e) {
    out.why = 'probe error: ' + e.message;
  } finally {
    if (proc) { try { proc.kill('SIGTERM'); } catch { /* gone */ } await sleep(300); try { proc.kill('SIGKILL'); } catch { /* gone */ } }
    try { fs.rmSync(root, { recursive: true, force: true }); } catch { /* temp */ }
  }
  return out;
}

module.exports = { probe };
if (require.main === module) probe().then((r) => { console.log(r.ok ? 'JavaScript hub alone: OK' : 'JavaScript hub alone: FAILED', '-', r.why); process.exit(r.ok ? 0 : 1); });
