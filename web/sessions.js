// VibeRoller Work: the session list (T-287; team note plans/multi-session.md §§ 2b, 3, 4, 6). A hidable sidebar of the
// person's ongoing and past sessions (like ChatGPT / Claude): grouped (pinned / folders / today / earlier / hidden /
// archived), searchable, each with a state dot that has a colour AND a shape (colour-blind safe) and its meaning on hover.
// One menu for "…" and right-click, built from one item list. The saved conversations (Past sessions) are in it too.
// Sessions that are not on screen are followed here in the background: their replies go into their own conversation on
// the hub, and their queued messages are sent when a slot frees (a 429 keeps a message queued).
// Loaded before the main page script; the DOM part runs only after sign-in and uses the page's helpers at call time
// ($, json, api, sys, ic, store, me, SESSION, planSession, openWorkSession, startOpen, hubView, describeTool, ...).
// The pure helpers at the top (no DOM) are what the suite runs in node (R489).
(function () {
  'use strict';
  const W = typeof window !== 'undefined' ? window : globalThis;
  const DAY = 86400e3;

  // ── pure helpers ──
  // the states in the hub's order of precedence (§ 3): colour, shape, name, what it means
  const STATES = {
    waiting: { color: '#e3b40b', shape: '▲', name: 'Needs you', hint: 'it waits on you (a question, a permission or a plan to approve)' },
    working: { color: '#9b5cff', shape: '◐', name: 'Working', hint: 'the AI is working on it' },
    'done-unseen': { color: '#e5484d', shape: '●', name: 'Done', hint: 'it finished, with a reply you have not seen yet' },
    draft: { color: '#3b82f6', shape: '◆', name: 'Draft', hint: 'an unsent draft is saved' },
    idle: { color: '#8b8f98', shape: '◌', name: 'Idle', hint: 'nothing has happened for 5 minutes or more' },
    ready: { color: '', shape: '', name: 'Ready', hint: 'recently active, nothing pending' },
  };
  const ENDED = { silent: 'the run sent nothing for too long, so it was ended', stalled: 'the run stopped answering (its process or job was gone), so it was ended' };
  const WAITS = { question: 'a question for you', permission: 'it may not use a tool without you', plan: 'a plan to approve' };
  // what a session's dot shows; a run that stalled or ended without saying done shows idle, with the reason
  function stateView(s) {
    let st = s && STATES[s.state] ? s.state : 'ready', why = '';
    const ended = s && s.ended && s.ended.why;
    if (ended && (st === 'ready' || st === 'idle')) { st = 'idle'; why = ENDED[ended] || 'the run ended without saying done'; }
    if (st === 'waiting' && s.waiting && WAITS[s.waiting.kind]) why = WAITS[s.waiting.kind] + (Array.isArray(s.waiting.tools) && s.waiting.tools.length ? ' (' + s.waiting.tools.join(', ') + ')' : '');
    const d = STATES[st];
    return { state: st, color: d.color, shape: d.shape, name: d.name, title: d.name + ': ' + (why || d.hint) };
  }
  const startOfDay = (ms) => { const d = new Date(ms); d.setHours(0, 0, 0, 0); return d.getTime(); };
  const norm = (q) => String(q || '').toLowerCase().split(/\s+/).filter(Boolean);
  const titleOf = (s) => (s && s.title) || (s && s.mode === 'planner' ? 'Planner' : '') || (s && s.noProject ? 'No project' : (s && s.project) || 'Session');
  function matches(s, q) { const words = Array.isArray(q) ? q : norm(q); const hay = [s.title, s.project, s.provider, s.model, s.host, s.harness].join(' ').toLowerCase(); return words.every((w) => hay.includes(w)); }
  // the groups, in order: Pinned, the person's folders, Today, Earlier, Hidden (collapsed), Archived (collapsed). Searching
  // opens every group and leaves out what does not match (a hidden session found by search opens, and so is unhidden).
  function groupSessions(list, folders, opt) {
    const o = opt || {}, now = o.now || Date.now(), sod = startOfDay(now), q = norm(o.q);
    const fs = (folders || []).slice().sort((a, b) => (a.order || 0) - (b.order || 0));
    const groups = [{ key: 'pinned', label: 'Pinned', items: [] }, ...fs.map((f) => ({ key: 'f:' + f.id, label: f.name, folder: f, collapsed: !!f.collapsed, items: [] })),
      { key: 'today', label: 'Today', items: [] }, { key: 'earlier', label: 'Earlier', items: [] },
      { key: 'hidden', label: 'Hidden', collapsed: !o.showHidden, items: [] }, { key: 'archived', label: 'Archived', collapsed: !o.showArchived, items: [] }];
    const by = Object.fromEntries(groups.map((g) => [g.key, g]));
    for (const s of list || []) {
      if (q.length && !matches(s, q)) continue;
      const at = s.lastActivityAt || s.updatedAt || 0;
      const g = s.archived ? by.archived : s.hidden ? by.hidden : s.pinned ? by.pinned : s.folder && by['f:' + s.folder] ? by['f:' + s.folder] : at >= sod ? by.today : by.earlier;
      g.items.push(s);
    }
    for (const g of groups) { g.items.sort((a, b) => (b.lastActivityAt || 0) - (a.lastActivityAt || 0)); if (q.length) g.collapsed = false; }
    return groups.filter((g) => g.items.length || (g.folder && !q.length));
  }
  // "3 working · 1 needs you"
  function counterText(c) {
    const k = c || {}, w = k.working || 0, n = k.waiting || 0, x = k.externalWorking || 0;
    const parts = [w + ' working']; if (n) parts.push(n + ' need' + (n === 1 ? 's' : '') + ' you'); if (x) parts.push(x + ' in other tools');
    return parts.join(' · ');
  }
  // a host's name for people: "Hub", "Your host", "Gee's host" (its nickname on hover)
  function hostName(host, providers, meUser, nick) {
    if (!host || host === 'hub') return 'Hub';
    const p = (providers || []).find((x) => String(x.id || '').startsWith('h:' + host + '/'));
    if (!p) return host;
    if (p.owner) return p.owner === meUser ? 'Your host' : (nick ? nick(p.owner) : p.owner) + '\'s host';
    return p.nickname || host;
  }
  // the person's room per host, when the hub gives the limits: [{ host, text: "Gee's host 3/5", full, title }]
  function roomText(perHost, name) {
    return Object.entries(perHost || {}).filter(([, h]) => h && h.max).map(([host, h]) => {
      const label = name ? name(host) : host, prov = Object.entries(h.providers || {}).map(([p, n]) => p + ' ' + n + (h.maxPerProvider ? '/' + h.maxPerProvider : '')).join(', ');
      return { host, text: label + ' ' + (h.used || 0) + '/' + h.max, full: (h.used || 0) >= h.max, title: label + ': ' + (h.used || 0) + ' of ' + h.max + ' working sessions' + (prov ? ' · per AI: ' + prov : '') };
    });
  }
  function providerName(provider, providers) {
    if (!provider || provider === 'host' || provider === 'host-claude') return 'Hub Claude';
    const p = (providers || []).find((x) => x.id === provider);
    return p ? p.label || provider : provider;
  }
  // the one menu of a session ("…" and right-click are built from this list): only what the hub supports
  function menuItems(s, ctx) {
    const c = ctx || {}, items = [{ id: 'open', label: 'Open' }, { id: 'rename', label: 'Rename…' }, { id: 'pin', label: s.pinned ? 'Unpin' : 'Pin' },
      { id: 'move', label: 'Move to folder ▸', sub: [...(c.folders || []).map((f) => ({ id: 'move:' + f.id, label: f.name, on: s.folder === f.id })), { id: 'move:new', label: 'New folder…' }, ...(s.folder ? [{ id: 'move:none', label: 'No folder' }] : [])] },
      { id: 'share', label: c.shared ? 'Stop sharing' : 'Share with everyone' }];
    if (!s.primary && !s.noProject && s.worktree) items.push({ id: 'home', label: 'Merge into my work' });
    items.push({ id: 'link', label: 'Copy link' }, { id: 'hide', label: s.hidden ? 'Unhide' : 'Hide' });
    if (!s.archived) items.push({ id: 'archive', label: 'Archive' });
    if (!s.primary) items.push({ id: 'delete', label: 'Delete…', danger: true });
    return items;
  }
  function folderMenuItems(f) { return [{ id: 'frename', label: 'Rename…' }, { id: 'fcollapse', label: f.collapsed ? 'Expand' : 'Collapse' }, { id: 'fdelete', label: 'Delete folder…', danger: true }]; }

  // ── the panel (DOM; after sign-in) ──
  const P = { data: null, open: false, timer: null, bg: new Map(), retryAt: {}, showHidden: false, showArchived: false, showSaved: false, saved: null, savedRun: 0, menu: null, linked: '', draftTimer: null, started: false };
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const wide = () => !W.matchMedia || !W.matchMedia('(max-width: 900px)').matches;
  const onScreen = () => (typeof planSession !== 'undefined' && planSession) || (typeof SESSION !== 'undefined' && SESSION) || (P.data && P.data.primary) || '';
  const byId = (id) => ((P.data && P.data.sessions) || []).find((s) => s.id === id) || null;
  const DRAFTS = 'vr.sessDrafts';
  const drafts = () => { try { return JSON.parse(store.get(DRAFTS) || '{}') || {}; } catch { return {}; } };
  function draftOf(id) { return drafts()[id] || ''; }
  // keeps what was typed in a session: here (this browser) and on the hub (its blue dot, other devices)
  function keepDraft(id, text) {
    if (!id) return;
    clearTimeout(P.draftTimer);
    const d = drafts(), t = String(text || '').slice(0, 50 * 1024), had = d[id] || '';
    if (t) d[id] = t; else delete d[id];
    store.set(DRAFTS, JSON.stringify(d));
    if (t !== had || t) json('/api/work/draft', { method: 'POST', body: JSON.stringify({ session: id, draft: t }) }).catch(() => {});
  }
  // a session without a name takes the first words of its first message (as the saved conversations do)
  function autoTitle(message) {
    const id = onScreen(), s = byId(id); if (!s || s.title) return;
    const t = String(message || '').replace(/<!--vr-files[\s\S]*?-->/g, '').replace(/\s+/g, ' ').trim().slice(0, 60);
    if (!t || t.startsWith('/')) return;
    s.title = t; json('/api/work/sessions/rename', { method: 'POST', body: JSON.stringify({ id, title: t }) }).then(refresh).catch(() => {});
  }
  function typing(text) { clearTimeout(P.draftTimer); const id = onScreen(); P.draftTimer = setTimeout(() => keepDraft(id, text), 1200); }

  // anim (5.35.0): a click on the Sessions button opens or closes it with the genie (the page's genieIn / genieOut)
  function setOpen(on, focus, anim) {
    const g = anim && typeof genieIn === 'function';
    if (!on && P.open && g && !$('sessPanel').hidden) { P.open = false; $('sessToggle').classList.remove('on'); if (wide()) store.set('vr.sessPanel', '0'); $('spScrim').hidden = true; genieIn($('sessPanel'), $('sessToggle'), () => { if (!P.open) $('sessPanel').hidden = true; }); return; }
    if (g) genieStop($('sessPanel'));
    P.open = !!on; $('sessPanel').hidden = !P.open; $('spScrim').hidden = !P.open || wide();
    if (g && P.open) genieOut($('sessPanel'), $('sessToggle'));
    $('sessToggle').classList.toggle('on', P.open);
    if (wide()) store.set('vr.sessPanel', P.open ? '1' : '0');
    if (P.open) { render(); refresh(); if (focus) setTimeout(() => $('spQ').focus(), 0); }
  }
  function show(focus) { setOpen(true, focus); }
  function init() {
    $('sessToggle').onclick = () => setOpen(!P.open, false, true);
    $('spClose').onclick = () => setOpen(false, false, true);
    $('spScrim').onclick = () => setOpen(false);
    $('spNew').onclick = () => { if (typeof SESSIONS_OK !== 'undefined' && SESSIONS_OK) startOpen(); else newChatNow(); if (!wide()) setOpen(false); };
    let qt = null;
    $('spQ').addEventListener('input', () => { render(); clearTimeout(qt); qt = setTimeout(loadSaved, 250); });
    $('spQ').addEventListener('keydown', (e) => { if (e.key === 'Escape') { if ($('spQ').value) { $('spQ').value = ''; render(); loadSaved(); } else setOpen(false); } });
    document.addEventListener('click', (e) => { if (P.menu && !P.menu.contains(e.target)) closeMenu(); });
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && P.menu) closeMenu(); });
    W.addEventListener('resize', () => { if (P.open) $('spScrim').hidden = wide(); });
    const m = /^#\/?work\/(s-[0-9a-f]{12})$/.exec(location.hash || ''); if (m) P.linked = m[1];
  }
  // Work opened: the list, kept fresh while Work is on screen
  function start(list) {
    if (list) apply(list);
    if (!P.started) { P.started = true; const keep = store.get('vr.sessPanel'); setOpen(wide() && keep !== '0'); }
    clearInterval(P.timer); let tick = 0;
    // every 4 s while Work is in view; every 32 s otherwise (sessions followed in the background, queued messages)
    P.timer = setInterval(() => { tick++; if (me && ((!$('shell').hidden && document.visibilityState === 'visible') || tick % 8 === 0)) refresh(); }, 4000);
  }
  async function refresh() {
    if (!me) return;
    try { apply(await json('/api/work/sessions?archived=1')); } catch {}
  }
  function apply(d) {
    if (!d || !Array.isArray(d.sessions)) return;
    const sig = JSON.stringify(d), same = sig === P.sig; P.sig = sig;
    P.data = d; if (!same) render(); sync();
    const n = (d.counts || {}).working || 0, b = $('sessCount'); b.textContent = n ? String(n) : ''; b.hidden = !n;
    if (typeof onSessionSlots === 'function') onSessionSlots(d);
  }
  function linked() { const id = P.linked; P.linked = ''; return id; }

  function dot(s) {
    const v = stateView(s), d = el('span', 'spDot ' + v.state, v.shape);
    d.style.color = v.color; d.title = v.title; d.setAttribute('role', 'img'); d.setAttribute('aria-label', v.title);
    return d;
  }
  function render() {
    const d = P.data, list = $('spList'); if (!d || !list || $('sessPanel').hidden) return;
    const q = $('spQ').value.trim(), cur = onScreen(), had = document.activeElement && list.contains(document.activeElement) ? document.activeElement.closest('[data-id]') : null;
    $('spCount').textContent = counterText(d.counts);
    const room = $('spRoom'); room.textContent = '';
    for (const r of roomText((d.counts || {}).perHost, (h) => hostName(h, me.providers, me.username, typeof nickOf === 'function' ? nickOf : null))) { const x = el('span', r.full ? 'full' : '', r.text); x.title = r.title; room.append(x); }
    room.hidden = !room.childNodes.length;
    list.textContent = '';
    for (const g of groupSessions(d.sessions, d.folders, { q, showHidden: P.showHidden, showArchived: P.showArchived })) {
      const box = el('div', 'spGroup'), h = el('button', 'spGH'); h.type = 'button';
      const collapsible = g.folder || g.key === 'hidden' || g.key === 'archived';
      h.append(el('span', '', collapsible ? (g.collapsed ? '▸ ' : '▾ ') : ''), el('span', 'grow', g.label + (collapsible && g.collapsed ? ' (' + g.items.length + ')' : '')));
      if (g.folder) {
        const f = g.folder, more = el('span', 'spAct', '…'); more.title = 'Folder menu'; more.setAttribute('role', 'button');
        more.onclick = (e) => { e.stopPropagation(); const r = more.getBoundingClientRect(); popMenu(folderMenuItems(f), r.left, r.bottom, (id) => folderAct(f, id)); };
        h.append(more); h.oncontextmenu = (e) => { e.preventDefault(); popMenu(folderMenuItems(f), e.clientX, e.clientY, (id) => folderAct(f, id)); };
        h.onclick = () => folderAct(f, 'fcollapse');
      } else if (g.key === 'hidden') h.onclick = () => { P.showHidden = !P.showHidden; render(); };
      else if (g.key === 'archived') h.onclick = () => { P.showArchived = !P.showArchived; render(); };
      box.append(h);
      if (!g.collapsed) for (const s of g.items) box.append(item(s, s.id === cur));
      list.append(box);
    }
    // what other tools report (Jcode PGM, ...): shown, not opened here
    const ext = (d.external || []).filter((x) => !q || matches(x, q));
    if (ext.length) {
      const box = el('div', 'spGroup'); box.append(el('div', 'spGH', 'Other tools'));
      for (const x of ext) { const r = el('div', 'spItem ext'), t = el('div', 'spTxt'); t.append(el('b', '', x.title || x.id), el('small', '', [x.source, x.device, x.project, x.branch].filter(Boolean).join(' · '))); r.append(dot(x), t); box.append(r); }
      list.append(box);
    }
    savedGroup(list, q);
    if (had) { const back = list.querySelector('[data-id="' + had.dataset.id + '"]'); if (back) back.focus(); } // the list redraws itself: keyboard focus stays where it was
  }
  function item(s, current) {
    const r = el('div', 'spItem' + (current ? ' current' : '') + (s.hidden ? ' hid' : '')); r.tabIndex = 0; r.dataset.id = s.id;
    const t = el('div', 'spTxt'), meta = [s.noProject ? 'no project' : s.project, providerName(s.provider, me.providers) + (s.model ? ' ' + s.model : ''), hostName(s.host, me.providers, me.username, typeof nickOf === 'function' ? nickOf : null)];
    t.append(el('b', '', titleOf(s)), el('small', '', meta.filter(Boolean).join(' · ') + (s.queued ? ' · ' + s.queued + ' queued' : '')));
    t.title = titleOf(s) + '\n' + meta.join(' · ') + (s.worktree && s.worktree.branch ? '\n' + s.worktree.branch : '') + (s.mode === 'planner' ? '\nPlanner session' : '');
    const x = el('button', 'spAct'); x.append(ic(s.hidden ? 'eye' : 'close')); x.type = 'button'; x.title = s.hidden ? 'Unhide' : 'Hide (it keeps running; find it under Hidden)'; x.setAttribute('aria-label', x.title);
    x.onclick = (e) => { e.stopPropagation(); act(s, 'hide'); };
    const more = el('button', 'spAct', '…'); more.type = 'button'; more.title = 'More'; more.setAttribute('aria-label', 'More actions');
    more.onclick = (e) => { e.stopPropagation(); const b = more.getBoundingClientRect(); sessionMenu(s, b.left, b.bottom); };
    r.append(dot(s), t, x, more);
    r.onclick = () => act(s, 'open');
    r.onkeydown = (e) => { if (e.key === 'Enter') act(s, 'open'); else if (e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) { e.preventDefault(); const b = r.getBoundingClientRect(); sessionMenu(s, b.left + 20, b.bottom); } };
    r.oncontextmenu = (e) => { e.preventDefault(); sessionMenu(s, e.clientX, e.clientY); };
    return r;
  }
  function sessionMenu(s, x, y) { popMenu(menuItems(s, { folders: (P.data && P.data.folders) || [], shared: !!(typeof myShare !== 'undefined' && myShare) && s.id === onScreen() }), x, y, (id) => act(s, id)); }

  // one menu: a list of {id, label, danger?, sub?}; a "sub" opens in place (with a way back)
  function closeMenu() { if (P.menu) { P.menu.remove(); P.menu = null; } }
  function popMenu(items, x, y, pick) {
    closeMenu();
    const m = el('div', 'spMenu'); m.setAttribute('role', 'menu'); P.menu = m;
    const fill = (list, back) => {
      m.textContent = '';
      if (back) { const b = el('button', '', '‹ Back'); b.type = 'button'; b.onclick = (e) => { e.stopPropagation(); fill(items); }; m.append(b, el('hr')); }
      for (const it of list) {
        const b = el('button', it.danger ? 'danger' : ''); if (it.on) b.append(ic('check', 'lead')); b.append(it.label); b.type = 'button'; b.setAttribute('role', 'menuitem'); b.dataset.id = it.id;
        b.onclick = (e) => { e.stopPropagation(); if (it.sub) return fill(it.sub, true); closeMenu(); pick(it.id); };
        m.append(b);
      }
      const f = m.querySelector('button'); if (f) f.focus();
    };
    fill(items); document.body.append(m);
    const r = m.getBoundingClientRect();
    m.style.left = Math.max(4, Math.min(x, innerWidth - r.width - 4)) + 'px'; m.style.top = Math.max(4, Math.min(y, innerHeight - r.height - 4)) + 'px';
    m.addEventListener('keydown', (e) => { const bs = [...m.querySelectorAll('button')], i = bs.indexOf(document.activeElement); if (e.key === 'ArrowDown') { e.preventDefault(); (bs[i + 1] || bs[0]).focus(); } else if (e.key === 'ArrowUp') { e.preventDefault(); (bs[i - 1] || bs[bs.length - 1]).focus(); } });
  }

  const post = (path, body) => json(path, { method: 'POST', body: JSON.stringify(body) });
  const say = (t, bad) => sys(t, bad);
  async function act(s, id) {
    try {
      if (id === 'open') { if (!wide()) setOpen(false); await openWorkSession(s.id); return; }
      if (id === 'rename') { const t = prompt('Rename this session:', s.title || ''); if (t === null || !t.trim()) return; await post('/api/work/sessions/rename', { id: s.id, title: t.trim() }); }
      else if (id === 'pin') await post('/api/work/sessions/pin', { id: s.id, pinned: !s.pinned });
      else if (id === 'hide') await post('/api/work/sessions/hide', { id: s.id, hidden: !s.hidden });
      else if (id.startsWith('move:')) {
        let folder = id.slice(5);
        if (folder === 'new') { const n = prompt('Name of the new folder:', ''); if (!n || !n.trim()) return; folder = (await post('/api/work/folders/new', { name: n.trim() })).id; }
        await post('/api/work/sessions/move', { id: s.id, folder: folder === 'none' ? null : folder });
      } else if (id === 'share') { if (s.id !== onScreen()) await openWorkSession(s.id); $('shareBtn').click(); }
      else if (id === 'home') { const r = await api('/api/work/sessions/home', { method: 'POST', body: JSON.stringify({ id: s.id }) }); const d = await r.json().catch(() => ({})); say((r.ok ? 'Merged into your own work: ' : 'Not merged: ') + (d.text || d.error || r.statusText), !r.ok); }
      else if (id === 'link') {
        const u = location.origin + location.pathname + '#work/' + s.id;
        try { await navigator.clipboard.writeText(u); say('Link copied: it opens "' + titleOf(s) + '" in Work.'); } catch { prompt('The link to this session:', u); }
      } else if (id === 'archive') {
        if (s.running && !confirm('"' + titleOf(s) + '" is working. Stop it and archive it?')) return;
        if (s.running) await stopAndWait(s.id);
        if (s.id === onScreen() && !s.primary) await openWorkSession(P.data.primary);
        await post('/api/work/sessions/archive', { id: s.id }); say('"' + titleOf(s) + '" is archived (Archived, at the bottom of your sessions; opening it brings it back).');
      } else if (id === 'delete') {
        if (!confirm('Delete "' + titleOf(s) + '"?' + (s.running ? '\n\nIt is working: its running agents are stopped first.' : '') + '\n\nIts conversation goes; this cannot be undone (unmerged work in its worktree is kept as a branch).')) return;
        if (s.id === onScreen()) await openWorkSession(P.data.primary);
        await release(s.id);
        const d = await post('/api/work/sessions/delete', { id: s.id }); keepDraft(s.id, '');
        say('Deleted "' + titleOf(s) + '".' + (d.worktree ? ' ' + d.worktree : ''));
      }
    } catch (err) { say(err.message, true); }
    refresh();
  }
  async function folderAct(f, id) {
    try {
      if (id === 'frename') { const n = prompt('Rename the folder:', f.name); if (!n || !n.trim()) return; await post('/api/work/folders/rename', { id: f.id, name: n.trim() }); }
      else if (id === 'fcollapse') await post('/api/work/folders/rename', { id: f.id, name: f.name, collapsed: !f.collapsed });
      else if (id === 'fdelete') { if (!confirm('Delete the folder "' + f.name + '"? Its sessions move out of it (none is deleted).')) return; await post('/api/work/folders/delete', { id: f.id }); }
    } catch (err) { say(err.message, true); }
    refresh();
  }
  async function stopAndWait(id) {
    await api('/api/stop?session=' + encodeURIComponent(id), { method: 'POST', body: '{}' }).catch(() => {});
    for (let i = 0; i < 40; i++) { await new Promise((r) => setTimeout(r, 250)); await refresh(); const s = byId(id); if (!s || !s.running) return; }
  }

  // the saved conversations (Past sessions): searched by everything said in them, opened into the session on screen
  function savedGroup(list, q) {
    const box = el('div', 'spGroup'), h = el('button', 'spGH'); h.type = 'button';
    const open = P.showSaved || !!q;
    h.append(el('span', '', open ? '▾ ' : '▸ '), el('span', 'grow', 'Saved conversations'));
    h.title = 'Past sessions: saved by "Save session", /save or the auto-save (Settings → Auto-save). Searching looks into everything said in them.';
    h.onclick = () => { P.showSaved = !P.showSaved; render(); if (P.showSaved) loadSaved(); };
    box.append(h); list.append(box);
    if (!open) return;
    const items = P.saved && P.saved.q === q ? P.saved.items : null;
    if (!items) { box.append(el('div', 'spNote', 'Loading…')); if (!P.saved || P.saved.q !== q) loadSaved(); return; }
    if (!items.length) box.append(el('div', 'spNote', q ? 'No saved conversation matches.' : 'Nothing saved yet. Sessions are saved automatically (Settings → Auto-save) or with "Save session".'));
    const words = norm(q);
    for (const it of items) {
      const r = el('div', 'spItem saved' + (typeof archiveId !== 'undefined' && it.id === archiveId ? ' current' : '')); r.tabIndex = 0;
      const t = el('div', 'spTxt'), b = el('b'); markText(b, it.title, words);
      t.append(b, el('small', '', new Date(it.savedAt).toLocaleString() + ' · ' + it.count + ' entries' + (q ? ' · ' + it.hits + ' match' + (it.hits === 1 ? '' : 'es') : '')));
      for (const sn of it.snippets || []) { const sd = el('div', 'snip ' + (sn.cls || '')); markText(sd, sn.text, words); t.append(sd); }
      const ren = el('button', 'spAct'); ren.append(ic('edit')); ren.type = 'button'; ren.title = 'Rename'; ren.onclick = (e) => { e.stopPropagation(); renameConversation(it.id, it.title); };
      const del = el('button', 'spAct danger'); del.append(ic('trash')); del.type = 'button'; del.title = 'Delete this saved conversation';
      del.onclick = async (e) => {
        e.stopPropagation();
        if (del.dataset.armed !== '1') { del.dataset.armed = '1'; del.textContent = 'Confirm'; setTimeout(() => { del.dataset.armed = ''; del.textContent = ''; del.append(ic('trash')); }, 3000); return; }
        try { await post('/api/archive/delete', { id: it.id }); if (it.id === archiveId) setConversation(null, null, transcript); loadSaved(); } catch (err) { say(err.message, true); }
      };
      r.append(t, ren, del); r.title = 'Open it here: Claude continues that conversation in the session on screen';
      r.onclick = () => { openConversation(it.id); if (!wide()) setOpen(false); };
      r.onkeydown = (e) => { if (e.key === 'Enter') r.click(); };
      box.append(r);
    }
  }
  async function loadSaved() {
    const q = $('spQ').value.trim(); if (!P.showSaved && !q) return;
    const run = ++P.savedRun;
    try { const d = await post(q ? '/api/archive/search' : '/api/archive/list', q ? { q } : {}); if (run !== P.savedRun) return; P.saved = { q, items: d.items || [] }; }
    catch (err) { if (run === P.savedRun) P.saved = { q, items: [] }; }
    render();
  }

  // ── sessions not on screen: followed here, so their replies land in their own conversation and their queue goes ──
  function sync() {
    const cur = onScreen(), now = Date.now();
    for (const s of (P.data && P.data.sessions) || []) {
      if (s.id === cur || s.archived || P.bg.has(s.id)) continue;
      if (s.running || (s.queued && !(P.retryAt[s.id] > now))) follow(s.id, !!s.running);
    }
  }
  // stops following a session (it is about to be opened on screen) once its conversation is saved
  async function release(id) { const f = P.bg.get(id); if (f) { f.ac.abort(); await f.done; } }
  function follow(id, running) {
    const ac = new AbortController(), q = '?session=' + encodeURIComponent(id), f = { ac, done: null };
    let st = null, lastSave = 0;
    const save = async () => { if (st && st.dirty) { st.dirty = false; lastSave = Date.now(); await json('/api/work/current' + q, { method: 'POST', body: JSON.stringify({ transcript: st.tr, archiveId: st.archiveId, archiveTitle: st.archiveTitle, run: st.run }) }).catch(() => {}); } };
    const saveSoon = () => { if (Date.now() - lastSave > 3000) save(); };
    const on = (ev) => {
      st.dirty = true;
      if (ev.type === 'bridge_run') { st.run = { id: ev.id, n: 0 }; return; }
      if (ev.type === 'bridge_done') { st.ended = true; return; }
      const hv = typeof hubView === 'function' ? hubView(ev) : null;
      if (hv && hv.kind === 'assistant') for (const b of hv.blocks || []) {
        if (b.type === 'text' && b.text.trim()) st.tr.push({ cls: 'claude', text: b.text });
        else if (b.type === 'tool_use') st.tr.push({ cls: 'tool', text: describeTool(b) });
      }
    };
    const attach = async () => { // the run, from where its conversation is, to its end
      let run = st.run ? st.run.id : '', after = st.run ? st.run.n : 0;
      for (;;) {
        const d = await json('/api/work/attach' + q + '&run=' + encodeURIComponent(run) + '&after=' + after, { signal: ac.signal });
        if (!d || !d.run) break;
        for (const ev of d.events || []) on(ev);
        st.run = { id: d.run, n: d.next || 0 }; run = d.run; after = st.run.n; saveSoon();
        if (!d.busy) break;
      }
    };
    f.done = (async () => {
      try {
        const cur = await json('/api/work/current' + q, { signal: ac.signal });
        st = { tr: Array.isArray(cur.transcript) ? cur.transcript : [], run: cur.run || null, archiveId: cur.archiveId || null, archiveTitle: cur.archiveTitle || null };
        if (st.run || running) { await attach(); if (st.run) st.dirty = true; st.run = null; await save(); }
        // what waits in its queue: one after the other (a 429 / 409 keeps it queued; tried again a little later)
        let queue = Array.isArray(cur.queue) ? cur.queue.slice() : [], sendNow = cur.sendNow || null;
        for (let i = 0; i < 50 && !ac.signal.aborted; i++) {
          const next = sendNow || queue[0]; if (!next) break;
          const res = await api('/api/chat' + q, { method: 'POST', body: JSON.stringify({ message: next, fresh: false }), signal: ac.signal });
          if (!res.ok) { P.retryAt[id] = Date.now() + (res.status === 429 || res.status === 409 ? 15000 : 120000); try { await res.body.cancel(); } catch {} break; }
          if (sendNow) sendNow = null; else queue.shift();
          await post('/api/work/queue' + q, { queue, sendNow }).catch(() => {});
          st.tr.push({ cls: 'me', text: next }); st.run = null; st.ended = false; st.dirty = true;
          const reader = res.body.getReader(), dec = new TextDecoder(); let buf = '';
          try {
            for (;;) {
              const { value, done } = await reader.read(); if (done) break;
              buf += dec.decode(value, { stream: true }); let i2;
              while ((i2 = buf.indexOf('\n')) >= 0) { const l = buf.slice(0, i2); buf = buf.slice(i2 + 1); if (l.trim()) { try { on(JSON.parse(l)); } catch {} if (st.run) st.run.n++; } }
              saveSoon();
            }
          } catch { try { reader.cancel(); } catch {} }
          if (ac.signal.aborted) break;
          if (!st.ended) await attach();
          st.run = null; st.dirty = true; await save();
        }
      } catch {} // aborted (it is opened on screen now) or the hub is away: the next round of the list tries again
      finally { await save(); P.bg.delete(id); }
    })();
    P.bg.set(id, f);
  }

  W.SESSIONS = { STATES, stateView, groupSessions, counterText, roomText, hostName, providerName, menuItems, folderMenuItems, matches, titleOf, startOfDay,
    init, start, refresh, show, release, keepDraft, draftOf, typing, autoTitle, byId, linked, onScreen, seen: (id) => { const x = id || onScreen(); if (x && document.visibilityState === 'visible') post('/api/work/sessions/seen', { id: x }).then(refresh).catch(() => {}); },
    data: () => P.data };
})();
