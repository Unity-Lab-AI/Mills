// Mills team notes: markdown files in folders on the host (Mills/Notes/, never committed), shared by the
// team and by Claude. An idea from HyperProductivityManager: notes are real .md files you can also
// open, grep and back up outside Mills. Like the board, notes are NOT end-to-end encrypted (Claude has
// to read them); the README says so.
//
// Paths are relative to Mills/Notes, use "/" between folders, and allow letters, digits, space, dot,
// dash and underscore. Notes end in ".md". Names are treated case-insensitively (Windows and Linux must
// agree): making "Plan.md" when "plan.md" exists is refused. Line endings are stored as "\n".
// Routes: GET /api/notes (tree) · GET /api/notes/read?path= · POST /api/notes/write {path, text, baseAt}
// · POST /api/notes/delete {path} · POST /api/notes/move {from, to} · POST /api/notes/folder {path}
// · GET /api/notes/search?q=. Claude: /internal/notes (notes_list, notes_read, notes_write, notes_delete).
'use strict';

const fs = require('fs');
const path = require('path');

const SEG_RE = /^[A-Za-z0-9][A-Za-z0-9 ._-]{0,79}$/;

module.exports = function createNotes({ dir, send, readBody, displayOf, changed }) {
  fs.mkdirSync(dir, { recursive: true });
  const META = path.join(dir, '.index.json'); // path -> { by, at } (who saved it last)
  let meta = {}; try { meta = JSON.parse(fs.readFileSync(META, 'utf8')); } catch {}
  let ver = 1;
  const saveMeta = () => { fs.writeFileSync(META + '.tmp', JSON.stringify(meta)); fs.renameSync(META + '.tmp', META); ver++; changed(); };

  function segs(p, { note = false, folder = false } = {}) { // → clean segments or a thrown error
    const parts = String(p || '').replace(/\\/g, '/').split('/').filter((s) => s !== '');
    if (parts.some((s) => s === '.' || s === '..' || !SEG_RE.test(s) || s.startsWith('.'))) throw new Error('Bad path: folders and notes use letters, digits, spaces, dots, dashes and underscores.');
    if (note) { if (!parts.length || !/\.md$/i.test(parts[parts.length - 1])) throw new Error('A note ends in .md'); }
    if (folder && !parts.length) throw new Error('Name the folder.');
    return parts;
  }
  const rel = (parts) => parts.join('/');
  const abs = (parts) => path.join(dir, ...parts);
  // The on-disk entry whose name matches case-insensitively (Windows finds it anyway; on Linux it would be a second file).
  function existing(parts) {
    let cur = dir;
    const out = [];
    for (const s of parts) {
      if (!fs.existsSync(cur)) return null;
      const hit = fs.readdirSync(cur).find((n) => n.toLowerCase() === s.toLowerCase());
      if (!hit) return null;
      out.push(hit); cur = path.join(cur, hit);
    }
    return out;
  }
  function tree(folder = []) { // every folder and note under `folder`, sorted, folders first
    const base = abs(folder);
    if (!fs.existsSync(base)) return [];
    return fs.readdirSync(base, { withFileTypes: true }).filter((e) => !e.name.startsWith('.') && (e.isDirectory() || /\.md$/i.test(e.name)))
      .sort((a, b) => (b.isDirectory() - a.isDirectory()) || a.name.localeCompare(b.name))
      .map((e) => {
        const p = rel([...folder, e.name]);
        if (e.isDirectory()) return { type: 'folder', path: p, name: e.name, children: tree([...folder, e.name]) };
        const st = fs.statSync(path.join(base, e.name)), m = meta[p] || {};
        return { type: 'note', path: p, name: e.name.replace(/\.md$/i, ''), size: st.size, at: st.mtimeMs, by: m.by || '' };
      });
  }
  function read(p) {
    const parts = segs(p, { note: true }), hit = existing(parts);
    if (!hit) throw Object.assign(new Error('No such note.'), { status: 404 });
    const file = abs(hit), st = fs.statSync(file), m = meta[rel(hit)] || {};
    return { path: rel(hit), text: fs.readFileSync(file, 'utf8').replace(/\r\n?/g, '\n'), at: st.mtimeMs, by: m.by || '' };
  }
  function write(actor, p, text, baseAt) {
    const parts = segs(p, { note: true }), hit = existing(parts);
    if (hit && rel(hit) !== rel(parts)) throw new Error(`"${rel(hit)}" exists already (names are the same ignoring case; pick another).`);
    const file = abs(parts);
    if (hit && baseAt !== undefined && baseAt !== null && Math.abs(fs.statSync(file).mtimeMs - Number(baseAt)) > 1) throw Object.assign(new Error('Someone saved this note after you opened it. Reload it, then save again.'), { status: 409 });
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const body = String(text || '').replace(/\r\n?/g, '\n');
    if (body.length > 2e6) throw new Error('That note is too large (2 MB).');
    fs.writeFileSync(file + '.tmp', body); fs.renameSync(file + '.tmp', file);
    meta[rel(parts)] = { by: actor, at: Date.now() }; saveMeta();
    return { path: rel(parts), at: fs.statSync(file).mtimeMs, by: actor, created: !hit };
  }
  function remove(p) {
    const parts = segs(p), hit = existing(parts);
    if (!hit) throw Object.assign(new Error('Nothing there.'), { status: 404 });
    const target = abs(hit);
    if (fs.statSync(target).isDirectory()) { if (fs.readdirSync(target).length) throw new Error('Empty the folder first.'); fs.rmdirSync(target); }
    else fs.unlinkSync(target);
    delete meta[rel(hit)]; saveMeta();
    return { removed: rel(hit) };
  }
  function move(actor, from, to) {
    const a = existing(segs(from)); if (!a) throw Object.assign(new Error('No such note or folder.'), { status: 404 });
    const isDir = fs.statSync(abs(a)).isDirectory();
    const b = segs(to, { note: !isDir, folder: isDir }), bHit = existing(b);
    if (bHit && rel(bHit).toLowerCase() !== rel(a).toLowerCase()) throw new Error(`"${rel(bHit)}" exists already.`);
    fs.mkdirSync(path.dirname(abs(b)), { recursive: true });
    fs.renameSync(abs(a), abs(b));
    for (const k of Object.keys(meta)) if (k === rel(a) || k.startsWith(rel(a) + '/')) { meta[rel(b) + k.slice(rel(a).length)] = { ...meta[k], by: actor, at: Date.now() }; delete meta[k]; }
    saveMeta();
    return { from: rel(a), to: rel(b) };
  }
  function folder(p) { const parts = segs(p, { folder: true }), hit = existing(parts); if (hit) throw new Error('That folder exists already.'); fs.mkdirSync(abs(parts), { recursive: true }); ver++; changed(); return { path: rel(parts) }; }
  function search(q) { // every word must appear (case-insensitive) in the path or the text; snippets around the first hit
    const words = String(q || '').toLowerCase().split(/\s+/).filter(Boolean).slice(0, 8);
    const out = [];
    const walk = (folder) => { for (const e of tree(folder)) { if (e.type === 'folder') walk(e.path.split('/')); else out.push(e); } };
    walk([]);
    if (!words.length) return out.map((n) => ({ ...n, snippet: '' }));
    const hits = [];
    for (const n of out) {
      const text = fs.readFileSync(abs(n.path.split('/')), 'utf8'), low = (n.path + '\n' + text).toLowerCase();
      if (!words.every((w) => low.includes(w))) continue;
      const i = Math.min(...words.map((w) => text.toLowerCase().indexOf(w)).filter((k) => k >= 0));
      const start = Number.isFinite(i) ? Math.max(0, i - 60) : 0;
      hits.push({ ...n, snippet: Number.isFinite(i) ? (start ? '…' : '') + text.slice(start, start + 160).replace(/\s+/g, ' ') + '…' : '' });
    }
    return hits;
  }
  const fail = (res, e) => send(res, e.status || 400, { error: e.message });
  async function route(p, req, res, acct, url) {
    if (p !== '/api/notes' && !p.startsWith('/api/notes/')) return false;
    try {
      if (p === '/api/notes' && req.method === 'GET') return send(res, 200, { ver, tree: tree() }), true;
      if (p === '/api/notes/read' && req.method === 'GET') return send(res, 200, read(url.searchParams.get('path'))), true;
      if (p === '/api/notes/search' && req.method === 'GET') return send(res, 200, { items: search(url.searchParams.get('q')) }), true;
      if (req.method !== 'POST') return false;
      const b = await readBody(req), who = displayOf(acct.username);
      if (p === '/api/notes/write') return send(res, 200, write(who, b.path, b.text, b.baseAt)), true;
      if (p === '/api/notes/delete') return send(res, 200, remove(b.path)), true;
      if (p === '/api/notes/move') return send(res, 200, move(who, b.from, b.to)), true;
      if (p === '/api/notes/folder') return send(res, 200, folder(b.path)), true;
      return false;
    } catch (e) { fail(res, e); return true; }
  }
  // Claude in a Work run, acting as "<person> · Claude"
  function call(name, args, acct) {
    const who = displayOf(acct.username) + ' · Claude';
    try {
      if (name === 'notes_list') { const items = search(String(args.q || '')); return { ok: true, result: { notes: items.map((n) => ({ path: n.path, by: n.by, at: new Date(n.at).toISOString(), size: n.size, snippet: n.snippet || undefined })) } }; }
      if (name === 'notes_read') return { ok: true, result: read(args.path) };
      if (name === 'notes_write') return { ok: true, result: write(who, args.path, args.text, undefined) };
      if (name === 'notes_delete') return { ok: true, result: remove(args.path) };
      return { ok: false, error: 'Unknown notes tool: ' + name };
    } catch (e) { return { ok: false, error: e.message }; }
  }
  return { route, call, version: () => ver, tree, read, write, remove, move, folder, search };
};
