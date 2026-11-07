// VibeRoller Work: the agent threads (T-298 = T-294 · 4/6; team note plans/agent-talk.md). People stay in charge of what
// their agent says and takes: a hidable sidebar next to the Work conversation (the "Agents" button, the menu tile) with every
// message to and from the person's agent, a thread per agent pair (both directions, who / when / kind, the session it went
// into one click away), the person's controls (mute a pair, pause my inbox, "ask me first") and an accept / decline card for
// a held message; a new message notifies the way the page does, following Do Not Disturb (it mutes the notification only).
// It only reads and asks the hub's /api/agents/* (the Rust hub's, T-296): threads, thread, settings, held. A hub without
// them (an older hub, the JavaScript fallback hub) answers 404: then the panel stays empty and hidden and nothing throws.
// Loaded before the main page script. The pure helpers at the top (no DOM) and mount() with a plain element API are what
// the suite runs in node (R532, R532b).
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
    const A = { open: false, ok: null, settings: null, threads: new Map(), known: new Set(), first: true, pair: '', busy: false, timer: null, gen: 0, err: {}, notes: {}, expanded: new Set() };
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
      const sig = JSON.stringify([ts.map((t) => [t.id, t.lastAt, t.status, (t.messages || []).map((m) => [m.id, m.status, m.why, (m.to || {}).session])]), set, A.pair, [...A.expanded], A.err, (o.hasSession ? ts.map((t) => (t.messages || []).map((m) => !!o.hasSession(sessionOf(m, me)))) : 0)]);
      if (!force && sig === drawn) return;
      drawn = sig;
      const box = o.box; box.textContent = '';
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
      ctl.append(pz, af);
      if (set.paused) ctl.append(el('div', 'agNote', 'Your inbox is paused: messages wait here until you resume.'));
      box.append(ctl);
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
    }
    function sessionLink(sid, label) {
      if (!sid || !o.hasSession || !o.hasSession(sid)) return null;
      const b = btn(label || 'Open session', 'openSession', 'Open the session it went into: ' + ((o.titleOf && o.titleOf(sid)) || sid)); b.className = 'ghost agLink'; b.dataset.id = sid;
      b.onclick = () => o.open(sid);
      return b;
    }
    function thread(t, me, set) {
      const box = el('div', 'agThread' + (t.status === 'stopped' ? ' stopped' : ''));
      const th = el('div', 'agTHead');
      th.append(el('span', 'agSubj', t.subject || '(no subject)'));
      if (t.status === 'stopped') th.append(el('span', 'agTSt', { loop: 'stopped: going in circles', hops: 'stopped: at its limit', full: 'stopped: full', 'stopped-by-person': 'stopped' }[t.why] || 'stopped'));
      const tl = sessionLink(t.session, 'Session'); if (tl) th.append(tl);
      box.append(th);
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
    function stop() { A.gen++; clearTimeout(A.timer); A.timer = null; A.busy = false; A.ok = null; A.open = false; A.threads.clear(); A.pair = ''; hideAll(); }
    if (o.button) o.button.onclick = () => setOpen(!A.open);
    o.box.hidden = true;
    return { refresh, render, setOpen, openPair, start, stop, isOpen: () => A.open, available: () => A.ok === true };
  }

  W.AGENTS = { KINDS, kindLabel, isMine, sessionOf, whoText, statusText, pairs, heldFor, unread, shouldNotify, notifyText, cardText, when, toggled, summary, mount };
})();
