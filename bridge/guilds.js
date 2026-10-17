// Mills servers ("guilds"), channels and group conversations: the Discord-shaped side of the chat.
//
// Conversation ids the hub routes by (the text inside stays end-to-end encrypted; these are metadata):
//   'room'                  #general of the everyone-server "Mills": every account, Unity included (unchanged)
//   'dm:<a>|<b>'            a private conversation between two people (unchanged, carried by `to`)
//   'ch:<server>/<channel>' a channel of a server: the server's members who may see that channel
//   'grp:<id>'              a group conversation: its members only (up to GROUP_MAX people)
// Membership decides who may receive a key slot, read, type, mark seen, call and be posted to. Unity
// (the room agent) counts as a member everywhere, so she can be @-called in any channel or group.
//
// Servers (Discord-shaped; nobody signs up here, the hub admin makes accounts, so "public" never happens):
//   - an owner (the creator; transferable), roles with colours / positions / permissions (bridge/guild-perms.js),
//     server nicknames, a description and a colour; the audit log keeps the last AUDIT_KEEP changes;
//   - joining: a personal invitation (someone invites you by name: Accept / Decline), or an invite code
//     (expiry, max uses, optional "temporary": you leave again when you go offline unless you got a role);
//   - password lock: a locked server asks every invite-CODE join for its password (a code can leak; a personal
//     invitation is the inviter vouching for you). Wrong passwords: 5 per 10 minutes per person and server.
//   - kick, ban (with reason; banned people cannot rejoin by code or invitation until unbanned);
//   - channels: categories, order, topic, display name, private (only chosen roles + owner / admins see it),
//     announcement (only ANNOUNCE / MANAGE_CHANNELS / owner post).
// The everyone-server "Mills" is a server record too (editable: name, icon, colour, description, channels, roles),
// but everyone is always in it: no invites, password, kick or ban, and the hub admins act as its owner.
// Storage: Mills/Chat/guilds.json (gitignored, next to the chat ciphertext; names, member lists, settings; the
// password only as a salted scrypt hash).
'use strict';

const path = require('path');
const crypto = require('crypto');
const createPerms = require('./guild-perms');

const ID_RE = /^[a-z0-9-]{1,32}$/;
const slug = (s) => String(s || '').toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 32);
const GROUP_MAX = 10, CHANNEL_MAX = 50, ROLE_MAX = 25, INVITE_MAX = 50, AUDIT_KEEP = 200, SERVER_MAX_PER_USER = 25;
const COLOR_RE = /^#[0-9a-f]{6}$/i;
const INVITE_AGES = [0, 30, 60, 360, 720, 1440, 10080]; // minutes; 0 = never expires
const PW_TRIES = 5, PW_WINDOW = 10 * 60e3, TEMP_GRACE = 60e3;

