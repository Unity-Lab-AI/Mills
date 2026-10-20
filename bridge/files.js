// VibeRoller attachments: files, pictures and videos sent with a message, in the team chat and in Work.
//
// Chat (end-to-end encrypted, like the messages): the browser encrypts every file with its own random key and uploads
// only the ciphertext (POST /api/room/file); the file's name, type, size and key travel INSIDE the encrypted message
// body, so this hub never sees what a file is or holds. The message names its files' ids in the clear (files: [...])
// only so the hub knows who may fetch them: whoever may read that message (GET /api/room/file/<id>). Deleting the
// message deletes its files; files uploaded but never sent are removed after a day. Store: Chat/files/<id> + <id>.json.
//
// Work: files go to the person's own upload area on the hub (bridge/profiles/<user>/uploads/<id>/<name>, POST
// /api/work/file), where their Claude can read them (the run gets --add-dir for that folder and the paths in the
// message). GET /api/work/file/<id> hands them back to their owner for the conversation on screen. Kept 30 days.
//
// Both: up to MAX_FILE per file, sent as the raw request body (no base64), never served with a type a browser would run
// (application/octet-stream + nosniff + attachment; the page shows pictures / video from a blob of the type it knows).
'use strict';
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const MAX_FILE = 24 * 1024 * 1024; // nginx lets 25 MB through (client_max_body_size 25m): room for the encryption's few bytes
const ID_RE = /^[a-f0-9]{24}$/;
const DAY = 86400e3;

