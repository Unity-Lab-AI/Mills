// Mills server permissions (bridge/guilds.js): Discord-shaped roles, kept small.
//
// Every server has the implicit role "@everyone" (its permissions are the server's `everyonePerms`) and any
// number of named roles with a colour, a position (higher = listed first, outranks lower) and permissions.
// A person's permissions = @everyone's + every role they hold. The server's owner has every permission, and
// so does anyone holding a role with ADMIN. In the everyone-server ("Mills"), the hub's admin accounts count
// as owners. Ranking: you can only manage people and roles strictly below your own highest role (the owner
// outranks everyone), so a moderator can't kick the admin who made them a moderator.
'use strict';

const PERMS = {
  ADMIN: 'Administrator: every permission, every private channel',
  MANAGE_SERVER: 'Change the server: name, icon, colour, description, password lock, see the audit log',
  MANAGE_ROLES: 'Make, change and hand out roles below their own',
  MANAGE_CHANNELS: 'Make, change, reorder and delete any channel',
  CREATE_CHANNELS: 'Make channels (and change or delete the ones they made)',
  KICK: 'Remove people from the server',
  BAN: 'Ban and unban people',
  INVITE: 'Make invite links and invite people',
  MANAGE_INVITES: 'See and revoke everyone\'s invites',
  MANAGE_NICKNAMES: 'Change other people\'s server nicknames',
  ANNOUNCE: 'Post in announcement (read-only) channels',
};
const PERM_IDS = Object.keys(PERMS);
const DEFAULT_EVERYONE = ['INVITE', 'CREATE_CHANNELS'];

const cleanPerms = (list) => [...new Set((Array.isArray(list) ? list : String(list || '').split(/[,\s]+/)).map((p) => String(p).trim().toUpperCase()).filter((p) => PERM_IDS.includes(p)))];

module.exports = function createPerms({ isHubAdmin }) {
  const isOwner = (s, user) => (s.everyone ? isHubAdmin(user) : s.owner === user);
  const rolesOf = (s, user) => (s.memberRoles[user] || []).map((id) => s.roles.find((r) => r.id === id)).filter(Boolean);
  function permsOf(s, user) {
    if (isOwner(s, user)) return [...PERM_IDS];
    const set = new Set(s.everyonePerms || []);
    for (const r of rolesOf(s, user)) for (const p of r.perms) set.add(p);
    return set.has('ADMIN') ? [...PERM_IDS] : [...set];
  }
  const can = (s, user, perm) => permsOf(s, user).includes(perm);
  const rank = (s, user) => (isOwner(s, user) ? Infinity : Math.max(0, ...rolesOf(s, user).map((r) => r.position)));
  const outranks = (s, user, other) => rank(s, user) > rank(s, other);
  const roleBelow = (s, user, role) => rank(s, user) > role.position;
  return { PERMS, PERM_IDS, DEFAULT_EVERYONE, cleanPerms, isOwner, rolesOf, permsOf, can, rank, outranks, roleBelow };
};
module.exports.PERMS = PERMS;
module.exports.PERM_IDS = PERM_IDS;
module.exports.DEFAULT_EVERYONE = DEFAULT_EVERYONE;
module.exports.cleanPerms = cleanPerms;
