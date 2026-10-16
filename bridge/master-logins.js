#!/usr/bin/env node
// Master login list for the host. Sets a NEW generated password on every account the hub knows
// (bridge/auth.json + every bridge/profiles/<name>/) and writes username + password to ONE file
// OUTSIDE the Mills folder, so Claude runs in the hub (which can read Mills/) never see it.
// Only the scrypt hash is kept by the hub, same as set-password.js. Device locks and existing
// sign-ins are untouched; the new passwords work on the next sign-in, no restart needed.
//
//   node master-logins.js                 every account → %USERPROFILE%/Desktop/Mills-master-logins.txt
//   node master-logins.js --out <file>    write the list somewhere else (must be outside Mills/)
'use strict';
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { MILLS_DIR, writeJson, loadProfile, allProfiles, hashPassword } = require('./common');

const argv = process.argv.slice(2);
const outAt = argv.indexOf('--out');
const OUT = path.resolve(outAt >= 0 ? argv.splice(outAt, 2)[1] : path.join(os.homedir(), 'Desktop', 'Mills-master-logins.txt'));
if (!path.relative(path.resolve(MILLS_DIR), OUT).startsWith('..')) {
  console.error('The list must live outside the Mills folder (Claude in the hub can read everything inside it).');
  process.exit(1);
}
const DEFAULT_USERNAME = loadProfile('').config.defaultUsername || 'mills';
const rows = [];
for (const profile of allProfiles()) {
  const P = loadProfile(profile);
  const username = profile || DEFAULT_USERNAME;
  const password = crypto.randomBytes(15).toString('base64url'); // 20 characters
  fs.mkdirSync(P.PROFILE_DIR, { recursive: true });
  writeJson(P.AUTH_FILE, hashPassword(password));
  rows.push([username, P.config.user || '', !!P.config.admin, password]);
}
fs.writeFileSync(OUT, [
  `Mills master login list — written ${new Date().toISOString()}`,
  'Keep this file private. Anyone with it can sign in as any account (first sign-in on a new device',
  'also needs the device lock reset by an admin). Regenerate with: node bridge/master-logins.js',
  '',
  ...rows.map(([u, d, a, p]) => `${u.padEnd(12)} ${p.padEnd(22)} ${d}${a ? ' (admin)' : ''}`),
  '',
].join(os.EOL));
console.log(`New passwords set for ${rows.map((r) => r[0]).join(', ')}.`);
console.log(`Master login list: ${OUT}`);
