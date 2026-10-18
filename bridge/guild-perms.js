// VibeRoller server permissions (bridge/guilds.js), modelled on Discord's (docs.discord.com → Topics → Permissions).
//
// Server level: every server has the implicit role "@everyone" (permissions = the server's `everyonePerms`) plus
// named roles with a colour, a position (higher outranks lower) and permissions. A person's server permissions =
// @everyone's + every role they hold. The owner has every permission, and so does anyone with ADMINISTRATOR (which
// also skips channel overwrites). In the everyone-server "VibeRoller" the hub's admin accounts count as owners.
//
// Channel level (Discord's order, each step overriding the one before):
//   server permissions → @everyone deny → @everyone allow → role denies (all roles held) → role allows → member deny
//   → member allow. Implicit rules: without VIEW_CHANNEL nothing else in that channel; without SEND_MESSAGES no
//   voice messages; without CONNECT no SPEAK / STREAM. An announcement channel needs ANNOUNCE (or MANAGE_MESSAGES)
//   to post. A member in timeout keeps only VIEW_CHANNEL + READ_MESSAGE_HISTORY. A server-muted member loses SPEAK.
//
// Ranking: you manage people and roles strictly below your own highest role (the owner outranks everyone), and you
// can only hand out permissions you hold yourself.
//
// Some Discord permissions have no VibeRoller feature behind them (stickers, threads, stage channels, webhooks, apps,
// polls, soundboard, events, insights) and are left out. Text content is end-to-end encrypted, so permissions that
// depend on reading it (embed links, attach files, mention @everyone) cannot be enforced by the hub and are left out
// too rather than pretended.
'use strict';

