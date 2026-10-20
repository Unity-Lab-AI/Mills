#!/usr/bin/env node
// Vibe Roller AI Host: installer, launcher and updater in one (Windows and Linux). The downloaded package runs this
// through "VibeRoller AI Host.cmd" (Windows) or "viberoller-ai-host.sh" (Linux):
//
//   (no option)    run from an installed copy → the control app (bridge/vr-host.js: status, start / stop, settings, start
//                  at boot). Run from a downloaded package → look for an earlier install; found → update it to this
//                  package and start it; none → ask: install (then it runs in the background and the control app
//                  opens), or run portable here.
//   --install      install (or update) into the standard folder and start it there
//   --portable     run from this folder as it is, installing nothing
//   --update       update the install that was found to this package (nothing else)
//   --status       say what is installed where, its version, and whether it runs
//   --yes          no questions (install when nothing is found)
//
// Standard folder: %LOCALAPPDATA%\VibeRoller\AIHost (Windows), ~/.local/share/viberoller/ai-host (Linux). Earlier installs
// are found there, at the VibeRollerAIHost service's folder, in the places the older README suggested
// (C:\VibeRollerAIHost, ~/VibeRollerAIHost), and through a running host's lock. An install inside a git checkout (a hub
// folder handed off to hosting, like Gee's PC) is never overwritten: it updates itself from the hub (or git pull).
// An update copies the code and the tools; this machine's own files (ai-node.json, logs, the pause switch, update
// state) are never touched. A running host picks up new code at its next quiet moment (it restarts itself on it); a
// launcher that would not start it again is covered: the setup starts it again itself.
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn, spawnSync, execFileSync } = require('child_process');

