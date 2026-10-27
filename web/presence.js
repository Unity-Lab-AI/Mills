// VibeRoller presence everywhere (T-261), page side of plans/presence.md (hub side: native/crates/pgm-hcom-internal/src/chat.rs,
// status_of / set_manual; GET /api/presence, POST /api/presence/me, `status` on the room long poll).
// Loaded after the page script; uses the page's helpers at call time when they exist ($, json, me, nickOf, colorOf, ic).
//
// - A status badge on avatars and names: one SHAPE per label (colour-blind safe), the colour only helps:
//   Online = filled dot · Available for work = dot with a check · Away = moon · Offline = ring · Working = gear ·
//   Working + available = diamond; do not disturb (not offline) = a dot with a minus, whatever the label.
// - A hover card on anything marked data-ps-user: label (with the presence beside Working), text, since, where, dnd.
// - "Team now": everyone, their label, project, task and for how long. "My status": override + until, dnd + until,
//   text + until, accepting work, hide where.
// Shared with the other PGM products (the web kit): on a hub without these routes (404 / no `status` in the long poll)
// nothing is shown and nothing throws: badges stay empty, the buttons stay hidden.
const PRESENCE = (() => {
  const LABELS = ['Online', 'Available for work', 'Away', 'Offline', 'Working', 'Working + available'];
  const SHAPE = { 'Online': 'dot', 'Available for work': 'check', 'Away': 'moon', 'Offline': 'ring', 'Working': 'gear', 'Working + available': 'diamond' };
  const COLOR = { dot: '#40c057', check: '#12b886', moon: '#fcc419', ring: '#868e96', gear: '#4dabf7', diamond: '#15aabf', minus: '#fa5252' };
  const S = { map: {}, ok: false, gen: 0, panel: null, card: null, cardFor: null };

  // ── pure helpers (the check R488 evaluates these in node) ──
  /** The badge shape for a status (or a bare label): do not disturb wins over every label but Offline. */
  function shapeOf(st) {
    const label = typeof st === 'string' ? st : st && st.label;
    if (st && typeof st === 'object' && st.dnd && label !== 'Offline') return 'minus';
    return SHAPE[label] || '';
  }
  /** "just now" / "12 min" / "2 h 5" / "3 d" for a span in ms. */
  function dur(ms) {
    const m = Math.floor(Math.max(0, ms) / 60000);
    if (m < 1) return 'just now';
    if (m < 60) return m + ' min';
    if (m < 1440) { const h = Math.floor(m / 60), r = m % 60; return h + ' h' + (r ? ' ' + r : ''); }
    return Math.floor(m / 1440) + ' d';
  }
  /** "on <project> (<branch>) · T-12 <title>", or '' (where is left out when its person hides it). */
  function whereLine(w) {
    if (!w || typeof w !== 'object') return '';
    const proj = w.project ? w.project + (w.branch ? ' (' + w.branch + ')' : '') : '';
    const task = w.task && w.task.id ? w.task.id + (w.task.title ? ' ' + w.task.title : '') : '';
    const bits = [proj, task].filter(Boolean);
    return bits.length ? 'on ' + bits.join(' · ') : '';
  }
  /** The headline: the label, with the presence beside Working ("Working · away"), so "my AI works while I'm away" reads at a glance. */
  function headline(st) {
    if (!st || !st.label) return '';
    return st.label + (st.working && st.presence && st.presence !== 'online' ? ' · ' + st.presence : '');
  }
  /** The hover card's lines (plain strings, never HTML): [{k, t}] in order. */
  function describe(st, now) {
    if (!st || !st.label) return [];
    now = now || Date.now();
    const out = [{ k: 'label', t: headline(st) }];
    if (st.since) { const d = dur(now - st.since); out.push({ k: 'since', t: d === 'just now' ? 'since just now' : 'for ' + d + ', since ' + new Date(st.since).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }); }
    if (st.text) out.push({ k: 'text', t: '“' + st.text + '”' });
    const w = whereLine(st.where); if (w) out.push({ k: 'where', t: w });
    if (st.workingSessions > 1) out.push({ k: 'sessions', t: st.workingSessions + ' sessions working' });
    if (st.dnd) out.push({ k: 'dnd', t: 'Do not disturb: notifications muted, a reply may wait' });
    if (st.presence === 'offline' && st.seen) out.push({ k: 'seen', t: 'last seen ' + dur(now - st.seen) + (now - st.seen < 60000 ? '' : ' ago') });
    else if (st.devices > 1) out.push({ k: 'devices', t: 'on ' + st.devices + ' devices' });
    return out;
  }
  /** The order of the Team now list: working first, then online / available, away, offline; then by name. */
  function rank(st) { const l = st && st.label; return l === 'Working + available' ? 0 : l === 'Working' ? 1 : l === 'Available for work' ? 2 : l === 'Online' ? 3 : l === 'Away' ? 4 : 5; }
  /** An until for a choice: '' (no end) → null, minutes → now + them, 'tomorrow' → 8:00 tomorrow. */
  function untilOf(choice, now) {
    now = now || Date.now();
    if (!choice) return null;
    if (choice === 'tomorrow') { const d = new Date(now); d.setDate(d.getDate() + 1); d.setHours(8, 0, 0, 0); return d.getTime(); }
    const m = Number(choice); return m > 0 ? now + m * 60000 : null;
  }

  // ── DOM ──
  const hasDom = typeof document !== 'undefined';
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const nameOf = (u) => (typeof nickOf === 'function' ? nickOf(u) : u);
  const myName = () => (typeof me !== 'undefined' && me ? me.username : '');
  const api = (p, o) => (typeof json === 'function' ? json(p, o) : Promise.reject(new Error('no hub')));
  const NS = 'http://www.w3.org/2000/svg';
  const sv = (tag, attrs) => { const e = document.createElementNS(NS, tag); for (const k in attrs) e.setAttribute(k, attrs[k]); return e; };
  const GEAR = (() => { const p = []; for (let k = 0; k < 8; k++) { const a = k * 45; for (const [d, r] of [[-14, 5.4], [-8, 7.6], [8, 7.6], [14, 5.4]]) { const t = (a + d) * Math.PI / 180; p.push((8 + r * Math.cos(t)).toFixed(2) + ',' + (8 + r * Math.sin(t)).toFixed(2)); } } return p.join(' '); })();
  function shapeSvg(shape) { // a 16×16 badge: the panel colour outlines it, so it reads on any avatar
    const s = sv('svg', { viewBox: '0 0 16 16', 'aria-hidden': 'true', class: 'psSvg ps-' + shape }), c = COLOR[shape], o = { stroke: 'var(--panel, #222)', 'stroke-width': '2', 'paint-order': 'stroke' };
    const white = { fill: 'none', stroke: '#fff', 'stroke-width': '1.8', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' };
    if (shape === 'dot') s.append(sv('circle', { cx: 8, cy: 8, r: 6, fill: c, ...o }));
    else if (shape === 'check') s.append(sv('circle', { cx: 8, cy: 8, r: 6.5, fill: c, ...o }), sv('path', { d: 'M5 8.3l2 2 4-4.4', ...white }));
    else if (shape === 'moon') s.append(sv('path', { d: 'M9 1.8A6.3 6.3 0 1 0 14.2 10 5 5 0 0 1 9 1.8z', fill: c, ...o }));
    else if (shape === 'ring') s.append(sv('circle', { cx: 8, cy: 8, r: 5.2, fill: 'var(--panel, #222)', stroke: c, 'stroke-width': '2.6' }));
    else if (shape === 'gear') s.append(sv('polygon', { points: GEAR, fill: c, ...o }), sv('circle', { cx: 8, cy: 8, r: 2.1, fill: 'var(--panel, #222)' }));
    else if (shape === 'diamond') s.append(sv('polygon', { points: '8,0.8 15.2,8 8,15.2 0.8,8', fill: c, ...o }));
    else if (shape === 'minus') s.append(sv('circle', { cx: 8, cy: 8, r: 6.5, fill: c, ...o }), sv('path', { d: 'M4.8 8h6.4', ...white, 'stroke-width': '2.2' }));
    return s;
  }
  function paint(b) { // one badge, from what is known now
    const st = S.map[b.dataset.psUser], shape = shapeOf(st), host = b.parentElement;
    const key = shape + '|' + headline(st); if (b.dataset.psKey === key) return; // unchanged: no redraw
    b.dataset.psKey = key; b.textContent = '';
    if (shape) { b.append(shapeSvg(shape)); b.hidden = false; b.setAttribute('aria-label', headline(st) + (st.dnd ? ', do not disturb' : '')); } else { b.hidden = true; b.removeAttribute('aria-label'); }
    if (host && b.classList.contains('psCorner')) host.classList.toggle('psOn', !!shape); // the old online / away corner gives way
  }
  /** A badge for a person. corner: placed on the host's corner (an avatar / dot); else inline before a name. */
  function badge(u, corner) {
    const b = el('i', 'psB' + (corner ? ' psCorner' : '')); b.dataset.psUser = u || ''; b.hidden = true; b.setAttribute('role', 'img');
    if (hasDom) queueMicrotask(() => paint(b)); // after the caller has put it in its host
    return b;
  }
  /** Mark an avatar / dot (corner badge) and the element that hovers as that person's. Safe without a status. */
  function decorate(host, u, hover) {
    if (!host || !u) return host;
    host.classList.add('psHost'); host.append(badge(u, true));
    (hover || host).dataset.psUser = u;
    return host;
  }
  function refresh() {
    if (!hasDom) return;
    buttons();
    for (const b of document.querySelectorAll('.psB')) paint(b);
    if (S.panel && S.panel.isConnected && S.panel.dataset.kind === 'team') renderTeam();
    if (S.card && !S.card.hidden && S.cardFor) fillCard(S.cardFor);
    buttons();
  }
  /** The status list from the room long poll (or GET /api/presence). Not an array = an older hub: nothing changes. */
  function update(list) {
    if (!Array.isArray(list)) return;
    S.ok = true;
    const m = {}; for (const st of list) if (st && st.user) m[st.user] = st;
    S.map = m; refresh();
  }
  function reset() { S.map = {}; S.ok = false; S.gen++; hideCard(); closePanel(); buttons(); }
  async function start() {
    const gen = ++S.gen;
    try { const d = await api('/api/presence'); if (gen === S.gen && d && Array.isArray(d.people)) update(d.people); } catch { /* no presence on this hub: show nothing */ }
  }

  // ── the hover card ──
  function cardEl() {
    if (!S.card) { S.card = el('div', 'psCard'); S.card.id = 'psCard'; S.card.hidden = true; S.card.setAttribute('role', 'tooltip'); document.body.append(S.card); }
    return S.card;
  }
  function fillCard(host) {
    const u = host.dataset.psUser, st = S.map[u], c = cardEl(); c.textContent = '';
    const top = el('div', 'psCardTop'); const sh = shapeOf(st); if (sh) top.append(shapeSvg(sh));
    top.append(el('b', '', nameOf(u)));
    c.append(top);
    for (const line of describe(st)) c.append(el('div', 'psL psL-' + line.k, line.t));
    if (host.dataset.psTitle) c.append(el('div', 'psL psL-title', host.dataset.psTitle));
  }
  function showCard(host) {
    const st = S.map[host.dataset.psUser]; if (!st) return; // nothing known (an older hub): the native title stays
    if (host.title) { host.dataset.psTitle = host.title; host.removeAttribute('title'); } // the card carries it instead
    S.cardFor = host; fillCard(host);
    const c = cardEl(); c.hidden = false;
    const r = host.getBoundingClientRect(), w = c.offsetWidth, h = c.offsetHeight;
    let y = r.bottom + 6; if (y + h > innerHeight - 8) y = Math.max(8, r.top - h - 6);
    c.style.left = Math.max(8, Math.min(r.left, innerWidth - w - 8)) + 'px'; c.style.top = y + 'px';
  }
  function hideCard() { if (S.card) S.card.hidden = true; S.cardFor = null; }
  if (hasDom) {
    document.addEventListener('mouseover', (e) => { const h = e.target.closest && e.target.closest('[data-ps-user]'); if (h && h !== S.cardFor && h.dataset.psUser) showCard(h); else if (!h && S.cardFor) hideCard(); });
    document.addEventListener('focusin', (e) => { const h = e.target.closest && e.target.closest('[data-ps-user]'); if (h && h.dataset.psUser) showCard(h); });
    document.addEventListener('focusout', hideCard);
    document.addEventListener('scroll', hideCard, true);
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape') { hideCard(); closePanel(); } });
    document.addEventListener('click', (e) => { hideCard(); if (S.panel && S.panel.isConnected && !e.target.closest('#psPanel') && !e.target.closest('[data-ps-open]')) closePanel(); });
  }

  // ── panels: Team now, My status ──
  function openPanel(kind, near) {
    closePanel();
    const p = el('div', 'psPanel'); p.id = 'psPanel'; p.dataset.kind = kind; p.setAttribute('role', 'dialog');
    document.body.append(p); S.panel = p;
    S.near = near;
    if (kind === 'team') { p.setAttribute('aria-label', 'Team now'); renderTeam(); start(); } else renderMine().then(place);
    place();
  }
  function place() { // beside the button that opened it, inside the window
    const p = S.panel; if (!p) return;
    const r = S.near && S.near.isConnected ? S.near.getBoundingClientRect() : null, w = p.offsetWidth, h = p.offsetHeight;
    p.style.left = Math.max(8, Math.min(r ? r.left : (innerWidth - w) / 2, innerWidth - w - 8)) + 'px';
    p.style.top = Math.max(8, Math.min(r ? r.bottom + 6 : 80, innerHeight - h - 8)) + 'px';
  }
  function closePanel() { if (S.panel) S.panel.remove(); S.panel = null; }
  function panelHead(p, title) {
    const h = el('div', 'psHead'); h.append(el('b', '', title));
    const x = el('button', 'ghost tiny'); x.type = 'button'; x.title = 'Close'; x.setAttribute('aria-label', 'Close');
    if (typeof ic === 'function') x.append(ic('close')); else x.textContent = '×';
    x.onclick = closePanel; h.append(x); p.append(h);
  }
  function renderTeam() {
    const p = S.panel; if (!p) return; p.textContent = '';
    panelHead(p, 'Team now');
    const people = Object.values(S.map).sort((a, b) => rank(a) - rank(b) || nameOf(a.user).localeCompare(nameOf(b.user)));
    if (!people.length) { p.append(el('div', 'psEmpty', 'Nobody to show yet.')); return; }
    const now = Date.now(), list = el('div', 'psTeam');
    for (const st of people) {
      const r = el('div', 'psRow'); r.dataset.psUser = st.user;
      const av = el('span', 'psAv', nameOf(st.user).slice(0, 1).toUpperCase()); if (typeof colorOf === 'function') av.style.background = colorOf(st.user);
      decorate(av, st.user, r);
      const mid = el('div', 'psMid'); mid.append(el('b', '', nameOf(st.user) + (st.user === myName() ? ' (you)' : '')));
      const sub = [headline(st), st.text ? '“' + st.text + '”' : ''].filter(Boolean).join(' · '); mid.append(el('small', '', sub));
      const w = whereLine(st.where); if (w) mid.append(el('small', 'psWhere', w));
      r.append(av, mid, el('span', 'psSince', st.since ? dur(now - st.since) : ''));
      if (st.dnd) r.classList.add('dnd');
      list.append(r);
    }
    p.append(list);
  }
  const UNTIL = [['', 'no end'], ['30', '30 min'], ['60', '1 hour'], ['240', '4 hours'], ['tomorrow', 'until tomorrow 8:00']];
  function untilSel(cur) {
    const s = el('select', 'psUntil'); for (const [v, t] of UNTIL) { const o = el('option', '', t); o.value = v; s.append(o); }
    if (cur && cur > Date.now()) { const o = el('option', '', 'until ' + new Date(cur).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })); o.value = 'keep'; s.append(o); s.value = 'keep'; }
    return s;
  }
  async function renderMine() {
    const p = S.panel; if (!p) return; p.textContent = ''; p.setAttribute('aria-label', 'My status');
    panelHead(p, 'My status');
    let st; try { st = await api('/api/presence/me'); } catch { p.append(el('div', 'psEmpty', 'Your status cannot be set on this hub.')); return; }
    if (S.panel !== p) return;
    const form = el('form', 'psMine');
    const note = el('div', 'psNow', 'Others see: ' + (headline(st) || '?') + (st.override && st.override !== 'auto' ? ' (you are really ' + (st.real || '?') + ')' : ''));
    const row = (lbl, ...kids) => { const r = el('label', 'psField'); r.append(el('span', '', lbl), ...kids); form.append(r); return r; };
    const over = el('select'); for (const [v, t] of [['auto', 'Automatic'], ['away', 'Appear away'], ['offline', 'Appear offline']]) { const o = el('option', '', t); o.value = v; over.append(o); } over.value = st.override || 'auto';
    const overU = untilSel(st.overrideUntil); row('Show me as', over, overU);
    const dnd = el('input'); dnd.type = 'checkbox'; dnd.checked = !!st.dnd; const dndU = untilSel(st.dndUntil);
    row('Do not disturb (mutes notifications only)', dnd, dndU);
    const text = el('input'); text.type = 'text'; text.maxLength = 80; text.placeholder = 'What are you up to? (80 characters)'; text.value = st.text || ''; const textU = untilSel(st.textUntil && st.text ? st.textUntil : 0);
    row('Status text', text, textU);
    const acc = el('input'); acc.type = 'checkbox'; acc.checked = !!st.acceptingWork; row('Accepting work', acc);
    const hide = el('input'); hide.type = 'checkbox'; hide.checked = !!st.hideWhere; row('Hide my project and task from others', hide);
    const err = el('div', 'psErr'); const save = el('button', '', 'Save'); save.type = 'submit';
    const acts = el('div', 'row'); acts.append(save); form.append(acts, err);
    const pick = (sel, on, cur) => (!on ? 0 : sel.value === 'keep' ? cur || 0 : untilOf(sel.value) || 0); // off = no until; "keep" = the one set before
    form.onsubmit = async (e) => {
      e.preventDefault(); err.textContent = '';
      const body = { override: over.value, dnd: dnd.checked, text: text.value.trim(), acceptingWork: acc.checked, hideWhere: hide.checked };
      body.overrideUntil = pick(overU, over.value !== 'auto', st.overrideUntil); body.dndUntil = pick(dndU, dnd.checked, st.dndUntil); body.textUntil = pick(textU, !!body.text, st.textUntil);
      for (const k of ['overrideUntil', 'dndUntil', 'textUntil']) if (body[k] && body[k] <= Date.now()) body[k] = 0; // an until that passed meanwhile: no end
      save.disabled = true;
      try { const r = await api('/api/presence/me', { method: 'POST', body: JSON.stringify(body) }); if (r && r.user) { S.map[r.user] = { ...(S.map[r.user] || {}), ...r }; refresh(); } closePanel(); }
      catch (x) { err.textContent = x.message || String(x); save.disabled = false; }
    };
    p.append(note, form);
  }

  // ── the entry points: the menu, the chat's top bar (and the app's user panel, through myButton) ──
  function btn(id, kind, text, title) {
    const b = el('button', 'ghost psBtn'); b.type = 'button'; b.id = id; b.title = title; b.dataset.psOpen = kind;
    b.onclick = (e) => { e.stopPropagation(); if (S.panel && S.panel.dataset.kind === kind) closePanel(); else openPanel(kind, b); };
    if (kind === 'mine') b.append(badge(myName(), false)); else if (typeof ic === 'function') b.append(ic('users', 'lead'));
    b.append(text); return b;
  }
  /** The "my status" button (for a host that wants one, e.g. the app's user panel). Hidden on hubs without presence. */
  function myButton() { const b = btn('', 'mine', '', 'My status: appear away / offline, do not disturb, status text, accepting work'); b.removeAttribute('id'); b.classList.add('tiny'); b.hidden = !S.ok; return b; }
  function buttons() {
    if (!hasDom || typeof $ !== 'function') return;
    const who = $('menuWho');
    if (who && !$('psBar')) { const bar = el('div', 'psBar'); bar.id = 'psBar'; bar.append(btn('psMineBtn', 'mine', 'My status', 'Appear away / offline, do not disturb, status text, accepting work'), btn('psTeamBtn', 'team', 'Team now', 'Who is on, what they work at, for how long')); who.after(bar); }
    const call = $('callBtn');
    if (call && !$('psRoomTeam')) { const b = btn('psRoomTeam', 'team', '', 'Team now: who is on, what they work at'); b.setAttribute('aria-label', 'Team now'); call.before(b); }
    for (const id of ['psBar', 'psRoomTeam']) { const n = $(id); if (n) n.hidden = !S.ok || !myName(); }
    for (const b of document.querySelectorAll('[data-ps-open="mine"]')) {
      if (b.id !== 'psMineBtn') b.hidden = !S.ok || !myName();
      const bd = b.querySelector('.psB'); if (bd && bd.dataset.psUser !== myName()) bd.dataset.psUser = myName(); // made before sign-in: now it is yours
    }
  }

  return { LABELS, shapeOf, dur, whereLine, headline, describe, rank, untilOf, badge, decorate, update, reset, start, refresh, myButton, openTeam: (near) => openPanel('team', near), openMine: (near) => openPanel('mine', near), state: S };
})();
