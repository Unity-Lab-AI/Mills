// localai.js 1.0.0: this computer's own Ollama, found from the page and listed as "Local (this computer)" (T-208, T-209).
// Product-neutral: no page globals, only what the page passes in (opts: native, user, onChange, refresh, origin).
// The desktop app asks its own program (command `ollama_local`, no CORS); a browser asks the page's way, which Ollama
// refuses unless OLLAMA_ORIGINS allows this page: a no-cors ping tells "running but not allowed" (hint) from "not
// running" (nothing shown at all). Work and the API run on the hub or an AI host, never in the browser: the way to
// use these models is this computer's AI host, so the list is display only.
//   LOCALAI.probe({ native, force, onChange })   -> { found, blocked, models, at }   (cached 30 s; onChange(state) once on a change)
//   LOCALAI.group(select, providers, { user })   adds a greyed "Local (this computer)" optgroup when found and no own host lends one
//   LOCALAI.line(box, providers, { native, user, origin, onChange, refresh })   draws the small status line (nothing when not running)
//   LOCALAI.fromTags(json)  LOCALAI.state()  LOCALAI.ownProvider(providers, user)
const LOCALAI = (() => {
  const LOCAL_OLLAMA = 'http://127.0.0.1:11434';
  const fromTags = (j) => (j && Array.isArray(j.models) ? j.models.map((m) => m && (m.name || m.model)).filter((n) => typeof n === 'string' && n).slice(0, 40) : []);
  let localOllama = { found: false, blocked: false, models: [], at: 0 };
  async function probe(o) {
    o = o || {};
    if (!o.force && Date.now() - localOllama.at < 30000) return localOllama;
    let r = { found: false, blocked: false, models: [], at: Date.now() };
    try {
      if (o.native) { const d = await o.native.call('ollama_local'); r.found = !!(d && d.found); r.models = r.found ? (d.models || []) : []; r.url = d && d.url; }
      else {
        const ac = new AbortController(), t = setTimeout(() => ac.abort(), 1500);
        try { const x = await fetch(LOCAL_OLLAMA + '/api/tags', { signal: ac.signal }); if (x.ok) { r.models = fromTags(await x.json()); r.found = true; } }
        catch { try { await fetch(LOCAL_OLLAMA + '/api/version', { mode: 'no-cors', signal: ac.signal }); r.blocked = true; } catch {} } // answered, but not to this page
        clearTimeout(t);
      }
    } catch {}
    const changed = r.found !== localOllama.found || r.blocked !== localOllama.blocked || r.models.join('|') !== localOllama.models.join('|');
    localOllama = r;
    if (changed && typeof o.onChange === 'function') o.onChange(r);
    return r;
  }
  // does one of this person's own AI hosts already lend a local model (an Ollama it found)?
  const ownProvider = (provs, user) => (provs || []).find((p) => p.kind === 'host-openai' && p.owner === user && p.online !== false);
  // the model list: this computer's models, shown and never selectable
  function group(sel, provs, o) {
    if (!(localOllama.found && localOllama.models.length && !ownProvider(provs, o && o.user))) return;
    const g = document.createElement('optgroup'); g.label = 'Local (this computer) · not for Work: lend it through your AI host (Settings → AI source)';
    for (const m of localOllama.models) { const op = document.createElement('option'); op.value = 'local-ollama|' + m; op.textContent = m; op.disabled = true; g.append(op); }
    sel.append(g);
  }
  function line(box, provs, o) {
    o = o || {}; const native = o.native;
    const wrap = document.createElement('div'); wrap.className = 'status'; wrap.hidden = true; box.append(wrap);
    probe({ native, force: true, onChange: o.onChange }).then(async (r) => {
      if (!r.found && !r.blocked) return; // nothing running here: nothing new on the page
      wrap.hidden = false; wrap.textContent = '';
      const h = document.createElement('b'); h.textContent = 'Local (this computer)'; wrap.append(h);
      if (r.blocked) {
        wrap.append(document.createTextNode(': Ollama is running here, but your browser may not ask it. Start Ollama with OLLAMA_ORIGINS="' + (o.origin || location.origin) + '" to list its models here, or use the desktop app, which asks it directly.'));
        return;
      }
      wrap.append(document.createTextNode(': ' + (r.models.length ? r.models.join(', ') : 'Ollama is running, no model pulled yet') + '. '));
      const own = ownProvider(provs, o.user);
      const how = document.createElement('span');
      if (own) how.textContent = 'Your AI host "' + (own.nickname || own.label) + '" lends them (greyed in the model list: Work needs a Claude-capable provider; pick them in API access and Unity).';
      else {
        how.textContent = 'To use them, lend them through this computer\'s AI host (it finds Ollama by itself; the shared provider stays the default). ';
        if (native) {
          const b = document.createElement('button'); b.textContent = 'Start this computer\'s AI host';
          b.onclick = async () => { b.disabled = true; try { const st = await native.call('host_status'); if (!st || !st.configured) how.textContent = 'Set it up first: My AI hosts → "Run on this PC".'; else { await native.call('host_act', { what: 'start' }); how.textContent = 'Starting the AI host; the models appear in a moment.'; if (typeof o.refresh === 'function') setTimeout(o.refresh, 5000); } } catch (e) { how.textContent = String(e.message || e); } b.disabled = false; };
          how.append(b);
        } else how.append(document.createTextNode('Settings → My AI hosts (download vr-host, or use the desktop app).'));
      }
      wrap.append(how);
    });
  }
  return { probe, group, line, fromTags, ownProvider, state: () => localOllama, URL: LOCAL_OLLAMA };
})();
if (typeof window !== 'undefined') window.LOCALAI = LOCALAI;
if (typeof module !== 'undefined' && module.exports) module.exports = LOCALAI;
