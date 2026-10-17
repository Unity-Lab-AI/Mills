// Mills team tools for Claude in a Work run (bridge/team-mcp.js → /internal/team): post into the
// team chat as "<person> · Claude" and see who is here. Kept apart from server.js so the chat room,
// the presence table and this relay stay separate pieces.
//
// A post is signed with the room agent's key (the only key the hub holds), so every member can read
// it; the `as` label tells browsers whose Claude wrote it and `via` whose colour to use. Unity does not
// answer these (they come from the agent's own key), so a Claude run can never call her by accident.
'use strict';

module.exports = function createTeam({ accounts, agentSay, presenceList, displayOf, chatKeys }) {
  const ok = (result) => ({ ok: true, result });
  const no = (error) => ({ ok: false, error });
  const tools = {
    team_say(acct, args) {
      const text = String(args.text || '').trim().slice(0, 20000);
      if (!text) return no('team_say needs "text".');
      const to = String(args.to || '').trim().toLowerCase();
      if (to) {
        if (!accounts.has(to)) return no(`No account called "${to}".`);
        if (!chatKeys[to]) return no(`${displayOf(to)} has no chat key yet (they have not signed in since chat was added), so nothing can be encrypted for them.`);
      }
      const label = displayOf(acct.username) + ' · Claude';
      const seq = agentSay(text, to, { as: label, via: acct.username });
      return ok({ posted: true, seq, as: label, to: to || 'everyone' });
    },
    team_presence(acct) {
      const now = Date.now();
      const list = presenceList().map((p) => ({ user: p.user, name: displayOf(p.user), state: p.online ? (p.idle ? 'away' : 'online') : 'offline', screen: p.screen || '', lastSeen: p.seen ? new Date(p.seen).toISOString() : null, agoMin: p.seen ? Math.round((now - p.seen) / 60000) : null }));
      for (const a of accounts.values()) if (!list.some((x) => x.user === a.username)) list.push({ user: a.username, name: displayOf(a.username), state: 'offline', screen: '', lastSeen: null, agoMin: null });
      return ok({ you: acct.username, people: list });
    },
  };
  // name + args from the MCP relay; acct = the account whose Work run this is
  function call(name, args, acct) {
    const t = tools[name];
    if (!t) return no('Unknown team tool: ' + name);
    try { return t(acct, args || {}); } catch (e) { return no(e.message); }
  }
  return { call, TOOL_NAMES: Object.keys(tools) };
};
