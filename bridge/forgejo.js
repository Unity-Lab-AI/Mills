// Mills ↔ Forgejo issue mirroring (an idea inherited from HyperProductivityManager / ProjectTaskManager:
// Forgejo has no plugin system, so this is an outside service speaking its REST API).
//
// Off until the host writes bridge/forgejo.json (gitignored):
//   { "url": "https://git.unityailab.com", "token": "<a Forgejo access token with issue rights>",
//     "repo": "UnityAILab/Mills", "board": "team", "pollMinutes": 2 }
// Then, for the tasks of that board:
//   Mills → Forgejo  a new task becomes an issue (title, body + "Mills task T-n"); title / body / label
//                    changes update it; completing closes it; moving a done task back reopens it;
//                    deleting closes it with a comment.
//   Forgejo → Mills  every pollMinutes the mirror reads issues changed since the last look: a closed issue
//                    completes its task, a reopened one puts it back in To do, title / body changes land
//                    on the task, and an issue nobody mirrored yet becomes a task (label "forgejo").
// Loops are avoided by remembering, per task, the issue's updated_at we last saw or produced; anything
// older is ignored. The mirror acts on the board as "Forgejo".
'use strict';

const fs = require('fs');
const path = require('path');

module.exports = function createForgejo({ ROOT, board, readJson, log = console.log }) {
  const FILE = path.join(ROOT, 'forgejo.json');
  const cfg = readJson(FILE, null);
  const on = !!(cfg && cfg.url && cfg.token && cfg.repo);
  const BOARD = (cfg && cfg.board) || 'team';
  const base = on ? String(cfg.url).replace(/\/+$/, '') + '/api/v1/repos/' + String(cfg.repo).replace(/^\/+|\/+$/g, '') : '';
  const ACTOR = { user: 'forgejo', agent: false, name: 'Forgejo' };
  const MARK = (id) => `\n\n---\n_Mills task ${id} — mirrored by Mills; edit it in either place._`;
  let lastPoll = cfg && cfg.since ? cfg.since : new Date(0).toISOString();
  const busy = new Set(), again = new Set(); // task ids with an outbound call in flight / changed meanwhile (re-synced after)

  async function api(method, p, body) {
    const r = await fetch(base + p, { method, headers: { Authorization: 'token ' + cfg.token, 'Content-Type': 'application/json', Accept: 'application/json' }, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(15000) });
    if (!r.ok) throw new Error(`Forgejo ${method} ${p} → ${r.status} ${(await r.text()).slice(0, 200)}`);
    return r.status === 204 ? null : r.json();
  }
  const issueBody = (t) => (t.body || '').replace(/\n\n---\n_Mills task T-\d+[^\n]*_\s*$/, '') + MARK(t.id);
  const labelsOf = (t) => [...new Set([...(t.labels || []), ...(t.priority ? ['P' + t.priority] : [])])];

  // ---- Mills → Forgejo ----
  async function outbound(what, t) {
    if (!on || t.board !== BOARD) return;
    if (busy.has(t.id)) { again.add(t.id); return; } // the call in flight re-syncs the task when it is done
    busy.add(t.id);
    try {
      const ext = t.forgejo || null;
      if (what === 'deleted') { if (ext) { await api('POST', `/issues/${ext.number}/comments`, { body: `Deleted on the Mills board (${t.id}).` }); await api('PATCH', `/issues/${ext.number}`, { state: 'closed' }); } return; }
      if (!ext) {
        if (what === 'completed' && t.status === 'done') return; // nothing to mirror for a finished task that never had an issue
        const is = await api('POST', '/issues', { title: t.title, body: issueBody(t), labels: [] });
        board.setExternal(t.id, { number: is.number, url: is.html_url, updatedAt: is.updated_at, title: t.title, body: t.body || '', state: t.status === 'done' ? 'closed' : 'open' });
        if (t.status === 'done') { const c = await api('PATCH', `/issues/${is.number}`, { state: 'closed' }); board.setExternal(t.id, { ...board.external(t.id), updatedAt: c.updated_at, state: 'closed' }); }
        return;
      }
      const patch = {};
      if (t.title !== ext.title) patch.title = t.title;
      if ((t.body || '') !== (ext.body || '')) patch.body = issueBody(t);
      const wantState = t.status === 'done' ? 'closed' : 'open';
      if (wantState !== ext.state) patch.state = wantState;
      if (!Object.keys(patch).length) return;
      const r = await api('PATCH', `/issues/${ext.number}`, patch);
      board.setExternal(t.id, { ...ext, updatedAt: r.updated_at, title: t.title, body: t.body || '', state: wantState });
    } catch (e) { log('[mills] forgejo: ' + e.message); } finally {
      busy.delete(t.id);
      if (again.has(t.id)) { again.delete(t.id); const cur = board.get(t.id); if (cur) outbound(cur.deleted ? 'deleted' : 'updated', cur); }
    }
  }

  // ---- Forgejo → Mills ----
  async function poll() {
    if (!on) return;
    try {
      const since = lastPoll, started = new Date().toISOString();
      const issues = await api('GET', `/issues?state=all&type=issues&since=${encodeURIComponent(since)}&limit=50`);
      for (const is of issues || []) {
        if (is.pull_request) continue;
        const t = board.byExternal(is.number);
        if (t) {
          const ext = t.forgejo || {};
          if (ext.updatedAt && Date.parse(is.updated_at) <= Date.parse(ext.updatedAt)) continue; // our own change, or nothing new
          const body = String(is.body || '').replace(/\n\n---\n_Mills task T-\d+[^\n]*_\s*$/, '');
          const patch = {};
          if (is.title && is.title !== t.title) patch.title = is.title;
          if (body !== (t.body || '')) patch.body = body;
          if (is.state === 'closed' && t.status !== 'done') patch.status = 'done';
          if (is.state === 'open' && t.status === 'done') patch.status = 'todo';
          board.setExternal(t.id, { ...ext, updatedAt: is.updated_at, title: is.title, body, state: is.state });
          if (Object.keys(patch).length) board.ops.update(ACTOR, { id: t.id, ...patch, skipMirror: true });
        } else if (is.state === 'open' && !/_Mills task T-\d+/.test(is.body || '')) { // an issue made on Forgejo: becomes a task
          const r = board.ops.create(ACTOR, { title: is.title, body: (is.body || '') + `\n\n---\n_From Forgejo issue [#${is.number}](${is.html_url})._`, board: BOARD, status: 'todo', labels: ['forgejo', ...(is.labels || []).map((l) => l.name)], skipMirror: true });
          if (r.status === 200) board.setExternal(r.body.task.id, { number: is.number, url: is.html_url, updatedAt: is.updated_at, title: is.title, body: is.body || '', state: 'open' });
        }
      }
      lastPoll = started;
      fs.writeFileSync(FILE, JSON.stringify({ ...cfg, since: lastPoll }, null, 2));
    } catch (e) { log('[mills] forgejo poll: ' + e.message); }
  }
  if (on) {
    board.onChange((what, t, opts) => { if (!(opts && opts.skipMirror)) outbound(what, t); });
    const every = Math.max(3000, (Number(cfg.pollMinutes) || 2) * 60000);
    setTimeout(poll, 2000).unref();
    setInterval(poll, every).unref();
    log(`[mills] Forgejo mirroring on: ${cfg.repo} ↔ board "${BOARD}", every ${Math.round(every / 1000)} s.`);
  }
  return { on, poll, outbound, status: () => ({ on, repo: on ? cfg.repo : '', board: BOARD, lastPoll }) };
};
