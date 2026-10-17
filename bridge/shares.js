// Mills shared Work sessions (watch-only). A person can share their Work session with everyone;
// members can watch it in a window: the Claude conversation as it happens, what the owner is typing
// (live), what they send and what is queued. Watching is read-only; everyone keeps working in their own
// session and worktree. Work runs happen on the hub anyway, so this adds no new reader.
// Routes: /api/work/share | live | watch (long poll). The hub calls mirror() / mirrorEvent() per run.
'use strict';

module.exports = function createShares({ accounts, convMember, roomChanged, send, readBody }) {
  const shares = new Map();        // owner -> { conv, since, n, log: [{ n, t, text, ts }], live: { typing, queue }, ver }
  const watchWaiters = new Map();  // owner -> Set(wake fn)
  const wakeWatchers = (owner) => { for (const fn of watchWaiters.get(owner) || []) fn(); };
  function mirror(acct, t, text) { // one line of the owner's Work conversation for the watchers
    const s = shares.get(acct.username);
    if (!s || !text) return;
    s.log.push({ n: ++s.n, t, text: String(text).slice(0, 20000), ts: new Date().toISOString() });
    if (s.log.length > 400) s.log.splice(0, s.log.length - 400);
    wakeWatchers(acct.username);
  }
  function mirrorEvent(acct, ev) { // a Claude stream event of the owner's run
    if (!shares.has(acct.username)) return;
    if (ev.type === 'assistant' && ev.message) {
      for (const b of ev.message.content || []) {
        if (b.type === 'text' && b.text && b.text.trim()) mirror(acct, 'claude', b.text);
        else if (b.type === 'tool_use') {
          const i = b.input || {}, target = i.path ?? i.file_path ?? i.command ?? i.pattern ?? i.url ?? i.query ?? '';
          // (no icon in the text: the page draws the gear itself)
          mirror(acct, 'tool', String(b.name).replace(/^mcp__pc__/, 'their PC › ') + (target !== '' ? '  ' + String(target).slice(0, 160) : ''));
        }
      }
    } else if (ev.type === 'result') mirror(acct, 'sys', ev.is_error ? 'Claude stopped with an error' : 'Done');
  }
  const canWatch = (owner, viewer) => { const s = shares.get(owner); return !!s && owner !== viewer && convMember(s.conv, viewer); };
  const listFor = (user) => [...shares].filter(([o, s]) => o !== user && convMember(s.conv, user)).map(([o, s]) => ({ user: o, conv: s.conv, since: s.since }));
  const convOf = (owner) => (shares.has(owner) ? shares.get(owner).conv : '');
  function stop(owner) { if (shares.delete(owner)) { wakeWatchers(owner); roomChanged(); } }

  async function route(p, req, res, acct, url) {
    if (!p.startsWith('/api/work/')) return false;
    if (p === '/api/work/share' && req.method === 'POST') {
      const { conv, transcript } = await readBody(req);
      if (!conv) { stop(acct.username); send(res, 200, { share: '' }); return true; }
      if (conv !== 'room') { send(res, 400, { error: 'A Work session can only be shared with everyone.' }); return true; } // no per-person shares
      const prev = shares.get(acct.username);
      const s = { conv, since: new Date().toISOString(), n: 0, log: [], live: prev ? prev.live : { typing: '', queue: [] }, ver: 0 };
      const kinds = { me: 'user', claude: 'claude', tool: 'tool', sys: 'sys' };
      for (const t of (Array.isArray(transcript) ? transcript : []).slice(-150)) {
        if (t && kinds[t.cls] && typeof t.text === 'string') s.log.push({ n: ++s.n, t: kinds[t.cls], text: t.text.slice(0, 20000), ts: s.since });
      }
      if (prev) wakeWatchers(acct.username); // watchers of the old share see it restart
      shares.set(acct.username, s);
      roomChanged();
      send(res, 200, { share: conv }); return true;
    }
    if (p === '/api/work/live' && req.method === 'POST') {
      const s = shares.get(acct.username);
      if (!s) { send(res, 200, { ok: false }); return true; }
      const { typing, queue } = await readBody(req);
      s.live = { typing: typeof typing === 'string' ? typing.slice(0, 4000) : '', queue: (Array.isArray(queue) ? queue : []).slice(0, 20).map((q) => String(q).slice(0, 2000)) };
      s.ver++; wakeWatchers(acct.username);
      send(res, 200, { ok: true }); return true;
    }
    if (p === '/api/work/watch' && req.method === 'GET') {
      // Long poll: new log entries after `after`, or a live typing / queue change since `ver`; waits up to 25 s.
      const owner = String(url.searchParams.get('user') || ''), after = Number(url.searchParams.get('after')) || 0;
      const ver = Number(url.searchParams.get('ver')), since = url.searchParams.get('since') || '';
      const answer = () => {
        const s = shares.get(owner), o = accounts.get(owner);
        if (!s || !canWatch(owner, acct.username)) return send(res, 200, { stopped: true });
        const restart = since !== s.since; // a new share: send everything again
        send(res, 200, { since: s.since, conv: s.conv, busy: !!(o && o.running), live: s.live, ver: s.ver,
          events: s.log.filter((e) => restart || e.n > after), after: s.n, restart });
      };
      const s = shares.get(owner);
      if (!s || !canWatch(owner, acct.username) || since !== s.since || s.log.some((e) => e.n > after) || ver !== s.ver) { answer(); return true; }
      let done = false;
      const set = watchWaiters.get(owner) || new Set(); watchWaiters.set(owner, set);
      const finish = () => { if (done) return; done = true; clearTimeout(timer); set.delete(finish); answer(); };
      const timer = setTimeout(finish, 25000);
      set.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); set.delete(finish); } });
      return true;
    }
    return false;
  }
  return { mirror, mirrorEvent, listFor, convOf, stop, route };
};
