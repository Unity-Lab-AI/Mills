// Mills presence, hub-wide: every signed-in page keeps one long poll open, so the hub knows exactly who
// is connected. A person is online while they have a poll open, idle when their page says so (tab
// hidden or no input for 5 minutes), and offline a few seconds after their last connection closes (or
// at once when the page reports it is going away). Each change is pushed to everyone through the room's
// long poll. Typing is per conversation and only shown to that conversation's members; an entry lapses
// after 6 s unless the browser renews it (it renews every 2.5 s while keys are pressed).
'use strict';

const TYPING_MS = 6000, GONE_MS = 8000, ONLINE_MS = 70000; // ONLINE_MS: fallback if a close is never seen

module.exports = function createPresence({ accounts, roomChanged, extraTyping = () => [], convMember = null }) {
  const presence = new Map();   // username -> { online, idle, screen, at (last change), seen (last contact), polls }
  const typing = new Map();     // username -> { conv, at, timer }
  function pres(user) { let p = presence.get(user); if (!p) { p = { online: false, idle: false, screen: '', at: 0, seen: 0, polls: 0 }; presence.set(user, p); } return p; }
  function set(user, patch) {
    const p = pres(user), before = JSON.stringify([p.online, p.idle, p.screen]);
    Object.assign(p, patch); p.seen = Date.now();
    if (JSON.stringify([p.online, p.idle, p.screen]) !== before) { p.at = Date.now(); roomChanged(); }
  }
  const touch = (user) => { pres(user).seen = Date.now(); }; // any contact, no state change
  function pollOpened(user) { const p = pres(user); p.polls++; clearTimeout(p.gone); set(user, { online: true }); }
  function pollClosed(user) { // offline once no connection has been open for GONE_MS
    const p = pres(user); p.polls = Math.max(0, p.polls - 1);
    if (p.polls) return;
    clearTimeout(p.gone);
    p.gone = setTimeout(() => { if (!p.polls) set(user, { online: false, idle: false }); }, GONE_MS);
    p.gone.unref();
  }
  function gone(user) { const q = pres(user); q.polls = 0; clearTimeout(q.gone); set(user, { online: false, idle: false }); } // offline right away (sign-out, pagehide)
  function list() {
    const now = Date.now(), out = [];
    for (const [u, p] of presence) {
      if (!accounts.has(u)) continue;
      if (p.online && now - p.seen > ONLINE_MS) { p.online = false; p.idle = false; p.at = now; } // never saw the close
      out.push({ user: u, online: p.online, idle: p.online && p.idle, screen: p.online ? p.screen : '', at: p.at, seen: p.seen });
    }
    return out;
  }
  const online = () => list().filter((p) => p.online).map((p) => p.user);
  function setTyping(user, conv, on) {
    const prev = typing.get(user);
    if (prev) clearTimeout(prev.timer);
    if (!on) { typing.delete(user); if (prev) roomChanged(); return; }
    const timer = setTimeout(() => { typing.delete(user); roomChanged(); }, TYPING_MS);
    timer.unref();
    typing.set(user, { conv, at: Date.now(), timer });
    if (!prev || prev.conv !== conv) roomChanged();
  }
  function typingFor(user) { // conversations this reader is in, plus whatever the hub adds (Unity composing)
    const out = [];
    const inConv = (c) => (convMember ? convMember(c, user) : c === 'room' || c.split(/[:|]/).includes(user));
    for (const [u, t] of typing) if (u !== user && inConv(t.conv)) out.push({ user: u, conv: t.conv });
    return out.concat(extraTyping(user));
  }
  return { set, touch, pollOpened, pollClosed, gone, list, online, typing: setTyping, typingFor };
};
