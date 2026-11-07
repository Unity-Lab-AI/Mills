// VibeRoller Work: the queue panel (T-188). A hidable sidebar next to the Work conversation that lists every message
// still to go in the open session (the "Send now" text first, then the queue in order; never one already sent), with
// cancel one / cancel all and copy one / copy all, plus how many wait in the person's other sessions (the session list's
// `queued`, T-284), each a click away. The queue itself stays the page's (web/index.html: `queue`, `sendNowMessage`,
// saved whole to /api/work/queue): this panel only reads it and asks the page to change it, so nothing about how the hub
// keeps it changed.
// Loaded before the main page script. The pure helpers at the top (no DOM) and mount() with a plain element API are what
// the suite runs in node (R526).
(function () {
  'use strict';
  const W = typeof window !== 'undefined' ? window : globalThis;

  // ── pure helpers ──
  const FILES = /\n*<!--vr-files (\[[\s\S]*?\]) -->\s*$/; // the attachment marker a Work message carries at its end
  // a message as the person wrote it: the marker becomes a line naming the files
  function plain(message) {
    const m = String(message || ''), f = FILES.exec(m);
    if (!f) return m;
    let names = []; try { names = JSON.parse(f[1]).map((x) => x && x.n).filter(Boolean); } catch {}
    const text = m.slice(0, f.index);
    return names.length ? (text ? text + '\n' : '') + '[attached: ' + names.join(', ') + ']' : text;
  }
  // what is still to go, in the order it goes: "Send now" first, then the queue
  function items(queue, sendNow) {
    const out = [];
    if (typeof sendNow === 'string' && sendNow) out.push({ kind: 'now', at: -1, text: sendNow });
    (queue || []).forEach((t, i) => out.push({ kind: 'queued', at: i, text: String(t) }));
    return out;
  }
  // copy all: every message as written, in order, a rule between them
  const copyText = (list) => list.map((x) => plain(x.text)).join('\n\n---\n\n');
  // the person's other sessions with messages waiting (the open one is counted from the page, it is newer)
  function others(sessions, current) {
    return (sessions || []).filter((s) => s && s.id !== current && !s.archived && s.queued > 0).map((s) => ({ id: s.id, s, queued: s.queued }));
  }
  function totals(here, sessions, current) {
    const o = others(sessions, current), elsewhere = o.reduce((n, x) => n + x.queued, 0);
    return { here, elsewhere, all: here + elsewhere, sessions: o.length };
  }
  function summary(t) {
    const here = t.here ? t.here + ' here' : 'nothing here';
    return t.elsewhere ? here + ' · ' + t.all + ' in all your sessions' : here;
  }
  const badgeText = (t) => (t.all ? String(t.all) : '');
  const place = (x, i) => (x.kind === 'now' ? 'Send now' : i === 0 ? 'Next' : '#' + (i + 1)); // i: its place in items()

  // ── the panel ──
  // o: { box, button, badge, queue(): [string], sendNow(): string|null, cancel(item), cancelAll(), sessions(): [session],
  //      current(): id, open(id), titleOf(session), copy(text): Promise, note(text, bad), store: { get, set } }
  function mount(o) {
    const doc = o.document || W.document;
    const el = (tag, cls, text) => { const e = doc.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; };
    const btn = (label, act, title) => { const b = el('button', 'ghost', label); b.type = 'button'; b.dataset.act = act; if (title) b.title = title; return b; };
    let armed = null; // "Cancel all" asks once: the second click within 3 s does it
    const P = { open: false };
    const setOpen = (v) => { P.open = !!v; o.box.hidden = !P.open; if (o.button) o.button.setAttribute('aria-expanded', String(P.open)); if (o.store) o.store.set('vr.queuePanel', P.open ? '1' : '0'); render(); };
    const copy = async (text, what) => {
      try { await o.copy(text); o.note(what + ' copied.'); } catch { o.note('Could not copy to the clipboard: select the text in the panel instead.', true); }
    };
    let drawn = ''; // what the open panel shows: the session list refreshes every few seconds, and an unchanged panel is not redrawn (an opened message and the focus stay)
    function render(force) {
      const list = items(o.queue(), o.sendNow()), t = totals(list.length, o.sessions(), o.current());
      if (o.badge) { o.badge.textContent = badgeText(t); o.badge.hidden = !t.all; }
      if (o.button) o.button.title = 'Queued messages: ' + summary(t);
      if (!P.open) { drawn = ''; return; }
      const sig = JSON.stringify([list, others(o.sessions(), o.current()).map((x) => [x.id, x.queued, o.titleOf(x.s)]), !!armed]);
      if (!force && sig === drawn) return;
      drawn = sig;
      const box = o.box; box.textContent = '';
      const head = el('div', 'qpHead');
      const close = btn('', 'close', 'Hide the queue panel'); close.className = 'ghost qpX'; close.setAttribute('aria-label', 'Hide the queue panel');
      if (typeof W.ic === 'function') close.append(W.ic('close')); else close.textContent = '×';
      close.onclick = () => setOpen(false);
      head.append(el('b', '', 'Queued'), el('span', 'qpSum', summary(t)), close);
      const acts = el('div', 'qpActs');
      const all = btn('Copy all', 'copyAll', 'Copy every queued message, in order'), none = btn(armed ? 'Sure? Cancel all' : 'Cancel all', 'cancelAll', 'Remove every queued message of this session (nothing already sent is touched)');
      all.disabled = none.disabled = !list.length; if (armed) none.className = 'ghost danger';
      all.onclick = () => copy(copyText(list), list.length === 1 ? 'The message' : 'All ' + list.length + ' messages');
      none.onclick = () => {
        if (!armed) { armed = setTimeout(() => { armed = null; render(); }, 3000); render(); return; }
        clearTimeout(armed); armed = null; o.cancelAll(); render();
      };
      acts.append(all, none);
      box.append(head, acts);
      const ul = el('ol', 'qpList');
      list.forEach((x, i) => {
        const li = el('li', 'qpItem' + (x.kind === 'now' ? ' now' : '')), text = plain(x.text);
        li.dataset.kind = x.kind; li.dataset.at = String(x.at);
        const tx = el('div', 'qpText', text); tx.title = 'Click to show it all'; tx.onclick = () => li.classList.toggle('open');
        const row = el('div', 'qpRow');
        const c = btn('Copy', 'copy', 'Copy this message'), k = btn('Cancel', 'cancel', 'Remove it from the queue: it is not sent');
        c.onclick = () => copy(text, 'The message');
        k.onclick = () => { o.cancel(x); render(); };
        row.append(el('span', 'qpPlace', place(x, i)), c, k);
        li.append(row, tx); ul.append(li);
      });
      if (list.length) box.append(ul);
      else box.append(el('div', 'qpNote', 'Nothing queued in this session. While Claude is busy, Send queues a message here; it goes when the reply ends.'));
      const ot = others(o.sessions(), o.current());
      if (ot.length) {
        box.append(el('div', 'qpSub', 'Waiting in your other sessions'));
        for (const x of ot) {
          const r = btn('', 'openSession', 'Open this session to see, copy or cancel its queue'); r.className = 'ghost qpOther'; r.dataset.id = x.id;
          r.append(el('span', 'qpOT', o.titleOf(x.s)), el('span', 'qpON', x.queued + ' queued'));
          r.onclick = () => o.open(x.id);
          box.append(r);
        }
      }
    }
    if (o.button) o.button.onclick = () => setOpen(!P.open);
    P.open = !!(o.store && o.store.get('vr.queuePanel') === '1'); o.box.hidden = !P.open;
    render();
    return { render, setOpen, isOpen: () => P.open };
  }

  W.QUEUE = { plain, items, copyText, others, totals, summary, badgeText, mount };
})();
