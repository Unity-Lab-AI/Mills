// VibeRoller Tools: this app's own window (never loaded from the network), for what only the native app can do.
// Every action is a command of the app (src/main.rs); outputs of terminals and runs arrive as events.
'use strict';
const T = window.__TAURI__;
const call = (cmd, args = {}) => T.core.invoke(cmd, args);
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
function toast(msg, bad) { const t = $('toast'); t.textContent = msg; t.style.borderColor = bad ? 'var(--bad)' : 'var(--accent)'; t.style.display = 'block'; clearTimeout(toast.t); toast.t = setTimeout(() => { t.style.display = 'none'; }, 4000); }
const fail = (e) => toast(String(e), true);
let S = {}, cwd = ''; // settings; the folder open under Files (relative to the projects folder)

// ---------- tabs ----------
for (const b of document.querySelectorAll('nav button')) b.onclick = () => {
  for (const x of document.querySelectorAll('nav button')) x.classList.toggle('on', x === b);
  for (const s of document.querySelectorAll('.tab')) s.classList.toggle('on', s.id === b.dataset.tab);
  if (b.dataset.tab === 'host') hostRefresh();
  if (b.dataset.tab === 'proxy') proxyRefresh();
  if (b.dataset.tab === 'build') drawSaved();
  const where = cwd ? '/' + cwd : 'the projects folder'; for (const id of ['termWhere', 'buildWhere', 'gitWhere']) $(id).textContent = where;
};

// ---------- files ----------
async function list() {
  $('crumbs').textContent = (S.projects || '(choose a projects folder in Settings)') + (cwd ? '/' + cwd : '');
  const ul = $('fList'); ul.textContent = '';
  if (!S.projects) return;
  let items; try { items = await call('fs_list', { path: cwd }); } catch (e) { return fail(e); }
  for (const it of items) {
    const li = el('li'); li.append(el('span', '', (it.dir ? '▸ ' : '') + it.name), el('small', '', it.dir ? '' : size(it.size)));
    li.onclick = () => { if (it.dir) { cwd = cwd ? cwd + '/' + it.name : it.name; list(); } else open(cwd ? cwd + '/' + it.name : it.name); };
    ul.append(li);
  }
}
const size = (n) => (n < 1024 ? n + ' B' : n < 1048576 ? (n / 1024).toFixed(1) + ' KB' : (n / 1048576).toFixed(1) + ' MB');
let openPath = '', openText = '';
async function open(p) {
  let r; try { r = await call('fs_read', { path: p }); } catch (e) { return fail(e); }
  openPath = p; $('edName').textContent = p; $('edDelete').disabled = false;
  if (r.binary) { $('ed').value = '(not text: ' + size(r.size) + ')'; $('ed').disabled = true; $('edSave').disabled = true; openText = null; return; }
  openText = r.text; $('ed').value = r.text; $('ed').disabled = false; $('edSave').disabled = false; $('edState').textContent = '';
}
async function save() { if (openText === null || !openPath) return; try { await call('fs_write', { path: openPath, text: $('ed').value }); openText = $('ed').value; $('edState').textContent = 'Saved ' + new Date().toLocaleTimeString(); } catch (e) { fail(e); } }
$('edSave').onclick = save;
$('ed').addEventListener('keydown', (e) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') { e.preventDefault(); save(); } if (e.key === 'Tab') { e.preventDefault(); $('ed').setRangeText('  ', $('ed').selectionStart, $('ed').selectionEnd, 'end'); } });
$('ed').addEventListener('input', () => { $('edState').textContent = $('ed').value !== openText ? 'unsaved' : ''; });
$('edDelete').onclick = async () => { if (!openPath || !confirm('Delete ' + openPath + '?')) return; try { await call('fs_delete', { path: openPath }); openPath = ''; $('ed').value = ''; $('edName').textContent = 'No file open'; list(); } catch (e) { fail(e); } };
$('fUp').onclick = () => { cwd = cwd.split('/').slice(0, -1).join('/'); list(); };
$('fRefresh').onclick = list;
$('fNewFile').onclick = async () => { const n = prompt('New file name'); if (!n) return; const p = cwd ? cwd + '/' + n : n; try { await call('fs_write', { path: p, text: '' }); await list(); open(p); } catch (e) { fail(e); } };
$('fNewDir').onclick = async () => { const n = prompt('New folder name'); if (!n) return; try { await call('fs_mkdir', { path: cwd ? cwd + '/' + n : n }); list(); } catch (e) { fail(e); } };

// ---------- terminal (a real pseudo-terminal; colours and cursor moves are dropped, the text stays) ----------
let termId = 0;
const ANSI = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78]/g;
function termWrite(s) {
  const o = $('termOut'); let t = o.textContent + s.replace(ANSI, '');
  t = t.replace(/[^\n]*\r(?!\n)/g, '').replace(/.\x08/g, '').replace(/\x07/g, '');
  o.textContent = t.slice(-200000); o.scrollTop = o.scrollHeight;
}
$('termNew').onclick = async () => {
  if (termId) await call('term_close', { id: termId }).catch(() => {});
  $('termOut').textContent = '';
  try { termId = await call('term_open', { path: cwd, cols: 120, rows: 32 }); $('termOut').focus(); } catch (e) { fail(e); }
};
$('termKill').onclick = async () => { if (termId) { await call('term_close', { id: termId }).catch(() => {}); termId = 0; } };
const KEYS = { Enter: '\r', Backspace: '\x7f', Tab: '\t', Escape: '\x1b', ArrowUp: '\x1b[A', ArrowDown: '\x1b[B', ArrowRight: '\x1b[C', ArrowLeft: '\x1b[D', Home: '\x1b[H', End: '\x1b[F', Delete: '\x1b[3~' };
$('termOut').addEventListener('keydown', (e) => {
  if (!termId) return;
  let d = '';
  if (e.ctrlKey && e.key.length === 1 && /[a-z]/i.test(e.key)) d = String.fromCharCode(e.key.toUpperCase().charCodeAt(0) - 64);
  else if (KEYS[e.key]) d = KEYS[e.key];
  else if (e.key.length === 1 && !e.metaKey) d = e.key;
  if (!d) return;
  e.preventDefault(); call('term_write', { id: termId, data: d }).catch(fail);
});
$('termOut').addEventListener('paste', (e) => { if (!termId) return; e.preventDefault(); call('term_write', { id: termId, data: e.clipboardData.getData('text') }).catch(fail); });
T.event.listen('term', (ev) => { const p = ev.payload; if (p.id !== termId) return; if (p.exit) { termWrite('\n[the terminal ended]\n'); termId = 0; } else termWrite(p.data); });

