// Files: everything this person uploaded in VibeRoller (hub side: bridge/files.js library), on the web and in the Desktop
// app. Loaded after the page script; uses its helpers at call time ($, json, api, ic, sys, convLabel, chatFileBytes,
// workFileBytes, openOwn, ATTACH, me).
//   Each upload: its name, type and size, when it was uploaded, where it went (Work · project, or the chat conversation,
//   and whether that message was deleted since), when it was last opened or used. Open (pictures, players, the text /
//   code viewer), Download, Delete (for good). A chat file can be copied into the person's uploaded_files folder (it is
//   decrypted here, in the browser, and uploaded like a Work file: then it is a normal file to work on, edit, run).
//   At the top: whether the person's Linux account on the server is linked (their uploads land in ~/uploaded_files),
//   and how to link it.
// Chat files' names and keys are sealed to their uploader (only this browser's chat key opens them): the hub never knows.
const LIBRARY = (() => {
  const L = { items: [], linux: null, q: '', kind: '' };
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const when = (t) => (t ? new Date(t).toLocaleString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : 'never');
  const bytesOf = (it) => (it.kind === 'work' ? workFileBytes({ id: it.id }) : chatFileBytes({ id: it.id, k: it.k, i: it.i }));
  async function load() {
    const box = $('libList'); box.textContent = 'Loading…';
    let d; try { d = await json('/api/library'); } catch (e) { box.textContent = e.message; return; }
    L.linux = d.linux;
    // chat files: open their sealed name / type / key with this browser's chat key
    L.items = await Promise.all(d.entries.map(async (e) => {
      if (e.kind !== 'chat') return { ...e, n: e.name, m: e.mime, s: e.size };
      if (!e.seal || typeof chatPriv === 'undefined' || !chatPriv) return { ...e, n: '(an encrypted chat file)', m: 'application/octet-stream', s: e.size, locked: true };
      try { const o = await openOwn(e.seal); return { ...e, n: o.n, m: o.m, s: o.s, k: o.k, i: o.i }; } catch { return { ...e, n: '(cannot be opened in this browser)', m: 'application/octet-stream', s: e.size, locked: true }; }
    }));
    draw();
  }
  function whereOf(it) {
    if (it.kind === 'work') { const u = (it.uses || []).slice(-1)[0]; return 'Work' + (u && u.project ? ' · ' + u.project : '') + (it.uses && it.uses.length ? '' : ' (not sent yet)'); }
    if (!it.where) return 'Chat (never sent)';
    return 'Chat · ' + (typeof convLabel === 'function' ? convLabel(it.where.conv || 'room') : it.where.conv) + (it.where.deleted ? ' · message deleted' : '');
  }
  function drawLink() {
    const b = $('libLink'); b.textContent = ''; const x = L.linux || {};
    if (x.linked) b.append(ic('check', 'lead'), `Linked to your server account "${x.linuxUser}": your uploads are in ${x.folder}, yours to work on, edit and run. Shared projects: ${x.sharedwork}.`);
    else if (x.linuxUser) { b.append(ic('warn', 'lead'), `Your server account "${x.linuxUser}" is not linked yet, so your uploads are kept in VibeRoller's own area. To link it, once, as ${x.linuxUser} on the server (or your AI agent there): `); b.append(el('code', '', x.howToLink.replace(/^.*?: /, ''))); }
    else b.append(ic('warn', 'lead'), 'The server has no Linux account for you, so your uploads are kept in VibeRoller\'s own area.');
  }
  function draw() {
    drawLink();
    const box = $('libList'); box.textContent = '';
    const q = L.q.trim().toLowerCase();
    const list = L.items.filter((it) => (!L.kind || it.kind === L.kind) && (!q || (it.n + ' ' + whereOf(it)).toLowerCase().includes(q)));
    $('libCount').textContent = list.length + ' of ' + L.items.length + ' file' + (L.items.length === 1 ? '' : 's') + ' · ' + ATTACH.size(list.reduce((a, it) => a + (it.s || it.size || 0), 0));
    if (!list.length) { box.append(el('div', 'status', L.items.length ? 'Nothing matches.' : 'Nothing uploaded yet. The + button on a message box (Work or Chat) attaches files.')); return; }
    for (const it of list) {
      const row = el('div', 'libRow' + (it.missing ? ' gone' : ''));
      const k = it.locked ? 'binary' : ATTACH.kind(it);
      const name = el('div', 'libName'); name.append(ic(k === 'image' ? 'image' : k === 'video' ? 'play' : 'file', 'lead'), el('b', '', it.n), el('small', '', (it.m || '') + ' · ' + ATTACH.size(it.s || it.size || 0)));
      const meta = el('div', 'libMeta');
      meta.append(el('span', '', 'Uploaded ' + when(it.at)), el('span', '', whereOf(it)), el('span', '', 'Last used ' + when(it.lastAccess || (it.uses && it.uses.length ? it.uses[it.uses.length - 1].at : 0))));
      if (it.folder) meta.append(el('span', 'libPath', it.folder));
      if (it.missing) meta.append(el('span', 'bad', 'the file itself is gone'));
      const acts = el('div', 'libActs');
      const btn = (label, icon, fn, cls = 'ghost') => { const b = el('button', cls); b.type = 'button'; b.append(ic(icon, 'lead'), label); b.onclick = fn; acts.append(b); return b; };
      if (!it.missing && !it.locked) {
        btn('Open', 'file', () => ATTACH.open(it, () => bytesOf(it)));
        const d = btn('Download', 'download', () => ATTACH.download(it, () => bytesOf(it), d));
        if (it.kind === 'chat' && L.linux && L.linux.linked) btn('Copy to my uploaded_files', 'plus', async (e) => {
          const b = e.currentTarget; b.disabled = true;
          try { const bytes = await bytesOf(it); const r = await api('/api/work/file', { method: 'POST', headers: { 'Content-Type': it.m, 'X-File-Name': encodeURIComponent(it.n) }, body: bytes }); if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText); await load(); }
          catch (err) { b.disabled = false; alert('Not copied: ' + err.message); }
        });
      }
      const del = btn('Delete', 'trash', async () => {
        if (del.dataset.armed !== '1') { del.dataset.armed = '1'; del.lastChild.textContent = 'Delete for good?'; setTimeout(() => { del.dataset.armed = ''; del.lastChild.textContent = 'Delete'; }, 3000); return; }
        try { await json('/api/library/delete', { method: 'POST', body: JSON.stringify({ id: it.id }) }); L.items = L.items.filter((x) => x.id !== it.id); draw(); } catch (err) { alert(err.message); }
      }, 'ghost danger');
      del.lastChild.textContent = 'Delete';
      row.append(name, meta, acts); box.append(row);
    }
  }
  function wire() {
    $('libSearch').oninput = () => { L.q = $('libSearch').value; draw(); };
    $('libKind').onchange = () => { L.kind = $('libKind').value; draw(); };
    $('libRefresh').onclick = load;
  }
  let wired = false;
  return { show() { if (!wired) { wired = true; wire(); } load(); }, state: L };
})();
