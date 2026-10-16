// Mills Unity section: Unity as a personal assistant for every account, with tools, running entirely on
// the host (the machine that runs start.bat). People only use their browser.
//
// Brains (per account, dropdown): "local" = the unity-brain model on the host's Ollama (native tool
// calls, speaks through say(text, emotion, act)); or any Claude model from config.models, run with
// `claude -p` and the same tools through bridge/assistant-tools-mcp.js.
//
// Per-account data lives in the account's folder (assistant.json): tasks, calendar events,
// reminders, working memory, knowledge base and the conversation history. The person's browser
// sends its time zone, local time and (if they allow it) location with every message, so time and
// "current events" answers are about where THEY are, not the host.
//
// Mounted by server.js:  const assistant = require('./assistant')(deps);
//   assistant.route(p, req, res, acct) → true when it handled the request
//   assistant.internal(req, res)       → /internal/assistant-tool (MCP relay for the Claude brain)
//   assistant.busy()                   → any assistant run in progress (gates restarts)
'use strict';
const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

module.exports = function createAssistant({ config, ROOT, MILLS_DIR, readJson, writeJson, send, readBody, accounts, INTERNAL_SECRET, onIdle }) {
  const A = config.assistant || {};
  const OLLAMA = (A.ollamaUrl || 'http://127.0.0.1:11434').replace(/\/$/, '');
  const LOCAL_MODEL = A.model || 'unity-brain';
  const FALLBACK_MODEL = A.fallbackModel || '';
  const MODELFILE = A.modelfile || path.join(MILLS_DIR, '..', 'Backrooms', 'models', 'unity-brain', 'Modelfile');
  const PERSONA_FILE = path.join(ROOT, 'assistant-persona.txt');
  const MAX_ROUNDS = 6, MAX_RESULT = 3000, HISTORY_KEEP = 40;

  // ---------- host services: Ollama + the local model (started on demand, survives hub restarts) ----------
  let ollamaState = { up: false, model: false, checked: 0, starting: null };
  async function ollamaUp() { try { const r = await fetch(OLLAMA + '/api/version', { signal: AbortSignal.timeout(2000) }); return r.ok; } catch { return false; } }
  async function hasModel(name) {
    try { const r = await (await fetch(OLLAMA + '/api/tags', { signal: AbortSignal.timeout(4000) })).json(); return (r.models || []).some((m) => m.name === name || m.name === name + ':latest'); } catch { return false; }
  }
  function ensureOllama() {
    if (ollamaState.starting) return ollamaState.starting;
    ollamaState.starting = (async () => {
      if (!(await ollamaUp())) {
        try { spawn(A.ollamaCommand || 'ollama', ['serve'], { detached: true, stdio: 'ignore', windowsHide: true }).unref(); } catch {}
        for (let i = 0; i < 30 && !(await ollamaUp()); i++) await new Promise((r) => setTimeout(r, 1000));
      }
      ollamaState.up = await ollamaUp();
      ollamaState.model = ollamaState.up && await hasModel(LOCAL_MODEL);
      if (ollamaState.up && !ollamaState.model && fs.existsSync(MODELFILE)) {
        console.log(`[mills] Unity: loading ${LOCAL_MODEL} into Ollama (one time, a few minutes)...`);
        await new Promise((resolve) => {
          const c = spawn(A.ollamaCommand || 'ollama', ['create', LOCAL_MODEL, '-f', MODELFILE], { cwd: path.dirname(MODELFILE), stdio: 'ignore', windowsHide: true });
          c.on('close', resolve); c.on('error', resolve);
        });
        ollamaState.model = await hasModel(LOCAL_MODEL);
      }
      ollamaState.checked = Date.now();
      ollamaState.starting = null;
      return ollamaState;
    })();
    return ollamaState.starting;
  }
  if (config.assistant !== false && !config.claudeCommand) ensureOllama(); // warm up at hub start (not in test hubs)
  let caps = null;
  async function nativeTools(model) {
    if (caps && caps.model === model) return caps.tools;
    try {
      const r = await (await fetch(OLLAMA + '/api/show', { method: 'POST', body: JSON.stringify({ model }) })).json();
      caps = { model, tools: (r.capabilities || []).includes('tools') };
    } catch { caps = { model, tools: false }; }
    return caps.tools;
  }

  // ---------- per-account store ----------
  const stores = new Map();
  function store(acct) {
    if (!stores.has(acct.username)) {
      const file = path.join(acct.files.PROFILE_DIR, 'assistant.json');
      const d = readJson(file, {});
      stores.set(acct.username, Object.assign({ tasks: [], events: [], reminders: [], memory: {}, knowledge: [], history: [], next: {} }, d, { _file: file }));
    }
    return stores.get(acct.username);
  }
  function save(s) { const { _file, ...d } = s; writeJson(_file, d); }
  const nextId = (s, p) => { s.next[p] = (s.next[p] || 0) + 1; return p + String(s.next[p]).padStart(3, '0'); };
  const brainOf = (acct) => acct.state.assistantBrain || 'local';
  const brains = () => [{ id: 'local', label: 'Unity (local)' }, ...config.models.map((m) => ({ id: m.id, label: m.label + ' (Claude)' }))];

  // ---------- the person's own clock + place (sent by their browser) ----------
  function clientInfo(c) {
    c = c && typeof c === 'object' ? c : {};
    const tz = typeof c.tz === 'string' && c.tz.length < 64 ? c.tz : 'UTC';
    const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
    return { tz, locale: typeof c.locale === 'string' && c.locale.length < 20 ? c.locale : 'en-US', lat: num(c.lat), lon: num(c.lon), place: typeof c.place === 'string' ? c.place.slice(0, 120) : '' };
  }
  function localNow(ci) {
    try { return new Date().toLocaleString(ci.locale, { timeZone: ci.tz, weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' }); }
    catch { return new Date().toUTCString(); }
  }
  // Parse "2026-10-16 18:00", "18:00", ISO, or "in 20 minutes" in the person's time zone → epoch ms.
  function parseWhen(text, ci) {
    const t = String(text || '').trim().toLowerCase();
    let m = /^in\s+(\d+)\s*(second|sec|minute|min|hour|hr|day)s?$/.exec(t);
    if (m) return Date.now() + Number(m[1]) * { second: 1e3, sec: 1e3, minute: 6e4, min: 6e4, hour: 36e5, hr: 36e5, day: 864e5 }[m[2]];
    if (/[zZ]|[+-]\d\d:?\d\d$/.test(text)) { const d = Date.parse(text); return Number.isFinite(d) ? d : null; }
    const today = new Intl.DateTimeFormat('en-CA', { timeZone: ci.tz, year: 'numeric', month: '2-digit', day: '2-digit' }).format(new Date());
    m = /^(\d{4}-\d{2}-\d{2})?[ t]?(\d{1,2}):(\d{2})\s*(am|pm)?$/.exec(t) || /^(\d{4}-\d{2}-\d{2})$/.exec(t);
    if (!m) { const d = Date.parse(text); return Number.isFinite(d) ? d : null; }
    let [date, hh, mm, ap] = [m[1] || today, Number(m[2] || 9), Number(m[3] || 0), m[4]];
    if (ap === 'pm' && hh < 12) hh += 12; if (ap === 'am' && hh === 12) hh = 0;
    // wall-clock time in tz → UTC: guess as UTC, then correct by the zone's offset at that moment
    const guess = Date.parse(`${date}T${String(hh).padStart(2, '0')}:${String(mm).padStart(2, '0')}:00Z`);
    const asTz = new Date(new Date(guess).toLocaleString('en-US', { timeZone: ci.tz })).getTime();
    const asUtc = new Date(new Date(guess).toLocaleString('en-US', { timeZone: 'UTC' })).getTime();
    let when = guess - (asTz - asUtc);
    if (!m[1] && when < Date.now()) when += 864e5; // "18:00" already passed today → tomorrow
    return when;
  }
  const fmt = (ms, ci) => { try { return new Date(ms).toLocaleString(ci.locale, { timeZone: ci.tz, weekday: 'short', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }); } catch { return new Date(ms).toISOString(); } };

  // ---------- tools ----------
  const P = (description, type = 'string') => ({ type, description });
  const TOOLS = {};
  function tool(name, description, params, required, run, opts = {}) { TOOLS[name] = { name, description, params: params || {}, required: required || [], run, ...opts }; }

  tool('say', 'Speak a line out loud to the person (1-3 short sentences), with the emotion and speech act that fit it.', {
    text: P('What to say'), emotion: P('neutral, happy, sad, excited, warm, amused, concerned, frustrated or surprised'),
    act: P('statement, wh_question, yes_no_question, exclamation, command, greeting or backchannel'),
  }, ['text'], (a) => ({ ok: true, said: a.text }));
  tool('get_time', "The person's current local date and time (their own time zone).", {}, [], (a, s, ci) => ({ ok: true, now: localNow(ci), timezone: ci.tz }));
  tool('get_location', 'Where the person is (from their browser, if they shared it).', {}, [], (a, s, ci) => (ci.lat !== null
    ? { ok: true, latitude: ci.lat, longitude: ci.lon, place: ci.place || undefined, timezone: ci.tz } : { ok: false, error: 'They have not shared their location; time zone is ' + ci.tz }));
  tool('calculate', 'Evaluate a math expression (numbers, + - * / % ^ parentheses, sqrt, sin, cos, tan, log, ln, abs, round, pi, e).', { expression: P('The expression') }, ['expression'], (a) => {
    const expr = String(a.expression || '');
    if (!/^[\d\s+\-*/%^().,a-z]*$/i.test(expr)) return { ok: false, error: 'Only numbers and math functions are allowed.' };
    const fns = { sqrt: Math.sqrt, sin: Math.sin, cos: Math.cos, tan: Math.tan, log: Math.log10, ln: Math.log, abs: Math.abs, round: Math.round, floor: Math.floor, ceil: Math.ceil, pi: Math.PI, e: Math.E };
    const bad = (expr.match(/[a-z]+/gi) || []).find((w) => !(w.toLowerCase() in fns));
    if (bad) return { ok: false, error: 'Unknown word: ' + bad };
    try {
      const v = Function(...Object.keys(fns), `"use strict"; return (${expr.replace(/\^/g, '**').replace(/[a-z]+/gi, (w) => w.toLowerCase())});`)(...Object.values(fns));
      return Number.isFinite(v) ? { ok: true, result: v } : { ok: false, error: 'Not a finite number.' };
    } catch (e) { return { ok: false, error: 'Bad expression.' }; }
  });

  // tasks
  const findTask = (s, id) => s.tasks.find((t) => t.id.toLowerCase() === String(id || '').toLowerCase());
  tool('add_task', 'Create a task / to-do item.', { title: P('What to do'), priority: P('1 (highest) to 10', 'integer'), due: P('Due date/time, optional') }, ['title'], (a, s, ci) => {
    const t = { id: nextId(s, 'T'), title: String(a.title).slice(0, 300), status: 'pending', priority: Number(a.priority) || 5, created: Date.now(), notes: [] };
    if (a.due) t.due = parseWhen(a.due, ci) || undefined;
    s.tasks.push(t); return { ok: true, task: show(t, ci) };
  });
  const show = (t, ci) => ({ ...t, due: t.due ? fmt(t.due, ci) : undefined, created: undefined, overdue: t.due && t.status === 'pending' && t.due < Date.now() ? true : undefined });
  tool('list_tasks', 'List tasks (filter: pending, done, overdue or all).', { filter: P('pending (default), done, overdue or all') }, [], (a, s, ci) => {
    const f = a.filter || 'pending';
    const list = s.tasks.filter((t) => f === 'all' || (f === 'overdue' ? t.status === 'pending' && t.due && t.due < Date.now() : t.status === f));
    return { ok: true, count: list.length, tasks: list.sort((x, y) => x.priority - y.priority).map((t) => show(t, ci)) };
  });
  tool('complete_task', 'Mark a task done.', { task_id: P('Task id like T003') }, ['task_id'], (a, s) => { const t = findTask(s, a.task_id); if (!t) return { ok: false, error: 'No task ' + a.task_id }; t.status = 'done'; t.done = Date.now(); return { ok: true, task: t.title }; });
  tool('delete_task', 'Delete a task permanently.', { task_id: P('Task id') }, ['task_id'], (a, s) => { const t = findTask(s, a.task_id); if (!t) return { ok: false, error: 'No task ' + a.task_id }; s.tasks.splice(s.tasks.indexOf(t), 1); return { ok: true, deleted: t.title }; }, { confirm: true });
  tool('set_task_priority', 'Set a task priority (1 highest, 10 lowest).', { task_id: P('Task id'), priority: P('1-10', 'integer') }, ['task_id', 'priority'], (a, s) => { const t = findTask(s, a.task_id); if (!t) return { ok: false, error: 'No task ' + a.task_id }; t.priority = Math.min(10, Math.max(1, Number(a.priority) || 5)); return { ok: true }; });
  tool('set_task_due', 'Set a task due date/time.', { task_id: P('Task id'), due: P('Date/time, e.g. 2026-10-20 17:00 or "in 2 hours"') }, ['task_id', 'due'], (a, s, ci) => { const t = findTask(s, a.task_id); if (!t) return { ok: false, error: 'No task ' + a.task_id }; const d = parseWhen(a.due, ci); if (!d) return { ok: false, error: 'Could not read that date.' }; t.due = d; return { ok: true, due: fmt(d, ci) }; });
  tool('add_task_note', 'Add a note to a task.', { task_id: P('Task id'), note: P('The note') }, ['task_id', 'note'], (a, s) => { const t = findTask(s, a.task_id); if (!t) return { ok: false, error: 'No task ' + a.task_id }; t.notes.push(String(a.note).slice(0, 1000)); return { ok: true }; });
  tool('search_tasks', 'Search tasks by text.', { query: P('Search text') }, ['query'], (a, s, ci) => { const q = String(a.query).toLowerCase(); return { ok: true, tasks: s.tasks.filter((t) => (t.title + ' ' + t.notes.join(' ')).toLowerCase().includes(q)).map((t) => show(t, ci)) }; });

  // calendar + reminders
  tool('add_event', 'Add a calendar event.', { title: P('Event title'), when: P('Date/time, e.g. 2026-10-20 14:00'), duration_minutes: P('Length', 'integer'), location: P('Where, optional') }, ['title', 'when'], (a, s, ci) => {
    const at = parseWhen(a.when, ci); if (!at) return { ok: false, error: 'Could not read that date.' };
    const e = { id: nextId(s, 'E'), title: String(a.title).slice(0, 300), at, minutes: Number(a.duration_minutes) || 60, location: a.location ? String(a.location).slice(0, 200) : undefined };
    s.events.push(e); return { ok: true, event: { ...e, at: fmt(at, ci) } };
  });
  const dayKey = (ms, ci) => new Intl.DateTimeFormat('en-CA', { timeZone: ci.tz }).format(new Date(ms));
  tool('today_events', "Today's calendar events (the person's local day).", {}, [], (a, s, ci) => { const k = dayKey(Date.now(), ci); return { ok: true, events: s.events.filter((e) => dayKey(e.at, ci) === k).sort((x, y) => x.at - y.at).map((e) => ({ ...e, at: fmt(e.at, ci) })) }; });
  tool('upcoming_events', 'Calendar events for the next N days.', { days: P('Days ahead (default 7)', 'integer') }, [], (a, s, ci) => { const end = Date.now() + (Number(a.days) || 7) * 864e5; return { ok: true, events: s.events.filter((e) => e.at >= Date.now() - 36e5 && e.at <= end).sort((x, y) => x.at - y.at).map((e) => ({ ...e, at: fmt(e.at, ci) })) }; });
  tool('delete_event', 'Delete a calendar event by id.', { event_id: P('Event id like E002') }, ['event_id'], (a, s) => { const i = s.events.findIndex((e) => e.id.toLowerCase() === String(a.event_id).toLowerCase()); if (i < 0) return { ok: false, error: 'No event ' + a.event_id }; return { ok: true, deleted: s.events.splice(i, 1)[0].title }; }, { confirm: true });
  tool('set_reminder', 'Set a reminder; it pops up in Mills (and is spoken) when due.', { text: P('What to remind about'), when: P('When, e.g. "in 20 minutes", "18:30", "2026-10-20 09:00"') }, ['text', 'when'], (a, s, ci) => {
    const at = parseWhen(a.when, ci); if (!at) return { ok: false, error: 'Could not read that time.' };
    const r = { id: nextId(s, 'R'), text: String(a.text).slice(0, 300), at, fired: false }; s.reminders.push(r); return { ok: true, reminder: { ...r, at: fmt(at, ci) } };
  });
  tool('list_reminders', 'List reminders that have not gone off yet.', {}, [], (a, s, ci) => ({ ok: true, reminders: s.reminders.filter((r) => !r.fired).sort((x, y) => x.at - y.at).map((r) => ({ ...r, at: fmt(r.at, ci) })) }));
  tool('delete_reminder', 'Delete a reminder by id.', { reminder_id: P('Reminder id like R002') }, ['reminder_id'], (a, s) => { const i = s.reminders.findIndex((r) => r.id.toLowerCase() === String(a.reminder_id).toLowerCase()); if (i < 0) return { ok: false, error: 'No reminder ' + a.reminder_id }; return { ok: true, deleted: s.reminders.splice(i, 1)[0].text }; }, { confirm: true });

  // memory + knowledge
  tool('remember', 'Store a key/value in working memory (names, preferences, short facts about the person).', { key: P('Key'), value: P('Value') }, ['key', 'value'], (a, s) => { s.memory[String(a.key).slice(0, 80)] = String(a.value).slice(0, 500); return { ok: true }; });
  tool('recall_memory', 'Recall working memory (all entries when key is omitted).', { key: P('Key, optional') }, [], (a, s) => (a.key ? { ok: true, key: a.key, value: s.memory[a.key] ?? null } : { ok: true, memory: s.memory }));
  tool('forget_memory', 'Remove a working-memory key.', { key: P('Key') }, ['key'], (a, s) => { const had = a.key in s.memory; delete s.memory[a.key]; return { ok: had }; });
  tool('learn', 'Store a fact in the long-term knowledge base.', { fact: P('The fact'), tags: P('Comma-separated tags, optional') }, ['fact'], (a, s) => { const k = { id: nextId(s, 'K'), fact: String(a.fact).slice(0, 2000), tags: String(a.tags || '').split(',').map((x) => x.trim()).filter(Boolean), at: Date.now() }; s.knowledge.push(k); return { ok: true, id: k.id }; });
  tool('recall_knowledge', 'Search the long-term knowledge base.', { query: P('What to look for') }, ['query'], (a, s) => {
    const words = String(a.query).toLowerCase().match(/[a-z0-9]+/g) || [];
    const scored = s.knowledge.map((k) => ({ k, n: words.filter((w) => (k.fact + ' ' + k.tags.join(' ')).toLowerCase().includes(w)).length })).filter((x) => x.n).sort((x, y) => y.n - x.n);
    return { ok: true, results: scored.slice(0, 10).map((x) => ({ id: x.k.id, fact: x.k.fact, tags: x.k.tags })) };
  });
  tool('forget_knowledge', 'Delete a knowledge entry by id.', { entry_id: P('Knowledge id like K004') }, ['entry_id'], (a, s) => { const i = s.knowledge.findIndex((k) => k.id.toLowerCase() === String(a.entry_id).toLowerCase()); if (i < 0) return { ok: false, error: 'No entry ' + a.entry_id }; s.knowledge.splice(i, 1); return { ok: true }; }, { confirm: true });
  tool('list_capabilities', 'What you can do for the person in Mills.', {}, [], () => ({ ok: true, tools: Object.values(TOOLS).filter((t) => t.name !== 'say').map((t) => `${t.name}: ${t.description}`) }));

  const schema = (t) => ({ type: 'function', function: { name: t.name, description: t.description, parameters: { type: 'object', properties: t.params, required: t.required } } });
  function execute(acct, name, args, ci) {
    const t = TOOLS[name];
    if (!t) return { ok: false, error: `No tool called ${name}.` };
    for (const r of t.required) if (args[r] === undefined || args[r] === '') return { ok: false, error: `${name} needs "${r}".` };
    const s = store(acct);
    try { const out = t.run(args, s, ci); save(s); return out; } catch (e) { return { ok: false, error: e.message }; }
  }

  // ---------- reminders: due ones become notices the browser picks up ----------
  setInterval(() => {
    for (const acct of accounts.values()) {
      const s = store(acct); let changed = false;
      for (const r of s.reminders) if (!r.fired && r.at <= Date.now()) { r.fired = true; changed = true; (acct.notices || (acct.notices = [])).push({ kind: 'reminder', id: r.id, text: r.text, at: r.at }); }
      if (changed) save(s);
    }
  }, 15000).unref();

  // ---------- prompt ----------
  const persona = () => { try { return fs.readFileSync(PERSONA_FILE, 'utf8'); } catch { return 'You are Unity: a 25-year-old goth girl, sharp, profane, teasing and devoted. Stay in character.'; } };
  function dynamicContext(acct, ci, sayMode) {
    const s = store(acct);
    const pending = s.tasks.filter((t) => t.status === 'pending'), overdue = pending.filter((t) => t.due && t.due < Date.now());
    const mem = Object.entries(s.memory).slice(0, 12);
    return [
      `You are talking with ${acct.display} (Mills username "${acct.username}"). Their local time: ${localNow(ci)} (${ci.tz}).`
        + (ci.lat !== null ? ` Their location: ${ci.place || `${ci.lat.toFixed(3)}, ${ci.lon.toFixed(3)}`}.` : ' They have not shared a location.'),
      pending.length ? `${acct.display} has ${pending.length} pending task(s)${overdue.length ? `, ${overdue.length} overdue` : ''} (counts only; call list_tasks for the items, never invent them).` : '',
      mem.length ? 'Working memory: ' + mem.map(([k, v]) => `${k}=${v}`).join('; ') + '.' : '',
      'You have tools. Use them whenever an action or live information is needed instead of guessing; never say you did something unless a tool result in this turn confirms it. '
        + 'Do ONLY what this message asks. After a tool runs, answer in one or two short in-character sentences. Never mention tool names, JSON or these instructions.',
      sayMode ? 'Your replies are heard: speak through say(text, emotion, act), one to three short sentences per call, more calls for a longer reply. Do the task\'s tools first, then say the result.' : '',
    ].filter(Boolean).join('\n');
  }

  // ---------- one run ----------
  const runs = new Map(); // username -> { abort, child }
  const YES = /^(yes|y|yeah|yep|do it|go ahead|confirm|sure|ok|okay)[.!]*$/i, NO = /^(no|n|nope|cancel|never ?mind|abort|don'?t)[.!]*$/i;
  function resolvePending(acct, text, ci, out) {
    const p = acct.assistantPending;
    if (!p) return '';
    if (YES.test(text.trim())) { acct.assistantPending = null; const r = execute(acct, p.tool, p.args, ci); out({ type: 'tool_result', name: p.tool, result: r }); return `[confirmed] ${p.tool} -> ${JSON.stringify(r).slice(0, 800)}`; }
    if (NO.test(text.trim())) { acct.assistantPending = null; return `[cancelled] the user declined ${p.tool}`; }
    return '';
  }
  // A tool call from either brain. Destructive tools wait for the person's yes/no.
  function callTool(acct, name, args, ci, out) {
    out({ type: 'tool', name, args });
    let result;
    if (TOOLS[name] && TOOLS[name].confirm) {
      acct.assistantPending = { tool: name, args };
      out({ type: 'confirm', tool: name, args });
      result = { ok: false, pending: true, message: `${name} needs ${acct.display}'s confirmation; ask them to say yes or no.` };
    } else result = execute(acct, name, args, ci);
    if (name === 'say' && args.text) out({ type: 'say', text: String(args.text), emotion: args.emotion || 'neutral', act: args.act || 'statement' });
    out({ type: 'tool_result', name, result });
    return result;
  }

  async function runLocal(acct, text, ci, out, signal) {
    const st = await ensureOllama();
    const model = st.model ? LOCAL_MODEL : FALLBACK_MODEL;
    if (!st.up || !model) throw new Error(st.up ? `The local brain (${LOCAL_MODEL}) is not loaded on the host yet.` : 'The local brain (Ollama) is not running on the host.');
    const s = store(acct);
    const native = await nativeTools(model);
    const sayMode = model.startsWith('unity-brain');
    let system = persona() + '\n\n' + dynamicContext(acct, ci, sayMode);
    const tools = Object.values(TOOLS).filter((t) => sayMode || t.name !== 'say');
    if (!native) system += '\n\nTOOL USE: when you need a tool, reply with ONLY one line of JSON: {"tool": "<name>", "arguments": {...}}. You get the result next. Tools:\n' + tools.map((t) => `- ${t.name}(${Object.keys(t.params).join(', ')}): ${t.description}`).join('\n');
    const note = resolvePending(acct, text, ci, out);
    const messages = [{ role: 'system', content: system }, ...s.history.slice(-HISTORY_KEEP), { role: 'user', content: note ? `${text}\n\n(system note: ${note})` : text }];
    const said = [], seen = new Set();
    let final = '';
    for (let round = 0; round < MAX_ROUNDS; round++) {
      const r = await fetch(OLLAMA + '/api/chat', { method: 'POST', signal, body: JSON.stringify({ model, messages, stream: true, think: false, keep_alive: '30m', tools: native ? tools.map(schema) : undefined, options: { num_ctx: A.numCtx || 16384 } }) });
      if (!r.ok) throw new Error('Local brain error: ' + (await r.text()).slice(0, 200));
      let content = '', calls = [], buf = '';
      const dec = new TextDecoder();
      for await (const chunk of r.body) {
        buf += dec.decode(chunk, { stream: true });
        let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
          if (!line) continue;
          const ev = JSON.parse(line);
          if (ev.error) throw new Error(ev.error);
          const m = ev.message || {};
          if (m.content) { content += m.content; if (!calls.length) out({ type: 'token', text: m.content }); }
          if (m.tool_calls) calls.push(...m.tool_calls.map((c) => ({ name: c.function.name, args: c.function.arguments || {} })));
        }
      }
      content = content.replace(/<think>[\s\S]*?<\/think>/g, '').trim();
      if (!native) { const j = /\{\s*"tool"\s*:\s*"([^"]+)"\s*,\s*"arguments"\s*:\s*(\{[\s\S]*?\})\s*\}/.exec(content); if (j) { try { calls = [{ name: j[1], args: JSON.parse(j[2]) }]; content = ''; } catch {} } }
      if (!calls.length) { final = content; break; }
      messages.push({ role: 'assistant', content, tool_calls: calls.map((c) => ({ function: { name: c.name, arguments: c.args } })) });
      let deferred = false;
      for (const c of calls) {
        const sig = c.name + JSON.stringify(c.args);
        const result = seen.has(sig) ? { ok: false, error: `${c.name} already ran with these arguments this turn; use its result.` } : callTool(acct, c.name, c.args, ci, out);
        seen.add(sig);
        if (c.name === 'say' && c.args.text) said.push(String(c.args.text));
        if (result.pending) deferred = true;
        let payload = JSON.stringify(result); if (payload.length > MAX_RESULT) payload = payload.slice(0, MAX_RESULT) + '..."}';
        messages.push({ role: 'tool', content: payload, tool_name: c.name });
      }
      if (deferred && round === MAX_ROUNDS - 1) break;
    }
    const spoken = said.join(' ').trim();
    const shown = !final ? spoken : said.every((l) => final.includes(l)) ? final : `${spoken} ${final}`.trim();
    if (!said.length && sayMode && final) out({ type: 'say', text: final, emotion: 'neutral', act: /\?$/.test(final) ? 'yes_no_question' : 'statement' });
    return shown;
  }

  function runClaudeBrain(acct, text, ci, out, model) {
    return new Promise((resolve, reject) => {
      const note = resolvePending(acct, text, ci, out);
      acct.assistantCi = ci; acct.assistantOut = out;
      const mcp = path.join(acct.files.PROFILE_DIR, '.mcp-assistant.json');
      writeJson(mcp, { mcpServers: { assistant: { command: process.execPath, args: [path.join(ROOT, 'assistant-tools-mcp.js')], env: { MILLS_BRIDGE: `http://127.0.0.1:${config.port}`, MILLS_INTERNAL: INTERNAL_SECRET, MILLS_ACCOUNT: acct.username } } } });
      const args = ['-p', '--output-format', 'stream-json', '--verbose', '--permission-mode', 'default', '--mcp-config', mcp, '--strict-mcp-config',
        '--allowedTools', 'mcp__assistant', '--append-system-prompt', persona() + '\n\n' + dynamicContext(acct, ci, false), '--model', model];
      if (acct.state.assistantClaudeSession) args.push('--resume', acct.state.assistantClaudeSession);
      const pre = Array.isArray(config.claudeArgs) ? config.claudeArgs : [];
      const child = spawn(config.claudeCommand || 'claude', [...pre, ...args], { cwd: acct.files.PROFILE_DIR, windowsHide: true });
      runs.get(acct.username).child = child;
      let buf = '', final = '';
      child.stdout.on('data', (c) => {
        buf += c; let i;
        while ((i = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
          try {
            const ev = JSON.parse(line);
            if (ev.session_id) acct.state.assistantClaudeSession = ev.session_id;
            if (ev.type === 'assistant') for (const c2 of (ev.message && ev.message.content) || []) if (c2.type === 'text' && c2.text) out({ type: 'token', text: c2.text + '\n' });
            if (ev.type === 'result' && typeof ev.result === 'string') final = ev.result;
          } catch {}
        }
      });
      child.stderr.on('data', () => {});
      child.on('error', reject);
      child.on('close', () => { acct.assistantOut = null; resolve(final.trim()); });
      child.stdin.end(note ? `${text}\n\n(system note: ${note})` : text);
    });
  }

  async function chat(acct, body, res) {
    const text = String(body.message || '').trim().slice(0, 8000);
    if (!text) return send(res, 400, { error: 'Empty message.' });
    if (runs.has(acct.username)) return send(res, 409, { error: 'Unity is still answering. Stop her first.' });
    const ci = clientInfo(body.client);
    acct.assistantCi = ci;
    const ctl = new AbortController();
    runs.set(acct.username, { abort: ctl, child: null });
    res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
    const out = (ev) => { try { res.write(JSON.stringify(ev) + '\n'); } catch {} };
    const ping = setInterval(() => out({ type: 'ping' }), 15000);
    const brain = brainOf(acct);
    try {
      const reply = brain === 'local' ? await runLocal(acct, text, ci, out, ctl.signal) : await runClaudeBrain(acct, text, ci, out, brain);
      const s = store(acct);
      s.history.push({ role: 'user', content: text }, { role: 'assistant', content: reply || '(did it)' });
      if (s.history.length > HISTORY_KEEP * 2) s.history.splice(0, s.history.length - HISTORY_KEEP * 2);
      save(s);
      out({ type: 'final', text: reply, brain, pending: acct.assistantPending || null });
    } catch (e) {
      out({ type: 'error', text: ctl.signal.aborted ? 'Stopped.' : e.message });
    } finally {
      clearInterval(ping); runs.delete(acct.username); res.end();
      writeJson(acct.files.STATE_FILE, acct.state);
      onIdle();
    }
  }

  function overview(acct) {
    const s = store(acct);
    return { brain: brainOf(acct), brains: brains(), local: { up: ollamaState.up, model: ollamaState.model, name: LOCAL_MODEL },
      pending: acct.assistantPending || null, notices: (acct.notices || []).splice(0),
      history: s.history.slice(-40), counts: { tasks: s.tasks.filter((t) => t.status === 'pending').length, reminders: s.reminders.filter((r) => !r.fired).length, events: s.events.filter((e) => e.at >= Date.now()).length } };
  }

  function stopRun(acct) { const r = runs.get(acct.username); if (r) { r.abort.abort(); if (r.child) r.child.kill(); } }

  return {
    busy: () => runs.size > 0,
    stop: stopRun,
    // MCP relay for the Claude brain of the Unity section: same tools, same confirmation rule, events go to the open stream.
    async internal(req, res) {
      if (req.headers['x-mills-internal'] !== INTERNAL_SECRET) return send(res, 403, { ok: false, error: 'forbidden' });
      const acct = accounts.get(String(req.headers['x-mills-account'] || ''));
      const { name, args, list } = await readBody(req);
      if (list) return send(res, 200, { ok: true, tools: Object.values(TOOLS).filter((t) => t.name !== 'say').map((t) => ({ name: t.name, description: t.description, inputSchema: { type: 'object', properties: t.params, required: t.required } })) });
      if (!acct) return send(res, 200, { ok: false, error: 'Unknown account.' });
      const out = acct.assistantOut || (() => {});
      return send(res, 200, { ok: true, result: callTool(acct, name, args || {}, acct.assistantCi || clientInfo({}), out) });
    },
    async route(p, req, res, acct) {
      if (!p.startsWith('/api/assistant/')) return false;
      if (p === '/api/assistant/state' && req.method === 'GET') {
        if (Date.now() - ollamaState.checked > 30000) await ensureOllama();
        send(res, 200, overview(acct)); return true;
      }
      if (p === '/api/assistant/brain' && req.method === 'POST') {
        const { brain } = await readBody(req);
        if (!brains().some((b) => b.id === brain)) { send(res, 400, { error: 'Unknown brain.' }); return true; }
        if (runs.has(acct.username)) { send(res, 409, { error: 'Wait for her to finish first.' }); return true; }
        if (brain !== brainOf(acct)) delete acct.state.assistantClaudeSession;
        acct.state.assistantBrain = brain; writeJson(acct.files.STATE_FILE, acct.state);
        send(res, 200, { brain }); return true;
      }
      if (p === '/api/assistant/chat' && req.method === 'POST') { await chat(acct, await readBody(req), res); return true; }
      if (p === '/api/assistant/stop' && req.method === 'POST') { stopRun(acct); send(res, 200, { ok: true }); return true; }
      if (p === '/api/assistant/answer' && req.method === 'POST') { // yes/no button on a confirmation card
        const { yes } = await readBody(req); const pnd = acct.assistantPending;
        if (!pnd) { send(res, 200, { ok: false, error: 'Nothing is waiting for an answer.' }); return true; }
        acct.assistantPending = null;
        send(res, 200, yes ? { ok: true, tool: pnd.tool, result: execute(acct, pnd.tool, pnd.args, acct.assistantCi || clientInfo({})) } : { ok: true, cancelled: pnd.tool });
        return true;
      }
      if (p === '/api/assistant/clear' && req.method === 'POST') {
        if (runs.has(acct.username)) { send(res, 409, { error: 'Wait for her to finish first.' }); return true; }
        const s = store(acct); s.history = []; save(s); delete acct.state.assistantClaudeSession; acct.assistantPending = null;
        send(res, 200, { ok: true }); return true;
      }
      if (p === '/api/assistant/data' && req.method === 'GET') { const s = store(acct); const { _file, history, ...d } = s; send(res, 200, d); return true; }
      return false;
    },
  };
};