// ---------- build & run ----------
let runId = 0;
const projKey = () => cwd.split('/')[0] || '(projects)';
function drawSaved() {
  const box = $('bSaved'); box.textContent = '';
  for (const b of (S.builds && S.builds[projKey()]) || []) { const x = el('button', '', b.name); x.title = b.cmd; x.onclick = () => run(b.cmd); x.oncontextmenu = async (e) => { e.preventDefault(); if (!confirm('Remove the button ' + b.name + '?')) return; const list = (S.builds[projKey()] || []).filter((y) => y !== b); S = await call('settings_set', { patch: { builds: { ...S.builds, [projKey()]: list } } }); drawSaved(); }; box.append(x); }
}
async function run(cmd) {
  $('bOut').textContent = '$ ' + cmd + '\n';
  try { runId = await call('run_start', { path: cwd, cmd }); } catch (e) { fail(e); }
}
$('bRun').onclick = () => run($('bCmd').value);
$('bCmd').addEventListener('keydown', (e) => { if (e.key === 'Enter') run($('bCmd').value); });
$('bStop').onclick = () => { if (runId) call('run_stop', { id: runId }); };
$('bSave').onclick = async () => { const cmd = $('bCmd').value.trim(); if (!cmd) return; const name = prompt('Button name', cmd.split(' ').slice(0, 2).join(' ')); if (!name) return; const list = [...((S.builds && S.builds[projKey()]) || []), { name, cmd }]; try { S = await call('settings_set', { patch: { builds: { ...(S.builds || {}), [projKey()]: list } } }); drawSaved(); } catch (e) { fail(e); } };
T.event.listen('run', (ev) => { const p = ev.payload; if (p.id !== runId) return; const o = $('bOut'); if (p.done) { o.append(el('div', p.code === 0 ? '' : 'err', `[finished with code ${p.code}]`)); runId = 0; } else o.append(el('div', p.err ? 'err' : '', p.line.replace(ANSI, ''))); o.scrollTop = o.scrollHeight; });

