// Mills team board: tasks that people and Claude agents work at the same time.
//
// Ideas carried over from HyperProductivityManager (rebuilt for the hub, not ported):
// - One task object is both the board card and the calendar entry (a task with a due date IS the
//   calendar item), so the two views can never drift apart.
// - Advisory, time-bounded claims: before working a task an actor (a person, or "<person> · Claude")
//   takes a claim; nobody else may edit the task while it is held; a claim that is not renewed
//   lapses, so a crashed or abandoned agent never holds work forever. Lapsed-but-unreleased claims
//   are reported as stale.
// - Every task carries a revision; an update may name the revision it was based on and is refused
//   if someone changed the task meanwhile (no silent overwrites).
// - Deletes are tombstones and every change goes to an append-only activity log.
// - Recurrence is month-end aware: a task due on the 31st that repeats monthly lands on the last day
//   of shorter months, then returns to the 31st.
//
// Storage: Mills/Board/board.json (gitignored, on the host only), written atomically. Board data is
// NOT end-to-end encrypted: Claude has to read it, exactly like Work runs.
'use strict';

const fs = require('fs');
const path = require('path');

const STATUSES = [
  { id: 'backlog', label: 'Backlog' }, { id: 'todo', label: 'To do' }, { id: 'doing', label: 'Doing' },
  { id: 'review', label: 'Review' }, { id: 'done', label: 'Done' },
];
const STATUS_IDS = STATUSES.map((s) => s.id);
const RECUR = ['daily', 'weekly', 'biweekly', 'monthly', 'quarterly', 'half_year', 'yearly'];
const CLAIM_DEFAULT_MIN = 15, CLAIM_MAX_MIN = 240;
const ACTIVITY_KEEP = 3000;

function addMonthsClamped(ms, months, anchorDay) {
  const d = new Date(ms);
  const day = anchorDay || d.getDate();
  const target = new Date(d.getFullYear(), d.getMonth() + months, 1, d.getHours(), d.getMinutes(), d.getSeconds());
  const last = new Date(target.getFullYear(), target.getMonth() + 1, 0).getDate();
  target.setDate(Math.min(day, last));
  return target.getTime();
}
function nextDue(ms, kind, anchorDay) {
  const DAY = 86400e3;
  switch (kind) {
    case 'daily': return ms + DAY;
    case 'weekly': return ms + 7 * DAY;
    case 'biweekly': return ms + 14 * DAY;
    case 'monthly': return addMonthsClamped(ms, 1, anchorDay);
    case 'quarterly': return addMonthsClamped(ms, 3, anchorDay);
    case 'half_year': return addMonthsClamped(ms, 6, anchorDay);
    case 'yearly': return addMonthsClamped(ms, 12, anchorDay);
    default: return null;
  }
}

