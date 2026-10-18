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

module.exports = { cleanProfile, publicProfile, PALETTE_SIZE };
