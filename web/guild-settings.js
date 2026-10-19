// VibeRoller Server Settings and the server / group dialogs (page side; hub side: bridge/guilds.js, roles in
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
    ['overview', 'Overview', () => true], ['settings', 'Settings', (s) => can(s, 'MANAGE_SERVER')], ['members', 'Members', () => true],
    ['invites', 'Invites', (s) => !s.everyone && (can(s, 'CREATE_INVITE') || can(s, 'MANAGE_INVITES'))],
    ['roles', 'Roles', (s) => can(s, 'MANAGE_ROLES') || (s.roles || []).length > 0], ['channels', 'Channels', (s) => can(s, 'MANAGE_CHANNELS') || can(s, 'CREATE_CHANNELS')],
    ['bans', 'Bans', (s) => !s.everyone && can(s, 'BAN_MEMBERS')], ['security', 'Security', (s) => !s.everyone && can(s, 'MANAGE_SERVER')],
    ['audit', 'Audit log', (s) => can(s, 'VIEW_AUDIT_LOG')], ['owner', 'Ownership', (s) => s.me.owner],
  ];
  const sel = (pairs, value) => { const x = el('select'); for (const [v, t] of pairs) { const o = el('option', '', t); o.value = v; x.append(o); } x.value = String(value); return x; };
  const mins = (m) => (m >= 1440 ? m / 1440 + ' day' + (m > 1440 ? 's' : '') : m >= 60 ? m / 60 + ' hour' + (m > 60 ? 's' : '') : m + ' minute' + (m > 1 ? 's' : ''));
  const secs = (n) => (!n ? 'off' : n >= 3600 ? n / 3600 + ' h' : n >= 60 ? n / 60 + ' min' : n + ' s');
  function settings(s) { // MANAGE_SERVER: welcome channel, rules, notifications, invites, new-member wait
    const st = s.settings || {}, D = data();
    const chans = [['', '(none)'], ...s.channels.map((c) => [c.id, '#' + c.name])];
    const sys = sel(chans, st.systemChannel || ''), joins = check('post a welcome line when someone joins', st.systemJoins !== false), leaves = check('post a line when someone leaves', !!st.systemLeaves);
    const rules = sel(chans, st.rulesChannel || '');
    const notify = sel([['all', 'all messages'], ['mentions', 'only @mentions']], st.notify || 'all');
    const paused = check('pause invites (invite links stop working; personal invitations still do)', !!st.invitesPaused);
    const wait = sel(D.waits.map((m) => [m, m ? 'new members wait ' + m + ' minutes before posting (unless given a role)' : 'new members may post at once']), st.wait || 0);
    if (s.everyone) { sys.disabled = true; joins.box.disabled = leaves.box.disabled = paused.box.disabled = true; }
    bodyEl.append(
      sect('Welcome channel', el('small', '', 'Where the hub posts join / leave lines as "VibeRoller".'), row(sys), joins, leaves),
      sect('Rules channel', el('small', '', 'Named in the welcome line and marked in the channel list.'), row(rules)),
      sect('Default notifications', el('small', '', 'For everyone in this server: sound, pop-ups and reading aloud for every message, or only when someone @mentions or replies to you.'), row(notify)),
      sect('Safety', paused, row(wait)),
      row(btn('Save settings', () => op('server/update', { server: s.id, settings: s.everyone ? { rulesChannel: rules.value, notify: notify.value, wait: Number(wait.value) } : { systemChannel: sys.value, systemJoins: joins.box.checked, systemLeaves: leaves.box.checked, rulesChannel: rules.value, notify: notify.value, invitesPaused: paused.box.checked, wait: Number(wait.value) } }))));
  }
  function drawServer(o) {
    const s = srv(o.id);
    if (!s) { titleEl.textContent = 'Server'; bodyEl.append(el('div', '', 'You are not in that server any more.')); return; }
    titleEl.textContent = s.name + ' · Server Settings';
    tabsEl.hidden = false;
    const tabs = TABS.filter(([, , ok]) => ok(s));
    if (!tabs.some(([id]) => id === o.tab)) o.tab = 'overview';
    for (const [id, name] of tabs) { const b = btn(name, () => { o.tab = id; say(''); draw(); }, 'ghost' + (id === o.tab ? ' on' : '')); tabsEl.append(b); }
    ({ overview, settings, members, invites, roles, channels, bans, security, audit, owner })[o.tab](s, o);
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
    const myNick = input((s.nicks || {})[me.username] || '', nickOf(me.username)); myNick.maxLength = 32; myNick.disabled = !can(s, 'CHANGE_NICKNAME');
    bodyEl.append(sect('Your nickname in ' + s.name, el('small', '', can(s, 'CHANGE_NICKNAME') ? 'Shown instead of your name inside this server only.' : 'Changing your nickname is not allowed for you here.'), row(myNick, can(s, 'CHANGE_NICKNAME') ? btn('Save', () => op('member/nick', { server: s.id, user: me.username, nick: myNick.value })) : null)));
    // your roles here (highest first; the top coloured one colours your name in this server), and the ones you may pick
    const mineR = GUILDS.rolesOfUser(s, me.username), chip = (r) => { const c = el('span', 'roleChip', r.name); if (r.color) { c.style.borderColor = r.color; c.style.color = r.color; } return c; };
    const pickable = (s.roles || []).filter((r) => r.selfAssign);
    bodyEl.append(sect('Your roles', el('small', '', 'Your name shows in the colour of your highest coloured role, inside this server only.'),
      row(...(mineR.length ? mineR.map(chip) : [el('small', '', 'none')])),
      ...(pickable.length ? [el('small', '', 'Roles you can pick yourself:'), row(...pickable.map((r) => { const on = mineR.some((x) => x.id === r.id); const c = check(r.name, on); if (r.color) c.style.color = r.color; c.box.onchange = () => op('member/selfrole', { server: s.id, role: r.id, on: c.box.checked }); return c; }))] : [])));
    if (s.timeouts && s.timeouts[me.username]) bodyEl.append(sect('Timeout', el('div', 'status', 'You are timed out until ' + when(s.timeouts[me.username]) + ': you can read but not post, type or join calls.')));
    const facts = [s.everyone ? 'Everyone with a VibeRoller account is in this server.' + (s.owner ? ' Owner: ' + nick(s, s.owner) + '.' : '') : `${s.members.length} member${s.members.length === 1 ? '' : 's'} · owner ${nick(s, s.owner)}`, s.locked ? 'Password locked: joining with an invite code asks for its password.' : '', 'Your permissions: ' + (s.me.perms.map((p) => p.toLowerCase().replace(/_/g, ' ')).join(', ') || 'none')].filter(Boolean);
    bodyEl.append(sect('About', ...facts.map((t) => el('div', '', t))));
    if (!s.everyone && !s.me.owner) bodyEl.append(sect('Leave', armed('Leave ' + s.name, async () => { if (await op('server/leave', { server: s.id })) { close(); GUILDS.state.server = 'mills'; selectConv('room'); } })));
  }
  function members(s, o) {
    if (!s.everyone && can(s, 'CREATE_INVITE')) {
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
        s.timeouts && s.timeouts[u] ? 'timed out until ' + when(s.timeouts[u]) : '', (s.voiceMuted || []).includes(u) ? 'muted in calls' : '',
        ...((s.memberRoles || {})[u] || []).map((id) => ((s.roles || []).find((r) => r.id === id) || {}).name).filter(Boolean)].filter(Boolean).join(' · ');
      it.append(el('small', '', sub || 'no roles'));
      const below = u !== me.username && myRank > rankOf(u);
      const acts = el('div', 'row');
      const co = (s.roles || []).find((r) => r.builtin === 'coowner');
      if (co && s.me.owner && u !== me.username && u !== s.owner) { // the owner names co-owners
        const isCo = ((s.memberRoles || {})[u] || []).includes(co.id), cur = (s.memberRoles || {})[u] || [];
        acts.append(isCo ? armed('Remove co-owner', () => op('member/roles', { server: s.id, user: u, roles: cur.filter((id) => id !== co.id) })) : btn('Make co-owner', () => op('member/roles', { server: s.id, user: u, roles: [...cur, co.id] }), 'ghost'));
      }
      if (can(s, 'MANAGE_ROLES') && (s.roles || []).length && (below || s.me.owner)) acts.append(btn('Roles', () => { o.focus = u; o.rolesFor = o.rolesFor === u ? '' : u; draw(); }, 'ghost'));
      if (can(s, 'MANAGE_NICKNAMES') && below) acts.append(btn('Nickname', async () => { const n = window.prompt('Server nickname for ' + nickOf(u) + ' (empty = none):', (s.nicks || {})[u] || ''); if (n !== null) op('member/nick', { server: s.id, user: u, nick: n }); }, 'ghost'));
      if (can(s, 'MODERATE_MEMBERS') && below) {
        if (s.timeouts && s.timeouts[u]) acts.append(btn('End timeout', () => op('member/timeout', { server: s.id, user: u, minutes: 0 }), 'ghost'));
        else acts.append(btn('Timeout', () => { o.focus = u; o.panel = o.panel === 'timeout:' + u ? '' : 'timeout:' + u; draw(); }, 'ghost'));
      }
      if (can(s, 'MUTE_MEMBERS') && below) { const muted = (s.voiceMuted || []).includes(u); acts.append(btn(muted ? 'Unmute in calls' : 'Mute in calls', () => op('member/mute', { server: s.id, user: u, on: !muted }), 'ghost')); }
      if (!s.everyone && can(s, 'KICK_MEMBERS') && below) acts.append(armed('Kick', () => { const r = window.prompt('Reason (optional, kept in the audit log):', '') || ''; op('member/kick', { server: s.id, user: u, reason: r }); }));
      if (!s.everyone && can(s, 'BAN_MEMBERS') && below) acts.append(btn('Ban', () => { o.focus = u; o.panel = o.panel === 'ban:' + u ? '' : 'ban:' + u; draw(); }, 'ghost'));
      if (acts.childNodes.length) it.append(acts);
      const D = data();
      if (o.panel === 'timeout:' + u) {
        const len = sel(D.timeouts.map((m) => [m, mins(m)]), 60), why = input('', 'reason (optional)');
        it.append(row(el('span', '', 'Time out for'), len, why, btn('Time out', () => { o.panel = ''; op('member/timeout', { server: s.id, user: u, minutes: Number(len.value), reason: why.value }); })));
      }
      if (o.panel === 'ban:' + u) {
        const del = sel(D.banDelete.map((n) => [n, n ? 'and delete their messages of the last ' + (n === 3600 ? 'hour' : n === 86400 ? 'day' : '7 days') : 'keep their messages']), 0), why = input('', 'reason (optional)');
        it.append(row(del, why, armed('Ban ' + nickOf(u), () => { o.panel = ''; op('member/ban', { server: s.id, user: u, reason: why.value, deleteSeconds: Number(del.value) }); })));
      }
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
    bodyEl.append(sect('Make an invite link', el('small', '', 'Anyone with a VibeRoller account who has the link (and the password, if the server is locked) can join.'), row(age, uses), temp,
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
  // permission checkboxes grouped like Discord's (General / Membership / Text / Voice / Advanced); you can only
  // tick or untick permissions you hold yourself (the hub refuses the rest too)
  function permBoxes(s, perms, opts = {}) {
    const D = data(), mine = s.me.perms, out = [];
    for (const [cat, ids] of D.categories) {
      const list = ids.filter((id) => !(opts.noAdmin && id === 'ADMINISTRATOR'));
      if (!list.length) continue;
      out.push(el('div', 'cat', cat));
      for (const id of list) { const c = check(id.toLowerCase().replace(/_/g, ' '), perms.includes(id)); c.title = D.perms[id]; c.perm = id; if (opts.readOnly || !mine.includes(id)) c.box.disabled = true; out.push(c); }
    }
    return out;
  }
  const ticked = (boxes) => boxes.filter((c) => c.perm && c.box.checked).map((c) => c.perm);
  function roles(s) {
    const edit = can(s, 'MANAGE_ROLES'), D = data();
    const ev = permBoxes(s, s.everyonePerms || [], { noAdmin: true, readOnly: !edit });
    bodyEl.append(sect('@everyone', el('small', '', 'What every member may do. Hover a permission for what it means.'), el('div', 'gsetPerms'), ...(edit ? [row(btn('Save @everyone', () => op('server/update', { server: s.id, everyonePerms: ticked(ev) })))] : [])));
    bodyEl.lastChild.querySelector('.gsetPerms').append(...ev);
    if (edit) {
      const nm = input('', 'new role name'); nm.maxLength = 32;
      const col = input('#3ba55d', '', 'color');
      const tpl = sel([['', 'start empty'], ...Object.entries(D.templates).filter(([k]) => k !== 'coowner').map(([k, t]) => [k, 'start with ' + t.name + "'s permissions"])], '');
      bodyEl.append(sect('New role', el('small', '', 'A custom role starts just above Member; move it between the built-in levels with Move up / down.'),
        row(nm, col, tpl, btn('Make role', () => op('role', { server: s.id, name: nm.value, color: col.value, ...(tpl.value ? { template: tpl.value } : { perms: [] }) })))));
    }
    bodyEl.append(sect('Built-in levels', el('small', '', 'Every server has these, highest first: Owner (one person) → Co-owner (owner rights under the owner: everything except deleting or handing over the server and managing other co-owners) → Super Admin (administrator) → Admin (manage server, roles, channels) → Super Moderator (ban, invites, announce, make channels) → Moderator (kick, timeout, mute in calls, delete + pin messages, audit log, nicknames, slowmode bypass) → Member (everyone who joins) → @everyone. Each level holds everything below it. They cannot be deleted or reordered; the owner can rename them and change their permissions (Co-owner and Super Admin always have every permission).')));
    for (const r of s.roles || []) {
      const mine = s.me.owner || s.me.rank > r.position;
      const nm = input(r.name); nm.maxLength = 32; nm.disabled = !edit || !mine;
      const col = input(r.color || '#99aab5', '', 'color'); col.disabled = !edit || !mine;
      const hoist = check('show its members separately', r.hoist); hoist.box.disabled = !edit || !mine;
      const ment = check('anyone may @mention it', r.mentionable); ment.box.disabled = !edit || !mine;
      const selfA = check('members may pick it themselves (everyday permissions only)', r.selfAssign); selfA.box.disabled = !edit || !mine || !!r.builtin;
      const fixed = r.builtin === 'coowner' || r.builtin === 'superadmin';
      const boxes = permBoxes(s, r.perms, { readOnly: !edit || !mine || fixed });
      const who = s.members.filter((u) => ((s.memberRoles || {})[u] || []).includes(r.id)).map((u) => nick(s, u)).join(', ');
      const box = sect(r.name + (r.builtin ? ' · built-in level' : ' · custom role') + ' · position ' + r.position, el('small', '', who ? 'held by ' + who : r.builtin === 'member' && s.everyone ? 'everyone' : 'nobody has it yet'), row(nm, col, hoist), row(ment, selfA), el('div', 'gsetPerms'));
      box.querySelector('.gsetPerms').append(...boxes);
      if (edit && mine) box.append(row(btn('Save', () => op('role/update', { server: s.id, role: r.id, name: nm.value, color: col.value, hoist: hoist.box.checked, mentionable: ment.box.checked, ...(r.builtin ? {} : { selfAssign: selfA.box.checked }), ...(fixed ? {} : { perms: ticked(boxes) }) })),
        ...(r.builtin ? [] : [btn('Move up', () => op('role/update', { server: s.id, role: r.id, position: r.position + 1 }), 'ghost'), btn('Move down', () => op('role/update', { server: s.id, role: r.id, position: Math.max(1, r.position - 1) }), 'ghost'),
          armed('Delete role', () => op('role/delete', { server: s.id, role: r.id }))])));
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
    const stay = check('stay on as Co-owner', true);
    bodyEl.append(sect('Hand the server over', el('small', '', 'The new owner gets every right, including deleting the server and naming co-owners. You stay a member, and by default a Co-owner.'), row(to, stay, armed('Make them the owner', () => to.value && op('server/transfer', { server: s.id, user: to.value, keepCoOwner: stay.box.checked })))));
    if (s.everyone) return; // the VibeRoller server is everyone's: it can be handed on, never deleted
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
    const ann = check('announcement channel: only people with "announce" (or "manage messages") post', ch && ch.announce);
    const nsfw = check('age-restricted: everyone confirms once before opening it', ch && ch.nsfw);
    const D = data();
    const slow = sel(D.slowmodes.map((n) => [n, 'slowmode ' + secs(n)]), ch ? ch.slowmode || 0 : 0);
    const general = ch && ch.id === 'general';
    bodyEl.append(sect('Channel', row(name, topic), row(cat, dl), row(slow), ann, nsfw));
    // permission overwrites, Discord's way: per @everyone / role / person, each permission inherit / allow / deny
    let ows = JSON.parse(JSON.stringify((ch && ch.overwrites) || []));
    const permsBox = el('div');
    const canEditPerms = !(s.everyone && general);
    const drawOws = () => {
      permsBox.textContent = '';
      const targets = [['role:everyone', '@everyone'], ...(s.roles || []).map((r) => ['role:' + r.id, r.name]), ...s.members.map((u) => ['member:' + u, nick(s, u)])];
      const has = (k) => ows.some((o) => o.type + ':' + o.id === k);
      const isPriv = ows.some((o) => o.type === 'role' && o.id === 'everyone' && (o.deny || []).includes('VIEW_CHANNEL'));
      const priv = check('private channel (@everyone cannot see it; allow "view channel" below for who may)', isPriv);
      priv.box.onchange = () => {
        let e = ows.find((o) => o.type === 'role' && o.id === 'everyone'); if (!e) { e = { type: 'role', id: 'everyone', allow: [], deny: [] }; ows.unshift(e); }
        e.deny = priv.box.checked ? [...new Set([...e.deny, 'VIEW_CHANNEL'])] : e.deny.filter((p) => p !== 'VIEW_CHANNEL');
        e.allow = e.allow.filter((p) => p !== 'VIEW_CHANNEL'); drawOws();
      };
      permsBox.append(priv);
      for (const o of ows) {
        const tname = (targets.find(([k]) => k === o.type + ':' + o.id) || [, o.id])[1];
        const box = el('div', 'setSection'); box.append(el('b', '', (o.type === 'member' ? 'Person: ' : 'Role: ') + tname));
        for (const p of D.channelPerms) {
          const r = el('div', 'owRow'); r.append(el('span', '', p.toLowerCase().replace(/_/g, ' ')));
          const v = (o.allow || []).includes(p) ? 'allow' : (o.deny || []).includes(p) ? 'deny' : '';
          const x = sel([['', 'inherit'], ['allow', 'allow'], ['deny', 'deny']], v); x.title = D.perms[p];
          if (!s.me.perms.includes(p)) x.disabled = true; // only what you hold yourself
          x.onchange = () => { o.allow = (o.allow || []).filter((q) => q !== p); o.deny = (o.deny || []).filter((q) => q !== p); if (x.value) o[x.value].push(p); };
          r.append(x); box.append(r);
        }
        box.append(row(btn('Remove this override', () => { ows = ows.filter((q) => q !== o); drawOws(); }, 'ghost')));
        permsBox.append(box);
      }
      const add = sel([['', 'add an override for…'], ...targets.filter(([k]) => !has(k))], '');
      add.onchange = () => { if (!add.value) return; const [type, id] = add.value.split(':'); ows.push({ type, id, allow: [], deny: [] }); drawOws(); };
      permsBox.append(row(add));
    };
    if (canEditPerms) { bodyEl.append(sect('Channel permissions', el('small', '', 'Overrides on top of the server roles, applied like Discord: @everyone, then roles (an allow beats a deny between roles), then the person. Administrators skip them.'), permsBox)); drawOws(); }
    const actions = row(btn(ch ? 'Save' : 'Make channel', async () => {
      const body = { server: s.id, name: name.value, topic: topic.value, category: cat.value, announce: ann.box.checked, nsfw: nsfw.box.checked, slowmode: Number(slow.value), ...(canEditPerms ? { overwrites: ows.filter((o) => (o.allow || []).length || (o.deny || []).length) } : {}) };
      if (ch) { if (await op('channel/update', { ...body, channel: ch.id })) close(); }
      else { const made = await op('channel', body); if (made && made.id) { close(); selectConv('ch:' + s.id + '/' + made.id); } }
    }));
    if (ch && !general) actions.append(armed('Delete channel', async () => { if (await op('channel/delete', { server: s.id, channel: ch.id })) { close(); if (currentConv === 'ch:' + s.id + '/' + ch.id) selectConv('room'); } }));
    bodyEl.append(actions);
  }

  // ── groups ──
  function newGroup() { show({ kind: 'newGroup' }); }
  function drawNewGroup() {
    titleEl.textContent = 'New group DM';
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
    bodyEl.append(sect('Server', row(name), row(icon, color), desc), sect('People', el('small', '', 'They get an invitation to accept and join as Members. You can also make invite links later.'), row(who, dl)),
      sect('Roles', el('small', '', 'You are the owner. The server comes with the built-in levels Co-owner, Super Admin, Admin, Super Moderator, Moderator and Member, ready to hand out.')),
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