// the raw request body, at most `limit` bytes: too big → an error with status 413 once the rest has been read and
// dropped (answering mid-upload would cut the connection, and the sender would see a network error, not the reason)
function readRaw(req, limit = MAX_FILE) {
  return new Promise((resolve, reject) => {
    const parts = []; let n = 0, over = false;
    req.on('data', (c) => { n += c.length; if (over) return; if (n > limit) { over = true; parts.length = 0; return; } parts.push(c); });
    req.on('end', () => { if (over) reject(Object.assign(new Error(`That file is too large (up to ${Math.round(limit / 1048576)} MB).`), { status: 413 })); else resolve(Buffer.concat(parts)); });
    req.on('error', reject);
  });
}
// a file name that is safe on every system: no folders, no control or reserved characters, not empty, not too long
function safeName(raw) {
  let n = '';
  try { n = decodeURIComponent(String(raw || '')); } catch { n = String(raw || ''); }
  n = path.basename(n.replace(/\\/g, '/')).replace(/[\u0000-\u001f<>:"/\\|?*\u007f]/g, '_').replace(/^\.+/, '').trim().slice(0, 120);
  return n || 'file';
}
const cleanMime = (m) => (/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/i.test(String(m || '').split(';')[0].trim()) ? String(m).split(';')[0].trim().toLowerCase() : 'application/octet-stream');
function serve(res, buf, name, mime) {
  res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': buf.length, 'Content-Disposition': `attachment; filename="${encodeURIComponent(name || 'file')}"`, 'X-VR-File-Type': mime || 'application/octet-stream', 'Cache-Control': 'private, no-store', 'X-Content-Type-Options': 'nosniff' });
  res.end(buf);
}

module.exports = function createFiles({ CHAT_DIR, send, log = console.log }) {
  // ---------- chat: ciphertext only ----------
  const DIR = path.join(CHAT_DIR, 'files');
  fs.mkdirSync(DIR, { recursive: true });
  const meta = (id) => { try { return JSON.parse(fs.readFileSync(path.join(DIR, id + '.json'), 'utf8')); } catch { return null; } };
  const setMeta = (id, m) => fs.writeFileSync(path.join(DIR, id + '.json'), JSON.stringify(m));
  const chat = {
    async upload(acct, req, res) {
      const buf = await readRaw(req, MAX_FILE + 64); // + the AES-GCM tag
      if (!buf.length) return send(res, 400, { error: 'Empty file.' });
      const id = crypto.randomBytes(12).toString('hex');
      fs.writeFileSync(path.join(DIR, id), buf, { mode: 0o600 });
      setMeta(id, { by: acct.username, at: Date.now(), size: buf.length, seq: 0 });
      return send(res, 200, { id, size: buf.length });
    },
    // the files a message names: the sender's own, uploaded and not sent yet; marked as that message's
    attach(user, ids, seq) {
      if (ids === undefined) return [];
      if (!Array.isArray(ids) || ids.length > 20) throw Object.assign(new Error('Up to 20 files per message.'), { status: 400 });
      for (const id of ids) { const m = ID_RE.test(String(id)) ? meta(id) : null; if (!m || m.by !== user || m.seq) throw Object.assign(new Error('A file of this message is not yours or was sent already.'), { status: 400 }); }
      for (const id of ids) setMeta(id, { ...meta(id), seq });
      return ids.map(String);
    },
    // the ciphertext, for whoever may read the message it belongs to (or its uploader before sending)
    fetch(id, user, mayRead) {
      if (!ID_RE.test(String(id || ''))) return null;
      const m = meta(id); if (!m) return null;
      if (!(m.seq ? mayRead(m.seq) : m.by === user)) return null;
      try { return fs.readFileSync(path.join(DIR, id)); } catch { return null; }
    },
    remove(ids) { for (const id of ids || []) if (ID_RE.test(String(id))) { fs.rmSync(path.join(DIR, id), { force: true }); fs.rmSync(path.join(DIR, id + '.json'), { force: true }); } },
    clear() { for (const f of fs.readdirSync(DIR)) fs.rmSync(path.join(DIR, f), { force: true }); },
    sweep() { // uploaded but never sent within a day
      for (const f of fs.readdirSync(DIR).filter((x) => x.endsWith('.json'))) { const id = f.slice(0, -5), m = meta(id); if (m && !m.seq && Date.now() - m.at > DAY) chat.remove([id]); }
    },
  };

  // ---------- Work: the person's own uploads, readable by their Claude ----------
  const work = {
    dirOf: (acct) => path.join(path.dirname(acct.files.STATE_FILE), 'uploads'),
    async upload(acct, req, res) {
      const buf = await readRaw(req);
      if (!buf.length) return send(res, 400, { error: 'Empty file.' });
      const id = crypto.randomBytes(12).toString('hex'), name = safeName(req.headers['x-file-name']), mime = cleanMime(req.headers['content-type']);
      const dir = path.join(work.dirOf(acct), id);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, name), buf, { mode: 0o600 });
      fs.writeFileSync(path.join(dir, '.meta.json'), JSON.stringify({ name, mime, size: buf.length, at: Date.now() }));
      return send(res, 200, { id, name, mime, size: buf.length });
    },
    // { id, name, mime, size, path } of one of this person's uploads, or null
    get(acct, id) {
      if (!ID_RE.test(String(id || ''))) return null;
      const dir = path.join(work.dirOf(acct), id);
      let m; try { m = JSON.parse(fs.readFileSync(path.join(dir, '.meta.json'), 'utf8')); } catch { return null; }
      const file = path.join(dir, safeName(m.name));
      return fs.existsSync(file) ? { id, name: m.name, mime: m.mime, size: m.size, path: file } : null;
    },
    serve(acct, id, res) { const f = work.get(acct, id); if (!f) return send(res, 404, { error: 'That file is no longer kept.' }); return serve(res, fs.readFileSync(f.path), f.name, f.mime); },
    sweep(accounts) { // older than 30 days
      for (const a of accounts) {
        const root = work.dirOf(a); if (!fs.existsSync(root)) continue;
        for (const id of fs.readdirSync(root)) { try { const m = JSON.parse(fs.readFileSync(path.join(root, id, '.meta.json'), 'utf8')); if (Date.now() - m.at > 30 * DAY) fs.rmSync(path.join(root, id), { recursive: true, force: true }); } catch {} }
      }
    },
  };
  // A Work message carries its files as a marker block at its end (so the queue, the saved conversation and Past sessions
  // keep them with no change): <!--vr-files [{"id","n","m","s"}] -->. For Claude it becomes the list of paths.
  const MARK = /\n*<!--vr-files (\[[\s\S]*?\]) -->\s*$/;
  function workMessage(acct, message) {
    const m = MARK.exec(message); if (!m) return { text: message, files: [] };
    let list = []; try { list = JSON.parse(m[1]); } catch {}
    const files = (Array.isArray(list) ? list : []).slice(0, 20).map((f) => work.get(acct, f && f.id)).filter(Boolean);
    const text = message.slice(0, m.index).trim();
    if (!files.length) return { text, files };
    const lines = files.map((f) => `- ${f.path}  (${f.name}, ${f.mime}, ${f.size} bytes)`);
    return { text: (text || '(no text: see the attached files)') + `\n\n[Attached by the user: ${files.length} file(s). Read them with the Read tool (it reads pictures too):\n${lines.join('\n')}]`, files };
  }
  return { readRaw, chat, work, workMessage, MAX_FILE, serve };
};