// ---------- git ----------
async function git(args) {
  $('gOut').textContent = 'git ' + args.join(' ') + ' …';
  try { const r = await call('git', { path: cwd, args }); $('gOut').textContent = `git ${args.join(' ')}  (${r.ok ? 'ok' : 'failed, code ' + r.code})\n\n${r.out}${r.err}`; if (args[0] === 'clone') list(); } catch (e) { $('gOut').textContent = String(e); }
}
const words = (s) => (s.match(/"[^"]*"|\S+/g) || []).map((w) => w.replace(/^"|"$/g, ''));
for (const b of document.querySelectorAll('[data-git]')) b.onclick = () => git(words(b.dataset.git));
$('gRun').onclick = () => git(words($('gArgs').value.replace(/^git\s+/, '')));
$('gCommit').onclick = async () => { const m = $('gMsg').value.trim(); if (!m) return toast('Write a commit message first.', true); await git(['add', '-A']); await git(['commit', '-m', m]); };
$('gCloneBtn').onclick = () => { const u = $('gClone').value.trim(); if (u) git(['clone', u]); };

// ---------- the AI host ----------
async function hostRefresh() {
  const s = await call('host_status').catch((e) => ({ error: String(e) }));
  const box = $('hostState'); box.textContent = '';
  if (s.configured === false) { box.append(el('span', 'bad', 'Not set up on this computer yet.'), ' On the hub: Settings → My AI hosts → Run on this PC.'); return; }
  if (s.error) { box.append(el('span', 'bad', s.error)); return; }
  box.append(el('b', '', s.nickname || 'AI host'), ' · ', el('span', s.running ? (s.paused ? '' : 'ok') : 'bad', s.running ? (s.paused ? 'paused' : s.connected ? 'running, connected to ' + (s.hub || 'the hub') : 'running, not connected yet') : 'not running'), ' · start at boot: ' + (s.boot || 'off') + ' · version ' + (s.version || '?'));
  if (s.lastError) box.append(el('div', 'bad', s.lastError));
}
for (const b of document.querySelectorAll('[data-host]')) b.onclick = async () => { try { $('hostOut').textContent = await call('host_act', { what: b.dataset.host }); } catch (e) { $('hostOut').textContent = String(e); } setTimeout(hostRefresh, 1500); };

// ---------- the API proxy ----------
async function proxyRefresh() { const p = await call('proxy_status'); $('pPort').value = S.proxyPort; $('pState').textContent = p.running ? 'Running: ' + p.url : 'Stopped' + (S.hasProxyKey ? '' : ' (no key saved yet)'); }
$('pSave').onclick = async () => { try { S = await call('settings_set', { patch: { proxyPort: Number($('pPort').value) || 8789, proxyKey: $('pKey').value.trim() } }); $('pKey').value = ''; toast('Saved.'); proxyRefresh(); } catch (e) { fail(e); } };
$('pStart').onclick = async () => { try { await call('proxy_start'); proxyRefresh(); } catch (e) { fail(e); } };
$('pStop').onclick = async () => { await call('proxy_stop'); proxyRefresh(); };

// ---------- settings + updates ----------
function fill() { $('sHub').value = S.hub || ''; $('sProjects').value = S.projects || ''; $('sFjUrl').value = S.forgejoUrl || ''; $('sFjTok').placeholder = S.hasForgejoToken ? '(saved)' : '(none)'; $('sTray').checked = !!S.closeToTray; $('sAuto').checked = !!S.autoUpdate; $('gitAuth').textContent = S.hasForgejoToken ? 'Forgejo: signed in with your token' : 'Forgejo: no token (Settings)'; }
$('sPick').onclick = async () => { const f = await call('pick_folder').catch(fail); if (f) $('sProjects').value = f; };
$('sSave').onclick = async () => {
  try { S = await call('settings_set', { patch: { hub: $('sHub').value.trim(), projects: $('sProjects').value, forgejoUrl: $('sFjUrl').value.trim(), forgejoToken: $('sFjTok').value.trim(), closeToTray: $('sTray').checked, autoUpdate: $('sAuto').checked } }); $('sFjTok').value = ''; fill(); cwd = ''; list(); $('sState').textContent = 'Saved. A new hub address applies at the next start.'; } catch (e) { $('sState').textContent = String(e); }
};
$('uCheck').onclick = async () => { $('uState').textContent = 'checking…'; try { const u = await call('update_check'); $('uState').textContent = u.available ? `version ${u.version} is ready (you have ${u.current})` : `up to date (${u.current})`; } catch (e) { $('uState').textContent = String(e); } };
$('uApply').onclick = async () => { $('uState').textContent = 'updating…'; try { const r = await call('update_apply'); if (!r) $('uState').textContent = 'already up to date'; } catch (e) { $('uState').textContent = String(e); } };

(async () => {
  const info = await call('app_info'); document.body.classList.add(info.os === 'windows' ? 'win' : 'lin');
  $('ver').textContent = 'VibeRoller Desktop ' + info.version;
  S = await call('settings_get'); fill(); list();
  // the build check (vr-desktop --smoke): report that this page loaded, ran and reached the app
  if (location.hash === '#smoke') { const h = await call('host_status').catch((e) => ({ error: String(e) })); const p = await call('proxy_status'); await call('notify', { title: 'smoke-ok', body: JSON.stringify({ version: info.version, os: info.os, settings: !!S.hub, host: 'configured' in h || !!h.error, proxy: p.running === false }) }); }
})();