const HERE = __dirname;
const WIN = process.platform === 'win32';
const args = process.argv.slice(2), has = (f) => args.includes(f);
const say = (...a) => console.log('[VibeRoller AI Host]', ...a);
const STD = WIN ? path.join(process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local'), 'VibeRoller', 'AIHost') : path.join(os.homedir(), '.local', 'share', 'viberoller', 'ai-host');
// this machine's own files: never copied over, never deleted
const OWN = /^(ai-node\.json|ai-node\.log.*|ai-node\.lock|ai-node\.status\.json|ai-host\.off|\.update-.*|\.stop-now|\.restart-now|\.vr-installed|Api|host-tools[\\/].*\.(exe|ico))$/i;
// the heart & knife control app of an install (bridge/vr-host.js): what an installed copy opens
const control = (dir) => spawnSync(process.execPath, [path.join(dir, 'vr-host.js')], { cwd: dir, stdio: 'inherit' }).status || 0;
const same = (a, b) => path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

const versionOf = (dir) => { try { return fs.readFileSync(path.join(dir, 'VERSION.txt'), 'utf8').trim(); } catch { return fs.existsSync(path.join(dir, 'ai-node.js')) ? '(older, no VERSION.txt)' : ''; } };
const isHost = (dir) => !!dir && fs.existsSync(path.join(dir, 'ai-node.js'));
const inGit = (dir) => { for (let d = path.resolve(dir), i = 0; i < 4; i++, d = path.dirname(d)) if (fs.existsSync(path.join(d, '.git'))) return true; return false; };
const running = (dir) => { try { const pid = Number(fs.readFileSync(path.join(dir, 'ai-node.lock'), 'utf8').trim()); if (!pid) return 0; process.kill(pid, 0); return pid; } catch { return 0; } };

// ---------- where are earlier installs? ----------
function serviceDir() {
  try {
    if (WIN) { // BINARY_PATH_NAME : "C:\…\host-tools\VibeRollerAIHost.exe"
      const out = execFileSync('sc', ['qc', 'VibeRollerAIHost'], { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
      const m = /BINARY_PATH_NAME\s*:\s*"?([^"\r\n]+?\.exe)"?/i.exec(out); if (!m) return '';
      const d = path.dirname(m[1]); return path.basename(d).toLowerCase() === 'host-tools' ? path.dirname(d) : d;
    }
    for (const f of ['/etc/systemd/system/viberoller-ai-host.service', path.join(os.homedir(), '.config', 'systemd', 'user', 'viberoller-ai-host.service')]) {
      const m = /^WorkingDirectory=(.+)$/m.exec(fs.readFileSync(f, 'utf8')); if (m) return m[1].trim();
    }
  } catch {}
  return '';
}
function findInstalls() {
  const c = [STD, serviceDir(), WIN ? 'C:\\VibeRollerAIHost' : '', path.join(os.homedir(), 'VibeRollerAIHost'), WIN ? path.join(os.homedir(), 'Desktop', 'VibeRollerAIHost') : ''];
  const out = [];
  for (const d of c) if (d && isHost(d) && !same(d, HERE) && !out.some((x) => same(x, d))) out.push(path.resolve(d));
  return out.map((dir) => ({ dir, version: versionOf(dir), git: inGit(dir), running: running(dir), service: same(dir, serviceDir() || '//none') }));
}

// ---------- copy this package over an install, keeping the machine's own files ----------
function files(dir, rel = '') {
  const out = [];
  for (const e of fs.readdirSync(path.join(dir, rel), { withFileTypes: true })) {
    const r = path.join(rel, e.name);
    if (OWN.test(r)) continue;
    if (e.isDirectory()) out.push(...files(dir, r)); else out.push(r);
  }
  return out;
}
function copyInto(to) {
  fs.mkdirSync(to, { recursive: true });
  const busy = running(to), list = files(HERE);
  // a launcher that is running right now is read by its shell as it goes: replace it only when nothing runs
  const launcher = /^(start-node\.(bat|sh)|VibeRoller AI Host\.cmd|viberoller-ai-host\.sh)$/i;
  let n = 0, held = [];
  for (const r of list) {
    if (busy && launcher.test(r)) { fs.copyFileSync(path.join(HERE, r), path.join(to, r + '.new')); held.push(r); continue; }
    fs.mkdirSync(path.dirname(path.join(to, r)), { recursive: true });
    fs.copyFileSync(path.join(HERE, r), path.join(to, r));
    if (!WIN && /\.sh$/.test(r)) fs.chmodSync(path.join(to, r), 0o755);
    n++;
  }
  // a personal package (ai-node.json inside) gives a new install its config; an existing config always stays
  if (fs.existsSync(path.join(HERE, 'ai-node.json')) && !fs.existsSync(path.join(to, 'ai-node.json'))) fs.copyFileSync(path.join(HERE, 'ai-node.json'), path.join(to, 'ai-node.json'));
  fs.writeFileSync(path.join(to, '.vr-installed'), new Date().toISOString() + '\n');
  return { n, held };
}
// launchers held back while the host ran: swapped in now that it does not (called at every start of an installed copy)
function swapHeld(dir) { for (const f of fs.readdirSync(dir).filter((x) => x.endsWith('.new'))) { try { fs.renameSync(path.join(dir, f), path.join(dir, f.slice(0, -4))); } catch {} } }

// ---------- start the host ----------
function start(dir, { detached = false } = {}) {
  if (!fs.existsSync(path.join(dir, 'ai-node.json'))) {
    say(`No ai-node.json in ${dir} yet. Copy ai-node.example.json to ai-node.json there and fill in hub, id and token`);
    say('(or download your own host\'s package: VibeRoller → Settings → Connections → My AI hosts), then run this again.');
    return 1;
  }
  if (running(dir)) { say(`The host in ${dir} is already running (pid ${running(dir)}).`); return 0; }
  swapHeld(dir);
  const launcher = path.join(dir, WIN ? 'start-node.bat' : 'start-node.sh');
  if (detached) {
    const p = WIN ? spawn('cmd.exe', ['/c', 'start', '"VibeRoller AI node"', '/min', launcher], { cwd: dir, detached: true, stdio: 'ignore', windowsHide: true })
      : spawn('sh', [launcher], { cwd: dir, detached: true, stdio: 'ignore' });
    p.unref(); say(`Started the host in ${dir}.`); return 0;
  }
  say(`Starting the host in ${dir} (close this window or press Ctrl+C to stop it).`);
  return spawnSync(WIN ? 'cmd.exe' : 'sh', WIN ? ['/c', launcher] : [launcher], { cwd: dir, stdio: 'inherit' }).status || 0;
}
// after an update the running host restarts itself on the new code once nothing is in flight; if its launcher does not
// start it again (an older start-node.bat closes on that), start it here
async function followRestart(dir, pid) {
  say('The running host picks up the new code at its next quiet moment (it never cuts off an answer).');
  if (!pid) return;
  for (let i = 0; i < 120; i++) { // up to 10 minutes
    await new Promise((r) => setTimeout(r, 5000));
    const now = running(dir);
    if (now && now !== pid) { say(`The host restarted on the new version (pid ${now}).`); return; }
    if (!now) { await new Promise((r) => setTimeout(r, 15000)); if (!running(dir)) { say('Its launcher did not start it again: starting it now.'); start(dir, { detached: true }); } return; }
  }
  say('The host is still busy; it switches to the new version by itself when it is quiet.');
}

function ask(q, def) {
  if (has('--yes') || !process.stdin.isTTY) return def;
  process.stdout.write(q);
  const buf = Buffer.alloc(64); let n = 0;
  try { n = fs.readSync(0, buf, 0, 64, null); } catch { return def; }
  return buf.toString('utf8', 0, n).trim().toLowerCase() || def;
}

async function main() {
  const mine = versionOf(HERE), installs = findInstalls();
  const thisIsInstall = fs.existsSync(path.join(HERE, '.vr-installed')) || same(HERE, STD);
  if (has('--status')) {
    say(`This package: ${HERE} (${mine || 'unknown version'})${thisIsInstall ? ', an installed copy' : ''}${running(HERE) ? `, running (pid ${running(HERE)})` : ''}`);
    if (!installs.length) say('No other install found.');
    for (const i of installs) say(`Install: ${i.dir} · ${i.version || '?'}${i.git ? ' · git checkout (updates itself)' : ''}${i.service ? ' · the service runs it' : ''}${i.running ? ` · running (pid ${i.running})` : ' · not running'}`);
    return 0;
  }
  if (has('--portable')) return start(HERE);
  // an installed copy: the control app (start / stop / restart / settings / start at boot …), the host itself runs in the background
  if (thisIsInstall && !has('--install') && !has('--update')) return fs.existsSync(path.join(HERE, 'vr-host.js')) && process.stdin.isTTY ? control(HERE) : start(HERE);
  // an earlier install: update it (a git checkout is left to update itself)
  const target = installs.find((i) => !i.git) || null, gitOne = installs.find((i) => i.git);
  if (!target && gitOne && !has('--install')) {
    say(`This machine already runs a host from a git checkout: ${gitOne.dir} (${gitOne.version}). It updates itself from the hub;`);
    say('nothing to install. (--install puts a separate copy in the standard folder anyway; --portable runs this one here.)');
    return 0;
  }
  if (target && !has('--install')) {
    if (target.version === mine && !has('--update')) { say(`${target.dir} is already this version (${mine}).`); return target.running ? 0 : start(target.dir); }
    say(`Found an earlier install: ${target.dir} (${target.version}) → updating it to ${mine}.`);
    const r = copyInto(target.dir);
    say(`Updated ${r.n} files; its ai-node.json, logs and settings are kept.${r.held.length ? ' (Its launcher is swapped in at its next start.)' : ''}`);
    if (target.running) { await followRestart(target.dir, target.running); return 0; }
    return has('--update') ? 0 : start(target.dir, { detached: target.service });
  }
  if (has('--update')) { say('No earlier install found to update. Run without options (or with --install) to install.'); return 1; }
  const choice = has('--install') ? 'i' : ask(`Install the VibeRoller AI Host into ${STD} (I), or run it portable from this folder (P)? [I/p] `, 'i');
  if (choice.startsWith('p')) return start(HERE);
  const r = copyInto(STD);
  say(`Installed into ${STD} (${r.n} files, ${mine}).`);
  // in the background from here on; the control app shows it, changes every setting and sets up start at boot ([b])
  const code = start(STD, { detached: true });
  if (code || !process.stdin.isTTY || !fs.existsSync(path.join(STD, 'vr-host.js'))) {
    say(`Open the control app any time: node "${path.join(STD, 'vr-host.js')}" (settings, start / stop, start at boot).`);
    return code;
  }
  say('Opening the control app: [b] makes it start at boot, [c] has every setting.');
  return control(STD);
}
main().then((code) => process.exit(code || 0), (e) => { say('Setup failed:', e.message); process.exit(1); });
