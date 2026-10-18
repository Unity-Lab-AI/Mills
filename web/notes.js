// VibeRoller team notes, page side (hub side: bridge/notes.js). Loaded after the page script; uses its
// helpers at call time ($, json, me, ic, label, MD, showScreen, store, sys…).
// Left: the folder tree with search; right: the note (edit / preview, Save = Ctrl+S, rename / move, delete).
// Saving sends the time the note was opened, so two people never silently overwrite each other.
const NOTES = (() => {
  const N = { tree: [], ver: 0, open: null, at: null, dirty: false, gen: 0, q: '', collapsed: new Set() };
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const msg = (t, bad) => { const m = $('ntMsg'); if (!m) return; m.textContent = t || ''; m.className = 'status' + (bad ? '' : ' on'); if (t) setTimeout(() => { if (m.textContent === t) m.textContent = ''; }, 4000); };
  async function refresh() {
    if (!me) return;
    try { const d = await json('/api/notes'); N.tree = d.tree; N.ver = d.ver; } catch (e) { msg(e.message, true); return; }
    renderTree();
  }
  function renderTree() {
    const box = $('ntTree'); if (!box) return;
    box.textContent = '';
    if (N.q.trim()) return renderSearch();
    const walk = (items, depth) => {
      for (const it of items) {
        if (it.type === 'folder') {
          const row = el('div', 'ntRow ntFolder'); row.style.paddingLeft = (depth * 0.9 + 0.3) + 'rem';
          const closed = N.collapsed.has(it.path);
          row.append(ic(closed ? 'folder' : 'folder', 'lead'), el('span', 'ntName', it.name), el('small', '', closed ? '…' : ''));
          row.onclick = () => { if (closed) N.collapsed.delete(it.path); else N.collapsed.add(it.path); renderTree(); };
          const add = el('button', 'ghost tiny'); add.type = 'button'; add.title = 'New note in ' + it.name; add.append(ic('plus')); add.onclick = (e) => { e.stopPropagation(); newNote(it.path); }; row.append(add);
          box.append(row);
          if (!closed) walk(it.children, depth + 1);
        } else {
          const row = el('div', 'ntRow' + (N.open === it.path ? ' on' : '')); row.style.paddingLeft = (depth * 0.9 + 0.3) + 'rem';
          row.append(ic('file', 'lead'), el('span', 'ntName', it.name));
          const s = el('small', '', (it.by ? it.by + ' · ' : '') + ago(it.at)); row.append(s);
          row.onclick = () => open(it.path);
          box.append(row);
        }
      }
    };
    walk(N.tree, 0);
    if (!N.tree.length) box.append(el('div', 'bdNone', 'No notes yet. Make the first one with +.'));
  }
  async function renderSearch() {
    const box = $('ntTree'); box.textContent = '';
    let items = [];
    try { items = (await json('/api/notes/search?q=' + encodeURIComponent(N.q))).items; } catch (e) { return msg(e.message, true); }
    for (const it of items) {
      const row = el('div', 'ntRow' + (N.open === it.path ? ' on' : '')); row.append(ic('file', 'lead'), el('span', 'ntName', it.path.replace(/\.md$/i, '')));
      if (it.snippet) row.append(el('div', 'ntSnip', it.snippet));
      row.onclick = () => open(it.path); box.append(row);
    }
    if (!items.length) box.append(el('div', 'bdNone', 'Nothing matches.'));
  }
  const ago = (ms) => { const s = Math.round((Date.now() - ms) / 1000); return s < 60 ? 'just now' : s < 3600 ? Math.round(s / 60) + ' min ago' : s < 86400 ? Math.round(s / 3600) + ' h ago' : Math.round(s / 86400) + ' d ago'; };
  async function open(p) {
    if (N.dirty && !window.confirm('Leave without saving the current note?')) return;
    try {
      const n = await json('/api/notes/read?path=' + encodeURIComponent(p));
      N.open = n.path; N.at = n.at; N.dirty = false;
      $('ntTitle').textContent = n.path.replace(/\.md$/i, ''); $('ntEditor').value = n.text; $('ntMeta').textContent = (n.by ? 'saved by ' + n.by + ' · ' : '') + new Date(n.at).toLocaleString();
      $('ntEmpty').hidden = true; $('ntDoc').hidden = false;
      if (!$('ntPreview').hidden) MD.render($('ntPreview'), n.text || '_Empty note._');
      store.set('mills.note.' + me.username, n.path);
      renderTree();
    } catch (e) { msg(e.message, true); }
  }
  async function save() {
    if (!N.open) return;
    try {
      const r = await json('/api/notes/write', { method: 'POST', body: JSON.stringify({ path: N.open, text: $('ntEditor').value, baseAt: N.at }) });
      N.at = r.at; N.dirty = false; $('ntMeta').textContent = 'saved by ' + r.by + ' · ' + new Date(r.at).toLocaleString(); msg('Saved.');
      refresh();
    } catch (e) { msg(e.message, true); }
  }
  async function newNote(folder) {
    const name = window.prompt('Name of the new note' + (folder ? ' in ' + folder : '') + ':'); if (!name || !name.trim()) return;
    const p = (folder ? folder + '/' : '') + name.trim().replace(/\.md$/i, '') + '.md';
    try { await json('/api/notes/write', { method: 'POST', body: JSON.stringify({ path: p, text: '# ' + name.trim() + '\n\n' }) }); await refresh(); open(p); } catch (e) { msg(e.message, true); }
  }
  async function newFolder() {
    const name = window.prompt('Name of the new folder (use / for a folder inside a folder):'); if (!name || !name.trim()) return;
    try { await json('/api/notes/folder', { method: 'POST', body: JSON.stringify({ path: name.trim() }) }); refresh(); } catch (e) { msg(e.message, true); }
  }
  async function rename() {
    if (!N.open) return;
    const to = window.prompt('New path (folders with /):', N.open.replace(/\.md$/i, '')); if (!to || !to.trim()) return;
    try { const r = await json('/api/notes/move', { method: 'POST', body: JSON.stringify({ from: N.open, to: to.trim().replace(/\.md$/i, '') + '.md' }) }); N.open = r.to; await refresh(); open(r.to); } catch (e) { msg(e.message, true); }
  }
  async function remove() {
    if (!N.open) return;
    const b = $('ntDelete'); if (b.dataset.armed !== '1') { b.dataset.armed = '1'; label(b, 'trash', 'Click again to delete'); setTimeout(() => { b.dataset.armed = ''; label(b, 'trash', 'Delete'); }, 3000); return; }
    try { await json('/api/notes/delete', { method: 'POST', body: JSON.stringify({ path: N.open }) }); N.open = null; N.dirty = false; $('ntDoc').hidden = true; $('ntEmpty').hidden = false; refresh(); } catch (e) { msg(e.message, true); }
  }
  function togglePreview() {
    const pv = $('ntPreview'), ed = $('ntEditor');
    pv.hidden = !pv.hidden; ed.hidden = !pv.hidden;
    label($('ntToggle'), pv.hidden ? 'eye' : 'edit', pv.hidden ? 'Preview' : 'Edit');
    if (!pv.hidden) MD.render(pv, ed.value || '_Empty note._');
  }
  function wire() {
    if (wire.done) return; wire.done = true;
    $('ntNew').onclick = () => newNote('');
    $('ntNewFolder').onclick = newFolder;
    $('ntSave').onclick = save; $('ntRename').onclick = rename; $('ntDelete').onclick = remove; $('ntToggle').onclick = togglePreview;
    $('ntSearch').oninput = () => { N.q = $('ntSearch').value; renderTree(); };
    $('ntEditor').addEventListener('input', () => { N.dirty = true; });
    $('ntEditor').addEventListener('keydown', (e) => { if ((e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); save(); } });
    window.addEventListener('beforeunload', (e) => { if (N.dirty && !$('notes').hidden) { e.preventDefault(); e.returnValue = ''; } });
  }
  return {
    show() { wire(); refresh().then(() => { const last = store.get('mills.note.' + (me ? me.username : '')); if (last && !N.open) open(last).catch(() => {}); }); },
    onVersion(v) { if (v && v !== N.ver && !$('notes').hidden) refresh(); },
    stop() { N.open = null; N.dirty = false; N.tree = []; N.ver = 0; },
    state: N,
  };
})();