module.exports = function createBoard({ dir, send, readBody, accounts, displayOf }) {
  const FILE = path.join(dir, 'board.json');
  fs.mkdirSync(dir, { recursive: true });
  let db;
  try { db = JSON.parse(fs.readFileSync(FILE, 'utf8')); } catch { db = null; }
  if (!db || !Array.isArray(db.tasks)) db = { n: 0, boards: [{ id: 'team', name: 'Team' }], tasks: [], activity: [] };
  let ver = 1;
  const waiters = new Set();
  const viewers = new Map(); // username -> last time they had the board open
  function save() {
    if (db.activity.length > ACTIVITY_KEEP) db.activity.splice(0, db.activity.length - ACTIVITY_KEEP);
    const tmp = FILE + '.tmp';
    fs.writeFileSync(tmp, JSON.stringify(db));
    fs.renameSync(tmp, FILE);
    ver++;
    for (const fn of [...waiters]) fn();
  }
  const now = () => Date.now();
  const live = (t) => !t.deleted;
  const find = (id) => db.tasks.find((t) => t.id === id && live(t));
  const claimActive = (t) => !!(t.claim && t.claim.until > now());
  const actorName = (a) => (a.agent ? displayOf(a.user) + ' · Claude' : displayOf(a.user));
  const actorKey = (a) => (a.agent ? a.user + '#agent' : a.user);
  const heldByOther = (t, a) => claimActive(t) && t.claim.key !== actorKey(a);
  function log(t, a, what, detail) {
    db.activity.push({ at: now(), task: t ? t.id : null, title: t ? t.title : '', by: actorName(a), agent: !!a.agent, what, ...(detail ? { detail: String(detail).slice(0, 300) } : {}) });
  }
  function touch(t) { t.rev = (t.rev || 0) + 1; t.updatedAt = now(); }
  const cleanLabels = (l) => (Array.isArray(l) ? l : String(l || '').split(',')).map((s) => String(s).trim().slice(0, 32)).filter(Boolean).slice(0, 12);
  const card = (t) => ({ // list payloads carry no prose: the description is fetched per task
    id: t.id, board: t.board, title: t.title, status: t.status, order: t.order, priority: t.priority || 0, labels: t.labels || [],
    dueAt: t.dueAt || null, recur: t.recur || null, assignee: t.assignee || null, createdBy: t.createdBy, updatedAt: t.updatedAt, rev: t.rev,
    hasBody: !!t.body, claim: claimActive(t) ? { by: t.claim.by, agent: t.claim.agent, until: t.claim.until, key: t.claim.key } : null,
    stale: !!(t.claim && !claimActive(t)),
  });
  function view(user) {
    const tasks = db.tasks.filter(live);
    const presence = [];
    for (const [u, at] of viewers) if (now() - at < 60e3 && accounts.has(u)) presence.push({ user: u, name: displayOf(u), agent: false });
    for (const t of tasks) if (claimActive(t) && t.claim.agent && !presence.some((p) => p.key === t.claim.key)) presence.push({ key: t.claim.key, name: t.claim.by, agent: true });
    return { ver, statuses: STATUSES, boards: db.boards, tasks: tasks.map(card), presence,
      activity: db.activity.slice(-60).reverse(), me: user };
  }

  // ── operations shared by people (HTTP) and Claude (internal MCP relay) ──
  // a = { user, agent: bool }. Each returns { status, body }.
  const ok = (body) => ({ status: 200, body });
  const no = (status, error) => ({ status, body: { error } });
  function guard(t, a, baseRev) {
    if (!t) return no(404, 'No such task.');
    if (heldByOther(t, a)) return no(409, `${t.claim.by} holds a claim on ${t.id} until ${new Date(t.claim.until).toLocaleTimeString()}. Edits are refused until it is released or lapses.`);
    if (baseRev != null && Number(baseRev) !== t.rev) return no(409, `${t.id} changed since you loaded it (revision ${t.rev}, you had ${baseRev}). Reload it and try again.`);
    return null;
  }
  const ops = {
    create(a, b) {
      const title = String(b.title || '').trim().slice(0, 200);
      if (!title) return no(400, 'A task needs a title.');
      const board = db.boards.some((x) => x.id === b.board) ? b.board : db.boards[0].id;
      const status = STATUS_IDS.includes(b.status) ? b.status : 'todo';
      const order = Math.max(0, ...db.tasks.filter((t) => live(t) && t.board === board && t.status === status).map((t) => t.order || 0)) + 1;
      const t = { id: 'T-' + (++db.n), board, title, body: String(b.body || '').slice(0, 50000), status, order, priority: Math.max(0, Math.min(5, b.priority | 0)),
        labels: cleanLabels(b.labels), dueAt: Number(b.dueAt) || null, recur: RECUR.includes(b.recur) ? b.recur : null,
        assignee: b.assignee && accounts.has(b.assignee) ? b.assignee : null, createdBy: actorName(a), createdAt: now(), updatedAt: now(), rev: 1, claim: null };
      t.dueDay = t.dueAt ? new Date(t.dueAt).getDate() : null; // the day a monthly repeat returns to (31st stays the 31st after February)
      db.tasks.push(t); log(t, a, 'created'); save();
      return ok({ task: { ...card(t), body: t.body } });
    },
    get(a, b) {
      const t = find(b.id);
      if (!t) return no(404, 'No such task.');
      return ok({ task: { ...card(t), body: t.body || '' }, activity: db.activity.filter((x) => x.task === t.id).slice(-40).reverse() });
    },
    update(a, b) {
      const t = find(b.id), g = guard(t, a, b.rev);
      if (g) return g;
      const changed = [];
      if (b.title !== undefined) { const v = String(b.title).trim().slice(0, 200); if (v && v !== t.title) { t.title = v; changed.push('title'); } }
      if (b.body !== undefined) { const v = String(b.body).slice(0, 50000); if (v !== (t.body || '')) { t.body = v; changed.push('description'); } }
      if (b.appendNote) { t.body = ((t.body || '') + '\n\n---\n**' + actorName(a) + '** · ' + new Date().toISOString().slice(0, 16).replace('T', ' ') + '\n\n' + String(b.appendNote).slice(0, 8000)).trim(); changed.push('note'); }
      if (b.labels !== undefined) { t.labels = cleanLabels(b.labels); changed.push('labels'); }
      if (b.priority !== undefined) { t.priority = Math.max(0, Math.min(5, b.priority | 0)); changed.push('priority ' + t.priority); }
      if (b.dueAt !== undefined) { t.dueAt = Number(b.dueAt) || null; t.dueDay = t.dueAt ? new Date(t.dueAt).getDate() : null; changed.push(t.dueAt ? 'due ' + new Date(t.dueAt).toISOString().slice(0, 10) : 'no due date'); }
      if (b.recur !== undefined) { t.recur = RECUR.includes(b.recur) ? b.recur : null; changed.push(t.recur ? 'repeats ' + t.recur : 'no repeat'); }
      if (b.assignee !== undefined) { t.assignee = b.assignee && accounts.has(b.assignee) ? b.assignee : null; changed.push(t.assignee ? 'assigned to ' + displayOf(t.assignee) : 'unassigned'); }
      if (b.status !== undefined && STATUS_IDS.includes(b.status) && b.status !== t.status) {
        if (b.status === 'done') return ops.complete(a, { id: t.id });
        t.status = b.status; changed.push('moved to ' + b.status);
      }
      if (!changed.length) return ok({ task: { ...card(t), body: t.body || '' } });
      touch(t); log(t, a, 'updated', changed.join(', ')); save();
      return ok({ task: { ...card(t), body: t.body || '' } });
    },
    move(a, b) { // drag and drop: new column + position
      const t = find(b.id), g = guard(t, a);
      if (g) return g;
      const status = STATUS_IDS.includes(b.status) ? b.status : t.status;
      if (status === 'done' && t.status !== 'done') return ops.complete(a, { id: t.id });
      const col = db.tasks.filter((x) => live(x) && x.board === t.board && x.status === status && x !== t).sort((p, q) => p.order - q.order);
      col.splice(Math.max(0, Math.min(col.length, b.index | 0)), 0, t);
      col.forEach((x, i) => { x.order = i + 1; });
      const moved = t.status !== status;
      t.status = status; touch(t);
      if (moved) log(t, a, 'moved', 'to ' + status);
      save();
      return ok({ task: card(t) });
    },
    complete(a, b) {
      const t = find(b.id), g = guard(t, a);
      if (g) return g;
      if (t.recur && t.dueAt) { // a repeating task re-arms instead of finishing
        t.dueAt = nextDue(t.dueAt, t.recur, t.dueDay || new Date(t.dueAt).getDate());
        t.status = 'todo'; t.claim = null; touch(t);
        log(t, a, 'completed', 'repeats ' + t.recur + ': next due ' + new Date(t.dueAt).toISOString().slice(0, 10));
      } else { t.status = 'done'; t.claim = null; touch(t); log(t, a, 'completed'); }
      save();
      return ok({ task: card(t) });
    },
    claim(a, b) {
      const t = find(b.id);
      if (!t) return no(404, 'No such task.');
      if (heldByOther(t, a)) return no(409, `${t.claim.by} already holds ${t.id} until ${new Date(t.claim.until).toLocaleTimeString()}.`);
      const minutes = Math.max(1, Math.min(CLAIM_MAX_MIN, Number(b.minutes) || CLAIM_DEFAULT_MIN));
      const renew = claimActive(t) && t.claim.key === actorKey(a);
      t.claim = { key: actorKey(a), by: actorName(a), agent: !!a.agent, user: a.user, until: now() + minutes * 60e3 };
      if (!renew && (t.status === 'todo' || t.status === 'backlog')) t.status = 'doing';
      touch(t); if (!renew) log(t, a, 'claimed', minutes + ' min'); save();
      return ok({ task: card(t), until: t.claim.until });
    },
    release(a, b) {
      const t = find(b.id);
      if (!t) return no(404, 'No such task.');
      if (heldByOther(t, a)) return no(409, `${t.claim.by} holds ${t.id}; only they can release it (or wait for it to lapse).`);
      if (t.claim) { t.claim = null; touch(t); log(t, a, 'released', b.note); save(); }
      return ok({ task: card(t) });
    },
    remove(a, b) {
      const t = find(b.id), g = guard(t, a);
      if (g) return g;
      t.deleted = true; t.claim = null; touch(t); log(t, a, 'deleted'); save(); // tombstone: kept, never shown
      return ok({ ok: true });
    },
    newBoard(a, b) {
      const name = String(b.name || '').trim().slice(0, 40);
      if (!name) return no(400, 'A board needs a name.');
      const id = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'board';
      if (db.boards.some((x) => x.id === id)) return no(400, 'That board exists already.');
      db.boards.push({ id, name }); log(null, a, 'created board', name); save();
      return ok({ boards: db.boards });
    },
    list(a, b) { // for Claude: a compact text-friendly list
      const v = view(a.user);
      let tasks = v.tasks;
      if (b.board) tasks = tasks.filter((t) => t.board === b.board);
      if (b.status) tasks = tasks.filter((t) => t.status === b.status);
      if (b.mine) tasks = tasks.filter((t) => t.assignee === a.user || (t.claim && t.claim.key === actorKey(a)));
      return ok({ boards: v.boards, statuses: STATUS_IDS, tasks });
    },
  };

  // ── HTTP for people ──
  const ROUTES = { '/api/board/create': 'create', '/api/board/task': 'get', '/api/board/update': 'update', '/api/board/move': 'move', '/api/board/complete': 'complete',
    '/api/board/claim': 'claim', '/api/board/release': 'release', '/api/board/delete': 'remove', '/api/board/new': 'newBoard' };
  async function handle(req, res, p, url, acct) {
    if (p === '/api/board' && req.method === 'GET') { // long poll like the chat: ?wait=1&ver=N
      viewers.set(acct.username, now());
      if (url.searchParams.get('wait') !== '1' || Number(url.searchParams.get('ver')) !== ver) { send(res, 200, view(acct.username)); return true; }
      let done = false;
      const finish = () => { if (done) return; done = true; clearTimeout(timer); waiters.delete(finish); viewers.set(acct.username, now()); send(res, 200, view(acct.username)); };
      const timer = setTimeout(finish, 25000);
      waiters.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); waiters.delete(finish); } });
      return true;
    }
    const op = ROUTES[p];
    if (!op) return false;
    const b = req.method === 'POST' ? await readBody(req) : Object.fromEntries(url.searchParams);
    const r = ops[op]({ user: acct.username, agent: false }, b || {});
    send(res, r.status, r.body);
    return true;
  }
  // ── internal: Claude in a Work run (board MCP tools), acting as "<person> · Claude" ──
  async function internal(req, res, acctName) {
    const { name, args } = await readBody(req);
    const map = { board_list: 'list', task_get: 'get', task_create: 'create', task_update: 'update', task_claim: 'claim', task_renew: 'claim', task_release: 'release', task_complete: 'complete' };
    const op = map[name];
    if (!op || !accounts.has(acctName)) return send(res, 200, { ok: false, error: op ? 'Unknown account.' : 'Unknown board tool: ' + name });
    const r = ops[op]({ user: acctName, agent: true }, args || {});
    send(res, 200, r.status === 200 ? { ok: true, result: r.body } : { ok: false, error: r.body.error });
  }
  return { handle, internal, STATUSES, nextDue };
};
module.exports.nextDue = nextDue;
