// Mills servers, channels, groups and the member column, page side (hub side: bridge/guilds.js).
// Loaded after the page script and calls.js; uses the page's helpers at call time ($, json, me, nickOf,
// colorOf, statusOf, ic, selectConv, currentConv, roomMembers, sysRoom, store). Server Settings, the join /
// invitation dialogs and group settings live in web/guild-settings.js (GSET).
//
// The chat's side bar is Discord-shaped: a rail of servers on the far left (the "Mills" server is
// everyone's and always first; its #general is the old # Everyone), with buttons for pending invitations and
// joining by invite code; then the picked server's channels by category (private ones carry a lock,
// announcement ones a flag), group conversations and private messages. The member column (app mode, or wide
// screens) lists who is in the current conversation by hoisted role and presence; Unity and
// "<person> · Claude" carry a BOT badge.
const GUILDS = (() => {
  const G = { data: null, ver: 0, server: 'mills' };
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const isGuildConv = (c) => /^(ch|grp):/.test(String(c || ''));
  const has = (c) => !!(G.data && (G.data.servers.some((s) => s.channels.some((ch) => ch.conv === c)) || G.data.groups.some((g) => g.conv === c)));
  function find(c) { // → { kind: 'ch', server, channel } | { kind: 'grp', group } | null
    if (!G.data) return null;
    for (const s of G.data.servers) for (const ch of s.channels) if (ch.conv === c) return { kind: 'ch', server: s, channel: ch };
    const g = G.data.groups.find((x) => x.conv === c);
    return g ? { kind: 'grp', group: g } : null;
  }
  const label = (c) => { const f = find(c); return !f ? c : f.kind === 'ch' ? '# ' + f.channel.name : f.group.name; };
  const members = (c) => { const f = find(c); return !f ? [] : f.kind === 'ch' ? f.channel.members || f.server.members : f.group.members; }; // a private channel: only who may see it
  const serverOf = (c) => { const f = find(c); return f && f.kind === 'ch' ? f.server.id : null; };
  const current = () => (G.data ? G.data.servers.find((x) => x.id === G.server) || G.data.servers[0] : null);
  const can = (s, p) => !!(s && s.me && s.me.perms.includes(p));
  const nickIn = (c, u) => { const f = find(c); return f && f.kind === 'ch' && f.server.nicks ? f.server.nicks[u] || '' : ''; };
  // ── role-based looks: the highest coloured role decides a name's colour inside that server only ──
  const rolesOfUser = (s, u) => { const ids = (s.memberRoles || {})[u] || []; return (s.roles || []).filter((x) => ids.includes(x.id) || (s.everyone && x.builtin === 'member')).sort((a, b) => b.position - a.position); };
  const roleColor = (s, u) => { const r = rolesOfUser(s, u).find((x) => x.color); return r ? r.color : ''; };
  const serverOfConv = (c) => { const f = find(c); return f && f.kind === 'ch' ? f.server : null; }; // 'room' = Mills (its #general)
  const colorIn = (c, u) => { const s = serverOfConv(c); return s && s.members.includes(u) ? roleColor(s, u) : ''; };
  const roleHandle = (r) => String(r.name).replace(/\s+/g, '').toLowerCase();
  const mentionableRoles = (c) => { const s = serverOfConv(c); if (!s) return []; const mine = new Set(rolesOfUser(s, me.username).map((r) => r.id)); return (s.roles || []).filter((r) => r.mentionable).map((r) => ({ id: r.id, name: r.name, color: r.color, handle: roleHandle(r), mine: mine.has(r.id) })); };
  const roleByHandle = (c, h) => mentionableRoles(c).find((r) => r.handle === String(h).toLowerCase()) || null;
  const mentionsMyRole = (c, text) => (String(text || '').match(/@([\w.-]+)/g) || []).some((x) => { const r = roleByHandle(c, x.slice(1)); return !!(r && r.mine); });
  const canPost = (c) => { const f = find(c); return !f || f.kind !== 'ch' || f.channel.canPost !== false; };
  // ── per-conversation permissions the page needs (the hub checks them again) ──
  const chanPerms = (c) => { const f = find(c); return f && f.kind === 'ch' ? f.channel.perms || [] : null; }; // null = not a server channel
  const canPin = (c) => { const p = chanPerms(c); return p ? p.includes('PIN_MESSAGES') : /^(grp|dm):/.test(String(c || '')); };
  const canModerate = (c) => { const p = chanPerms(c); return !!p && p.includes('MANAGE_MESSAGES'); };
  const pinsOf = (c) => { const f = find(c); if (f) return (f.kind === 'ch' ? f.channel.pins : f.group.pins) || []; return ((G.data && G.data.dmPins) || {})[c] || []; };
  const pinned = (c, seq) => pinsOf(c).some((p) => p.seq === seq);
  async function pin(c, seq, on) { const r = await op('pin', { conv: c, seq, on }); if (r) renderPins(); }
  const mentionsOnly = (c) => { const f = find(c); return !!(f && f.kind === 'ch' && f.server.settings && f.server.settings.notify === 'mentions'); };
  function renderPins() { // a "Pins (n)" button next to the conversation name; its list jumps to the message
    const name = $('convName'); if (!name) return;
    let b = $('pinsBtn');
    if (!b) { b = el('button', 'ghost tiny'); b.id = 'pinsBtn'; b.type = 'button'; b.title = 'Pinned messages'; name.after(b); b.onclick = (e) => { e.stopPropagation(); showPins(); }; }
    const n = pinsOf(currentConv).length; b.textContent = ''; b.append(ic('pin')); if (n) b.append(' ' + n); b.hidden = !n;
  }
  function showPins() {
    const old = $('pinsPop'); if (old) { old.remove(); return; }
    const pop = el('div', 'pinsPop'); pop.id = 'pinsPop';
    pop.append(el('b', '', 'Pinned in ' + label(currentConv)));
    for (const p of [...pinsOf(currentConv)].reverse()) {
      const rec = msgs.get(p.seq);
      const row = el('div', 'pinRow'); row.append(el('span', 'pinWho', rec ? authorOf(rec.m) : '?'), el('span', 'pinText', rec ? snippet(rec) : '(an older message, not loaded)'), el('small', '', 'pinned by ' + nickOf(p.by)));
      row.onclick = () => { pop.remove(); if (rec && rec.el) { rec.el.scrollIntoView({ block: 'center' }); rec.el.classList.add('flash'); setTimeout(() => rec.el && rec.el.classList.remove('flash'), 1500); } };
      if (canPin(currentConv)) { const x = el('button', 'ghost tiny', 'Unpin'); x.type = 'button'; x.onclick = (e) => { e.stopPropagation(); pin(currentConv, p.seq, false); pop.remove(); }; row.append(x); }
      pop.append(row);
    }
    document.body.append(pop);
    const r = $('pinsBtn').getBoundingClientRect(); pop.style.top = (r.bottom + 6) + 'px'; pop.style.left = Math.max(8, Math.min(r.left, innerWidth - 380)) + 'px';
    setTimeout(() => document.addEventListener('click', function off(e) { if (!e.target.closest('#pinsPop')) { pop.remove(); document.removeEventListener('click', off); } }), 0);
  }
  async function refresh() {
    if (!me) return;
    try { G.data = await json('/api/guilds'); G.ver = G.data.ver; } catch { return; }
    if (!G.data.servers.some((s) => s.id === G.server)) G.server = 'mills';
    const sv = serverOf(currentConv); if (sv) G.server = sv;
    if (isGuildConv(currentConv) && !has(currentConv)) selectConv('room'); // kicked, banned, left, or the channel went private
    render();
    if (typeof GSET !== 'undefined') GSET.refreshOpen();
  }
  async function op(path, body, quiet) {
    try { const d = await json('/api/guilds/' + path, { method: 'POST', body: JSON.stringify(body) }); G.data = d; G.ver = d.ver; render(); if (typeof GSET !== 'undefined') GSET.refreshOpen(); return d.result === undefined ? true : d.result; }
    catch (e) { if (quiet) throw e; sysRoom(e.message); return null; }
  }
  // ── server rail ──
  function renderRail() {
    const rail = $('serverRail'); if (!rail || !G.data) return;
    rail.textContent = '';
    for (const s of G.data.servers) {
      const b = el('button', 'srv' + (s.id === G.server ? ' on' : '')); b.type = 'button';
      b.title = s.name + (s.everyone ? ' · everyone' : ' · ' + s.members.length + ' member' + (s.members.length === 1 ? '' : 's')) + (s.locked ? ' · password locked' : '') + (s.description ? '\n' + s.description : '');
      b.textContent = (s.icon || s.name).slice(0, 2).toUpperCase(); b.style.background = s.color || (s.id === 'mills' ? 'var(--accent)' : colorOf(s.owner || s.createdBy || s.id));
      const badge = el('span', 'badge'); badge.hidden = true; badge.dataset.serverBadge = s.id; b.append(badge);
      b.onclick = () => { G.server = s.id; render(); const first = s.channels[0]; if (first && serverOf(currentConv) !== s.id && currentConv !== first.conv) selectConv(first.conv); };
      b.oncontextmenu = (e) => { e.preventDefault(); G.server = s.id; render(); GSET.openServer(s.id); };
      rail.append(b);
    }
    const inv = G.data.invitations || [];
    if (inv.length) { const b = el('button', 'srv invites'); b.type = 'button'; b.title = inv.length + ' server invitation' + (inv.length === 1 ? '' : 's'); b.append(ic('mail')); const n = el('span', 'badge', String(inv.length)); b.append(n); b.onclick = () => GSET.invitations(); rail.append(b); }
    const add = el('button', 'srv add'); add.type = 'button'; add.title = 'Make a server'; add.append(ic('plus')); add.onclick = () => GSET.newServer(); rail.append(add);
    const join = el('button', 'srv add'); join.type = 'button'; join.title = 'Join a server with an invite code or link'; join.append(ic('link')); join.onclick = () => GSET.join(); rail.append(join);
  }
  // ── channels of the picked server (by category), then groups ──
  function renderChannels() {
    const box = $('chanList'); if (!box || !G.data) return;
    box.textContent = '';
    const s = current();
    if (!s) return;
    const head = el('div', 'chHead');
    const name = el('b', '', s.name); name.title = s.description || ''; head.append(name);
    if (s.locked) { const l = ic('lock'); l.setAttribute('aria-label', 'password locked'); head.append(l); }
    if (!s.everyone && can(s, 'CREATE_INVITE')) { const inv = el('button', 'ghost tiny'); inv.type = 'button'; inv.title = 'Invite people'; inv.append(ic('plus')); inv.onclick = () => GSET.openServer(s.id, 'invites'); head.append(inv); }
    const set = el('button', 'ghost tiny'); set.type = 'button'; set.title = 'Server settings: overview, members, roles, channels, invites' + (s.everyone ? '' : ', bans, password lock, audit log'); set.append(ic('gear')); set.onclick = () => GSET.openServer(s.id); head.append(set);
    box.append(head);
    const cats = []; for (const ch of s.channels) { const c = ch.category || 'Text channels'; if (!cats.includes(c)) cats.push(c); }
    if (!cats.length) cats.push('Text channels');
    cats.forEach((cat, i) => {
      const sect = el('div', 'chSect'); sect.append(el('span', '', cat));
      if (i === 0 && (can(s, 'CREATE_CHANNELS') || can(s, 'MANAGE_CHANNELS'))) { const plus = el('button', 'ghost tiny'); plus.type = 'button'; plus.title = 'New channel'; plus.append(ic('plus')); plus.onclick = () => GSET.newChannel(s); sect.append(plus); }
      box.append(sect);
      for (const ch of s.channels.filter((c) => (c.category || 'Text channels') === cat)) box.append(convButton(ch.conv, ch.private ? '' : '#', ch.name, [s.settings && s.settings.rulesChannel === ch.id ? 'rules' : '', ch.slowmode ? 'slowmode' : '', ch.topic || (ch.conv === 'room' ? 'the whole team' : '')].filter(Boolean).join(' · '), ch, null, s));
    });
    const gs = el('div', 'chSect'); gs.append(el('span', '', 'Groups'));
    const gp = el('button', 'ghost tiny'); gp.type = 'button'; gp.title = 'New group conversation'; gp.append(ic('plus')); gp.onclick = () => GSET.newGroup(); gs.append(gp); box.append(gs);
    for (const g of G.data.groups) box.append(convButton(g.conv, '', g.name, g.members.map((u) => nickOf(u)).join(', '), null, g));
    if (!G.data.groups.length) box.append(el('div', 'chNone', 'no groups yet'));
    const dmHead = el('div', 'chSect'); dmHead.append(el('span', '', 'Direct messages')); box.append(dmHead);
  }
  function convButton(conv, prefix, name, sub, ch, g, s) {
    const b = el('button', 'cv ch' + (conv === currentConv ? ' on' : '')); b.type = 'button';
    const dot = el('span', 'dot hash'); // a drawn icon, never a typed character: # / lock / megaphone for channels, people for groups
    dot.append(ic(ch ? (ch.announce ? 'megaphone' : ch.private ? 'lock' : 'hash') : 'users'));
    const nm = el('span', 'nm', name); const sm = el('small', '', sub); nm.append(sm);
    const badge = el('span', 'badge'); badge.dataset.convBadge = conv; badge.hidden = true;
    b.append(dot, nm, badge); b.onclick = () => selectConv(conv);
    if (ch && s && (can(s, 'MANAGE_CHANNELS') || ch.createdBy === me.username)) {
      const x = el('button', 'ghost tiny chDel'); x.type = 'button'; x.title = 'Edit channel'; x.append(ic('edit'));
      x.onclick = (e) => { e.stopPropagation(); GSET.editChannel(s, ch); };
      b.append(x);
      b.oncontextmenu = (e) => { e.preventDefault(); GSET.editChannel(s, ch); };
    }
    if (g) {
      const x = el('button', 'ghost tiny chDel'); x.type = 'button'; x.title = 'Group settings: rename, people, owner, leave'; x.append(ic('gear'));
      x.onclick = (e) => { e.stopPropagation(); GSET.editGroup(g.id); };
      b.append(x);
      b.oncontextmenu = (e) => { e.preventDefault(); GSET.editGroup(g.id); };
    }
    return b;
  }
  // ── member column ──
  function renderMembers() {
    const box = $('memberList'); if (!box || !me) return;
    // shown in the installed app, and on wide screens while the Chat screen is open (not inside Work's team-chat pane)
    box.hidden = $('room').hidden || !(document.body.classList.contains('appMode') || window.innerWidth >= 1500);
    box.textContent = '';
    if (box.hidden) return;
    const f = find(currentConv);
    const s = f && f.kind === 'ch' ? f.server : currentConv === 'room' && G.data ? G.data.servers.find((x) => x.id === 'mills') : null;
    const people = f ? members(currentConv) : currentConv === 'room' ? roomMembers.filter((m) => !m.agent).map((m) => m.username) : currentConv.slice(3).split('|');
    const agent = roomMembers.find((m) => m.agent);
    const shown = new Set();
    const row = (u) => {
      const r = el('div', 'mRow'); const dot = el('i', 'pdot ' + statusOf(u).cls); dot.style.background = colorOf(u);
      const av = typeof PROFILE !== 'undefined' && PROFILE.avatarOf(u); if (av) { dot.classList.add('hasAv'); dot.style.backgroundImage = `url("${av}")`; } // their picture, with the presence dot on it
      const nick = (s && s.nicks && s.nicks[u]) || nickOf(u);
      const nm = el('span', 'mName', nick + (u === me.username ? ' (you)' : '')); nm.style.color = (s && roleColor(s, u)) || colorOf(u);
      r.append(dot, nm);
      if (s && !s.everyone && s.owner === u) { const o = el('span', 'botTag', 'OWNER'); o.prepend(ic('crown')); o.title = 'owns ' + s.name; r.append(o); }
      else if (s) { const co = (s.roles || []).find((x) => x.builtin === 'coowner'); if (co && ((s.memberRoles || {})[u] || []).includes(co.id)) r.append(el('span', 'botTag', 'CO-OWNER')); }
      if (s && (s.temp || []).includes(u)) { const t = el('span', 'botTag', 'TEMP'); r.append(t); }
      if (s && s.timeouts && s.timeouts[u]) { const t = el('span', 'botTag', 'TIMEOUT'); t.title = 'timed out until ' + new Date(s.timeouts[u]).toLocaleString(); r.append(t); }
      if (s && (s.voiceMuted || []).includes(u)) { const t = el('span', 'botTag', 'MUTED'); t.title = 'muted in calls by a moderator'; r.append(t); }
      r.title = statusOf(u).text + (nick !== nickOf(u) ? ' · ' + nickOf(u) : '');
      r.dataset.profile = '1'; r.onclick = (ev) => PROFILE.show(u, ev); // their profile card (it has Message)
      if (s) r.oncontextmenu = (e) => { e.preventDefault(); GSET.openServer(s.id, 'members', u); };
      return r;
    };
    const section = (title, list) => { if (!list.length) return; box.append(el('div', 'mHead', title + ' — ' + list.length)); for (const u of list) { shown.add(u); box.append(row(u)); } };
    if (s && f && f.kind === 'ch' || (s && currentConv === 'room')) { // hoisted roles first (Discord), for the people online
      for (const r of (s.roles || []).filter((x) => x.hoist)) section(r.name, people.filter((u) => !shown.has(u) && statusOf(u).cls !== 'off' && ((s.memberRoles || {})[u] || []).includes(r.id)));
    }
    const groups = { online: [], away: [], offline: [] };
    for (const u of people) { if (shown.has(u)) continue; if (u === me.username) { groups.online.push(u); continue; } const st = statusOf(u).cls; (st === 'on' ? groups.online : st === 'idle' ? groups.away : groups.offline).push(u); }
    section('Online', groups.online); section('Away', groups.away); section('Offline', groups.offline);
    if (agent && (f || currentConv === 'room')) { // Unity is in every channel and group
      box.append(el('div', 'mHead', 'Bots — 1'));
      const r = el('div', 'mRow bot'); r.append(ic('bot', 'lead'), el('span', 'mName', agent.display), el('span', 'botTag', 'BOT')); r.title = '@' + agent.display.toLowerCase() + ' calls her';
      r.onclick = () => selectConv('dm:' + [me.username, agent.username].sort().join('|'));
      box.append(r);
    }
  }
  function render() { renderRail(); renderChannels(); renderMembers(); renderPins(); if (typeof renderAppUser === 'function') renderAppUser(); if (typeof updateUnread === 'function') updateUnread(); }
  // an invite link opened this page (?invite=CODE): offer the join once signed in
  function pendingInvite() {
    const code = new URLSearchParams(location.search).get('invite');
    if (!code) return;
    const u = new URL(location.href); u.searchParams.delete('invite'); history.replaceState(null, '', u.pathname + u.search + u.hash);
    setTimeout(() => GSET.join(code), 300);
  }
  return { isGuildConv, has, find, label, members, serverOf, refresh, render, renderMembers, renderPins, op, can, current, nickIn, canPost, canPin, canModerate, pinned, pin, mentionsOnly, pendingInvite, colorIn, roleColor, mentionableRoles, roleByHandle, mentionsMyRole, rolesOfUser, state: G };
})();
