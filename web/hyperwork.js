// hyperwork.js 1.0.0: "/hyperwork <name> <prompt>" (or "/hw") typed into a chat composer: the prompt is sent through a
// Hyper Workflow on the VibeRoller hub and the run is followed in the chat (T-207; VibeRoller's team note
// plans/hyper-workflows.md § 10.2). Product-neutral: no page globals, only what the page passes in. The hub's routes
// (VibeRoller serves them itself; a product proxies them to PGM Gateway's /internal/gateway/hyperwork/<rest>, signed
// as the person; pgm-product-internal does):
//   GET  /api/hyperwork/list?q=                  {items: [{id, name, shown, title, description, scope, unrunnable, …}], rev}
//   POST /api/hyperwork/command {line, project}  202 {run} | 404 {error, nearest: [...]} | 422 {problems: [{message}]} | 400 {error}
//   GET  /api/hyperwork/log?id=&from=&follow=1   NDJSON events (node_end, wait, run_end, …) up to the run_end
//   POST /api/hyperwork/answer {run, node, answers: {approved} | {rerun}}
//   GET /api/hyperwork/run?id=   GET /api/hyperwork/runs   POST /api/hyperwork/cancel {id}   (not used by this module)
//
//   HyperworkUI.create(hooks) → { typeahead, isCommand, run, pick, items, hide, open, key }
//     hooks = { input, menu, api(path, init) → Response, streamUrl(path) → absolute URL, headers() → auth headers,
//               project() → '' | name, me(text) → the person's own line, say(text, bad) → a system line (returns its
//               element), answer(text) → the final answer (markdown if the page has it) }
//     typeahead()      call from the input's input event: true when the line is "/hyperwork <name being typed>" (the
//                      menu was shown / updated from GET list?q=, debounced 150 ms; greyed rows cannot run); false otherwise
//     isCommand(line)  is this a /hyperwork or /hw line (then run() it instead of sending a message)
//     run(line)        POST command, then follow the run: steps as say() lines, a wait as Approve / Reject buttons,
//                      the result as answer(); resolves when the follow ends (or the command was refused)
//     key(e)           call from the input's keydown: arrows / Tab / Enter / Escape while the menu is open; true = handled
//     pick(item)       puts "/hyperwork <name> " in the input;  open() → is the menu shown;  hide();  items = the last list
//   HyperworkUI.nameQuery(text) → the name being typed after "/hyperwork " or "/hw " (null otherwise)
//   HyperworkUI.renderEvent(ev) → { kind: 'step' | 'fail' | 'wait' | 'done' | 'ended' | null, text, result? }
const HyperworkUI = (() => {
  const NAME_RE = /^\/(?:hyperwork|hw) (\S*)$/;
  const nameQuery = (text) => { const m = NAME_RE.exec(String(text || '')); return m ? m[1] : null; };
  const isCommand = (line) => /^\/(?:hyperwork|hw)(?:\s|$)/.test(String(line || '').trim());
  const secs = (ms, d) => (ms ? ' in ' + (Math.round(ms / d) / (1000 / d)) + ' s' : '');
  const errText = (e) => (e && typeof e === 'object' ? e.message || '' : e ? String(e) : '');
  // one event of a run's log as a line for the chat (a pure function: the page and its tests use it)
  function renderEvent(ev) {
    ev = ev || {};
    if (ev.type === 'node_end' && ev.status === 'ok' && !['start', 'end'].includes(ev.kind)) return { kind: 'step', text: '· ' + ev.node + ' (' + ev.kind + ') done' + secs(ev.durationMs, 100) };
    if (ev.type === 'node_end' && ev.status === 'error' && !ev.retry) return { kind: 'fail', text: '· ' + ev.node + ' (' + ev.kind + ') failed: ' + errText(ev.error) };
    if (ev.type === 'wait') return { kind: 'wait', text: 'The workflow waits for you at ' + ev.node + ' (' + (ev.what || 'approval') + ').' };
    if (ev.type === 'run_end' && ev.status === 'ok') {
      const result = typeof ev.result === 'string' ? ev.result : ev.result === undefined || ev.result === null ? '' : '```json\n' + JSON.stringify(ev.result, null, 2) + '\n```';
      return { kind: 'done', text: 'Hyper Workflow done (' + (ev.outcome || 'done') + ')' + secs(ev.durationMs, 1000) + '.', result };
    }
    if (ev.type === 'run_end') return { kind: 'ended', text: 'Hyper Workflow ended ' + (ev.status || '') + (errText(ev.error) ? ': ' + errText(ev.error) : '') };
    return { kind: null, text: '' };
  }
  const logPath = (run, from) => '/api/hyperwork/log?id=' + encodeURIComponent(run) + '&from=' + encodeURIComponent(from) + '&follow=1';

  function create(h) {
    let items = [], timer = null, lastQ = null, idx = 0;
    const hide = () => { h.menu.hidden = true; h.menu.textContent = ''; };
    const open = () => !h.menu.hidden;
    function render() {
      const box = h.menu; box.textContent = '';
      if (!items.length) { box.hidden = true; return; }
      idx = Math.min(idx, items.length - 1);
      items.forEach((w, i) => {
        const b = document.createElement('button'); b.type = 'button'; b.className = 'ghost hw' + (i === idx ? ' on' : '');
        const c = document.createElement('code'); c.textContent = w.shown || w.name;
        const d = document.createElement('small'); d.textContent = w.unrunnable ? 'cannot run: ' + w.unrunnable : (w.title || w.description || '');
        const scope = String(w.scope || '').replace(/^me-.*/, 'mine').replace(/^(team|grp)-/, '');
        b.append(c, ' ', d);
        if (scope) { const t = document.createElement('span'); t.className = 'tag'; t.textContent = scope; b.append(' ', t); }
        if (w.unrunnable) { b.style.opacity = '.55'; b.title = w.unrunnable; }
        b.onmousedown = (e) => { e.preventDefault(); pick(w); };
        box.append(b);
      });
      box.hidden = false;
    }
    function typeahead() {
      const q = nameQuery(h.input.value);
      if (q === null) { if (lastQ !== null) { lastQ = null; items = []; hide(); } return false; }
      if (lastQ !== q) {
        lastQ = q; idx = 0; clearTimeout(timer);
        timer = setTimeout(async () => {
          let got = [];
          try { const r = await h.api('/api/hyperwork/list?q=' + encodeURIComponent(q)); got = r.ok ? ((await r.json()).items || []) : []; } catch { got = []; }
          if (nameQuery(h.input.value) === q) { items = got; render(); }
        }, 150);
      }
      render();
      return true;
    }
    function pick(w) {
      const el = h.input;
      el.value = '/hyperwork ' + (w.shown || w.name) + ' '; hide(); lastQ = null;
      if (typeof el.focus === 'function') el.focus();
      if (typeof el.setSelectionRange === 'function') el.setSelectionRange(el.value.length, el.value.length);
      if (typeof el.dispatchEvent === 'function' && typeof Event !== 'undefined') el.dispatchEvent(new Event('input', { bubbles: true }));
    }
    function key(e) {
      if (!open() || !items.length) return false;
      if (e.key === 'ArrowDown') { e.preventDefault(); idx = (idx + 1) % items.length; render(); return true; }
      if (e.key === 'ArrowUp') { e.preventDefault(); idx = (idx - 1 + items.length) % items.length; render(); return true; }
      if (e.key === 'Tab' || (e.key === 'Enter' && !e.shiftKey)) { e.preventDefault(); pick(items[idx]); return true; }
      if (e.key === 'Escape') { e.preventDefault(); hide(); return true; }
      return false;
    }
    async function run(line) {
      line = String(line || '').trim().replace(/^\/hw(?=\s|$)/, '/hyperwork'); // the hub parses "/hyperwork <name> <prompt>"
      hide(); lastQ = null;
      h.me(line);
      let r;
      try { r = await h.api('/api/hyperwork/command', { method: 'POST', body: JSON.stringify({ line, project: (typeof h.project === 'function' && h.project()) || '' }) }); }
      catch (e) { h.say(errText(e) || 'Could not reach the hub.', true); return; }
      const d = await r.json().catch(() => ({}));
      if (r.status === 404 && Array.isArray(d.nearest)) { h.say((d.error || 'No such workflow.') + (d.nearest.length ? ' Did you mean: ' + d.nearest.join(', ') + '?' : ''), true); return; }
      if (r.status === 404) { h.say(d.error && /link/i.test(d.error) ? d.error : 'This hub has no Hyper Workflows (it is older, or the JavaScript hub serves alone).', true); return; }
      if (r.status === 422) { h.say('This workflow cannot run here: ' + (d.problems || []).map((p) => p.message || p.code).join('; '), true); return; }
      if (r.status !== 202) { h.say(d.error || 'Could not start the workflow (' + r.status + ').', true); return; }
      h.say('Hyper Workflow started (' + d.run + ').');
      await follow(d.run, 0);
    }
    async function follow(run, from) {
      let res;
      try { res = await fetch(h.streamUrl(logPath(run, from)), { headers: (typeof h.headers === 'function' && h.headers()) || {} }); }
      catch { h.say('Lost sight of the workflow run ' + run + '; it goes on on the hub.', true); return; }
      if (!res.ok) { h.say('Lost sight of the workflow run ' + run + ' (' + res.status + '); it goes on on the hub.', true); return; }
      const each = (l) => { let ev; try { ev = JSON.parse(l); } catch { return; } event(run, ev); };
      if (!res.body || typeof res.body.getReader !== 'function') { for (const l of (await res.text()).split('\n')) if (l.trim()) each(l); return; }
      const reader = res.body.getReader(), dec = new TextDecoder();
      let buf = '';
      for (;;) {
        let chunk;
        try { chunk = await reader.read(); } catch { h.say('Lost sight of the workflow run ' + run + '; it goes on on the hub.', true); return; }
        if (chunk.done) break;
        buf += dec.decode(chunk.value, { stream: true });
        let i;
        while ((i = buf.indexOf('\n')) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1); if (l.trim()) each(l); }
      }
      if (buf.trim()) each(buf);
    }
    function event(run, ev) {
      const out = renderEvent(ev);
      if (out.kind === 'step') h.say(out.text);
      else if (out.kind === 'fail') h.say(out.text, true);
      else if (out.kind === 'done') { if (out.result) h.answer(out.result); h.say(out.text); }
      else if (out.kind === 'ended') h.say(out.text, true);
      else if (out.kind === 'wait') {
        const el = h.say(out.text);
        if (!el || typeof document === 'undefined') return;
        for (const [label, ok] of [['Approve', true], ['Reject', false]]) {
          const b = document.createElement('button'); b.type = 'button'; b.textContent = label;
          b.onclick = async () => {
            el.querySelectorAll('button').forEach((x) => { x.disabled = true; });
            const answers = ev.what === 'resume' ? { rerun: ok } : { approved: ok };
            const r = await h.api('/api/hyperwork/answer', { method: 'POST', body: JSON.stringify({ run, node: ev.key || ev.node, answers }) }).catch(() => null);
            if (r && r.ok) follow(run, (ev.seq || 0) + 1); else h.say('Could not answer.', true);
          };
          el.append(' ', b);
        }
      }
    }
    return { typeahead, isCommand, run, pick, hide, open, key, get items() { return items; } };
  }
  return { create, nameQuery, isCommand, renderEvent };
})();
if (typeof window !== 'undefined') window.HyperworkUI = HyperworkUI;
if (typeof module !== 'undefined' && module.exports) module.exports = HyperworkUI;
