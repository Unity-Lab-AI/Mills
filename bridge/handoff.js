#!/usr/bin/env node
// VibeRoller hand-off: the hub on this machine moves to the server, and this machine becomes an AI host instead. Made so
// a Work run can do the whole move for the host's owner (nothing to type for Gee): the running hub packs its own data,
// carries it to the server over ssh, restores it there, and then hands off.
//
//   node bridge/handoff.js pack [--to <ssh alias>] [--dir /srv/apps/VibeRoller]
//       Pauses writes on this hub (bridge/.maintenance: new sign-ins, chat, board, notes answer "moved to the server";
//       the Work run doing the move keeps going), packs exactly the hub's data (accounts with their password hashes and
//       device locks, chat + keys, board, notes, pictures, saved sessions, settings: the same list as deploy/pack-data)
//       into viberoller-data-<stamp>.tar.gz. With --to it copies the archive to <alias>:<dir>/ over ssh and runs
//       deploy/restore-data.sh there, which refuses to overwrite accounts the server already has. Nothing is reset.
//   node bridge/handoff.js switch [--dry-run]
//       Hands off: writes bridge/.handoff and asks the hub for a graceful restart; from then on bridge/run.js (start.bat,
//       the autostart launcher, the service) runs the AI host (bridge/ai-node.js, config bridge/ai-node.json) here
//       instead of the hub. The hub lives on the server. Run it LAST: the Work run that calls it ends with the hub.
//   node bridge/handoff.js tools        Windows: build the heart / knife / service programs, Desktop shortcuts (no admin needed)
//   node bridge/handoff.js service      Windows: start the boot-service installer (the person at the PC clicks Yes once)
//   node bridge/handoff.js probe        one tiny real call through each login this host shares (no token printed)
//   node bridge/handoff.js restart-host restart the AI host here on its current code + config
//   node bridge/handoff.js undo         remove .handoff and .maintenance: the hub serves here again on its next start
//   node bridge/handoff.js status
// Every flag lands in the RUNNING hub's bridge folder (the host folder's), never in a worktree's copy.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');
const { spawnSync, spawn } = require('child_process');
const { REPO } = require('./worktree');

const BRIDGE = path.join(REPO, 'bridge');
const MAINT = path.join(BRIDGE, '.maintenance'), HANDOFF = path.join(BRIDGE, '.handoff'), RESTART = path.join(BRIDGE, '.restart');
// (front-sessions.json / device-locks.json: the Rust hub's sign-in module keeps them, behind config rustHub; work.json
// and work-current.json: the default account's Work state + conversation, next to its state.json)
const DATA = ['bridge/auth.json', 'bridge/state.json', 'bridge/work.json', 'bridge/work-current.json', 'bridge/hub-state.json', 'bridge/front-sessions.json', 'bridge/device-locks.json', 'bridge/usage.json', 'bridge/assistant.json', 'bridge/providers.json', 'bridge/forgejo.json', 'bridge/forgejo-users.json', 'bridge/server-owners.json', 'bridge/ai-hosts.json', 'bridge/profiles', 'bridge/profiles-deleted', 'Chat', 'Archive', 'Board', 'Notes', 'Avatars', 'Images', 'Api'];
const SKIP = /\.(tmp|log)$/i;

// ---------- a tar writer (ustar), gzipped: the shape deploy/restore-data.sh and plain tar read ----------
function header(name, size, mode, type, mtime) {
  const h = Buffer.alloc(512);
  let base = name, prefix = '';
  if (Buffer.byteLength(name) > 100) { const i = name.lastIndexOf('/', 154); if (i > 0 && Buffer.byteLength(name.slice(i + 1)) <= 100) { prefix = name.slice(0, i); base = name.slice(i + 1); } else throw new Error('path too long for tar: ' + name); }
  h.write(base, 0, 100, 'utf8');
  h.write(mode.toString(8).padStart(7, '0') + '\0', 100, 8, 'ascii');
  h.write('0000000\0', 108, 8, 'ascii'); h.write('0000000\0', 116, 8, 'ascii');
  h.write(size.toString(8).padStart(11, '0') + '\0', 124, 12, 'ascii');
  h.write(Math.floor(mtime / 1000).toString(8).padStart(11, '0') + '\0', 136, 12, 'ascii');
  h.write('        ', 148, 8, 'ascii'); // checksum placeholder: spaces while summing
  h.write(type, 156, 1, 'ascii');
  h.write('ustar\0', 257, 6, 'ascii'); h.write('00', 263, 2, 'ascii');
  h.write(prefix, 345, 155, 'utf8');
  let sum = 0; for (const b of h) sum += b;
  h.write(sum.toString(8).padStart(6, '0') + '\0 ', 148, 8, 'ascii');
  return h;
}
function collect() { // every file under the data list, as repo-relative forward-slash paths
  const out = [];
  const walk = (rel) => {
    const abs = path.join(REPO, rel); if (!fs.existsSync(abs)) return;
    const st = fs.statSync(abs);
    if (st.isDirectory()) { out.push({ rel: rel + '/', dir: true, mtime: st.mtimeMs }); for (const e of fs.readdirSync(abs).sort()) walk(rel + '/' + e); }
    else if (!SKIP.test(rel)) out.push({ rel, dir: false, size: st.size, mtime: st.mtimeMs });
  };
  for (const d of DATA) walk(d);
  return out;
}
function buildTar(entries) {
  const parts = [];
  for (const e of entries) {
    if (e.dir) { parts.push(header(e.rel, 0, 0o750, '5', e.mtime)); continue; }
    const data = fs.readFileSync(path.join(REPO, e.rel));
    parts.push(header(e.rel, data.length, 0o640, '0', e.mtime), data);
    const pad = (512 - (data.length % 512)) % 512; if (pad) parts.push(Buffer.alloc(pad));
  }
  parts.push(Buffer.alloc(1024));
  return Buffer.concat(parts);
}

