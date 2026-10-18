// Mills servers ("guilds"; the two words mean the same here), channels and group conversations: the
// Discord-shaped side of the chat.
//
// Conversation ids the hub routes by (the text inside stays end-to-end encrypted; these are metadata):
//   'room'                  #general of the everyone-server "Mills": every account, Unity included (unchanged)
//   'dm:<a>|<b>'            a private conversation between two people (unchanged, carried by `to`)
//   'ch:<server>/<channel>' a channel of a server: the server's members who may view that channel
//   'grp:<id>'              a group conversation: its members only (up to GROUP_MAX people)
// Membership decides who may receive a key slot, read, type, mark seen, call and be posted to. Unity
// (the room agent) counts as a member everywhere, so she can be @-called in any channel or group.
//
// Ownership: whoever makes a server or a group owns it (older records: their creator). Ownership can be handed on;
// a group's owner who leaves hands it to the next member; a server's owner must hand it over (or delete it) first.
//
// Servers (Discord-shaped; nobody signs up here, the hub admin makes accounts, so "public" never happens):
//   - roles + permissions + channel permission overwrites: bridge/guild-perms.js (Discord's model and order);
//     preset levels Admin / Moderator / Member when a server is made, and as role templates;
//   - joining: a personal invitation (Accept / Decline), or an invite code (expiry, max uses, "temporary": they
//     leave when they go offline unless they got a role); invites can be paused; a password lock asks every
//     invite-CODE join for the password (5 wrong tries per 10 min); bans keep people out;
//   - moderation: kick, ban (optionally deleting their messages of the last hour / day / week), timeout (read only
//     for a while), server mute in calls; the audit log keeps the last AUDIT_KEEP changes;
//   - settings: name, icon, colour, description, welcome channel (join / leave messages posted by the hub as
//     "Mills"), rules channel, default notifications (all / mentions), a wait-before-posting rule for new members;
//   - channels: categories, order, topic, display name, slowmode, age-restricted flag, announcement, overwrites;
//   - pins: per conversation (server channels need PIN_MESSAGES; groups and private conversations: any member).
// The everyone-server "Mills" is a server record too (editable by the hub admins, who act as its owner), but
// everyone is always in it: no invites, password, kick or ban (timeouts and mutes do work there).
// Storage: Mills/Chat/guilds.json (gitignored, next to the chat ciphertext; names, member lists, settings; the
// password only as a salted scrypt hash).
'use strict';

const path = require('path');
const crypto = require('crypto');
const createPerms = require('./guild-perms');

const slug = (s) => String(s || '').toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 32);
const GROUP_MAX = 10, CHANNEL_MAX = 50, ROLE_MAX = 25, INVITE_MAX = 50, AUDIT_KEEP = 200, SERVER_MAX_PER_USER = 25, PIN_MAX = 50;
const COLOR_RE = /^#[0-9a-f]{6}$/i;
const INVITE_AGES = [0, 30, 60, 360, 720, 1440, 10080]; // minutes; 0 = never expires
const TIMEOUTS = [1, 5, 10, 60, 1440, 10080, 40320];    // minutes (Discord: 60 s … 28 days)
const SLOWMODES = [0, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 21600]; // seconds
const BAN_DELETE = [0, 3600, 86400, 604800];             // seconds of their messages to delete
const WAITS = [0, 5, 10];                                // minutes a new member waits before posting
const PW_TRIES = 5, PW_WINDOW = 10 * 60e3, TEMP_GRACE = 60e3;

