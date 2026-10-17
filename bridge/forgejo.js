// Mills ↔ Forgejo: personal Forgejo tokens + issue mirroring (the mirroring idea is inherited from
// HyperProductivityManager / ProjectTaskManager: Forgejo has no plugin system, so this is an outside service
// speaking its REST API).
//
// Personal tokens ("My Forgejo" in Settings): every person can save their own Forgejo access token. The hub
// checks it against <url>/api/v1/user, keeps it in bridge/forgejo-users.json (gitignored, never sent back to a
// page), and uses it for two things:
//   - board mirroring acts on Forgejo AS that person (their issues, their closes, their comments);
//   - their own Work runs get it: FORGEJO_URL / FORGEJO_TOKEN in the environment, and git pushes / fetches to
//     the Forgejo host authenticate with it (an http extraHeader passed through GIT_CONFIG_COUNT, nothing written
//     to any git config). Nobody else's run sees it.
// The Forgejo address is forgejo.json's "url", else config.json's "forgejoUrl", else https://git.unityailab.com.
//
// Mirroring is off until the host writes bridge/forgejo.json (gitignored):
//   { "url": "https://git.unityailab.com", "repo": "UnityAILab/Mills", "board": "team", "pollMinutes": 2,
//     "token": "<optional hub token>" }
// For the tasks of that board:
//   Mills → Forgejo  a new task becomes an issue (title, body + "Mills task T-n"); title / body changes update it;
//                    completing closes it; moving a done task back reopens it; deleting closes it with a comment.
//                    Each call uses the token of the person who made the change, else the hub token, else it is
//                    skipped (logged).
//   Forgejo → Mills  needs the hub token (a background job belongs to nobody): every pollMinutes it reads issues
//                    changed since the last look: a closed issue completes its task, a reopened one puts it back in
//                    To do, title / body changes land on the task, and an issue nobody mirrored yet becomes a task
//                    (label "forgejo").
// Loops are avoided by remembering, per task, the issue's updated_at we last saw or produced; anything older is
// ignored. The poll acts on the board as "Forgejo".
'use strict';

const fs = require('fs');
const path = require('path');

const DEFAULT_URL = 'https://git.unityailab.com';

