'use strict';
// Shared by server.js (the bridge app), run.js (the supervisor that owns the tunnel and keeps
// the app running) and restart.js: profile resolution, config merge and the JSON helpers.
// Keeping it in one place means all three always agree on which port, state file, flag file
// and gist belong to which person.
const fs = require('fs');
const path = require('path');

const ROOT = __dirname;
const MILLS_DIR = path.join(ROOT, '..');

function readJson(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch { return fallback; }
}
function writeJson(file, data) {
  // Write-then-rename so a crash mid-write never leaves a half-written file behind: the device
  // lock, sign-in sessions and model choice in state.json survive any restart intact.
  const tmp = file + '.tmp';
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2));
  fs.renameSync(tmp, file);
}

// --profile <name> on the command line, or MILLS_PROFILE in the environment. '' = the default
// profile (Alfreddo's original layout: bridge/auth.json, bridge/state.json, Mills/Archive).
function profileFromArgv(argv = process.argv) {
  const i = argv.indexOf('--profile');
  return (i >= 0 && argv[i + 1]) || process.env.MILLS_PROFILE || '';
}

function loadProfile(PROFILE) {
  if (PROFILE && !/^[a-z0-9-]{1,32}$/.test(PROFILE)) {
    console.error('Profile names use lowercase letters, digits and dashes only.');
    process.exit(1);
  }
  const PROFILE_DIR = PROFILE ? path.join(ROOT, 'profiles', PROFILE) : ROOT;
  const baseConfig = readJson(path.join(ROOT, 'config.json'), {});
  const profileConfig = PROFILE ? readJson(path.join(PROFILE_DIR, 'config.json'), null) : {};
  if (PROFILE && (!profileConfig || !profileConfig.port)) {
    console.error(`Missing bridge/profiles/${PROFILE}/config.json with at least {"port": <number>, "user": "<name>"}.`);
    process.exit(1);
  }
  if (PROFILE) { delete baseConfig.port; delete baseConfig.gistId; } // never share a port or a gist between profiles
  const config = Object.assign({
    port: 7777,
    user: 'Alfreddo', // who sits at the browser for this bridge (shown to Claude and in messages)
    allowedOrigins: ['https://unity-lab-ai.github.io'],
    permissionMode: 'acceptEdits',
    allowedTools: ['mcp__pc', 'Read', 'Edit', 'Write', 'Glob', 'Grep', 'WebFetch', 'WebSearch'],
    model: '', // this bridge's default model ('' = Claude Code's own default); the web app's dropdown overrides it
    models: [  // what the web app's model dropdown offers; id = what `claude --model` accepts
      { id: 'claude-fable-5-1', label: 'Fable 5.1' },
      { id: 'claude-opus-5-5', label: 'Opus 5.5' },
      { id: 'claude-sonnet-5-5', label: 'Sonnet 5.5' },
      { id: 'claude-haiku-5-5', label: 'Haiku 5.5' },
    ],
    tunnel: true,
    cloudflared: 'cloudflared',
    gistId: '',
  }, baseConfig, profileConfig);
  if (PROFILE && !profileConfig.user) config.user = PROFILE[0].toUpperCase() + PROFILE.slice(1);

  return {
    PROFILE,
    PROFILE_DIR,
    config,
    AUTH_FILE: path.join(PROFILE_DIR, 'auth.json'),
    STATE_FILE: path.join(PROFILE_DIR, 'state.json'),
    MCP_RUNTIME: path.join(PROFILE_DIR, '.mcp-runtime.json'),
    TUNNEL_FILE: path.join(PROFILE_DIR, 'tunnel.json'),   // written by run.js: gist id + current public URL
    RESTART_FLAG: path.join(PROFILE_DIR, '.restart'),      // dropped by restart.js, consumed by server.js
    ARCHIVE_DIR: PROFILE ? path.join(MILLS_DIR, 'Archive', PROFILE) : path.join(MILLS_DIR, 'Archive'), // saved conversations, one JSON file each
    GIST_FILE: PROFILE ? `mills-bridge-${PROFILE}.json` : 'mills-bridge.json', // what the web app looks for
  };
}

// Every profile that has a config.json, default first.
function allProfiles() {
  const dir = path.join(ROOT, 'profiles');
  const named = fs.existsSync(dir) ? fs.readdirSync(dir).filter((p) => fs.existsSync(path.join(dir, p, 'config.json'))).sort() : [];
  return ['', ...named];
}

module.exports = { ROOT, MILLS_DIR, readJson, writeJson, profileFromArgv, loadProfile, allProfiles };