module.exports = function createGuilds({ dir, accounts, readJson, writeJson, agentName, changed, send, readBody, isOnline = () => true,
  getSeq = () => 0, msgConv = () => null, announce = () => {}, deleteRecent = () => 0 }) {
  const FILE = path.join(dir, 'guilds.json');
  const P = createPerms({ isHubAdmin: (u) => !!(accounts.get(u) && accounts.get(u).admin) });
  let db = readJson(FILE, null);
  if (!db || !Array.isArray(db.servers)) db = { servers: [], groups: [] };
  if (!Array.isArray(db.groups)) db.groups = [];
  if (!db.pins || typeof db.pins !== 'object') db.pins = {};
  const obj = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : {});
  // ---- load + upgrade older records (every field a server / channel / group may lack) ----
  function normChannel(ch, i, s) {
    const c = { id: ch.id, name: ch.name || ch.id, topic: ch.topic || '', category: ch.category || '', position: Number.isFinite(ch.position) ? ch.position : i,
      announce: !!ch.announce, nsfw: !!ch.nsfw, slowmode: SLOWMODES.includes(ch.slowmode) ? ch.slowmode : 0, createdBy: ch.createdBy || '', overwrites: Array.isArray(ch.overwrites) ? ch.overwrites : [] };
    if (Array.isArray(ch.roles) && ch.roles.length && !c.overwrites.length) { // the first version's "private to roles" → overwrites
      c.overwrites = [{ type: 'role', id: 'everyone', allow: [], deny: ['VIEW_CHANNEL'] }, ...ch.roles.filter((id) => s.roles.some((r) => r.id === id)).map((id) => ({ type: 'role', id, allow: ['VIEW_CHANNEL'], deny: [] }))];
    }
    c.overwrites = c.overwrites.map((o) => ({ type: o.type === 'member' ? 'member' : 'role', id: String(o.id), allow: P.cleanPerms(o.allow, P.CHANNEL_PERMS), deny: P.cleanPerms(o.deny, P.CHANNEL_PERMS) }));
    return c;
  }
  function normServer(s) {
    s.name = s.name || s.id; s.icon = s.icon || ''; s.color = COLOR_RE.test(s.color || '') ? s.color : ''; s.description = s.description || '';
    s.owner = s.owner || s.createdBy || ''; s.members = Array.isArray(s.members) ? s.members : [];
    s.roles = (Array.isArray(s.roles) ? s.roles : []).map((r) => ({ ...r, perms: P.cleanPerms(r.perms) }));
    s.memberRoles = obj(s.memberRoles);
    const ev = Array.isArray(s.everyonePerms) ? P.cleanPerms(s.everyonePerms) : [...P.DEFAULT_EVERYONE];
    s.everyonePerms = s.permsVersion === 2 ? ev : [...new Set([...ev, ...P.DEFAULT_EVERYONE])].filter((p) => p !== 'ADMINISTRATOR'); // v1 had only invite + make channels
    s.nicks = obj(s.nicks); s.bans = Array.isArray(s.bans) ? s.bans : []; s.invites = Array.isArray(s.invites) ? s.invites : []; s.pending = obj(s.pending);
    s.temp = obj(s.temp); s.audit = Array.isArray(s.audit) ? s.audit : []; s.password = s.password || null;
    s.timeouts = obj(s.timeouts); s.voiceMuted = obj(s.voiceMuted); s.joined = obj(s.joined);
    s.settings = { systemChannel: '', systemJoins: true, systemLeaves: false, rulesChannel: '', notify: 'all', invitesPaused: false, wait: 0, ...obj(s.settings) };
    s.channels = (Array.isArray(s.channels) ? s.channels : []).map((ch, i) => normChannel(ch, i, s));
    if (!s.channels.some((c) => c.id === 'general')) s.channels.unshift(normChannel({ id: 'general', name: 'general', position: -1 }, 0, s));
    s.permsVersion = 2;
    ensureBuiltins(s);
    return s;
  }
  // The built-in ladder (bridge/guild-perms.js BUILTINS) on every server. Older servers: a same-named role becomes the
  // built-in (keeping its id and holders), custom roles move up between Admin (40) and Super Admin (50) so they keep
  // outranking ordinary members, and every non-temporary member gets Member.
  const BUILTIN_POS = new Set(P.BUILTINS.map((b) => b.position));
  const isBuiltinPos = (n) => BUILTIN_POS.has(n);
  function ensureBuiltins(s) {
    const fresh = s.rolesVersion !== 3;
    for (const b of P.BUILTINS) {
      let r = s.roles.find((x) => x.builtin === b.key) || (fresh && s.roles.find((x) => !x.builtin && String(x.name).toLowerCase() === b.name.toLowerCase()));
      if (!r) { r = { id: 'b-' + b.key, name: b.name, color: b.color, perms: [...b.perms], hoist: true, mentionable: false }; s.roles.push(r); }
      r.builtin = b.key; r.position = b.position;
      if (b.key === 'coowner' || b.key === 'superadmin') r.perms = ['ADMINISTRATOR'];
    }
    if (fresh) {
      s.roles.filter((r) => !r.builtin).sort((a, c) => a.position - c.position).forEach((r, i) => { r.position = Math.min(49, 41 + i); });
      const mem = s.roles.find((r) => r.builtin === 'member');
      if (!s.everyone) for (const u of s.members) if (!s.temp[u]) { const l = s.memberRoles[u] || []; if (!l.includes(mem.id)) s.memberRoles[u] = [mem.id, ...l]; }
      s.rolesVersion = 3;
    }
  }
  const memberRole = (s) => s.roles.find((r) => r.builtin === 'member');
  const realRoles = (s, u) => (s.memberRoles[u] || []).filter((id) => { const r = s.roles.find((x) => x.id === id); return r && r.builtin !== 'member'; }); // roles beyond the automatic Member
  let mills = db.servers.find((s) => s.id === 'mills');
  if (!mills) { mills = { id: 'mills', name: 'Mills', everyone: true, channels: [{ id: 'general', name: 'general', topic: 'the whole team' }], createdBy: '' }; db.servers.unshift(mills); }
  mills.everyone = true;
  db.servers.forEach(normServer);
  for (const g of db.groups) g.owner = g.owner || g.createdBy || g.members[0] || '';
  let ver = 1;
  const save = () => { writeJson(FILE, db); ver++; changed(); };
  const server = (id) => db.servers.find((s) => s.id === id);
  const group = (id) => db.groups.find((g) => g.id === id);
  const serverMembers = (s) => (s.everyone ? [...accounts.keys()] : s.members.filter((u) => accounts.has(u)));
  const inServer = (s, user) => serverMembers(s).includes(user);
  const audit = (s, by, what, detail) => { s.audit.push({ at: new Date().toISOString(), by, what, ...(detail ? { detail: String(detail).slice(0, 200) } : {}) }); if (s.audit.length > AUDIT_KEEP) s.audit.splice(0, s.audit.length - AUDIT_KEEP); };
  const sortedChannels = (s) => [...s.channels].sort((a, b) => a.position - b.position);
  const sees = (s, ch, user) => P.canIn(s, ch, user, 'VIEW_CHANNEL');

  function parse(conv) { // → { kind, server?, channel?, group? } or null
    if (conv === 'room') return { kind: 'room' };
    let m = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
    if (m) return { kind: 'dm', a: m[1], b: m[2] };
    m = /^ch:([a-z0-9-]{1,32})\/([a-z0-9-]{1,32})$/.exec(String(conv || ''));
    if (m) return { kind: 'ch', server: m[1], channel: m[2] };
    m = /^grp:([a-z0-9-]{1,32})$/.exec(String(conv || ''));
    if (m) return { kind: 'grp', group: m[1] };
    return null;
  }
  function chanOf(conv) { // 'room' and 'ch:…' → { s, ch } (room = Mills #general); anything else → null
    const c = parse(conv); if (!c) return null;
    if (c.kind === 'room') return { s: mills, ch: mills.channels.find((x) => x.id === 'general') };
    if (c.kind !== 'ch') return null;
    const s = server(c.server); const ch = s && s.channels.find((x) => x.id === c.channel);
    if (!ch || (s.everyone && ch.id === 'general')) return null;
    return { s, ch };
  }
  const exists = (conv) => {
    const c = parse(conv);
    if (!c) return false;
    if (c.kind === 'room') return true;
    if (c.kind === 'dm') return c.a < c.b && accounts.has(c.a) && accounts.has(c.b);
    if (c.kind === 'ch') return !!chanOf(conv);
    return !!group(c.group);
  };
  function members(conv) { // the people (usernames) of a channel / group conversation; null for room / dm / unknown
    const c = parse(conv);
    if (!c) return null;
    if (c.kind === 'ch') { const f = chanOf(conv); return f ? serverMembers(f.s).filter((u) => sees(f.s, f.ch, u)) : null; }
    if (c.kind === 'grp') { const g = group(c.group); return g ? g.members.filter((u) => accounts.has(u)) : null; }
    return null;
  }
  const isGuildConv = (conv) => /^(ch|grp):/.test(String(conv || ''));
  function isMember(conv, user) { const list = members(conv); return !!list && list.includes(user); }
  // ---- permission checks the hub's chat / call routes use (room = Mills #general; groups + private: no roles) ----
  function allows(conv, user, perm) { const f = chanOf(conv); return f ? P.canIn(f.s, f.ch, user, perm) : true; }
  const lastPost = new Map(); // `${conv}|${user}` -> ms (slowmode)
  function postCheck(conv, user, { voice = false, edit = false } = {}) { // '' = may post; otherwise why not
    const f = chanOf(conv); if (!f) return '';
    const { s, ch } = f, perms = P.permsIn(s, ch, user), has = (p) => perms.includes(p);
    if (P.timedOut(s, user)) return `You are timed out in ${s.name} until ${new Date(s.timeouts[user]).toLocaleString()}: you can read but not post.`;
    if (!has('SEND_MESSAGES')) return ch.announce ? `#${ch.name} is an announcement channel: only people with the announce permission post here.` : `You may not post in #${ch.name}.`;
    if (voice && !has('SEND_VOICE_MESSAGES')) return `You may not send voice clips in #${ch.name}.`;
    if (edit) return '';
    const wait = s.settings.wait || 0, j = s.joined[user];
    if (wait && j && !realRoles(s, user).length && Date.now() - j.at < wait * 60e3) return `New members of ${s.name} wait ${wait} minutes before posting (${Math.ceil((wait * 60e3 - (Date.now() - j.at)) / 60e3)} to go).`;
    if (ch.slowmode && !has('BYPASS_SLOWMODE') && !has('MANAGE_MESSAGES') && !has('MANAGE_CHANNELS')) {
      const last = lastPost.get(conv + '|' + user) || 0, left = ch.slowmode * 1000 - (Date.now() - last);
      if (left > 0) return `Slowmode in #${ch.name}: wait ${Math.ceil(left / 1000)} s.`;
    }
    return '';
  }
  const notePost = (conv, user) => { if (chanOf(conv)) lastPost.set(conv + '|' + user, Date.now()); };
  function canRead(conv, user, seq) { // READ_MESSAGE_HISTORY: without it, only messages from after they joined
    const f = chanOf(conv); if (!f) return true;
    if (P.canIn(f.s, f.ch, user, 'READ_MESSAGE_HISTORY')) return true;
    const j = f.s.joined[user]; return !!j && seq > (j.seq || 0);
  }
  function media(conv, user) { // what a person may do in this conversation's call
    const f = chanOf(conv); if (!f) return { connect: true, speak: true, stream: true, muted: false };
    const perms = P.permsIn(f.s, f.ch, user);
    return { connect: perms.includes('CONNECT'), speak: perms.includes('SPEAK'), stream: perms.includes('STREAM'), muted: !!f.s.voiceMuted[user] };
  }

  // ---- what one person sees ----
  const pinsOf = (conv) => (db.pins[conv] || []).filter((p) => msgConv(p.seq) !== null);
  const isPrivate = (ch) => ch.overwrites.some((o) => o.type === 'role' && o.id === 'everyone' && o.deny.includes('VIEW_CHANNEL'));
  function view(user) {
    const servers = db.servers.filter((s) => inServer(s, user)).map((s) => {
      const perms = P.permsOf(s, user), has = (p) => perms.includes(p), manage = has('MANAGE_CHANNELS') || has('MANAGE_ROLES');
      return {
        id: s.id, name: s.name, icon: s.icon, color: s.color, description: s.description, everyone: !!s.everyone, owner: s.everyone ? '' : s.owner, createdBy: s.createdBy || '',
        locked: !!s.password, settings: s.settings, members: serverMembers(s), temp: Object.keys(s.temp), nicks: s.nicks,
        timeouts: Object.fromEntries(Object.entries(s.timeouts).filter(([u, t]) => Date.parse(t) > Date.now() && (u === user || has('MODERATE_MEMBERS')))),
        voiceMuted: Object.keys(s.voiceMuted),
        me: { perms, roles: s.memberRoles[user] || [], owner: P.isOwner(s, user), admin: P.isAdmin(s, user), rank: P.rank(s, user) === Infinity ? 1e9 : P.rank(s, user) },
        roles: [...s.roles].sort((a, b) => b.position - a.position), memberRoles: s.memberRoles, everyonePerms: s.everyonePerms,
        channels: sortedChannels(s).filter((ch) => sees(s, ch, user)).map((ch) => {
          const conv = s.everyone && ch.id === 'general' ? 'room' : `ch:${s.id}/${ch.id}`, mine = P.permsIn(s, ch, user);
          return { id: ch.id, name: ch.name, topic: ch.topic, category: ch.category, position: ch.position, private: isPrivate(ch), announce: ch.announce, nsfw: ch.nsfw, slowmode: ch.slowmode, createdBy: ch.createdBy,
            perms: mine, canPost: mine.includes('SEND_MESSAGES'), conv, pins: pinsOf(conv),
            ...(isPrivate(ch) ? { members: serverMembers(s).filter((u) => sees(s, ch, u)) } : {}),
            ...(manage || ch.createdBy === user ? { overwrites: ch.overwrites } : {}) };
        }),
        ...(has('CREATE_INVITE') || has('MANAGE_INVITES') ? { invites: s.invites.filter((iv) => live(iv) && (has('MANAGE_INVITES') || iv.by === user)).map(pubInvite), pending: Object.entries(s.pending).map(([u, p]) => ({ user: u, by: p.by, at: p.at })) } : {}),
        ...(has('BAN_MEMBERS') ? { bans: s.bans.map((b) => ({ user: b.user, by: b.by, at: b.at, reason: b.reason || '' })) } : {}),
        ...(has('VIEW_AUDIT_LOG') ? { audit: s.audit.slice(-100).reverse() } : {}),
      };
    });
    const invitations = db.servers.filter((s) => !s.everyone && s.pending[user] && !inServer(s, user)).map((s) => ({ server: s.id, name: s.name, icon: s.icon, color: s.color, by: s.pending[user].by, at: s.pending[user].at, members: serverMembers(s).length }));
    const groups = db.groups.filter((g) => g.members.includes(user)).map((g) => ({ id: g.id, name: g.name, members: g.members.filter((u) => accounts.has(u)), owner: g.owner, createdBy: g.createdBy, conv: 'grp:' + g.id, pins: pinsOf('grp:' + g.id) }));
    const dmPins = Object.fromEntries(Object.keys(db.pins).filter((c) => c.startsWith('dm:') && c.slice(3).split('|').includes(user)).map((c) => [c, pinsOf(c)]));
    return { ver, servers, groups, invitations, dmPins, agent: agentName, perms: P.PERMS, categories: P.CATEGORIES.map(([n, p]) => [n, Object.keys(p)]), channelPerms: P.CHANNEL_PERMS,
      templates: P.TEMPLATES, groupMax: GROUP_MAX, inviteAges: INVITE_AGES, timeouts: TIMEOUTS, slowmodes: SLOWMODES, banDelete: BAN_DELETE, waits: WAITS };
  }
  const cleanName = (n, max = 40) => String(n || '').replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, max);
  const userList = (list) => [...new Set((Array.isArray(list) ? list : String(list || '').split(/[,\s]+/)).map((u) => String(u).trim().toLowerCase()).filter((u) => accounts.has(u)))];
  const cleanMembers = (list, user) => [...new Set([user, ...userList(list)])];
  const nameOf = (u) => { const a = accounts.get(u); return a ? a.display || u : u; };

  // ---- guards ----
  function member(id, user) { const s = server(id); if (!s) throw new Error('No such server.'); if (!inServer(s, user)) throw new Error('You are not in that server.'); return s; }
  const permName = (p) => p.toLowerCase().replace(/_/g, ' ');
  function need(id, user, perm) { const s = member(id, user); if (!P.can(s, user, perm)) throw new Error(`You need the "${permName(perm)}" permission in ${s.name}.`); return s; }
  function owned(id, user) { const s = member(id, user); if (s.everyone) throw new Error('The Mills server is everyone\'s.'); if (s.owner !== user) throw new Error('Only the server\'s owner may do that.'); return s; }
  const notEveryone = (s, what) => { if (s.everyone) throw new Error(`Everyone is always in the Mills server, so it has no ${what}.`); };
  function inGroup(id, user) { const g = group(id); if (!g || !g.members.includes(user)) throw new Error('No such group (or you are not in it).'); return g; }
  function target(s, user, who, perm) {
    who = String(who || '').toLowerCase();
    if (!accounts.has(who)) throw new Error('No such person.');
    if (who === user) throw new Error('Not on yourself.');
    if (P.isOwner(s, who)) throw new Error('Not on the server\'s owner.');
    if (perm && !P.can(s, user, perm)) throw new Error(`You need the "${permName(perm)}" permission.`);
    if (!P.outranks(s, user, who)) throw new Error(`${nameOf(who)} is not below you in ${s.name}'s roles.`);
    return who;
  }
  const sysSay = (s, text, key) => { // welcome channel messages ("Mills" posts, sealed by the hub like Unity's)
    if (s.everyone || !s.settings.systemChannel || !s.settings[key]) return;
    const ch = s.channels.find((c) => c.id === s.settings.systemChannel); if (!ch) return;
    try { announce(`ch:${s.id}/${ch.id}`, text); } catch {}
  };
  function removeFrom(s, who) { s.members = s.members.filter((u) => u !== who); delete s.memberRoles[who]; delete s.nicks[who]; delete s.temp[who]; delete s.pending[who]; delete s.joined[who]; delete s.voiceMuted[who]; }
  function join(s, who, how, temporary) {
    if (s.bans.some((b) => b.user === who)) throw new Error('You are banned from ' + s.name + '.');
    if (inServer(s, who)) return s;
    if (!s.members.includes(who)) s.members.push(who);
    delete s.pending[who];
    s.joined[who] = { at: Date.now(), seq: getSeq() };
    if (temporary) s.temp[who] = Date.now();
    else { const m = memberRole(s); if (m) s.memberRoles[who] = [...new Set([m.id, ...(s.memberRoles[who] || [])])]; } // everyone who joins is a Member
    audit(s, who, 'joined', how + (temporary ? ' (temporary)' : ''));
    sysSay(s, `${nameOf(who)} joined ${s.name}. Welcome!` + (s.settings.rulesChannel ? ` Read #${(s.channels.find((c) => c.id === s.settings.rulesChannel) || {}).name || 'rules'} first.` : ''), 'systemJoins');
    return s;
  }

  // ---- invite codes ----
  const live = (iv) => (!iv.expiresAt || Date.parse(iv.expiresAt) > Date.now()) && (!iv.maxUses || iv.uses < iv.maxUses);
  const pubInvite = (iv) => ({ code: iv.code, by: iv.by, at: iv.at, expiresAt: iv.expiresAt || '', maxUses: iv.maxUses || 0, uses: iv.uses, temporary: !!iv.temporary });
  const findInvite = (code) => { code = String(code || '').trim().replace(/^.*[?&]invite=/, '').replace(/[^A-Za-z0-9]/g, ''); for (const s of db.servers) { const iv = s.invites.find((x) => x.code === code); if (iv) return { s, iv }; } return null; };
  const pwTries = new Map(); // `${user}|${server}` -> [timestamps]
  function checkPassword(s, user, pw) {
    if (!s.password) return;
    const k = user + '|' + s.id, now = Date.now(), list = (pwTries.get(k) || []).filter((t) => now - t < PW_WINDOW);
    if (list.length >= PW_TRIES) throw new Error('Too many wrong passwords for ' + s.name + '. Try again in a few minutes.');
    const got = crypto.scryptSync(String(pw || ''), Buffer.from(s.password.salt, 'base64'), 32);
    if (!crypto.timingSafeEqual(got, Buffer.from(s.password.hash, 'base64'))) { list.push(now); pwTries.set(k, list); throw new Error(pw ? 'Wrong server password.' : s.name + ' is password locked: enter its password.'); }
    pwTries.delete(k);
  }
  function maybeDrop(s) { if (!s.everyone && !s.members.length) db.servers.splice(db.servers.indexOf(s), 1); }
  function newRole(s, user, b) {
    if (s.roles.length >= ROLE_MAX) throw new Error('That server has enough roles.');
    const tpl = b.template && P.TEMPLATES[b.template];
    const name = cleanName(b.name || (tpl && tpl.name), 32); if (!name) throw new Error('A role needs a name.');
    const perms = P.cleanPerms(b.perms !== undefined ? b.perms : tpl ? tpl.perms : []);
    if (!P.grantable(s, user, perms)) throw new Error('You can only give a role permissions you have yourself.');
    // a new custom role starts just above Member (like Discord: new roles go to the bottom), never on a built-in's spot
    const myRank = P.rank(s, user);
    let position = Math.min(myRank - 1, 11);
    while (position > 0 && isBuiltinPos(position)) position--;
    if (!(position >= 1) || !(myRank > position)) throw new Error('You can only make roles below your own.');
    const r = { id: crypto.randomBytes(4).toString('hex'), name, color: COLOR_RE.test(b.color || (tpl && tpl.color) || '') ? b.color || tpl.color : '', perms, position, hoist: b.hoist !== false, mentionable: !!b.mentionable, selfAssign: false };
    if (b.selfAssign) { safeForSelf(perms); r.selfAssign = true; }
    s.roles.push(r); return r;
  }
  // Self-assignable roles ("pick your roles", like Discord's onboarding / reaction roles): members give them to
  // themselves. They can carry only harmless permissions, so nobody climbs by picking one; channel overwrites can
  // still open opt-in channels to them.
  const SELF_SAFE = ['VIEW_CHANNEL', 'CHANGE_NICKNAME', 'SEND_MESSAGES', 'SEND_VOICE_MESSAGES', 'READ_MESSAGE_HISTORY', 'CONNECT', 'SPEAK', 'STREAM'];
  function safeForSelf(perms) { const bad = perms.filter((p) => !SELF_SAFE.includes(p)); if (bad.length) throw new Error('A self-assignable role may only carry everyday permissions (not: ' + bad.map(permName).join(', ') + ').'); }
  function cleanOverwrites(s, user, list) {
    const out = [];
    for (const o of Array.isArray(list) ? list : []) {
      const type = o.type === 'member' ? 'member' : 'role', id = String(o.id || '').toLowerCase();
      if (type === 'role' && id !== 'everyone' && !s.roles.some((r) => r.id === id)) continue;
      if (type === 'member' && !inServer(s, id)) continue;
      const allow = P.cleanPerms(o.allow, P.CHANNEL_PERMS), deny = P.cleanPerms(o.deny, P.CHANNEL_PERMS).filter((p) => !allow.includes(p));
      if (!P.grantable(s, user, [...allow, ...deny])) throw new Error('You can only allow or deny permissions you have yourself.');
      if (allow.length || deny.length) out.push({ type, id, allow, deny });
    }
    return out;
  }
  function privateOverwrites(roles) { // the "private to these roles" shortcut
    const ids = [...new Set((Array.isArray(roles) ? roles : []).map(String))];
    return ids.length ? [{ type: 'role', id: 'everyone', allow: [], deny: ['VIEW_CHANNEL'] }, ...ids.map((id) => ({ type: 'role', id, allow: ['VIEW_CHANNEL'], deny: [] }))] : [];
  }
  const cleanSettings = (s, b) => {
    const st = s.settings, chId = (v) => (v === '' || s.channels.some((c) => c.id === v) ? v : st.systemChannel);
    if (b.systemChannel !== undefined) st.systemChannel = chId(String(b.systemChannel || ''));
    if (b.rulesChannel !== undefined) st.rulesChannel = s.channels.some((c) => c.id === b.rulesChannel) ? b.rulesChannel : '';
    if (b.systemJoins !== undefined) st.systemJoins = !!b.systemJoins;
    if (b.systemLeaves !== undefined) st.systemLeaves = !!b.systemLeaves;
    if (b.notify !== undefined) st.notify = b.notify === 'mentions' ? 'mentions' : 'all';
    if (b.invitesPaused !== undefined) st.invitesPaused = !!b.invitesPaused;
    if (b.wait !== undefined) st.wait = WAITS.includes(Number(b.wait)) ? Number(b.wait) : 0;
  };

  const ops = {
    // ---- servers ----
    createServer(user, b) {
      const name = cleanName(b.name); if (!name) throw new Error('A server needs a name.');
      if (db.servers.filter((s) => s.owner === user).length >= SERVER_MAX_PER_USER) throw new Error('You own enough servers already.');
      let id = slug(name) || 's'; if (id === 'mills' || db.servers.some((s) => s.id === id)) id = (id.slice(0, 27) + '-' + crypto.randomBytes(2).toString('hex'));
      const s = normServer({ id, name, icon: cleanName(b.icon, 2), color: b.color, description: cleanName(b.description, 300), channels: [{ id: 'general', name: 'general', topic: '' }], members: [user], owner: user, createdBy: user, createdAt: new Date().toISOString(), permsVersion: 2 });
      s.joined[user] = { at: Date.now(), seq: 0 }; s.memberRoles[user] = [memberRole(s).id]; // the built-in levels came with normServer
      audit(s, user, 'created the server');
      for (const u of userList(b.members)) if (u !== user) { s.pending[u] = { by: user, at: new Date().toISOString() }; audit(s, user, 'invited', nameOf(u)); }
      db.servers.push(s); save(); return s;
    },
    updateServer(user, b) {
      const s = need(b.server, user, 'MANAGE_SERVER'); const ch = [];
      if (b.name !== undefined) { const n = cleanName(b.name); if (n && n !== s.name) { s.name = n; ch.push('name'); } }
      if (b.icon !== undefined) { s.icon = cleanName(b.icon, 2); ch.push('icon'); }
      if (b.color !== undefined) { s.color = COLOR_RE.test(b.color) ? b.color : ''; ch.push('colour'); }
      if (b.description !== undefined) { s.description = cleanName(b.description, 300); ch.push('description'); }
      if (b.settings && typeof b.settings === 'object') { if (s.everyone && (b.settings.invitesPaused || b.settings.systemChannel)) throw new Error('The Mills server has no invites or join messages.'); cleanSettings(s, b.settings); ch.push('settings'); }
      if (b.everyonePerms !== undefined) {
        if (!P.can(s, user, 'MANAGE_ROLES')) throw new Error('You need the "manage roles" permission.');
        const perms = P.cleanPerms(b.everyonePerms).filter((p) => p !== 'ADMINISTRATOR');
        const changedPerms = [...perms.filter((p) => !s.everyonePerms.includes(p)), ...s.everyonePerms.filter((p) => !perms.includes(p))];
        if (!P.grantable(s, user, changedPerms)) throw new Error('You can only change permissions you have yourself.');
        s.everyonePerms = perms; ch.push('@everyone permissions');
      }
      if (ch.length) audit(s, user, 'changed the server', ch.join(', '));
      save(); return s;
    },
    setPassword(user, b) {
      const s = need(b.server, user, 'MANAGE_SERVER'); notEveryone(s, 'password lock');
      const pw = String(b.password || '');
      if (!pw) { s.password = null; audit(s, user, 'removed the password lock'); }
      else {
        if (pw.length < 4 || pw.length > 200) throw new Error('A server password is 4 to 200 characters.');
        const salt = crypto.randomBytes(16); s.password = { salt: salt.toString('base64'), hash: crypto.scryptSync(pw, salt, 32).toString('base64') };
        audit(s, user, 'set a password lock');
      }
      save(); return { locked: !!s.password };
    },
    transferServer(user, b) {
      const s = owned(b.server, user); const to = String(b.user || '').toLowerCase();
      if (!inServer(s, to) || to === user) throw new Error('Hand it to another member of the server.');
      const co = s.roles.find((r) => r.builtin === 'coowner');
      s.owner = to; delete s.temp[to]; delete s.timeouts[to]; delete s.voiceMuted[to];
      if (co) { s.memberRoles[to] = (s.memberRoles[to] || []).filter((id) => id !== co.id); if (b.keepCoOwner !== false) s.memberRoles[user] = [...new Set([...(s.memberRoles[user] || []), co.id])]; } // the old owner stays on as Co-owner
      audit(s, user, 'handed ownership to', nameOf(to) + (co && b.keepCoOwner !== false ? ' (stays on as Co-owner)' : '')); save(); return s;
    },
    leaveServer(user, b) {
      const s = member(b.server, user); notEveryone(s, 'leaving');
      if (s.owner === user) throw new Error('Hand the server to someone else first (or delete it).');
      removeFrom(s, user); audit(s, user, 'left'); sysSay(s, `${nameOf(user)} left ${s.name}.`, 'systemLeaves'); maybeDrop(s); save(); return true;
    },
    deleteServer(user, b) { const s = owned(b.server, user); db.servers.splice(db.servers.indexOf(s), 1); for (const c of Object.keys(db.pins)) if (c.startsWith('ch:' + s.id + '/')) delete db.pins[c]; save(); return true; },
    // ---- people ----
    invitePeople(user, b) { // personal invitations: they Accept or Decline
      const s = need(b.server, user, 'CREATE_INVITE'); notEveryone(s, 'invitations');
      const list = userList(Array.isArray(b.add) ? b.add : [b.add]).filter((u) => !inServer(s, u));
      if (!list.length) throw new Error('Nobody new to invite (they are in already, or no such person).');
      for (const u of list) { if (s.bans.some((x) => x.user === u)) throw new Error(nameOf(u) + ' is banned from ' + s.name + '.'); s.pending[u] = { by: user, at: new Date().toISOString() }; audit(s, user, 'invited', nameOf(u)); }
      save(); return { invited: list };
    },
    cancelInvitation(user, b) { const s = need(b.server, user, 'CREATE_INVITE'); const u = String(b.user || '').toLowerCase(); if (s.pending[u] && (s.pending[u].by === user || P.can(s, user, 'MANAGE_INVITES'))) { delete s.pending[u]; audit(s, user, 'cancelled the invitation of', nameOf(u)); save(); } return true; },
    acceptInvitation(user, b) { const s = server(b.server); if (!s || !s.pending[user]) throw new Error('No invitation to that server.'); join(s, user, 'invited by ' + nameOf(s.pending[user].by)); save(); return s; },
    declineInvitation(user, b) { const s = server(b.server); if (s && s.pending[user]) { delete s.pending[user]; save(); } return true; },
    kick(user, b) {
      const s = member(b.server, user); notEveryone(s, 'kicking'); const who = target(s, user, b.user, 'KICK_MEMBERS');
      if (!inServer(s, who)) throw new Error('They are not in the server.');
      removeFrom(s, who); audit(s, user, 'kicked', nameOf(who) + (b.reason ? ': ' + cleanName(b.reason, 120) : '')); sysSay(s, `${nameOf(who)} left ${s.name}.`, 'systemLeaves'); save(); return true;
    },
    ban(user, b) {
      const s = member(b.server, user); notEveryone(s, 'bans'); const who = target(s, user, b.user, 'BAN_MEMBERS');
      const del = BAN_DELETE.includes(Number(b.deleteSeconds)) ? Number(b.deleteSeconds) : 0;
      removeFrom(s, who); s.bans = s.bans.filter((x) => x.user !== who); s.bans.push({ user: who, by: user, at: new Date().toISOString(), reason: cleanName(b.reason, 120) });
      const gone = del ? deleteRecent(who, s.channels.map((c) => `ch:${s.id}/${c.id}`), Date.now() - del * 1000) : 0;
      audit(s, user, 'banned', nameOf(who) + (b.reason ? ': ' + cleanName(b.reason, 120) : '') + (gone ? ` (${gone} recent message${gone === 1 ? '' : 's'} deleted)` : ''));
      save(); return { deleted: gone };
    },
    unban(user, b) { const s = need(b.server, user, 'BAN_MEMBERS'); const who = String(b.user || '').toLowerCase(); s.bans = s.bans.filter((x) => x.user !== who); audit(s, user, 'unbanned', nameOf(who)); save(); return true; },
    timeout(user, b) {
      const s = member(b.server, user); const who = target(s, user, b.user, 'MODERATE_MEMBERS');
      if (!inServer(s, who)) throw new Error('They are not in the server.');
      if (P.isAdmin(s, who)) throw new Error('Administrators cannot be timed out.');
      const min = Number(b.minutes);
      if (!min) { delete s.timeouts[who]; audit(s, user, 'removed the timeout of', nameOf(who)); }
      else { if (!TIMEOUTS.includes(min)) throw new Error('Pick a timeout length from the list.'); s.timeouts[who] = new Date(Date.now() + min * 60e3).toISOString(); audit(s, user, 'timed out', nameOf(who) + ' for ' + (min >= 1440 ? min / 1440 + ' d' : min >= 60 ? min / 60 + ' h' : min + ' min') + (b.reason ? ': ' + cleanName(b.reason, 120) : '')); }
      save(); return { until: s.timeouts[who] || '' };
    },
    voiceMute(user, b) {
      const s = member(b.server, user); const who = target(s, user, b.user, 'MUTE_MEMBERS');
      if (b.on === false) { delete s.voiceMuted[who]; audit(s, user, 'unmuted in calls', nameOf(who)); } else { s.voiceMuted[who] = true; audit(s, user, 'muted in calls', nameOf(who)); }
      save(); return true;
    },
    setNick(user, b) {
      const s = member(b.server, user); const who = String(b.user || user).toLowerCase();
      if (who !== user) target(s, user, who, 'MANAGE_NICKNAMES');
      else if (!P.can(s, user, 'CHANGE_NICKNAME')) throw new Error('You may not change your nickname in ' + s.name + '.');
      if (!inServer(s, who)) throw new Error('They are not in the server.');
      const n = cleanName(b.nick, 32); if (n) s.nicks[who] = n; else delete s.nicks[who];
      if (who !== user) audit(s, user, 'changed the nickname of', nameOf(who)); save(); return true;
    },
    setRoles(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); const who = String(b.user || '').toLowerCase();
      if (!inServer(s, who)) throw new Error('They are not in the server.');
      if (who !== user && !P.isOwner(s, user)) target(s, user, who);
      const want = [...new Set((Array.isArray(b.roles) ? b.roles : []).map(String))].filter((id) => s.roles.some((r) => r.id === id));
      const before = s.memberRoles[who] || [];
      for (const id of [...want.filter((x) => !before.includes(x)), ...before.filter((x) => !want.includes(x))]) { // only roles below you change hands
        const r = s.roles.find((x) => x.id === id); if (r && !P.roleBelow(s, user, r)) throw new Error(`The role "${r.name}" is not below your own.`);
      }
      if (want.length) s.memberRoles[who] = want; else delete s.memberRoles[who];
      if (realRoles(s, who).length || want.includes((memberRole(s) || {}).id)) delete s.temp[who]; // any role (Member included, when handed out) makes a temporary member permanent
      audit(s, user, 'changed the roles of', nameOf(who) + ': ' + (want.map((id) => s.roles.find((r) => r.id === id).name).join(', ') || 'none'));
      save(); return true;
    },
    selfRole(user, b) { // pick / drop a self-assignable role (no permission needed beyond being a member)
      const s = member(b.server, user); const r = s.roles.find((x) => x.id === b.role);
      if (!r || !r.selfAssign) throw new Error('That role is not one you can pick yourself.');
      if (P.timedOut(s, user)) throw new Error('Not while you are timed out.');
      const cur = s.memberRoles[user] || [];
      s.memberRoles[user] = b.on === false ? cur.filter((id) => id !== r.id) : [...new Set([...cur, r.id])];
      if (!s.memberRoles[user].length) delete s.memberRoles[user];
      save(); return true;
    },
    // ---- roles ----
    createRole(user, b) { const s = need(b.server, user, 'MANAGE_ROLES'); const r = newRole(s, user, b); audit(s, user, 'made the role', r.name); save(); return r; },
    updateRole(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); const r = s.roles.find((x) => x.id === b.role); if (!r) throw new Error('No such role.');
      if (!P.roleBelow(s, user, r)) throw new Error('That role is not below your own.');
      if (b.name !== undefined) r.name = cleanName(b.name, 32) || r.name;
      if (b.color !== undefined) r.color = COLOR_RE.test(b.color || '') ? b.color : '';
      if (b.hoist !== undefined) r.hoist = !!b.hoist;
      if (b.mentionable !== undefined) r.mentionable = !!b.mentionable;
      if (b.selfAssign !== undefined) { if (b.selfAssign && r.builtin) throw new Error('Built-in levels are handed out, not picked.'); if (b.selfAssign) safeForSelf(b.perms !== undefined ? P.cleanPerms(b.perms) : r.perms); r.selfAssign = !!b.selfAssign; }
      if (b.perms !== undefined && r.selfAssign) safeForSelf(P.cleanPerms(b.perms));
      if (b.perms !== undefined && (r.builtin === 'coowner' || r.builtin === 'superadmin')) throw new Error(r.name + ' always has every permission.');
      if (b.position !== undefined && r.builtin) throw new Error('The built-in levels keep their order; move your own roles between them.');
      if (b.perms !== undefined) {
        const perms = P.cleanPerms(b.perms), diff = [...perms.filter((p) => !r.perms.includes(p)), ...r.perms.filter((p) => !perms.includes(p))];
        if (!P.grantable(s, user, diff)) throw new Error('You can only change permissions you have yourself.');
        r.perms = perms;
      }
      if (b.position !== undefined) {
        let pos = Math.max(1, Math.min(P.COOWNER_POS - 1, Number(b.position) | 0)); const dir = pos > r.position ? 1 : -1;
        while (isBuiltinPos(pos)) pos += dir; // hop over a built-in level
        if (pos < 1 || pos >= P.COOWNER_POS) throw new Error('A custom role stays between Member and Co-owner.');
        if (!(P.rank(s, user) > pos)) throw new Error('A role can only move below your own.');
        r.position = pos;
      }
      audit(s, user, 'changed the role', r.name); save(); return r;
    },
    deleteRole(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); const r = s.roles.find((x) => x.id === b.role); if (!r) throw new Error('No such role.');
      if (r.builtin) throw new Error(r.name + ' is a built-in level; it stays (rename or re-permission it instead).');
      if (!P.roleBelow(s, user, r)) throw new Error('That role is not below your own.');
      s.roles.splice(s.roles.indexOf(r), 1);
      for (const u of Object.keys(s.memberRoles)) { s.memberRoles[u] = s.memberRoles[u].filter((id) => id !== r.id); if (!s.memberRoles[u].length) delete s.memberRoles[u]; }
      for (const ch of s.channels) ch.overwrites = ch.overwrites.filter((o) => !(o.type === 'role' && o.id === r.id));
      audit(s, user, 'deleted the role', r.name); save(); return true;
    },
    // ---- invite codes ----
    createInvite(user, b) {
      const s = need(b.server, user, 'CREATE_INVITE'); notEveryone(s, 'invite links');
      if (s.settings.invitesPaused) throw new Error('Invites to ' + s.name + ' are paused.');
      if (s.invites.filter(live).length >= INVITE_MAX) throw new Error('That server has enough open invites; revoke some.');
      const age = INVITE_AGES.includes(Number(b.maxAge)) ? Number(b.maxAge) : 1440;
      const maxUses = [0, 1, 5, 10, 25, 100].includes(Number(b.maxUses)) ? Number(b.maxUses) : 0;
      const iv = { code: crypto.randomBytes(6).toString('base64').replace(/[^A-Za-z0-9]/g, '').slice(0, 8) + crypto.randomBytes(2).toString('hex'), by: user, at: new Date().toISOString(), expiresAt: age ? new Date(Date.now() + age * 60e3).toISOString() : '', maxUses, uses: 0, temporary: !!b.temporary };
      s.invites = s.invites.filter(live); s.invites.push(iv);
      audit(s, user, 'made an invite', (age ? 'expires in ' + (age >= 1440 ? age / 1440 + ' d' : age >= 60 ? age / 60 + ' h' : age + ' min') : 'never expires') + (maxUses ? ', ' + maxUses + ' uses' : '') + (iv.temporary ? ', temporary' : ''));
      save(); return pubInvite(iv);
    },
    revokeInvite(user, b) {
      const f = findInvite(b.code); if (!f) throw new Error('No such invite.');
      if (!inServer(f.s, user) || !(f.iv.by === user || P.can(f.s, user, 'MANAGE_INVITES'))) throw new Error('Only who made the invite (or someone who manages invites) revokes it.');
      f.s.invites.splice(f.s.invites.indexOf(f.iv), 1); audit(f.s, user, 'revoked an invite'); save(); return true;
    },
    previewInvite(user, b) {
      const f = findInvite(b.code); if (!f || !live(f.iv)) throw new Error('That invite is invalid or has expired.');
      const s = f.s;
      return { server: s.id, name: s.name, icon: s.icon, color: s.color, description: s.description, members: serverMembers(s).length, locked: !!s.password, paused: !!s.settings.invitesPaused, temporary: !!f.iv.temporary, by: f.iv.by, already: inServer(s, user), banned: s.bans.some((x) => x.user === user) };
    },
    joinByCode(user, b) {
      const f = findInvite(b.code); if (!f || !live(f.iv)) throw new Error('That invite is invalid or has expired.');
      const s = f.s; if (inServer(s, user)) return s;
      if (s.settings.invitesPaused) throw new Error('Invites to ' + s.name + ' are paused right now.');
      if (s.bans.some((x) => x.user === user)) throw new Error('You are banned from ' + s.name + '.');
      checkPassword(s, user, b.password);
      f.iv.uses++; join(s, user, 'with an invite from ' + nameOf(f.iv.by), f.iv.temporary);
      if (!live(f.iv)) s.invites.splice(s.invites.indexOf(f.iv), 1);
      save(); return s;
    },
    // ---- channels ----
    createChannel(user, b) {
      const s = member(b.server, user);
      if (!P.can(s, user, 'CREATE_CHANNELS') && !P.can(s, user, 'MANAGE_CHANNELS')) throw new Error('You may not make channels in ' + s.name + '.');
      const id = slug(b.name); if (!id) throw new Error('A channel needs a name (letters, digits, dashes).');
      if (s.channels.some((c) => c.id === id)) throw new Error('That channel exists already.');
      if (s.channels.length >= CHANNEL_MAX) throw new Error('That server has enough channels.');
      const ch = normChannel({ id, name: id, topic: cleanName(b.topic, 120), category: cleanName(b.category, 32), position: Math.max(0, ...s.channels.map((c) => c.position)) + 1, announce: !!b.announce, nsfw: !!b.nsfw, slowmode: Number(b.slowmode) || 0, createdBy: user }, 0, s);
      ch.overwrites = b.overwrites !== undefined ? cleanOverwrites(s, user, b.overwrites) : cleanOverwrites(s, user, privateOverwrites(b.roles));
      s.channels.push(ch);
      if (!sees(s, ch, user)) { s.channels.pop(); throw new Error('Those permissions would hide the channel from you; give one of your roles access too.'); }
      audit(s, user, 'made the channel', '#' + id); save(); return ch;
    },
    updateChannel(user, b) {
      const s = member(b.server, user); const ch = s.channels.find((c) => c.id === b.channel); if (!ch) throw new Error('No such channel.');
      const manage = P.canIn(s, ch, user, 'MANAGE_CHANNELS'), own = ch.createdBy === user && P.can(s, user, 'CREATE_CHANNELS');
      if (!manage && !own) throw new Error('Only who made the channel (or someone who manages channels) changes it.');
      const before = JSON.stringify(ch);
      if (b.name !== undefined) ch.name = slug(b.name) || ch.name; // the id (and so the conversation) stays; only the shown name changes
      if (b.topic !== undefined) ch.topic = cleanName(b.topic, 120);
      if (b.category !== undefined) ch.category = cleanName(b.category, 32);
      if (b.announce !== undefined) ch.announce = !!b.announce;
      if (b.nsfw !== undefined) ch.nsfw = !!b.nsfw;
      if (b.slowmode !== undefined) ch.slowmode = SLOWMODES.includes(Number(b.slowmode)) ? Number(b.slowmode) : 0;
      if (b.overwrites !== undefined || b.roles !== undefined) {
        if (s.everyone && ch.id === 'general') throw new Error('#general is everyone\'s; its permissions stay.');
        if (!manage && !P.canIn(s, ch, user, 'MANAGE_ROLES') && !own) throw new Error('You need "manage channels" or "manage roles" to change channel permissions.');
        const next = b.overwrites !== undefined ? cleanOverwrites(s, user, b.overwrites) : cleanOverwrites(s, user, privateOverwrites(b.roles));
        const prev = ch.overwrites; ch.overwrites = next;
        if (!sees(s, ch, user)) { ch.overwrites = prev; throw new Error('That would hide the channel from you; keep access for one of your roles or yourself.'); }
      }
      if (JSON.stringify(ch) !== before) audit(s, user, 'changed the channel', '#' + ch.name);
      save(); return ch;
    },
    moveChannel(user, b) {
      const s = need(b.server, user, 'MANAGE_CHANNELS'); const list = sortedChannels(s); const i = list.findIndex((c) => c.id === b.channel);
      if (i < 0) throw new Error('No such channel.');
      const j = Math.max(0, Math.min(list.length - 1, i + (b.dir === 'up' ? -1 : 1)));
      if (j !== i) { list.splice(j, 0, list.splice(i, 1)[0]); list.forEach((c, k) => { c.position = k; }); save(); }
      return true;
    },
    deleteChannel(user, b) {
      const s = member(b.server, user); const i = s.channels.findIndex((c) => c.id === b.channel);
      if (i < 0) throw new Error('No such channel.'); if (s.channels[i].id === 'general') throw new Error('#general stays.');
      if (!(P.canIn(s, s.channels[i], user, 'MANAGE_CHANNELS') || s.channels[i].createdBy === user)) throw new Error('Only who made the channel (or someone who manages channels) deletes it.');
      audit(s, user, 'deleted the channel', '#' + s.channels[i].name);
      delete db.pins[`ch:${s.id}/${s.channels[i].id}`];
      for (const k of ['systemChannel', 'rulesChannel']) if (s.settings[k] === s.channels[i].id) s.settings[k] = '';
      s.channels.splice(i, 1); save(); return true;
    },
    // ---- pins (any conversation) ----
    pin(user, b) {
      const conv = String(b.conv || ''), seq = Number(b.seq), c = parse(conv);
      if (!c) throw new Error('Unknown conversation.');
      if (c.kind === 'dm' ? !(c.a < c.b && [c.a, c.b].includes(user)) : c.kind === 'grp' ? !isMember(conv, user) : c.kind === 'ch' ? !isMember(conv, user) : false) throw new Error('Unknown conversation.');
      if ((c.kind === 'room' || c.kind === 'ch') && !allows(conv, user, 'PIN_MESSAGES')) throw new Error('You need the "pin messages" permission here.');
      if (msgConv(seq) !== conv) throw new Error('No such message in that conversation.');
      const list = db.pins[conv] || (db.pins[conv] = []);
      if (b.on === false) db.pins[conv] = list.filter((p) => p.seq !== seq);
      else if (!list.some((p) => p.seq === seq)) { if (list.length >= PIN_MAX) throw new Error(`Up to ${PIN_MAX} pins per conversation; unpin one first.`); list.push({ seq, by: user, at: new Date().toISOString() }); }
      const f = chanOf(conv); if (f && !f.s.everyone) audit(f.s, user, b.on === false ? 'unpinned a message in' : 'pinned a message in', '#' + f.ch.name);
      save(); return { pins: db.pins[conv] };
    },
    // ---- groups ----
    createGroup(user, b) {
      const members = cleanMembers(b.members, user); if (members.length < 2) throw new Error('A group needs at least one other person.');
      if (members.length > GROUP_MAX) throw new Error(`A group holds up to ${GROUP_MAX} people.`);
      const g = { id: crypto.randomBytes(5).toString('hex'), name: cleanName(b.name) || members.map((u) => nameOf(u)).join(', ').slice(0, 40), members, owner: user, createdBy: user, createdAt: new Date().toISOString() };
      db.groups.push(g); save(); return g;
    },
    renameGroup(user, b) { const g = inGroup(b.group, user); const name = cleanName(b.name); if (name) g.name = name; save(); return g; },
    addToGroup(user, b) {
      const g = inGroup(b.group, user); const next = cleanMembers([...g.members, ...userList(Array.isArray(b.add) ? b.add : [b.add])], user);
      if (next.length > GROUP_MAX) throw new Error(`A group holds up to ${GROUP_MAX} people.`);
      g.members = next; save(); return g;
    },
    removeFromGroup(user, b) {
      const g = inGroup(b.group, user); const who = String(b.user || '').toLowerCase();
      if (g.owner !== user) throw new Error('Only the group\'s owner removes people.');
      if (who === user) throw new Error('Leave the group instead.');
      g.members = g.members.filter((u) => u !== who); save(); return g;
    },
    transferGroup(user, b) { const g = inGroup(b.group, user); const to = String(b.user || '').toLowerCase(); if (g.owner !== user) throw new Error('Only the group\'s owner hands it on.'); if (!g.members.includes(to) || to === user) throw new Error('Hand it to another member.'); g.owner = to; save(); return g; },
    leaveGroup(user, b) {
      const g = inGroup(b.group, user); g.members = g.members.filter((u) => u !== user);
      if (!g.members.length) { db.groups.splice(db.groups.indexOf(g), 1); delete db.pins['grp:' + g.id]; } else if (g.owner === user) g.owner = g.members[0];
      save(); return true;
    },
  };
  ops.renameServer = (user, b) => ops.updateServer(user, b); // older name, kept for callers
  ops.addMembers = (user, b) => ops.invitePeople(user, b);   // "add people" sends invitations
  ops.removeMember = (user, b) => (String(b.user || '').toLowerCase() === user ? ops.leaveServer(user, b) : ops.kick(user, b));
  function dropUser(user) { // an account was deleted: out of every server and group
    for (const s of db.servers) {
      if (s.everyone) { for (const k of ['memberRoles', 'nicks', 'timeouts', 'voiceMuted', 'joined']) delete s[k][user]; continue; }
      removeFrom(s, user); delete s.timeouts[user];
      if (s.owner === user) s.owner = s.members.find((u) => accounts.has(u) && u !== user) || '';
    }
    for (const g of db.groups) { g.members = g.members.filter((u) => u !== user); if (g.owner === user) g.owner = g.members[0] || ''; }
    db.servers = db.servers.filter((s) => s.everyone || s.members.length); db.groups = db.groups.filter((g) => g.members.length);
    save();
  }
  // temporary members (joined with a temporary invite, no role since) leave once they are offline; ended timeouts go
  const tempTimer = setInterval(() => {
    let any = false;
    for (const s of db.servers) {
      for (const [u, at] of Object.entries(s.temp)) {
        if (Date.now() - at < TEMP_GRACE || isOnline(u)) continue;
        if (realRoles(s, u).length) { delete s.temp[u]; any = true; continue; }
        removeFrom(s, u); audit(s, u, 'left', 'temporary membership ended (went offline)'); maybeDrop(s); any = true;
      }
      for (const [u, t] of Object.entries(s.timeouts)) if (Date.parse(t) <= Date.now()) { delete s.timeouts[u]; any = true; }
    }
    if (any) save();
  }, 20e3);
  tempTimer.unref();

  const ROUTES = {
    '/api/guilds/server': 'createServer', '/api/guilds/server/update': 'updateServer', '/api/guilds/server/rename': 'renameServer', '/api/guilds/server/password': 'setPassword',
    '/api/guilds/server/transfer': 'transferServer', '/api/guilds/server/leave': 'leaveServer', '/api/guilds/server/delete': 'deleteServer',
    '/api/guilds/server/add': 'addMembers', '/api/guilds/server/invite': 'invitePeople', '/api/guilds/server/remove': 'removeMember',
    '/api/guilds/invitation/cancel': 'cancelInvitation', '/api/guilds/invitation/accept': 'acceptInvitation', '/api/guilds/invitation/decline': 'declineInvitation',
    '/api/guilds/member/kick': 'kick', '/api/guilds/member/ban': 'ban', '/api/guilds/member/unban': 'unban', '/api/guilds/member/nick': 'setNick', '/api/guilds/member/roles': 'setRoles',
    '/api/guilds/member/timeout': 'timeout', '/api/guilds/member/mute': 'voiceMute', '/api/guilds/member/selfrole': 'selfRole',
    '/api/guilds/role': 'createRole', '/api/guilds/role/update': 'updateRole', '/api/guilds/role/delete': 'deleteRole',
    '/api/guilds/invite': 'createInvite', '/api/guilds/invite/revoke': 'revokeInvite', '/api/guilds/invite/preview': 'previewInvite', '/api/guilds/invite/join': 'joinByCode',
    '/api/guilds/channel': 'createChannel', '/api/guilds/channel/update': 'updateChannel', '/api/guilds/channel/move': 'moveChannel', '/api/guilds/channel/delete': 'deleteChannel',
    '/api/guilds/pin': 'pin',
    '/api/guilds/group': 'createGroup', '/api/guilds/group/rename': 'renameGroup', '/api/guilds/group/add': 'addToGroup', '/api/guilds/group/remove': 'removeFromGroup',
    '/api/guilds/group/transfer': 'transferGroup', '/api/guilds/group/leave': 'leaveGroup',
  };
  const pub = (r) => (r && typeof r === 'object' && 'password' in r && 'members' in r ? { id: r.id, name: r.name } : r); // a server record never leaves whole (password hash)
  async function route(p, req, res, acct) {
    if (!p.startsWith('/api/guilds')) return false;
    if (p === '/api/guilds' && req.method === 'GET') { send(res, 200, view(acct.username)); return true; }
    const op = ROUTES[p];
    if (!op || req.method !== 'POST') return false;
    try { const r = ops[op](acct.username, (await readBody(req)) || {}); send(res, 200, { ok: true, result: pub(r), ...view(acct.username) }); }
    catch (e) { send(res, 400, { error: e.message }); }
    return true;
  }
  return { parse, exists, members, isMember, isGuildConv, allows, postCheck, notePost, canRead, media, canPost: (conv, user) => !postCheck(conv, user, { edit: true }),
    moderates: (conv, user) => !!chanOf(conv) && allows(conv, user, 'MANAGE_MESSAGES'), // may delete others' messages there
    view, route, dropUser, ops, ROUTES, version: () => ver, DEFAULT_SERVER: 'mills', setOnline: (fn) => { isOnline = fn; } };
};
