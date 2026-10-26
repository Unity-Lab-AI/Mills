// The Workflows screen (T-205): the list of Hyper Workflows (search, categories, new, open, duplicate, delete), then the
// editor: core.js + view.js + the workflow plugin (kinds-workflow.js). Open, edit, save the draft (also by itself after a
// quiet moment), validate, publish, test-run the draft with the live overlay, the AI side chat, the workflow's settings
// and its triggers. Loaded after the page script; uses its helpers at call time ($, api, base, token, me, showScreen).
// Plain styling from the page's CSS variables; Mills does the look later. All text goes in through textContent.
const WORKFLOWS = (() => {
  const W = { cl: null, items: [], q: '', cat: '*', edit: null, kindsFrom: null };
  const h = (tag, attrs, kids) => { const e = document.createElement(tag); for (const k of Object.keys(attrs || {})) { if (k === 'text') e.textContent = attrs[k]; else if (k === 'class') e.className = attrs[k]; else if (k === 'style') e.setAttribute('style', attrs[k]); else e.setAttribute(k, attrs[k]); } for (const c of kids || []) e.append(c); return e; };
  const note = (text, bad) => { const m = document.getElementById('wfMsg'); if (!m) return; m.textContent = text || ''; m.className = 'status' + (bad ? ' bad' : ''); clearTimeout(note.t); if (text) note.t = setTimeout(() => { m.textContent = ''; }, 8000); };
  const body = () => document.getElementById('wfBody');
  const muted = 'color:var(--muted,#8b93a7)';
  const btn = (text, fn, title, cls) => { const b = h('button', { type: 'button', text, class: cls || 'ghost' }); if (title) b.title = title; b.onclick = fn; return b; };

  // the hub, as the page reaches it: api() (it signs out on 401, waits out a restart) and the log's live stream
  const request = async (method, path, bodyObj) => {
    const res = await api(path, { method, body: bodyObj === undefined || method === 'GET' ? undefined : JSON.stringify(bodyObj) });
    const text = await res.text(); let data = null; if (!/\/log\?/.test(path)) { try { data = JSON.parse(text); } catch { data = null; } }
    return { status: res.status, data, text };
  };
  const stream = async (path, onLine, stopped) => {
    const res = await fetch(base + path, { headers: token ? { Authorization: 'Bearer ' + token } : {} });
    if (!res.ok || !res.body) throw new Error('no stream');
    const reader = res.body.getReader(), dec = new TextDecoder(); let buf = '';
    for (;;) {
      if (stopped()) { try { await reader.cancel(); } catch { /* closed */ } return; }
      const { value, done } = await reader.read(); if (done) return;
      buf += dec.decode(value, { stream: true }); let i;
      while ((i = buf.indexOf('\n')) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1); if (l.trim()) onLine(l); }
    }
  };
  const client = () => W.cl || (W.cl = VrKindsWorkflow.client(request, { stream }));

  // ---------------------------------------------------------------- the list
  async function showList() {
    closeEditor(); const root = body(); root.textContent = '';
    const bar = h('div', { style: 'display:flex;gap:8px;flex-wrap:wrap;align-items:center;padding:10px' });
    const search = h('input', { type: 'search', placeholder: 'Search workflows', 'aria-label': 'Search workflows', style: 'flex:1;min-width:180px' }); search.value = W.q;
    const nameIn = h('input', { type: 'text', placeholder: 'new-workflow-name', 'aria-label': 'Name of the new workflow', maxlength: '60', spellcheck: 'false' });
    const scopeSel = h('select', { 'aria-label': 'Scope of the new workflow' });
    bar.append(search, nameIn, scopeSel, btn('New workflow', async () => {
      const name = nameIn.value.trim();
      try { const r = await client().create(name, scopeSel.value || undefined); nameIn.value = ''; openEditor(r.id); } catch (e) { note(e.message, true); }
    }, 'Make a workflow with a start and an end', ''));
    const cats = h('div', { style: 'display:flex;gap:6px;flex-wrap:wrap;padding:0 10px 8px' });
    const list = h('div', { style: 'padding:0 10px 10px;overflow:auto' });
    root.append(bar, cats, list);
    const draw = () => {
      cats.textContent = ''; list.textContent = '';
      for (const c of VrKindsWorkflow.categories(W.items)) { const b = btn(c.label + ' (' + c.count + ')', () => { W.cat = c.id; draw(); }, '', 'ghost' + (W.cat === c.id ? ' on' : '')); if (W.cat === c.id) b.style.borderColor = 'var(--accent,#7c9cff)'; cats.append(b); }
      const rows = VrKindsWorkflow.filterList(W.items, { q: W.q, category: W.cat });
      if (!rows.length) list.append(h('p', { text: W.items.length ? 'Nothing matches.' : 'No workflows yet. Give one a name above and press New workflow.', style: muted }));
      for (const w of rows) list.append(row(w));
    };
    const row = (w) => {
      const r = h('div', { style: 'display:flex;gap:10px;align-items:center;padding:8px;border:1px solid var(--line,#2a2f3a);border-radius:8px;margin-bottom:6px;flex-wrap:wrap' });
      const info = h('div', { style: 'flex:1;min-width:200px;cursor:pointer' }); info.onclick = () => openEditor(w.id);
      info.append(h('b', { text: w.shown || w.name }), h('span', { text: '  ' + String(w.scope || '').replace(/^me-.*/, 'mine') + (w.version ? ' · v' + w.version : ' · not published') + (w.draft ? ' · you have a draft' : ''), style: muted + ';font-size:12px' }));
      if (w.title || w.description) info.append(h('div', { text: [w.title, w.description].filter(Boolean).join(' · '), style: muted + ';font-size:12px' }));
      if ((w.labels || []).length) info.append(h('div', { text: w.labels.join(' · '), style: 'font-size:11px;color:var(--accent,#7c9cff)' }));
      if (w.unrunnable && w.version) info.append(h('div', { text: 'cannot run here: ' + w.unrunnable, style: 'font-size:11px;color:var(--warn,#e6a14f)' }));
      r.append(info, btn('Open', () => openEditor(w.id)));
      if (w.rights && w.rights.edit) r.append(btn('Duplicate', async () => { try { const n = VrKindsWorkflow.copyName(w.name, W.items.map((x) => x.name)); const d = await client().duplicate(w.id, n); note('Copied as ' + n + '.'); await reload(); if (d.id) openEditor(d.id); } catch (e) { note(e.message, true); } }, 'Copy it as a new workflow'));
      if (w.rights && w.rights.admin) r.append(btn('Delete', async () => { if (!confirm('Delete the workflow "' + w.name + '"? Its published versions are kept for the audit.')) return; try { await client().remove(w.id); note('Deleted ' + w.name + '.'); await reload(); } catch (e) { note(e.message, true); } }, 'Delete the workflow'));
      return r;
    };
    const reload = async () => {
      try { W.items = await client().list(); } catch (e) { list.textContent = e.status === 404 ? 'This hub has no Hyper Workflows (it is older, or the JavaScript hub serves alone).' : e.message; return; }
      const scopes = [...new Set(['me-' + ((me && me.username) || ''), ...W.items.map((x) => x.scope)])].filter((s) => s && s !== 'me-');
      scopeSel.textContent = ''; for (const s of scopes) scopeSel.append(h('option', { value: s, text: s.replace(/^me-.*/, 'mine (personal)') }));
      draw();
    };
    search.oninput = () => { W.q = search.value; draw(); };
    await reload();
  }

  // ---------------------------------------------------------------- the editor
  function closeEditor() {
    const e = W.edit; if (!e) return;
    if (e.watch) e.watch.stop();
    if (e.stopAuto) e.stopAuto();
    if (e.view) e.view.destroy();
    W.edit = null;
  }

  async function openEditor(id) {
    const root = body(); root.textContent = 'Opening…';
    const core = VrEditorCore, WF = VrKindsWorkflow, cl = client();
    let opened, manifests = null;
    try { manifests = await cl.kinds(); } catch { manifests = null; } // the hub's kinds (custom ones too), else the static copy
    try { opened = await cl.open(id); } catch (e) { root.textContent = ''; root.append(h('p', { text: e.message, style: 'padding:10px;color:var(--bad,#ff6b6b)' }), btn('Back to the list', showList)); return; }
    const reg = core.createRegistry(); WF.install(reg, manifests);
    let doc;
    try { doc = WF.fromWorkflow(opened.json); } catch (e) { root.textContent = ''; root.append(h('p', { text: 'This workflow cannot be opened in the editor: ' + e.message, style: 'padding:10px;color:var(--bad,#ff6b6b)' }), btn('Back to the list', showList)); return; }
    const ed = core.createEditor({ registry: reg, doc, idPrefix: 'n', linkPrefix: 'e', rootLabel: opened.meta.name || 'Workflow' });
    const E = { id, ed, reg, etag: opened.etag, rights: opened.rights, meta: opened.meta, version: opened.version, draft: opened.draft, dirty: false, saving: null, overlay: WF.overlay(), problems: [], runId: null, watch: null, view: null, stopAuto: null };
    W.edit = E; root.textContent = '';
    const canEdit = !!(opened.rights && opened.rights.edit);

    const top = h('div', { style: 'display:flex;gap:6px;align-items:center;flex-wrap:wrap;padding:6px 10px;border-bottom:1px solid var(--line,#2a2f3a)' });
    const stateEl = h('span', { style: muted + ';font-size:12px' });
    const promptIn = h('input', { type: 'text', placeholder: 'A prompt for a test run', 'aria-label': 'Prompt for a test run', style: 'flex:1;min-width:140px;max-width:320px' });
    const host = h('div', { style: 'flex:1;min-height:0;display:flex' }); const hostInner = h('div', { style: 'flex:1;min-width:0;min-height:0' }); host.append(hostInner);
    root.append(top, host);
    const view = VrEditorView.mount(hostInner, { editor: ed }); E.view = view;
    const say = (t, bad) => note(t, bad);
    const paintState = () => { stateEl.textContent = (E.meta.name || '') + ' · ' + (E.draft ? 'draft' : 'published v' + E.version + (canEdit ? ' (your edits become a draft)' : ' (read only)')) + (E.dirty ? ' · unsaved' : (E.savedAt ? ' · saved' : '')) + (canEdit ? '' : ' · you may run it, not change it'); };

    // ---- problems: from save / validate: marks on the nodes and a list
    const linkNode = (eid) => { const l = ed.doc.links[eid]; return l ? l.to.item : null; };
    const showProblems = (problems) => {
      E.problems = problems || [];
      view.setMarks(WF.problemMarks(E.problems.map((p) => Object.assign({}, p, { node: p.node || (p.edge ? linkNode(p.edge) : null) }))));
      drawProblems();
    };
    const probBox = h('div', { style: 'padding:8px' });
    view.addPanel('problems', 'Problems', probBox);
    function drawProblems() {
      probBox.textContent = '';
      if (!E.problems.length) { probBox.append(h('p', { text: 'No problems found. Press Validate to check again.', style: muted })); return; }
      for (const p of E.problems) {
        const row = h('div', { style: 'padding:4px 0;border-bottom:1px solid var(--line,#2a2f3a);cursor:' + (p.node ? 'pointer' : 'default') });
        row.append(h('b', { text: (p.level === 'error' ? 'Error' : 'Warning') + (p.node ? ' · ' + p.node : '') + (p.port ? '.' + p.port : '') + ' ', style: 'color:var(--' + (p.level === 'error' ? 'bad,#ff6b6b' : 'warn,#e6a14f') + ')' }), h('span', { text: p.message || p.code }));
        const n = p.node || (p.edge ? linkNode(p.edge) : null); if (n && ed.item(n)) row.onclick = () => { ed.goto(0); ed.select([n]); view.showPanel('props'); };
        probBox.append(row);
      }
    }
    drawProblems();

    // ---- save, validate, publish
    const save = async (quiet) => {
      if (!canEdit) return false; if (E.saving) return E.saving;
      E.saving = (async () => {
        try {
          const r = await cl.save(E.id, ed.toJSON(), { etag: E.etag }); E.etag = r.etag; E.dirty = false; E.draft = true; E.savedAt = Date.now(); showProblems(r.problems);
          if (r.warnings && r.warnings.length) say(r.warnings[0].message, true); else if (!quiet) say('Draft saved' + (r.problems.some((p) => p.level === 'error') ? ' (it has errors; fix them before publishing)' : '.'));
          return true;
        } catch (e) { say(e.status === 409 ? 'Saving stopped: ' + e.message + ' Reopen the workflow to get the newest draft (your edits here are not lost until you leave).' : e.message, true); if (e.status === 409 && E.stopAuto) E.stopAuto(); return false; }
        finally { E.saving = null; paintState(); }
      })();
      return E.saving;
    };
    const validate = async () => { try { const p = await cl.validate(ed.toJSON(), E.id); showProblems(p); view.showPanel('problems'); say(p.some((x) => x.level === 'error') ? p.filter((x) => x.level === 'error').length + ' error(s)' : p.length ? 'Valid, with ' + p.length + ' warning(s).' : 'Valid.'); } catch (e) { say(e.message, true); } };
    const publish = async () => {
      if (!(await save(true))) return; const msg = prompt('Publish this workflow: what changed?'); if (!msg || !msg.trim()) return;
      try { const r = await cl.publish(E.id, msg.trim()); E.version = r.version; say('Published as version ' + r.version + '.'); paintState(); }
      catch (e) { if (e.problems && e.problems.length) { showProblems(e.problems.map((p) => Object.assign({ level: 'error' }, p))); view.showPanel('problems'); } say(e.message, true); }
    };

    // ---- the test run, with the live overlay
    const runBox = h('div', { style: 'padding:8px' }); view.addPanel('run', 'Run', runBox);
    const runLine = (text, bad) => { runBox.append(h('div', { text, style: 'padding:2px 0;font-size:12px' + (bad ? ';color:var(--bad,#ff6b6b)' : '') })); runBox.scrollTop = runBox.scrollHeight; };
    const testRun = async () => {
      if (E.watch) { E.watch.stop(); E.watch = null; }
      if (!(await save(true)) && canEdit) return;
      runBox.textContent = ''; view.showPanel('run'); E.overlay.reset(); view.setRunStates({});
      let r; try { r = await cl.run(E.id, { prompt: promptIn.value, draft: canEdit }); } catch (e) { runLine(e.message + (e.problems && e.problems.length ? ': ' + e.problems.map((p) => p.message).join('; ') : ''), true); say('The test run did not start.', true); return; }
      E.runId = r.run; runLine('Test run ' + r.run + ' started on the ' + (r.runner || 'hub') + '.');
      const onEv = (ev) => {
        E.overlay.apply(ev); view.setRunStates(E.overlay.marks());
        if (ev.type === 'node_end' && ev.status === 'ok' && !['start', 'end'].includes(ev.kind)) runLine('· ' + ev.node + ' (' + ev.kind + ') done' + (ev.durationMs ? ' in ' + Math.round(ev.durationMs / 100) / 10 + ' s' : ''));
        else if (ev.type === 'node_end' && ev.status === 'error' && !ev.retry) runLine('· ' + ev.node + ' (' + ev.kind + ') failed: ' + ((ev.error && ev.error.message) || ''), true);
        else if (ev.type === 'wait') {
          const d = h('div', { style: 'padding:4px 0' }); d.append(h('span', { text: 'Waits for you at ' + ev.node + ' (' + ev.what + '). ' }));
          for (const [label, ok] of [['Approve', true], ['Reject', false]]) d.append(btn(label, async () => {
            d.querySelectorAll('button').forEach((x) => { x.disabled = true; });
            try { await cl.answer(r.run, ev.key || ev.node, ev.what === 'resume' ? { rerun: ok } : { approved: ok }); E.watch = cl.watch(r.run, onEv, { from: (ev.seq || 0) + 1 }); } catch (e) { runLine(e.message, true); }
          }, '', ''));
          runBox.append(d);
        } else if (ev.type === 'run_end') {
          runLine('Run ended: ' + ev.status + (ev.outcome ? ' (' + ev.outcome + ')' : '') + (ev.error && ev.error.message ? ': ' + ev.error.message : ''), ev.status !== 'ok');
          if (ev.result !== undefined && ev.result !== null) runBox.append(h('pre', { text: typeof ev.result === 'string' ? ev.result : JSON.stringify(ev.result, null, 2), style: 'white-space:pre-wrap;font-size:12px;background:var(--bg,#0f1115);padding:6px;border-radius:6px' }));
        }
      };
      E.watch = cl.watch(r.run, onEv);
    };
    const stopRun = async () => { if (!E.runId) return; try { await cl.cancel(E.runId); say('Cancel sent.'); } catch (e) { say(e.message, true); } };

    // ---- top bar
    top.append(btn('← Workflows', async () => { if (E.dirty && canEdit) await save(true); showList(); }, 'Back to the list'), stateEl);
    if (canEdit) { top.append(btn('Save', () => save(false), 'Save the draft'), btn('Validate', validate, 'Check the draft'), btn('Publish', publish, 'Make the draft the published version')); }
    top.append(promptIn, btn('Test run', testRun, 'Run the draft with this prompt and watch the nodes'), btn('Stop', stopRun, 'Cancel the test run'));
    paintState();
    ed.on('change', (ev) => { if (ev && ev.kind === 'load') return; E.dirty = true; paintState(); });
    if (canEdit) E.stopAuto = ed.autosave(() => { if (E.dirty) save(true); }, { delay: 3000 });

    // ---- the workflow's own settings (name, labels ... are the tool's meta, not items)
    const sBox = h('div', { class: 've-props', style: 'padding:8px' }); view.addPanel('wf', 'Workflow', sBox);
    const field = (label, key, kind, opts) => {
      const cur = (ed.meta('wf') || {})[key]; let inp;
      if (kind === 'bool') { inp = h('input', { type: 'checkbox' }); inp.checked = cur === true; }
      else if (kind === 'enum') { inp = h('select'); for (const o of ['', ...opts]) inp.append(h('option', { value: o, text: o || '(default)' })); inp.value = cur || ''; }
      else if (kind === 'json') { inp = h('textarea', { rows: '4' }); inp.value = cur === undefined ? '' : JSON.stringify(cur, null, 1); }
      else if (kind === 'text') { inp = h('textarea', { rows: '3' }); inp.value = cur || ''; }
      else { inp = h('input', { type: 'text' }); inp.value = kind === 'labels' ? (cur || []).join(', ') : cur || ''; }
      const err = h('div', { class: 'err' });
      inp.disabled = !canEdit;
      inp.onchange = () => {
        let v;
        if (kind === 'bool') v = inp.checked; else if (kind === 'labels') v = inp.value.split(',').map((x) => x.trim()).filter(Boolean);
        else if (kind === 'json') { if (!inp.value.trim()) v = undefined; else { try { v = JSON.parse(inp.value); } catch { err.textContent = 'That is not valid JSON'; return; } } }
        else v = inp.value === '' ? undefined : inp.value;
        if (key === 'name' && !WF.nameOk(v)) { err.textContent = 'Letters, digits and - _ . only'; return; }
        err.textContent = ''; const m = Object.assign({}, ed.meta('wf')); if (v === undefined || (Array.isArray(v) && !v.length && kind === 'labels')) delete m[key]; else m[key] = v; ed.setMeta('wf', m); E.meta = Object.assign({}, E.meta, { name: m.name || E.meta.name }); paintState();
      };
      sBox.append(h('label', { text: label }), inp, err);
    };
    field('Name (what /hyperwork types)', 'name'); field('Title', 'title'); field('Description', 'description', 'text'); field('Categories (comma separated)', 'labels', 'labels');
    field('Runs on', 'runOn', 'enum', ['any', 'hub', 'local', 'gateway']); field('Needs a project', 'needsProject', 'bool');
    field('Inputs (JSON list of {name, type, required, default})', 'inputs', 'json'); field('Outputs (JSON list)', 'outputs', 'json'); field('Defaults (model, laws, limits, retry, onError: JSON)', 'defaults', 'json');
    sBox.append(h('p', { text: 'Scope ' + (E.meta.scope || '') + ' · owner ' + (E.meta.owner || '') + (E.version ? ' · published v' + E.version : ''), style: muted + ';font-size:12px' }));

    // ---- triggers (T-186)
    const tBox = h('div', { style: 'padding:8px' }); view.addPanel('triggers', 'Triggers', tBox);
    const drawTriggers = async () => {
      tBox.textContent = 'Loading…'; let list = [];
      try { list = await cl.triggers(E.id); } catch (e) { tBox.textContent = e.status === 404 ? 'This hub has no triggers.' : e.message; return; }
      tBox.textContent = '';
      if (!list.length) tBox.append(h('p', { text: 'Nothing starts this workflow by itself yet.', style: muted }));
      for (const t of list) {
        const r = h('div', { style: 'padding:6px 0;border-bottom:1px solid var(--line,#2a2f3a)' });
        r.append(h('b', { text: t.title || t.id }), h('div', { text: WF.triggerSummary(t), style: 'font-size:12px' }));
        const last = (t.runs || [])[0]; if (last) r.append(h('div', { text: 'last: ' + last.status + ' ' + new Date(last.at).toLocaleString(), style: muted + ';font-size:11px' }));
        if (canEdit || E.rights.run) {
          r.append(btn(t.paused ? 'Resume' : 'Pause', async () => { try { await (t.paused ? cl.triggerResume(t.id) : cl.triggerPause(t.id)); drawTriggers(); } catch (e) { say(e.message, true); } }),
            btn('Remove', async () => { if (!confirm('Remove this trigger?')) return; try { await cl.triggerDelete(t.id); drawTriggers(); } catch (e) { say(e.message, true); } }));
        }
        tBox.append(r);
      }
      // the form: a timer, a webhook or an event
      const f = h('div', { style: 'margin-top:10px' }); f.append(h('b', { text: 'Add a trigger' }));
      const kindSel = h('select'); for (const k of ['timer', 'webhook', 'event']) kindSel.append(h('option', { value: k, text: k }));
      const mk = (ph) => h('input', { type: 'text', placeholder: ph, style: 'width:100%;margin-top:4px' });
      const title = mk('Title (optional)'), prompt_ = mk('The prompt the run gets'), every = mk('Every N'), unit = h('select'), recur = h('select'), at = mk('At HH:MM (optional)'), events = mk('Events, e.g. pull_request push (empty = all)'), evName = mk('Event name');
      for (const u of ['minutes', 'hours', 'days']) unit.append(h('option', { value: u, text: u })); unit.value = 'hours';
      recur.append(h('option', { value: '', text: '(use "every")' })); for (const u of ['nightly', 'daily', 'weekly', 'biweekly', 'monthly', 'quarterly', 'half_year', 'yearly']) recur.append(h('option', { value: u, text: u }));
      const timerBox = h('div', {}, [every, unit, recur, at]), hookBox = h('div', {}, [events]), evBox = h('div', {}, [evName]);
      const swap = () => { timerBox.hidden = kindSel.value !== 'timer'; hookBox.hidden = kindSel.value !== 'webhook'; evBox.hidden = kindSel.value !== 'event'; }; kindSel.onchange = swap; swap();
      const out = h('div', { style: 'font-size:12px;margin-top:6px;word-break:break-all' });
      f.append(kindSel, title, timerBox, hookBox, evBox, prompt_, btn('Add trigger', async () => {
        try {
          const r = await cl.triggerSave(WF.triggerBody(E.id, { kind: kindSel.value, title: title.value, prompt: prompt_.value, n: every.value, unit: unit.value, recur: recur.value, at: at.value, events: events.value, name: evName.value }));
          if (r.secret) { out.textContent = 'Webhook ' + r.path + ' · secret (shown once): ' + r.secret; } else { out.textContent = ''; await drawTriggers(); }
        } catch (e) { say(e.message, true); }
      }), out);
      if (E.rights.run) tBox.append(f); else tBox.append(h('p', { text: 'You may not run this workflow, so you may not trigger it.', style: muted }));
    };
    const trigTab = [...view.element.querySelectorAll('[data-tab="triggers"]')][0]; if (trigTab) trigTab.addEventListener('click', drawTriggers);

    // ---- the AI side chat (T-206): a conversation; a patch waits as a preview you accept or reject
    const chat = WF.chat(cl, ed); E.chat = chat;
    const chatLog = h('div', { style: 'padding:8px;overflow:auto;flex:1' }), chatIn = h('textarea', { rows: '3', placeholder: 'Ask about this workflow, or ask for a change…', 'aria-label': 'Message to the AI', style: 'width:100%' });
    const send = () => { const t = chatIn.value; chatIn.value = ''; chat.send(t); };
    chatIn.onkeydown = (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); } };
    view.aiSlot.textContent = ''; view.aiSlot.style.cssText = 'display:flex;flex-direction:column;height:100%'; view.aiSlot.append(chatLog, h('div', { style: 'padding:8px' }, [chatIn, btn('Send', send, 'Enter sends, Shift+Enter makes a new line', '')]));
    const previewOf = (m) => { const d = m.changes, pv = m.preview; return { added: d.items.added.map((i) => pv.items[i]), changed: d.items.changed.filter((i) => ed.item(i)), removed: d.items.removed, links: d.links.added.map((i) => pv.links[i]) }; };
    const drawChat = () => {
      chatLog.textContent = '';
      if (!chat.messages.length) chatLog.append(h('p', { text: 'Ask the AI about this workflow. It reads the document and may suggest changes as a patch: you see it on the canvas first and accept or reject it. It never saves, publishes or runs anything.', style: muted + ';font-size:12px' }));
      chat.messages.forEach((m, i) => {
        const b = h('div', { style: 'margin:6px 0;padding:6px;border-radius:6px;background:' + (m.who === 'me' ? 'var(--bg,#0f1115)' : 'transparent') + ';border:1px solid var(--line,#2a2f3a);white-space:pre-wrap' });
        b.append(h('b', { text: m.who === 'me' ? 'You' : 'AI', style: 'font-size:11px;' + muted }), h('div', { text: m.error ? 'Could not ask: ' + m.error : m.text }));
        if (m.rejected) b.append(h('div', { text: m.rejected, style: 'color:var(--bad,#ff6b6b);font-size:12px' }));
        if ((m.problems || []).length) b.append(h('div', { text: 'Problems with that change: ' + m.problems.map((p) => p.message).join('; '), style: 'color:var(--warn,#e6a14f);font-size:12px' }));
        if (m.proposal) {
          const d = m.changes, live = chat.pending === m.proposal;
          b.append(h('div', { text: 'Suggested change: ' + [d.items.added.length && d.items.added.length + ' new item(s)', d.items.changed.length && d.items.changed.length + ' changed', d.items.removed.length && d.items.removed.length + ' removed', d.links.added.length && d.links.added.length + ' new wire(s)', d.links.removed.length && d.links.removed.length + ' wire(s) removed'].filter(Boolean).join(', '), style: 'font-size:12px' }));
          if (live) b.append(btn('Accept', () => { const r = chat.accept(); view.setPreview(null); if (!r.ok) say(r.error, true); else say('Change added to your draft (Undo takes it back).'); }, 'Add the change to the draft as one undo step', ''), btn('Reject', () => { chat.reject(); view.setPreview(null); }, 'Drop it without a trace'));
          else b.append(h('div', { text: i === chat.messages.length - 1 ? '(settled)' : '(settled)', style: muted + ';font-size:11px' }));
        }
        chatLog.append(b);
      });
      if (chat.busy) chatLog.append(h('p', { text: 'Thinking…', style: muted }));
      chatLog.scrollTop = chatLog.scrollHeight;
      const live = chat.messages.find((m) => m.proposal && chat.pending === m.proposal); view.setPreview(live ? previewOf(live) : null);
    };
    chat.on(drawChat); drawChat();
    const aiTab = [...view.element.querySelectorAll('[data-tab="ai"]')][0]; if (aiTab) aiTab.textContent = 'AI chat';

    // the section "Workflow" first when the workflow is brand new (a start and an end only)
    view.fit();
    if (!canEdit) say('Read only: you may run and look at this workflow, not change it.');
    return E;
  }

  return {
    show(id) { const b = body(); if (!b) return; if (id) return openEditor(id); if (!W.edit) showList(); },
    list: showList, open: openEditor, stop() { closeEditor(); },
    // the typed name of a workflow (the /workflows <name> command): its id from the list, then the editor
    async openByName(name) { try { const items = await client().list(name); const w = items.find((x) => x.name === name || x.shown === name) || items[0]; if (w) return openEditor(w.id); } catch { /* the list shows why */ } return showList(); },
  };
})();
