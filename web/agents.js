// VibeRoller Work: the agent threads (T-298 = T-294 · 4/6; team note plans/agent-talk.md). People stay in charge of what
// their agent says and takes: a hidable sidebar next to the Work conversation (the "Agents" button, the menu tile) with every
// message to and from the person's agent, a thread per agent pair (both directions, who / when / kind, the session it went
// into one click away), the person's controls (mute a pair, pause my inbox, "ask me first", wake me) and an accept / decline
// card for a held message; stop / reopen a thread from its head; and writing to another person's agent from the page (the
// composer: to whom, the kind, a task ref, the text; a reply inside a thread), sent as the person (from.source "person"),
// with the hub's refusals in words. A new message notifies the way the page does, following Do Not Disturb (it mutes the
// notification only). It only reads and asks the hub's /api/agents/* (the Rust hub's, T-296): threads, thread, settings,
// held, stop, reopen, list, send, reply. A hub without them (an older hub, the JavaScript fallback hub) answers 404: then
// the panel stays empty and hidden, a control whose route is missing says so, and nothing throws.
// Loaded before the main page script. The pure helpers at the top (no DOM) and mount() with a plain element API are what
// the suite runs in node (R532, R533, R532b).
(function () {
  'use strict';
  const W = typeof window !== 'undefined' ? window : globalThis;

  // ── pure helpers ──
  const KINDS = { ask: 'Question', handoff: 'Hand-off', review: 'Review', report: 'Report', reply: 'Reply' };
  const kindLabel = (k) => KINDS[k] || String(k || 'Message');
  // what "ask me first" may hold (the hub's HOLDABLE), in the order the panel shows them
  const ASK_FIRST = [['handoff', 'hand-offs'], ['ask', 'questions'], ['review', 'reviews']];
  const SOURCES = { jcode: 'Jcode PGM', hpm: 'Hyper Prod Man', workflow: 'a workflow' };
  const userOf = (side) => String((side && side.user) || '');
  const isMine = (m, me) => userOf(m && m.from) === me;
  // the session a message went into on this person's side: theirs it came from, or theirs it was delivered into
  const sessionOf = (m, me) => String(((isMine(m, me) ? m.from : m.to) || {}).session || '');
  const sourceOf = (m, me) => String(((isMine(m, me) ? m.from : m.to) || {}).source || '');
  // who wrote it, as this person reads it
  function whoText(m, me, nameOf) {
    const f = (m && m.from) || {}, person = f.source === 'person', src = SOURCES[f.source] ? ' (' + SOURCES[f.source] + ')' : '';
    if (isMine(m, me)) return (person ? 'You' : 'Your agent') + src;
    const n = nameOf(userOf(f)) || userOf(f);
    return (person ? n : n + "'s agent") + src;
  }
  // a message's state in words (mine = I sent it)
  function statusText(m, me, paused) {
    const mine = isMine(m, me), st = m && m.status, why = (m && m.why) || '';
    if (st === 'held') return mine ? 'waits for their okay' : 'waits for your okay';
    if (st === 'queued') return !mine && paused ? 'waiting: your inbox is paused' : 'waiting to be delivered';
    if (st === 'refused') return { muted: mine ? 'not taken: they muted your agent' : 'muted: not delivered', declined: 'declined', 'no-session': 'not delivered: the session is gone', hops: 'refused: the thread is at its limit', rate: 'refused: too many messages' }[why] || 'refused';
    if (st === 'stopped') return { loop: 'stopped: the agents went in circles', full: 'stopped: the thread is full' }[why] || 'stopped';
    if (st === 'expired') return 'no answer in time';
    return { delivered: 'delivered', read: 'read', answered: 'answered' }[st] || String(st || '');
  }
  // threads (each { id, with, subject, status, why, lastAt, session, messages }) → one group per agent pair, newest first
  function pairs(threads) {
    const by = new Map();
    for (const t of threads || []) {
      if (!t || !t.with) continue;
      const p = by.get(t.with) || { with: t.with, threads: [], lastAt: 0, count: 0 };
      p.threads.push(t); p.lastAt = Math.max(p.lastAt, Number(t.lastAt) || 0); p.count += (t.messages || []).length;
      by.set(t.with, p);
    }
    const out = [...by.values()];
    for (const p of out) p.threads.sort((a, b) => (Number(b.lastAt) || 0) - (Number(a.lastAt) || 0));
    return out.sort((a, b) => b.lastAt - a.lastAt || (a.with < b.with ? -1 : 1));
  }
  const sorted = (msgs) => [...(msgs || [])].sort((a, b) => (Number(a.at) || 0) - (Number(b.at) || 0));
  // the held messages that wait for this person's okay
  const heldFor = (threads, me) => (threads || []).flatMap((t) => (t.messages || []).filter((m) => m.status === 'held' && userOf(m.to) === me).map((m) => ({ ...m, with: t.with, subject: t.subject })));
  // messages from the other side newer than what this person saw of the pair
  const unread = (msgs, me, seenAt) => (msgs || []).filter((m) => !isMine(m, me) && (Number(m.at) || 0) > (Number(seenAt) || 0) && m.why !== 'muted').length;
  // should a new message tell the person (a sound, a toast, a desktop notification)? Not their own, not from a pair they
  // muted, not one that was refused or stopped, and never during Do Not Disturb (it only mutes: the message is there)
  function shouldNotify(m, me, settings, dnd) {
    if (!m || isMine(m, me) || dnd) return false;
    if (((settings && settings.muted) || []).includes(userOf(m.from))) return false;
    return ['queued', 'delivered', 'read', 'answered', 'held'].includes(m.status);
  }
  // the words of the notification
  function notifyText(m, me, nameOf) {
    const who = whoText(m, me, nameOf), task = (m.refs && m.refs.task) || '', first = String(m.text || '').split('\n').map((l) => l.trim()).find(Boolean) || '';
    if (m.status === 'held') return { title: who + (m.kind === 'handoff' ? ' hands you ' + (task || 'a task') : ' asks first: ' + kindLabel(m.kind).toLowerCase()), body: 'Waits for your okay: ' + first.slice(0, 160), held: true };
    return { title: who + ' · ' + kindLabel(m.kind), body: first.slice(0, 160), held: false };
  }
  // the held card's words
  function cardText(m, me, nameOf) {
    const who = whoText(m, me, nameOf), task = (m.refs && m.refs.task) || '';
    if (m.kind === 'handoff') return { title: who + ' hands you ' + (task || 'a task'), hint: 'Accept: ' + (task ? task + ' becomes yours and ' : '') + 'your agent takes it on. Decline: it stays theirs, and they are told (with your note).' };
    return { title: who + ' sends a ' + kindLabel(m.kind).toLowerCase() + ' (you asked to be asked first)', hint: 'Accept: your agent gets it. Decline: it is not delivered, and they are told (with your note).' };
  }
  // "14:02" today, "3 Nov 14:02" before
  function when(at, now) {
    const d = new Date(Number(at) || 0), n = new Date(Number(now) || Date.now()), p = (x) => String(x).padStart(2, '0');
    const hm = p(d.getHours()) + ':' + p(d.getMinutes());
    return d.toDateString() === n.toDateString() ? hm : d.getDate() + ' ' + ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][d.getMonth()] + ' ' + hm;
  }
  // a list with `x` in it or not
  const toggled = (list, x, on) => { const l = (list || []).filter((y) => y !== x); return on ? [...l, x] : l; };
  // a thread's stopped state in words ("" = open)
  const stoppedText = (t) => (t && t.status === 'stopped' ? { loop: 'stopped: going in circles', hops: 'stopped: at its limit', full: 'stopped: full', 'stopped-by-person': 'stopped by a person' }[t.why] || 'stopped' : '');
  // what a person may write from the page (the hub's envelope kinds; a reply only inside a thread)
  const COMPOSE = [['ask', 'Question'], ['handoff', 'Hand-off of a board task'], ['review', 'Review'], ['report', 'Report']];
  const REPLY = [['reply', 'Reply'], ['report', 'Report']];
  // a draft that cannot go yet, in words ("" = it can)
  function draftError(d) {
    if (!d || !String(d.to || '').trim()) return 'Pick the person whose agent it goes to.';
    if (!String(d.text || '').trim()) return 'Write the message first.';
    if (d.kind === 'handoff' && !String(d.task || '').trim()) return 'A hand-off names the board task it hands over (e.g. T-12).';
    return '';
  }
  // a refused send / reply / stop ({status, data: {why, error}}) in plain words; name = the other person
  function refusalText(r, name) {
    const st = (r && r.status) || 0, d = (r && r.data) || {}, err = String(d.error || '').trim(), who = name || 'they';
    if (!st) return 'The hub did not answer' + (err ? ' (' + err + ')' : '') + ': try again in a moment.';
    if (st === 404 && (!err || /unknown endpoint/i.test(err))) return 'This hub does not support that yet (an older hub).';
    switch (d.why) {
      case 'muted': return 'Not sent: ' + who + '\'s agent is not taking messages from you (they muted you).';
      case 'rate': return 'Not sent: too many messages in an hour' + (err ? ' (' + err + ')' : '') + '. Try again later.';
      case 'hops': return 'Not sent: the thread reached its limit of messages and is stopped now. Reopen it to go on.';
      case 'loop': return 'Not sent: the agents were going in circles in this thread, so it is stopped. Reopen it to go on.';
      case 'full': return 'Not sent: the thread is full. Start a new one.';
      case 'stopped': return 'Not sent: this thread is stopped. Reopen it to write in it.';
    }
    return 'Not sent: ' + (err || 'the hub refused it (' + st + ').');
  }
  // a send the hub took ({status, note}), in words
  function sentText(d, name) {
    const who = name || 'their';
    if (d && d.status === 'held') return 'Sent: it waits for ' + who + '\'s okay (they asked to be asked first).';
    if (d && /paused/.test(d.note || '')) return 'Sent: it waits in ' + who + '\'s inbox, which they paused.';
    return 'Sent to ' + who + '\'s agent.';
  }
  // one person of the team list (GET /api/agents/list) as the composer shows them
  function teamLabel(p) {
    const bits = [];
    if (p.mutedYou) bits.push('muted you'); else if (p.paused) bits.push('inbox paused');
    if ((p.askFirst || []).length && !p.mutedYou) bits.push('asks first: ' + p.askFirst.map((k) => (ASK_FIRST.find((x) => x[0] === k) || [k, k])[1]).join(', '));
    return (p.name || p.user) + (bits.length ? ' · ' + bits.join(' · ') : '');
  }
  function summary(ps, held, paused) {
    const n = ps.reduce((k, p) => k + p.threads.length, 0);
    const bits = [n ? n + (n === 1 ? ' thread' : ' threads') + ' with ' + ps.length + (ps.length === 1 ? ' person' : ' people') : 'no threads yet'];
    if (held) bits.push(held + (held === 1 ? ' waits' : ' wait') + ' for you');
    if (paused) bits.push('inbox paused');
    return bits.join(' · ');
  }

  // ── the panel ──
  // o: { box, button, badge, tile, tileBadge, document, api(path, body?): Promise<{status, data}>, me(): username, nameOf(user),
  //      dnd(): bool, hasSession(id), titleOf(id), open(id), notify({title, body, held, with}), note(text, bad),
  //      store: { get, set }, interval: ms between refreshes (0 = only when asked), now?() }
  const MAX_FETCH = 40; // thread details fetched per refresh at most (the newest first)
  function mount(o) {
    const doc = o.document || W.document, now = () => (o.now ? o.now() : Date.now());
    const el = (tag, cls, text) => { const e = doc.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; };
    const btn = (label, act, title) => { const b = el('button', 'ghost', label); b.type = 'button'; b.dataset.act = act; if (title) b.title = title; return b; };
    const opt = (value, label) => { const x = el('option', '', label); x.value = value; return x; };
    const A = { open: false, ok: null, settings: null, threads: new Map(), known: new Set(), first: true, pair: '', busy: false, timer: null, gen: 0, err: {}, notes: {}, expanded: new Set(),
      // the composer (team: the people to write to, null = this hub has no list) and the reply form of one thread
      compose: false, team: undefined, draft: { to: '', kind: 'ask', task: '', text: '' }, cmsg: '', cbad: false, sending: false, replying: '', rdraft: {}, rmsg: {} };
    const seenOf = () => { try { return JSON.parse((o.store && o.store.get('vr.agentsSeen')) || 'null'); } catch { return null; } };
    let seen = seenOf();
    const saveSeen = () => { if (o.store) o.store.set('vr.agentsSeen', JSON.stringify(seen || {})); };
    const call = async (p, body) => { try { const r = await o.api(p, body); return r && typeof r.status === 'number' ? r : { status: 0, data: {} }; } catch (e) { return { status: 0, data: { error: (e && e.message) || 'offline' } }; } };
    const list = () => [...A.threads.values()];
    const mutedSet = () => new Set((A.settings && A.settings.muted) || []);
    function markSeen(user) {
      const p = pairs(list()).find((x) => x.with === user); if (!p) return;
      seen = seen || {}; seen[user] = Math.max(Number(seen[user]) || 0, ...p.threads.flatMap((t) => (t.messages || []).map((m) => Number(m.at) || 0)));
      saveSeen();
    }
    function hideAll() {
      for (const x of [o.button, o.tile, o.badge, o.tileBadge]) if (x) x.hidden = true;
      o.box.hidden = true; o.box.textContent = '';
    }
    // one round: the settings (and whether the hub has agents at all), the thread list, the details of what changed
    async function refresh() {
      if (A.busy) return A.ok; A.busy = true;
      const gen = A.gen;
      try {
        const s = await call('/api/agents/settings');
        if (gen !== A.gen) return A.ok;
        if (s.status !== 200 || !s.data || !Array.isArray(s.data.muted)) { if (s.status !== 0 || A.ok === null) { A.ok = false; hideAll(); } return A.ok; }
        A.ok = true; A.settings = s.data;
        const l = await call('/api/agents/threads');
        if (gen !== A.gen || l.status !== 200) return A.ok;
        const sums = ((l.data && l.data.threads) || []).filter((t) => t && t.id);
        const ids = new Set(sums.map((t) => t.id));
        for (const id of [...A.threads.keys()]) if (!ids.has(id)) A.threads.delete(id);
        let fetched = 0;
        for (const sm of sums) { // newest first (the hub's order)
          const had = A.threads.get(sm.id);
          const stale = !had || had.lastAt !== sm.lastAt || (had.messages || []).length !== sm.count || had.status !== sm.status
            || (had.messages || []).some((m) => m.status === 'held') || (A.open && A.pair === sm.with);
          if (!stale) continue;
          if (fetched >= MAX_FETCH) { if (!had) A.threads.set(sm.id, { ...sm, messages: [], partial: true }); continue; }
          fetched++;
          const d = await call('/api/agents/thread?id=' + encodeURIComponent(sm.id));
          if (gen !== A.gen) return A.ok;
          if (d.status !== 200 || !d.data) continue;
          const meta = d.data.thread || {};
          A.threads.set(sm.id, { id: sm.id, with: meta.with || sm.with, subject: meta.subject || sm.subject || '', status: meta.status || sm.status, why: meta.why || '', lastAt: sm.lastAt, session: meta.session || '', messages: sorted(d.data.messages) });
        }
        // what is new since the last round tells the person (never what was there when the page opened)
        const me = o.me(), fresh = [];
        for (const t of list()) for (const m of t.messages || []) if (m.id && !A.known.has(m.id)) { A.known.add(m.id); if (!A.first) fresh.push(m); }
        if (seen === null) { seen = {}; for (const p of pairs(list())) seen[p.with] = Math.max(0, ...p.threads.flatMap((t) => (t.messages || []).map((m) => Number(m.at) || 0))); saveSeen(); } // the first time ever: nothing old counts as new
        if (A.first) A.first = false;
        else {
          const dnd = !!(o.dnd && o.dnd());
          for (const m of fresh.slice(-3)) if (shouldNotify(m, me, A.settings, dnd)) { try { o.notify({ ...notifyText(m, me, o.nameOf), with: userOf(m.from), id: m.id }); } catch {} }
        }
        if (A.open && A.pair) markSeen(A.pair);
        render();
        return A.ok;
      } finally { A.busy = false; }
    }
    async function setSettings(change, what) {
      const r = await call('/api/agents/settings', change);
      if (r.status === 200 && r.data && Array.isArray(r.data.muted)) { A.settings = r.data; render(true); return true; }
      o.note('Could not ' + what + ': ' + ((r.data && r.data.error) || 'the hub did not answer') + '.', true); render(true); return false;
    }
    async function answer(m, accept) {
      const note = String(A.notes[m.id] || '').trim();
      A.err[m.id] = ''; render(true);
      const r = await call('/api/agents/held', accept ? { id: m.id, accept: true } : { id: m.id, accept: false, note });
      if (r.status === 200) {
        delete A.notes[m.id];
        const d = r.data || {};
        if (accept) o.note(m.kind === 'handoff' ? 'Accepted: ' + ((m.refs && m.refs.task) || 'the task') + (d.reassigned ? ' is yours now, ' : ' ') + 'and your agent takes it on.' + (d.note ? ' ' + d.note : '') : 'Accepted: your agent gets it.');
        else o.note('Declined: ' + (o.nameOf(userOf(m.from)) || userOf(m.from)) + ' is told' + (note ? ', with your note.' : '.'));
      } else if (r.status === 409) o.note('That message no longer waits for your okay.');
      else { A.err[m.id] = 'Could not send your answer: ' + ((r.data && r.data.error) || 'the hub did not answer') + '.'; }
      const t = A.threads.get(m.thread); if (t) t.lastAt = -1; // fetched again now
      await refresh(); render(true);
    }
    // stop / reopen a thread (either person may; reopening resets its hop count)
    async function setThread(t, stop) {
      const name = o.nameOf(t.with) || t.with;
      const r = await call('/api/agents/' + (stop ? 'stop' : 'reopen'), { thread: t.id });
      if (r.status === 200) {
        t.status = stop ? 'stopped' : 'open'; t.why = stop ? 'stopped-by-person' : ''; t.lastAt = -1;
        if (stop && A.replying === t.id) A.replying = '';
        o.note(stop ? 'Stopped: neither agent can write in this thread until you or ' + name + ' reopen it.' : 'Reopened: the agents can write in this thread again.');
      } else o.note('Could not ' + (stop ? 'stop' : 'reopen') + ' the thread. ' + refusalText(r, name), true);
      await refresh(); render(true);
    }
    // the people the composer offers (everyone but me, as the hub lists them)
    async function loadTeam() {
      const r = await call('/api/agents/list'), me = o.me();
      if (r.status === 200 && r.data && Array.isArray(r.data.agents)) A.team = r.data.agents.filter((p) => p && p.user && p.user !== me);
      else if (r.status === 404) A.team = null;
      else if (A.team === undefined) A.team = [];
    }
    async function openCompose(v) {
      A.compose = !!v; A.cmsg = ''; A.cbad = false;
      if (A.compose) { render(true); await loadTeam(); }
      render(true);
    }
    // a new thread from the composer: POST /api/agents/send as the person (from.source "person")
    async function sendNew(d) {
      A.draft = d;
      const bad = draftError(d);
      if (bad) { A.cmsg = bad; A.cbad = true; render(true); return; }
      const name = o.nameOf(d.to) || d.to, body = { to: d.to, kind: d.kind, text: d.text };
      if (String(d.task || '').trim()) body.refs = { task: d.task.trim() };
      A.sending = true; A.cmsg = 'Sending…'; A.cbad = false; render(true);
      const r = await call('/api/agents/send', body);
      A.sending = false;
      if (r.status === 202 || r.status === 200) { A.cmsg = sentText(r.data, name); A.draft = { ...d, task: '', text: '' }; A.pair = d.to; }
      else { A.cmsg = refusalText(r, name); A.cbad = true; }
      await loadTeam(); await refresh(); render(true);
    }
    // an answer inside a thread: POST /api/agents/reply (a reply or a report)
    async function sendReply(t, kind, text) {
      A.rdraft[t.id] = { kind, text };
      if (!String(text || '').trim()) { A.rmsg[t.id] = 'Write the reply first.'; render(true); return; }
      const name = o.nameOf(t.with) || t.with;
      A.rmsg[t.id] = 'Sending…'; render(true);
      const r = await call('/api/agents/reply', { thread: t.id, kind, text });
      if (r.status === 202 || r.status === 200) { delete A.rdraft[t.id]; A.rmsg[t.id] = ''; A.replying = ''; o.note(sentText(r.data, name)); t.lastAt = -1; }
      else A.rmsg[t.id] = refusalText(r, name);
      await refresh(); render(true);
    }
    let drawn = '';
    function render(force) {
      if (!A.ok) { if (A.ok === false) hideAll(); return; }
      const me = o.me(), ts = list(), ps = pairs(ts), held = heldFor(ts, me), muted = mutedSet(), set = A.settings || {};
      const fresh = ps.filter((p) => !muted.has(p.with)).reduce((k, p) => k + unread(p.threads.flatMap((t) => t.messages || []), me, (seen || {})[p.with]), 0);
      const count = held.length + fresh;
      if (o.button) { o.button.hidden = false; o.button.title = 'Agent threads: ' + summary(ps, held.length, set.paused); }
      if (o.tile) o.tile.hidden = false;
      for (const b of [o.badge, o.tileBadge]) if (b) { b.textContent = count ? String(count) : ''; b.hidden = !count; }
      if (!A.open) { drawn = ''; return; }
      const sig = JSON.stringify([ts.map((t) => [t.id, t.lastAt, t.status, (t.messages || []).map((m) => [m.id, m.status, m.why, (m.to || {}).session])]), set, A.pair, [...A.expanded], A.err, [A.compose, A.team, A.cmsg, A.cbad, A.sending, A.replying, A.rmsg], (o.hasSession ? ts.map((t) => (t.messages || []).map((m) => !!o.hasSession(sessionOf(m, me)))) : 0)]);
      if (!force && sig === drawn) return;
      drawn = sig;
      const box = o.box;
      // a field being typed in keeps its focus over the redraw
      const fa = doc.activeElement, focus = fa && fa.dataset && fa.dataset.act && typeof box.contains === 'function' && box.contains(fa) ? { act: fa.dataset.act, thread: fa.dataset.thread || '', a: fa.selectionStart, b: fa.selectionEnd } : null;
      box.textContent = '';
      // the head: what there is, close
      const head = el('div', 'agHead');
      const close = btn('', 'close', 'Hide the agent threads'); close.className = 'ghost agX'; close.setAttribute('aria-label', 'Hide the agent threads');
      if (typeof W.ic === 'function') close.append(W.ic('close')); else close.textContent = '×';
      close.onclick = () => setOpen(false);
      head.append(el('b', '', 'Agents'), el('span', 'agSum', summary(ps, held.length, set.paused)), close);
      box.append(head);
      // the person's controls
      const ctl = el('div', 'agCtl');
      const pz = el('label', 'agTog'), pc = el('input'); pc.type = 'checkbox'; pc.checked = !!set.paused; pc.dataset.act = 'pause';
      pc.onchange = () => setSettings({ paused: !!pc.checked }, pc.checked ? 'pause your inbox' : 'resume your inbox');
      pz.title = 'Messages to your agent wait until you resume (answers to your own agent\'s questions still come)';
      pz.append(pc, el('span', '', 'Pause my agent\'s inbox'));
      const af = el('div', 'agAsk'); af.append(el('span', 'agAskL', 'Ask me first before my agent takes'));
      for (const [k, label] of ASK_FIRST) {
        const lb = el('label', 'agTog'), cb = el('input'); cb.type = 'checkbox'; cb.dataset.act = 'askFirst'; cb.dataset.kind = k;
        cb.checked = ((set.askFirst || []).includes(k));
        cb.onchange = () => setSettings({ askFirst: toggled(set.askFirst, k, !!cb.checked) }, 'change "ask me first"');
        lb.append(cb, el('span', '', label)); af.append(lb);
      }
      ctl.append(pz);
      // wake me (a hub whose settings have no "wake" shows none)
      if (typeof set.wake === 'boolean') {
        const wk = el('label', 'agTog'), wc = el('input'); wc.type = 'checkbox'; wc.checked = set.wake; wc.dataset.act = 'wake';
        wc.onchange = () => setSettings({ wake: !!wc.checked }, wc.checked ? 'turn "wake me" on' : 'turn "wake me" off');
        wk.title = 'On: a message to your agent starts a run in your idle session at once. Off: it waits in that session and goes in after your next run there ends (nothing starts on its own).';
        wk.append(wc, el('span', '', 'Wake me: a message may start my idle session'));
        ctl.append(wk);
      }
      ctl.append(af);
      if (set.paused) ctl.append(el('div', 'agNote', 'Your inbox is paused: messages wait here until you resume.'));
      if (set.wake === false && !set.paused) ctl.append(el('div', 'agNote', '"Wake me" is off: messages wait until your next run ends.'));
      const wr = btn('Write to an agent…', 'compose', A.compose ? 'Close the composer' : 'Write to another person\'s agent: a question, a hand-off of a board task, a review or a report');
      wr.className = 'ghost agWrite'; wr.setAttribute('aria-expanded', String(A.compose));
      wr.onclick = () => openCompose(!A.compose);
      ctl.append(wr);
      box.append(ctl);
      if (A.compose) box.append(composer());
      // what waits for the person's okay, first
      if (held.length) {
        box.append(el('div', 'agSub', 'Waiting for your okay'));
        for (const m of held) box.append(card(m, me));
      }
      // the pairs
      if (!ps.length) box.append(el('div', 'agNote', 'No agent threads yet. When another person\'s agent writes to yours, or yours to theirs, the thread shows here: both directions, and the session it went into.'));
      for (const p of ps) {
        const isOpen = A.pair === p.with, isMuted = muted.has(p.with);
        const sec = el('div', 'agPair' + (isOpen ? ' open' : '') + (isMuted ? ' muted' : ''));
        const row = el('div', 'agPRow');
        const name = o.nameOf(p.with) || p.with, n = unread(p.threads.flatMap((t) => t.messages || []), me, (seen || {})[p.with]);
        const ph = btn('', 'pair', isOpen ? 'Fold this pair' : 'Show the threads between your agent and ' + name + '\'s'); ph.className = 'ghost agPH'; ph.dataset.user = p.with;
        ph.setAttribute('aria-expanded', String(isOpen));
        ph.append(el('span', 'agPN', 'You ⇄ ' + name), el('span', 'agPM', p.threads.length + (p.threads.length === 1 ? ' thread' : ' threads') + (isMuted ? ' · muted' : '') + (n ? ' · ' + n + ' new' : '')));
        ph.onclick = () => { A.pair = isOpen ? '' : p.with; if (A.pair) markSeen(A.pair); render(true); };
        const mu = btn(isMuted ? 'Unmute' : 'Mute', 'mute', isMuted ? 'Let ' + name + '\'s agent write to yours again' : 'Mute ' + name + '\'s agent: its messages are refused and nothing notifies (this thread stays)'); mu.dataset.user = p.with;
        mu.onclick = () => setSettings({ muted: toggled(set.muted, p.with, !isMuted) }, isMuted ? 'unmute' : 'mute');
        row.append(ph, mu); sec.append(row);
        if (isOpen) for (const t of p.threads) sec.append(thread(t, me, set));
        box.append(sec);
      }
      if (focus && typeof box.querySelector === 'function') {
        const f = box.querySelector('[data-act="' + focus.act + '"]' + (focus.thread ? '[data-thread="' + focus.thread + '"]' : ''));
        if (f) { f.focus(); try { if (typeof focus.a === 'number') f.setSelectionRange(focus.a, focus.b); } catch {} }
      }
    }
    // the composer: to whom (the team), the kind, a task ref, the text
    function composer() {
      const c = el('div', 'agCompose');
      c.append(el('div', 'agCTitle', 'Write to another person\'s agent'));
      if (A.team === null) { c.append(el('div', 'agCMsg bad', 'This hub does not support writing to an agent yet (an older hub).')); return c; }
      const team = A.team || [], d = A.draft;
      if (!team.some((p) => p.user === d.to)) d.to = (team.find((p) => p.user === A.pair) || team.find((p) => !p.mutedYou) || team[0] || {}).user || '';
      const field = (label, x, cls) => { const l = el('label', 'agCF' + (cls ? ' ' + cls : '')); l.append(el('span', '', label), x); return l; };
      const to = el('select'); to.dataset.act = 'to';
      if (!team.length) to.append(opt('', A.team === undefined ? 'loading the team…' : 'nobody else on the team'));
      for (const p of team) to.append(opt(p.user, teamLabel(p)));
      to.value = d.to;
      const kind = el('select'); kind.dataset.act = 'kind';
      for (const [k, label] of COMPOSE) kind.append(opt(k, label));
      kind.value = d.kind;
      const task = el('input'); task.type = 'text'; task.dataset.act = 'task'; task.value = d.task; task.maxLength = 300;
      const text = el('textarea'); text.dataset.act = 'text'; text.rows = 3; text.value = d.text;
      const read = () => ({ to: to.value, kind: kind.value, task: task.value, text: text.value });
      const hints = () => {
        task.placeholder = kind.value === 'handoff' ? 'T-12 (a hand-off needs its task)' : 'T-12 (optional)';
        text.placeholder = 'What ' + ((o.nameOf(to.value) || to.value || 'their') + '\'s agent') + ' should know or do. It goes in as written by you.';
      };
      const keep = () => { A.draft = read(); hints(); };
      to.onchange = to.oninput = kind.onchange = kind.oninput = task.oninput = text.oninput = keep;
      hints();
      const row = el('div', 'agCRow'); row.append(field('Kind', kind), field('Task', task, 'grow'));
      const foot = el('div', 'agCFoot'), send = el('button', 'primary', A.sending ? 'Sending…' : 'Send'), close = btn('Close', 'composeClose', 'Close the composer (the draft stays)');
      send.type = 'button'; send.dataset.act = 'send'; send.disabled = A.sending || !team.length;
      send.onclick = () => sendNew(read());
      close.onclick = () => openCompose(false);
      foot.append(el('span', 'agCMsg' + (A.cbad ? ' bad' : ''), A.cmsg), close, send);
      c.append(field('To', to), row, text, foot);
      return c;
    }
    // the reply form inside a thread
    function replyForm(t) {
      const f = el('div', 'agReply'), d = A.rdraft[t.id] || { kind: 'reply', text: '' };
      const kind = el('select'); kind.dataset.act = 'replyKind'; kind.dataset.thread = t.id;
      for (const [k, label] of REPLY) kind.append(opt(k, label));
      kind.value = d.kind;
      const text = el('textarea'); text.dataset.act = 'replyText'; text.dataset.thread = t.id; text.rows = 2; text.value = d.text;
      text.placeholder = 'Your answer to ' + (o.nameOf(t.with) || t.with) + '\'s agent, in this thread';
      const keep = () => { A.rdraft[t.id] = { kind: kind.value, text: text.value }; };
      kind.onchange = kind.oninput = text.oninput = keep;
      const foot = el('div', 'agCFoot'), send = el('button', 'primary', 'Send'), cancel = btn('Cancel', 'replyCancel');
      send.type = 'button'; send.dataset.act = 'replySend'; send.dataset.thread = t.id;
      send.onclick = () => sendReply(t, kind.value, text.value);
      cancel.onclick = () => { A.replying = ''; A.rmsg[t.id] = ''; render(true); };
      const msg = A.rmsg[t.id] || '';
      foot.append(el('span', 'agCMsg' + (msg && msg !== 'Sending…' ? ' bad' : ''), msg), kind, cancel, send);
      f.append(text, foot);
      return f;
    }
    function sessionLink(sid, label) {
      if (!sid || !o.hasSession || !o.hasSession(sid)) return null;
      const b = btn(label || 'Open session', 'openSession', 'Open the session it went into: ' + ((o.titleOf && o.titleOf(sid)) || sid)); b.className = 'ghost agLink'; b.dataset.id = sid;
      b.onclick = () => o.open(sid);
      return b;
    }
    function thread(t, me, set) {
      const stopped = t.status === 'stopped', name = o.nameOf(t.with) || t.with;
      const box = el('div', 'agThread' + (stopped ? ' stopped' : '')); box.dataset.thread = t.id;
      const th = el('div', 'agTHead');
      th.append(el('span', 'agSubj', t.subject || '(no subject)'));
      if (stopped) th.append(el('span', 'agTSt', stoppedText(t)));
      const tl = sessionLink(t.session, 'Session'); if (tl) th.append(tl);
      // the person's say on the thread: answer in it, stop it, reopen it
      if (!stopped && !t.partial) {
        const rb = btn('Reply', 'reply', 'Answer ' + name + '\'s agent in this thread'); rb.dataset.thread = t.id; rb.setAttribute('aria-expanded', String(A.replying === t.id));
        rb.onclick = () => { A.replying = A.replying === t.id ? '' : t.id; render(true); };
        th.append(rb);
      }
      const sb = stopped ? btn('Reopen', 'reopen', 'Reopen the thread: the agents may write in it again (its hop count starts over)')
        : btn('Stop', 'stop', 'Stop the thread: no agent (and no person) can write in it until you or ' + name + ' reopen it');
      sb.dataset.thread = t.id; sb.onclick = () => { sb.disabled = true; return setThread(t, !stopped); };
      th.append(sb);
      box.append(th);
      if (stopped) box.append(el('div', 'agNote', t.why === 'stopped-by-person' ? 'Stopped: nothing can be written here until you or ' + name + ' reopen it.' : 'The hub stopped this thread (' + stoppedText(t).replace(/^stopped: /, '') + '). Reopen it to go on.'));
      if (t.partial) { box.append(el('div', 'agNote', 'Loading…')); return box; }
      const ol = el('ol', 'agMsgs');
      for (const m of t.messages || []) {
        const mine = isMine(m, me), li = el('li', 'agMsg ' + (mine ? 'out' : 'in') + (m.status === 'held' ? ' held' : '') + (m.status === 'refused' || m.status === 'stopped' ? ' refused' : ''));
        li.dataset.id = m.id || '';
        const ln = el('div', 'agLine');
        ln.append(el('span', 'agDir', mine ? '→' : '←'), el('b', 'agFrom', whoText(m, me, o.nameOf)), el('span', 'agKind', kindLabel(m.kind)));
        if (m.refs && m.refs.task) ln.append(el('span', 'agRef', m.refs.task));
        const at = el('span', 'agAt', when(m.at, now())); at.title = new Date(Number(m.at) || 0).toLocaleString(); ln.append(at);
        li.append(ln);
        const tx = el('div', 'agText', String(m.text || '')); tx.title = 'Click to show it all';
        if (A.expanded.has(m.id)) li.classList.add('openText');
        tx.onclick = () => { if (A.expanded.has(m.id)) A.expanded.delete(m.id); else A.expanded.add(m.id); li.classList.toggle('openText'); };
        li.append(tx);
        const ft = el('div', 'agFoot'); ft.append(el('span', 'agSt', statusText(m, me, set.paused)));
        const src = sourceOf(m, me); if (SOURCES[src] && !mine) ft.append(el('span', 'agSrc', 'in ' + SOURCES[src]));
        const lk = sessionLink(sessionOf(m, me)); if (lk) ft.append(lk);
        li.append(ft); ol.append(li);
      }
      box.append(ol);
      if (A.replying === t.id && !stopped) box.append(replyForm(t));
      return box;
    }
    function card(m, me) {
      const w = cardText(m, me, o.nameOf), c = el('div', 'qcard agCard'); c.dataset.id = m.id;
      c.append(el('div', 'qtitle', w.title));
      if (m.subject && m.kind !== 'handoff') c.append(el('div', 'agCSub', m.subject));
      c.append(el('div', 'agText open', String(m.text || '')), el('div', 'qhint', w.hint));
      const note = el('textarea', 'qown'); note.rows = 2; note.placeholder = 'A note for ' + (o.nameOf(userOf(m.from)) || userOf(m.from)) + ' (optional, goes with a decline)'; note.value = A.notes[m.id] || '';
      note.oninput = () => { A.notes[m.id] = note.value; };
      const row = el('div', 'qfoot'), ok = el('button', 'primary', 'Accept'), no = el('button', 'ghost', 'Decline');
      ok.type = no.type = 'button'; ok.dataset.act = 'accept'; no.dataset.act = 'decline'; ok.dataset.id = no.dataset.id = m.id;
      ok.onclick = () => { ok.disabled = no.disabled = true; return answer(m, true); };
      no.onclick = () => { ok.disabled = no.disabled = true; return answer(m, false); };
      const lk = sessionLink(sessionOf(m, me));
      row.append(ok, no); if (lk) row.append(el('span', 'qgrow'), lk);
      c.append(note, row, el('div', 'qerr', A.err[m.id] || ''));
      return c;
    }
    function setOpen(v) {
      A.open = !!v && A.ok === true; o.box.hidden = !A.open;
      if (o.button) o.button.setAttribute('aria-expanded', String(A.open));
      if (o.store) o.store.set('vr.agentsPanel', A.open ? '1' : '0');
      if (A.open && A.pair) markSeen(A.pair);
      render(true);
    }
    // show one pair (a notification's click)
    function openPair(user) { if (user) A.pair = user; setOpen(true); }
    function loop(gen) {
      if (gen !== A.gen || !o.interval) return;
      A.timer = setTimeout(async () => { if (gen !== A.gen) return; await refresh(); if (A.ok !== false) loop(gen); }, o.interval);
    }
    async function start() { stop(); const gen = A.gen; A.first = true; A.known.clear(); A.threads.clear(); await refresh(); if (A.ok && o.store && o.store.get('vr.agentsPanel') === '1') setOpen(true); if (A.ok) loop(gen); }
    function stop() { A.gen++; clearTimeout(A.timer); A.timer = null; A.busy = false; A.ok = null; A.open = false; A.threads.clear(); A.pair = '';
      A.compose = false; A.team = undefined; A.draft = { to: '', kind: 'ask', task: '', text: '' }; A.cmsg = ''; A.replying = ''; A.rdraft = {}; A.rmsg = {}; hideAll(); }
    if (o.button) o.button.onclick = () => setOpen(!A.open);
    o.box.hidden = true;
    return { refresh, render, setOpen, openPair, start, stop, isOpen: () => A.open, available: () => A.ok === true };
  }

  W.AGENTS = { KINDS, kindLabel, isMine, sessionOf, whoText, statusText, pairs, heldFor, unread, shouldNotify, notifyText, cardText, when, toggled, summary, stoppedText, COMPOSE, REPLY, draftError, refusalText, sentText, teamLabel, mount };
})();
