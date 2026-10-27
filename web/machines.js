// VibeRoller linked machines, page side (T-314, plans/linked-machines.md 2.3, 4.6, 13 Q12). Hub side: the Rust hub's
// native/crates/vr-hub/src/modules/machines.rs (GET /api/machines, PATCH / DELETE /api/machines/<id>, POST /api/device/peek).
// Loaded after the page script; uses its helpers ($, json, ic, sys, rootHandle) only when something is shown.
//
// - Settings > Connections > My machines: ONLY the person's own machines (the hub answers nothing else): each card has the scope in
//   words (red when it is the whole system or unrestricted), a status dot with a shape (filled = online, hollow = offline, bars =
//   paused), the mode, the in-use line, rename / pause / unlink, and how the scope and the kill switch are changed (on the machine only).
// - The in-use bar in Work: "Using the opened folder <name>" while a run uses a folder (the browser's opened folder, or one folder
//   on a linked machine); "Using your PC" only when the run really uses wider access; red for the whole system / unrestricted. It
//   appears when the run first uses it and stays until the run ends; the run cannot hide it.
// - The hub also writes one plain transcript line for a run's first call to a machine (a bridge_info, so older pages show it too).
// The helpers at the top are pure (no page, no DOM): the test suite runs them in node.
const MACHINES = (() => {
  const str = (x) => (x == null ? '' : String(x));
  /** What a scope in words means: {kind: folder | folders | full | unrestricted | none | unknown, folder, access}. */
  function parse(scope) {
    const s = str(scope).trim();
    if (/^unrestricted\b/i.test(s)) return { kind: 'unrestricted', folder: '', access: 'read-write' };
    if (/\bthe whole system\b/i.test(s)) return { kind: 'full', folder: '', access: /the whole system \(read-only\)/i.test(s) && !/the whole system \(read-write\)/i.test(s) ? 'read-only' : 'read-write' };
    const one = /^one folder: (.+?) \((read-write|read-only)\)(;|$)/i.exec(s);
    if (one) return { kind: 'folder', folder: one[1], access: one[2].toLowerCase() };
    if (/^one folder\b/i.test(s)) return { kind: 'folder', folder: '', access: /read-only/i.test(s) ? 'read-only' : 'read-write' };
    if (/^folders: /i.test(s)) return { kind: 'folders', folder: '', access: /read-write/i.test(s) ? 'read-write' : 'read-only' };
    if (/^nothing allowed\b/i.test(s)) return { kind: 'none', folder: '', access: '' };
    return { kind: 'unknown', folder: '', access: '' };
  }
  /** Red: the whole system (read-write or read-only) or unrestricted. */
  const wide = (scope) => ['full', 'unrestricted'].includes(parse(scope).kind);
  /** The scope as the card says it. */
  const scopeWords = (scope) => str(scope).trim() || 'scope not reported yet (the machine tells it on its next poll)';
  /** The in-use bar's text. use: {kind: 'browser', folder} (this browser's opened folder) or {kind: 'machine', name, scope, session}. */
  function barText(use) {
    const u = use || {}, sess = u.session ? ' · session ' + u.session : '';
    if (u.kind === 'browser') return 'Using the opened folder ' + (str(u.folder) || '(the folder you opened)') + ' in this browser' + sess;
    const name = str(u.name).trim() || 'your linked machine', p = parse(u.scope);
    if (p.kind === 'folder') return 'Using the opened folder ' + (p.folder ? p.folder + ' ' : '') + 'on ' + name + (p.access === 'read-only' ? ' (read-only)' : '') + sess;
    if (p.kind === 'full' || p.kind === 'unrestricted') return 'Using your PC ' + name + ': ' + scopeWords(u.scope) + sess;
    if (p.kind === 'folders') return 'Using folders on ' + name + ': ' + scopeWords(u.scope) + sess;
    return 'Using ' + name + ': ' + scopeWords(u.scope) + sess;
  }
  /** The status dot: a colour AND a shape (colour-blind safe), and the words. */
  function statusOf(card) {
    const c = card || {};
    if (c.paused) return { cls: 'paused', glyph: '‖', text: 'paused from the web' + (c.online ? ' (its link is up)' : '') };
    if (c.online) return { cls: 'on', glyph: '●', text: 'online' };
    return { cls: 'off', glyph: '○', text: c.lastSeen ? 'offline, last seen ' + new Date(c.lastSeen).toLocaleString() : 'offline, never connected yet' };
  }
  const modeText = (mode) => (mode === 'persistent' ? 'persistent (a service: reachable while the computer is on)' : mode === 'while open' ? 'while open (only while the app or terminal runs it)' : 'mode not reported yet');
  /** The card's in-use line ('' when no run uses it). */
  function inUseLine(card) {
    const u = (card && Array.isArray(card.inUse)) ? card.inUse : [];
    if (!u.length) return '';
    return 'Now in use by: ' + u.map((x) => 'session ' + (str(x.session) || 'main')).join(', ');
  }

  // ---------------- the page (only from here on is there a DOM) ----------------
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const btn = (text, cls, onclick) => { const b = el('button', cls || '', text); b.type = 'button'; b.onclick = onclick; return b; };
  const dot = (card) => { const st = statusOf(card), d = el('span', 'mDot ' + st.cls, st.glyph); d.title = st.text; d.setAttribute('aria-label', st.text); return d; };

  /** Settings > Connections > My machines. */
  async function render() {
    const box = $('machBox'); if (!box) return;
    let d; try { d = await json('/api/machines'); } catch (e) { box.textContent = /unknown endpoint|not found|no such route/i.test(e.message) ? 'This hub does not offer linked machines yet.' : e.message; return; }
    box.textContent = '';
    const msg = el('span', 'status');
    // link a machine: the code its app shows; check it first (what asks, its key, the scope it starts with), then approve
    const row = el('div', 'row'), code = el('input'); code.placeholder = 'ABCD-EFGH'; code.maxLength = 9; code.autocomplete = 'off';
    const peekBox = el('div', 'mPeek'); peekBox.hidden = true;
    row.append(code, btn('Check the code', '', async () => {
      peekBox.textContent = ''; peekBox.hidden = true;
      try {
        const p = await json('/api/device/peek', { method: 'POST', body: JSON.stringify({ userCode: code.value }) });
        if (p.kind !== 'machine') { msg.textContent = `That code is ${p.client || 'a device'} "${p.device || ''}", not a machine: approve it under Signed-in devices.`; return; }
        peekBox.append(el('div', '', `${p.device || 'A machine'} · ${p.os || '?'} · ${p.app || '?'} ${p.appVersion || ''} · from ${p.ip || '?'} · key ${p.keyFingerprint || '?'}`),
          el('div', '', 'It starts with: ' + scopeWords((p.defaultScope || {}).summary)));
        const go = (replace) => async () => { try { await json('/api/device/approve', { method: 'POST', body: JSON.stringify({ userCode: code.value, ...(replace ? { replace } : {}) }) }); code.value = ''; msg.textContent = 'Linked.'; render(); } catch (e) { msg.textContent = e.message; } };
        const acts = el('div', 'row'); acts.append(btn('Link it to my account', '', go('')));
        for (const r of p.replaceable || []) acts.append(btn(`Replace the old record ${r.name}`, 'ghost', go(r.id)));
        acts.append(btn('Refuse', 'ghost', async () => { try { await json('/api/device/deny', { method: 'POST', body: JSON.stringify({ userCode: code.value }) }); code.value = ''; peekBox.hidden = true; msg.textContent = 'Refused.'; } catch (e) { msg.textContent = e.message; } }));
        peekBox.append(acts); peekBox.hidden = false;
      } catch (e) { msg.textContent = e.message; }
    }), msg);
    box.append(row, peekBox);
    const ms = d.machines || [];
    if (!ms.length) box.append(el('div', 'status', 'No linked machines. Run "Link this machine" in the VibeRoller app (or vr-host machine link) on your computer and type its code here.'));
    for (const m of ms) {
      const card = el('div', 'mCard'), head = el('div', 'row');
      head.append(dot(m), el('b', '', m.name || m.id), el('span', 'muted', ` ${m.os || ''} · ${m.app || ''} ${m.appVersion || ''}`.replace(/ · +$/, '')));
      const sc = el('div', 'mScope' + (wide(m.scope) ? ' wide' : ''), 'Scope: ' + scopeWords(m.scope) + (m.scopeIsDefault ? ' (the default it started with)' : ''));
      if (wide(m.scope)) sc.title = 'This machine lets a run reach the whole system: narrow it on the machine if that is not what you want.';
      card.append(head, sc, el('div', '', 'Status: ' + statusOf(m).text + ' · mode: ' + modeText(m.mode)));
      const used = inUseLine(m); if (used) card.append(el('div', 'mInUse', used));
      card.append(el('div', 'muted', 'Linked ' + (m.linked ? new Date(m.linked).toLocaleString() : '?') + '. The scope and the kill switch are set on the machine itself (its Machine access screen, or vr-host machine scope / stop); the web can only pause or unlink it.'));
      const note = el('span', 'status');
      const patch = (b) => json('/api/machines/' + encodeURIComponent(m.id), { method: 'PATCH', body: JSON.stringify(b) }).then(render).catch((e) => { note.textContent = e.message; });
      const acts = el('div', 'row');
      acts.append(btn('Rename', 'ghost', () => { const n = prompt('A name for this machine', m.name || ''); if (n && n.trim()) patch({ name: n.trim() }); }),
        btn(m.paused ? 'Resume' : 'Pause', 'ghost', () => patch({ paused: !m.paused })),
        btn('Unlink', 'ghost', () => { if (!confirm(`Unlink ${m.name || m.id}? Its token stops working at once and its running jobs end; link it again with a new code.`)) return; json('/api/machines/' + encodeURIComponent(m.id), { method: 'DELETE' }).then(render).catch((e) => { note.textContent = e.message; }); }), note);
      card.append(acts); box.append(card);
    }
  }

  // ---------------- the in-use bar in Work ----------------
  let using = null; // what the running run uses: {kind, ...}
  function showBar() {
    const bar = $('useBar'); if (!bar) return;
    bar.hidden = !using;
    if (!using) return;
    bar.className = wide(using.scope) ? 'wide' : '';
    bar.textContent = '';
    bar.append(ic(using.kind === 'browser' ? 'folder' : 'server', 'lead'), barText(using));
    bar.title = using.kind === 'browser' ? 'Claude reads and writes in the folder you opened in this browser while this run goes on.' : 'Claude uses your linked machine while this run goes on: ' + scopeWords(using.scope);
  }
  /** Every Work event passes here first (handleEvent): a run starts / ends, the browser folder answers a call, the hub says a machine is used. */
  function onEvent(ev) {
    if (!ev || !ev.type) return;
    if (ev.type === 'bridge_run' || ev.type === 'bridge_done') { using = null; return showBar(); }
    if (ev.type === 'pc_tool') { if (!using || using.kind === 'browser') { using = { kind: 'browser', folder: (typeof rootHandle !== 'undefined' && rootHandle) ? rootHandle.name : '' }; showBar(); } return; }
    if (ev.type === 'bridge_info' && ev.machine && typeof ev.machine === 'object') {
      const m = ev.machine;
      // the wider one wins when a run uses two places
      if (!using || using.kind === 'browser' || wide(m.scope) || !wide(using.scope)) { using = { kind: 'machine', id: str(m.id), name: str(m.name), scope: str(m.scope), session: '' }; showBar(); }
    }
  }
  return { parse, wide, scopeWords, barText, statusOf, modeText, inUseLine, render, onEvent, using: () => using };
})();