module.exports = function createForgejo({ ROOT, board, readJson, writeJson, config = {}, log = console.log }) {
  const FILE = path.join(ROOT, 'forgejo.json');
  const USERS = path.join(ROOT, 'forgejo-users.json');
  const cfg = readJson(FILE, null);
  const URL_ = String((cfg && cfg.url) || config.forgejoUrl || DEFAULT_URL).replace(/\/+$/, '');
  const on = !!(cfg && cfg.repo);
  const hubToken = (cfg && cfg.token) || '';
  const BOARD = (cfg && cfg.board) || 'team';
  const base = on ? URL_ + '/api/v1/repos/' + String(cfg.repo).replace(/^\/+|\/+$/g, '') : '';
  const ACTOR = { user: 'forgejo', agent: false, name: 'Forgejo' };
  const MARK = (id) => `\n\n---\n_Mills task ${id} — mirrored by Mills; edit it in either place._`;
  let lastPoll = cfg && cfg.since ? cfg.since : new Date(0).toISOString();
  const busy = new Set(), again = new Map(); // task ids with an outbound call in flight / changed meanwhile (→ actor)

  // ---- personal tokens ----
  let users = readJson(USERS, { users: {} });
  if (!users || typeof users.users !== 'object') users = { users: {} };
  const saveUsers = () => (writeJson ? writeJson(USERS, users) : fs.writeFileSync(USERS, JSON.stringify(users, null, 2)));
  const rec = (user) => (user && Object.prototype.hasOwnProperty.call(users.users, user) ? users.users[user] : null);
  const tokenFor = (user) => { const r = rec(user); return r ? r.token : ''; };
  function me(user) {
    const r = rec(user);
    return { url: URL_, connected: !!r, login: r ? r.login : '', savedAt: r ? r.savedAt : '', mirror: on ? { repo: cfg.repo, board: BOARD, poll: !!hubToken } : null };
  }
  async function whoIs(token) {
    const r = await fetch(URL_ + '/api/v1/user', { headers: { Authorization: 'token ' + token, Accept: 'application/json' }, signal: AbortSignal.timeout(15000) });
    if (r.status === 401 || r.status === 403) throw new Error('Forgejo did not accept that token.');
    if (!r.ok) throw new Error('Forgejo answered ' + r.status + '.');
    const j = await r.json();
    if (!j || !j.login) throw new Error('Forgejo did not say whose token that is.');
    return j.login;
  }
  async function connect(user, token) {
    token = String(token || '').trim();
    if (!/^[\w.-]{8,200}$/.test(token)) throw new Error('Paste a Forgejo access token (Settings → Applications on ' + URL_ + ').');
    const login = await whoIs(token);
    users.users[user] = { token, login, savedAt: new Date().toISOString() };
    saveUsers();
    return me(user);
  }
  function disconnect(user) { if (rec(user)) { delete users.users[user]; saveUsers(); } return me(user); }
  // Environment for this person's own Work run: nothing when they have no token.
  function workEnv(user) {
    const r = rec(user);
    if (!r) return {};
    const n = Number(process.env.GIT_CONFIG_COUNT) || 0;
    return {
      FORGEJO_URL: URL_, FORGEJO_TOKEN: r.token, FORGEJO_LOGIN: r.login,
      GIT_CONFIG_COUNT: String(n + 1),
      ['GIT_CONFIG_KEY_' + n]: 'http.' + URL_ + '/.extraHeader',
      ['GIT_CONFIG_VALUE_' + n]: 'Authorization: Basic ' + Buffer.from(r.login + ':' + r.token).toString('base64'),
    };
  }

  // ---- mirroring ----
  async function api(method, p, body, token) {
    const r = await fetch(base + p, { method, headers: { Authorization: 'token ' + token, 'Content-Type': 'application/json', Accept: 'application/json' }, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(15000) });
    if (!r.ok) throw new Error(`Forgejo ${method} ${p} → ${r.status} ${(await r.text()).slice(0, 200)}`);
    return r.status === 204 ? null : r.json();
  }
  const issueBody = (t) => (t.body || '').replace(/\n\n---\n_Mills task T-\d+[^\n]*_\s*$/, '') + MARK(t.id);

  // ---- Mills → Forgejo ----
  async function outbound(what, t, actor) {
    if (!on || t.board !== BOARD) return;
    if (busy.has(t.id)) { again.set(t.id, actor || null); return; } // the call in flight re-syncs the task when it is done
    const token = (actor && tokenFor(actor.user)) || hubToken;
    if (!token) { log(`[mills] forgejo: ${t.id} not mirrored: ${actor ? actor.user : 'nobody'} has no Forgejo token and the hub has none.`); return; }
    busy.add(t.id);
    try {
      const ext = t.forgejo || null;
      if (what === 'deleted') { if (ext) { await api('POST', `/issues/${ext.number}/comments`, { body: `Deleted on the Mills board (${t.id}).` }, token); await api('PATCH', `/issues/${ext.number}`, { state: 'closed' }, token); } return; }
      if (!ext) {
        if (what === 'completed' && t.status === 'done') return; // nothing to mirror for a finished task that never had an issue
        const is = await api('POST', '/issues', { title: t.title, body: issueBody(t), labels: [] }, token);
        board.setExternal(t.id, { number: is.number, url: is.html_url, updatedAt: is.updated_at, title: t.title, body: t.body || '', state: t.status === 'done' ? 'closed' : 'open' });
        if (t.status === 'done') { const c = await api('PATCH', `/issues/${is.number}`, { state: 'closed' }, token); board.setExternal(t.id, { ...board.external(t.id), updatedAt: c.updated_at, state: 'closed' }); }
        return;
      }
      const patch = {};
      if (t.title !== ext.title) patch.title = t.title;
      if ((t.body || '') !== (ext.body || '')) patch.body = issueBody(t);
      const wantState = t.status === 'done' ? 'closed' : 'open';
      if (wantState !== ext.state) patch.state = wantState;
      if (!Object.keys(patch).length) return;
      const r = await api('PATCH', `/issues/${ext.number}`, patch, token);
      board.setExternal(t.id, { ...ext, updatedAt: r.updated_at, title: t.title, body: t.body || '', state: wantState });
    } catch (e) { log('[mills] forgejo: ' + e.message); } finally {
      busy.delete(t.id);
      if (again.has(t.id)) { const a = again.get(t.id); again.delete(t.id); const cur = board.get(t.id); if (cur) outbound(cur.deleted ? 'deleted' : 'updated', cur, a); }
    }
  }

  // ---- Forgejo → Mills ----
  async function poll() {
    if (!on || !hubToken) return;
    try {
      const since = lastPoll, started = new Date().toISOString();
      const issues = await api('GET', `/issues?state=all&type=issues&since=${encodeURIComponent(since)}&limit=50`, null, hubToken);
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
    board.onChange((what, t, opts) => { if (!(opts && opts.skipMirror)) outbound(what, t, opts && opts.actor); });
    const every = Math.max(3000, (Number(cfg.pollMinutes) || 2) * 60000);
    if (hubToken) { setTimeout(poll, 2000).unref(); setInterval(poll, every).unref(); }
    log(`[mills] Forgejo mirroring on: ${cfg.repo} ↔ board "${BOARD}"` + (hubToken ? `, polled every ${Math.round(every / 1000)} s.` : ', outbound only (no hub token: personal tokens, no polling).'));
  }
  return { on, poll, outbound, me, connect, disconnect, tokenFor, workEnv, url: URL_, USERS,
    status: () => ({ on, repo: on ? cfg.repo : '', board: BOARD, lastPoll, poll: on && !!hubToken }) };
};
