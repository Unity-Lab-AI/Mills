// Draws VibeRoller's app icon (a rounded square in the brand gradient with a white heart) as PNGs and a Windows .ico,
// for the desktop app and the AI host (Tauri needs icons/icon.png + icons/icon.ico). Run: node make-icons.js
'use strict';
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');

function crc32(buf) { let c, crc = 0xffffffff; for (let n = 0; n < buf.length; n++) { c = (crc ^ buf[n]) & 0xff; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; crc = (crc >>> 8) ^ c; } return (crc ^ 0xffffffff) >>> 0; }
function chunk(type, data) { const len = Buffer.alloc(4); len.writeUInt32BE(data.length); const td = Buffer.concat([Buffer.from(type), data]); const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(td)); return Buffer.concat([len, td, crc]); }
function png(size) {
  const raw = Buffer.alloc((size * 4 + 1) * size);
  const r = size * 0.22; // corner radius
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    for (let x = 0; x < size; x++) {
      const o = y * (size * 4 + 1) + 1 + x * 4;
      // rounded square mask (anti-aliased by distance)
      const dx = Math.max(r - x, x - (size - 1 - r), 0), dy = Math.max(r - y, y - (size - 1 - r), 0);
      const d = Math.hypot(dx, dy) - r, a = Math.max(0, Math.min(1, 0.5 - d));
      // gradient violet → pink
      const t = (x + y) / (2 * size);
      let R = 124 + (236 - 124) * t, G = 58 + (72 - 58) * t, B = 237 + (153 - 237) * t;
      // heart: (x² + y² - 1)³ - x² y³ ≤ 0, centered
      const hx = (x - size / 2) / (size * 0.3), hy = -(y - size * 0.53) / (size * 0.3);
      const h = Math.pow(hx * hx + hy * hy - 1, 3) - hx * hx * hy * hy * hy;
      if (h <= 0) { R = 255; G = 255; B = 255; }
      raw[o] = R; raw[o + 1] = G; raw[o + 2] = B; raw[o + 3] = Math.round(a * 255);
    }
  }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4); ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}
function ico(sizes) { // PNG-in-ICO (Vista+)
  const imgs = sizes.map(png), head = Buffer.alloc(6 + 16 * sizes.length);
  head.writeUInt16LE(0, 0); head.writeUInt16LE(1, 2); head.writeUInt16LE(sizes.length, 4);
  let off = head.length;
  sizes.forEach((s, i) => { const e = 6 + 16 * i; head[e] = s >= 256 ? 0 : s; head[e + 1] = s >= 256 ? 0 : s; head.writeUInt16LE(1, e + 4); head.writeUInt16LE(32, e + 6); head.writeUInt32LE(imgs[i].length, e + 8); head.writeUInt32LE(off, e + 12); off += imgs[i].length; });
  return Buffer.concat([head, ...imgs]);
}
const dir = __dirname;
fs.writeFileSync(path.join(dir, 'icon.png'), png(512));
fs.writeFileSync(path.join(dir, '32x32.png'), png(32));
fs.writeFileSync(path.join(dir, '128x128.png'), png(128));
fs.writeFileSync(path.join(dir, 'tray.png'), png(64));
fs.writeFileSync(path.join(dir, 'icon.ico'), ico([16, 24, 32, 48, 64, 128, 256]));
console.log('icons written to', dir);
