// Mills voice / video calls, hub side. The hub only passes call set-up messages between browsers and
// keeps track of who is in which call (with cam / mic / screen flags). Audio, video and the shared
// screen go browser-to-browser over WebRTC (DTLS-SRTP). The set-up messages themselves (SDP with the
// DTLS fingerprints, ICE candidates) are encrypted with the same per-pair ECDH keys as the chat, so the
// hub can neither read them nor swap in its own fingerprints: the media is end-to-end encrypted, not
// just encrypted to the hub. One call per conversation ("room" or a private "dm:a|b"); a person is in
// at most one call at a time. Routes: /api/call/join | leave | state | signal | poll (long poll).
'use strict';

const ICE_DEFAULT = [{ urls: 'stun:stun.l.google.com:19302' }]; // finds each browser's public address; sees no media

// extraMember(conv, user) / extraConv(conv): membership and existence of the server-channel and group
// conversations (bridge/guilds.js); room and private conversations are judged here.
// media(conv, user) → { connect, speak, stream, muted }: server permissions (bridge/guilds.js). Without connect the
// hub refuses the join; without speak (or while server-muted) the mic flag is held off, without stream cam + screen.
// The flags tell every browser what to show and the person's own page turns the mic / camera off itself; the media
// goes browser-to-browser, so the hub cannot cut it, it can only refuse the call.
module.exports = function createCalls({ accounts, config, chatKeys, send, readBody, b64, extraMember = () => false, extraConv = () => false, media = () => ({ connect: true, speak: true, stream: true, muted: false }) }) {
  const calls = new Map();   // conv -> Map(username -> { cam, mic, screen, since, seen })
  const mailbox = new Map(); // username -> [{ n, from, conv, iv, ct }]
  const waiters = new Map(); // username -> Set(wake fn) of open long-polls
  let callN = 0, callVer = 0;
  const convMember = (conv, user) => conv === 'room' || (conv.startsWith('dm:') && conv.slice(3).split('|').includes(user)) || extraMember(conv, user);
  function validConv(conv) {
    if (conv === 'room') return true;
    const m = /^dm:([a-z0-9-]{1,32})\|([a-z0-9-]{1,32})$/.exec(String(conv || ''));
    if (m) return m[1] < m[2] && accounts.has(m[1]) && accounts.has(m[2]);
    return extraConv(conv);
  }
  function wake(users) { for (const u of users) for (const fn of waiters.get(u) || []) fn(); }
  function changed(conv) { callVer++; wake([...accounts.keys()].filter((u) => convMember(conv, u))); }
  function state(user) { // the calls this person may see, with who is in them
    const out = {};
    for (const [c, ps] of calls) if (ps.size && convMember(c, user)) out[c] = [...ps].map(([u, p]) => { const md = media(c, u), held = !md.speak || md.muted;
      return { user: u, cam: p.cam && md.stream, mic: p.mic && !held, screen: !!p.screen && md.stream, since: p.since, ...(held ? { held: md.muted ? 'muted' : 'nospeak' } : {}), ...(md.stream ? {} : { nostream: true }) }; });
    return out;
  }
  function leave(user) { // out of every call (sign-out, account deleted, Leave)
    for (const [c, ps] of calls) if (ps.delete(user)) { if (!ps.size) calls.delete(c); changed(c); }
    mailbox.delete(user);
  }
  setInterval(() => { // browsers long-poll at least every 25 s while in a call; 60 s of silence = gone
    const now = Date.now();
    for (const [c, ps] of calls) for (const [u, p] of ps) if (now - p.seen > 60000) { ps.delete(u); if (!ps.size) calls.delete(c); changed(c); }
  }, 10000).unref();

  async function route(p, req, res, acct, url) {
    if (!p.startsWith('/api/call/')) return false;
    if (p === '/api/call/join' && req.method === 'POST') {
      const { conv, cam, mic, screen } = await readBody(req);
      if (!validConv(conv) || !convMember(conv, acct.username)) { send(res, 400, { error: 'Unknown conversation.' }); return true; }
      if (!chatKeys[acct.username]) { send(res, 400, { error: 'Your chat key is not set up yet.' }); return true; }
      if (!media(conv, acct.username).connect) { send(res, 403, { error: 'You may not join calls here (no "connect" permission, or you are timed out).' }); return true; }
      for (const [c, ps] of calls) if (c !== conv && ps.delete(acct.username)) { if (!ps.size) calls.delete(c); changed(c); }
      const ps = calls.get(conv) || new Map(); calls.set(conv, ps);
      const prev = ps.get(acct.username);
      ps.set(acct.username, { cam: !!cam, mic: !!mic, screen: !!screen, since: prev ? prev.since : new Date().toISOString(), seen: Date.now() });
      changed(conv);
      send(res, 200, { calls: state(acct.username), ver: callVer, iceServers: Array.isArray(config.iceServers) ? config.iceServers : ICE_DEFAULT }); return true;
    }
    if (p === '/api/call/leave' && req.method === 'POST') { leave(acct.username); send(res, 200, { ok: true }); return true; }
    if (p === '/api/call/state' && req.method === 'POST') {
      const { conv, cam, mic, screen } = await readBody(req);
      const mine = calls.get(conv) && calls.get(conv).get(acct.username);
      if (!mine) { send(res, 409, { error: 'You are not in that call.' }); return true; }
      Object.assign(mine, { cam: !!cam, mic: !!mic, screen: !!screen, seen: Date.now() });
      changed(conv);
      send(res, 200, { ok: true }); return true;
    }
    if (p === '/api/call/signal' && req.method === 'POST') {
      const { to, conv, iv, ct } = await readBody(req);
      const ps = calls.get(conv);
      if (!ps || !ps.has(acct.username) || !ps.has(to) || to === acct.username) { send(res, 409, { error: 'Both people must be in this call.' }); return true; }
      if (!b64(iv, 64) || !b64(ct, 200000)) { send(res, 400, { error: 'Bad signal.' }); return true; }
      const box = mailbox.get(to) || []; mailbox.set(to, box);
      box.push({ n: ++callN, from: acct.username, conv, iv, ct });
      if (box.length > 500) box.splice(0, box.length - 500);
      wake([to]);
      send(res, 200, { ok: true }); return true;
    }
    if (p === '/api/call/poll' && req.method === 'GET') {
      // Long poll: answers at once when there are signals after `after` or the call list changed
      // since `ver`, otherwise waits up to 25 s. Doubles as the "still here" heartbeat for calls.
      const after = Number(url.searchParams.get('after')) || 0, ver = Number(url.searchParams.get('ver'));
      const user = acct.username;
      const box = mailbox.get(user) || [];
      if (box.length) mailbox.set(user, box.filter((s) => s.n > after)); // delivered ones can go
      const answer = () => {
        for (const ps of calls.values()) if (ps.has(user)) ps.get(user).seen = Date.now();
        const sigs = (mailbox.get(user) || []).filter((s) => s.n > after);
        send(res, 200, { signals: sigs, after: sigs.length ? sigs[sigs.length - 1].n : after, calls: state(user), ver: callVer });
      };
      if ((mailbox.get(user) || []).some((s) => s.n > after) || ver !== callVer) { answer(); return true; }
      let done = false;
      const set = waiters.get(user) || new Set(); waiters.set(user, set);
      const finish = () => { if (done) return; done = true; clearTimeout(timer); set.delete(finish); answer(); };
      const timer = setTimeout(finish, 25000);
      set.add(finish);
      req.on('close', () => { if (!done) { done = true; clearTimeout(timer); set.delete(finish); } });
      return true;
    }
    return false;
  }
  return { convMember, validConv, state, leave, route };
};
