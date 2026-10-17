// Mills Server Settings and the server / group dialogs (page side; hub side: bridge/guilds.js, roles in
// bridge/guild-perms.js). Loaded after web/guilds.js; uses GUILDS plus the page's helpers ($, json, me,
// nickOf, colorOf, roomMembers, selectConv, currentConv, ic).
//
// One modal, rebuilt from the latest /api/guilds view every time something changes (GSET.refreshOpen):
//   Server Settings  Overview (name, icon, colour, description, your nickname, leave) · Members (roles, nickname,
//                    kick, ban, pending invitations, invite by name) · Invites (links with expiry / max uses /
//                    temporary, copy, revoke) · Roles (@everyone + named roles: colour, shown separately, position,
//                    permissions) · Channels (order, edit, delete, new) · Bans · Security (password lock) · Audit log
//                    · Ownership (hand over, delete). Each tab shows only what your permissions allow.
//   Dialogs          new server, join by invite code or link (+ password when locked), invitations, new / edit
//                    channel (category, topic, announcement, private to roles), new group, group settings.
const GSET = (() => {
  const S = { open: null }; // { kind: 'server', id, tab, focus } | { kind: 'channel', server, channel } | { kind: 'group', id } | { kind: 'join', code } | ...
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const btn = (text, fn, cls) => { const b = el('button', cls || '', text); b.type = 'button'; b.onclick = fn; return b; };
  const armed = (text, fn) => { const b = btn(text, () => { if (b.dataset.armed !== '1') { b.dataset.armed = '1'; b.textContent = 'Click again: ' + text.toLowerCase(); b.classList.add('armed'); return; } fn(); }, 'ghost'); return b; };
  const input = (value, ph, type) => { const i = el('input'); i.value = value || ''; if (ph) i.placeholder = ph; if (type) i.type = type; i.autocomplete = 'off'; i.spellcheck = false; return i; };
  const row = (...kids) => { const d = el('div', 'row'); d.append(...kids.filter(Boolean)); return d; };
  const sect = (title, ...kids) => { const d = el('div', 'setSection'); d.append(el('b', '', title), ...kids.filter(Boolean)); return d; };
  const check = (text, on) => { const l = el('label', 'chk'); const c = el('input'); c.type = 'checkbox'; c.checked = !!on; l.append(c, ' ' + text); l.box = c; return l; };
  const data = () => GUILDS.state.data;
  const srv = (id) => data() && data().servers.find((s) => s.id === id);
  const can = (s, p) => GUILDS.can(s, p);
  const nick = (s, u) => (s && s.nicks && s.nicks[u]) || nickOf(u);
  const inviteUrl = (code) => location.origin + location.pathname + '?invite=' + code;
  const when = (iso) => (iso ? new Date(iso).toLocaleString() : '');
  const people = () => roomMembers.filter((m) => !m.agent).map((m) => m.username);
  let modal, titleEl, tabsEl, bodyEl, msgEl;
  function shell() {
    if (modal) return;
    modal = el('div', 'modal'); modal.id = 'gsetModal'; modal.hidden = true;
    const box = el('div', 'box gsetBox');
    const bar = el('div', 'bar'); titleEl = el('span'); bar.append(titleEl, btn('Close', close, 'ghost'));
    tabsEl = el('div', 'gsetTabs'); bodyEl = el('div', 'setBody'); msgEl = el('div', 'status');
    box.append(bar, tabsEl, msgEl, bodyEl); modal.append(box); document.body.append(modal);
    modal.onclick = (e) => { if (e.target === modal) close(); };
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !modal.hidden) close(); });
  }
  function close() { if (modal) modal.hidden = true; S.open = null; }
  function show(open) { shell(); S.open = open; msgEl.textContent = ''; draw(); modal.hidden = false; }
  const say = (t) => { if (msgEl) msgEl.textContent = t || ''; };
  async function op(path, body) { try { const r = await GUILDS.op(path, body, true); say(''); return r; } catch (e) { say(e.message); return null; } }
  function refreshOpen() { if (S.open && modal && !modal.hidden) draw(); }
  function draw() {
    const o = S.open; if (!o) return;
    tabsEl.textContent = ''; bodyEl.textContent = ''; tabsEl.hidden = true;
    if (o.kind === 'server') return drawServer(o);
    if (o.kind === 'channel') return drawChannel(o);
    if (o.kind === 'group') return drawGroup(o);
    if (o.kind === 'newServer') return drawNewServer();
    if (o.kind === 'newGroup') return drawNewGroup();
    if (o.kind === 'join') return drawJoin(o);
    if (o.kind === 'invitations') return drawInvitations();
  }

  // ── Server Settings ──
  const TABS = [
    ['overview', 'Overview', () => true], ['members', 'Members', () => true], ['invites', 'Invites', (s) => !s.everyone && (can(s, 'INVITE') || can(s, 'MANAGE_INVITES'))],
    ['roles', 'Roles', (s) => can(s, 'MANAGE_ROLES') || (s.roles || []).length > 0], ['channels', 'Channels', (s) => can(s, 'MANAGE_CHANNELS') || can(s, 'CREATE_CHANNELS')],
    ['bans', 'Bans', (s) => !s.everyone && can(s, 'BAN')], ['security', 'Security', (s) => !s.everyone && can(s, 'MANAGE_SERVER')],
    ['audit', 'Audit log', (s) => can(s, 'MANAGE_SERVER')], ['owner', 'Ownership', (s) => !s.everyone && s.me.owner],
  ];
  function drawServer(o) {
    const s = srv(o.id);
    if (!s) { titleEl.textContent = 'Server'; bodyEl.append(el('div', '', 'You are not in that server any more.')); return; }
    titleEl.textContent = s.name + ' · Server Settings';
    tabsEl.hidden = false;
    const tabs = TABS.filter(([, , ok]) => ok(s));
    if (!tabs.some(([id]) => id === o.tab)) o.tab = 'overview';
    for (const [id, name] of tabs) { const b = btn(name, () => { o.tab = id; say(''); draw(); }, 'ghost' + (id === o.tab ? ' on' : '')); tabsEl.append(b); }
    ({ overview, members, invites, roles, channels, bans, security, audit, owner })[o.tab](s, o);
  }
  function overview(s) {
    const edit = can(s, 'MANAGE_SERVER');
    const name = input(s.name, 'server name'); name.maxLength = 40; name.disabled = !edit;
    const icon = input(s.icon, 'icon: 1–2 letters'); icon.maxLength = 2; icon.disabled = !edit; icon.className = 'gsetIcon';
    const color = input(s.color || '#7c5cff', '', 'color'); color.disabled = !edit;
    const noColor = check('no colour (use the default)', !s.color); noColor.box.disabled = !edit;
    const desc = el('textarea'); desc.value = s.description || ''; desc.maxLength = 300; desc.rows = 3; desc.placeholder = 'what this server is for'; desc.disabled = !edit;
    bodyEl.append(sect('Server', row('Name ', name), row('Icon ', icon, ' Colour ', color, noColor), desc,
      edit ? row(btn('Save', () => op('server/update', { server: s.id, name: name.value, icon: icon.value, color: noColor.box.checked ? '' : color.value, description: desc.value }))) : el('small', '', 'Only people with "manage server" change these.')));
    const myNick = input((s.nicks || {})[me.username] || '', nickOf(me.username)); myNick.maxLength = 32;
    bodyEl.append(sect('Your nickname in ' + s.name, el('small', '', 'Shown instead of your name inside this server only.'), row(myNick, btn('Save', () => op('member/nick', { server: s.id, user: me.username, nick: myNick.value })))));
    const facts = [s.everyone ? 'Everyone with a Mills account is in this server.' : `${s.members.length} member${s.members.length === 1 ? '' : 's'} · owner ${nick(s, s.owner)}`, s.locked ? 'Password locked: joining with an invite code asks for its password.' : '', 'Your permissions: ' + (s.me.perms.map((p) => p.toLowerCase().replace(/_/g, ' ')).join(', ') || 'none')].filter(Boolean);
    bodyEl.append(sect('About', ...facts.map((t) => el('div', '', t))));
    if (!s.everyone && !s.me.owner) bodyEl.append(sect('Leave', armed('Leave ' + s.name, async () => { if (await op('server/leave', { server: s.id })) { close(); GUILDS.state.server = 'mills'; selectConv('room'); } })));
  }
  function members(s, o) {
    if (!s.everyone && can(s, 'INVITE')) {
      const who = input('', 'usernames, comma separated'); who.setAttribute('list', 'gsetPeople');
      const dl = el('datalist'); dl.id = 'gsetPeople'; for (const u of people().filter((u) => !s.members.includes(u))) { const op_ = el('option'); op_.value = u; op_.label = nickOf(u); dl.append(op_); }
      bodyEl.append(sect('Invite people', el('small', '', 'They get an invitation to accept (a personal invitation skips the password lock).'), row(who, dl, btn('Invite', () => op('server/invite', { server: s.id, add: who.value.split(/[,\s]+/).filter(Boolean) }))),
        ...(s.pending || []).map((p) => row(el('span', '', nickOf(p.user) + ' · invited by ' + nickOf(p.by) + ' ' + when(p.at)), btn('Cancel', () => op('invitation/cancel', { server: s.id, user: p.user }), 'ghost')))));
    }
    const list = sect('Members — ' + s.members.length);
    const myRank = s.me.rank;
    const rankOf = (u) => (u === s.owner && !s.everyone ? 1e9 : Math.max(0, ...((s.memberRoles || {})[u] || []).map((id) => ((s.roles || []).find((r) => r.id === id) || {}).position || 0)));
    for (const u of [...s.members].sort((a, b) => rankOf(b) - rankOf(a) || nick(s, a).localeCompare(nick(s, b)))) {
      const it = el('div', 'gsetMember' + (o.focus === u ? ' focus' : ''));
      const nm = el('b', '', nick(s, u)); nm.style.color = colorOf(u); it.append(nm);
      const sub = [nick(s, u) !== nickOf(u) ? nickOf(u) : '', !s.everyone && s.owner === u ? 'owner' : '', (s.temp || []).includes(u) ? 'temporary member' : '',
        ...((s.memberRoles || {})[u] || []).map((id) => ((s.roles || []).find((r) => r.id === id) || {}).name).filter(Boolean)].filter(Boolean).join(' · ');
      it.append(el('small', '', sub || 'no roles'));
      const below = u !== me.username && myRank > rankOf(u);
      const acts = el('div', 'row');
      if (can(s, 'MANAGE_ROLES') && (s.roles || []).length && (below || s.me.owner)) acts.append(btn('Roles', () => { o.focus = u; o.rolesFor = o.rolesFor === u ? '' : u; draw(); }, 'ghost'));
      if (can(s, 'MANAGE_NICKNAMES') && below) acts.append(btn('Nickname', async () => { const n = window.prompt('Server nickname for ' + nickOf(u) + ' (empty = none):', (s.nicks || {})[u] || ''); if (n !== null) op('member/nick', { server: s.id, user: u, nick: n }); }, 'ghost'));
      if (!s.everyone && can(s, 'KICK') && below) acts.append(armed('Kick', () => { const r = window.prompt('Reason (optional, kept in the audit log):', '') || ''; op('member/kick', { server: s.id, user: u, reason: r }); }));
      if (!s.everyone && can(s, 'BAN') && below) acts.append(armed('Ban', () => { const r = window.prompt('Reason (optional):', '') || ''; op('member/ban', { server: s.id, user: u, reason: r }); }));
      if (acts.childNodes.length) it.append(acts);
      if (o.rolesFor === u) {
        const boxes = (s.roles || []).map((r) => { const c = check(r.name, ((s.memberRoles || {})[u] || []).includes(r.id)); c.box.disabled = !(s.me.owner || myRank > r.position); c.role = r.id; return c; });
        it.append(row(...boxes, btn('Save roles', () => op('member/roles', { server: s.id, user: u, roles: boxes.filter((c) => c.box.checked).map((c) => c.role) }))));
      }
      list.append(it);
    }
    bodyEl.append(list);
  }
  function invites(s) {
    const D = data();
    const age = el('select'); for (const m of D.inviteAges) { const op_ = el('option', '', m === 0 ? 'never expires' : 'expires after ' + (m >= 1440 ? m / 1440 + ' day' + (m > 1440 ? 's' : '') : m >= 60 ? m / 60 + ' hour' + (m > 60 ? 's' : '') : m + ' minutes')); op_.value = m; age.append(op_); } age.value = '1440';
    const uses = el('select'); for (const n of [0, 1, 5, 10, 25, 100]) { const op_ = el('option', '', n ? 'up to ' + n + ' use' + (n === 1 ? '' : 's') : 'no use limit'); op_.value = n; uses.append(op_); }
    const temp = check('temporary membership (they leave when they go offline, unless given a role)');
    const out = el('div');
    bodyEl.append(sect('Make an invite link', el('small', '', 'Anyone with a Mills account who has the link (and the password, if the server is locked) can join.'), row(age, uses), temp,
      row(btn('Make invite', async () => { const iv = await op('invite', { server: s.id, maxAge: Number(age.value), maxUses: Number(uses.value), temporary: temp.box.checked }); if (iv && iv.code) { out.textContent = ''; out.append(linkRow(iv.code)); } })), out));
    const list = sect('Open invites');
    for (const iv of s.invites || []) {
      const it = el('div', 'gsetMember');
      it.append(el('b', '', iv.code), el('small', '', [nickOf(iv.by), iv.uses + (iv.maxUses ? ' / ' + iv.maxUses : '') + ' uses', iv.expiresAt ? 'expires ' + when(iv.expiresAt) : 'never expires', iv.temporary ? 'temporary' : ''].filter(Boolean).join(' · ')));
      it.append(row(linkRow(iv.code), btn('Revoke', () => op('invite/revoke', { code: iv.code }), 'ghost')));
      list.append(it);
    }
    if (!(s.invites || []).length) list.append(el('small', '', 'No open invites.'));
    bodyEl.append(list);
  }
  function linkRow(code) {
    const i = input(inviteUrl(code)); i.readOnly = true;
    const c = btn('Copy link', async () => { try { await navigator.clipboard.writeText(i.value); c.textContent = 'Copied'; } catch { i.select(); c.textContent = 'Select + copy'; } }, 'ghost');
    return row(i, c);
  }
  function permBoxes(perms, disabledAdmin) {
    const D = data();
    return Object.entries(D.perms).map(([id, text]) => { const c = check(id.toLowerCase().replace(/_/g, ' '), perms.includes(id)); c.title = text; c.perm = id; if (id === 'ADMIN' && disabledAdmin) c.box.disabled = true; return c; });
  }
  function roles(s) {
    const edit = can(s, 'MANAGE_ROLES'), admin = can(s, 'ADMIN');
    const ev = permBoxes(s.everyonePerms || [], true).filter((c) => c.perm !== 'ADMIN'); ev.forEach((c) => { c.box.disabled = !edit; });
    bodyEl.append(sect('@everyone', el('small', '', 'What every member may do.'), el('div', 'gsetPerms'), ...(edit ? [row(btn('Save @everyone', () => op('server/update', { server: s.id, everyonePerms: ev.filter((c) => c.box.checked).map((c) => c.perm) })))] : [])));
    bodyEl.lastChild.querySelector('.gsetPerms').append(...ev);
    if (edit) {
      const nm = input('', 'new role name'); nm.maxLength = 32;
      const col = input('#3ba55d', '', 'color');
      bodyEl.append(sect('New role', row(nm, col, btn('Make role', () => op('role', { server: s.id, name: nm.value, color: col.value, perms: [] })))));
    }
    for (const r of s.roles || []) {
      const mine = s.me.owner || s.me.rank > r.position;
      const nm = input(r.name); nm.maxLength = 32; nm.disabled = !edit || !mine;
      const col = input(r.color || '#99aab5', '', 'color'); col.disabled = !edit || !mine;
      const hoist = check('show its members separately', r.hoist); hoist.box.disabled = !edit || !mine;
      const boxes = permBoxes(r.perms, !admin); boxes.forEach((c) => { if (!edit || !mine) c.box.disabled = true; });
      const who = s.members.filter((u) => ((s.memberRoles || {})[u] || []).includes(r.id)).map((u) => nick(s, u)).join(', ');
      const box = sect(r.name + ' · position ' + r.position, el('small', '', who ? 'held by ' + who : 'nobody has it yet'), row(nm, col, hoist), el('div', 'gsetPerms'));
      box.querySelector('.gsetPerms').append(...boxes);
      if (edit && mine) box.append(row(btn('Save', () => op('role/update', { server: s.id, role: r.id, name: nm.value, color: col.value, hoist: hoist.box.checked, perms: boxes.filter((c) => c.box.checked).map((c) => c.perm) })),
        btn('Move up', () => op('role/update', { server: s.id, role: r.id, position: r.position + 1 }), 'ghost'), btn('Move down', () => op('role/update', { server: s.id, role: r.id, position: Math.max(1, r.position - 1) }), 'ghost'),
        armed('Delete role', () => op('role/delete', { server: s.id, role: r.id }))));
      else if (!mine) box.append(el('small', '', 'Above or level with your own highest role: you cannot change it.'));
      bodyEl.append(box);
    }
  }
  function channels(s) {
    const box = sect('Channels', el('small', '', 'The order here is the order in the side bar. #general always stays.'));
    s.channels.forEach((ch) => {
      const mineOrAll = can(s, 'MANAGE_CHANNELS') || ch.createdBy === me.username;
      const r = row(el('b', '', (ch.private ? 'private ' : '') + (ch.announce ? 'announcement ' : '') + '#' + ch.name), el('small', '', [ch.category, ch.topic].filter(Boolean).join(' · ')));
      if (can(s, 'MANAGE_CHANNELS')) r.append(btn('Up', () => op('channel/move', { server: s.id, channel: ch.id, dir: 'up' }), 'ghost'), btn('Down', () => op('channel/move', { server: s.id, channel: ch.id, dir: 'down' }), 'ghost'));
      if (mineOrAll) r.append(btn('Edit', () => editChannel(s, ch), 'ghost'));
      box.append(r);
    });
    if (can(s, 'CREATE_CHANNELS') || can(s, 'MANAGE_CHANNELS')) box.append(row(btn('New channel', () => newChannel(s))));
    bodyEl.append(box);
  }
  function bans(s) {
    const box = sect('Bans', el('small', '', 'Banned people cannot come back by invite link or invitation until unbanned.'));
    for (const b of s.bans || []) box.append(row(el('b', '', nickOf(b.user)), el('small', '', 'by ' + nickOf(b.by) + ' ' + when(b.at) + (b.reason ? ' · ' + b.reason : '')), btn('Unban', () => op('member/unban', { server: s.id, user: b.user }), 'ghost')));
    if (!(s.bans || []).length) box.append(el('small', '', 'Nobody is banned.'));
    bodyEl.append(box);
  }
  function security(s) {
    const pw = input('', s.locked ? 'new password' : 'password', 'password'); const pw2 = input('', 'the same again', 'password');
    bodyEl.append(sect('Password lock', el('div', '', s.locked ? 'This server is password locked.' : 'This server has no password.'),
      el('small', '', 'With a password, joining by invite link asks for it (5 wrong tries per 10 minutes). Personal invitations from a member skip it. The hub keeps only a salted hash.'),
      row(pw, pw2, btn(s.locked ? 'Change password' : 'Lock with password', () => { if (pw.value !== pw2.value) return say('The two passwords differ.'); op('server/password', { server: s.id, password: pw.value }); })),
      s.locked ? row(armed('Remove the password lock', () => op('server/password', { server: s.id, password: '' }))) : null));
  }
  function audit(s) {
    const box = sect('Audit log', el('small', '', 'The last changes, newest first.'));
    for (const a of s.audit || []) box.append(el('div', 'gsetAudit', when(a.at) + ' · ' + nick(s, a.by) + ' ' + a.what + (a.detail ? ' ' + a.detail : '')));
    if (!(s.audit || []).length) box.append(el('small', '', 'Nothing yet.'));
    bodyEl.append(box);
  }
  function owner(s) {
    const to = el('select'); for (const u of s.members.filter((u) => u !== me.username)) { const op_ = el('option', '', nick(s, u)); op_.value = u; to.append(op_); }
    bodyEl.append(sect('Hand the server over', el('small', '', 'The new owner gets every permission; you stay as a member.'), row(to, armed('Make them the owner', () => to.value && op('server/transfer', { server: s.id, user: to.value })))));
    bodyEl.append(sect('Delete the server', el('small', '', 'Its channels and settings go for everyone. Messages already sent stay encrypted on the host but nobody can open the channels again.'),
      row(armed('Delete ' + s.name, async () => { if (await op('server/delete', { server: s.id })) { close(); GUILDS.state.server = 'mills'; selectConv('room'); } }))));
  }

  // ── channel dialog (new or edit) ──
  function newChannel(s) { show({ kind: 'channel', server: s.id, channel: '' }); }
  function editChannel(s, ch) { show({ kind: 'channel', server: s.id, channel: ch.id }); }
  function drawChannel(o) {
    const s = srv(o.server); if (!s) return close();
    const ch = o.channel ? s.channels.find((c) => c.id === o.channel) : null;
    if (o.channel && !ch) return close();
    titleEl.textContent = (ch ? 'Edit #' + ch.name : 'New channel') + ' · ' + s.name;
    const name = input(ch ? ch.name : '', 'name: letters, digits, dashes'); name.maxLength = 32;
    const topic = input(ch ? ch.topic : '', 'topic (optional)'); topic.maxLength = 120;
    const cats = [...new Set(s.channels.map((c) => c.category).filter(Boolean))];
    const cat = input(ch ? ch.category : '', 'category (optional), e.g. Projects'); cat.maxLength = 32; cat.setAttribute('list', 'gsetCats');
    const dl = el('datalist'); dl.id = 'gsetCats'; for (const c of cats) { const op_ = el('option'); op_.value = c; dl.append(op_); }
    const ann = check('announcement channel: only people with "announce" post', ch && ch.announce);
    const general = ch && ch.id === 'general';
    const roleBoxes = (s.roles || []).map((r) => { const c = check(r.name, ch && ch.roles.includes(r.id)); c.role = r.id; return c; });
    bodyEl.append(sect('Channel', row(name, topic), row(cat, dl), ann));
    if (!(s.everyone && general)) bodyEl.append(sect('Private channel', el('small', '', (s.roles || []).length ? 'Tick roles to make it private: only those roles (plus the owner and administrators) see and read it. Nothing ticked = everyone in the server.' : 'Make roles first (Server Settings → Roles) to have private channels.'), row(...roleBoxes)));
    const pick = () => roleBoxes.filter((c) => c.box.checked).map((c) => c.role);
    const actions = row(btn(ch ? 'Save' : 'Make channel', async () => {
      const body = { server: s.id, name: name.value, topic: topic.value, category: cat.value, announce: ann.box.checked, roles: pick() };
      if (ch) { if (await op('channel/update', { ...body, channel: ch.id })) close(); }
      else { const made = await op('channel', body); if (made && made.id) { close(); selectConv('ch:' + s.id + '/' + made.id); } }
    }));
    if (ch && !general) actions.append(armed('Delete channel', async () => { if (await op('channel/delete', { server: s.id, channel: ch.id })) { close(); if (currentConv === 'ch:' + s.id + '/' + ch.id) selectConv('room'); } }));
    bodyEl.append(actions);
  }

  // ── groups ──
  function newGroup() { show({ kind: 'newGroup' }); }
  function drawNewGroup() {
    titleEl.textContent = 'New group conversation';
    const who = input('', 'usernames, comma separated (up to ' + (data().groupMax - 1) + ' others)'); who.setAttribute('list', 'gsetPeople2');
    const dl = el('datalist'); dl.id = 'gsetPeople2'; for (const u of people().filter((u) => u !== me.username)) { const op_ = el('option'); op_.value = u; op_.label = nickOf(u); dl.append(op_); }
    const name = input('', 'group name (optional)'); name.maxLength = 40;
    bodyEl.append(sect('People', row(who, dl)), sect('Name', name), row(btn('Make group', async () => { const g = await op('group', { name: name.value, members: who.value.split(/[,\s]+/).filter(Boolean) }); if (g && g.id) { close(); selectConv('grp:' + g.id); } })));
  }
  function editGroup(id) { show({ kind: 'group', id }); }
  function drawGroup(o) {
    const g = data() && data().groups.find((x) => x.id === o.id); if (!g) return close();
    const own = g.owner === me.username;
    titleEl.textContent = g.name + ' · Group settings';
    const name = input(g.name); name.maxLength = 40;
    bodyEl.append(sect('Name', row(name, btn('Rename', () => op('group/rename', { group: g.id, name: name.value })))));
    const list = sect('People — ' + g.members.length + ' of ' + data().groupMax);
    for (const u of g.members) {
      const r = row(el('b', '', nickOf(u) + (u === me.username ? ' (you)' : '')), el('small', '', u === g.owner ? 'owner' : ''));
      if (own && u !== me.username) r.append(btn('Make owner', () => op('group/transfer', { group: g.id, user: u }), 'ghost'), armed('Remove', () => op('group/remove', { group: g.id, user: u })));
      list.append(r);
    }
    const add = input('', 'add people: usernames'); add.setAttribute('list', 'gsetPeople3');
    const dl = el('datalist'); dl.id = 'gsetPeople3'; for (const u of people().filter((u) => !g.members.includes(u))) { const op_ = el('option'); op_.value = u; op_.label = nickOf(u); dl.append(op_); }
    list.append(row(add, dl, btn('Add', () => op('group/add', { group: g.id, add: add.value.split(/[,\s]+/).filter(Boolean) }))));
    bodyEl.append(list);
    bodyEl.append(sect('Leave', el('small', '', own ? 'You own this group; leaving hands it to the next member.' : ''), row(armed('Leave the group', async () => { if (await op('group/leave', { group: g.id })) { close(); if (currentConv === g.conv) selectConv('room'); } }))));
  }

  // ── new server, join, invitations ──
  function newServer() { show({ kind: 'newServer' }); }
  function drawNewServer() {
    titleEl.textContent = 'Make a server';
    const name = input('', 'server name'); name.maxLength = 40;
    const icon = input('', 'icon: 1–2 letters'); icon.maxLength = 2; icon.className = 'gsetIcon';
    const color = input('#7c5cff', '', 'color');
    const desc = el('textarea'); desc.rows = 2; desc.maxLength = 300; desc.placeholder = 'what it is for (optional)';
    const who = input('', 'invite people now (usernames, optional)'); who.setAttribute('list', 'gsetPeople4');
    const dl = el('datalist'); dl.id = 'gsetPeople4'; for (const u of people().filter((u) => u !== me.username)) { const op_ = el('option'); op_.value = u; op_.label = nickOf(u); dl.append(op_); }
    bodyEl.append(sect('Server', row(name), row(icon, color), desc), sect('People', el('small', '', 'They get an invitation to accept. You can also make invite links later.'), row(who, dl)),
      row(btn('Make server', async () => {
        const s = await op('server', { name: name.value, icon: icon.value, color: color.value, description: desc.value, members: who.value.split(/[,\s]+/).filter(Boolean) });
        if (s && s.id) { close(); GUILDS.state.server = s.id; GUILDS.render(); selectConv('ch:' + s.id + '/general'); }
      })));
    name.focus();
  }
  function join(code) { show({ kind: 'join', code: code || '', preview: null }); }
  function drawJoin(o) {
    titleEl.textContent = 'Join a server';
    const code = input(o.code, 'invite link or code');
    const out = el('div');
    const look = async () => {
      o.code = code.value.trim(); out.textContent = '';
      try { const r = await json('/api/guilds/invite/preview', { method: 'POST', body: JSON.stringify({ code: o.code }) }); o.preview = r.result; say(''); drawPreview(o, out); }
      catch (e) { say(e.message); }
    };
    code.onkeydown = (e) => { if (e.key === 'Enter') look(); };
    bodyEl.append(sect('Invite', el('small', '', 'Paste the link someone sent you (or just its code).'), row(code, btn('Look', look))), out);
    if (o.code) look();
  }
  function drawPreview(o, out) {
    const p = o.preview; if (!p) return;
    const head = el('div', 'gsetPreview'); const badge = el('span', 'srv', (p.icon || p.name).slice(0, 2).toUpperCase()); badge.style.background = p.color || colorOf(p.server);
    head.append(badge, el('b', '', p.name), el('small', '', p.members + ' member' + (p.members === 1 ? '' : 's') + ' · invited by ' + nickOf(p.by) + (p.temporary ? ' · temporary membership' : '') + (p.locked ? ' · password locked' : '')));
    out.append(head);
    if (p.description) out.append(el('div', '', p.description));
    if (p.already) { out.append(row(btn('Open it', () => { close(); GUILDS.state.server = p.server; GUILDS.render(); selectConv('ch:' + p.server + '/general'); }))); return; }
    if (p.banned) { out.append(el('div', 'status', 'You are banned from this server.')); return; }
    const pw = p.locked ? input('', 'server password', 'password') : null;
    const go = btn('Join ' + p.name, async () => {
      try { const d = await json('/api/guilds/invite/join', { method: 'POST', body: JSON.stringify({ code: o.code, password: pw ? pw.value : '' }) }); GUILDS.state.data = d; GUILDS.state.ver = d.ver; close(); GUILDS.state.server = p.server; GUILDS.render(); selectConv('ch:' + p.server + '/general'); }
      catch (e) { say(e.message); }
    });
    if (pw) pw.onkeydown = (e) => { if (e.key === 'Enter') go.click(); };
    out.append(row(pw, go));
    if (pw) pw.focus();
  }
  function invitations() { show({ kind: 'invitations' }); }
  function drawInvitations() {
    titleEl.textContent = 'Server invitations';
    const list = data().invitations || [];
    if (!list.length) { bodyEl.append(el('div', '', 'No invitations right now.')); return; }
    for (const iv of list) {
      bodyEl.append(sect(iv.name, el('small', '', 'invited by ' + nickOf(iv.by) + ' ' + when(iv.at) + ' · ' + iv.members + ' member' + (iv.members === 1 ? '' : 's')),
        row(btn('Accept', async () => { if (await op('invitation/accept', { server: iv.server })) { close(); GUILDS.state.server = iv.server; GUILDS.render(); selectConv('ch:' + iv.server + '/general'); } }), btn('Decline', () => op('invitation/decline', { server: iv.server }), 'ghost'))));
    }
  }
  function openServer(id, tab, focus) { show({ kind: 'server', id, tab: tab || 'overview', focus: focus || '' }); }
  return { openServer, newServer, newChannel, editChannel, newGroup, editGroup, join, invitations, refreshOpen, close };
})();
