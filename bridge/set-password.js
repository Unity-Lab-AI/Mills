#!/usr/bin/env node
// Usage: node set-password.js "<new password>"                    — writes bridge/auth.json (scrypt hash only).
//        node set-password.js --profile <name> "<new password>"   — writes bridge/profiles/<name>/auth.json.
'use strict';
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const argv = process.argv.slice(2);
const flag = argv.indexOf('--profile');
const profile = flag >= 0 ? argv[flag + 1] : '';
if (flag >= 0) argv.splice(flag, 2);
const password = argv[0];
if (!password || password.length < 12 || (flag >= 0 && !/^[a-z0-9-]{1,32}$/.test(profile || ''))) {
  console.error('Usage: node set-password.js [--profile <name>] "<password of 12+ characters>"');
  process.exit(1);
}
const dir = profile ? path.join(__dirname, 'profiles', profile) : __dirname;
fs.mkdirSync(dir, { recursive: true });
const params = { N: 16384, r: 8, p: 1 };
const salt = crypto.randomBytes(16);
const hash = crypto.scryptSync(password, salt, 64, params);
fs.writeFileSync(path.join(dir, 'auth.json'),
  JSON.stringify({ ...params, salt: salt.toString('hex'), hash: hash.toString('hex') }, null, 2));
console.log(`Password set${profile ? ' for profile "' + profile + '"' : ''}. Restart that bridge if it is running.`);
