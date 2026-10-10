#!/usr/bin/env node
// Usage: node set-password.js "<new password>"   — writes bridge/auth.json (scrypt hash only).
'use strict';
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const password = process.argv[2];
if (!password || password.length < 12) {
  console.error('Usage: node set-password.js "<password of 12+ characters>"');
  process.exit(1);
}
const params = { N: 16384, r: 8, p: 1 };
const salt = crypto.randomBytes(16);
const hash = crypto.scryptSync(password, salt, 64, params);
fs.writeFileSync(path.join(__dirname, 'auth.json'),
  JSON.stringify({ ...params, salt: salt.toString('hex'), hash: hash.toString('hex') }, null, 2));
console.log('Password set. Restart the bridge if it is running.');
