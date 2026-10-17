// Mills past sessions: each account's saved Claude conversations, one JSON file each under the
// account's Archive folder (Mills/Archive[/<name>]/<id>.json, never committed). Save / list / search /
// open / rename / delete, all per account; a search looks through titles and whole transcripts.
'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

module.exports = function createArchive({ readJson, writeJson, send, readBody, saveState }) {
  const file = (acct, id) => {
    if (typeof id !== 'string' || !/^[a-f0-9]{16}$/.test(id)) throw new Error('Bad conversation id.');
    return path.join(acct.files.ARCHIVE_DIR, id + '.json');
  };
  const cleanTitle = (t) => String(t || '').replace(/\s+/g, ' ').trim().slice(0, 120) || 'Untitled conversation';
  function list(acct) {
    const dir = acct.files.ARCHIVE_DIR;
    if (!fs.existsSync(dir)) return [];
    return fs.readdirSync(dir).filter((f) => f.endsWith('.json'))
      .map((f) => readJson(path.join(dir, f), null)).filter(Boolean)
      .map(({ id, title, savedAt, transcript }) => ({ id, title, savedAt, count: (transcript || []).length }))
      .sort((a, b) => String(b.savedAt).localeCompare(String(a.savedAt)));
  }
  // Full-text search over this account's saved sessions: every word must appear in the title or
  // somewhere in the transcript (case-insensitive). Each hit comes with short snippets around the
  // first matches, so the list shows *why* a session matched.
  function search(acct, q) {
    const words = String(q || '').toLowerCase().split(/\s+/).filter(Boolean).slice(0, 8);
    if (!words.length) return list(acct);
    const dir = acct.files.ARCHIVE_DIR;
    if (!fs.existsSync(dir)) return [];
    const out = [];
    for (const f of fs.readdirSync(dir).filter((x) => x.endsWith('.json'))) {
      const e = readJson(path.join(dir, f), null);
      if (!e) continue;
      const lines = (e.transcript || []).filter((t) => t && typeof t.text === 'string');
      const hay = (e.title + '\n' + lines.map((t) => t.text).join('\n')).toLowerCase();
      if (!words.every((w) => hay.includes(w))) continue;
      const snippets = [];
      let hits = 0;
      for (const t of lines) {
        const low = t.text.toLowerCase();
        for (const w of words) {
          let i = low.indexOf(w);
          while (i >= 0) { hits++; i = low.indexOf(w, i + w.length); }
        }
        const first = Math.min(...words.map((w) => low.indexOf(w)).filter((i) => i >= 0));
        if (Number.isFinite(first) && snippets.length < 3) {
          const start = Math.max(0, first - 60);
          snippets.push({ cls: t.cls, text: (start ? '…' : '') + t.text.slice(start, first + 90).replace(/\s+/g, ' ') + (first + 90 < t.text.length ? '…' : '') });
        }
      }
      out.push({ id: e.id, title: e.title, savedAt: e.savedAt, count: lines.length, hits, snippets });
    }
    return out.sort((a, b) => b.hits - a.hits || String(b.savedAt).localeCompare(String(a.savedAt)));
  }
  // The /api/archive/* routes for a signed-in account; true when handled.
  async function route(p, req, res, acct) {
    if (!p.startsWith('/api/archive/') || req.method !== 'POST') return false;
    if (p === '/api/archive/list') { send(res, 200, { items: list(acct) }); return true; }
    if (p === '/api/archive/search') { const { q } = await readBody(req); send(res, 200, { items: search(acct, String(q || '').slice(0, 200)) }); return true; }
    if (p === '/api/archive/save') {
      const { id, title, transcript } = await readBody(req);
      if (!Array.isArray(transcript) || !transcript.length) { send(res, 400, { error: 'Nothing to save yet.' }); return true; }
      if (!acct.state.claudeSessionId) { send(res, 400, { error: 'This conversation has no Claude session yet.' }); return true; }
      const entryId = id || crypto.randomBytes(8).toString('hex');
      const f = file(acct, entryId);
      const prev = readJson(f, {});
      fs.mkdirSync(acct.files.ARCHIVE_DIR, { recursive: true });
      const entry = {
        id: entryId, title: cleanTitle(title || prev.title), savedAt: new Date().toISOString(),
        sessionId: acct.state.claudeSessionId,
        transcript: transcript.filter((t) => t && typeof t.cls === 'string' && typeof t.text === 'string'),
      };
      writeJson(f, entry);
      send(res, 200, { id: entry.id, title: entry.title }); return true;
    }
    if (p === '/api/archive/open') {
      if (acct.running) { send(res, 409, { error: 'Claude is working. Stop it first.' }); return true; }
      const entry = readJson(file(acct, (await readBody(req)).id), null);
      if (!entry) { send(res, 404, { error: 'Conversation not found.' }); return true; }
      acct.state.claudeSessionId = entry.sessionId; saveState(acct);
      send(res, 200, entry); return true;
    }
    if (p === '/api/archive/rename') {
      const { id, title } = await readBody(req);
      const f = file(acct, id);
      const entry = readJson(f, null);
      if (!entry) { send(res, 404, { error: 'Conversation not found.' }); return true; }
      entry.title = cleanTitle(title); writeJson(f, entry);
      send(res, 200, { id, title: entry.title }); return true;
    }
    if (p === '/api/archive/delete') {
      const f = file(acct, (await readBody(req)).id);
      if (fs.existsSync(f)) fs.unlinkSync(f);
      send(res, 200, { ok: true }); return true;
    }
    return false;
  }
  return { list, search, route };
};
