// /app-signin (T-371): the browser sign-in for Jcode PGM. The tool opened this page with
// ?client&device&redirect&state&challenge. The hub's `check` says whether that request is one it will honour; only then
// is anything shown, and the browser only ever goes to the redirect that `check` accepted (a loopback address of the
// tool): with a code when the person approves, with error=access_denied when they cancel. The code is never shown.
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const q = new URLSearchParams(location.search);
  const req = { client: q.get('client') || '', device: q.get('device') || '', redirect: q.get('redirect') || '', state: q.get('state') || '', challenge: q.get('challenge') || '' };
  let token = null, user = null, who = { device: '', client: '' };

  function show(id) { for (const s of ['busy', 'bad', 'login', 'ask', 'done']) $(s).hidden = s !== id; }
  function fail(text) { $('badText').textContent = text; $('title').textContent = 'Cannot link this device'; show('bad'); }
  async function api(path, opts) {
    const r = await fetch(path, opts);
    let data = {}; try { data = await r.json(); } catch {}
    if (!r.ok) { const e = new Error(data.error || 'The request failed.'); e.status = r.status; throw e; }
    return data;
  }
  // the same browser id the VibeRoller page uses (the hub locks an account to the devices that signed in)
  function deviceId() {
    let id = null; try { id = localStorage.getItem('mills.device'); } catch {}
    if (!id) { id = crypto.randomUUID() + crypto.randomUUID(); try { localStorage.setItem('mills.device', id); } catch {} }
    return id;
  }
  // where the browser goes back to: the validated redirect, nothing else
  function back(params) {
    const to = req.redirect + '?' + params;
    if (!to.startsWith(req.redirect + '?')) return;
    show('done');
    $('title').textContent = 'Done';
    $('doneText').textContent = 'Going back to the app. You can close this tab afterwards.';
    location.replace(to);
  }
  function deny() { back('error=access_denied&state=' + encodeURIComponent(req.state)); }

  function ask() {
    $('title').textContent = 'Link a device';
    const dev = document.createElement('b'), cli = document.createElement('b'), acc = document.createElement('b');
    dev.textContent = who.device; cli.textContent = who.client; acc.textContent = user;
    const qn = $('question'); qn.textContent = '';
    qn.append('Link ', dev, ' (', cli, ') to your account ', acc, '?');
    show('ask');
    $('approve').focus();
  }

  $('approve').addEventListener('click', async () => {
    $('approve').disabled = true; $('cancel').disabled = true; $('askErr').textContent = '';
    try {
      const r = await api('/api/app-signin/authorize', { method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token }, body: JSON.stringify(req) });
      const to = typeof r.redirectTo === 'string' ? r.redirectTo : '';
      if (!to.startsWith(req.redirect + '?')) throw new Error('The hub answered with an unexpected address.');
      show('done'); $('title').textContent = 'Done';
      $('doneText').textContent = 'Going back to the app. You can close this tab afterwards.';
      location.replace(to);
    } catch (e) { $('askErr').textContent = e.message; $('approve').disabled = false; $('cancel').disabled = false; }
  });
  $('cancel').addEventListener('click', deny);
  $('loginCancel').addEventListener('click', deny);

  $('login').addEventListener('submit', async (e) => {
    e.preventDefault();
    $('loginErr').textContent = '';
    const username = $('username').value.trim().toLowerCase(), password = $('password').value;
    if (!username || !password) { $('loginErr').textContent = 'Enter your username and password.'; return; }
    try {
      const d = await api('/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username, password, deviceId: deviceId(), client: { tz: Intl.DateTimeFormat().resolvedOptions().timeZone, locale: navigator.language } }) });
      token = d.token; user = d.username; $('password').value = '';
      ask();
    } catch (err) { $('loginErr').textContent = err.message; }
  });

  (async () => {
    try {
      const p = new URLSearchParams(req).toString();
      const c = await api('/api/app-signin/check?' + p);
      if (!c.ok) throw new Error('This request is not valid.');
      who = { device: c.device, client: c.client };
    } catch (e) { return fail(e.message); }
    // already signed in on this page (the same tab or app window)?
    let t = null; try { t = sessionStorage.getItem('mills.token'); } catch {}
    if (t) {
      try { const me = await api('/api/check', { headers: { Authorization: 'Bearer ' + t } }); token = t; user = me.username; return ask(); } catch {}
    }
    $('title').textContent = 'Link a device';
    try { $('username').value = (localStorage.getItem('mills.username') || '').toLowerCase(); } catch {}
    show('login');
    $(($('username').value ? 'password' : 'username')).focus();
  })();
})();
