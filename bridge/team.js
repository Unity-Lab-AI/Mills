// Mills team tools for Claude in a Work run (bridge/team-mcp.js → /internal/team): post into the
// team chat as "<person> · Claude" and see who is here. Kept apart from server.js so the chat room,
// the presence table and this relay stay separate pieces.
//
// A post is signed with the room agent's key (the only key the hub holds), so every member can read
// it; the `as` label tells browsers whose Claude wrote it and `via` whose colour to use. Unity does not
// answer these (they come from the agent's own key), so a Claude run can never call her by accident.
'use strict';

module.exports = function createTeam({ accounts, agentSay, presenceList, displayOf, chatKeys, guilds = null, hub = {} }) {
  // hub: optional getters for the wider picture: callsState(user), sharesFor(user), usageStatus(acct), archiveSearch(acct, q), providersList(user)
  const ok = (result) => ({ ok: true, result });
  const no = (error) => ({ ok: false, error });
  const tools = {
    team_channel_create(acct, args) { // a channel in a server this person is in (the Mills server included)
      if (!guilds) return no('Servers are not available.');
      try { const ch = guilds.ops.createChannel(acct.username, { server: String(args.server || 'mills'), name: String(args.name || ''), topic: String(args.topic || '') }); return ok({ created: true, conv: 'ch:' + String(args.server || 'mills') + '/' + ch.id, channel: ch }); }
      catch (e) { return no(e.message); }
    },
    team_group_create(acct, args) { // a group conversation with the people named (this person included)
      if (!guilds) return no('Groups are not available.');
      try { const g = guilds.ops.createGroup(acct.username, { name: String(args.name || ''), members: Array.isArray(args.members) ? args.members : String(args.members || '').split(/[,\s]+/) }); return ok({ created: true, conv: 'grp:' + g.id, group: { id: g.id, name: g.name, members: g.members } }); }
      catch (e) { return no(e.message); }
    },
    hub_status(acct) { // the wider picture: who is here, calls, shared Work sessions, the host account's usage windows
      const now = Date.now();
      const people = presenceList().map((p) => ({ user: p.user, name: displayOf(p.user), state: p.online ? (p.idle ? 'away' : 'online') : 'offline', screen: p.screen || '', agoMin: p.seen ? Math.round((now - p.seen) / 60000) : null }));
      const calls = hub.callsState ? hub.callsState(acct.username) : {};
      const inCalls = Object.entries(calls).map(([conv, ps]) => ({ conv, people: ps.map((p) => ({ user: p.user, name: displayOf(p.user), cam: p.cam, mic: p.mic, screen: p.screen })) }));
      const shares = hub.sharesFor ? hub.sharesFor(acct.username).map((s) => ({ user: s.user, name: displayOf(s.user), since: s.since })) : [];
      const usage = hub.usageStatus ? hub.usageStatus(acct) : null;
      const fmt = (w) => (!w ? null : { source: w.source, status: w.status, usedPercent: w.utilization == null ? null : Math.round(w.utilization * 100), resetsInMin: w.resetsAt ? Math.max(0, Math.round((w.resetsAt - now) / 60000)) : null });
      return ok({ you: acct.username, people, calls: inCalls, sharedWorkSessions: shares, usage: usage ? { fiveHour: fmt(usage.five_hour), sevenDay: fmt(usage.seven_day), context: usage.context } : null });
    },
    sessions_search(acct, args) { // this person's own saved Claude sessions (titles + transcripts)
      if (!hub.archiveSearch) return no('Past sessions are not available.');
      const q = String(args.q || args.query || '').slice(0, 200);
      const items = hub.archiveSearch(acct, q).slice(0, 20).map((it) => ({ id: it.id, title: it.title, savedAt: it.savedAt, entries: it.count, hits: it.hits, snippets: (it.snippets || []).map((s) => (s.cls === 'me' ? displayOf(acct.username) : 'Claude') + ': ' + s.text) }));
      return ok({ query: q, items });
    },
    my_providers(acct) { // the AIs this person may use (the host's Claude, their own, the team's shared ones)
      if (!hub.providersList) return ok({ providers: [] });
      return ok({ providers: hub.providersList(acct.username).map((p) => ({ id: p.id, label: p.label, kind: p.kind, owner: p.owner || 'host', shared: p.shared, models: p.models })), current: acct.state.provider || 'host', model: acct.state.model || '' });
    },
    team_say(acct, args) {
      const text = String(args.text || '').trim().slice(0, 20000);
      if (!text) return no('team_say needs "text".');
      const to = String(args.to || '').trim().toLowerCase(), conv = String(args.conv || '').trim();
      if (to && conv) return no('Give either "to" (a private message) or "conv" (a channel / group), not both.');
      if (to) {
        if (!accounts.has(to)) return no(`No account called "${to}".`);
        if (!chatKeys[to]) return no(`${displayOf(to)} has no chat key yet (they have not signed in since chat was added), so nothing can be encrypted for them.`);
      }
      if (conv && conv !== 'room') {
        if (!guilds || !guilds.isGuildConv(conv) || !guilds.exists(conv)) return no(`No conversation "${conv}" (use team_channels to list them).`);
        if (!guilds.isMember(conv, acct.username)) return no(`${displayOf(acct.username)} is not in ${conv}.`);
      }
      const label = displayOf(acct.username) + ' · Claude';
      const seq = agentSay(text, to, { as: label, via: acct.username, conv: conv && conv !== 'room' ? conv : '' });
      return ok({ posted: true, seq, as: label, to: to || (conv && conv !== 'room' ? conv : 'everyone') });
    },
    team_channels(acct) { // the servers, channels and groups this person is in (conv ids for team_say)
      if (!guilds) return ok({ servers: [], groups: [] });
      const v = guilds.view(acct.username);
      return ok({ servers: v.servers.map((s) => ({ id: s.id, name: s.name, members: s.members, channels: s.channels.map((c) => ({ name: '#' + c.name, conv: c.conv, topic: c.topic })) })), groups: v.groups.map((g) => ({ name: g.name, conv: g.conv, members: g.members })) });
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