function pack(opts) {
  const target = opts.to ? `https://portal.unityailab.com` : '';
  fs.writeFileSync(MAINT, (target || 'the server') + '\n'); // writes pause here from now on
  const stamp = new Date().toISOString().replace(/[-:]/g, '').slice(0, 13).replace('T', '-');
  const name = `viberoller-data-${stamp}.tar.gz`, out = path.join(os.tmpdir(), name);
  const entries = collect(), files = entries.filter((e) => !e.dir);
  if (!files.length) { fs.unlinkSync(MAINT); throw new Error(`Nothing to pack: no data under ${REPO}.`); }
  fs.writeFileSync(out, zlib.gzipSync(buildTar(entries), { level: 6 }));
  const accounts = files.filter((e) => /(^|\/)auth\.json$/.test(e.rel)).length;
  const lines = [`Packed ${files.length} files (${accounts} account password files, Chat/agent-key.json ${files.some((e) => e.rel === 'Chat/agent-key.json') ? 'present' : 'MISSING'}) from ${REPO}`, `→ ${out} (${(fs.statSync(out).size / 1024).toFixed(0)} KB)`, `This hub now answers "moved to the server" to new sign-ins, chat, board and notes (bridge/.maintenance); "undo" lifts that.`];
  if (!opts.to) return lines.concat(`Copy the archive to the server, then there: sh /srv/apps/VibeRoller/app/deploy/restore-data.sh <it>`);
  const dir = String(opts.dir || '/srv/apps/VibeRoller').replace(/\/+$/, ''), remote = `${dir}/${name}`;
  const cp = spawnSync('ssh', [opts.to, `cat > '${remote}'`], { input: fs.readFileSync(out), encoding: 'utf8', windowsHide: true, maxBuffer: 64 * 1024 * 1024 });
  if (cp.status !== 0) { fs.unlinkSync(MAINT); throw new Error(`Could not copy the archive to ${opts.to}:${remote} (${(cp.stderr || '').trim() || 'ssh failed'}). Nothing changed on the server; writes here are back on.`); }
  lines.push(`Copied to ${opts.to}:${remote}.`);
  const rs = spawnSync('ssh', [opts.to, `sh '${dir}/app/deploy/restore-data.sh' '${remote}'`], { encoding: 'utf8', windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
  lines.push('--- restore on the server ---', (rs.stdout || '').trim(), (rs.stderr || '').trim());
  if (rs.status !== 0) { fs.unlinkSync(MAINT); throw new Error(lines.join('\n') + `\nThe restore failed (code ${rs.status}). Nothing was handed off; writes here are back on.`); }
  lines.push(`The server has the data. Sign-ins now belong to ${target}. Next, as the LAST step of this run: node bridge/handoff.js switch`);
  return lines;
}
function doSwitch(dry) {
  if (!fs.existsSync(path.join(BRIDGE, 'ai-node.json'))) throw new Error(`${path.join(BRIDGE, 'ai-node.json')} is missing: this machine needs its AI host config before it can hand off (Settings → My AI hosts → Download, or bridge/ai-node.example.json).`);
  const lines = [`Hand-off: ${REPO} stops serving the hub and runs the AI host (bridge/ai-node.js) from its next start, at every start (start.bat, the autostart launcher, the service).`];
  if (dry) return lines.concat('(dry run: nothing written)');
  fs.writeFileSync(HANDOFF, new Date().toISOString() + '\n');
  fs.writeFileSync(RESTART, new Date().toISOString() + '\n');
  return lines.concat(`Written ${HANDOFF} and asked the hub for a graceful restart: as soon as no reply is in progress it restarts as the AI host. This Work run ends with it; carry on at the server's address.`);
}
// The heart and the knife (and the service program) for this machine: builds them with bridge/host-tools/build-host-tools.ps1
// (Windows' own C# compiler) and puts "Start / Stop VibeRoller AI Host" on the Desktop. Installing the boot service needs
// administrator rights, so that stays a separate, optional double-click (install-ai-host-service.cmd); without it the
// Startup launcher starts the AI host at sign-in.
function tools() {
  if (process.platform !== 'win32') return ['On Linux the heart and knife are host-tools/start-viberoller-host.sh / stop-viberoller-host.sh (nothing to build).'];
  const ps1 = path.join(BRIDGE, 'host-tools', 'build-host-tools.ps1');
  if (!fs.existsSync(ps1)) throw new Error(ps1 + ' is missing.');
  const r = spawnSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ps1], { encoding: 'utf8', windowsHide: true });
  if (r.status !== 0) throw new Error('Building the host tools failed:\n' + (r.stdout || '') + (r.stderr || ''));
  const exes = fs.readdirSync(path.join(BRIDGE, 'host-tools')).filter((f) => f.endsWith('.exe'));
  return [(r.stdout || '').trim(), `Built: ${exes.join(', ')}`];
}
// Restart this machine's AI host on its current code + config (it is started again at once by run.js or the service).
// Newer hosts do this by themselves when their files change; this is for one that predates that.
function restartHost() {
  const lock = path.join(BRIDGE, 'ai-node.lock');
  if (!fs.existsSync(lock)) return ['No AI host is running here (no bridge/ai-node.lock).'];
  const pid = Number(fs.readFileSync(lock, 'utf8').trim());
  try { process.kill(pid); } catch (e) { return [`Could not stop the AI host (pid ${pid}): ${e.message}`]; }
  return [`Stopped the AI host (pid ${pid}); run.js / the service starts it again on the new code within a few seconds.`];
}
// Install the VibeRollerAIHost service (start at boot). Windows asks the person at this PC for administrator rights once.
function service() {
  if (process.platform !== 'win32') return ['On Linux: sudo sh bridge/host-tools/install-ai-host-service.sh (needs sudo).'];
  const cmd = path.join(BRIDGE, 'host-tools', 'install-ai-host-service.cmd');
  if (!fs.existsSync(path.join(BRIDGE, 'host-tools', 'VibeRollerAIHost.exe'))) throw new Error('Build the host tools first: node bridge/handoff.js tools');
  spawn('cmd.exe', ['/c', 'start', '""', cmd], { cwd: path.dirname(cmd), detached: true, stdio: 'ignore', windowsHide: false }).unref();
  return ['Started the service installer: Windows now shows an administrator prompt on this PC. Once Yes is clicked, the AI host runs as a service from every boot (the heart / knife start and stop it).'];
}
// Does this machine's shared AI answer? One tiny real call per login relay in bridge/ai-node.json (no token printed).
async function probe() {
  const cfgFile = path.join(BRIDGE, 'ai-node.json');
  const cfg = JSON.parse(fs.readFileSync(cfgFile, 'utf8'));
  const relays = require('./host-relay');
  const lines = [];
  for (const p of (cfg.providers || []).filter((x) => relays.isRelay(x))) {
    try { const r = await relays.probe(p, { sandbox: path.join(REPO, 'Api', 'sandbox'), log: (m) => lines.push('  ' + m) }); lines.push(`${p.id} (${r.kind}, ${r.model}): HTTP ${r.status}${r.said ? ', said "' + r.said + '"' : ''}${r.error ? ', ' + r.error : ''}${r.usage ? ', usage ' + JSON.stringify(r.usage) : ''}`); }
    catch (e) { lines.push(`${p.id}: ${e.message}`); }
  }
  return lines.length ? lines : ['No login relays in ' + cfgFile];
}
function undo() { let n = 0; for (const f of [HANDOFF, MAINT]) if (fs.existsSync(f)) { fs.unlinkSync(f); n++; } return [n ? `Removed ${n} flag(s): the hub serves here again from its next start (node bridge/restart.js, or start.bat).` : 'Nothing to undo.']; }
function status() { return [`host folder ${REPO}`, `handed off: ${fs.existsSync(HANDOFF)}`, `writes paused (maintenance): ${fs.existsSync(MAINT)}`, `AI host config: ${fs.existsSync(path.join(BRIDGE, 'ai-node.json'))}`]; }