module.exports = function createGuilds({ dir, accounts, readJson, writeJson, agentName, changed, send, readBody, isOnline = () => true, log = () => {} }) {
  const FILE = path.join(dir, 'guilds.json');
  const P = createPerms({ isHubAdmin: (u) => !!(accounts.get(u) && accounts.get(u).admin) });
  let db = readJson(FILE, null);
  if (!db || !Array.isArray(db.servers)) db = { servers: [], groups: [] };
  if (!Array.isArray(db.groups)) db.groups = [];
  // ---- load + upgrade older records (every field a server / channel / group may lack) ----
  function normServer(s) {
    s.name = s.name || s.id; s.icon = s.icon || ''; s.color = COLOR_RE.test(s.color || '') ? s.color : ''; s.description = s.description || '';
    s.owner = s.owner || s.createdBy || ''; s.members = Array.isArray(s.members) ? s.members : [];
    s.roles = Array.isArray(s.roles) ? s.roles : []; s.memberRoles = s.memberRoles && typeof s.memberRoles === 'object' ? s.memberRoles : {};
    s.everyonePerms = Array.isArray(s.everyonePerms) ? P.cleanPerms(s.everyonePerms) : [...P.DEFAULT_EVERYONE];
    s.nicks = s.nicks && typeof s.nicks === 'object' ? s.nicks : {}; s.bans = Array.isArray(s.bans) ? s.bans : [];
    s.invites = Array.isArray(s.invites) ? s.invites : []; s.pending = s.pending && typeof s.pending === 'object' ? s.pending : {};
    s.temp = s.temp && typeof s.temp === 'object' ? s.temp : {}; s.audit = Array.isArray(s.audit) ? s.audit : []; s.password = s.password || null;
    s.channels = (Array.isArray(s.channels) ? s.channels : []).map((ch, i) => ({ id: ch.id, name: ch.name || ch.id, topic: ch.topic || '', category: ch.category || '', position: Number.isFinite(ch.position) ? ch.position : i, roles: Array.isArray(ch.roles) ? ch.roles : [], announce: !!ch.announce, createdBy: ch.createdBy || '' }));
    if (!s.channels.some((c) => c.id === 'general')) s.channels.unshift({ id: 'general', name: 'general', topic: '', category: '', position: -1, roles: [], announce: false, createdBy: '' });
    return s;
  }
  let mills = db.servers.find((s) => s.id === 'mills');
  if (!mills) { mills = { id: 'mills', name: 'Mills', everyone: true, channels: [{ id: 'general', name: 'general', topic: 'the whole team' }], createdBy: '' }; db.servers.unshift(mills); }
  mills.everyone = true;
  db.servers.forEach(normServer);
  for (const g of db.groups) { g.owner = g.owner || g.createdBy || g.members[0] || ''; }
  let ver = 1;
  const save = () => { writeJson(FILE, db); ver++; changed(); };
  const server = (id) => db.servers.find((s) => s.id === id);
  const group = (id) => db.groups.find((g) => g.id === id);
  const serverMembers = (s) => (s.everyone ? [...accounts.keys()] : s.members.filter((u) => accounts.has(u)));
  const inServer = (s, user) => serverMembers(s).includes(user);
  const audit = (s, by, what, detail) => { s.audit.push({ at: new Date().toISOString(), by, what, ...(detail ? { detail: String(detail).slice(0, 200) } : {}) }); if (s.audit.length > AUDIT_KEEP) s.audit.splice(0, s.audit.length - AUDIT_KEEP); };
  const chanVisible = (s, ch, user) => !ch.roles.length || P.can(s, user, 'ADMIN') || P.rolesOf(s, user).some((r) => ch.roles.includes(r.id));
  const sortedChannels = (s) => [...s.channels].sort((a, b) => a.position - b.position);

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
  const exists = (conv) => {
    const c = parse(conv);
    if (!c) return false;
    if (c.kind === 'room') return true;
    if (c.kind === 'dm') return c.a < c.b && accounts.has(c.a) && accounts.has(c.b);
    if (c.kind === 'ch') { const s = server(c.server); return !!(s && !(s.everyone && c.channel === 'general') && s.channels.some((ch) => ch.id === c.channel)); }
    return !!group(c.group);
  };
  function members(conv) { // the people (usernames) of a channel / group conversation; null for room / dm / unknown
    const c = parse(conv);
    if (!c) return null;
    if (c.kind === 'ch') {
      const s = server(c.server); const ch = s && s.channels.find((x) => x.id === c.channel);
      if (!ch || (s.everyone && ch.id === 'general')) return null; // Mills #general is 'room'
      return serverMembers(s).filter((u) => chanVisible(s, ch, u));
    }
    if (c.kind === 'grp') { const g = group(c.group); return g ? g.members.filter((u) => accounts.has(u)) : null; }
    return null;
  }
  const isGuildConv = (conv) => /^(ch|grp):/.test(String(conv || ''));
  function isMember(conv, user) { const list = members(conv); return !!list && list.includes(user); }
  function canPost(conv, user) { // membership + announcement channels
    if (!isMember(conv, user)) return false;
    const c = parse(conv);
    if (c.kind !== 'ch') return true;
    const s = server(c.server), ch = s.channels.find((x) => x.id === c.channel);
    return !ch.announce || P.can(s, user, 'ANNOUNCE') || P.can(s, user, 'MANAGE_CHANNELS');
  }

  // ---- what one person sees ----
  function view(user) {
    const servers = db.servers.filter((s) => inServer(s, user)).map((s) => {
      const perms = P.permsOf(s, user), has = (p) => perms.includes(p);
      return {
        id: s.id, name: s.name, icon: s.icon, color: s.color, description: s.description, everyone: !!s.everyone, owner: s.everyone ? '' : s.owner, createdBy: s.createdBy || '',
        locked: !!s.password, members: serverMembers(s), temp: Object.keys(s.temp), nicks: s.nicks, me: { perms, roles: s.memberRoles[user] || [], owner: P.isOwner(s, user), rank: P.rank(s, user) === Infinity ? 1e9 : P.rank(s, user) },
        roles: [...s.roles].sort((a, b) => b.position - a.position), memberRoles: s.memberRoles, everyonePerms: s.everyonePerms,
        channels: sortedChannels(s).filter((ch) => chanVisible(s, ch, user)).map((ch) => ({ id: ch.id, name: ch.name, topic: ch.topic, category: ch.category, position: ch.position, roles: ch.roles, private: ch.roles.length > 0, ...(ch.roles.length ? { members: serverMembers(s).filter((u) => chanVisible(s, ch, u)) } : {}), announce: ch.announce, createdBy: ch.createdBy,
          canPost: !ch.announce || has('ANNOUNCE') || has('MANAGE_CHANNELS'), conv: s.everyone && ch.id === 'general' ? 'room' : `ch:${s.id}/${ch.id}` })),
        ...(has('INVITE') || has('MANAGE_INVITES') ? { invites: s.invites.filter((iv) => live(iv) && (has('MANAGE_INVITES') || iv.by === user)).map(pubInvite) } : {}),
        ...(has('INVITE') || has('MANAGE_INVITES') ? { pending: Object.entries(s.pending).map(([u, p]) => ({ user: u, by: p.by, at: p.at })) } : {}),
        ...(has('BAN') ? { bans: s.bans.map((b) => ({ user: b.user, by: b.by, at: b.at, reason: b.reason || '' })) } : {}),
        ...(has('MANAGE_SERVER') ? { audit: s.audit.slice(-100).reverse() } : {}),
      };
    });
    const invitations = db.servers.filter((s) => !s.everyone && s.pending[user] && !inServer(s, user)).map((s) => ({ server: s.id, name: s.name, icon: s.icon, color: s.color, by: s.pending[user].by, at: s.pending[user].at, members: serverMembers(s).length }));
    const groups = db.groups.filter((g) => g.members.includes(user)).map((g) => ({ id: g.id, name: g.name, members: g.members.filter((u) => accounts.has(u)), owner: g.owner, createdBy: g.createdBy, conv: 'grp:' + g.id }));
    return { ver, servers, groups, invitations, agent: agentName, perms: P.PERMS, groupMax: GROUP_MAX, inviteAges: INVITE_AGES };
  }
  const cleanName = (n, max = 40) => String(n || '').replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, max);
  const userList = (list) => [...new Set((Array.isArray(list) ? list : String(list || '').split(/[,\s]+/)).map((u) => String(u).trim().toLowerCase()).filter((u) => accounts.has(u)))];
  const cleanMembers = (list, user) => [...new Set([user, ...userList(list)])];
  const nameOf = (u) => { const a = accounts.get(u); return a ? a.display || u : u; };

  // ---- guards ----
  function member(id, user) { const s = server(id); if (!s) throw new Error('No such server.'); if (!inServer(s, user)) throw new Error('You are not in that server.'); return s; }
  function need(id, user, perm) { const s = member(id, user); if (!P.can(s, user, perm)) throw new Error(`You need the "${perm.toLowerCase().replace(/_/g, ' ')}" permission in ${s.name}.`); return s; }
  function owned(id, user) { const s = member(id, user); if (s.everyone) throw new Error('The Mills server is everyone\'s.'); if (s.owner !== user) throw new Error('Only the server\'s owner may do that.'); return s; }
  const notEveryone = (s, what) => { if (s.everyone) throw new Error(`Everyone is always in the Mills server, so it has no ${what}.`); };
  function inGroup(id, user) { const g = group(id); if (!g || !g.members.includes(user)) throw new Error('No such group (or you are not in it).'); return g; }
  function target(s, user, who, perm) {
    who = String(who || '').toLowerCase();
    if (!accounts.has(who)) throw new Error('No such person.');
    if (who === user) throw new Error('Not on yourself.');
    if (P.isOwner(s, who)) throw new Error('Not on the server\'s owner.');
    if (!P.outranks(s, user, who)) throw new Error(`${nameOf(who)} is not below you in ${s.name}'s roles.`);
    if (perm && !P.can(s, user, perm)) throw new Error('You lack that permission.');
    return who;
  }
  function removeFrom(s, who) { s.members = s.members.filter((u) => u !== who); delete s.memberRoles[who]; delete s.nicks[who]; delete s.temp[who]; delete s.pending[who]; }
  function join(s, who, how, temporary) {
    if (s.bans.some((b) => b.user === who)) throw new Error('You are banned from ' + s.name + '.');
    if (inServer(s, who)) return s;
    if (!s.members.includes(who)) s.members.push(who);
    delete s.pending[who];
    if (temporary) s.temp[who] = Date.now();
    audit(s, who, 'joined', how + (temporary ? ' (temporary)' : ''));
    return s;
  }

  // ---- invite codes ----
  const live = (iv) => (!iv.expiresAt || Date.parse(iv.expiresAt) > Date.now()) && (!iv.maxUses || iv.uses < iv.maxUses);
  const pubInvite = (iv) => ({ code: iv.code, by: iv.by, at: iv.at, expiresAt: iv.expiresAt || '', maxUses: iv.maxUses || 0, uses: iv.uses, temporary: !!iv.temporary, channel: iv.channel || '' });
  const findInvite = (code) => { code = String(code || '').trim().replace(/^.*[?&]invite=/, '').replace(/[^A-Za-z0-9]/g, ''); for (const s of db.servers) { const iv = s.invites.find((x) => x.code === code); if (iv) return { s, iv }; } return null; };
  const pwTries = new Map(); // `${user}|${server}` -> [timestamps]
  function checkPassword(s, user, pw) {
    if (!s.password) return;
    const k = user + '|' + s.id, now = Date.now(), list = (pwTries.get(k) || []).filter((t) => now - t < PW_WINDOW);
    if (list.length >= PW_TRIES) throw new Error('Too many wrong passwords for ' + s.name + '. Try again in a few minutes.');
    const { salt, hash } = s.password;
    const got = crypto.scryptSync(String(pw || ''), Buffer.from(salt, 'base64'), 32);
    if (!crypto.timingSafeEqual(got, Buffer.from(hash, 'base64'))) { list.push(now); pwTries.set(k, list); throw new Error(pw ? 'Wrong server password.' : s.name + ' is password locked: enter its password.'); }
    pwTries.delete(k);
  }
  function maybeDrop(s) { if (!s.everyone && !s.members.length) db.servers.splice(db.servers.indexOf(s), 1); }

  const ops = {
    // ---- servers ----
    createServer(user, b) {
      const name = cleanName(b.name); if (!name) throw new Error('A server needs a name.');
      if (db.servers.filter((s) => s.owner === user).length >= SERVER_MAX_PER_USER) throw new Error('You own enough servers already.');
      let id = slug(name) || 's'; if (id === 'mills' || db.servers.some((s) => s.id === id)) id = (id.slice(0, 27) + '-' + crypto.randomBytes(2).toString('hex'));
      const s = normServer({ id, name, icon: cleanName(b.icon, 2), color: b.color, description: cleanName(b.description, 300), channels: [{ id: 'general', name: 'general', topic: '' }], members: [user], owner: user, createdBy: user, createdAt: new Date().toISOString() });
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
      if (b.everyonePerms !== undefined) { if (!P.can(s, user, 'MANAGE_ROLES')) throw new Error('You need the "manage roles" permission.'); s.everyonePerms = P.cleanPerms(b.everyonePerms).filter((p) => p !== 'ADMIN'); ch.push('@everyone permissions'); }
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
      s.owner = to; delete s.temp[to]; audit(s, user, 'handed ownership to', nameOf(to)); save(); return s;
    },
    leaveServer(user, b) {
      const s = member(b.server, user); notEveryone(s, 'leaving');
      if (s.owner === user) throw new Error('Hand the server to someone else first (or delete it).');
      removeFrom(s, user); audit(s, user, 'left'); maybeDrop(s); save(); return true;
    },
    deleteServer(user, b) { const s = owned(b.server, user); db.servers.splice(db.servers.indexOf(s), 1); save(); return true; },
    // ---- people ----
    invitePeople(user, b) { // personal invitations: they Accept or Decline
      const s = need(b.server, user, 'INVITE'); notEveryone(s, 'invitations');
      const list = userList(Array.isArray(b.add) ? b.add : [b.add]).filter((u) => !inServer(s, u));
      if (!list.length) throw new Error('Nobody new to invite (they are in already, or no such person).');
      for (const u of list) { if (s.bans.some((x) => x.user === u)) throw new Error(nameOf(u) + ' is banned from ' + s.name + '.'); s.pending[u] = { by: user, at: new Date().toISOString() }; audit(s, user, 'invited', nameOf(u)); }
      save(); return { invited: list };
    },
    cancelInvitation(user, b) { const s = need(b.server, user, 'INVITE'); const u = String(b.user || '').toLowerCase(); if (s.pending[u] && (s.pending[u].by === user || P.can(s, user, 'MANAGE_INVITES'))) { delete s.pending[u]; audit(s, user, 'cancelled the invitation of', nameOf(u)); save(); } return true; },
    acceptInvitation(user, b) { const s = server(b.server); if (!s || !s.pending[user]) throw new Error('No invitation to that server.'); join(s, user, 'invited by ' + nameOf(s.pending[user].by)); save(); return s; },
    declineInvitation(user, b) { const s = server(b.server); if (s && s.pending[user]) { delete s.pending[user]; save(); } return true; },
    kick(user, b) { const s = member(b.server, user); notEveryone(s, 'kicking'); const who = target(s, user, b.user, 'KICK'); if (!inServer(s, who)) throw new Error('They are not in the server.'); removeFrom(s, who); audit(s, user, 'kicked', nameOf(who) + (b.reason ? ': ' + cleanName(b.reason, 120) : '')); save(); return true; },
    ban(user, b) {
      const s = member(b.server, user); notEveryone(s, 'bans'); const who = target(s, user, b.user, 'BAN');
      removeFrom(s, who); s.bans = s.bans.filter((x) => x.user !== who); s.bans.push({ user: who, by: user, at: new Date().toISOString(), reason: cleanName(b.reason, 120) });
      audit(s, user, 'banned', nameOf(who) + (b.reason ? ': ' + cleanName(b.reason, 120) : '')); save(); return true;
    },
    unban(user, b) { const s = need(b.server, user, 'BAN'); const who = String(b.user || '').toLowerCase(); s.bans = s.bans.filter((x) => x.user !== who); audit(s, user, 'unbanned', nameOf(who)); save(); return true; },
    setNick(user, b) {
      const s = member(b.server, user); const who = String(b.user || user).toLowerCase();
      if (who !== user) { target(s, user, who, 'MANAGE_NICKNAMES'); }
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
      if (want.length) { s.memberRoles[who] = want; delete s.temp[who]; } else delete s.memberRoles[who]; // a role makes a temporary member permanent
      audit(s, user, 'changed the roles of', nameOf(who) + ': ' + (want.map((id) => s.roles.find((r) => r.id === id).name).join(', ') || 'none'));
      save(); return true;
    },
    // ---- roles ----
    createRole(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); if (s.roles.length >= ROLE_MAX) throw new Error('That server has enough roles.');
      const name = cleanName(b.name, 32); if (!name) throw new Error('A role needs a name.');
      const myRank = P.rank(s, user), top = Math.max(0, ...s.roles.map((r) => r.position));
      const position = myRank === Infinity ? top + 1 : Math.max(1, Math.min(myRank - 1, top + 1));
      const perms = P.cleanPerms(b.perms); if (perms.includes('ADMIN') && !P.can(s, user, 'ADMIN')) throw new Error('Only administrators make administrator roles.');
      const r = { id: crypto.randomBytes(4).toString('hex'), name, color: COLOR_RE.test(b.color || '') ? b.color : '', perms, position, hoist: b.hoist !== false };
      if (!(P.rank(s, user) > r.position)) throw new Error('You can only make roles below your own.');
      s.roles.push(r); audit(s, user, 'made the role', name); save(); return r;
    },
    updateRole(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); const r = s.roles.find((x) => x.id === b.role); if (!r) throw new Error('No such role.');
      if (!P.roleBelow(s, user, r)) throw new Error('That role is not below your own.');
      if (b.name !== undefined) r.name = cleanName(b.name, 32) || r.name;
      if (b.color !== undefined) r.color = COLOR_RE.test(b.color || '') ? b.color : '';
      if (b.hoist !== undefined) r.hoist = !!b.hoist;
      if (b.perms !== undefined) { const perms = P.cleanPerms(b.perms); if (perms.includes('ADMIN') && !P.can(s, user, 'ADMIN')) throw new Error('Only administrators make administrator roles.'); r.perms = perms; }
      if (b.position !== undefined) { const pos = Math.max(1, Number(b.position) | 0); if (!(P.rank(s, user) > pos)) throw new Error('A role can only move below your own.'); r.position = pos; }
      audit(s, user, 'changed the role', r.name); save(); return r;
    },
    deleteRole(user, b) {
      const s = need(b.server, user, 'MANAGE_ROLES'); const r = s.roles.find((x) => x.id === b.role); if (!r) throw new Error('No such role.');
      if (!P.roleBelow(s, user, r)) throw new Error('That role is not below your own.');
      s.roles.splice(s.roles.indexOf(r), 1);
      for (const u of Object.keys(s.memberRoles)) { s.memberRoles[u] = s.memberRoles[u].filter((id) => id !== r.id); if (!s.memberRoles[u].length) delete s.memberRoles[u]; }
      for (const ch of s.channels) ch.roles = ch.roles.filter((id) => id !== r.id);
      audit(s, user, 'deleted the role', r.name); save(); return true;
    },
    // ---- invite codes ----
    createInvite(user, b) {
      const s = need(b.server, user, 'INVITE'); notEveryone(s, 'invite links');
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
      return { server: s.id, name: s.name, icon: s.icon, color: s.color, description: s.description, members: serverMembers(s).length, locked: !!s.password, temporary: !!f.iv.temporary, by: f.iv.by, already: inServer(s, user), banned: s.bans.some((x) => x.user === user) };
    },
    joinByCode(user, b) {
      const f = findInvite(b.code); if (!f || !live(f.iv)) throw new Error('That invite is invalid or has expired.');
      const s = f.s; if (inServer(s, user)) return s;
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
      const roles = channelRoles(s, user, b.roles);
      const ch = { id, name: id, topic: cleanName(b.topic, 120), category: cleanName(b.category, 32), position: Math.max(0, ...s.channels.map((c) => c.position)) + 1, roles, announce: !!b.announce, createdBy: user };
      s.channels.push(ch); audit(s, user, 'made the channel', '#' + id); save(); return ch;
    },
    updateChannel(user, b) {
      const s = member(b.server, user); const ch = s.channels.find((c) => c.id === b.channel); if (!ch) throw new Error('No such channel.');
      if (!(P.can(s, user, 'MANAGE_CHANNELS') || (ch.createdBy === user && P.can(s, user, 'CREATE_CHANNELS')))) throw new Error('Only who made the channel (or someone who manages channels) changes it.');
      if (b.name !== undefined) ch.name = slug(b.name) || ch.name; // the id (and so the conversation) stays; only the shown name changes
      if (b.topic !== undefined) ch.topic = cleanName(b.topic, 120);
      if (b.category !== undefined) ch.category = cleanName(b.category, 32);
      if (b.announce !== undefined) ch.announce = !!b.announce;
      if (b.roles !== undefined) { if (s.everyone && ch.id === 'general') throw new Error('#general is everyone\'s.'); ch.roles = channelRoles(s, user, b.roles); }
      audit(s, user, 'changed the channel', '#' + ch.name); save(); return ch;
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
      if (!(P.can(s, user, 'MANAGE_CHANNELS') || s.channels[i].createdBy === user)) throw new Error('Only who made the channel (or someone who manages channels) deletes it.');
      audit(s, user, 'deleted the channel', '#' + s.channels[i].name); s.channels.splice(i, 1); save(); return true;
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
      if (!g.members.length) db.groups.splice(db.groups.indexOf(g), 1); else if (g.owner === user) g.owner = g.members[0];
      save(); return true;
    },
  };
  ops.renameServer = (user, b) => ops.updateServer(user, b); // older name, kept for callers
  ops.addMembers = (user, b) => ops.invitePeople(user, b);   // "add people" now sends invitations
  ops.removeMember = (user, b) => (String(b.user || '').toLowerCase() === user ? ops.leaveServer(user, b) : ops.kick(user, b));
  function channelRoles(s, user, roles) {
    const list = [...new Set((Array.isArray(roles) ? roles : []).map(String))].filter((id) => s.roles.some((r) => r.id === id));
    if (list.length && !P.can(s, user, 'ADMIN') && !P.rolesOf(s, user).some((r) => list.includes(r.id))) throw new Error('Give one of your own roles access too, or you lock yourself out.');
    return list;
  }
  function dropUser(user) { // an account was deleted: out of every server and group
    for (const s of db.servers) {
      if (s.everyone) { delete s.memberRoles[user]; delete s.nicks[user]; continue; }
      removeFrom(s, user);
      if (s.owner === user) s.owner = s.members.find((u) => accounts.has(u) && u !== user) || '';
    }
    for (const g of db.groups) { g.members = g.members.filter((u) => u !== user); if (g.owner === user) g.owner = g.members[0] || ''; }
    db.servers = db.servers.filter((s) => s.everyone || s.members.length); db.groups = db.groups.filter((g) => g.members.length);
    save();
  }
  // temporary members (joined with a temporary invite, no role since) leave once they are offline
  const tempTimer = setInterval(() => {
    let any = false;
    for (const s of db.servers) for (const [u, at] of Object.entries(s.temp)) {
      if (Date.now() - at < TEMP_GRACE || isOnline(u)) continue;
      if ((s.memberRoles[u] || []).length) { delete s.temp[u]; any = true; continue; }
      removeFrom(s, u); audit(s, u, 'left', 'temporary membership ended (went offline)'); maybeDrop(s); any = true;
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
    '/api/guilds/role': 'createRole', '/api/guilds/role/update': 'updateRole', '/api/guilds/role/delete': 'deleteRole',
    '/api/guilds/invite': 'createInvite', '/api/guilds/invite/revoke': 'revokeInvite', '/api/guilds/invite/preview': 'previewInvite', '/api/guilds/invite/join': 'joinByCode',
    '/api/guilds/channel': 'createChannel', '/api/guilds/channel/update': 'updateChannel', '/api/guilds/channel/move': 'moveChannel', '/api/guilds/channel/delete': 'deleteChannel',
    '/api/guilds/group': 'createGroup', '/api/guilds/group/rename': 'renameGroup', '/api/guilds/group/add': 'addToGroup', '/api/guilds/group/remove': 'removeFromGroup',
    '/api/guilds/group/transfer': 'transferGroup', '/api/guilds/group/leave': 'leaveGroup',
  };
  async function route(p, req, res, acct) {
    if (!p.startsWith('/api/guilds')) return false;
    if (p === '/api/guilds' && req.method === 'GET') { send(res, 200, view(acct.username)); return true; }
    const op = ROUTES[p];
    if (!op || req.method !== 'POST') return false;
    try { const r = ops[op](acct.username, (await readBody(req)) || {}); send(res, 200, { ok: true, result: r && r.password !== undefined ? { id: r.id, name: r.name } : r, ...view(acct.username) }); }
    catch (e) { send(res, 400, { error: e.message }); }
    return true;
  }
  return { parse, exists, members, isMember, canPost, isGuildConv, view, route, dropUser, ops, ROUTES, version: () => ver, DEFAULT_SERVER: 'mills', setOnline: (fn) => { isOnline = fn; } };
};
