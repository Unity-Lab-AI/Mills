#!/usr/bin/env node
// A zip of a folder, with nothing but Node (this server has no zip program): node native/zip.js out.zip folder
// Every file deflated, CRC-32 from zlib, plain names (no folders entries), the shape every unzip reads.
'use strict';
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');
const [out, dir] = process.argv.slice(2);
if (!out || !dir) { console.error('usage: node zip.js out.zip folder'); process.exit(2); }
const files = [];
(function walk(d, rel) { for (const e of fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) { const p = path.join(d, e.name), r = rel ? rel + '/' + e.name : e.name; if (e.isDirectory()) walk(p, r); else if (e.isFile()) files.push([p, r]); } })(dir, '');
const parts = [], central = []; let offset = 0;
const dosTime = (d) => ((d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1)), dosDate = (d) => (((d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate());
for (const [p, r] of files) {
  const data = fs.readFileSync(p), st = fs.statSync(p), comp = zlib.deflateRawSync(data, { level: 9 }), crc = zlib.crc32(data), name = Buffer.from(r, 'utf8'), d = new Date(st.mtimeMs);
  const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6); local.writeUInt16LE(8, 8); local.writeUInt16LE(dosTime(d), 10); local.writeUInt16LE(dosDate(d), 12);
  local.writeUInt32LE(crc >>> 0, 14); local.writeUInt32LE(comp.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(name.length, 26); local.writeUInt16LE(0, 28);
  const cen = Buffer.alloc(46); cen.writeUInt32LE(0x02014b50, 0); cen.writeUInt16LE(20, 4); cen.writeUInt16LE(20, 6); cen.writeUInt16LE(0x0800, 8); cen.writeUInt16LE(8, 10); cen.writeUInt16LE(dosTime(d), 12); cen.writeUInt16LE(dosDate(d), 14);
  cen.writeUInt32LE(crc >>> 0, 16); cen.writeUInt32LE(comp.length, 20); cen.writeUInt32LE(data.length, 24); cen.writeUInt16LE(name.length, 28); cen.writeUInt16LE(0, 30); cen.writeUInt16LE(0, 32); cen.writeUInt16LE(0, 34); cen.writeUInt16LE(0, 36);
  cen.writeUInt32LE(((p.endsWith('.exe') ? 0o100755 : 0o100644) << 16) >>> 0, 38); cen.writeUInt32LE(offset, 42);
  parts.push(local, name, comp); central.push(cen, name); offset += local.length + name.length + comp.length;
}
const cenBuf = Buffer.concat(central), end = Buffer.alloc(22);
end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(0, 4); end.writeUInt16LE(0, 6); end.writeUInt16LE(files.length, 8); end.writeUInt16LE(files.length, 10); end.writeUInt32LE(cenBuf.length, 12); end.writeUInt32LE(offset, 16); end.writeUInt16LE(0, 20);
fs.writeFileSync(out, Buffer.concat([...parts, cenBuf, end]));
console.log(`${out}: ${files.length} file(s)`);
