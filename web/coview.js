// VibeRoller co-view (T-244), page side (hub side: native/crates/pgm-hcom-internal/src/chat.rs, /api/coview/*).
// Loaded after the page script and calls.js; uses the page's helpers ($, json, me, showScreen, selectConv, openWatch, BOARD, NOTES).
//
// Not pixels: the sharer's (or the lead's) page posts WHICH view it shows ({ screen, item: { kind, id } }), the hub carries only that, and
// each viewer's page opens the same screen + item ITSELF, through the normal routes with its own permissions. A viewer who may not read
// the note / task / session sees "not shared with you" instead. The sharer can invite a viewer to take over (or hand over directly); a
// viewer can stop following any time. Older hubs (and the JavaScript fallback hub) answer "Unknown endpoint": the buttons stay hidden.
const COVIEW = (() => {
  const SCREENS = ['menu', 'shell', 'room', 'board', 'cal', 'notes', 'assist'];
  const S = { gen: 0, ver: -1, after: 0, id: '', mine: null, avail: [], ok: false, last: '', applied: 0, timer: null, notice: '', dismissed: new Set() };
  const post = (p, b) => json('/api/coview/' + p, { method: 'POST', body: JSON.stringify(b || {}) });
  const nameOf = (u) => (typeof nickOf === 'function' ? nickOf(u) : u);
  const SCREEN_LABEL = (s) => (typeof SCREEN_NAMES !== 'undefined' && SCREEN_NAMES[s]) || s || '';

  function capture() { // what this page shows right now
    const screen = SCREENS.find((id) => $(id) && !$(id).hidden) || 'menu';
    let item = null;
    try {
      if (screen === 'board' && BOARD.openId()) item = { kind: 'board-task', id: BOARD.openId() };
      else if (screen === 'notes' && NOTES.state.open) item = { kind: 'note', id: NOTES.state.open };
      else if (screen === 'room' && currentConv) item = { kind: 'conv', id: currentConv };
      else if (screen === 'shell' && myShare) item = { kind: 'work-session', id: me.username };
    } catch {}
    return { screen, item };
  }
  async function follow(v) { // open the same view in THIS page; what the person may not read is not shown
    S.notice = '';
    if (!v || !SCREENS.includes(v.screen)) return;
    const it = v.item;
    if (it && it.hidden) { showScreen(v.screen); S.notice = 'Not shared with you'; return render(); }
    if ($(v.screen) && $(v.screen).hidden) showScreen(v.screen);
    let ok = true;
    try {
      if (!it) { /* just the screen */ }
      else if (it.kind === 'board-task') ok = await BOARD.openTask(it.id);
      else if (it.kind === 'note') ok = await NOTES.openPath(it.id);
      else if (it.kind === 'conv') selectConv(it.id);
      else if (it.kind === 'work-session') { if (it.id !== me.username) openWatch(it.id); }
    } catch { ok = false; }
    if (!ok) S.notice = 'Not shared with you';
    render();
  }
  function onState(d) {
    const was = S.mine; S.mine = d.mine || null; S.avail = d.available || []; S.ver = d.ver;
    if (!S.mine) { S.id = ''; S.after = 0; S.applied = 0; S.last = ''; render(); return; }
    if (S.mine.id !== S.id) { S.id = S.mine.id; S.after = 0; S.applied = 0; S.last = ''; }
    const lead = !!S.mine.isLead;
    if (!lead && (!was || was.isLead || S.mine.seq > S.applied)) { // somebody else leads: follow their newest view
      const newer = S.mine.seq > S.applied; S.applied = S.mine.seq;
      if (newer || !was) follow(S.mine.view);
    } else if (lead) { S.applied = S.mine.seq; if (!was || !was.isLead) S.last = ''; }
    S.after = S.mine.seq; render();
  }
  async function loop(gen) {
    while (gen === S.gen && me) {
      let d;
      try { d = await json(`/api/coview/poll?wait=1&ver=${S.ver}&after=${S.after}`); }
      catch (e) { if (/unknown endpoint/i.test(e.message)) { S.ok = false; render(); return; } await sleep(3000); continue; }
      if (gen !== S.gen) return;
      S.ok = true; onState(d);
    }
  }
  function tick() { // the lead's page tells the hub what it shows when that changed
    if (!S.mine || !S.mine.isLead) return;
    const v = capture(), sig = JSON.stringify(v);
    if (sig === S.last) return;
    S.last = sig;
    post('view', v).catch(() => { S.last = ''; });
  }
  async function act(p, b) { try { onState(await post(p, b)); } catch (e) { S.notice = e.message; render(); } }
  const startCoview = () => act('start', Object.assign({ conv: (typeof call !== 'undefined' && call.conv) || 'room' }, capture()));

  function bar() {
    let b = $('coviewBar');
    if (!b) {
      b = document.createElement('div'); b.id = 'coviewBar'; b.hidden = true;
      b.style.cssText = 'position:fixed;top:0;left:50%;transform:translateX(-50%);z-index:60;display:flex;gap:.5rem;align-items:center;padding:.35rem .7rem;border-radius:0 0 .6rem .6rem;background:var(--panel,#222);color:var(--fg,#eee);border:1px solid var(--accent,#68f);border-top:0;font-size:.85rem;box-shadow:0 2px 8px rgba(0,0,0,.4)';
      document.body.append(b);
    }
    return b;
  }
  function render() {
    for (const id of ['coviewBtn', 'callCoview']) {
      const x = $(id); if (!x) continue;
      const mine = S.mine && S.mine.role === 'sharer';
      x.hidden = !S.ok || (id === 'callCoview' && !(typeof call !== 'undefined' && call.conv));
      x.textContent = mine ? 'End co-view' : 'Co-view'; x.classList.toggle('on', !!mine);
    }
    const b = bar(); b.textContent = ''; const m = S.mine;
    const add = (t) => { const s = document.createElement('span'); s.textContent = t; b.append(s); return s; };
    const btn = (t, f, title) => { const x = document.createElement('button'); x.type = 'button'; x.className = 'ghost'; x.textContent = t; if (title) x.title = title; x.onclick = f; b.append(x); return x; };
    if (m) {
      const lead = m.lead === m.sharer ? '' : ' · led by ' + m.leadName;
      if (m.role === 'sharer') {
        add('Co-view: ' + (m.viewers.length ? m.viewers.length + ' following' : 'nobody following yet') + lead);
        if (m.viewers.length) {
          const sel = document.createElement('select'); sel.title = 'Hand the lead to a viewer (the others follow them)';
          sel.append(new Option('Hand over to…', ''));
          if (!m.isLead) sel.append(new Option('Me', m.sharer));
          for (const v of m.viewers) if (v.user !== m.lead) sel.append(new Option(v.name, v.user));
          sel.onchange = () => { if (sel.value) act('handover', { to: sel.value }); };
          const inv = btn('Invite to take over', () => { const x = m.viewers.find((v) => v.user !== m.lead); if (x) act('invite', { to: x.user }); }, 'Ask the first viewer to take over; they accept with one click');
          inv.hidden = m.viewers.length < 1; b.append(sel);
        }
        btn('End', () => act('stop'), 'End the co-view for everyone');
      } else {
        add((m.isLead ? 'Leading · ' : 'Following ' + (m.leadName || m.sharerName) + ' · ') + SCREEN_LABEL(m.view && m.view.screen));
        if (S.notice) add('· ' + S.notice);
        if (m.invited && !m.isLead) btn('Take over', () => act('take'), m.sharerName + ' invites you to show your view');
        if (m.isLead && m.sharer !== me.username) btn('Give back', () => act('handover', { to: m.sharer }));
        btn('Stop', () => act('stop'), 'Stop following (your page stays where it is)');
      }
      b.hidden = false; return;
    }
    const open = S.avail.filter((a) => !S.dismissed.has(a.id))[0];
    if (open) { add(open.name + ' is sharing their view'); btn('Follow', () => act('join', { sharer: open.id })); btn('Not now', () => { S.dismissed.add(open.id); render(); }); b.hidden = false; }
    else if (S.notice) { add(S.notice); b.hidden = false; setTimeout(() => { S.notice = ''; render(); }, 4000); }
    else b.hidden = true;
  }
  for (const id of ['coviewBtn', 'callCoview']) {
    const x = $(id); if (x) x.onclick = () => (S.mine && S.mine.role === 'sharer' ? act('stop') : startCoview());
  }
  return {
    start() { S.gen++; S.ver = -1; S.after = 0; S.mine = null; S.ok = false; clearInterval(S.timer); S.timer = setInterval(tick, 800); loop(S.gen); },
    stop() { S.gen++; clearInterval(S.timer); S.mine = null; S.ok = false; S.id = ''; render(); },
    state: S,
  };
})();
