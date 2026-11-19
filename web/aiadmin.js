// AI Admin (T-379): the team's AI and your own on one screen. Hub side: GET /api/admin/ai (Manage AI or See AI hosts: the
// default AI, its choices, every AI host, everyone's Unity tools), POST /api/admin/ai/default, the OpenRouter routes
// (/api/openrouter/*, /api/providers/*) and /api/admin/unity-tools; the hub checks each against the person's site role.
// Global (top): Default AI for everyone, Team OpenRouter keys, AI hosts on this hub, Unity's own-computer tools, each shown
// to the roles allowed it (siteCan; an older hub: the admin flag). Yours (under a line): AI source, My AIs, My OpenRouter
// (its keys from myKeys here), My AI hosts, drawn by the page's own functions (index.html). Loaded after the page script;
// uses its helpers at call time.
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

  // ── OpenRouter keys (T-381, T-383, T-387): every key has a name (it shows in the AI dropdown under it), is someone's own or
  // put into the team's keys, and may carry limits for everyone on it together (5 h and 7 d rolling, the calendar month;
  // empty = no limit) and a management key (for the account's credits). It carries the models it may be used for, each on
  // the providers its owner ticked, in their order, picked from OpenRouter's catalogue by searching and filtering. Only the
  // person who added a key changes any of that (an admin too only for a key they added). Global → Team OpenRouter keys shows
  // the team's (with everyone's month, Manage AI); Yours → My OpenRouter keys shows the person's own (index.html calls
  // myKeys). The hub hosts these models: Work's picker, the API and jcode's viberoller provider show them by name.
  const money = (n) => (n == null || !isFinite(n) ? '–' : '$' + Number(n).toFixed(2));
  const perM = (n) => (n == null ? '–' : n === 0 ? '$0' : '$' + (n >= 10 ? n.toFixed(1) : n >= 0.1 ? n.toFixed(2) : n.toFixed(3)));
  const DATA_WORDS = { zdr: 'No retention', safety: 'Retains, no training', train: 'Retains, may train' };
  // T-386: OpenRouter's API gives a data policy per endpoint as three tiers (its ZDR list); each is a marker, green, yellow or
  // red, with what OpenRouter's docs say it means. Finer marks are on its website only, and the hub reads its API only.
  const DATA_LEVEL = { zdr: 'green', safety: 'yellow', train: 'red' };
  const RANK = { green: 0, yellow: 1, red: 2 };
  // T-385: everything OpenRouter says about a provider of a model, as the picker's texts. Pure (no page), so the suite reads
  // exactly what the table shows. A deal is already taken off the price listed; the price before it is shown struck through.
  const MORE_NAMES = { cacheWrite1hPerM: 'cache write (1 hour)', reasoningPerM: 'reasoning', audioPerM: 'audio in', audioCachePerM: 'audio cache', imageTokenPerM: 'image tokens', imageOutputPerM: 'image out', imageEach: 'per input image', webSearchEach: 'per web search', requestEach: 'per request' };
  const each = (n) => '$' + (n >= 0.01 ? n.toFixed(2) : String(+n.toPrecision(3)));
  const pct = (x) => (x == null ? '–' : (Math.round(x * 10) / 10) + '%');
  const kTok = (n) => (n == null ? '' : n >= 1000 ? Math.round(n / 1000) + 'k' : String(n));
  function endpointFacts(e) {
    const pi = e.providerInfo || null;
    return {
      deal: e.discount ? Math.round(e.discount * 100) + '% off' : '',
      inBefore: e.discount && e.inPerMBefore != null ? perM(e.inPerMBefore) : '',
      outBefore: e.discount && e.outPerMBefore != null ? perM(e.outPerMBefore) : '',
      tier: e.serviceTier || '',
      cache: [e.cacheReadPerM != null ? 'read ' + perM(e.cacheReadPerM) : '', e.cacheWritePerM != null ? 'write ' + perM(e.cacheWritePerM) : ''].filter(Boolean).join(' · ') || '–',
      uptime: 'Uptime: last 5 min ' + pct(e.uptime5m) + ', 30 min ' + pct(e.uptime30m) + ', a day ' + pct(e.uptime1d),
      tiers: (e.tiers || []).map((t) => (t.minPromptTokens != null ? 'From ' + kTok(t.minPromptTokens) + ' prompt tokens' : t.fromUtc + '–' + t.toUtc + ' UTC') + ': in ' + perM(t.inPerM) + '/M, out ' + perM(t.outPerM) + '/M' + (t.cacheReadPerM != null ? ', cache read ' + perM(t.cacheReadPerM) + '/M' : '')),
      more: Object.entries(e.more || {}).filter(([k]) => MORE_NAMES[k]).map(([k, v]) => MORE_NAMES[k] + ' ' + (k.endsWith('PerM') ? perM(v) + '/M' : each(v))),
      limits: [e.context ? kTok(e.context) + ' context' : '', e.maxPrompt ? kTok(e.maxPrompt) + ' prompt at most' : '', e.maxOut ? kTok(e.maxOut) + ' out at most' : ''].filter(Boolean).join(' · '),
      params: (e.params || []).join(', '),
      where: pi ? [pi.headquarters ? 'Based in ' + pi.headquarters : '', (pi.datacenters || []).length ? 'runs in ' + pi.datacenters.join(', ') : ''].filter(Boolean).join(', ') : '',
      level: e.dataLevel || DATA_LEVEL[e.data] || 'red',
      policy: (DATA_WORDS[e.data] || e.dataLabel || '') + (e.dataMeaning ? ': ' + e.dataMeaning : ''),
      links: pi ? [['Privacy policy', pi.privacyUrl], ['Terms', pi.termsUrl], ['Status', pi.statusUrl]].filter(([, u]) => /^https?:\/\//.test(u || '')) : [],
    };
  }
  /** A model in the search list: its prices, a deal some provider gives on it, free, tiered prices. */
  function modelLine(m) {
    return m.id + ' · in ' + perM(m.inPerM) + '/M · out ' + perM(m.outPerM) + '/M' + (m.cacheReadPerM != null ? ' · cache read ' + perM(m.cacheReadPerM) + '/M' : '') + (m.context ? ' · ' + kTok(m.context) + ' context' : '')
      + ((m.tiers || []).some((t) => t.minPromptTokens != null) ? ' · another price from ' + kTok(m.tiers.find((t) => t.minPromptTokens != null).minPromptTokens) + ' prompt tokens' : '')
      + ((m.tiers || []).some((t) => t.fromUtc) ? ' · priced by time of day' : '') + (m.free ? ' · free' : '') + (m.bestDeal ? ' · a provider gives ' + Math.round(m.bestDeal * 100) + '% off' : '') + (m.zdrEndpoints ? ' · ' + m.zdrEndpoints + ' with zero data retention' : '');
  }
  /** The keys the person sees, each its status (GET /api/openrouter/keys) with its provider record (entries) beside it. */
  async function loadKeys() {
    const [k, p] = await Promise.all([json('/api/openrouter/keys'), json('/api/providers')]);
    const provs = (p.providers || []).filter((x) => x.kind === 'openrouter');
    return { manage: !!k.manage, keys: (k.keys || []).map((st) => ({ st, prov: provs.find((x) => x.id === st.id) || { id: st.id, label: st.label, pairs: [] } })) };
  }
  async function renderTeamOr() {
    const box = $id('agOrBox'); if (!box) return;
    let d; try { d = await loadKeys(); } catch (e) { say(box, e.message.includes('Not found') ? 'This hub cannot list OpenRouter keys yet (it needs a newer hub).' : e.message); return; }
    box.textContent = '';
    const team = d.keys.filter((x) => x.st.shared);
    if (!team.length) box.append(mk('div', 'status', 'The team has no OpenRouter key yet. Add one below, or anyone may put one of theirs into the team\'s keys (AI Admin → Yours).'));
    for (const x of team) box.append(keyCard(x, renderTeamOr));
    box.append(addKeyForm(true, renderTeamOr));
    await everyonesMonth(box);
  }
  /** Yours → My OpenRouter keys: the person's own keys (theirs alone, or put into the team's), and adding one. */
  async function myKeys(box, again) {
    let d; try { d = await loadKeys(); } catch { return false; }
    const mine = d.keys.filter((x) => x.st.owner === me.username);
    const f = mk('div', 'orMyKeys'); f.append(mk('h4', '', 'My OpenRouter keys'));
    f.append(mk('p', 'setHint', 'Each key shows in the AI dropdown under its name. Kept to yourself, only you use it (you pay OpenRouter; no hub budget applies). Put into the team\'s keys, the whole team may use it, within the limits you give it. The hub never shows a key again: only its last four characters.'));
    if (!mine.length) f.append(mk('div', 'status', 'No key of yours yet.'));
    for (const x of mine) f.append(keyCard(x, again));
    f.append(addKeyForm(false, again));
    box.append(f);
    return true;
  }
  /** One key: its name, who added it, what is left, its limits, its management key, its models (only who added it changes them). */
  function keyCard(x, again) {
    const { st, prov } = x; const edit = st.mayEdit !== false;
    const card = mk('div', 'orKey');
    const head = mk('div', 'row'); const name = mk('input'); name.value = st.label || ''; name.maxLength = 60; name.disabled = !edit; name.setAttribute('aria-label', 'The key\'s name');
    head.append(name);
    const note = mk('span', 'status');
    const upd = async (body, said) => { try { await json('/api/providers/update', { method: 'POST', body: JSON.stringify({ id: st.id, ...body }) }); note.textContent = said || 'Saved.'; refreshProviders(); again(); } catch (e) { note.textContent = e.message; } };
    if (edit) {
      const rn = mk('button', 'ghost', 'Rename'); rn.type = 'button'; rn.onclick = () => { const n = name.value.trim(); if (n) upd({ label: n }, 'Renamed.'); }; head.append(rn);
      if (st.owner === me.username) { const sh = mk('button', 'ghost', st.shared ? 'Keep it to myself' : 'Put it into the team\'s keys'); sh.type = 'button'; sh.onclick = () => upd({ shared: !st.shared }, st.shared ? 'Only you use it now.' : 'The whole team may use it now.'); head.append(sh); }
      const t = mk('button', 'ghost', 'Test'); t.type = 'button'; t.title = 'Ask OpenRouter whether the key works'; t.onclick = () => { note.textContent = 'Asking OpenRouter…'; json('/api/openrouter/mykey/test', { method: 'POST', body: JSON.stringify({ key: st.id }) }).then((r) => { note.textContent = r.ok ? 'OpenRouter accepts the key.' : r.error; }).catch((e) => { note.textContent = e.message; }); }; head.append(t);
      head.append(confirmButton('Remove key', () => json('/api/providers/remove', { method: 'POST', body: JSON.stringify({ id: st.id }) }).then(() => { refreshProviders(); again(); }).catch((e) => { note.textContent = e.message; })));
    }
    const adder = st.owner === me.username ? 'you' : (st.ownerName || st.owner || 'someone');
    head.append(mk('small', 'status', (st.shared ? 'team key' : 'yours alone') + ' · added by ' + adder + (st.last4 ? ' · key ' + st.last4 : '')));
    card.append(head, statusCard(st));
    if (edit) card.append(limitsForm(st, again), mgmtForm(st, again));
    card.append(modelsBox(prov, again, edit, adder));
    card.append(note);
    return card;
  }
  /** What is left: the account's credits, the three windows against the key's limits, the key's own counters, the session's. */
  function statusCard(st) {
    const c = mk('div', 'orStatus');
    const cr = st.credits;
    c.append(mk('div', 'orCredits', cr ? 'Credits left on the account: ' + money(cr.leftUsd) + ' (of ' + money(cr.totalUsd) + ' bought, ' + money(cr.usedUsd) + ' used)' : (st.creditsWhy || 'OpenRouter did not say what is left.')));
    const names = { fiveHour: 'Last 5 hours', week: 'Last 7 days', month: 'This month' };
    for (const k of ['fiveHour', 'week', 'month']) {
      const w = (st.windows || {})[k]; if (!w) continue;
      const lim = w.capUsd != null;
      const row = mk('div', 'orWin' + (w.over ? ' over' : ''));
      const meter = mk('div', 'orMeter'), fill = mk('span'); fill.style.width = (lim ? (w.capUsd > 0 ? Math.min(100, Math.round(w.spentUsd / w.capUsd * 100)) : 100) : 0) + '%'; meter.append(fill);
      meter.setAttribute('role', 'img'); meter.setAttribute('aria-label', names[k] + ': ' + money(w.spentUsd) + (lim ? ' of ' + money(w.capUsd) : ', no limit'));
      row.append(mk('span', 'orWinName', names[k]), meter, mk('span', 'orWinNum', money(w.spentUsd) + (lim ? ' of ' + money(w.capUsd) : ' · no limit') + (w.over && w.freesAt ? ' · frees up ' + (k === 'month' ? 'on ' : 'at ') + w.freesAt : '')));
      c.append(row);
    }
    const k = st.key;
    if (k) c.append(mk('div', 'status', 'The key on OpenRouter: ' + (k.limit == null ? 'no limit of its own' : 'its limit ' + money(k.limit) + (k.limitRemaining != null ? ', ' + money(k.limitRemaining) + ' left' : '')) + ' · spent today ' + money(k.usageDaily) + ', this week ' + money(k.usageWeekly) + ', this month ' + money(k.usageMonthly)));
    return c;
  }
  /** The key's limits for everyone on it together: 5 h and 7 d rolling, the calendar month; empty = no limit. */
  function limitsForm(st, again) {
    const f = mk('div', 'orLimits'); f.append(mk('h4', '', 'Limits for everyone on this key together (empty = no limit)'));
    const caps = st.caps || {};
    const inp = (label, v) => { const l = mk('label', 'orLim'), i = mk('input'); i.type = 'number'; i.min = '0'; i.step = '0.5'; i.placeholder = 'no limit'; i.value = v == null ? '' : String(v); i.setAttribute('aria-label', label); l.append(mk('span', '', label), i); f.append(l); return i; };
    const a = inp('Per 5 hours ($)', caps.fiveHourUsd), b = inp('Per 7 days ($)', caps.weekUsd), c = inp('Per month ($)', caps.monthUsd);
    const lim = st.key && st.key.limit != null ? st.key.limit : null;
    f.append(mk('p', 'setHint', lim != null ? 'The key itself is limited to ' + money(lim) + ' on OpenRouter: these cannot be higher.' : 'The key has no limit of its own on OpenRouter, so these may be higher than the credits on the account; the account\'s balance is the real stop (no top-up happens by itself). An answer already being written when a limit is reached finishes.'));
    const save = mk('button', '', 'Save limits'); save.type = 'button'; const note = mk('span', 'status');
    const val = (i) => (i.value.trim() === '' ? null : Number(i.value));
    save.onclick = async () => { try { await json('/api/openrouter/team/caps', { method: 'POST', body: JSON.stringify({ key: st.id, fiveHourUsd: val(a), weekUsd: val(b), monthUsd: val(c) }) }); note.textContent = 'Saved.'; again(); } catch (e) { note.textContent = e.message; } };
    const row = mk('div', 'row'); row.append(save, note); f.append(row);
    return f;
  }
  /** The key's optional management key (only for the account's credits; never shown). */
  function mgmtForm(st, again) {
    const f = mk('div', 'orKeys'); const note = mk('span', 'status');
    const mkey = mk('input'); mkey.type = 'password'; mkey.autocomplete = 'off'; mkey.placeholder = st.hasManagementKey ? 'A management key is set: paste another to replace it' : 'Optional: a management key, to see the account\'s credits'; mkey.setAttribute('aria-label', 'OpenRouter management key');
    const send = async (v) => { try { await json('/api/openrouter/team/management-key', { method: 'POST', body: JSON.stringify({ key: st.id, apiKey: v }) }); mkey.value = ''; again(); } catch (e) { note.textContent = e.message; } };
    const mb = mk('button', 'ghost', 'Save'); mb.type = 'button'; mb.onclick = () => send(mkey.value.trim());
    const r2 = mk('div', 'row'); r2.append(mkey, mb);
    if (st.hasManagementKey) { const rm = mk('button', 'ghost', 'Remove'); rm.type = 'button'; rm.onclick = () => send(''); r2.append(rm); }
    f.append(r2, note);
    return f;
  }
  /** Add a key: its name, the key, whether the team may use it, optional limits. */
  function addKeyForm(team, again) {
    const det = mk('details', 'orAdd'); det.append(mk('summary', '', team ? 'Add a team key' : 'Add a key'));
    const name = mk('input'); name.placeholder = 'Name it, e.g. ' + (team ? 'Team OpenRouter' : 'my OpenRouter'); name.maxLength = 60; name.setAttribute('aria-label', 'The key\'s name');
    const key = mk('input'); key.type = 'password'; key.autocomplete = 'off'; key.placeholder = 'The OpenRouter key (sk-or-…; kept on the hub, never shown again)'; key.setAttribute('aria-label', 'The OpenRouter key');
    const lab = mk('label', 'chk'), sh = mk('input'); sh.type = 'checkbox'; sh.checked = team; lab.append(sh, ' Put it into the team\'s keys (the whole team may use it)');
    const lims = mk('div', 'orLimits'); const inp = (label) => { const l = mk('label', 'orLim'), i = mk('input'); i.type = 'number'; i.min = '0'; i.step = '0.5'; i.placeholder = 'no limit'; i.setAttribute('aria-label', label); l.append(mk('span', '', label), i); lims.append(l); return i; };
    const a = inp('Per 5 hours ($)'), b = inp('Per 7 days ($)'), c = inp('Per month ($)');
    const go = mk('button', '', 'Add key'); go.type = 'button'; const note = mk('span', 'status');
    const val = (i) => (i.value.trim() === '' ? null : Number(i.value));
    go.onclick = async () => {
      if (!key.value.trim()) { note.textContent = 'Paste a key first.'; return; }
      try { await json('/api/providers/add', { method: 'POST', body: JSON.stringify({ kind: 'openrouter', label: name.value.trim() || (sh.checked ? 'Team OpenRouter' : 'my OpenRouter'), apiKey: key.value.trim(), pairs: [], shared: sh.checked, limits: { fiveHourUsd: val(a), weekUsd: val(b), monthUsd: val(c) } }) }); refreshProviders(); again(); }
      catch (e) { note.textContent = e.message; }
    };
    const r = mk('div', 'row'); r.append(go, note);
    det.append(name, key, lab, mk('p', 'setHint', 'Limits for everyone on it together (optional):'), lims, r);
    return det;
  }
  // ── the key's models (T-387): each an OpenRouter model the key may be used for, on the providers its owner ticked, in their
  // order (the next one when a provider is unavailable; none ticked = any provider, OpenRouter's choice). People pick each by
  // its name. It is saved as the hub's entry (id = the model's id, its providers the steps of one chain), so the hub, the API
  // and the Jcode PGM Server route it as before. An older entry made of several models is still shown, and may be removed.
  const stepsOf = (e) => (Array.isArray(e.steps) && e.steps.length ? e.steps : [{ model: e.model, provider: (e.providers || [])[0] || '', label: '' }]);
  /** An entry as the key's models show it: its model, its providers in order (empty = any), whether it is an older chain. */
  function entryView(e) {
    const steps = stepsOf(e).map((s) => ({ model: s.model || e.model, provider: s.provider || '', label: s.label || '' }));
    const older = steps.some((s) => s.model !== steps[0].model);
    const any = !older && steps.every((s) => !s.provider);
    return { id: e.id, name: e.label || e.id, model: steps[0].model, providers: any ? [] : steps.filter((s) => s.provider).map((s) => ({ route: s.provider, label: s.label })), any, older, steps };
  }
  /** What the key saves for a model and the providers ticked for it, in their order (none: any provider). */
  function entryFor(model, name, picks) {
    const ps = (picks || []).filter((x) => x && x.route);
    const label = (name || '').trim() || model;
    return ps.length ? { id: model, label, model, steps: ps.map((x) => ({ model, provider: x.route, label: x.label || '' })) } : { id: model, label, model, providers: [], allowFallbacks: true };
  }
  /** One allowed model in words: "DeepSeek V4 Flash: Relace, then DeepInfra (fp8)" or "…: any provider (OpenRouter chooses)". */
  function entryLine(e) {
    const v = entryView(e);
    if (v.older) return v.name + ' (an older chain): ' + v.steps.map((s) => s.label || s.model + (s.provider ? ' via ' + s.provider : '')).join(', then ');
    return v.name + ': ' + (v.any ? 'any provider (OpenRouter chooses)' : v.providers.map((x) => x.label || x.route).join(', then '));
  }
  function modelsBox(prov, again, edit, adder) {
    const f = mk('div', 'orEntries'); f.append(mk('h4', '', 'Models on this key'));
    f.append(mk('p', 'setHint', edit ? 'People pick each model by its name, in Work, the API and jcode\'s viberoller provider. A request goes to the model\'s providers in the order here: when one is unavailable, the next one is tried, and the session says so. With no provider ticked, OpenRouter chooses any.'
      : 'Only ' + adder + ', who added this key, changes its models and providers.'));
    const entries = (prov.pairs || []).map((e) => ({ ...e }));
    const note = mk('div', 'status');
    const save = async (list, said) => { try { await json('/api/providers/update', { method: 'POST', body: JSON.stringify({ id: prov.id, pairs: list }) }); note.textContent = said || 'Saved.'; refreshProviders(); (again || renderTeamOr)(); } catch (e) { note.textContent = e.message; } };
    if (!entries.length) f.append(mk('div', 'status', edit ? 'No models yet: add some below.' : 'No models yet.'));
    entries.forEach((e, ei) => {
      const v = entryView(e);
      const card = mk('div', 'orEntry');
      if (!edit) { card.append(mk('div', '', entryLine(e))); f.append(card); return; }
      const put = (picks, said) => { const list = entries.slice(); list[ei] = entryFor(v.model, v.name, picks); save(list, said); };
      const head = mk('div', 'row'); const name = mk('input'); name.value = v.name; name.maxLength = 60; name.setAttribute('aria-label', 'The model\'s name in the dropdown');
      const rn = mk('button', 'ghost', 'Rename'); rn.type = 'button'; rn.onclick = () => { const n = name.value.trim(); if (!n) return; const list = entries.slice(); list[ei] = { ...e, label: n }; save(list, 'Renamed.'); };
      head.append(name, rn, confirmButton('Remove model', () => save(entries.filter((_, i) => i !== ei), 'Removed.')), mk('small', 'status', v.model));
      card.append(head);
      if (v.older || v.any) card.append(mk('div', 'status', v.older ? entryLine(e) : 'Any provider (OpenRouter chooses).'));
      else {
        const ol = mk('ol', 'orSteps');
        v.providers.forEach((pv, pi) => {
          const li = mk('li'); li.append(mk('span', '', pv.label || pv.route));
          const acts = mk('span', 'orStepActs');
          const move = (dir, sym, words) => { const b = mk('button', 'ghost', sym); b.type = 'button'; b.title = words; b.setAttribute('aria-label', words + ': ' + (pv.label || pv.route)); b.onclick = () => { const ps = v.providers.slice(); [ps[pi + dir], ps[pi]] = [ps[pi], ps[pi + dir]]; put(ps); }; acts.append(b); };
          if (pi > 0) move(-1, '↑', 'Try it earlier');
          if (pi < v.providers.length - 1) move(1, '↓', 'Try it later');
          if (v.providers.length > 1) { const x = mk('button', 'ghost', 'Remove'); x.type = 'button'; x.onclick = () => put(v.providers.filter((_, i) => i !== pi), 'Provider removed.'); acts.append(x); }
          li.append(acts); ol.append(li);
        });
        card.append(ol);
      }
      if (!v.older) { const ch = mk('button', 'ghost', 'Change providers'); ch.type = 'button'; ch.onclick = () => openPicker(card, (model, nm, picks) => put(picks, 'Providers saved.'), { model: { id: v.model, name: v.name }, picks: v.providers }); card.append(ch); }
      f.append(card);
    });
    if (edit) {
      const add = mk('button', '', 'Add models'); add.type = 'button';
      add.onclick = () => openPicker(f, (model, nm, picks) => {
        const list = entries.slice(), i = list.findIndex((x) => x.id === model), e = entryFor(model, nm, picks);
        if (i >= 0) list[i] = e; else list.push(e);
        save(list, (i >= 0 ? 'Changed "' : 'Added "') + e.label + '".');
      }, { on: entries.map((x) => x.id) });
      f.append(add);
    }
    f.append(note);
    return f;
  }
  // ── the picker: search OpenRouter's models (words and filters, four orders), open one, search and filter its providers, tick
  // the ones to allow in the order they are to be tried (OpenRouter's catalogue, the same data jcode reads). The last search
  // is kept for the next time the picker opens. opts: { model, picks } opens one model's providers with those ticked;
  // { on } marks the models the key already has.
  const SEARCH = { q: '', deals: false, free: false, zdr: false, tools: false, reasoning: false, image: false, maxIn: '', minContext: '', sort: 'newest' };
  /** The model search's query string from its words, filters and order (only what is set). */
  function searchQuery(s) {
    const q = ['q=' + encodeURIComponent((s.q || '').trim())];
    for (const k of ['deals', 'free', 'zdr', 'tools', 'reasoning', 'image']) if (s[k]) q.push(k + '=1');
    if (s.maxIn !== '' && s.maxIn != null && isFinite(Number(s.maxIn))) q.push('maxIn=' + Number(s.maxIn));
    if (s.minContext) q.push('minContext=' + Number(s.minContext));
    if (s.sort && s.sort !== 'newest') q.push('sort=' + s.sort);
    return q.join('&');
  }
  function openPicker(host, done, opts) {
    opts = opts || {};
    const old = host.querySelector(':scope > .orPicker'); if (old) { old.remove(); return; }
    const p = mk('div', 'orPicker');
    const q = mk('input'); q.type = 'search'; q.placeholder = 'Search OpenRouter\'s models: names, makers, words from their description (e.g. deepseek, qwen coder, vision)'; q.setAttribute('aria-label', 'Search models'); q.value = SEARCH.q;
    const close = mk('button', 'ghost', 'Close'); close.type = 'button'; close.onclick = () => p.remove();
    const top = mk('div', 'row'); top.append(q, close); p.append(top);
    // the filters: what a model must be, cost and hold, and the order
    const fl = mk('div', 'row orFilters'); p.append(fl);
    const chk = (k, label) => { const l = mk('label', 'chk'), c = mk('input'); c.type = 'checkbox'; c.checked = !!SEARCH[k]; c.onchange = () => { SEARCH[k] = c.checked; search(); }; l.append(c, ' ' + label); fl.append(l); return c; };
    chk('deals', 'Deals'); chk('free', 'Free'); chk('zdr', 'Zero data retention'); chk('tools', 'Takes tools'); chk('reasoning', 'Reasons'); chk('image', 'Reads images');
    const maxIn = mk('input'); maxIn.type = 'number'; maxIn.min = '0'; maxIn.step = '0.05'; maxIn.placeholder = 'Max in $/M'; maxIn.value = SEARCH.maxIn; maxIn.setAttribute('aria-label', 'At most this input price, dollars per million tokens'); maxIn.className = 'orNum';
    const ctx = mk('select'); ctx.setAttribute('aria-label', 'At least this much context');
    for (const [v, l] of [['', 'Any context'], ['32000', '32k+ context'], ['128000', '128k+ context'], ['200000', '200k+ context'], ['1000000', '1M+ context']]) { const o = mk('option', '', l); o.value = v; ctx.append(o); }
    ctx.value = SEARCH.minContext;
    const ord = mk('select'); ord.setAttribute('aria-label', 'Order the models');
    for (const [v, l] of [['newest', 'Newest first'], ['price', 'Cheapest first'], ['context', 'Most context first'], ['deal', 'Biggest deal first']]) { const o = mk('option', '', l); o.value = v; ord.append(o); }
    ord.value = SEARCH.sort;
    fl.append(maxIn, ctx, ord);
    maxIn.oninput = () => { SEARCH.maxIn = maxIn.value; clearTimeout(timer); timer = setTimeout(search, 300); };
    ctx.onchange = () => { SEARCH.minContext = ctx.value; search(); }; ord.onchange = () => { SEARCH.sort = ord.value; search(); };
    const list = mk('div', 'orModels'), prov = mk('div', 'orProviders'), info = mk('div', 'status', 'Type to search; the newest models come first.');
    p.append(info, list, prov);
    let timer = 0, seq = 0, again = 0;
    const search = async () => {
      const my = ++seq; clearTimeout(again); info.textContent = 'Searching…';
      let d; try { d = await json('/api/openrouter/catalog/models?' + searchQuery(SEARCH)); } catch (e) { info.textContent = e.message; return; }
      if (my !== seq || !p.isConnected) return;
      list.textContent = '';
      const dl = d.deals || {};
      const pending = (SEARCH.deals || SEARCH.sort === 'deal') && !dl.ready; // the hub is still asking every model's providers for their deals
      info.textContent = (pending ? 'Looking up every provider\'s deals (a minute or two the first time); the list fills in by itself. ' : '')
        + (d.total ? d.total + ' model' + (d.total === 1 ? '' : 's') + (d.total > d.models.length ? ' (the first ' + d.models.length + ' shown)' : '') : pending ? '' : 'No model matches that.')
        + (SEARCH.deals && dl.ready ? ' · deals as of ' + (dl.ageMin ? dl.ageMin + ' min ago' : 'just now') : '');
      if (pending || (dl.building && (SEARCH.deals || SEARCH.sort === 'deal'))) again = setTimeout(search, 5000);
      for (const m of d.models.slice(0, 80)) {
        const b = mk('button', 'ghost orModel'); b.type = 'button';
        const nm = mk('b', '', m.name);
        if ((opts.on || []).includes(m.id)) nm.append(' ', mk('span', 'orTier', 'on this key'));
        if (m.bestDeal) nm.append(' ', mk('span', 'orDeal', 'up to ' + Math.round(m.bestDeal * 100) + '% off'));
        if (m.free) nm.append(' ', mk('span', 'orDeal', 'free'));
        b.append(nm, mk('small', '', modelLine(m) + (m.tools ? ' · tools' : '') + (m.reasoning ? ' · reasoning' : '') + ((m.input || []).includes('image') ? ' · reads images' : '')));
        b.onclick = () => showProviders(m);
        list.append(b);
      }
    };
    q.addEventListener('input', () => { SEARCH.q = q.value; clearTimeout(timer); timer = setTimeout(search, 250); });
    const showProviders = async (m, preset) => {
      prov.textContent = 'Loading ' + m.name + '\'s providers…';
      let d; try { d = await json('/api/openrouter/catalog/endpoints?model=' + encodeURIComponent(m.id)); } catch (e) { prov.textContent = e.message; return; }
      prov.textContent = '';
      prov.append(mk('h4', '', m.name + ': tick the providers to allow, in the order to try them'));
      // the ticked providers, in the order ticked (the first is tried first)
      const picks = (preset || []).map((x) => ({ route: x.route, label: x.label || x.route }));
      const filt = mk('select'); filt.setAttribute('aria-label', 'Data policy');
      for (const [v, l] of [['', 'Any data policy'], ['zdr', 'Green only: no retention (ZDR)'], ['nott', 'No training (green or yellow)']]) { const o = mk('option', '', l); o.value = v; filt.append(o); }
      const sort = mk('select'); sort.setAttribute('aria-label', 'Sort by');
      for (const [v, l] of [['tps', 'Fastest first'], ['in', 'Cheapest input first'], ['out', 'Cheapest output first'], ['deal', 'Biggest deal first'], ['policy', 'Best data policy first'], ['cache', 'Cheapest cache read first'], ['uptime', 'Most reliable first'], ['latency', 'Quickest first token']]) { const o = mk('option', '', l); o.value = v; sort.append(o); }
      const tiers = [...new Set((d.endpoints || []).map((e) => e.serviceTier || ''))];
      const tierSel = mk('select'); tierSel.setAttribute('aria-label', 'Service tier'); tierSel.hidden = tiers.length < 2;
      for (const v of ['*'].concat(tiers)) { const o = mk('option', '', v === '*' ? 'Any service tier' : v || 'Standard tier'); o.value = v; tierSel.append(o); }
      const dealsBox = mk('label', 'chk'), dealsOnly = mk('input'); dealsOnly.type = 'checkbox'; dealsBox.append(dealsOnly, ' Deals only'); dealsBox.hidden = !(d.endpoints || []).some((e) => e.discount);
      const pq = mk('input'); pq.type = 'search'; pq.placeholder = 'Search providers (name, country, quantization, tier)'; pq.setAttribute('aria-label', 'Search providers');
      const ctl = mk('div', 'row'); ctl.append(pq, filt, tierSel, dealsBox, sort); prov.append(ctl);
      const t = mk('table', 'orProvTable'); prov.append(t);
      const name = mk('input'); name.placeholder = 'Its name in the dropdown'; name.value = m.name; name.maxLength = 60; name.setAttribute('aria-label', 'The model\'s name in the dropdown');
      const allow = mk('button', '', ''); allow.type = 'button';
      const anyB = mk('button', 'ghost', 'Allow any provider'); anyB.type = 'button'; anyB.title = 'OpenRouter chooses among all of this model\'s providers';
      const finish = (ps) => { done(d.model.id || m.id, name.value.trim() || m.name, ps); p.remove(); };
      allow.onclick = () => { if (picks.length) finish(picks); };
      anyB.onclick = () => finish([]);
      const acts = mk('div', 'row'); acts.append(name, allow, anyB); prov.append(acts);
      const COLS = 12;
      const paint = () => {
        allow.textContent = picks.length ? 'Allow the ' + picks.length + ' ticked provider' + (picks.length === 1 ? '' : 's') : 'Tick providers to allow'; allow.disabled = !picks.length;
        t.textContent = '';
        const hr = mk('tr'); for (const h of ['Allow', 'Provider', 'In $/M', 'Out $/M', 'Cache $/M', 'Speed', 'First token', 'Uptime', 'Context', 'Data policy', '', '']) hr.append(mk('th', '', h)); t.append(hr);
        const words = pq.value.toLowerCase().split(/\s+/).filter(Boolean);
        const hay = (e) => [e.provider, e.route, e.quantization, e.serviceTier, ...((e.providerInfo || {}).datacenters || []), (e.providerInfo || {}).headquarters].filter(Boolean).join(' ').toLowerCase();
        let eps = (d.endpoints || []).filter((e) => (!filt.value || (filt.value === 'zdr' ? e.data === 'zdr' : e.data !== 'train')) && (tierSel.value === '*' || (e.serviceTier || '') === tierSel.value)
          && (!dealsOnly.checked || e.discount) && words.every((w) => hay(e).includes(w)));
        const num = (x, d0) => (x == null ? d0 : x);
        const by = { tps: (a, b) => num(b.tpsP50, -1) - num(a.tpsP50, -1), in: (a, b) => num(a.inPerM, 1e9) - num(b.inPerM, 1e9), out: (a, b) => num(a.outPerM, 1e9) - num(b.outPerM, 1e9), deal: (a, b) => num(b.discount, 0) - num(a.discount, 0), policy: (a, b) => RANK[endpointFacts(a).level] - RANK[endpointFacts(b).level],
          cache: (a, b) => num(a.cacheReadPerM, 1e9) - num(b.cacheReadPerM, 1e9), uptime: (a, b) => num(b.uptime30m, -1) - num(a.uptime30m, -1), latency: (a, b) => num(a.latencyP50Ms, 1e9) - num(b.latencyP50Ms, 1e9) };
        eps = eps.slice().sort(by[sort.value]);
        if (!eps.length) { const tr = mk('tr'), td = mk('td', 'status', 'No provider matches.'); td.colSpan = COLS; tr.append(td); t.append(tr); }
        for (const e of eps) {
          const f = endpointFacts(e);
          const tr = mk('tr');
          const at = picks.findIndex((x) => x.route === e.route);
          const tick = mk('td', 'orTick'); const cb = mk('input'); cb.type = 'checkbox'; cb.checked = at >= 0; cb.setAttribute('aria-label', 'Allow ' + e.provider + (e.quantization ? ' ' + e.quantization : '') + (e.serviceTier ? ' ' + e.serviceTier : ''));
          cb.onchange = () => { const i = picks.findIndex((x) => x.route === e.route); if (cb.checked && i < 0) picks.push({ route: e.route, label: e.provider + (e.quantization ? ' (' + e.quantization + ')' : '') + (e.serviceTier ? ' ' + e.serviceTier : '') }); if (!cb.checked && i >= 0) picks.splice(i, 1); paint(); };
          tick.append(cb); if (at >= 0) tick.append(mk('small', 'orOrder', String(at + 1)));
          const pn = mk('td'); pn.append(mk('b', '', e.provider)); if (e.quantization) pn.append(mk('small', '', ' ' + e.quantization)); if (f.tier) pn.append(' ', mk('span', 'orTier', f.tier)); pn.title = e.route + (e.detail ? ' · ' + e.detail : '');
          const price = (now, before) => { const td = mk('td'); if (before) td.append(mk('s', 'orBefore', before), ' '); td.append(perM(now)); if (before) td.append(' ', mk('span', 'orDeal', f.deal)); return td; };
          const up = mk('td', '', e.uptime30m != null ? Math.round(e.uptime30m) + '%' : '–'); up.title = f.uptime;
          const dp = mk('td'); dp.append(mk('span', 'orMark ' + f.level, DATA_WORDS[e.data] || e.dataLabel)); dp.title = [f.policy, f.where].filter(Boolean).join('\n');
          const more = mk('td'); const mb = mk('button', 'ghost', 'More'); mb.type = 'button'; mb.setAttribute('aria-expanded', 'false'); more.append(mb);
          tr.append(tick, pn, price(e.inPerM, f.inBefore), price(e.outPerM, f.outBefore), mk('td', '', f.cache), mk('td', '', e.tpsP50 != null ? Math.round(e.tpsP50) + ' t/s' : '–'), mk('td', '', e.latencyP50Ms != null ? (e.latencyP50Ms / 1000).toFixed(2) + ' s' : '–'),
            up, mk('td', '', e.context ? kTok(e.context) : '–'), dp, more, mk('td'));
          // the rest, a row under it: tiered and other prices, uptime over time, limits, parameters, who runs it and their policies
          const xr = mk('tr', 'orMoreRow'); xr.hidden = true; const xd = mk('td'); xd.colSpan = COLS; xr.append(xd);
          const line = (label, text) => { if (text) { const r = mk('div'); r.append(mk('b', '', label + ': '), text); xd.append(r); } };
          line('Data policy', f.policy);
          line('Deal', f.deal ? f.deal + ' (was in ' + f.inBefore + '/M, out ' + f.outBefore + '/M)' : '');
          line('Tiered prices', f.tiers.join(' · ')); line('Other prices', f.more.join(' · ')); line('Uptime', f.uptime.replace(/^Uptime: /, '')); line('Limits', f.limits);
          line('Service tier', f.tier); line('Runs on', f.where); line('Parameters', f.params);
          if (f.links.length) { const r = mk('div'); r.append(mk('b', '', 'Provider: ')); f.links.forEach(([l, u], i) => { const a = mk('a', '', l); a.href = u; a.target = '_blank'; a.rel = 'noopener noreferrer'; r.append(i ? ' · ' : '', a); }); xd.append(r); }
          mb.onclick = () => { xr.hidden = !xr.hidden; mb.setAttribute('aria-expanded', String(!xr.hidden)); mb.textContent = xr.hidden ? 'More' : 'Less'; };
          t.append(tr, xr);
        }
      };
      for (const el of [filt, sort, tierSel, dealsOnly]) el.onchange = paint; pq.oninput = paint; paint();
    };
    host.append(p);
    if (opts.model) { top.hidden = true; fl.hidden = true; info.hidden = true; showProviders(opts.model, opts.picks); } else { q.focus(); search(); }
  }
  // everyone's month on the team's keys, and each person's cap (Manage AI)
  async function everyonesMonth(box) {
    let all; try { all = await json('/api/openrouter/budget/all'); } catch { return; }
    const det = mk('details'); det.append(mk('summary', '', 'Each person\'s month on the team\'s keys · default cap ' + usd(all.defaultCapUsd)));
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
  return { open, myKeys, endpointFacts, modelLine, entryView, entryFor, entryLine, searchQuery };
})();
