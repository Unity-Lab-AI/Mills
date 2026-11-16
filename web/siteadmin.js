// Site admin (T-379; was Admin): the site roles and what each may do, every account's role, the site-wide settings. Hub
// side: GET /api/admin/roles (the permission catalogue, the ladder and custom roles, who holds which), POST
// /api/admin/roles/assign | save | delete, GET / POST /api/admin/site-settings; the hub checks the permission (and the rank:
// only roles and people below your own) on every call. The ladder is the chat servers' (Member 10, Moderator 20, Super
// Moderator 30, Admin 40, Super Admin 50, Co-owner 60); custom roles sit in between. The accounts table, hub controls,
// health, security log and gateway stay in index.html (loadAdmin), which asks this file for the role column. An older hub
// has none of these routes: the groups stay hidden and the accounts table keeps its Make admin buttons.
'use strict';
const SITEADMIN = (() => {
  const $id = (id) => document.getElementById(id);
  const mk = (tag, cls, txt) => { const e = document.createElement(tag); if (cls) e.className = cls; if (txt != null) e.textContent = txt; return e; };
  let R = null; // the last /api/admin/roles answer (null: this hub has no site roles)
  const roleOf = (id) => R && R.roles.find((r) => r.id === id);
  const myPos = () => (R && R.me ? (R.me.owner ? 1e9 : R.me.position) : 0); // the hub's own account stands above every role
  const holds = (perm) => !!(R && R.me && (R.me.perms.includes('ADMINISTRATOR') || R.me.perms.includes(perm)));
  const below = () => (R ? R.roles.filter((r) => r.position < myPos()).sort((a, b) => b.position - a.position) : []);
  const dot = (r) => { const d = mk('span', 'roleDot'); d.style.background = r.color || 'var(--muted)'; return d; };
  const permName = (id) => id.toLowerCase().replace(/_/g, ' ').replace(/\bai\b/g, 'AI').replace(/^\w/, (c) => c.toUpperCase()); // MANAGE_AI: Manage AI
  const chip = (r) => { const c = mk('span', 'roleChip'); c.append(dot(r), r.name); return c; };

  /** Before the accounts table is drawn: the roles (or null) and the groups this person may see. */
  async function load() {
    try { R = await json('/api/admin/roles'); } catch { R = null; }
    $id('saAccounts').hidden = !siteCan('VIEW_ACCOUNTS') && !siteCan('MANAGE_ACCOUNTS');
    $id('saHub').hidden = !siteCan('MANAGE_HUB');
    $id('saRoles').hidden = !R; if (R) renderRoles();
    $id('saSite').hidden = !R || !siteCan('MANAGE_HUB'); if (!$id('saSite').hidden) renderSite();
    const nr = $id('newRole'), na = $id('newAdmin');
    nr.hidden = !R || !holds('MANAGE_ROLES'); na.parentElement.hidden = !!R; // with roles, the role picker replaces the admin box
    if (!nr.hidden) { nr.textContent = ''; const o0 = mk('option', '', 'role: the default for new accounts'); o0.value = ''; nr.append(o0); for (const r of below()) { const o = mk('option', '', r.name); o.value = r.id; nr.append(o); } }
    $id('createUser').parentElement.hidden = !siteCan('MANAGE_ACCOUNTS');
    return !!R;
  }

  /** The role column of one account: a picker for those you may change, else its role. */
  function roleCell(a, out) {
    const td = mk('td');
    if (!R) { td.textContent = a.admin ? 'Admin' : 'Member'; return td; }
    const cur = roleOf((R.members || {})[a.username]) || roleOf(a.admin ? 'superadmin' : 'member') || { id: '', name: '?', position: 0 };
    const mine = a.username === me.username, can = holds('MANAGE_ROLES') && !mine && cur.position < myPos();
    if (!can) { td.append(chip(cur)); if (mine) td.title = 'Your own role: another person above you changes it'; return td; }
    const sel = mk('select'); sel.setAttribute('aria-label', 'Role of ' + a.username);
    for (const r of below()) { const o = mk('option', '', r.name); o.value = r.id; sel.append(o); }
    sel.value = cur.id;
    sel.onchange = async () => {
      try { const d = await json('/api/admin/roles/assign', { method: 'POST', body: JSON.stringify({ username: a.username, role: sel.value }) }); out(`${a.username} is ${(roleOf(d.role) || {}).name || d.role} now.`); loadAdmin(); }
      catch (e) { out('Error: ' + e.message); sel.value = cur.id; }
    };
    const w = mk('span', 'roleCell'); w.append(dot(cur), sel); td.append(w); return td;
  }

  // ── the roles: the ladder top down, what each may do; custom ones made, changed and deleted below your own ──
  function renderRoles() {
    const box = $id('saRolesBox'); box.textContent = '';
    const names = {}; for (const [, ps] of R.categories) for (const [id, text] of ps) names[id] = text;
    const count = {}; for (const id of Object.values(R.members || {})) count[id] = (count[id] || 0) + 1;
    const note = mk('div', 'status');
    for (const r of [...R.roles].sort((a, b) => b.position - a.position)) {
      const it = mk('div', 'roleRow'); const head = mk('div', 'roleHead');
      head.append(chip(r), mk('small', '', 'position ' + r.position + (r.builtin ? ' · built in' : ' · custom') + (count[r.id] ? ' · ' + count[r.id] + ' account' + (count[r.id] === 1 ? '' : 's') : '')));
      it.append(head);
      const ps = mk('div', 'rolePerms');
      if (r.perms.includes('ADMINISTRATOR')) ps.append(mk('span', 'permChip all', 'Every permission'));
      else if (!r.perms.length) ps.append(mk('span', 'status', 'No site permissions: their own account, Work, chat, the board.'));
      else for (const p of r.perms) { const c = mk('span', 'permChip', permName(p)); c.title = names[p] || p; ps.append(c); }
      it.append(ps);
      if (!r.builtin && holds('MANAGE_ROLES') && r.position < myPos()) {
        const acts = mk('div', 'row');
        const ed = mk('button', 'ghost', 'Change'); ed.type = 'button'; ed.onclick = () => { const f = it.querySelector('.roleForm'); if (f) f.remove(); else it.append(roleForm(r, note)); };
        const del = confirmButton('Delete', async () => { try { await json('/api/admin/roles/delete', { method: 'POST', body: JSON.stringify({ id: r.id }) }); note.textContent = r.name + ' deleted: its people are Members now.'; loadAdmin(); } catch (e) { note.textContent = e.message; } });
        acts.append(ed, del); it.append(acts);
      }
      box.append(it);
    }
    if (holds('MANAGE_ROLES')) {
      const add = mk('details'); add.append(mk('summary', '', 'New role')); add.append(roleForm(null, note)); box.append(add);
    }
    box.append(note);
  }
  /** A custom role's form: name, colour, position (below yours, between the built-in steps), the permissions you hold. */
  function roleForm(r, note) {
    const f = mk('div', 'roleForm');
    const name = mk('input'); name.placeholder = 'name, e.g. AI keeper'; name.maxLength = 32; name.value = r ? r.name : '';
    const color = mk('input'); color.type = 'color'; color.value = r ? r.color : '#8e44ad'; color.setAttribute('aria-label', 'Colour');
    const pos = mk('input'); pos.type = 'number'; pos.min = '1'; pos.max = String(Math.max(1, Math.min(59, myPos() - 1))); pos.value = String(r ? r.position : 35); pos.style.width = '6rem'; pos.setAttribute('aria-label', 'Position on the ladder');
    const top = mk('div', 'row'); top.append(name, color, mk('span', 'status', 'position'), pos, mk('span', 'status', '(Member 10 · Moderator 20 · Super Moderator 30 · Admin 40 · Super Admin 50; yours ' + (R.me.owner ? 'the owner, above every role' : myPos()) + ')'));
    f.append(top);
    const boxes = [];
    for (const [cat, ps] of R.categories) {
      const fs = mk('fieldset', 'permSet'); fs.append(mk('legend', '', cat));
      for (const [id, text] of ps) {
        const l = mk('label', 'chk'), cb = mk('input'), sp = mk('span', '', permName(id));
        cb.type = 'checkbox'; cb.value = id; cb.checked = !!(r && r.perms.includes(id)); cb.disabled = !holds(id) && !cb.checked; sp.append(mk('small', '', text + (holds(id) ? '' : ' (you do not hold it, so you cannot give it)')));
        l.append(cb, sp); fs.append(l); boxes.push(cb);
      }
      f.append(fs);
    }
    const save = mk('button', '', r ? 'Save role' : 'Make role'); save.type = 'button';
    save.onclick = async () => {
      const body = { name: name.value.trim(), color: color.value, position: Number(pos.value), perms: boxes.filter((b) => b.checked).map((b) => b.value) }; if (r) body.id = r.id;
      try { await json('/api/admin/roles/save', { method: 'POST', body: JSON.stringify(body) }); note.textContent = (r ? 'Saved ' : 'Made ') + body.name + '.'; loadAdmin(); }
      catch (e) { note.textContent = e.message; }
    };
    f.append(save); return f;
  }

  // ── site-wide settings: the message on everyone's menu, the default theme, the role new accounts get ──
  async function renderSite() {
    const box = $id('saSiteBox'); box.textContent = 'Loading…';
    let st; try { st = await json('/api/admin/site-settings'); } catch (e) { box.textContent = e.message; return; }
    box.textContent = '';
    const motd = mk('textarea'); motd.maxLength = 500; motd.rows = 2; motd.value = st.motd || ''; motd.placeholder = 'A message for everyone, on the menu (empty: none)'; motd.setAttribute('aria-label', 'Message for everyone');
    const theme = mk('select'); theme.setAttribute('aria-label', 'Default theme');
    const t0 = mk('option', '', 'Each browser\'s own (VibeRoller default)'); t0.value = ''; theme.append(t0);
    for (const n of Object.keys(typeof THEMES === 'object' ? THEMES : {})) { const o = mk('option', '', themeLabel(n)); o.value = n; theme.append(o); }
    theme.value = st.defaultTheme || ''; if (theme.selectedIndex < 0) theme.value = '';
    const role = mk('select'); role.setAttribute('aria-label', 'Role of new accounts');
    for (const r of below()) { const o = mk('option', '', r.name); o.value = r.id; role.append(o); }
    role.value = st.newAccountRole || 'member';
    const lab = (t, el, hint) => { const l = mk('label', 'setRow'); const sp = mk('span', '', t); if (hint) sp.append(mk('small', '', hint)); l.append(sp, el); return l; };
    const save = mk('button', '', 'Save site-wide settings'); save.type = 'button'; const note = mk('span', 'status');
    save.onclick = async () => {
      try { const d = await json('/api/admin/site-settings', { method: 'POST', body: JSON.stringify({ motd: motd.value.trim(), defaultTheme: theme.value, newAccountRole: role.value }) }); note.textContent = 'Saved: everyone gets it on their next page load.'; if (me.site) Object.assign(me.site, { motd: d.motd, defaultTheme: d.defaultTheme }); $id('menuMotd').hidden = !d.motd; $id('menuMotd').textContent = d.motd || ''; }
      catch (e) { note.textContent = e.message; }
    };
    const row = mk('div', 'row'); row.append(save, note);
    box.append(lab('Message for everyone', motd, 'Shown on everyone\'s menu until you clear it.'), lab('Default theme', theme, 'For people who never picked a theme of their own.'), lab('New accounts get', role, 'The role an account has when it is made here.'), row);
  }
  /** After an account is made: the role picked for it (when not the default). */
  async function afterCreate(username, out) {
    const nr = $id('newRole'); if (!R || nr.hidden || !nr.value) return;
    try { await json('/api/admin/roles/assign', { method: 'POST', body: JSON.stringify({ username, role: nr.value }) }); } catch (e) { out('Made, but the role was not given: ' + e.message); }
    nr.value = '';
  }
  return { load, roleCell, afterCreate, has: () => !!R };
})();
