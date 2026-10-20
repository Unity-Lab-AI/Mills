// VibeRoller attachments and everyone's uploaded files: files, pictures and videos sent with a message, in the team chat
// and in Work, and the "Files" library where each person sees, opens, downloads and deletes everything they uploaded.
//
// Chat (end-to-end encrypted, like the messages): the browser encrypts every file with its own random key and uploads
// only the ciphertext (POST /api/room/file); the file's name, type, size and key travel INSIDE the encrypted message body,
// so this hub never sees what a file is or holds. The message names its files' ids in the clear (files: [...]) only so
// the hub knows who may fetch them: whoever may read that message (GET /api/room/file/<id>). The uploader's browser also
// hands over the same name / type / key sealed to the uploader alone (X-VR-Own-Seal: only their chat key opens it), so
// their library can still open the file after the message is gone. Store: Chat/files/<id> (+ <id>.json).
//
// Work: files go to the person's own uploaded_files folder in their Linux account on the server when they linked it
// (bridge/linux-accounts.js; ~/uploaded_files/<date>/<name>, theirs to work on, edit, run), else to the hub's own area
// for them (bridge/profiles/<user>/uploads). Their Claude gets the paths and read access (--add-dir). GET
// /api/work/file/<id> hands a file back to its owner.
//
// The library (GET /api/library, POST /api/library/delete): per person, every upload with when, its name / type / size
// (Work; for chat files only the person's browser can read those), where it went (Work, or which chat conversation and
// message, and whether that message was deleted since), and when it was last opened or used. Deleting from the library
// deletes the file for good (a chat message that still names it shows it as gone). Index: <profile>/uploads-index.json.
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
// a name that is free in a folder: "notes.md", "notes (2).md", …
function freeName(dir, name) {
  if (!fs.existsSync(path.join(dir, name))) return name;
  const ext = path.extname(name), base = name.slice(0, name.length - ext.length);
  for (let i = 2; i < 10000; i++) { const n = `${base} (${i})${ext}`; if (!fs.existsSync(path.join(dir, n))) return n; }
  return crypto.randomBytes(4).toString('hex') + '-' + name;
}

