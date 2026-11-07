// VibeRoller menus (T-189: T-223 the kit, T-224 VibeRoller's lists). One context menu for every product page (and the
// desktop apps, the same page): right-click, long-press on touch, the ContextMenu key / Shift+F10 and a "⋯" button all
// open the same menu, built from one list of items, so the two ways in never drift apart.
// Plain DOM and standard events only (Chrome, Firefox, WebView2, WebKitGTK). Loaded before the page script; a page
// without this file simply keeps the browser's own menu.
//
// The kit (W.MENU):
//   open(items, x, y, pick?, opts?)  a menu at the pointer, kept on screen; returns { el, close }
//   attach(el, itemsFn, pick?, opts?) right-click / long-press / keyboard on el (or on anything inside it: itemsFn(target, ev)
//                                     gets what was clicked and returns its items, or null for the browser's menu);
//                                     opts.more(target) → the "⋯" element inside el whose click opens the same list
//   button(btn, itemsFn, pick?)       a "⋯" button: a click opens the list under it
//   close(), isOpen(), copy(text)
//   pure, no DOM: place(x, y, size, view), subPlace(row, size, view), normalize(items), textField(node), linkOf(node)
// An item: { id, label, icon?, shortcut?, danger?, disabled?, on? (ticked), sub? [items], confirm? (the label of a second
// pick that does it: "Click again to delete"), run? () }, a note { note: text } (not clickable), or '-' (a line). Falsy
// entries are left out, so a list reads `cond && {…}`. Picking calls item.run() and then pick(id, item).
// The browser's own menu still comes: Shift + right-click, in a text field, with text selected, and "Browser menu"
// (offered on links: the next right-click is left to the browser).
//
// VibeRoller's lists (MENU.lists; each takes what is known about the thing and `a`, the page's EXISTING actions; an item
// is there only when the person may do it, the same check as the page's own button, and when its action is given):
//   message (team chat; a click on the message opens it too): Reply · Copy text · Edit (own, not a voice clip / locked) ·
//            Pin / Unpin (who may pin there) · Go to the replied message · Profile of <author> · Delete (own) or
//            Delete (moderator) (manage messages, not in a DM), asks twice · the time and who has seen it
//   person (member column, DMs): Profile · Message (not yourself) · Mention · Copy @username · Member settings… (a server)
//   channel: Open · Edit channel… (manage channels, or its maker) · Server settings…
//   group (group DM): Open · Group settings…
//   server (the rail): Open · Server settings… · Invite people… (create invite, not the everyone server) · New channel…
//   card (board): Open · Claim / Renew 30 min · Release (yours) · Complete · Move to ▸ (the columns) · Assign to ▸ (people) ·
//            Delegate… · Archive (done) / Restore (archived) · Give to Claude · Copy id · Delete (asks twice); someone
//            else's claim leaves Open, Give to Claude and Copy id
//   note: Open · Rename or move… · Copy path · Delete (asks twice);  note folder: New note here… · Expand / Collapse · New folder…
//   file: Open · Download · Edit (Work text up to 2 MB) · Use in Work · Run in Work · Make executable / not (Work files) ·
//            Copy to my uploaded_files (a chat file, linked account) · Delete for good (asks twice); a gone or locked
//            file only Delete
//   work (a Work message): yours: Copy · Send again;  Claude's / a tool line: Copy;  queued: Copy · Show the queue ·
//            Cancel (the queue panel's own cancel)
//   saved (a saved conversation): Open here · Rename… · Delete (asks twice)
//   session / folder (the Work session list): web/sessions.js menuItems / folderMenuItems, shown through this kit
(function () {
  'use strict';
  const W = typeof window !== 'undefined' ? window : globalThis;
  const MARGIN = 4, LONG_PRESS = 550, MOVE_SLOP = 10;

  // ── pure: where it goes, what it shows ──
  // at the pointer; past the right / bottom edge it opens to the other side of the pointer; then kept inside the view
  function place(x, y, size, view, margin) {
    const m = margin == null ? MARGIN : margin;
    let left = x, top = y;
    if (left + size.w > view.w - m) left = x - size.w;
    if (top + size.h > view.h - m) top = y - size.h;
    return { left: Math.max(m, Math.min(left, view.w - size.w - m)), top: Math.max(m, Math.min(top, view.h - size.h - m)) };
  }
  // a sub-menu: beside its row, on the right; flipped to the left of the menu when it would leave the screen
  function subPlace(row, size, view, margin) {
    const m = margin == null ? MARGIN : margin;
    let left = row.right - 2, flipped = false;
    if (left + size.w > view.w - m) { left = row.left - size.w + 2; flipped = true; }
    const top = Math.max(m, Math.min(row.top - 4, view.h - size.h - m));
    return { left: Math.max(m, Math.min(left, view.w - size.w - m)), top, flipped };
  }
  // the list as it is drawn: falsy and hidden entries out, no line first, last or twice, a sub-menu without items out
  function normalize(items) {
    const out = [];
    (items || []).forEach((x, i) => {
      if (!x || x.hidden) return;
      if (x === '-' || x.sep) { if (out.length && !out[out.length - 1].sep) out.push({ sep: true }); return; }
      if (x.note != null) { if (String(x.note)) out.push({ note: String(x.note) }); return; }
      if (x.label == null || x.label === '') return;
      const it = { id: x.id != null ? String(x.id) : 'i' + i, label: String(x.label) };
      for (const k of ['icon', 'shortcut', 'danger', 'disabled', 'on', 'confirm', 'run', 'hint']) if (x[k] != null && x[k] !== false) it[k] = x[k];
      if (x.sub) { it.sub = normalize(x.sub); if (!it.sub.some((s) => s.id)) return; it.label = it.label.replace(/\s*▸$/, ''); }
      out.push(it);
    });
    while (out.length && out[out.length - 1].sep) out.pop();
    return out;
  }
  const up = (n, f) => { for (let x = n; x; x = x.parentNode) if (f(x)) return x; return null; };
  const TEXT_INPUT = /^(|text|search|email|url|password|number|tel|date|time|datetime-local|month|week)$/;
  // a place to type keeps the browser's menu (paste, spelling, …)
  function textField(n) {
    return !!up(n, (x) => {
      const t = String(x.tagName || '').toUpperCase();
      if (t === 'TEXTAREA') return true;
      if (t === 'INPUT') return TEXT_INPUT.test(String(x.type || '').toLowerCase());
      return x.isContentEditable === true || (x.getAttribute && x.getAttribute('contenteditable') === 'true');
    });
  }
  function linkOf(n) { const a = up(n, (x) => String(x.tagName || '').toUpperCase() === 'A' && !!(x.href || (x.getAttribute && x.getAttribute('href')))); return a ? String(a.href || a.getAttribute('href')) : ''; }

  // ── the menu on screen ──
  const S = { levels: [], opener: null, pick: null, timer: null, pass: 0, lpAt: 0, eatClick: 0, kbAt: 0, outside: null, wired: false };
  const doc = () => W.document;
  const now = () => Date.now();
  const later = (f, ms) => (W.setTimeout || setTimeout)(f, ms);
  const cancel = (t) => (W.clearTimeout || clearTimeout)(t);
  const view = () => ({ w: W.innerWidth || 1024, h: W.innerHeight || 768 });
  const mk = (tag, cls, text) => { const e = doc().createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const rows = (L) => L.rows.filter((r) => !r.item.disabled).map((r) => r.btn);
  const inMenu = (t) => S.levels.some((L) => L.el === t || (L.el.contains && L.el.contains(t)));

  function wireDoc() {
    if (S.wired || !doc() || !doc().addEventListener) return; S.wired = true;
    doc().addEventListener('pointerdown', (e) => { if (S.levels.length && !inMenu(e.target)) { S.outside = { t: e.target, at: now() }; close(false); } }, true);
    doc().addEventListener('scroll', (e) => { if (S.levels.length && !inMenu(e.target)) close(false); }, true);
    if (W.addEventListener) { W.addEventListener('resize', () => close(false)); W.addEventListener('blur', () => close(false)); }
  }
  function close(focusBack) {
    cancel(S.timer); S.timer = null;
    const had = S.levels.length;
    for (const L of S.levels) L.el.remove();
    S.levels = [];
    const o = S.opener; S.opener = null; S.pick = null;
    if (had && focusBack && o && o.focus && o.isConnected !== false) o.focus();
  }
  function closeFrom(depth) {
    while (S.levels.length > depth) { const L = S.levels.pop(); L.el.remove(); }
    const P = S.levels[depth - 1];
    if (P) for (const r of P.rows) if (r.btn.classList) { r.btn.classList.remove('open'); r.btn.setAttribute('aria-expanded', 'false'); }
  }
  function icon(name) {
    const s = mk('span', 'vrIc');
    if (name && typeof W.ic === 'function') s.append(W.ic(name)); // the page's own icon set
    return s;
  }
  function level(items, depth, opts) {
    const el = mk('div', 'vrMenu' + (opts && opts.className ? ' ' + opts.className : ''));
    el.setAttribute('role', 'menu'); el.tabIndex = -1; el.dataset.depth = String(depth);
    if (opts && opts.label) el.setAttribute('aria-label', opts.label);
    const L = { el, depth, rows: [], parent: null };
    for (const it of items) {
      if (it.sep) { const s = mk('div', 'vrSep'); s.setAttribute('role', 'separator'); el.append(s); continue; }
      if (it.note) { el.append(mk('div', 'vrNote', it.note)); continue; }
      const b = mk('button', 'vrMi' + (it.danger ? ' danger' : ''));
      b.type = 'button'; b.tabIndex = -1; b.dataset.id = it.id;
      b.setAttribute('role', it.on != null ? 'menuitemcheckbox' : 'menuitem');
      if (it.on != null) b.setAttribute('aria-checked', String(!!it.on));
      if (it.disabled) { b.disabled = true; b.setAttribute('aria-disabled', 'true'); }
      if (it.sub) { b.setAttribute('aria-haspopup', 'menu'); b.setAttribute('aria-expanded', 'false'); }
      if (it.hint) b.title = it.hint;
      const lb = mk('span', 'vrLb', it.label), r = { btn: b, item: it, lb };
      b.append(icon(it.on ? 'check' : it.icon), lb);
      if (it.sub) b.append(mk('span', 'vrAr', '▸')); else if (it.shortcut) b.append(mk('span', 'vrSc', it.shortcut));
      b.addEventListener('click', (e) => { e.stopPropagation(); if (e.preventDefault) e.preventDefault(); activate(L, r); });
      b.addEventListener('mouseenter', () => {
        if (it.disabled) return;
        b.focus(); cancel(S.timer);
        if (it.sub) S.timer = later(() => openSub(L, b, it, false), 120); else closeFrom(depth + 1);
      });
      L.rows.push(r);
      el.append(b);
    }
    el.addEventListener('keydown', (e) => keys(L, e));
    return L;
  }
  function show(L, at) {
    L.el.style.left = '0px'; L.el.style.top = '0px';
    doc().body.append(L.el);
    const r = L.el.getBoundingClientRect(), size = { w: r.width, h: r.height };
    const p = at.row ? subPlace(at.row, size, view()) : place(at.x, at.y, size, view());
    L.el.style.left = Math.round(p.left) + 'px'; L.el.style.top = Math.round(p.top) + 'px';
    if (p.flipped) L.el.classList.add('flipped');
    S.levels.push(L);
  }
  function openSub(L, b, it, focusFirst) {
    cancel(S.timer); S.timer = null;
    if (S.levels.indexOf(L) < 0) return;
    closeFrom(L.depth + 1);
    const sub = level(it.sub, L.depth + 1); sub.parent = b;
    b.classList.add('open'); b.setAttribute('aria-expanded', 'true');
    show(sub, { row: b.getBoundingClientRect() });
    const f = rows(sub)[0]; if (focusFirst && f) f.focus(); else sub.el.focus();
  }
  function activate(L, r) {
    const b = r.btn, it = r.item;
    if (it.disabled) return;
    if (it.sub) return openSub(L, b, it, true);
    if (it.confirm && b.dataset.armed !== '1') { // asks once more: the second pick does it
      b.dataset.armed = '1'; b.classList.add('armed'); r.lb.textContent = it.confirm; b.setAttribute('aria-label', it.confirm); b.focus();
      return;
    }
    const pick = S.pick;
    close(true); // focus back where it was first; the action may move it on
    if (typeof it.run === 'function') it.run(it);
    if (typeof pick === 'function') pick(it.id, it);
  }
  function keys(L, e) {
    const rs = rows(L), i = rs.indexOf(doc().activeElement), r = L.rows.find((x) => x.btn === doc().activeElement), k = e.key;
    const go = (j) => { if (rs.length) rs[(j + rs.length) % rs.length].focus(); };
    let done = true;
    if (k === 'ArrowDown') go(i + 1);
    else if (k === 'ArrowUp') go(i < 0 ? -1 : i - 1);
    else if (k === 'Home') go(0);
    else if (k === 'End') go(-1);
    else if (k === 'ArrowRight' && r && r.item.sub) openSub(L, r.btn, r.item, true);
    else if ((k === 'ArrowLeft' && L.depth > 0) || (k === 'Escape' && L.depth > 0)) { const p = L.parent; closeFrom(L.depth); if (p) p.focus(); }
    else if (k === 'Escape' || k === 'Tab') close(true);
    else if ((k === 'Enter' || k === ' ') && r) activate(L, r);
    else if (k && k.length === 1 && /\S/.test(k)) { // type-ahead: the next item starting with that letter
      const lab = (b) => L.rows.find((x) => x.btn === b).item.label.toLowerCase(), c = k.toLowerCase();
      const hit = [...rs.slice(i + 1), ...rs.slice(0, i + 1)].find((b) => lab(b).startsWith(c));
      if (hit) hit.focus(); else done = false;
    } else done = false;
    if (done) { e.preventDefault(); if (e.stopPropagation) e.stopPropagation(); }
  }
  function open(items, x, y, pick, opts) {
    const o = opts || {}, list = normalize(items);
    close(false);
    if (!list.some((it) => it.id)) return null;
    wireDoc();
    S.opener = o.returnFocus || (doc().activeElement && doc().activeElement !== doc().body ? doc().activeElement : null);
    S.pick = pick || null;
    const L = level(list, 0, o);
    show(L, { x, y });
    const f = rows(L)[0];
    if (o.focusFirst && f) f.focus(); else L.el.focus(); // a mouse opens it quietly; the keyboard lands on the first item
    return { el: L.el, close: () => close(true) };
  }
  // "Browser menu": the next right-click (within 8 s) is left to the browser
  function passNext() {
    S.pass = now() + 8000;
    if (!doc() || !doc().body) return;
    const h = mk('div', 'vrHint', 'Right-click again for the browser\'s menu (or Shift + right-click any time)');
    doc().body.append(h); later(() => h.remove(), 3000);
  }
  function copy(text) {
    const c = W.navigator && W.navigator.clipboard;
    return c && c.writeText ? c.writeText(String(text)) : Promise.reject(new Error('no clipboard'));
  }
  function withLink(items, href) {
    return [...items, '-', { id: 'link:open', label: 'Open link in a new tab', icon: 'link', run: () => W.open(href, '_blank', 'noopener') },
      { id: 'link:copy', label: 'Copy link', run: () => copy(href).catch(() => W.prompt && W.prompt('The link:', href)) },
      { id: 'browser', label: 'Browser menu', shortcut: 'Shift+right-click', run: passNext }];
  }
  const selected = (el) => { try { const s = W.getSelection && W.getSelection(); return !!(s && String(s) && s.anchorNode && el.contains(s.anchorNode)); } catch { return false; } };
  // right-click, long-press, ContextMenu key / Shift+F10, and the ⋯ button, on el or anything in it
  function attach(el, itemsFn, pick, opts) {
    const o = opts || {};
    if (!el || !el.addEventListener) return () => {};
    const build = (t, ev) => {
      const items = itemsFn(t, ev); if (!items) return null;
      const href = o.links === false ? '' : linkOf(t), all = href ? withLink(items, href) : items;
      return normalize(all).some((x) => x.id) ? all : null; // nothing to offer: the browser's menu
    };
    const focusOf = (t) => up(t, (x) => x === el || (x.tabIndex >= 0 && x.focus)) || el;
    const onMenu = (e) => {
      if (S.pass > now()) { S.pass = 0; return; } // "Browser menu" was picked
      if (e.shiftKey && e.button === 2) return; // Shift + right-click: the browser's own
      if (textField(e.target) || selected(el)) return;
      if (now() - S.lpAt < 900 || now() - S.kbAt < 500) { e.preventDefault(); return; } // already opened by the long-press / the key
      const items = build(e.target, e);
      if (!items) return;
      e.preventDefault(); if (e.stopPropagation) e.stopPropagation(); stopLp(); // a touch browser that also sends contextmenu on a long-press: opened once
      let x = e.clientX, y = e.clientY;
      if (!x && !y && e.target.getBoundingClientRect) { const r = e.target.getBoundingClientRect(); x = r.left + 12; y = r.bottom; } // from the keyboard
      open(items, x, y, pick, { returnFocus: focusOf(e.target), className: o.className, label: o.label, focusFirst: e.button !== 2 && !e.clientX && !e.clientY });
    };
    const onKey = (e) => {
      if (!(e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) || textField(e.target)) return;
      const items = build(e.target, e); if (!items) return;
      e.preventDefault(); S.kbAt = now();
      const r = e.target.getBoundingClientRect();
      open(items, r.left + 12, r.bottom, pick, { returnFocus: focusOf(e.target), className: o.className, label: o.label, focusFirst: true });
    };
    let lp = null;
    const stopLp = () => { if (lp) { cancel(lp.t); lp = null; } };
    const onDown = (e) => {
      if (e.pointerType !== 'touch' && e.pointerType !== 'pen') return;
      stopLp(); if (textField(e.target)) return;
      const t = e.target, x = e.clientX, y = e.clientY;
      lp = { x, y, t: later(() => {
        lp = null; const items = build(t, e); if (!items) return;
        S.lpAt = now(); S.eatClick = now();
        open(items, x, y, pick, { returnFocus: focusOf(t), className: o.className, label: o.label });
      }, o.longPress || LONG_PRESS) };
    };
    const onMove = (e) => { if (lp && (Math.abs(e.clientX - lp.x) > MOVE_SLOP || Math.abs(e.clientY - lp.y) > MOVE_SLOP)) stopLp(); };
    const onClick = (e) => {
      if (now() - S.eatClick < 800) { S.eatClick = 0; e.preventDefault(); if (e.stopPropagation) e.stopPropagation(); return; } // the click that ends a long-press opens nothing else
      const more = o.more && o.more(e.target);
      if (!more) return;
      e.preventDefault(); if (e.stopPropagation) e.stopPropagation();
      clickMore(more, (t) => build(t, e), pick, e, o);
    };
    const on = [['contextmenu', onMenu, false], ['keydown', onKey, false], ['pointerdown', onDown, false], ['pointermove', onMove, false], ['pointerup', stopLp, false], ['pointercancel', stopLp, false], ['click', onClick, true]];
    for (const [k, f, c] of on) el.addEventListener(k, f, c);
    return () => { for (const [k, f, c] of on) el.removeEventListener(k, f, c); };
  }
  // the ⋯ button: under it; a second click on it closes the menu
  function clickMore(btn, build, pick, e, o) {
    if (S.outside && S.outside.at > now() - 400 && (S.outside.t === btn || (btn.contains && btn.contains(S.outside.t)))) { S.outside = null; return; }
    const items = build(btn); if (!items) return;
    const r = btn.getBoundingClientRect();
    open(items, r.left, r.bottom, pick, { returnFocus: btn, className: o && o.className, label: o && o.label, focusFirst: !e || e.detail === 0 });
  }
  function button(btn, itemsFn, pick, opts) {
    if (!btn || !btn.addEventListener) return () => {};
    btn.setAttribute('aria-haspopup', 'menu');
    const f = (e) => { e.preventDefault(); if (e.stopPropagation) e.stopPropagation(); clickMore(btn, (t) => itemsFn(t, e), pick, e, opts); };
    btn.addEventListener('click', f);
    return () => btn.removeEventListener('click', f);
  }

  // ── VibeRoller's lists (T-224): the page's own actions in `a`; see the header for what each offers ──
  const can = (a, k) => !!a && typeof a[k] === 'function';
  const act = (a, k, item, ...args) => (can(a, k) ? { id: k, ...item, run: () => a[k](...args) } : null);
  const lists = {
    message(m, a) {
      const live = !m.deleted, readable = live && !m.locked;
      return [live && act(a, 'reply', { label: 'Reply', icon: 'reply' }),
        readable && m.hasText && act(a, 'copy', { label: 'Copy text', icon: 'file' }),
        m.mine && readable && !m.audio && act(a, 'edit', { label: 'Edit', icon: 'edit' }),
        live && m.canPin && act(a, 'pin', { label: m.pinned ? 'Unpin' : 'Pin', icon: 'pin' }, !m.pinned),
        live && m.replyTo && act(a, 'jump', { label: 'Go to the replied message', icon: 'reply' }),
        '-', m.profile && act(a, 'profile', { label: 'Profile of ' + (m.author || 'them'), icon: 'user' }),
        live && (m.mine || (!m.private && m.canModerate)) && act(a, 'del', { label: m.mine ? 'Delete' : 'Delete (moderator)', icon: 'trash', danger: true, confirm: 'Click again to delete' }),
        m.info && '-', m.info && { note: m.info }];
    },
    person(p, a) {
      return [!p.agent && act(a, 'profile', { label: 'Profile', icon: 'user' }),
        !p.self && act(a, 'message', { label: 'Message', icon: 'chat' }),
        !p.self && act(a, 'mention', { label: 'Mention @' + p.user, icon: 'users' }),
        act(a, 'copy', { label: 'Copy @' + p.user }),
        p.server && '-', p.server && act(a, 'member', { label: 'Member settings…', icon: 'shield' })];
    },
    channel(c, a) {
      return [!c.current && act(a, 'open', { label: 'Open', icon: 'hash' }),
        c.canEdit && act(a, 'edit', { label: 'Edit channel…', icon: 'edit' }),
        act(a, 'server', { label: 'Server settings…', icon: 'gear' })];
    },
    group(g, a) { return [!g.current && act(a, 'open', { label: 'Open', icon: 'users' }), act(a, 'settings', { label: 'Group settings…', icon: 'gear' })]; },
    server(s, a) {
      return [!s.current && act(a, 'open', { label: 'Open', icon: 'server' }),
        act(a, 'settings', { label: 'Server settings…', icon: 'gear' }),
        s.canInvite && act(a, 'invite', { label: 'Invite people…', icon: 'plus' }),
        s.canCreate && act(a, 'newChannel', { label: 'New channel…', icon: 'hash' })];
    },
    card(t, c, a) {
      const free = !c.held;
      return [act(a, 'open', { label: 'Open', icon: 'board' }),
        free && act(a, 'claim', { label: c.mineHeld ? 'Renew 30 min' : 'Claim 30 min', icon: 'flag' }),
        free && c.mineHeld && act(a, 'release', { label: 'Release', icon: 'close' }),
        free && t.status !== 'done' && act(a, 'complete', { label: t.recur ? 'Complete (repeats)' : 'Complete', icon: 'check' }),
        free && can(a, 'move') && { id: 'move', label: 'Move to', icon: 'board', sub: (c.statuses || []).map((s) => ({ id: 'move:' + s.id, label: s.label, on: s.id === t.status, run: () => a.move(s.id) })) },
        free && can(a, 'assign') && { id: 'assign', label: 'Assign to', icon: 'user', sub: [{ id: 'assign:', label: 'nobody', on: !t.assignee, run: () => a.assign(null) }, ...(c.members || []).map((m) => ({ id: 'assign:' + m.username, label: m.name || m.username, on: t.assignee === m.username, run: () => a.assign(m.username) }))] },
        free && act(a, 'delegate', { label: 'Delegate…', icon: 'share' }),
        free && t.archivedAt && act(a, 'restore', { label: 'Restore', icon: 'reply' }),
        free && !t.archivedAt && t.status === 'done' && c.archive && act(a, 'archive', { label: 'Archive', icon: 'archive' }),
        '-', c.ai && act(a, 'claude', { label: 'Give to Claude', icon: 'bot' }),
        act(a, 'copy', { label: 'Copy id (' + t.id + ')' }),
        free && '-', free && act(a, 'del', { label: 'Delete', icon: 'trash', danger: true, confirm: 'Click again to delete' })];
    },
    note(n, a) {
      return [!n.open && act(a, 'open', { label: 'Open', icon: 'file' }), act(a, 'rename', { label: 'Rename or move…', icon: 'edit' }),
        act(a, 'copy', { label: 'Copy path' }), '-', act(a, 'del', { label: 'Delete', icon: 'trash', danger: true, confirm: 'Click again to delete' })];
    },
    noteFolder(f, a) {
      return [act(a, 'newNote', { label: 'New note here…', icon: 'plus' }), act(a, 'toggle', { label: f.collapsed ? 'Expand' : 'Collapse', icon: 'folder' }), act(a, 'newFolder', { label: 'New folder…', icon: 'folder' })];
    },
    file(f, a) {
      const ok = !f.missing && !f.locked, work = ok && f.kind === 'work';
      return [ok && act(a, 'open', { label: 'Open', icon: 'file' }), ok && act(a, 'download', { label: 'Download', icon: 'download' }),
        work && f.editable && act(a, 'edit', { label: 'Edit', icon: 'edit' }),
        work && act(a, 'use', { label: 'Use in Work', icon: 'plus' }), work && act(a, 'run', { label: 'Run in Work', icon: 'play' }),
        work && act(a, 'exec', { label: f.exec ? 'Make not executable' : 'Make executable', icon: f.exec ? 'warn' : 'check' }),
        ok && f.kind === 'chat' && f.linked && act(a, 'copyIn', { label: 'Copy to my uploaded_files', icon: 'plus' }),
        '-', act(a, 'del', { label: 'Delete for good', icon: 'trash', danger: true, confirm: 'Delete for good?' })];
    },
    work(w, a) {
      const q = w.kind === 'queued';
      return [w.text && act(a, 'copy', { label: 'Copy', icon: 'file' }),
        w.kind === 'me' && w.text && act(a, 'resend', { label: 'Send again', icon: 'send' }),
        q && act(a, 'queue', { label: 'Show the queue', icon: 'tasks' }),
        q && '-', q && act(a, 'cancel', { label: 'Cancel (not sent)', icon: 'close', danger: true })];
    },
    saved(s, a) {
      return [act(a, 'open', { label: 'Open here', icon: 'archive' }), act(a, 'rename', { label: 'Rename…', icon: 'edit' }),
        '-', act(a, 'del', { label: 'Delete', icon: 'trash', danger: true, confirm: 'Click again to delete' })];
    },
  };

  W.MENU = { open, attach, button, close: () => close(true), isOpen: () => S.levels.length > 0, copy, passNext, place, subPlace, normalize, textField, linkOf, lists };
})();
