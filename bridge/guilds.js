// Mills servers ("guilds"), channels and group conversations: the Discord-shaped side of the chat.
//
// Conversation ids the hub routes by (the text inside stays end-to-end encrypted; these are metadata):
//   'room'                 #general of the default "Mills" server: every account, Unity included (unchanged)
//   'dm:<a>|<b>'           a private conversation between two people (unchanged, carried by `to`)
//   'ch:<server>/<channel>' a channel of a server: its members only
//   'grp:<id>'             a group conversation: its members only
// Membership decides who may receive a key slot, read, type, mark seen, call and be posted to. Unity
// (the room agent) counts as a member everywhere, so she can be @-called in any channel or group.
// Storage: Mills/Chat/guilds.json (gitignored, next to the chat ciphertext; names and member lists only).
'use strict';

const path = require('path');
const crypto = require('crypto');

const ID_RE = /^[a-z0-9-]{1,32}$/;
const slug = (s) => String(s || '').toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 32);

module.exports = function createGuilds({ dir, accounts, readJson, writeJson, agentName, changed, send, readBody }) {
  const FILE = path.join(dir, 'guilds.json');
  const DEFAULT = { id: 'mills', name: 'Mills', channels: [{ id: 'general', name: 'general', topic: 'the whole team' }], members: [], everyone: true, createdBy: '' };
  let db = readJson(FILE, null);
  if (!db || !Array.isArray(db.servers)) db = { servers: [], groups: [] };
  if (!Array.isArray(db.groups)) db.groups = [];
  let ver = 1;
  const save = () => { writeJson(FILE, db); ver++; changed(); };
  const server = (id) => (id === DEFAULT.id ? DEFAULT : db.servers.find((s) => s.id === id));
  const group = (id) => db.groups.find((g) => g.id === id);
  const serverMembers = (s) => (s.everyone ? [...accounts.keys()] : s.members.filter((u) => accounts.has(u)));

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
    if (c.kind === 'ch') { const s = server(c.server); return !!(s && s.channels.some((ch) => ch.id === c.channel)); }
    return !!group(c.group);
  };
  function members(conv) { // the people (usernames) of a channel / group conversation; null for room / dm / unknown
    const c = parse(conv);
    if (!c) return null;
    if (c.kind === 'ch') { const s = server(c.server); return s && s.channels.some((ch) => ch.id === c.channel) ? serverMembers(s) : null; }
    if (c.kind === 'grp') { const g = group(c.group); return g ? g.members.filter((u) => accounts.has(u)) : null; }
    return null;
  }
  const isGuildConv = (conv) => /^(ch|grp):/.test(String(conv || ''));
  function isMember(conv, user) { // for channel / group conversations only (room and dm are judged by the caller as before)
    const list = members(conv);
    return !!list && list.includes(user);
  }
  function view(user) { // what this person sees: servers they are in (with channels) and their groups
    const servers = [DEFAULT, ...db.servers].filter((s) => serverMembers(s).includes(user)).map((s) => ({ id: s.id, name: s.name, icon: s.icon || '', everyone: !!s.everyone, createdBy: s.createdBy || '',
      members: serverMembers(s), channels: s.channels.map((ch) => ({ id: ch.id, name: ch.name, topic: ch.topic || '', conv: s.id === DEFAULT.id && ch.id === 'general' ? 'room' : `ch:${s.id}/${ch.id}` })) }));
    const groups = db.groups.filter((g) => g.members.includes(user)).map((g) => ({ id: g.id, name: g.name, members: g.members.filter((u) => accounts.has(u)), createdBy: g.createdBy, conv: 'grp:' + g.id }));
    return { ver, servers, groups, agent: agentName };
  }
  const cleanName = (n, max = 40) => String(n || '').replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, max);
  const cleanMembers = (list, user) => [...new Set([user, ...(Array.isArray(list) ? list : String(list || '').split(/[,\s]+/)).map((u) => String(u).trim().toLowerCase()).filter((u) => accounts.has(u))])];

  const ops = {
    // ---- servers ----
    createServer(user, b) {
      const name = cleanName(b.name); if (!name) throw new Error('A server needs a name.');
      let id = slug(name) || 's'; if (id === DEFAULT.id || db.servers.some((s) => s.id === id)) id = (id + '-' + crypto.randomBytes(2).toString('hex')).slice(0, 32);
      const s = { id, name, icon: cleanName(b.icon, 2), channels: [{ id: 'general', name: 'general', topic: '' }], members: cleanMembers(b.members, user), createdBy: user, createdAt: new Date().toISOString() };
      db.servers.push(s); save(); return s;
    },
    renameServer(user, b) { const s = own(b.server, user); const name = cleanName(b.name); if (name) s.name = name; if (b.icon !== undefined) s.icon = cleanName(b.icon, 2); save(); return s; },
    addMembers(user, b) { const s = member(b.server, user); if (s.everyone) throw new Error('Everyone is already in the Mills server.'); s.members = cleanMembers([...s.members, ...(Array.isArray(b.add) ? b.add : [b.add])], user); save(); return s; },
    removeMember(user, b) { const s = member(b.server, user); if (s.everyone) throw new Error('Nobody can be removed from the Mills server.'); const who = String(b.user || '').toLowerCase(); if (who !== user && s.createdBy !== user) throw new Error('Only the server\'s creator removes other people.'); s.members = s.members.filter((u) => u !== who); if (!s.members.length) db.servers.splice(db.servers.indexOf(s), 1); save(); return s; },
    deleteServer(user, b) { const s = own(b.server, user); db.servers.splice(db.servers.indexOf(s), 1); save(); return true; },
    // ---- channels ----
    createChannel(user, b) {
      const s = member(b.server, user); const name = slug(b.name); if (!name) throw new Error('A channel needs a name (letters, digits, dashes).');
      if (s.channels.some((c) => c.id === name)) throw new Error('That channel exists already.');
      if (s.channels.length >= 50) throw new Error('That server has enough channels.');
      const ch = { id: name, name, topic: cleanName(b.topic, 120), createdBy: user }; s.channels.push(ch); save(); return ch;
    },
    updateChannel(user, b) { const s = member(b.server, user); const ch = s.channels.find((c) => c.id === b.channel); if (!ch) throw new Error('No such channel.'); if (b.topic !== undefined) ch.topic = cleanName(b.topic, 120); save(); return ch; },
    deleteChannel(user, b) {
      const s = member(b.server, user); const i = s.channels.findIndex((c) => c.id === b.channel);
      if (i < 0) throw new Error('No such channel.'); if (s.channels[i].id === 'general') throw new Error('#general stays.');
      if (s.channels[i].createdBy !== user && s.createdBy !== user) throw new Error('Only who made the channel (or the server) deletes it.');
      s.channels.splice(i, 1); save(); return true;
    },
    // ---- groups ----
    createGroup(user, b) {
      const members = cleanMembers(b.members, user); if (members.length < 2) throw new Error('A group needs at least one other person.');
      const g = { id: crypto.randomBytes(5).toString('hex'), name: cleanName(b.name) || members.map((u) => u).join(', ').slice(0, 40), members, createdBy: user, createdAt: new Date().toISOString() };
      db.groups.push(g); save(); return g;
    },
    renameGroup(user, b) { const g = inGroup(b.group, user); const name = cleanName(b.name); if (name) g.name = name; save(); return g; },
    addToGroup(user, b) { const g = inGroup(b.group, user); g.members = cleanMembers([...g.members, ...(Array.isArray(b.add) ? b.add : [b.add])], user); save(); return g; },
    leaveGroup(user, b) { const g = inGroup(b.group, user); g.members = g.members.filter((u) => u !== user); if (g.members.length < 1) db.groups.splice(db.groups.indexOf(g), 1); save(); return true; },
  };
  function member(id, user) { const s = server(id); if (!s) throw new Error('No such server.'); if (!serverMembers(s).includes(user)) throw new Error('You are not in that server.'); return s; }
  function own(id, user) { const s = member(id, user); if (s.everyone) throw new Error('The Mills server is everyone\'s.'); if (s.createdBy !== user) throw new Error('Only the server\'s creator may do that.'); return s; }
  function inGroup(id, user) { const g = group(id); if (!g || !g.members.includes(user)) throw new Error('No such group (or you are not in it).'); return g; }
  function dropUser(user) { // an account was deleted: out of every server and group
    for (const s of db.servers) s.members = s.members.filter((u) => u !== user);
    for (const g of db.groups) g.members = g.members.filter((u) => u !== user);
    db.servers = db.servers.filter((s) => s.members.length); db.groups = db.groups.filter((g) => g.members.length);
    save();
  }

  const ROUTES = { '/api/guilds/server': 'createServer', '/api/guilds/server/rename': 'renameServer', '/api/guilds/server/add': 'addMembers', '/api/guilds/server/remove': 'removeMember', '/api/guilds/server/delete': 'deleteServer',
    '/api/guilds/channel': 'createChannel', '/api/guilds/channel/update': 'updateChannel', '/api/guilds/channel/delete': 'deleteChannel',
    '/api/guilds/group': 'createGroup', '/api/guilds/group/rename': 'renameGroup', '/api/guilds/group/add': 'addToGroup', '/api/guilds/group/leave': 'leaveGroup' };
  async function route(p, req, res, acct) {
    if (!p.startsWith('/api/guilds')) return false;
    if (p === '/api/guilds' && req.method === 'GET') { send(res, 200, view(acct.username)); return true; }
    const op = ROUTES[p];
    if (!op || req.method !== 'POST') return false;
    try { const r = ops[op](acct.username, (await readBody(req)) || {}); send(res, 200, { ok: true, result: r, ...view(acct.username) }); }
    catch (e) { send(res, 400, { error: e.message }); }
    return true;
  }
  return { parse, exists, members, isMember, isGuildConv, view, route, dropUser, ops, version: () => ver, DEFAULT_SERVER: DEFAULT.id };
};
