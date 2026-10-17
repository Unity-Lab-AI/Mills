#!/usr/bin/env node
// Fetches the third-party browser libraries Mills ships itself (web/vendor/), straight from the npm
// registry, and verifies each package against the registry's published integrity hash before a
// single byte is unpacked. Usage, from the Mills folder:
//   node bridge/vendor.js            install / update everything listed below
//   node bridge/vendor.js --check    verify the files on disk match web/vendor/VENDOR.json
// The page's CSP allows no outside scripts, so these files are served from the site itself.
'use strict';

const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const crypto = require('crypto');

const VENDOR_DIR = path.join(__dirname, '..', 'web', 'vendor');
const MANIFEST = path.join(VENDOR_DIR, 'VENDOR.json');
// name → { range (dist-tag or exact version), files: [{ file inside the package, out: name here } | { dir, outDir }] }
// (`file` / `out` alone is the one-file form)
const LIBS = {
  mermaid: { range: '11', file: 'dist/mermaid.min.js', out: 'mermaid.min.js' },
  katex: { range: '0', files: [{ file: 'dist/katex.min.js', out: 'katex.min.js' }, { file: 'dist/katex.min.css', out: 'katex.min.css' }, { dir: 'dist/fonts/', outDir: 'fonts/', match: /\.woff2$/ }] }, // woff2 only: every browser Mills supports reads it
};

const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');
function tarEntries(tarBuf) { // minimal ustar reader: enough for an npm tarball
  const out = new Map();
  let off = 0;
  while (off + 512 <= tarBuf.length) {
    const head = tarBuf.subarray(off, off + 512);
    if (head.every((b) => b === 0)) break;
    let name = head.subarray(0, 100).toString().replace(/\0.*$/, '');
    const prefix = head.subarray(345, 500).toString().replace(/\0.*$/, '');
    if (prefix) name = prefix + '/' + name;
    const size = parseInt(head.subarray(124, 136).toString().replace(/\0.*$/, '').trim(), 8) || 0;
    const type = String.fromCharCode(head[156]);
    off += 512;
    if (type === '0' || type === '\0') out.set(name, tarBuf.subarray(off, off + size));
    off += Math.ceil(size / 512) * 512;
  }
  return out;
}
async function fetchJson(url) { const r = await fetch(url, { headers: { accept: 'application/json' } }); if (!r.ok) throw new Error(url + ' → ' + r.status); return r.json(); }
async function fetchBuf(url) { const r = await fetch(url); if (!r.ok) throw new Error(url + ' → ' + r.status); return Buffer.from(await r.arrayBuffer()); }

async function install(name, spec) {
  const meta = await fetchJson('https://registry.npmjs.org/' + name);
  const version = meta['dist-tags'][spec.range] || (meta.versions[spec.range] ? spec.range
    : Object.keys(meta.versions).filter((v) => v.startsWith(spec.range + '.') && !/-/.test(v)).sort((a, b) => a.localeCompare(b, undefined, { numeric: true })).pop());
  if (!version) throw new Error(`${name}: no version matches "${spec.range}"`);
  const dist = meta.versions[version].dist;
  const tgz = await fetchBuf(dist.tarball);
  // Verify the whole package against the registry's integrity (sha512) before unpacking.
  const [algo, b64] = String(dist.integrity || '').split('-');
  if (algo !== 'sha512' || crypto.createHash('sha512').update(tgz).digest('base64') !== b64) throw new Error(`${name}@${version}: tarball failed the registry integrity check`);
  const entries = tarEntries(zlib.gunzipSync(tgz));
  fs.mkdirSync(VENDOR_DIR, { recursive: true });
  const wanted = spec.files || [{ file: spec.file, out: spec.out }];
  const files = [];
  for (const w of wanted) {
    if (w.dir) { // every file under a folder of the package (e.g. a font set)
      let n = 0;
      for (const [entry, data] of entries) {
        if (!entry.startsWith('package/' + w.dir) || entry.endsWith('/') || (w.match && !w.match.test(entry))) continue;
        const out = path.posix.join(w.outDir, entry.slice(('package/' + w.dir).length));
        fs.mkdirSync(path.dirname(path.join(VENDOR_DIR, out)), { recursive: true });
        fs.writeFileSync(path.join(VENDOR_DIR, out), data); files.push({ file: out, bytes: data.length, sha256: sha256(data) }); n++;
      }
      if (!n) throw new Error(`${name}@${version}: ${w.dir} has no files in the package`);
      continue;
    }
    const data = entries.get('package/' + w.file);
    if (!data) throw new Error(`${name}@${version}: ${w.file} is not in the package`);
    fs.writeFileSync(path.join(VENDOR_DIR, w.out), data);
    files.push({ file: w.out, bytes: data.length, sha256: sha256(data) });
  }
  const main = files[0];
  return { version, file: main.file, bytes: files.reduce((s, f) => s + f.bytes, 0), sha256: main.sha256, files, source: dist.tarball };
}
function check() {
  const manifest = JSON.parse(fs.readFileSync(MANIFEST, 'utf8'));
  let bad = 0;
  for (const [name, rec] of Object.entries(manifest)) {
    for (const f of rec.files || [{ file: rec.file, sha256: rec.sha256 }]) {
      const file = path.join(VENDOR_DIR, f.file);
      const ok = fs.existsSync(file) && sha256(fs.readFileSync(file)) === f.sha256;
      if (!ok) bad++;
      if (!ok || !rec.files || f === rec.files[0]) console.log(`${ok ? 'ok  ' : 'BAD '} ${name}@${rec.version}  ${f.file}${rec.files && f === rec.files[0] ? ` (+${rec.files.length - 1} more files)` : ''}`);
    }
  }
  return bad;
}

async function installAll(log = console.log) {
  const manifest = fs.existsSync(MANIFEST) ? JSON.parse(fs.readFileSync(MANIFEST, 'utf8')) : {};
  for (const [name, spec] of Object.entries(LIBS)) {
    const rec = await install(name, spec);
    manifest[name] = rec;
    log(`${name}@${rec.version} → web/vendor/${rec.file}${rec.files && rec.files.length > 1 ? ` + ${rec.files.length - 1} more files` : ''} (${(rec.bytes / 1024).toFixed(0)} KB, sha256 ${rec.sha256.slice(0, 12)}…)`);
  }
  fs.writeFileSync(MANIFEST, JSON.stringify(manifest, null, 2) + '\n');
  return manifest;
}
// Also used by bridge/test/regress.js --install-vendor (the runner is a command Claude may run from a Work session).
module.exports = { installAll, check, LIBS, VENDOR_DIR, MANIFEST };

if (require.main === module) {
  (async () => {
    if (process.argv.includes('--check')) { process.exit(check() ? 1 : 0); }
    await installAll();
  })().catch((e) => { console.error('vendor: ' + e.message); process.exit(1); });
}
