// The native programs' releases, hub side: the pre-built binaries in native/dist/<target>/ (made by native/build.sh,
// on this machine: the auto-deploy builds them after a deploy that changed native/), described in signed manifests.
//
// A manifest is the JSON text { app, target, version, commit, files: [{ name, sha256, size }] } and is handed out as that
// exact text plus its signatures:
//   sig    Ed25519 with VibeRoller's release key (bridge/release-key.json, gitignored, made on first use, private to this
//          account). native/build.sh builds its public half into every binary (VR_RELEASE_PUBKEY), so a program takes
//          an update only when it was signed here.
//   hmac   for AI hosts: HMAC-SHA256 with that host's own secret too (as the JavaScript host's file updates), so a host
//          takes code only from the hub that knows its token.
// Apps: vr-host (the AI host + its control app; vr-hostw on Windows: its window-less starter) and vr-desktop.
// Routes: AI hosts GET /node/update/bin?app=&target= and /node/update/bin/file?app=&target=&name= (bridge/ai-nodes.js);
// the Desktop GET /api/pub/native?app=vr-desktop&target= and /api/pub/native/file?… (public: the binaries hold nothing
// private and are signed; bridge/downloads.js). /api/pub/versions lists what is there.
'use strict';
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const TARGETS = ['x86_64-unknown-linux-gnu', 'x86_64-pc-windows-gnu'];
const APPS = { 'vr-host': ['vr-host', 'vr-host.exe', 'vr-hostw.exe'], 'vr-desktop': ['vr-desktop', 'vr-desktop.exe'] };
// Ed25519 PKCS#8 DER before the 32-byte seed: SEQUENCE, version 0, the Ed25519 OID (1.3.101.112), OCTET STRING of 32
const PKCS8_PREFIX = Buffer.from([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20]);

module.exports = function createNativeReleases({ ROOT, MILLS_DIR, release = () => ({ version: '', commit: '' }) }) {
  const DIST = path.join(MILLS_DIR, 'native', 'dist');
  const KEY_FILE = path.join(ROOT, 'release-key.json');
  let key = null;
  // the release key pair: { secret (32-byte seed, hex), public (32 bytes, hex) }; made once
  function keyPair() {
    if (key) return key;
    try { const k = JSON.parse(fs.readFileSync(KEY_FILE, 'utf8')); if (/^[0-9a-f]{64}$/.test(k.secret) && /^[0-9a-f]{64}$/.test(k.public)) return (key = k); } catch {}
    const { privateKey, publicKey } = crypto.generateKeyPairSync('ed25519');
    const seed = privateKey.export({ format: 'der', type: 'pkcs8' }).subarray(-32), pub = publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
    key = { secret: seed.toString('hex'), public: pub.toString('hex'), _: 'VibeRoller release key: signs the native binaries. Never share the secret.' };
    fs.writeFileSync(KEY_FILE, JSON.stringify(key, null, 2) + '\n', { mode: 0o600 });
    return key;
  }
  const sign = (text) => crypto.sign(null, Buffer.from(text, 'utf8'), crypto.createPrivateKey({ key: Buffer.concat([PKCS8_PREFIX, Buffer.from(keyPair().secret, 'hex')]), format: 'der', type: 'pkcs8' })).toString('hex');
  const ok = (app, target) => Object.prototype.hasOwnProperty.call(APPS, app) && TARGETS.includes(target);
  const cache = new Map(); // app|target → { stamp, text }
  // the manifest text for one app + target, or null when nothing is built for it
  function manifestText(app, target) {
    if (!ok(app, target)) return null;
    const dir = path.join(DIST, target);
    const names = APPS[app].filter((n) => { try { return fs.statSync(path.join(dir, n)).isFile(); } catch { return false; } });
    if (!names.length) return null;
    const stamp = names.map((n) => { const s = fs.statSync(path.join(dir, n)); return `${n}:${s.mtimeMs}:${s.size}`; }).join('|');
    const c = cache.get(app + '|' + target); if (c && c.stamp === stamp) return c.text;
    let build = {}; try { build = JSON.parse(fs.readFileSync(path.join(dir, 'BUILD.json'), 'utf8')); } catch {}
    const rel = release();
    const files = names.map((n) => { const b = fs.readFileSync(path.join(dir, n)); return { name: n, sha256: crypto.createHash('sha256').update(b).digest('hex'), size: b.length }; });
    const text = JSON.stringify({ app, target, version: String(build.version || rel.version || ''), commit: String(build.commit || rel.commit || ''), files });
    cache.set(app + '|' + target, { stamp, text });
    return text;
  }
  // { manifest, sig, hmac? } or null
  function signed(app, target, hostSecret) {
    const text = manifestText(app, target); if (!text) return null;
    return { manifest: text, sig: sign(text), ...(hostSecret ? { hmac: crypto.createHmac('sha256', String(hostSecret)).update(text).digest('hex') } : {}) };
  }
  function file(app, target, name) {
    if (!ok(app, target) || !APPS[app].includes(String(name))) return null;
    try { return fs.readFileSync(path.join(DIST, target, name)); } catch { return null; }
  }
  // what is built, for /api/pub/versions and the Download panel
  function list() {
    const out = [];
    for (const app of Object.keys(APPS)) for (const target of TARGETS) { const t = manifestText(app, target); if (t) { const m = JSON.parse(t); out.push({ app, target, version: m.version, commit: m.commit, files: m.files.map((f) => ({ name: f.name, size: f.size, sha256: f.sha256 })) }); } }
    return out;
  }
  return { signed, file, list, publicKey: () => keyPair().public, KEY_FILE, DIST, TARGETS, APPS };
};
