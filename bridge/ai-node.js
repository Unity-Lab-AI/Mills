#!/usr/bin/env node
// VibeRoller AI node: lends this machine's local AI engines (Unity's Ollama brain, image, voice and speech engines)
// to a VibeRoller hub that runs somewhere else (bridge/ai-nodes.js on the hub). It connects OUT to the hub, so no
// port opens here. Run:  node bridge/ai-node.js            (config: bridge/ai-node.json, gitignored)
//
// bridge/ai-node.json:
//   { "hub": "https://viberoller.git.unityailab.com", "name": "gee-pc", "token": "<the same secret as the hub's
//     config aiNodes.gee-pc>", "engines": { "ollama": "http://127.0.0.1:11434", "image": "http://127.0.0.1:5124",
//     "voice": "http://127.0.0.1:5123", "speech": "http://127.0.0.1:5125" } }
// Only the engines listed there can ever be reached: the hub sends an engine NAME and a path, never an address.
// The knife (stop-viberoller-host, bridge/ai-host.off next to this file) pauses the node: it stops polling, so the
// hub sees it offline; the heart removes the file and it carries on by itself.
'use strict';

const fs = require('fs');
const path = require('path');

const CFG_FILE = process.argv.includes('--config') ? process.argv[process.argv.indexOf('--config') + 1] : path.join(__dirname, 'ai-node.json');
const PAUSE_FILE = path.join(__dirname, 'ai-host.off');
const cfg = (() => { try { return JSON.parse(fs.readFileSync(CFG_FILE, 'utf8')); } catch (e) { console.error(`[ai-node] Cannot read ${CFG_FILE}: ${e.message}`); process.exit(1); } })();
const HUB = String(cfg.hub || '').replace(/\/+$/, '');
const ENGINES = cfg.engines && typeof cfg.engines === 'object' ? cfg.engines : {};
if (!/^https?:\/\//.test(HUB) || !cfg.name || !cfg.token) { console.error('[ai-node] ai-node.json needs hub, name and token.'); process.exit(1); }
const headers = { 'X-VR-Node': String(cfg.name), 'X-VR-Node-Token': String(cfg.token) };
// one node per machine: a lock file with the running pid (exit code 2 = already running, like the hub's run.js)
const LOCK_FILE = path.join(__dirname, 'ai-node.lock');
(function singleInstance() {
  try {
    const pid = Number(fs.readFileSync(LOCK_FILE, 'utf8').trim());
    if (pid && pid !== process.pid) { process.kill(pid, 0); console.log(`[ai-node] already running (pid ${pid}); nothing to do.`); process.exit(2); }
  } catch {} // no lock, unreadable, or that pid is gone
  fs.writeFileSync(LOCK_FILE, String(process.pid));
  const bye = () => { try { if (Number(fs.readFileSync(LOCK_FILE, 'utf8').trim()) === process.pid) fs.unlinkSync(LOCK_FILE); } catch {} };
  process.on('exit', bye); for (const s of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(s, () => process.exit(0));
})();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), '[ai-node]', ...a);

async function handle(job) {
  let reply;
  try {
    const base = ENGINES[job.engine];
    if (!base || !/^\/[^\s]*$/.test(String(job.path || ''))) throw Object.assign(new Error('This node does not lend the ' + job.engine + ' engine.'), { status: 404 });
    const r = await fetch(String(base).replace(/\/+$/, '') + job.path, {
      method: job.method || 'GET', headers: job.headers && job.headers['content-type'] ? { 'content-type': job.headers['content-type'] } : {},
      body: job.body ? Buffer.from(job.body, 'base64') : undefined, signal: AbortSignal.timeout(290000),
    });
    reply = { id: job.id, status: r.status, headers: { 'content-type': r.headers.get('content-type') || '' }, body: Buffer.from(await r.arrayBuffer()).toString('base64') };
  } catch (e) {
    reply = { id: job.id, status: e.status || 502, headers: { 'content-type': 'application/json' }, body: Buffer.from(JSON.stringify({ error: e.message })).toString('base64') };
  }
  try { await fetch(HUB + '/node/reply', { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify(reply), signal: AbortSignal.timeout(60000) }); }
  catch (e) { log('could not hand back a job:', e.message); }
}

(async function loop() {
  log(`lending ${Object.keys(ENGINES).join(', ') || 'no engines'} to ${HUB} as "${cfg.name}"`);
  let paused = false, quiet = 0;
  for (;;) {
    if (fs.existsSync(PAUSE_FILE)) { if (!paused) { paused = true; log('paused by this machine\'s host (ai-host.off)'); } await sleep(3000); continue; }
    if (paused) { paused = false; log('back on'); }
    try {
      const r = await fetch(HUB + '/node/poll', { headers, signal: AbortSignal.timeout(40000) });
      if (r.status === 401) { log('the hub does not know this node (name / token): check ai-node.json and the hub\'s aiNodes'); await sleep(30000); continue; }
      if (!r.ok) throw new Error('hub answered ' + r.status);
      const { jobs = [] } = await r.json();
      quiet = 0;
      for (const job of jobs) handle(job); // in parallel; the next poll starts right away
    } catch (e) { if (quiet++ % 20 === 0) log('hub not reachable:', e.message); await sleep(3000); }
  }
})();