module.exports = { collect, buildTar, DATA };
if (require.main === module) {
  const argv = process.argv.slice(2);
  const take = (f) => { const i = argv.indexOf(f); return i >= 0 ? argv.splice(i, 2)[1] || '' : ''; };
  const to = take('--to'), dir = take('--dir'), dry = argv.includes('--dry-run');
  const cmd = argv.find((a) => !a.startsWith('--'));
  try {
    if (cmd === 'pack') console.log(pack({ to, dir }).join('\n'));
    else if (cmd === 'switch') console.log(doSwitch(dry).join('\n'));
    else if (cmd === 'undo') console.log(undo().join('\n'));
    else if (cmd === 'tools') console.log(tools().join('\n'));
    else if (cmd === 'restart-host') console.log(restartHost().join('\n'));
    else if (cmd === 'service') console.log(service().join('\n'));
    else if (cmd === 'probe') probe().then((l) => console.log(l.join('\n'))).catch((e) => { console.error(e.message); process.exit(1); });
    else if (cmd === 'status') console.log(status().join('\n'));
    else { console.error('Usage: node bridge/handoff.js pack [--to <ssh alias>] [--dir <server folder>] | switch [--dry-run] | undo | status'); process.exit(1); }
  } catch (e) { console.error(e.message); process.exit(1); }
}
