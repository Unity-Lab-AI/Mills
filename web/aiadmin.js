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

  // ── Team OpenRouter: the team's key(s) and models, the personal-keys switch, everyone's month ──
  async function renderTeamOr() {
    const box = $id('agOrBox'); if (!box) return;
    let provs = []; try { provs = ((await json('/api/providers')).providers || []).filter((p) => p.kind === 'openrouter' && !p.personal); } catch (e) { say(box, e.message); return; }
    box.textContent = '';
    if (!provs.length) box.append(mk('div', 'status', 'No team OpenRouter key yet: add one below. Work on an OpenRouter model then runs Jcode PGM on this server.'));
    for (const p of provs) {
      const it = mk('div', 'provItem'); it.append(mk('b', '', p.label));
      it.append(mk('small', '', (p.owner === me.username ? 'yours' : nickOf(p.owner) + '\'s') + (p.shared ? ' · shared with the team' : ' · private') + ' · ' + (p.pairs || []).length + ' model' + ((p.pairs || []).length === 1 ? '' : 's') + ((p.pairs || []).length ? ': ' + p.pairs.map((x) => x.label || x.id).join(', ') : '')));
      if (p.owner === me.username) {
        const sh = mk('button', 'ghost', p.shared ? 'Make private' : 'Share with team'); sh.type = 'button';
        sh.onclick = () => json('/api/providers/update', { method: 'POST', body: JSON.stringify({ id: p.id, shared: !p.shared }) }).then(() => { renderTeamOr(); refreshProviders(); }).catch((e) => sh.after(mk('span', 'status', e.message)));
        const rm = confirmButton('Remove', () => json('/api/providers/remove', { method: 'POST', body: JSON.stringify({ id: p.id }) }).then(() => { renderTeamOr(); refreshProviders(); }).catch((e) => rm.after(mk('span', 'status', e.message))));
        it.append(sh, rm, orProviderEditor(p, renderTeamOr));
      }
      box.append(it);
    }
    // add the team's key: a provider of kind openrouter (the hub lets only Manage AI add one)
    const add = mk('details'); add.open = !provs.length; add.append(mk('summary', '', 'Add a team OpenRouter key'));
    const label = mk('input'); label.placeholder = 'label, e.g. "Team OpenRouter"'; label.maxLength = 60;
    const key = mk('input'); key.type = 'password'; key.autocomplete = 'off'; key.placeholder = 'OpenRouter API key (sk-or-…; kept on the hub, never shown again)';
    const pe = pairsEditor([]);
    const shl = mk('label', 'chk'), shc = mk('input'); shc.type = 'checkbox'; shc.checked = true; shl.append(shc, ' share with the team');
    const go = mk('button', '', 'Add'); go.type = 'button'; const note = mk('span', 'status');
    go.onclick = async () => {
      try { await json('/api/providers/add', { method: 'POST', body: JSON.stringify({ kind: 'openrouter', label: label.value, apiKey: key.value, pairs: pe.get(), shared: shc.checked }) }); renderTeamOr(); refreshProviders(); renderOrBudget(); }
      catch (e) { note.textContent = e.message; }
    };
    const row = mk('div', 'row'); row.append(label, key, shl, go, note); add.append(pe.el, row); box.append(add);
    // the personal-keys switch
    let mykey = null; try { mykey = await json('/api/openrouter/mykey'); } catch {}
    if (mykey) {
      const row2 = mk('div', 'row'), lab = mk('label', 'chk'), sw = mk('input'); sw.type = 'checkbox'; sw.checked = mykey.personalKeys;
      lab.append(sw, ' Allow personal OpenRouter keys (people may pay with a key of their own instead of the team\'s)'); const n2 = mk('span', 'status');
      sw.onchange = () => json('/api/openrouter/personal', { method: 'POST', body: JSON.stringify({ on: sw.checked }) }).then(() => { n2.textContent = sw.checked ? 'Allowed.' : 'Off: stored personal keys stay but are not used.'; renderOrBudget(); refreshProviders(); }).catch((e) => { n2.textContent = e.message; sw.checked = !sw.checked; });
      row2.append(lab, n2); box.append(row2);
    }
    // everyone's month on the team key, and each person's cap
    let all; try { all = await json('/api/openrouter/budget/all'); } catch { return; }
    const det = mk('details'); det.open = true; det.append(mk('summary', '', 'Everyone\'s month on the team key · default cap ' + usd(all.defaultCapUsd)));
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