module.exports = function createFiles({ CHAT_DIR, send, accountOf, linux = null }) {
  // ---------- the library index, per person ----------
  const profileDir = (acct) => path.dirname(acct.files.STATE_FILE);
  const indexFile = (acct) => path.join(profileDir(acct), 'uploads-index.json');
  const readIndex = (acct) => { try { const j = JSON.parse(fs.readFileSync(indexFile(acct), 'utf8')); return j && typeof j.entries === 'object' ? j : { entries: {} }; } catch { return { entries: {} }; } };
  function updateIndex(acct, fn) { const ix = readIndex(acct); fn(ix.entries); const f = indexFile(acct); fs.writeFileSync(f + '.tmp', JSON.stringify(ix)); fs.renameSync(f + '.tmp', f); }
  const touch = (acct, ids, use) => { if (!acct || !ids.length) return; updateIndex(acct, (e) => { for (const id of ids) if (e[id]) { e[id].lastAccess = Date.now(); if (use) (e[id].uses = e[id].uses || []).push({ at: Date.now(), ...use }), e[id].uses = e[id].uses.slice(-50); } }); };

  // ---------- chat: ciphertext only ----------
  const DIR = path.join(CHAT_DIR, 'files');
  fs.mkdirSync(DIR, { recursive: true });
  const meta = (id) => { try { return JSON.parse(fs.readFileSync(path.join(DIR, id + '.json'), 'utf8')); } catch { return null; } };
  const setMeta = (id, m) => fs.writeFileSync(path.join(DIR, id + '.json'), JSON.stringify(m));
  const removeCipher = (id) => { fs.rmSync(path.join(DIR, id), { force: true }); fs.rmSync(path.join(DIR, id + '.json'), { force: true }); };
  const chat = {
    async upload(acct, req, res) {
      const buf = await readRaw(req, MAX_FILE + 64); // + the AES-GCM tag
      if (!buf.length) return send(res, 400, { error: 'Empty file.' });
      const seal = String(req.headers['x-vr-own-seal'] || '');
      const id = crypto.randomBytes(12).toString('hex');
      fs.writeFileSync(path.join(DIR, id), buf, { mode: 0o600 });
      setMeta(id, { by: acct.username, at: Date.now(), size: buf.length, seq: 0 });
      // the uploader's library: when and how big; name / type / key only inside the seal their own chat key opens
      updateIndex(acct, (e) => { e[id] = { id, kind: 'chat', size: buf.length, at: Date.now(), lastAccess: 0, ...(/^[A-Za-z0-9+/=_.:-]{20,4000}$/.test(seal) ? { seal } : {}) }; });
      return send(res, 200, { id, size: buf.length });
    },
    // the files a message names: the sender's own, uploaded and not sent yet; marked as that message's
    attach(user, ids, seq, conv) {
      if (ids === undefined) return [];
      if (!Array.isArray(ids) || ids.length > 20) throw Object.assign(new Error('Up to 20 files per message.'), { status: 400 });
      for (const id of ids) { const m = ID_RE.test(String(id)) ? meta(id) : null; if (!m || m.by !== user || m.seq) throw Object.assign(new Error('A file of this message is not yours or was sent already.'), { status: 400 }); }
      for (const id of ids) setMeta(id, { ...meta(id), seq });
      const acct = accountOf(user);
      if (acct) updateIndex(acct, (e) => { for (const id of ids) if (e[id]) e[id].where = { conv: conv || 'room', seq, at: Date.now() }; });
      return ids.map(String);
    },
    // the ciphertext: for its uploader always (their library, even after the message is gone), for anyone else while
    // they may read the message it belongs to
    fetch(id, user, mayRead) {
      if (!ID_RE.test(String(id || ''))) return null;
      const m = meta(id); if (!m) return null;
      if (m.by !== user && !(m.seq && mayRead(m.seq))) return null;
      let buf; try { buf = fs.readFileSync(path.join(DIR, id)); } catch { return null; }
      touch(accountOf(m.by), [id]);
      return buf;
    },
    // a message is gone (deleted, cleared, a ban's cleanup): its readers lose its files; each file stays in its uploader's
    // library (when their browser left them the key) until they delete it there
    messageGone(ids) {
      for (const id of ids || []) {
        if (!ID_RE.test(String(id))) continue;
        const m = meta(id); if (!m) continue;
        const acct = accountOf(m.by), entry = acct ? readIndex(acct).entries[id] : null;
        if (!entry || !entry.seal) { removeCipher(id); if (acct) updateIndex(acct, (e) => { delete e[id]; }); continue; }
        setMeta(id, { ...m, seq: 0, wasSeq: m.seq });
        updateIndex(acct, (e) => { if (e[id] && e[id].where) e[id].where.deleted = Date.now(); });
      }
    },
    allGone() { for (const f of fs.readdirSync(DIR).filter((x) => x.endsWith('.json'))) chat.messageGone([f.slice(0, -5)]); },
    sweep() { // uploaded but never sent within a day, and not in anyone's library (no seal: an older page)
      for (const f of fs.readdirSync(DIR).filter((x) => x.endsWith('.json'))) {
        const id = f.slice(0, -5), m = meta(id); if (!m || m.seq || m.wasSeq || Date.now() - m.at < DAY) continue;
        const acct = accountOf(m.by), entry = acct ? readIndex(acct).entries[id] : null;
        if (!entry || !entry.seal) { removeCipher(id); if (acct) updateIndex(acct, (e) => { delete e[id]; }); }
      }
    },
  };

  // ---------- Work: the person's own uploads, readable by their Claude ----------
  const work = {
    fallbackDir: (acct) => path.join(profileDir(acct), 'uploads'),
    dirOf: (acct) => (linux ? linux.uploadsDir(acct.username, work.fallbackDir(acct)).dir : work.fallbackDir(acct)),
    async upload(acct, req, res) {
      const buf = await readRaw(req);
      if (!buf.length) return send(res, 400, { error: 'Empty file.' });
      const id = crypto.randomBytes(12).toString('hex'), mime = cleanMime(req.headers['content-type']);
      const day = new Date().toISOString().slice(0, 10), dir = path.join(work.dirOf(acct), day);
      fs.mkdirSync(dir, { recursive: true });
      const name = freeName(dir, safeName(req.headers['x-file-name'])), file = path.join(dir, name);
      fs.writeFileSync(file, buf, { mode: 0o660 });
      updateIndex(acct, (e) => { e[id] = { id, kind: 'work', name, mime, size: buf.length, at: Date.now(), lastAccess: 0, path: file }; });
      return send(res, 200, { id, name, mime, size: buf.length });
    },
    // { id, name, mime, size, path } of one of this person's uploads, or null
    get(acct, id) {
      if (!ID_RE.test(String(id || ''))) return null;
      const e = readIndex(acct).entries[id];
      if (e && e.kind === 'work') return fs.existsSync(e.path) ? { id, name: e.name, mime: e.mime, size: e.size, path: e.path } : null;
      const dir = path.join(work.fallbackDir(acct), id); // an upload from before the library (3.7.0: uploads/<id>/<name>)
      let m; try { m = JSON.parse(fs.readFileSync(path.join(dir, '.meta.json'), 'utf8')); } catch { return null; }
      const file = path.join(dir, safeName(m.name));
      return fs.existsSync(file) ? { id, name: m.name, mime: m.mime, size: m.size, path: file } : null;
    },
    serve(acct, id, res) { const f = work.get(acct, id); if (!f) return send(res, 404, { error: 'That file is no longer kept.' }); touch(acct, [id]); return serve(res, fs.readFileSync(f.path), f.name, f.mime); },
    sweep(accounts) { // the pre-library layout kept 30 days; library files stay until their owner deletes them
      for (const a of accounts) {
        const root = work.fallbackDir(a); if (!fs.existsSync(root)) continue;
        for (const id of fs.readdirSync(root)) { try { const m = JSON.parse(fs.readFileSync(path.join(root, id, '.meta.json'), 'utf8')); if (Date.now() - m.at > 30 * DAY) fs.rmSync(path.join(root, id), { recursive: true, force: true }); } catch {} }
      }
    },
  };

  // ---------- the library ----------
  const library = {
    list(acct) {
      const out = Object.values(readIndex(acct).entries).map((e) => ({ ...e, path: undefined, folder: e.path ? path.dirname(e.path) : '', missing: e.kind === 'work' ? !fs.existsSync(e.path || '') : !meta(e.id) }));
      return out.sort((a, b) => b.at - a.at);
    },
    remove(acct, id) {
      const e = readIndex(acct).entries[id]; if (!e) return false;
      if (e.kind === 'work' && e.path) { fs.rmSync(e.path, { force: true }); try { fs.rmdirSync(path.dirname(e.path)); } catch {} } // the day's folder, when it is empty now
      if (e.kind === 'chat') { const m = meta(id); if (m && m.by === acct.username) removeCipher(id); }
      updateIndex(acct, (x) => { delete x[id]; });
      return true;
    },
  };

  // A Work message carries its files as a marker block at its end (so the queue, the saved conversation and Past sessions
  // keep them with no change): <!--vr-files [{"id","n","m","s"}] -->. For Claude it becomes the list of paths.
  const MARK = /\n*<!--vr-files (\[[\s\S]*?\]) -->\s*$/;
  function workMessage(acct, message, where = {}) {
    const m = MARK.exec(message); if (!m) return { text: message, files: [], dirs: [] };
    let list = []; try { list = JSON.parse(m[1]); } catch {}
    const files = (Array.isArray(list) ? list : []).slice(0, 20).map((f) => work.get(acct, f && f.id)).filter(Boolean);
    const text = message.slice(0, m.index).trim();
    if (!files.length) return { text, files, dirs: [] };
    touch(acct, files.map((f) => f.id), { where: 'work', ...where });
    const lines = files.map((f) => `- ${f.path}  (${f.name}, ${f.mime}, ${f.size} bytes)`);
    return { text: (text || '(no text: see the attached files)') + `\n\n[Attached by the user: ${files.length} file(s). Read them with the Read tool (it reads pictures too):\n${lines.join('\n')}]`, files, dirs: [...new Set(files.map((f) => path.dirname(f.path)))] };
  }
  return { readRaw, chat, work, library, workMessage, MAX_FILE, serve };
};
