// Mills servers, channels, groups and the member column, page side (hub side: bridge/guilds.js).
// Loaded after the page script and calls.js; uses the page's helpers at call time ($, json, me, nickOf,
// colorOf, statusOf, ic, label, selectConv, currentConv, roomMembers, sysRoom, store).
//
// The chat's side bar is Discord-shaped: a rail of servers on the far left (the "Mills" server is
// everyone's and always first; its #general is the old # Everyone), the picked server's channels, then
// group conversations and private messages. The member column (app mode, or wide screens) lists who is
// in the current conversation with their presence; Unity and "<person> · Claude" carry a BOT badge.
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
  const members = (c) => { const f = find(c); return !f ? [] : f.kind === 'ch' ? f.server.members : f.group.members; };
  const serverOf = (c) => { const f = find(c); return f && f.kind === 'ch' ? f.server.id : null; };
  async function refresh() {
    if (!me) return;
    try { G.data = await json('/api/guilds'); G.ver = G.data.ver; } catch { return; }
    if (!G.data.servers.some((s) => s.id === G.server)) G.server = 'mills';
    const sv = serverOf(currentConv); if (sv) G.server = sv;
    render();
  }
  async function op(path, body) {
    try { const d = await json('/api/guilds/' + path, { method: 'POST', body: JSON.stringify(body) }); G.data = d; G.ver = d.ver; render(); return d.result; }
    catch (e) { sysRoom(e.message); return null; }
  }
  // ── server rail ──
  function renderRail() {
    const rail = $('serverRail'); if (!rail || !G.data) return;
    rail.textContent = '';
    for (const s of G.data.servers) {
      const b = el('button', 'srv' + (s.id === G.server ? ' on' : '')); b.type = 'button'; b.title = s.name + (s.everyone ? ' · everyone' : ' · ' + s.members.length + ' member' + (s.members.length === 1 ? '' : 's'));
      b.textContent = (s.icon || s.name).slice(0, 2).toUpperCase(); b.style.background = s.id === 'mills' ? 'var(--accent)' : colorOf(s.createdBy || s.id);
      const badge = el('span', 'badge'); badge.hidden = true; badge.dataset.serverBadge = s.id; b.append(badge);
      b.onclick = () => { G.server = s.id; render(); const first = s.channels[0]; if (first && serverOf(currentConv) !== s.id && currentConv !== first.conv) selectConv(first.conv); };
      rail.append(b);
    }
    const add = el('button', 'srv add'); add.type = 'button'; add.title = 'Make a server'; add.append(ic('plus')); add.onclick = newServer; rail.append(add);
  }
  // ── channels of the picked server, then groups ──
  function renderChannels() {
    const box = $('chanList'); if (!box || !G.data) return;
    box.textContent = '';
    const s = G.data.servers.find((x) => x.id === G.server) || G.data.servers[0];
    if (!s) return;
    const head = el('div', 'chHead');
    const name = el('b', '', s.name); head.append(name);
    if (!s.everyone) {
      const inv = el('button', 'ghost tiny'); inv.type = 'button'; inv.title = 'Add people to this server'; inv.append(ic('plus')); inv.onclick = () => addPeople(s); head.append(inv);
      if (s.createdBy === me.username) { const del = el('button', 'ghost tiny'); del.type = 'button'; del.title = 'Delete this server (twice)'; del.append(ic('trash')); del.onclick = () => { if (del.dataset.armed !== '1') { del.dataset.armed = '1'; del.classList.add('armed'); return; } op('server/delete', { server: s.id }).then(() => { G.server = 'mills'; selectConv('room'); }); }; head.append(del); }
    }
    box.append(head);
    const sect = el('div', 'chSect'); sect.append(el('span', '', 'Text channels'));
    const plus = el('button', 'ghost tiny'); plus.type = 'button'; plus.title = 'New channel'; plus.append(ic('plus')); plus.onclick = () => newChannel(s); sect.append(plus); box.append(sect);
    for (const ch of s.channels) box.append(convButton(ch.conv, '#', ch.name, ch.topic || (ch.conv === 'room' ? 'the whole team' : ''), ch));
    const gs = el('div', 'chSect'); gs.append(el('span', '', 'Groups'));
    const gp = el('button', 'ghost tiny'); gp.type = 'button'; gp.title = 'New group conversation'; gp.append(ic('plus')); gp.onclick = newGroup; gs.append(gp); box.append(gs);
    for (const g of G.data.groups) box.append(convButton(g.conv, '', g.name, g.members.map((u) => nickOf(u)).join(', '), null, g));
    if (!G.data.groups.length) box.append(el('div', 'chNone', 'no groups yet'));
    const dmHead = el('div', 'chSect'); dmHead.append(el('span', '', 'Direct messages')); box.append(dmHead);
  }
  function convButton(conv, prefix, name, sub, ch, g) {
    const b = el('button', 'cv ch' + (conv === currentConv ? ' on' : '')); b.type = 'button';
    const dot = el('span', 'dot hash', prefix || ''); if (!prefix) dot.append(ic('chat'));
    const nm = el('span', 'nm', name); const s = el('small', '', sub); nm.append(s);
    const badge = el('span', 'badge'); badge.dataset.convBadge = conv; badge.hidden = true;
    b.append(dot, nm, badge); b.onclick = () => selectConv(conv);
    if (ch && conv !== 'room' && (ch.createdBy === me.username || (G.data.servers.find((x) => x.id === G.server) || {}).createdBy === me.username)) {
      const x = el('button', 'ghost tiny chDel'); x.type = 'button'; x.title = 'Delete channel (twice)'; x.append(ic('close'));
      x.onclick = (e) => { e.stopPropagation(); if (x.dataset.armed !== '1') { x.dataset.armed = '1'; x.classList.add('armed'); return; } op('channel/delete', { server: G.server, channel: ch.id }).then(() => { if (currentConv === conv) selectConv('room'); }); };
      b.append(x);
    }
    if (g) {
      const x = el('button', 'ghost tiny chDel'); x.type = 'button'; x.title = 'Leave this group (twice)'; x.append(ic('close'));
      x.onclick = (e) => { e.stopPropagation(); if (x.dataset.armed !== '1') { x.dataset.armed = '1'; x.classList.add('armed'); return; } op('group/leave', { group: g.id }).then(() => { if (currentConv === conv) selectConv('room'); }); };
      b.append(x);
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
    const people = f ? members(currentConv) : currentConv === 'room' ? roomMembers.filter((m) => !m.agent).map((m) => m.username) : currentConv.slice(3).split('|');
    const agent = roomMembers.find((m) => m.agent);
    const groups = { online: [], away: [], offline: [] };
    for (const u of people) { if (u === me.username) { groups.online.push(u); continue; } const st = statusOf(u).cls; (st === 'on' ? groups.online : st === 'idle' ? groups.away : groups.offline).push(u); }
    const section = (title, list) => {
      if (!list.length) return;
      box.append(el('div', 'mHead', title + ' — ' + list.length));
      for (const u of list) {
        const r = el('div', 'mRow'); const dot = el('i', 'pdot ' + statusOf(u).cls); dot.style.background = colorOf(u);
        const nm = el('span', 'mName', nickOf(u) + (u === me.username ? ' (you)' : '')); nm.style.color = colorOf(u);
        r.append(dot, nm); r.title = statusOf(u).text; r.onclick = () => { if (u !== me.username) selectConv('dm:' + [me.username, u].sort().join('|')); };
        box.append(r);
      }
    };
    section('Online', groups.online); section('Away', groups.away); section('Offline', groups.offline);
    if (agent && (f || currentConv === 'room')) { // Unity is in every channel and group
      box.append(el('div', 'mHead', 'Bots — 1'));
      const r = el('div', 'mRow bot'); r.append(ic('bot', 'lead'), el('span', 'mName', agent.display), el('span', 'botTag', 'BOT')); r.title = '@' + agent.display.toLowerCase() + ' calls her';
      r.onclick = () => selectConv('dm:' + [me.username, agent.username].sort().join('|'));
      box.append(r);
    }
  }
  function render() { renderRail(); renderChannels(); renderMembers(); if (typeof updateUnread === 'function') updateUnread(); }
  // ── dialogs (prompt-based, like the board) ──
  async function newServer() {
    const name = window.prompt('Name of the new server:'); if (!name || !name.trim()) return;
    const others = window.prompt('Who else is in it? Usernames, comma separated (you can add people later):', '') || '';
    const s = await op('server', { name: name.trim(), members: others.split(/[,\s]+/).filter(Boolean) });
    if (s) { G.server = s.id; render(); selectConv('ch:' + s.id + '/general'); }
  }
  async function newChannel(s) {
    const name = window.prompt('New channel in ' + s.name + ' (letters, digits, dashes):'); if (!name || !name.trim()) return;
    const topic = window.prompt('Topic (optional):', '') || '';
    const ch = await op('channel', { server: s.id, name: name.trim(), topic });
    if (ch) selectConv('ch:' + s.id + '/' + ch.id);
  }
  async function addPeople(s) {
    const who = window.prompt('Add to ' + s.name + ' (usernames, comma separated):', roomMembers.filter((m) => !m.agent && !s.members.includes(m.username)).map((m) => m.username).join(', '));
    if (!who || !who.trim()) return;
    await op('server/add', { server: s.id, add: who.split(/[,\s]+/).filter(Boolean) });
  }
  async function newGroup() {
    const who = window.prompt('Group with (usernames, comma separated):', ''); if (!who || !who.trim()) return;
    const name = window.prompt('Group name (optional):', '') || '';
    const g = await op('group', { name, members: who.split(/[,\s]+/).filter(Boolean) });
    if (g) selectConv('grp:' + g.id);
  }
  return { isGuildConv, has, find, label, members, serverOf, refresh, render, renderMembers, state: G };
})();
