// Mills team board, calendar and the Work "My tasks" pane (page side of bridge/board.js).
// Loaded before the main page script; everything here runs only after sign-in, so it may use the
// page's helpers ($, json, ic, label, MD, me, nickOf, colorOf, showScreen, …) at call time.
(function () {
  'use strict';
  const B = { data: null, ver: 0, gen: 0, board: 'team', filter: '', mine: false, open: null, detail: null, cal: null, members: [] };
  const DAY = 86400e3;
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const myKey = () => me.username;
  const isMine = (t) => t.assignee === me.username || (t.claim && (t.claim.key === myKey() || t.claim.key === myKey() + '#agent'));
  const startOfDay = (ms) => { const d = new Date(ms); d.setHours(0, 0, 0, 0); return d.getTime(); };
  const dueClass = (t) => (!t.dueAt || t.status === 'done' ? '' : t.dueAt < startOfDay(Date.now()) ? 'overdue' : t.dueAt < startOfDay(Date.now()) + DAY ? 'today' : '');
  const fmtDue = (ms) => new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  const minsLeft = (until) => Math.max(0, Math.round((until - Date.now()) / 60000));
  const personName = (u) => { const m = B.members.find((x) => x.username === u); return m ? m.nick || m.display : u; };
  const RECUR_LABEL = { daily: 'daily', weekly: 'weekly', biweekly: 'every 2 weeks', monthly: 'monthly', quarterly: 'quarterly', half_year: 'every 6 months', yearly: 'yearly' };
  function note(text, bad) { const f = $('bdActivity'); if (!f) return; f.textContent = ''; const s = el('span', bad ? 'bdErr' : '', text); f.append(s); setTimeout(() => { if (s.isConnected) renderFoot(); }, 5000); }

  async function op(name, body) {
    try { const r = await json('/api/board/' + name, { method: 'POST', body: JSON.stringify(body) }); await refresh(); return r; }
    catch (e) { note(e.message, true); if (!$('shell').hidden) sys(e.message, true); return null; }
  }
  async function refresh() { try { apply(await json('/api/board')); } catch {} }
  function apply(d) {
    if (!me) return;
    B.data = d; B.ver = d.ver;
    if (!d.boards.some((b) => b.id === B.board)) B.board = d.boards[0].id;
    if (!$('board').hidden) renderBoard();
    if (!$('cal').hidden) renderCal();
    renderTaskPane(); renderBadge();
  }
  async function loop(gen) {
    while (me && gen === B.gen) {
      try { const d = await json('/api/board?wait=1&ver=' + B.ver); if (gen === B.gen) apply(d); }
      catch { await sleep(3000); }
    }
  }
  async function loadMembers() {
    try { B.members = ((await json('/api/room/members')).members || []).filter((m) => !m.agent); } catch {}
  }

  // ── Board screen ──
  function visibleTasks() {
    const q = B.filter.trim().toLowerCase();
    return B.data.tasks.filter((t) => t.board === B.board && (!B.mine || isMine(t)) && (!q
      || [t.id, t.title, ...(t.labels || []), t.assignee ? personName(t.assignee) : '', t.claim ? t.claim.by : ''].some((s) => String(s).toLowerCase().includes(q))));
  }
  function renderBoard() {
    if (!B.data) return;
    const sel = $('bdBoard'); sel.textContent = '';
    for (const b of B.data.boards) { const o = el('option', '', b.name); o.value = b.id; sel.append(o); }
    sel.value = B.board;
    const tasks = visibleTasks(), cols = $('bdCols'); cols.textContent = '';
    for (const s of B.data.statuses) {
      const list = tasks.filter((t) => t.status === s.id).sort((a, b) => a.order - b.order);
      const col = el('section', 'bdCol'); col.dataset.status = s.id;
      const head = el('header'); head.append(el('span', 'bdColName', s.label), el('span', 'bdCount', String(list.length)));
      const add = el('button', 'ghost bdAdd'); add.type = 'button'; add.title = 'Add a task to ' + s.label; add.append(ic('plus'));
      add.onclick = () => newTask(s.id); head.append(add);
      const body = el('div', 'bdCards');
      list.forEach((t, i) => body.append(cardEl(t, i)));
      if (!list.length) body.append(el('div', 'bdNone', 'nothing here'));
      dropZone(body, s.id, list.length);
      col.append(head, body); cols.append(col);
    }
    const stale = B.data.tasks.filter((t) => t.stale && t.board === B.board);
    $('bdStale').hidden = !stale.length;
    if (stale.length) label($('bdStale'), 'warn', stale.length + ' lapsed claim' + (stale.length > 1 ? 's' : '') + ': ' + stale.map((t) => t.id).join(', '));
    renderFoot();
    if (B.open && !B.data.tasks.some((t) => t.id === B.open)) closeDetail();
    else if (B.open && !B.editing) openDetail(B.open, true);
  }
  function renderFoot() {
    if (!B.data) return;
    const pres = $('bdPresence'); pres.textContent = '';
    pres.append(el('span', 'bdMuted', 'Here now: '));
    for (const p of B.data.presence) { const c = el('span', 'bdChip' + (p.agent ? ' agent' : '')); c.append(ic(p.agent ? 'bot' : 'eye', 'lead'), p.name); pres.append(c); }
    const act = $('bdActivity'); act.textContent = '';
    for (const a of B.data.activity.slice(0, 4)) act.append(el('span', 'bdAct' + (a.agent ? ' agent' : ''), `${a.by} ${a.what}${a.task ? ' ' + a.task : ''}${a.detail ? ' (' + a.detail + ')' : ''} · ${new Date(a.at).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })}`));
  }
  let dragging = null;
  function cardEl(t, index) {
    const c = el('div', 'bdCard' + (B.open === t.id ? ' on' : '') + (t.claim ? ' held' : '') + (t.stale ? ' stale' : ''));
    c.dataset.id = t.id;
    const held = t.claim && t.claim.key !== myKey() && t.claim.key !== myKey() + '#agent';
    c.draggable = !held;
    const top = el('div', 'bdTop');
    top.append(el('span', 'bdId', t.id));
    if (t.priority) top.append(el('span', 'bdPri p' + t.priority, 'P' + t.priority));
    if (t.recur) { const r = el('span', 'bdRecur'); r.title = 'Repeats ' + RECUR_LABEL[t.recur]; r.append(ic('repeat')); top.append(r); }
    if (t.dueAt) top.append(el('span', 'bdDue ' + dueClass(t), fmtDue(t.dueAt)));
    if (t.assignee) { const a = el('span', 'bdAva', personName(t.assignee).slice(0, 1).toUpperCase()); a.title = 'Assigned to ' + personName(t.assignee); a.style.background = colorOf(t.assignee); top.append(a); }
    c.append(top, el('div', 'bdTitle', t.title));
    if (t.labels.length) { const l = el('div', 'bdLabels'); for (const x of t.labels) l.append(el('span', 'bdLabel', x)); c.append(l); }
    if (t.claim) { const h = el('div', 'bdClaim' + (t.claim.agent ? ' agent' : '')); h.append(ic(t.claim.agent ? 'bot' : 'flag', 'lead'), t.claim.by + ' · ' + minsLeft(t.claim.until) + ' min'); c.append(h); }
    else if (t.stale) { const h = el('div', 'bdClaim stale'); h.append(ic('warn', 'lead'), 'claim lapsed'); c.append(h); }
    c.onclick = () => openDetail(t.id);
    c.addEventListener('dragstart', (e) => { dragging = t; e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', t.id); c.classList.add('drag'); });
    c.addEventListener('dragend', () => { dragging = null; c.classList.remove('drag'); document.querySelectorAll('.bdCard.insert').forEach((x) => x.classList.remove('insert')); });
    c.addEventListener('dragover', (e) => { if (!dragging) return; e.preventDefault(); e.stopPropagation(); document.querySelectorAll('.bdCard.insert').forEach((x) => x.classList.remove('insert')); c.classList.add('insert'); });
    c.addEventListener('drop', (e) => { e.preventDefault(); e.stopPropagation(); drop(t.status, index); });
    return c;
  }
  function dropZone(zone, status, end) {
    zone.addEventListener('dragover', (e) => { if (dragging) { e.preventDefault(); zone.classList.add('target'); } });
    zone.addEventListener('dragleave', () => zone.classList.remove('target'));
    zone.addEventListener('drop', (e) => { e.preventDefault(); zone.classList.remove('target'); drop(status, end); });
  }
  async function drop(status, index) {
    const t = dragging; dragging = null;
    if (!t) return;
    await op('move', { id: t.id, status, index });
  }
  async function newTask(status, dueAt) {
    const title = window.prompt('New task' + (dueAt ? ' due ' + fmtDue(dueAt) : '') + ':');
    if (!title || !title.trim()) return;
    const r = await op('create', { title: title.trim(), status: status || 'todo', board: B.board, ...(dueAt ? { dueAt } : {}) });
    if (r && r.task && !$('board').hidden) openDetail(r.task.id);
  }

  // ── task detail (side panel on the board) ──
  function closeDetail() { B.open = null; B.detail = null; B.editing = false; $('bdDetail').hidden = true; document.querySelectorAll('.bdCard.on').forEach((x) => x.classList.remove('on')); }
  async function openDetail(id, quiet) {
    let d;
    try { d = await json('/api/board/task?id=' + encodeURIComponent(id)); } catch (e) { if (!quiet) note(e.message, true); return; }
    if (quiet && B.editing) return;
    B.open = id; B.detail = d.task;
    document.querySelectorAll('.bdCard').forEach((x) => x.classList.toggle('on', x.dataset.id === id));
    if (!B.members.length) await loadMembers();
    renderDetail(d.task, d.activity || []);
  }
  function renderDetail(t, activity) {
    const p = $('bdDetail'); p.hidden = false; p.textContent = '';
    const held = t.claim && t.claim.key !== myKey() && t.claim.key !== myKey() + '#agent';
    const mineHeld = t.claim && !held;
    const head = el('div', 'bdDHead'); head.append(el('span', 'bdId', t.id + ' · rev ' + t.rev));
    const x = el('button', 'ghost'); x.type = 'button'; x.title = 'Close (Esc)'; x.append(ic('close')); x.onclick = closeDetail; head.append(x);
    p.append(head);
    if (held) { const w = el('div', 'bdWarn'); w.append(ic('lock', 'lead'), `${t.claim.by} holds a claim on this task for ${minsLeft(t.claim.until)} more min. Edits are refused until it is released or lapses.`); p.append(w); }
    const field = (lbl, node) => { const f = el('label', 'bdField'); f.append(el('span', '', lbl), node); p.append(f); return node; };
    const title = field('Title', el('input')); title.value = t.title;
    const bodyWrap = el('div', 'bdBodyWrap');
    const body = el('textarea'); body.rows = 9; body.value = t.body || ''; body.placeholder = 'Markdown (tables, lists, ```mermaid diagrams…). Ctrl+Enter saves.';
    const prev = el('div', 'bdPrev'); prev.hidden = true;
    const tog = el('button', 'ghost bdTiny', 'Preview'); tog.type = 'button';
    tog.onclick = () => { prev.hidden = !prev.hidden; body.hidden = !prev.hidden; tog.textContent = prev.hidden ? 'Preview' : 'Edit'; if (!prev.hidden) MD.render(prev, body.value || '_No description._'); };
    const bh = el('div', 'bdBodyHead'); bh.append(el('span', '', 'Description'), tog);
    bodyWrap.append(bh, body, prev); p.append(bodyWrap);
    const labels = field('Labels', el('input')); labels.value = t.labels.join(', '); labels.placeholder = 'comma, separated';
    const row = el('div', 'bdRow'); p.append(row);
    const mini = (lbl, node) => { const f = el('label', 'bdField'); f.append(el('span', '', lbl), node); row.append(f); return node; };
    const status = mini('Status', el('select')); for (const s of B.data.statuses) { const o = el('option', '', s.label); o.value = s.id; status.append(o); } status.value = t.status;
    const due = mini('Due', el('input')); due.type = 'date'; if (t.dueAt) { const d = new Date(t.dueAt); due.value = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`; }
    const recur = mini('Repeats', el('select')); for (const [v, l] of [['', 'never'], ...Object.entries(RECUR_LABEL)]) { const o = el('option', '', l); o.value = v; recur.append(o); } recur.value = t.recur || '';
    const row2 = el('div', 'bdRow'); p.append(row2);
    const assignee = el('select'); { const o = el('option', '', 'nobody'); o.value = ''; assignee.append(o); for (const m of B.members) { const q = el('option', '', m.nick || m.display); q.value = m.username; assignee.append(q); } assignee.value = t.assignee || ''; }
    const fa = el('label', 'bdField'); fa.append(el('span', '', 'Assignee'), assignee); row2.append(fa);
    const pri = el('div', 'bdPriRow'); let priority = t.priority;
    for (let i = 0; i <= 5; i++) { const b = el('button', 'ghost' + (i === priority ? ' on' : ''), i ? 'P' + i : '–'); b.type = 'button'; b.onclick = () => { priority = i; pri.querySelectorAll('button').forEach((y, k) => y.classList.toggle('on', k === i)); dirty(); }; pri.append(b); }
    const fp = el('div', 'bdField'); fp.append(el('span', '', 'Priority'), pri); row2.append(fp);
    // B.editing = unsaved changes: while set, a board update never re-renders this panel under your cursor
    for (const n of [title, body, labels, status, due, recur, assignee]) { n.disabled = held; n.addEventListener('input', dirty); n.addEventListener('change', dirty); }
    pri.querySelectorAll('button').forEach((b) => { b.disabled = held; });
    const save = el('button', '', 'Save'); save.type = 'button'; save.disabled = true;
    function dirty() { save.disabled = held; B.editing = true; }
    save.onclick = async () => {
      const dueAt = due.value ? new Date(due.value + 'T17:00:00').getTime() : null;
      const r = await op('update', { id: t.id, rev: t.rev, title: title.value, body: body.value, labels: labels.value, status: status.value, dueAt, recur: recur.value || null, assignee: assignee.value || null, priority });
      B.editing = false; if (r) openDetail(t.id);
    };
    body.addEventListener('keydown', (e) => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) save.click(); });
    const acts = el('div', 'bdActs');
    const btn = (icon, text, fn, cls) => { const b = label(el('button', cls || 'ghost'), icon, text); b.type = 'button'; b.onclick = fn; acts.append(b); return b; };
    acts.append(save);
    if (!held) {
      btn('flag', mineHeld ? 'Renew 30 min' : 'Claim 30 min', () => op('claim', { id: t.id, minutes: 30 }).then(() => openDetail(t.id)));
      if (mineHeld) btn('close', 'Release', () => op('release', { id: t.id }).then(() => openDetail(t.id)));
      if (t.status !== 'done') btn('check', t.recur ? 'Complete (repeats)' : 'Complete', () => op('complete', { id: t.id }).then(() => openDetail(t.id)));
    }
    btn('bot', 'Give to Claude', () => giveToClaude(t));
    if (!held) {
      const del = btn('trash', 'Delete', async () => {
        if (del.dataset.armed !== '1') { del.dataset.armed = '1'; label(del, 'trash', 'Click again to delete'); return; }
        if (await op('delete', { id: t.id })) closeDetail();
      }, 'ghost bdDanger');
    }
    p.append(acts);
    const meta = el('div', 'bdMeta', `Created by ${t.createdBy} · updated ${new Date(t.updatedAt).toLocaleString()}`); p.append(meta);
    const log = el('div', 'bdLog'); log.append(el('b', '', 'Activity'));
    for (const a of activity) log.append(el('div', 'bdAct' + (a.agent ? ' agent' : ''), `${new Date(a.at).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })} · ${a.by} ${a.what}${a.detail ? ' (' + a.detail + ')' : ''}`));
    p.append(log);
    B.editing = false;
  }
  function giveToClaude(t) {
    showScreen('shell');
    $('input').value = `Work on board task ${t.id}: "${t.title}". Read it with task_get, claim it with task_claim before starting, add progress with task_update appendNote, and task_complete it when it is done (or task_release it with a note if you cannot finish).`;
    $('input').focus();
    $('input').dispatchEvent(new Event('input'));
  }

  // ── Calendar: a month of tasks by due date ──
  function renderCal() {
    if (!B.data) return;
    if (!B.cal) { const d = new Date(); B.cal = new Date(d.getFullYear(), d.getMonth(), 1); }
    const m = B.cal, grid = $('calGrid'); grid.textContent = '';
    $('calTitle').textContent = m.toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
    if (window.matchMedia('(max-width: 700px)').matches) return renderAgenda(m, grid); // phone: a list of days, not 7 cramped columns
    grid.classList.remove('agenda');
    const first = new Date(m.getFullYear(), m.getMonth(), 1), lead = (first.getDay() + 6) % 7; // weeks start on Monday
    const start = new Date(first.getFullYear(), first.getMonth(), 1 - lead);
    for (let i = 0; i < 7; i++) grid.append(el('div', 'calHead', new Date(2024, 0, 1 + i).toLocaleDateString(undefined, { weekday: 'short' })));
    const today = startOfDay(Date.now());
    const byDay = new Map();
    for (const t of B.data.tasks) if (t.dueAt) { const k = startOfDay(t.dueAt); (byDay.get(k) || byDay.set(k, []).get(k)).push(t); }
    for (let i = 0; i < 42; i++) {
      const day = new Date(start.getFullYear(), start.getMonth(), start.getDate() + i), k = day.getTime();
      const cell = el('div', 'calDay' + (day.getMonth() !== m.getMonth() ? ' other' : '') + (k === today ? ' today' : ''));
      cell.append(el('div', 'calNum', String(day.getDate())));
      for (const t of (byDay.get(k) || []).sort((a, b) => (b.priority || 0) - (a.priority || 0))) {
        const c = el('div', 'calTask s-' + t.status + ' ' + dueClass(t)); c.title = `${t.id} · ${t.title}${t.assignee ? ' · ' + personName(t.assignee) : ''}`;
        if (t.recur) c.append(ic('repeat', 'lead'));
        c.append(t.title);
        if (t.assignee) c.style.borderLeftColor = colorOf(t.assignee);
        c.onclick = (e) => { e.stopPropagation(); B.board = t.board; showScreen('board'); openDetail(t.id); };
        cell.append(c);
      }
      cell.onclick = () => newTask('todo', new Date(day.getFullYear(), day.getMonth(), day.getDate(), 17).getTime());
      grid.append(cell);
    }
  }

  function renderAgenda(m, grid) { // every day of the month that has a task, plus today; tap a day to add one
    grid.classList.add('agenda');
    const today = startOfDay(Date.now()), last = new Date(m.getFullYear(), m.getMonth() + 1, 0).getDate();
    const byDay = new Map();
    for (const t of B.data.tasks) if (t.dueAt) { const k = startOfDay(t.dueAt); (byDay.get(k) || byDay.set(k, []).get(k)).push(t); }
    let any = false;
    for (let d = 1; d <= last; d++) {
      const day = new Date(m.getFullYear(), m.getMonth(), d), k = day.getTime(), list = byDay.get(k) || [];
      if (!list.length && k !== today) continue;
      any = true;
      const cell = el('div', 'agDay' + (k === today ? ' today' : ''));
      cell.append(el('div', 'calNum', day.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' }) + (k === today ? ' · today' : '')));
      for (const t of list.sort((a, b) => (b.priority || 0) - (a.priority || 0))) {
        const c = el('div', 'calTask s-' + t.status + ' ' + dueClass(t));
        if (t.recur) c.append(ic('repeat', 'lead'));
        c.append(t.title + (t.assignee ? ' · ' + personName(t.assignee) : ''));
        if (t.assignee) c.style.borderLeftColor = colorOf(t.assignee);
        c.onclick = (e) => { e.stopPropagation(); B.board = t.board; showScreen('board'); openDetail(t.id); };
        cell.append(c);
      }
      if (!list.length) cell.append(el('div', 'bdNone', 'nothing due · tap to add'));
      cell.onclick = () => newTask('todo', new Date(day.getFullYear(), day.getMonth(), day.getDate(), 17).getTime());
      grid.append(cell);
    }
    if (!any) grid.append(el('div', 'bdNone', 'Nothing due this month.'));
  }
  window.matchMedia('(max-width: 700px)').addEventListener('change', () => { if (B.data && !$('cal').hidden) renderCal(); });

  // ── Work: My tasks pane ──
  function renderTaskPane() {
    const host = $('taskPaneBody'); if (!host || !B.data) return;
    host.textContent = '';
    const open = B.data.tasks.filter((t) => t.status !== 'done');
    const held = open.filter((t) => t.claim && (t.claim.key === myKey() || t.claim.key === myKey() + '#agent'));
    const soon = open.filter((t) => t.dueAt && t.dueAt < startOfDay(Date.now()) + 7 * DAY && (t.assignee === me.username || !t.assignee) && !held.includes(t)).sort((a, b) => a.dueAt - b.dueAt);
    const mine = open.filter((t) => t.assignee === me.username && !held.includes(t) && !soon.includes(t)).sort((a, b) => (b.priority || 0) - (a.priority || 0));
    const section = (title, list, empty) => {
      host.append(el('div', 'tpHead', title + ' (' + list.length + ')'));
      if (!list.length) host.append(el('div', 'tpNone', empty));
      for (const t of list) {
        const r = el('div', 'tpItem');
        const top = el('div', 'bdTop'); top.append(el('span', 'bdId', t.id));
        if (t.priority) top.append(el('span', 'bdPri p' + t.priority, 'P' + t.priority));
        if (t.dueAt) top.append(el('span', 'bdDue ' + dueClass(t), fmtDue(t.dueAt)));
        if (t.claim) top.append(el('span', 'bdMuted', (t.claim.agent ? 'Claude · ' : '') + minsLeft(t.claim.until) + ' min'));
        r.append(top, el('div', 'bdTitle', t.title));
        const acts = el('div', 'tpActs');
        const give = label(el('button', 'ghost'), 'bot', 'Claude'); give.type = 'button'; give.title = 'Put this task in the box for Claude'; give.onclick = () => giveToClaude(t);
        const openB = label(el('button', 'ghost'), 'board', 'Open'); openB.type = 'button'; openB.onclick = () => { B.board = t.board; showScreen('board'); openDetail(t.id); };
        acts.append(give, openB); r.append(acts); host.append(r);
      }
    };
    section('Held by you or your Claude', held, 'Nothing claimed.');
    section('Due within a week', soon, 'Nothing due.');
    section('Assigned to you', mine, 'Nothing else assigned.');
  }
  function renderBadge() {
    if (!B.data || !me) return;
    const n = B.data.tasks.filter((t) => t.status !== 'done' && t.dueAt && t.dueAt < startOfDay(Date.now()) + DAY && (t.assignee === me.username || !t.assignee)).length;
    setBadge($('boardBadge'), n); $('boardBadge').title = n ? n + ' task' + (n > 1 ? 's' : '') + ' due today or overdue' : '';
  }

  // ── wiring ──
  function wire() {
    if (wire.done) return; wire.done = true;
    $('bdBoard').onchange = () => { B.board = $('bdBoard').value; closeDetail(); renderBoard(); };
    $('bdFilter').oninput = () => { B.filter = $('bdFilter').value; renderBoard(); };
    $('bdMine').onchange = () => { B.mine = $('bdMine').checked; renderBoard(); };
    $('bdNewBoard').onclick = async () => { const n = window.prompt('Name of the new board:'); if (n && n.trim()) { const r = await op('new', { name: n.trim() }); if (r) { B.board = r.boards[r.boards.length - 1].id; renderBoard(); } } };
    $('calPrev').onclick = () => { B.cal = new Date(B.cal.getFullYear(), B.cal.getMonth() - 1, 1); renderCal(); };
    $('calNext').onclick = () => { B.cal = new Date(B.cal.getFullYear(), B.cal.getMonth() + 1, 1); renderCal(); };
    $('calToday').onclick = () => { B.cal = null; renderCal(); };
    $('tpOpenBoard').onclick = () => showScreen('board');
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !$('board').hidden && B.open && !e.target.closest('input, textarea, select')) closeDetail(); });
  }
  window.BOARD = {
    start() { wire(); B.gen++; B.ver = 0; loadMembers(); loop(B.gen); },
    stop() { B.gen++; B.data = null; B.ver = 0; closeDetail(); },
    show(name) { wire(); if (name === 'board') { loadMembers().then(() => B.data && renderBoard()); } if (B.data) (name === 'board' ? renderBoard : renderCal)(); },
  };
})();
