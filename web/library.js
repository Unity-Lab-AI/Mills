// Files: everything this person uploaded in VibeRoller (hub side: bridge/files.js library), on the web and in the Desktop
// app. Loaded after the page script; uses its helpers at call time ($, json, api, ic, sys, convLabel, chatFileBytes,
// workFileBytes, openOwn, ATTACH, me).
//   Each upload: its name, type and size, when it was uploaded, where it went (Work · project, or the chat conversation,
//   and whether that message was deleted since), when it was last opened or used. Open (pictures, players, the text /
//   code viewer), Download, Delete (for good). A chat file can be copied into the person's uploaded_files folder (it is
//   decrypted here, in the browser, and uploaded like a Work file: then it is a normal file to work on, edit, run).
//   Work files (and anything else in the person's uploaded_files folder, put there over SSH or by a Work run) can also be
//   edited in place (text, up to 2 MB), made executable or not, and sent to Work as they are ("Use in Work", or "Run in
//   Work": Claude runs it there and reports).
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
    if (it.disk) return 'Put in your folder outside VibeRoller';
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
        if (it.kind === 'work') {
          if (k === 'text' && (it.s || it.size || 0) <= 2 * 1048576) btn('Edit', 'file', () => edit(it));
          btn('Use in Work', 'plus', () => toWork(it, ''));
          btn('Run in Work', 'play', () => toWork(it, `Run the attached file ${it.n}${it.exec ? '' : ' (make it executable first if it needs to be)'} and tell me what it did. Ask me first if it would change anything outside its own folder.`));
          const x = btn(it.exec ? 'Executable' : 'Not executable', it.exec ? 'check' : 'warn', async () => {
            x.disabled = true;
            try { const r = await json('/api/library/exec', { method: 'POST', body: JSON.stringify({ id: it.id, on: !it.exec }) }); it.exec = r.exec; draw(); } catch (err) { x.disabled = false; alert(err.message); }
          });
          x.title = it.exec ? 'Can be run as a program. Click to take that away.' : 'Click to let it run as a program (chmod +x).';
        }
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
  // the file goes with the next Work message as it is (no new upload); Work opens with the message box ready
  function toWork(it, text) {
    showScreen('shell');
    setWorkLibFiles([{ id: it.id, n: it.n, m: it.m, s: it.s || it.size || 0 }], text || $('input').value);
  }
  // the text editor: loads the file, saves it back in place (its permissions kept); Ctrl+S saves, Escape closes
  async function edit(it) {
    let text; try { text = new TextDecoder('utf-8', { fatal: true }).decode(await bytesOf(it)); } catch { alert('This file is not UTF-8 text, so it cannot be edited here.'); return; }
    const wrap = el('div', 'libEdit'), box = el('div'), head = el('div', 'libActs'), ta = el('textarea'), foot = el('div', 'libActs');
    const state = el('small', '', it.folder ? it.folder + '/' + it.n : it.n);
    ta.value = text; ta.spellcheck = false;
    const close = () => { if (ta.value !== text && !confirm('Close without saving?')) return; wrap.remove(); };
    const save = async () => {
      sv.disabled = true; state.textContent = 'Saving…';
      try {
        const r = await api('/api/library/save?id=' + encodeURIComponent(it.id), { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: new TextEncoder().encode(ta.value) });
        const d = await r.json().catch(() => ({})); if (!r.ok) throw new Error(d.error || r.statusText);
        text = ta.value; it.s = it.size = d.size; state.textContent = 'Saved ' + new Date().toLocaleTimeString();
      } catch (err) { state.textContent = 'Not saved: ' + err.message; }
      sv.disabled = false;
    };
    const sv = el('button', '', 'Save'); sv.type = 'button'; sv.onclick = save;
    const cx = el('button', 'ghost', 'Close'); cx.type = 'button'; cx.onclick = close;
    ta.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') { e.preventDefault(); save(); }
      if (e.key === 'Escape') { e.preventDefault(); close(); }
      if (e.key === 'Tab' && !e.shiftKey) { e.preventDefault(); ta.setRangeText('  ', ta.selectionStart, ta.selectionEnd, 'end'); }
    });
    head.append(el('b', '', 'Edit ' + it.n), state);
    const right = el('div'); right.append(cx, sv); foot.append(el('small', '', 'Ctrl+S saves · Esc closes'), right);
    box.append(head, ta, foot); wrap.append(box); document.body.append(wrap); ta.focus();
  }
  function wire() {
    $('libSearch').oninput = () => { L.q = $('libSearch').value; draw(); };
    $('libKind').onchange = () => { L.kind = $('libKind').value; draw(); };
    $('libRefresh').onclick = load;
  }
  let wired = false;
  return { show() { if (!wired) { wired = true; wire(); } load(); }, state: L };
})();
