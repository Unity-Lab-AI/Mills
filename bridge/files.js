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
// a type from the name, for files that came without one (put in uploaded_files by hand)
const MIME = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', svg: 'image/svg+xml', mp4: 'video/mp4', webm: 'video/webm', mov: 'video/quicktime', mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg', pdf: 'application/pdf', zip: 'application/zip',
  txt: 'text/plain', md: 'text/markdown', json: 'application/json', js: 'text/javascript', ts: 'text/plain', py: 'text/x-python', sh: 'text/x-shellscript', html: 'text/html', css: 'text/css', csv: 'text/csv', rs: 'text/plain', toml: 'text/plain', yml: 'text/plain', yaml: 'text/plain', c: 'text/plain', h: 'text/plain', cpp: 'text/plain', go: 'text/plain', java: 'text/plain', log: 'text/plain', xml: 'text/xml' };
const mimeOf = (name) => MIME[path.extname(name).slice(1).toLowerCase()] || 'application/octet-stream';
const isExec = (p) => { try { return process.platform !== 'win32' && (fs.statSync(p).mode & 0o100) !== 0; } catch { return false; } };
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
    // { id, name, mime, size, path } of one of this person's uploads (or a file they put in their uploaded_files folder
    // themselves), or null
    get(acct, id) {
      if (!ID_RE.test(String(id || ''))) return null;
      const e = readIndex(acct).entries[id];
      if (e && e.kind === 'work') return fs.existsSync(e.path) ? { id, name: e.name, mime: e.mime, size: e.size, path: e.path } : null;
      const d = work.onDisk(acct).get(id);
      if (d) return { id, name: d.name, mime: d.mime, size: d.size, path: d.path };
      const dir = path.join(work.fallbackDir(acct), id); // an upload from before the library (3.7.0: uploads/<id>/<name>)
      let m; try { m = JSON.parse(fs.readFileSync(path.join(dir, '.meta.json'), 'utf8')); } catch { return null; }
      const file = path.join(dir, safeName(m.name));
      return fs.existsSync(file) ? { id, name: m.name, mime: m.mime, size: m.size, path: file } : null;
    },
    serve(acct, id, res) { const f = work.get(acct, id); if (!f) return send(res, 404, { error: 'That file is no longer kept.' }); touch(acct, [id]); return serve(res, fs.readFileSync(f.path), f.name, f.mime); },
    // files in the uploads folder that VibeRoller did not put there (made over SSH, by a Work run, copied in): listed too,
    // id = a hash of the path. Old-layout folders (<id>/ with .meta.json) and dot files are left out; at most 2000 files.
    onDisk(acct) {
      const root = work.dirOf(acct), known = new Set(Object.values(readIndex(acct).entries).map((e) => e.path).filter(Boolean)), out = new Map();
      const walk = (dir, depth) => {
        let list; try { list = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
        for (const d of list) {
          if (out.size >= 2000 || d.name.startsWith('.') || d.name.endsWith('.tmp')) continue;
          const p = path.join(dir, d.name);
          if (d.isDirectory()) { if (depth < 4 && !(depth === 0 && ID_RE.test(d.name))) walk(p, depth + 1); continue; }
          if (!d.isFile() || known.has(p)) continue;
          let st; try { st = fs.statSync(p); } catch { continue; }
          const id = crypto.createHash('sha1').update('disk:' + p).digest('hex').slice(0, 24);
          out.set(id, { id, kind: 'work', disk: true, name: d.name, mime: mimeOf(d.name), size: st.size, at: st.mtimeMs, lastAccess: st.atimeMs, path: p });
        }
      };
      walk(root, 0);
      return out;
    },
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
      const out = Object.values(readIndex(acct).entries).map((e) => ({ ...e, path: undefined, folder: e.path ? path.dirname(e.path) : '', missing: e.kind === 'work' ? !fs.existsSync(e.path || '') : !meta(e.id), exec: e.kind === 'work' && isExec(e.path) }));
      for (const d of work.onDisk(acct).values()) out.push({ ...d, path: undefined, folder: path.dirname(d.path), missing: false, exec: isExec(d.path) });
      return out.sort((a, b) => b.at - a.at);
    },
    remove(acct, id) {
      const e = readIndex(acct).entries[id] || work.onDisk(acct).get(id); if (!e) return false;
      if (e.kind === 'work' && e.path) { fs.rmSync(e.path, { force: true }); try { fs.rmdirSync(path.dirname(e.path)); } catch {} } // the day's folder, when it is empty now
      if (e.kind === 'chat') { const m = meta(id); if (m && m.by === acct.username) removeCipher(id); }
      updateIndex(acct, (x) => { delete x[id]; });
      return true;
    },
    // edit: a Work file's new content (the page's text editor), its permissions kept; chat files are end-to-end encrypted,
    // so they are copied into uploaded_files first and edited there
    save(acct, id, buf) {
      const f = work.get(acct, id); if (!f) throw Object.assign(new Error('Only your own Work files (uploaded_files) can be edited; copy a chat file there first.'), { status: 404 });
      if (buf.includes(0)) throw Object.assign(new Error('That is not text; only text files are edited here.'), { status: 400 });
      fs.writeFileSync(f.path + '.vr-save.tmp', buf); try { fs.chmodSync(f.path + '.vr-save.tmp', fs.statSync(f.path).mode & 0o7777); } catch {}
      fs.renameSync(f.path + '.vr-save.tmp', f.path);
      updateIndex(acct, (e) => { if (e[id]) { e[id].size = buf.length; e[id].edited = Date.now(); e[id].lastAccess = Date.now(); } });
      return { id, size: buf.length };
    },
    // execute permission on a Work file (chmod u+x,g+x / off): run it in a terminal on the server, from a Work run
    // ("Run in Work"), or in the Desktop app after downloading it. Not on Windows hubs (no such bit there).
    setExec(acct, id, on) {
      const f = work.get(acct, id); if (!f) throw Object.assign(new Error('Not found.'), { status: 404 });
      if (process.platform === 'win32') throw Object.assign(new Error('This hub runs on Windows: files there have no execute permission to set.'), { status: 400 });
      const mode = fs.statSync(f.path).mode & 0o7777;
      try { fs.chmodSync(f.path, on ? mode | 0o110 : mode & ~0o111); } catch (e) { throw Object.assign(new Error('Could not change it (' + e.code + '): the file belongs to your own account, so run chmod there.'), { status: 403 }); }
      return { id, exec: isExec(f.path) };
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
