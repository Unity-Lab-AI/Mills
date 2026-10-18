// Mills Work state per person: the conversation on screen and the message queue, kept on the hub so both
// survive hub restarts, page reloads, closing the browser and switching devices.
//
// Stored in the person's own profile folder as work-current.json (next to state.json; never deleted, only
// rewritten): { transcript: [{ cls, text }], archiveId, archiveTitle, queue: [text], sendNow, updatedAt }.
//   transcript  what Work shows (cls: me / claude / tool), the same shape Past sessions keep; opening Work
//               puts it back on screen. Claude's own session id stays in state.json as before, so Claude
//               continues the same conversation.
//   queue       messages typed while Claude was busy, in order; the page runs them one after another and
//               saves every change (queue, remove, run, Send now), so a reload or a restart loses none.
// Routes: GET /api/work/current, POST /api/work/current { transcript, archiveId, archiveTitle },
//         POST /api/work/queue { queue, sendNow }.
'use strict';

const fs = require('fs');
const path = require('path');

const MAX_ITEMS = 3000, MAX_TEXT = 200 * 1024, MAX_TOTAL = 6 * 1024 * 1024, MAX_QUEUE = 50, MAX_QUEUED = 50 * 1024;
const CLASSES = ['me', 'claude', 'tool'];

module.exports = function createWorkState({ send, readBody }) {
  const fileOf = (acct) => path.join(path.dirname(acct.files.STATE_FILE), 'work-current.json');
  const cache = new Map(); // username -> state
  function load(acct) {
    if (cache.has(acct.username)) return cache.get(acct.username);
    let s = null;
    try { s = JSON.parse(fs.readFileSync(fileOf(acct), 'utf8')); } catch {}
    if (!s || typeof s !== 'object') s = {};
    s = { transcript: Array.isArray(s.transcript) ? s.transcript : [], archiveId: s.archiveId || null, archiveTitle: s.archiveTitle || null,
      queue: Array.isArray(s.queue) ? s.queue : [], sendNow: s.sendNow || null, updatedAt: s.updatedAt || null };
    cache.set(acct.username, s); return s;
  }
  function save(acct, s) {
    s.updatedAt = new Date().toISOString();
    const f = fileOf(acct), tmp = f + '.tmp';
    fs.writeFileSync(tmp, JSON.stringify(s)); fs.renameSync(tmp, f); // whole file or nothing
  }
  function cleanTranscript(list) {
    if (!Array.isArray(list)) throw new Error('transcript must be a list.');
    let total = 0;
    const out = list.slice(-MAX_ITEMS).filter((t) => t && CLASSES.includes(t.cls) && typeof t.text === 'string').map((t) => ({ cls: t.cls, text: t.text.slice(0, MAX_TEXT) }));
    for (let i = out.length - 1; i >= 0; i--) { total += out[i].text.length; if (total > MAX_TOTAL) return out.slice(i + 1); } // keep the newest that fit
    return out;
  }
  const cleanMsg = (m) => (typeof m === 'string' && m.trim() ? m.slice(0, MAX_QUEUED) : null);
  async function route(p, req, res, acct) {
    if (p === '/api/work/current' && req.method === 'GET') { const s = load(acct); send(res, 200, s); return true; }
    if (p === '/api/work/current' && req.method === 'POST') {
      const b = await readBody(req), s = load(acct);
      try { s.transcript = cleanTranscript(b.transcript); } catch (e) { send(res, 400, { error: e.message }); return true; }
      s.archiveId = typeof b.archiveId === 'string' && b.archiveId ? b.archiveId.slice(0, 80) : null;
      s.archiveTitle = typeof b.archiveTitle === 'string' && b.archiveTitle ? b.archiveTitle.slice(0, 200) : null;
      save(acct, s); send(res, 200, { ok: true, items: s.transcript.length }); return true;
    }
    if (p === '/api/work/queue' && req.method === 'POST') {
      const b = await readBody(req), s = load(acct);
      if (!Array.isArray(b.queue)) { send(res, 400, { error: 'queue must be a list.' }); return true; }
      s.queue = b.queue.map(cleanMsg).filter(Boolean).slice(0, MAX_QUEUE);
      s.sendNow = cleanMsg(b.sendNow);
      save(acct, s); send(res, 200, { ok: true, queue: s.queue.length }); return true;
    }
    return false;
  }
  return { route, load, MAX_QUEUE };
};