const CATEGORIES = [
  ['General', {
    VIEW_CHANNEL: 'See channels and read them (deny it on a channel to make it private)',
    MANAGE_CHANNELS: 'Make, change, reorder and delete any channel, and set channel permissions',
    CREATE_CHANNELS: 'Make channels, and change or delete the ones they made (a VibeRoller extra)',
    MANAGE_ROLES: 'Make, change and hand out roles below their own',
    VIEW_AUDIT_LOG: 'Read the audit log',
    MANAGE_SERVER: 'Change the server: name, icon, colour, description, password lock, welcome channel, other settings',
  }],
  ['Membership', {
    CREATE_INVITE: 'Make invite links and invite people by name',
    MANAGE_INVITES: 'See and revoke everyone\'s invites (a VibeRoller extra)',
    CHANGE_NICKNAME: 'Change their own server nickname',
    MANAGE_NICKNAMES: 'Change other people\'s server nicknames',
    KICK_MEMBERS: 'Remove people from the server',
    BAN_MEMBERS: 'Ban and unban people (optionally deleting their recent messages)',
    MODERATE_MEMBERS: 'Time people out: they can read but not post, type or join calls for a while',
  }],
  ['Text', {
    SEND_MESSAGES: 'Post messages',
    SEND_VOICE_MESSAGES: 'Send voice clips',
    READ_MESSAGE_HISTORY: 'Read messages sent before they joined (without it: only what arrives while they are a member)',
    MANAGE_MESSAGES: 'Delete other people\'s messages',
    PIN_MESSAGES: 'Pin and unpin messages',
    ANNOUNCE: 'Post in announcement channels (a VibeRoller extra)',
    BYPASS_SLOWMODE: 'Post without waiting for a channel\'s slowmode',
  }],
  ['Voice', {
    CONNECT: 'Join calls',
    SPEAK: 'Talk in calls',
    STREAM: 'Turn on the camera or share the screen in calls',
    MUTE_MEMBERS: 'Mute other people in calls (server mute)',
  }],
  ['Advanced', {
    ADMINISTRATOR: 'Every permission, every channel, skips channel permissions. Give with care.',
  }],
];
const PERMS = Object.assign({}, ...CATEGORIES.map(([, p]) => p));
const PERM_IDS = Object.keys(PERMS);
const CHANNEL_PERMS = ['VIEW_CHANNEL', 'MANAGE_CHANNELS', 'MANAGE_ROLES', 'CREATE_INVITE', 'SEND_MESSAGES', 'SEND_VOICE_MESSAGES', 'READ_MESSAGE_HISTORY', 'MANAGE_MESSAGES', 'PIN_MESSAGES', 'ANNOUNCE', 'BYPASS_SLOWMODE', 'CONNECT', 'SPEAK', 'STREAM', 'MUTE_MEMBERS'];
// what @everyone may do in a new server (Discord's defaults, for the features VibeRoller has)
const DEFAULT_EVERYONE = ['VIEW_CHANNEL', 'CREATE_INVITE', 'CHANGE_NICKNAME', 'CREATE_CHANNELS', 'SEND_MESSAGES', 'SEND_VOICE_MESSAGES', 'READ_MESSAGE_HISTORY', 'CONNECT', 'SPEAK', 'STREAM'];
// The built-in role ladder every server has (made with the server, added to older ones, never deleted; the owner
// may rename, recolour and re-permission them). Each level holds everything below it. Positions leave gaps so
// custom roles can sit between levels. The owner stays above all of them; Co-owner is "owner rights, under the owner":
// every permission, outranks everyone else, but cannot delete the server, hand it over, or manage other co-owners.
// Everyone who joins gets Member (except temporary members, who stay without a role until someone gives them one).
const MEMBER = ['VIEW_CHANNEL', 'CHANGE_NICKNAME', 'CREATE_INVITE', 'SEND_MESSAGES', 'SEND_VOICE_MESSAGES', 'READ_MESSAGE_HISTORY', 'CONNECT', 'SPEAK', 'STREAM'];
const MODERATOR = [...MEMBER, 'MANAGE_MESSAGES', 'PIN_MESSAGES', 'MODERATE_MEMBERS', 'MUTE_MEMBERS', 'KICK_MEMBERS', 'MANAGE_NICKNAMES', 'BYPASS_SLOWMODE', 'VIEW_AUDIT_LOG'];
const SUPER_MODERATOR = [...MODERATOR, 'BAN_MEMBERS', 'MANAGE_INVITES', 'ANNOUNCE', 'CREATE_CHANNELS'];
const ADMIN = [...SUPER_MODERATOR, 'MANAGE_CHANNELS', 'MANAGE_ROLES', 'MANAGE_SERVER'];
const BUILTINS = [ // key, name, colour, position, permissions (lowest first)
  { key: 'member', name: 'Member', color: '#2ecc71', position: 10, perms: MEMBER },
  { key: 'moderator', name: 'Moderator', color: '#3498db', position: 20, perms: MODERATOR },
  { key: 'supermod', name: 'Super Moderator', color: '#1abc9c', position: 30, perms: SUPER_MODERATOR },
  { key: 'admin', name: 'Admin', color: '#e67e22', position: 40, perms: ADMIN },
  { key: 'superadmin', name: 'Super Admin', color: '#e74c3c', position: 50, perms: ['ADMINISTRATOR'] },
  { key: 'coowner', name: 'Co-owner', color: '#f1c40f', position: 60, perms: ['ADMINISTRATOR'] },
];
const COOWNER_POS = 60;
const TEMPLATES = Object.fromEntries(BUILTINS.map((b) => [b.key, { name: b.name, color: b.color, perms: b.perms }])); // also offered as templates for custom roles
// older stored names → Discord's
const RENAMED = { INVITE: 'CREATE_INVITE', KICK: 'KICK_MEMBERS', BAN: 'BAN_MEMBERS', ADMIN: 'ADMINISTRATOR' };

const cleanPerms = (list, allowed = PERM_IDS) => [...new Set((Array.isArray(list) ? list : String(list || '').split(/[,\s]+/)).map((p) => String(p).trim().toUpperCase()).map((p) => RENAMED[p] || p).filter((p) => allowed.includes(p)))];

