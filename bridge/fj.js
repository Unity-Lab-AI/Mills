// `fj` (the Forgejo CLI, forgejo-cli) in a Work run, signed in as the person whose run it is.
// fj keeps its logins in one keys file per operating-system account (Linux: $XDG_DATA_HOME/forgejo-cli/keys.json,
// Windows: %APPDATA%\forgejo-cli\forgejo-cli\data\keys.json), so on a shared machine everyone's runs would be one
// login. A run is one person's, so it gets its own: a folder of theirs holding that file (their token from Settings →
// My Forgejo, owner-only) and a tiny `fj` wrapper first on the PATH that points fj at it (XDG_DATA_HOME / APPDATA set
// for the fj process alone; nothing else in the run sees them). Without fj on the machine, or without a token, nothing
// is made and fj stays whatever the machine has. The hub does this for runs here (bridge/server.js), an AI host for
// the runs it takes (bridge/ai-node.js); the Rust hub and host do the same (pgm-ai-internal fj.rs).
'use strict';
const fs = require('fs');
const path = require('path');

const WRAP = 'fj-bin'; // the wrapper's folder name, so the real fj is never one of our wrappers
const WIN = process.platform === 'win32';
const pathKey = (env) => Object.keys(env).find((k) => k.toUpperCase() === 'PATH') || 'PATH';
// The real fj on the PATH, or ''.
function findFj(env = process.env) {
  const names = WIN ? ['fj.exe', 'fj.cmd', 'fj.bat', 'fj'] : ['fj'];
  for (const d of String(env[pathKey(env)] || '').split(path.delimiter).filter(Boolean)) {
    if (path.basename(d) === WRAP) continue;
    for (const n of names) { const p = path.join(d, n); try { if (fs.statSync(p).isFile()) return p; } catch { /* not there */ } }
  }
  return '';
}
const hostOf = (url) => { try { return new URL(String(url || '')).host; } catch { return ''; } };
const sq = (s) => `'${String(s).replace(/'/g, "'\\''")}'`; // a sh single-quoted word

// Writes the person's keys file + wrapper under dir and returns the environment their run gets (the PATH with the
// wrapper first); {} when nothing applies. Never throws: a run must start whatever happens here.
function env(dir, { url, token, login } = {}, base = process.env) {
  try {
    const host = hostOf(url);
    if (!token || !host) return {};
    const real = findFj(base);
    if (!real) return {};
    const data = WIN ? path.join(dir, 'forgejo-cli', 'forgejo-cli', 'data') : path.join(dir, 'forgejo-cli');
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 }); fs.chmodSync(dir, 0o700);
    fs.mkdirSync(data, { recursive: true });
    const keys = path.join(data, 'keys.json');
    let cur = {}; try { cur = JSON.parse(fs.readFileSync(keys, 'utf8')) || {}; } catch { /* first time */ }
    const next = { hosts: { ...(cur.hosts && typeof cur.hosts === 'object' ? cur.hosts : {}), [host]: { type: 'Application', token } }, aliases: cur.aliases && typeof cur.aliases === 'object' ? cur.aliases : {}, default_ssh: Array.isArray(cur.default_ssh) ? cur.default_ssh : [] };
    fs.writeFileSync(keys + '.tmp', JSON.stringify(next, null, 2), { mode: 0o600 }); fs.renameSync(keys + '.tmp', keys); fs.chmodSync(keys, 0o600);
    const wrap = path.join(dir, WRAP);
    fs.mkdirSync(wrap, { recursive: true });
    const varName = WIN ? 'APPDATA' : 'XDG_DATA_HOME';
    fs.writeFileSync(path.join(wrap, 'fj'), `#!/bin/sh\n# fj as ${login || host}: its keys file under ${dir} (made by VibeRoller for this person's Work runs)\n${varName}=${sq(dir)} exec ${sq(real)} "$@"\n`, { mode: 0o755 });
    if (!WIN) fs.chmodSync(path.join(wrap, 'fj'), 0o755);
    if (WIN) fs.writeFileSync(path.join(wrap, 'fj.cmd'), `@echo off\r\nset "APPDATA=${dir}"\r\n"${real}" %*\r\n`);
    const key = pathKey(base);
    return { [key]: wrap + path.delimiter + String(base[key] || '') };
  } catch { return {}; }
}

module.exports = { env, findFj };
