// Mills user profiles: what others see about a person (their "user settings"), kept apart from how Mills
// behaves for them (their "app settings": sounds, speech, calls, theme …, in state.prefs, private to them).
//
// Stored per account in state.profile (bridge/profiles/<name>/state.json, next to the prefs); everyone signed in
// sees it through /api/room/members. Nickname and colour stay in state.prefs where they always were (older pages
// and the board read them there) but count as profile: Settings shows them on the Profile tab.
//
// Fields (all optional; an empty profile is the default, which simply follows the person's colour):
//   bio        up to 300 characters, plain text (links shown as links)
//   pronouns   up to 40
//   status     a short custom status line, up to 80
//   location   up to 60
//   links      up to 3 https:// addresses
//   accent     palette index for the card's banner, or null = follow their colour
//   timezone   an IANA zone name (e.g. Europe/Berlin), only if they want their local time shown
'use strict';

const PALETTE_SIZE = 16;
const text = (v, max) => String(v == null ? '' : v).replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '').trim().slice(0, max);
const oneLine = (v, max) => text(v, max).replace(/\s+/g, ' ');

function cleanProfile(p, prev = {}) {
  const out = { ...prev };
  if (!p || typeof p !== 'object') return out;
  if (p.bio !== undefined) out.bio = text(p.bio, 300).replace(/\n{3,}/g, '\n\n');
  if (p.pronouns !== undefined) out.pronouns = oneLine(p.pronouns, 40);
  if (p.status !== undefined) out.status = oneLine(p.status, 80);
  if (p.location !== undefined) out.location = oneLine(p.location, 60);
  if (p.links !== undefined) {
    const list = (Array.isArray(p.links) ? p.links : String(p.links || '').split(/[\s,]+/)).map((l) => String(l).trim()).filter(Boolean);
    const bad = list.find((l) => !/^https:\/\/[^\s/]+\.[^\s]+$/i.test(l) || l.length > 200);
    if (bad) throw new Error('Links must be https:// addresses (this one is not: ' + bad.slice(0, 60) + ').');
    out.links = [...new Set(list)].slice(0, 3);
  }
  if (p.accent !== undefined) out.accent = Number.isInteger(p.accent) && p.accent >= 0 && p.accent < PALETTE_SIZE ? p.accent : null;
  if (p.timezone !== undefined) {
    const tz = oneLine(p.timezone, 60);
    if (tz) { try { new Intl.DateTimeFormat('en', { timeZone: tz }); } catch { throw new Error('Unknown time zone: ' + tz); } }
    out.timezone = tz;
  }
  return out;
}
const publicProfile = (state) => {
  const p = (state && state.profile) || {};
  return { bio: p.bio || '', pronouns: p.pronouns || '', status: p.status || '', location: p.location || '', links: p.links || [], accent: p.accent ?? null, timezone: p.timezone || '' };
};

// ---- profile pictures ----
// The browser crops + shrinks the picture to a square (256 px, WebP / PNG / JPEG) before it is sent, so no original
// and none of its metadata leaves the person's PC. The hub checks the type by its first bytes (never trusts the
// name), caps the size, and keeps one file per person in Mills/Avatars/<username>.<ext> (gitignored; outside the
// account folders). Everyone signed in gets it inside the member list as a data: address (no extra request, no
// cookie or token needed by an <img>).
const fs = require('fs');
const path = require('path');
const AVATAR_MAX = 300 * 1024; // bytes after decoding
const KINDS = { webp: 'image/webp', png: 'image/png', jpg: 'image/jpeg' };
function sniff(buf) {
  if (buf.length > 12 && buf.toString('ascii', 0, 4) === 'RIFF' && buf.toString('ascii', 8, 12) === 'WEBP') return 'webp';
  if (buf.length > 8 && buf[0] === 0x89 && buf.toString('ascii', 1, 4) === 'PNG') return 'png';
  if (buf.length > 3 && buf[0] === 0xff && buf[1] === 0xd8 && buf[2] === 0xff) return 'jpg';
  return '';
}
function createAvatars(dir) {
  const cache = new Map(); // username -> data: URL ('' = none)
  const fileOf = (user) => { for (const ext of Object.keys(KINDS)) { const f = path.join(dir, user + '.' + ext); if (fs.existsSync(f)) return { f, ext }; } return null; };
  function get(user) {
    if (cache.has(user)) return cache.get(user);
    const hit = fileOf(user); let url = '';
    if (hit) { try { url = 'data:' + KINDS[hit.ext] + ';base64,' + fs.readFileSync(hit.f).toString('base64'); } catch {} }
    cache.set(user, url); return url;
  }
  function set(user, dataUrl) {
    const m = /^data:image\/(webp|png|jpeg);base64,([A-Za-z0-9+/=]+)$/.exec(String(dataUrl || ''));
    if (!m) throw new Error('Send the picture as a WebP, PNG or JPEG image.');
    const buf = Buffer.from(m[2], 'base64');
    if (!buf.length || buf.length > AVATAR_MAX) throw new Error('That picture is too large (up to 300 KB after shrinking).');
    const ext = sniff(buf); if (!ext) throw new Error('That file is not a WebP, PNG or JPEG picture.');
    fs.mkdirSync(dir, { recursive: true });
    for (const e of Object.keys(KINDS)) if (e !== ext) { try { fs.unlinkSync(path.join(dir, user + '.' + e)); } catch {} } // one picture per person
    const f = path.join(dir, user + '.' + ext), tmp = f + '.tmp';
    fs.writeFileSync(tmp, buf); fs.renameSync(tmp, f);
    cache.delete(user); return get(user);
  }
  function remove(user) { for (const e of Object.keys(KINDS)) { try { fs.unlinkSync(path.join(dir, user + '.' + e)); } catch {} } cache.set(user, ''); return ''; }
  return { get, set, remove, DIR: dir };
}

module.exports = { cleanProfile, publicProfile, createAvatars, PALETTE_SIZE, AVATAR_MAX };