module.exports = function createPerms({ isHubAdmin }) {
  // The everyone-server's owner is its named owner (set from the host, bridge/server-owners.json); the hub's admin
  // accounts keep owner rights there too, so the host can always recover it. Other servers: their owner only.
  const isOwner = (s, user) => (s.everyone ? (!!s.owner && s.owner === user) || isHubAdmin(user) : s.owner === user);
  const rolesOf = (s, user) => { // in the everyone-server everybody holds Member (accounts come and go; nobody joins it)
    const list = (s.memberRoles[user] || []).map((id) => s.roles.find((r) => r.id === id)).filter(Boolean);
    if (s.everyone) { const m = s.roles.find((r) => r.builtin === 'member'); if (m && !list.includes(m)) list.push(m); }
    return list;
  };
  const timedOut = (s, user) => !!(s.timeouts && s.timeouts[user] && Date.parse(s.timeouts[user]) > Date.now());
  const isAdmin = (s, user) => isOwner(s, user) || rolesOf(s, user).some((r) => r.perms.includes('ADMINISTRATOR')) || (s.everyonePerms || []).includes('ADMINISTRATOR');
  function base(s, user) { // server level, before timeouts
    if (isAdmin(s, user)) return new Set(PERM_IDS);
    const set = new Set(s.everyonePerms || []);
    for (const r of rolesOf(s, user)) for (const p of r.perms) set.add(p);
    return set;
  }
  const limit = (s, user, set) => { // timeout + server mute (never for owners / administrators)
    if (isAdmin(s, user)) return set;
    if (timedOut(s, user)) return new Set([...set].filter((p) => p === 'VIEW_CHANNEL' || p === 'READ_MESSAGE_HISTORY'));
    if (s.voiceMuted && s.voiceMuted[user]) set.delete('SPEAK');
    return set;
  };
  function permsOf(s, user) { return [...limit(s, user, base(s, user))]; }
  function permsIn(s, ch, user) { // channel level
    if (!ch) return permsOf(s, user);
    if (isAdmin(s, user)) return [...PERM_IDS];
    const set = base(s, user);
    const ow = Array.isArray(ch.overwrites) ? ch.overwrites : [];
    const apply = (o) => { if (!o) return; for (const p of o.deny || []) set.delete(p); for (const p of o.allow || []) set.add(p); };
    apply(ow.find((o) => o.type === 'role' && o.id === 'everyone'));
    const mine = new Set(s.memberRoles[user] || []);
    const roleOw = ow.filter((o) => o.type === 'role' && mine.has(o.id));
    for (const o of roleOw) for (const p of o.deny || []) set.delete(p); // all role denies, then all role allows (allow wins)
    for (const o of roleOw) for (const p of o.allow || []) set.add(p);
    apply(ow.find((o) => o.type === 'member' && o.id === user));
    if (!set.has('VIEW_CHANNEL')) return [];
    if (!set.has('SEND_MESSAGES')) set.delete('SEND_VOICE_MESSAGES');
    if (ch.announce && !set.has('ANNOUNCE') && !set.has('MANAGE_MESSAGES')) { set.delete('SEND_MESSAGES'); set.delete('SEND_VOICE_MESSAGES'); }
    if (!set.has('CONNECT')) { set.delete('SPEAK'); set.delete('STREAM'); }
    return [...limit(s, user, set)];
  }
  const can = (s, user, perm) => permsOf(s, user).includes(perm);
  const canIn = (s, ch, user, perm) => permsIn(s, ch, user).includes(perm);
  const rank = (s, user) => (isOwner(s, user) ? Infinity : Math.max(0, ...rolesOf(s, user).map((r) => r.position)));
  const outranks = (s, user, other) => rank(s, user) > rank(s, other);
  const roleBelow = (s, user, role) => rank(s, user) > role.position;
  const grantable = (s, user, perms) => { const mine = base(s, user); return perms.every((p) => mine.has(p)); }; // only what you hold yourself
  const isCoOwner = (s, user) => rolesOf(s, user).some((r) => r.builtin === 'coowner');
  return { PERMS, PERM_IDS, CATEGORIES, CHANNEL_PERMS, DEFAULT_EVERYONE, TEMPLATES, BUILTINS, COOWNER_POS, isCoOwner, cleanPerms, isOwner, isAdmin, rolesOf, timedOut, permsOf, permsIn, can, canIn, rank, outranks, roleBelow, grantable };
};
Object.assign(module.exports, { PERMS, PERM_IDS, CATEGORIES, CHANNEL_PERMS, DEFAULT_EVERYONE, TEMPLATES, BUILTINS, COOWNER_POS, RENAMED, cleanPerms });
