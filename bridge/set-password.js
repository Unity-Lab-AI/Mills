#!/usr/bin/env node
// Usage: node set-password.js "<new password>"                    — the "mills" account (bridge/auth.json).
//        node set-password.js --profile <name> "<new password>"   — account <name> (bridge/profiles/<name>/auth.json).
// Only the scrypt hash is stored. Gee's admin section in the web app can also do this.
'use strict';
const fs = require('fs');
const path = require('path');
const { hashPassword } = require('./common');

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
fs.writeFileSync(path.join(dir, 'auth.json'), JSON.stringify(hashPassword(password), null, 2));
console.log(`Password set${profile ? ' for account "' + profile + '"' : ''}. Takes effect on the next sign-in, no restart needed.`);
