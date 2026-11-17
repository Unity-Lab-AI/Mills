// AI Admin (T-379): the team's AI and your own on one screen. Hub side: GET /api/admin/ai (Manage AI or See AI hosts: the
// default AI, its choices, every AI host, everyone's Unity tools), POST /api/admin/ai/default, the OpenRouter routes
// (/api/openrouter/*, /api/providers/*) and /api/admin/unity-tools; the hub checks each against the person's site role.
// Global (top): Default AI for everyone, Team OpenRouter, AI hosts on this hub, Unity's own-computer tools, each shown to
// the roles allowed it (siteCan; an older hub: the admin flag). Yours (under a line): AI source, My AIs, My OpenRouter,
// My AI hosts, drawn by the page's own functions (index.html). Loaded after the page script; uses its helpers at call time.
'use strict';
const AIADMIN = (() => {
  const $id = (id) => document.getElementById(id);
  const mk = (tag, cls, txt) => { const e = document.createElement(tag); if (cls) e.className = cls; if (txt != null) e.textContent = txt; return e; };
  const usd = (n) => '$' + Number(n || 0).toFixed(2);
  const say = (box, t) => { box.textContent = ''; box.append(mk('div', 'status', t)); };

  function open() {
    const manage = siteCan('MANAGE_AI'), see = manage || siteCan('VIEW_AI_HOSTS');
    $id('aiGlobal').hidden = !see; $id('aiSplit').hidden = !see;
    for (const id of ['agDefault', 'agOpenRouter', 'agUnity']) $id(id).hidden = !manage;
    $id('agHosts').hidden = !see;
    if (see) loadGlobal(manage);
    renderSource(); renderProviders(); renderOrBudget(); renderHosts(); // yours: the page's own (index.html)
  }
  async function loadGlobal(manage) {
    let d; try { d = await json('/api/admin/ai'); } catch (e) { d = { error: e.message }; }
    if (manage) { renderDefault(d); renderTeamOr(); renderUnity(d); }
    renderAllHosts(d);
  }

  // ── Default AI for everyone: "" (the hub's own order) or one person's AI host ──
  function renderDefault(d) {
    const box = $id('agDefaultBox'); if (!box) return;
    if (!d || !Array.isArray(d.choices)) { say(box, d && d.error ? d.error : 'This hub cannot set it from here (it needs a newer hub).'); return; }
    box.textContent = '';
    const sel = mk('select'); sel.setAttribute('aria-label', 'The default AI for everyone');
    for (const c of d.choices) { const o = mk('option', '', c.label); o.value = c.value; sel.append(o); }
    sel.value = d.defaultProvider || ''; if (sel.selectedIndex < 0) sel.value = '';
    const note = mk('span', 'status');
    sel.onchange = async () => {
      try { await json('/api/admin/ai/default', { method: 'POST', body: JSON.stringify({ defaultProvider: sel.value }) }); note.textContent = 'Saved: everyone on Default uses it from their next message.'; refreshProviders(); renderSource(); }
      catch (e) { note.textContent = e.message; sel.value = d.defaultProvider || ''; }
    };
    const row = mk('div', 'row'); row.append(sel, note); box.append(row);
    if (d.hubClaude === false) box.append(mk('div', 'status', 'This hub has no Claude of its own: the default is always an AI host.'));
  }

  // ── Team OpenRouter (T-381): what is left and the key's limits for everyone together, the team's key (and an optional
  // management key, for the account's credits), the entries (named fallback chains: model 1 and its fallbacks, each a model
  // on one exact provider endpoint, picked from OpenRouter's catalogue with its price, speed and data policy), the personal-
  // keys switch and everyone's month. The hub is the host of these entries: Work's picker shows them as "This hub · OpenRouter".
  const money = (n) => (n == null || !isFinite(n) ? '–' : '$' + Number(n).toFixed(2));
  const perM = (n) => (n == null ? '–' : '$' + (n >= 10 ? n.toFixed(1) : n.toFixed(2)));
  const DATA_WORDS = { zdr: 'No retention', safety: 'Retains, no training', train: 'Retains, may train' };
  async function renderTeamOr() {
    const box = $id('agOrBox'); if (!box) return;
    let provs = []; try { provs = ((await json('/api/providers')).providers || []).filter((p) => p.kind === 'openrouter' && !p.personal); } catch (e) { say(box, e.message); return; }
    let st = null; try { st = await json('/api/openrouter/team'); } catch {}
    box.textContent = '';
    const team = provs.find((p) => p.owner === me.username) || provs[0] || null;
    box.append(statusCard(st), limitsForm(st), keysForm(team, st));
    if (team) box.append(entriesBox(team));
    await personalSwitch(box);
    await everyonesMonth(box);
  }
  /** What is left: the account's credits, the three windows against the hub's limits, the key's own counters. */
  function statusCard(st) {
    const c = mk('div', 'orStatus');
    if (!st) { c.append(mk('div', 'status', 'This hub cannot tell the team key\'s limits (it needs a newer hub).')); return c; }
    const cr = st.credits;
    c.append(mk('div', 'orCredits', cr ? 'Credits left on the account: ' + money(cr.leftUsd) + ' (of ' + money(cr.totalUsd) + ' bought, ' + money(cr.usedUsd) + ' used)' : (st.creditsWhy || 'No team key yet.')));
    const names = { fiveHour: 'Last 5 hours', week: 'Last 7 days', month: 'This month' };
    for (const k of ['fiveHour', 'week', 'month']) {
      const w = (st.windows || {})[k]; if (!w) continue;
      const row = mk('div', 'orWin' + (w.over ? ' over' : ''));
      const meter = mk('div', 'orMeter'), fill = mk('span'); fill.style.width = (w.capUsd > 0 ? Math.min(100, Math.round(w.spentUsd / w.capUsd * 100)) : 100) + '%'; meter.append(fill);
      meter.setAttribute('role', 'img'); meter.setAttribute('aria-label', names[k] + ': ' + money(w.spentUsd) + ' of ' + money(w.capUsd));
      row.append(mk('span', 'orWinName', names[k]), meter, mk('span', 'orWinNum', money(w.spentUsd) + ' of ' + money(w.capUsd) + (w.over && w.freesAt ? ' · frees up ' + (k === 'month' ? 'on ' : 'at ') + w.freesAt : '')));
      c.append(row);
    }
    const k = st.key;
    if (k) c.append(mk('div', 'status', 'The key on OpenRouter: ' + (k.limit == null ? 'no limit of its own' : 'its limit ' + money(k.limit) + (k.limitRemaining != null ? ', ' + money(k.limitRemaining) + ' left' : '')) + ' · spent today ' + money(k.usageDaily) + ', this week ' + money(k.usageWeekly) + ', this month ' + money(k.usageMonthly)));
    return c;
  }
  /** The hub's limits on the team key (everyone together): 5 h and 7 d rolling, the calendar month. */
  function limitsForm(st) {
    const f = mk('div', 'orLimits'); f.append(mk('h4', '', 'Limits for everyone together'));
    const caps = (st && st.caps) || { fiveHourUsd: 10, weekUsd: 20, monthUsd: 20 };
    const inp = (label, v) => { const l = mk('label', 'orLim'), i = mk('input'); i.type = 'number'; i.min = '0'; i.step = '0.5'; i.value = String(v); i.setAttribute('aria-label', label); l.append(mk('span', '', label), i); f.append(l); return i; };
    const a = inp('Per 5 hours ($)', caps.fiveHourUsd), b = inp('Per 7 days ($)', caps.weekUsd), c = inp('Per month ($)', caps.monthUsd);
    const lim = st && st.key && st.key.limit != null ? st.key.limit : null;
    f.append(mk('p', 'setHint', lim != null ? 'The key itself is limited to ' + money(lim) + ' on OpenRouter: the hub\'s limits cannot be higher.' : 'The key has no limit of its own on OpenRouter, so these may be higher than the credits on the account; the account\'s balance is the real stop (no top-up happens by itself).'));
    const save = mk('button', '', 'Save limits'); save.type = 'button'; const note = mk('span', 'status');
    save.onclick = async () => { try { await json('/api/openrouter/team/caps', { method: 'POST', body: JSON.stringify({ fiveHourUsd: Number(a.value), weekUsd: Number(b.value), monthUsd: Number(c.value) }) }); note.textContent = 'Saved.'; renderTeamOr(); } catch (e) { note.textContent = e.message; } };
    const row = mk('div', 'row'); row.append(save, note); f.append(row);
    return f;
  }
  /** The team's key (set, or replace: never shown) and the optional management key (only for the account's credits). */
  function keysForm(team, st) {
    const f = mk('div', 'orKeys'); f.append(mk('h4', '', 'Keys'));
    const key = mk('input'); key.type = 'password'; key.autocomplete = 'off'; key.placeholder = team ? 'Replace the team key: paste a new one (sk-or-…)' : 'The team\'s OpenRouter key (sk-or-…; kept on the hub, never shown again)'; key.setAttribute('aria-label', 'The team\'s OpenRouter key');
    const kb = mk('button', '', team ? 'Replace key' : 'Set the team key'); kb.type = 'button'; const note = mk('span', 'status');
    kb.onclick = async () => {
      if (!key.value.trim()) { note.textContent = 'Paste a key first.'; return; }
      try {
        if (team) await json('/api/providers/update', { method: 'POST', body: JSON.stringify({ id: team.id, apiKey: key.value.trim() }) });
        else await json('/api/providers/add', { method: 'POST', body: JSON.stringify({ kind: 'openrouter', label: 'Team OpenRouter', apiKey: key.value.trim(), pairs: [], shared: true }) });
        key.value = ''; refreshProviders(); renderTeamOr();
      } catch (e) { note.textContent = e.message; }
    };
    const r1 = mk('div', 'row'); r1.append(key, kb); if (team) r1.append(mk('span', 'status', 'Set · ' + (team.label || 'Team OpenRouter'))); f.append(r1);
    const mkey = mk('input'); mkey.type = 'password'; mkey.autocomplete = 'off'; mkey.placeholder = st && st.hasManagementKey ? 'A management key is set: paste another to replace it' : 'Optional: a management key, to see the account\'s credits'; mkey.setAttribute('aria-label', 'OpenRouter management key');
    const mb = mk('button', 'ghost', 'Save'); mb.type = 'button';
    mb.onclick = async () => { try { await json('/api/openrouter/team/management-key', { method: 'POST', body: JSON.stringify({ apiKey: mkey.value.trim() }) }); mkey.value = ''; renderTeamOr(); } catch (e) { note.textContent = e.message; } };
    const r2 = mk('div', 'row'); r2.append(mkey, mb);
    if (st && st.hasManagementKey) { const rm = mk('button', 'ghost', 'Remove'); rm.type = 'button'; rm.onclick = async () => { try { await json('/api/openrouter/team/management-key', { method: 'POST', body: JSON.stringify({ apiKey: '' }) }); renderTeamOr(); } catch (e) { note.textContent = e.message; } }; r2.append(rm); }
    f.append(r2, note);
    return f;
  }
  // ── the entries: named fallback chains ──
  const slug = (t) => t.toLowerCase().replace(/[^a-z0-9._:@/-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60);
  const stepsOf = (e) => (Array.isArray(e.steps) && e.steps.length ? e.steps : [{ model: e.model, provider: (e.providers || [])[0] || '', label: '' }]);
  const stepName = (st) => st.label || (st.provider ? st.model + ' via ' + st.provider : st.model);
  function entriesBox(team) {
    const f = mk('div', 'orEntries'); f.append(mk('h4', '', 'Entries'));
    f.append(mk('p', 'setHint', 'Each entry is what people pick in Work and the API (by its name), hosted by this hub: model 1, and when its provider is unavailable the next fallback, in order. A switch is said in the session: "model 1: … unavailable, switching to model 2: …".'));
    const entries = (team.pairs || []).map((e) => ({ id: e.id, label: e.label || e.id, steps: stepsOf(e).map((s) => ({ model: s.model, provider: s.provider || '', label: s.label || '' })) }));
    const note = mk('div', 'status');
    const save = async (list, said) => { try { await json('/api/providers/update', { method: 'POST', body: JSON.stringify({ id: team.id, pairs: list.map((e) => ({ id: e.id, label: e.label, steps: e.steps })) }) }); note.textContent = said || 'Saved.'; refreshProviders(); renderTeamOr(); } catch (e) { note.textContent = e.message; } };
    if (!entries.length) f.append(mk('div', 'status', 'No entries yet: add one below.'));
    entries.forEach((e, ei) => {
      const card = mk('div', 'orEntry');
      const head = mk('div', 'row'); const name = mk('input'); name.value = e.label; name.maxLength = 60; name.setAttribute('aria-label', 'The entry\'s name');
      const rn = mk('button', 'ghost', 'Rename'); rn.type = 'button'; rn.onclick = () => { const n = name.value.trim(); if (!n) return; const list = entries.slice(); list[ei] = { ...e, label: n }; save(list, 'Renamed.'); };
      const del = confirmButton('Remove entry', () => save(entries.filter((_, i) => i !== ei), 'Removed.'));
      head.append(name, rn, del, mk('small', 'status', 'picked as ' + e.id)); card.append(head);
      const ol = mk('ol', 'orSteps');
      e.steps.forEach((s, si) => {
        const li = mk('li'); li.append(mk('span', '', (si === 0 ? 'Model 1: ' : 'Fallback ' + si + ': ') + stepName(s)));
        const acts = mk('span', 'orStepActs');
        if (si > 0) { const up = mk('button', 'ghost', '↑'); up.type = 'button'; up.title = 'Try it earlier'; up.onclick = () => { const st2 = e.steps.slice(); [st2[si - 1], st2[si]] = [st2[si], st2[si - 1]]; const list = entries.slice(); list[ei] = { ...e, steps: st2 }; save(list); }; acts.append(up); }
        if (e.steps.length > 1) { const x = mk('button', 'ghost', 'Remove'); x.type = 'button'; x.onclick = () => { const list = entries.slice(); list[ei] = { ...e, steps: e.steps.filter((_, i) => i !== si) }; save(list); }; acts.append(x); }
        li.append(acts); ol.append(li);
      });
      card.append(ol);
      const more = mk('button', 'ghost', 'Add another fallback'); more.type = 'button';
      more.onclick = () => openPicker(card, (step) => { const list = entries.slice(); list[ei] = { ...e, steps: e.steps.concat([step]) }; save(list, 'Fallback added.'); });
      card.append(more); f.append(card);
    });
    const add = mk('button', '', 'Add a fallback'); add.type = 'button';
    add.onclick = () => openPicker(f, (step, name) => {
      const n = (name || step.label || step.model).trim(); let id = slug(n) || 'entry'; let k = 2; while (entries.some((x) => x.id === id)) id = slug(n) + '-' + k++;
      save(entries.concat([{ id, label: n, steps: [step] }]), 'Added "' + n + '".');
    }, true);
    f.append(add, note);
    return f;
  }
  // ── the picker: search a model, then pick one of its providers (OpenRouter's catalogue, the same data jcode reads) ──
  function openPicker(host, done, askName) {
    const old = host.querySelector(':scope > .orPicker'); if (old) { old.remove(); return; }
    const p = mk('div', 'orPicker');
    const q = mk('input'); q.type = 'search'; q.placeholder = 'Search OpenRouter\'s models, e.g. deepseek, qwen coder, claude'; q.setAttribute('aria-label', 'Search models');
    const close = mk('button', 'ghost', 'Close'); close.type = 'button'; close.onclick = () => p.remove();
    const top = mk('div', 'row'); top.append(q, close); p.append(top);
    const list = mk('div', 'orModels'), prov = mk('div', 'orProviders'), info = mk('div', 'status', 'Type to search; the newest models come first.');
    p.append(info, list, prov);
    let timer = 0, seq = 0;
    const search = async () => {
      const my = ++seq; info.textContent = 'Searching…';
      let d; try { d = await json('/api/openrouter/catalog/models?q=' + encodeURIComponent(q.value.trim())); } catch (e) { info.textContent = e.message; return; }
      if (my !== seq) return;
      list.textContent = ''; prov.textContent = '';
      info.textContent = d.total ? d.total + ' model' + (d.total === 1 ? '' : 's') + (d.total > d.models.length ? ' (the first ' + d.models.length + ' shown)' : '') : 'No model matches that.';
      for (const m of d.models.slice(0, 60)) {
        const b = mk('button', 'ghost orModel'); b.type = 'button';
        b.append(mk('b', '', m.name), mk('small', '', m.id + ' · in ' + perM(m.inPerM) + '/M · out ' + perM(m.outPerM) + '/M' + (m.context ? ' · ' + Math.round(m.context / 1000) + 'k context' : '')));
        b.onclick = () => showProviders(m);
        list.append(b);
      }
    };
    q.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 250); });
    const showProviders = async (m) => {
      prov.textContent = 'Loading ' + m.name + '\'s providers…';
      let d; try { d = await json('/api/openrouter/catalog/endpoints?model=' + encodeURIComponent(m.id)); } catch (e) { prov.textContent = e.message; return; }
      prov.textContent = '';
      prov.append(mk('h4', '', m.name + ': pick a provider'));
      const filt = mk('select'); filt.setAttribute('aria-label', 'Data policy');
      for (const [v, l] of [['', 'Any data policy'], ['zdr', 'No retention only'], ['nott', 'No training (no retention or safety retention)']]) { const o = mk('option', '', l); o.value = v; filt.append(o); }
      const sort = mk('select'); sort.setAttribute('aria-label', 'Sort by');
      for (const [v, l] of [['tps', 'Fastest first'], ['in', 'Cheapest input first'], ['out', 'Cheapest output first'], ['uptime', 'Most reliable first'], ['latency', 'Quickest first token']]) { const o = mk('option', '', l); o.value = v; sort.append(o); }
      const pq = mk('input'); pq.type = 'search'; pq.placeholder = 'Filter providers'; pq.setAttribute('aria-label', 'Filter providers');
      const name = mk('input'); name.placeholder = 'Name it, e.g. deepseek'; name.maxLength = 60; name.setAttribute('aria-label', 'The entry\'s name'); name.hidden = !askName;
      const ctl = mk('div', 'row'); ctl.append(pq, filt, sort, name); prov.append(ctl);
      const t = mk('table', 'orProvTable'); prov.append(t);
      const paint = () => {
        t.textContent = '';
        const hr = mk('tr'); for (const h of ['Provider', 'In $/M', 'Out $/M', 'Speed', 'First token', 'Uptime', 'Context', 'Data policy', '']) hr.append(mk('th', '', h)); t.append(hr);
        const words = pq.value.toLowerCase().split(/\s+/).filter(Boolean);
        let eps = (d.endpoints || []).filter((e) => (!filt.value || (filt.value === 'zdr' ? e.data === 'zdr' : e.data !== 'train')) && words.every((w) => (e.provider + ' ' + (e.route || '') + ' ' + (e.quantization || '')).toLowerCase().includes(w)));
        const num = (x, d0) => (x == null ? d0 : x);
        const by = { tps: (a, b) => num(b.tpsP50, -1) - num(a.tpsP50, -1), in: (a, b) => num(a.inPerM, 1e9) - num(b.inPerM, 1e9), out: (a, b) => num(a.outPerM, 1e9) - num(b.outPerM, 1e9), uptime: (a, b) => num(b.uptime30m, -1) - num(a.uptime30m, -1), latency: (a, b) => num(a.latencyP50Ms, 1e9) - num(b.latencyP50Ms, 1e9) };
        eps = eps.slice().sort(by[sort.value]);
        if (!eps.length) { const tr = mk('tr'), td = mk('td', 'status', 'No provider matches.'); td.colSpan = 9; tr.append(td); t.append(tr); }
        for (const e of eps) {
          const tr = mk('tr');
          const pn = mk('td'); pn.append(mk('b', '', e.provider)); if (e.quantization) pn.append(mk('small', '', ' ' + e.quantization)); pn.title = e.route + (e.detail ? ' · ' + e.detail : '');
          const dp = mk('td'); dp.append(mk('span', 'orData ' + e.data, DATA_WORDS[e.data] || e.dataLabel));
          const use = mk('td'); const b = mk('button', '', 'Use'); b.type = 'button';
          b.onclick = () => { const label = m.name.replace(/^[^:]+:\s*/, '') + ' via ' + e.provider + (e.quantization ? ' (' + e.quantization + ')' : ''); done({ model: d.model.id || m.id, provider: e.route, label }, name.value.trim()); p.remove(); };
          use.append(b);
          tr.append(pn, mk('td', '', perM(e.inPerM)), mk('td', '', perM(e.outPerM)), mk('td', '', e.tpsP50 != null ? Math.round(e.tpsP50) + ' t/s' : '–'), mk('td', '', e.latencyP50Ms != null ? (e.latencyP50Ms / 1000).toFixed(2) + ' s' : '–'),
            mk('td', '', e.uptime30m != null ? Math.round(e.uptime30m) + '%' : '–'), mk('td', '', e.context ? Math.round(e.context / 1000) + 'k' : '–'), dp, use);
          t.append(tr);
        }
      };
      for (const el of [filt, sort]) el.onchange = paint; pq.oninput = paint; paint();
    };
    host.append(p); q.focus(); search();
  }
  async function personalSwitch(box) {
    let mykey = null; try { mykey = await json('/api/openrouter/mykey'); } catch {}
    if (!mykey) return;
    const row2 = mk('div', 'row'), lab = mk('label', 'chk'), sw = mk('input'); sw.type = 'checkbox'; sw.checked = mykey.personalKeys;
    lab.append(sw, ' Allow personal OpenRouter keys (people may pay with a key of their own instead of the team\'s)'); const n2 = mk('span', 'status');
    sw.onchange = () => json('/api/openrouter/personal', { method: 'POST', body: JSON.stringify({ on: sw.checked }) }).then(() => { n2.textContent = sw.checked ? 'Allowed.' : 'Off: stored personal keys stay but are not used.'; renderOrBudget(); refreshProviders(); }).catch((e) => { n2.textContent = e.message; sw.checked = !sw.checked; });
    row2.append(lab, n2); box.append(row2);
  }
  // everyone's month on the team key, and each person's cap
  async function everyonesMonth(box) {
    let all; try { all = await json('/api/openrouter/budget/all'); } catch { return; }
    const det = mk('details'); det.append(mk('summary', '', 'Each person\'s month on the team key · default cap ' + usd(all.defaultCapUsd)));
    const t = mk('table'), hr = mk('tr'); for (const h of ['Person', 'Spent', 'Cap', '']) hr.append(mk('th', '', h)); t.append(hr);
    for (const p of all.people || []) {
      const tr = mk('tr'); const cap = mk('input'); cap.type = 'number'; cap.min = '0'; cap.step = '1'; cap.value = String(p.capUsd); cap.style.width = '6rem'; cap.setAttribute('aria-label', 'Monthly cap of ' + p.username);
      const set = mk('button', 'ghost', 'Set'); set.type = 'button'; const def = mk('button', 'ghost', 'Default'); def.type = 'button'; def.hidden = !p.ownCap;
      const msg = mk('span', 'status');
      const send = (v) => json('/api/openrouter/budget/cap', { method: 'POST', body: JSON.stringify({ username: p.username, capUsd: v }) }).then(() => renderTeamOr()).catch((e) => { msg.textContent = e.message; });
      set.onclick = () => send(Number(cap.value)); def.onclick = () => send(null);
      const c3 = mk('td'); c3.append(cap, set, def, msg);
      tr.append(mk('td', '', p.name || p.username), mk('td', '', usd(p.spentUsd) + (p.over ? ' · used up' : '')), c3, mk('td', '', p.calls + ' answers')); t.append(tr);
    }
    det.append(t); box.append(det);
  }

  // ── every AI host on the hub (read-only) ──
  function renderAllHosts(d) {
    const box = $id('agHostsBox'); if (!box) return;
    if (!d || !Array.isArray(d.hosts)) { say(box, d && d.error ? d.error : 'This hub cannot list them here (it needs a newer hub).'); return; }
    box.textContent = '';
    if (!d.hosts.length) { box.append(mk('div', 'status', 'No AI host is connected to this hub yet.')); return; }
    const t = mk('table'), hr = mk('tr'); for (const h of ['Host', 'Owner', 'State', 'Version', 'In use']) hr.append(mk('th', '', h)); t.append(hr);
    for (const h of d.hosts) {
      const state = h.paused ? 'paused' : h.online ? 'online' : h.seen ? 'offline · last seen ' + new Date(h.seen).toLocaleString() : 'never connected';
      const use = (h.inUse || []).map((u) => (typeof u === 'string' ? u : (u.person ? nickOf(u.person) : u.what || 'someone') + (u.kind === 'work' ? ' (Work' + (u.project ? ', ' + u.project : '') + ')' : ''))).join(', ');
      const tr = mk('tr'); tr.append(mk('td', '', h.nickname || h.id), mk('td', '', h.owner ? nickOf(h.owner) : ''), mk('td', '', state), mk('td', '', h.version || ''), mk('td', '', use || (h.online ? 'idle' : ''))); t.append(tr);
    }
    box.append(t);
  }

  // ── Unity's own-computer tools, per person ──
  async function renderUnity(d) {
    const box = $id('agUnityBox'); if (!box) return;
    let people = d && Array.isArray(d.unity) ? d.unity : null;
    if (!people) { try { people = ((await json('/api/admin/accounts')).accounts || []).map((a) => ({ username: a.username, display: a.display, pc: a.unityPc, code: a.unityCode })); } catch (e) { say(box, e.message); return; } } // an older hub: from the accounts list
    box.textContent = '';
    const note = mk('div', 'status');
    const t = mk('table'), hr = mk('tr'); for (const h of ['Person', 'Files in their opened PC folder', 'Run code in their browser']) hr.append(mk('th', '', h)); t.append(hr);
    for (const p of people) {
      const tr = mk('tr'); tr.append(mk('td', '', (p.display || p.username) + ' (' + p.username + ')'));
      for (const key of ['pc', 'code']) {
        const td = mk('td'), cb = mk('input'); cb.type = 'checkbox'; cb.checked = !!p[key]; cb.setAttribute('aria-label', (key === 'pc' ? 'Unity: PC files for ' : 'Unity: run code for ') + p.username);
        cb.onchange = () => json('/api/admin/unity-tools', { method: 'POST', body: JSON.stringify({ username: p.username, [key]: cb.checked }) }).then((r) => { note.textContent = r.username + ': Unity PC files ' + (r.pc ? 'on' : 'off') + ', run code ' + (r.code ? 'on' : 'off') + '.'; }).catch((e) => { note.textContent = e.message; cb.checked = !cb.checked; });
        td.append(cb); tr.append(td);
      }
      t.append(tr);
    }
    box.append(t, note);
  }
  return { open };
})();
