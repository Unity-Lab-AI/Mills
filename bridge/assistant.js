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
  const IMAGE_URL = (A.imageUrl || 'http://127.0.0.1:5124').replace(/\/$/, '');
  const IMAGE_CKPT = A.imageCheckpoint || path.join(MILLS_DIR, '..', 'Unity 3D Equational Model', 'Unity 18+', 'models', 'realistic-vision-v51.safetensors');
  const IMAGES_DIR = path.join(MILLS_DIR, 'Images'); // Images/<username>/<id>.png (private) and Images/room/<id>.png (team chat); gitignored
  // Her look in pictures, added after the scene she writes so the moment leads (the engine encodes
  // long prompts in pieces, so neither the scene nor her look is cut).
  const SELFIE_LOOK = A.selfieLook || '25 year old emo goth woman, black hair with hot pink streaks';
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
  // The image engine (bridge/image-engine.py, local Stable Diffusion) is started the same way.
  let imageStarting = null;
  async function imageUp() { try { return (await fetch(IMAGE_URL + '/health', { signal: AbortSignal.timeout(2000) })).ok; } catch { return false; } }
  function ensureImage() {
    if (imageStarting) return imageStarting;
    imageStarting = (async () => {
      if (await imageUp()) return true;
      const py = A.imagePython || (process.platform === 'win32' ? 'py' : 'python3');
      const pre = A.imagePythonArgs || (process.platform === 'win32' && !A.imagePython ? ['-3.10'] : []);
      const port = new URL(IMAGE_URL).port || '5124';
      try { spawn(py, [...pre, path.join(ROOT, 'image-engine.py'), '--port', port, '--checkpoint', IMAGE_CKPT], { detached: true, stdio: 'ignore', windowsHide: true }).unref(); } catch {}
      for (let i = 0; i < 40; i++) { if (await imageUp()) return true; await new Promise((r) => setTimeout(r, 1000)); }
      return false;
    })().finally(() => { imageStarting = null; });
    return imageStarting;
  }
  if (config.assistant !== false && !config.claudeCommand) ensureImage();

  // ---------- the world around the person: place, weather, news (keyless public services) ----------
  const cache = new Map();
  async function getJson(url, ttl = 600000, headers = {}) {
    const hit = cache.get(url);
    if (hit && Date.now() - hit.t < ttl) return hit.v;
    const r = await fetch(url, { headers: { 'User-Agent': 'Mills/1.0 (team app)', ...headers }, signal: AbortSignal.timeout(10000) });
    if (!r.ok) throw new Error(`${new URL(url).host} answered ${r.status}`);
    const v = url.includes('/rss') ? await r.text() : await r.json();
    cache.set(url, { t: Date.now(), v });
    return v;
  }
  async function ipPlace(ip) { // rough place of the address someone connects from (no browser prompt needed)
    if (!ip || /^(127\.|::1|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.)/.test(ip)) return null;
    try { const r = await getJson('https://ipwho.is/' + encodeURIComponent(ip), 6 * 3600e3); return r.success ? { lat: r.latitude, lon: r.longitude, place: [r.city, r.region, r.country].filter(Boolean).join(', '), country: r.country_code, tz: r.timezone && r.timezone.id } : null; } catch { return null; }
  }
  async function placeName(lat, lon) {
    try { const r = await getJson(`https://nominatim.openstreetmap.org/reverse?format=json&zoom=10&lat=${lat.toFixed(3)}&lon=${lon.toFixed(3)}`, 24 * 3600e3);
      const a = r.address || {}; return { place: [a.city || a.town || a.village || a.county, a.state, a.country].filter(Boolean).join(', '), country: (a.country_code || '').toUpperCase() }; } catch { return null; }
  }
  async function locate(ci, city) { // → { lat, lon, place, country } for a named city or the person
    if (city) {
      const r = await getJson('https://geocoding-api.open-meteo.com/v1/search?count=1&name=' + encodeURIComponent(city), 24 * 3600e3);
      const g = (r.results || [])[0]; if (!g) throw new Error('No place called ' + city);
      return { lat: g.latitude, lon: g.longitude, place: [g.name, g.admin1, g.country].filter(Boolean).join(', '), country: g.country_code };
    }
    if (ci.lat === null) throw new Error('No location for them yet; ask which city.');
    if (!ci.place) Object.assign(ci, await placeName(ci.lat, ci.lon) || {});
    return { lat: ci.lat, lon: ci.lon, place: ci.place, country: ci.country };
  }
  const WMO = { 0: 'clear', 1: 'mostly clear', 2: 'partly cloudy', 3: 'overcast', 45: 'fog', 48: 'freezing fog', 51: 'light drizzle', 53: 'drizzle', 55: 'heavy drizzle', 61: 'light rain', 63: 'rain', 65: 'heavy rain', 66: 'freezing rain', 67: 'heavy freezing rain', 71: 'light snow', 73: 'snow', 75: 'heavy snow', 77: 'snow grains', 80: 'rain showers', 81: 'rain showers', 82: 'violent rain showers', 85: 'snow showers', 86: 'heavy snow showers', 95: 'thunderstorm', 96: 'thunderstorm with hail', 99: 'thunderstorm with heavy hail' };
  const usUnits = (ci, country) => (country || '').toUpperCase() === 'US' || /-US$/i.test(ci.locale);
  async function weather(ci, city, days = 0) {
    const at = await locate(ci, city), us = usUnits(ci, at.country);
    const q = `latitude=${at.lat}&longitude=${at.lon}&timezone=auto&current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m`
      + `&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&forecast_days=${Math.min(7, Math.max(1, days || 1))}`
      + (us ? '&temperature_unit=fahrenheit&wind_speed_unit=mph' : '');
    const r = await getJson('https://api.open-meteo.com/v1/forecast?' + q, 600000);
    const c = r.current || {}, d = r.daily || {}, u = us ? '°F' : '°C';
    return { ok: true, place: at.place, now: `${WMO[c.weather_code] || 'weather code ' + c.weather_code}, ${Math.round(c.temperature_2m)}${u} (feels ${Math.round(c.apparent_temperature)}${u}), humidity ${c.relative_humidity_2m}%, wind ${Math.round(c.wind_speed_10m)} ${us ? 'mph' : 'km/h'}`,
      days: (d.time || []).map((t, i) => `${t}: ${WMO[d.weather_code[i]] || ''}, ${Math.round(d.temperature_2m_min[i])}–${Math.round(d.temperature_2m_max[i])}${u}, rain ${d.precipitation_probability_max[i] ?? '?'}%`) };
  }
  async function news(ci, topic, local) {
    const lang = (ci.locale || 'en-US').split('-')[0], gl = (ci.country || (ci.locale.split('-')[1]) || 'US').toUpperCase();
    const base = `hl=${lang}-${gl}&gl=${gl}&ceid=${gl}:${lang}`;
    let q = topic || '';
    if (local && !q) { try { q = (await locate(ci)).place.split(',')[0]; } catch {} }
    const url = q ? `https://news.google.com/rss/search?q=${encodeURIComponent(q)}&${base}` : `https://news.google.com/rss?${base}`;
    const xml = await getJson(url, 900000);
    const items = [...xml.matchAll(/<item>([\s\S]*?)<\/item>/g)].slice(0, 8).map((m) => {
      const tag = (t) => ((new RegExp(`<${t}>([\\s\\S]*?)</${t}>`).exec(m[1]) || [])[1] || '').replace(/<!\[CDATA\[|\]\]>/g, '').replace(/&amp;/g, '&').replace(/&#39;/g, "'").replace(/&quot;/g, '"').trim();
      return { title: tag('title'), when: tag('pubDate'), source: tag('source') };
    });
    return { ok: true, about: q || 'top stories', items };
  }

  // ---------- images ----------
  const IMG_ID = /^[a-z0-9-]{1,40}$/;
  async function makeImage(ci, args) {
    if (!(await ensureImage())) return { ok: false, error: 'The camera app on the host is not starting (image engine down).' };
    const shape = String(args.shape || args.orientation || 'square').toLowerCase();
    const [width, height] = /portrait|tall|vertical/.test(shape) ? [576, 1024] : /landscape|wide|horizontal/.test(shape) ? [1024, 576] : [768, 768];
    const scene = String(args.prompt || '').slice(0, 1000).trim();
    const prompt = args.selfie ? `${scene}, she is a ${SELFIE_LOOK}, candid photo` : scene;
    const r = await fetch(IMAGE_URL + '/txt2img', { method: 'POST', body: JSON.stringify({ prompt, width, height }), signal: AbortSignal.timeout(300000) });
    if (!r.ok) return { ok: false, error: 'Image engine: ' + ((await r.json().catch(() => ({}))).error || r.status) };
    const scope = ci.scope || 'room';
    const id = Date.now().toString(36) + '-' + require('crypto').randomBytes(3).toString('hex');
    fs.mkdirSync(path.join(IMAGES_DIR, scope), { recursive: true });
    fs.writeFileSync(path.join(IMAGES_DIR, scope, id + '.png'), Buffer.from(await r.arrayBuffer()));
    const ref = scope + '/' + id;
    (ci.images || (ci.images = [])).push(ref);
    return { ok: true, image: ref, shown: 'The picture is attached to your reply automatically.', seconds: r.headers.get('x-seconds') };
  }

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
  function clientInfo(c, acct) {
    c = c && typeof c === 'object' ? c : {};
    const k = (acct && acct.client) || {};
    const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
    const okTz = (z) => { if (typeof z !== 'string' || z.length > 64) return null; try { new Intl.DateTimeFormat('en-US', { timeZone: z }); return z; } catch { return null; } };
    const browserLoc = num(c.lat) !== null && num(c.lon) !== null;
    return { tz: okTz(c.tz) || k.tz || 'UTC', locale: typeof c.locale === 'string' && c.locale.length < 20 ? c.locale : k.locale || 'en-US',
      lat: browserLoc ? num(c.lat) : k.lat ?? null, lon: browserLoc ? num(c.lon) : k.lon ?? null,
      place: browserLoc ? '' : k.place || '', country: browserLoc ? '' : k.country || '', scope: acct ? acct.username : 'room' };
  }
  // What the hub knows about where someone is: their browser (time zone, locale, location if shared)
  // and, when they come through the tunnel, the rough place of their address. Kept in memory only.
  async function learnClient(acct, c, ip) {
    const prev = acct.client || {};
    const k = { ...prev };
    if (c && typeof c === 'object') {
      if (typeof c.tz === 'string') k.tz = c.tz.slice(0, 64);
      if (typeof c.locale === 'string') k.locale = c.locale.slice(0, 20);
      if (typeof c.lat === 'number' && typeof c.lon === 'number') { k.lat = c.lat; k.lon = c.lon; k.place = ''; k.country = ''; k.fromBrowser = true; }
    }
    if (!k.fromBrowser && ip && ip !== prev.ip) {
      const g = await ipPlace(ip);
      if (g) Object.assign(k, { lat: g.lat, lon: g.lon, place: g.place, country: g.country }, k.tz ? {} : { tz: g.tz });
    }
    k.ip = ip || prev.ip;
    if (k.fromBrowser && !k.place && k.lat != null) Object.assign(k, await placeName(k.lat, k.lon) || {});
    acct.client = k;
    return k;
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
  tool('get_location', 'Where the person is (their browser location if shared, otherwise roughly from their connection).', {}, [], async (a, s, ci) => {
    if (ci.lat === null) return { ok: false, error: 'No location for them; their time zone is ' + ci.tz };
    const at = await locate(ci); return { ok: true, place: at.place, latitude: at.lat, longitude: at.lon, timezone: ci.tz };
  });
  tool('get_weather', "Current weather and today's outlook where the person is, or in a named city.", { city: P('City, optional (default: where they are)') }, [], (a, s, ci) => weather(ci, a.city, 1));
  tool('get_forecast', 'Weather forecast for the next days where the person is, or in a named city.', { days: P('Days (1-7, default 5)', 'integer'), city: P('City, optional') }, [], (a, s, ci) => weather(ci, a.city, Number(a.days) || 5));
  tool('get_news', 'Current news headlines: local to where the person is, on a topic, or top stories in their country.', { topic: P('Topic to search, optional'), local: P('true for news about where they are', 'boolean') }, [], (a, s, ci) => news(ci, a.topic, a.local === true || a.local === 'true'));
  tool('generate_image', 'Make a picture with the image engine (photos, art, scenes, objects; selfie true for a picture of yourself). The picture is shown with your reply.', {
    prompt: P('The picture, written fresh from THIS moment, most important first: what is happening right now in the conversation, '
      + 'what you are doing, where you are, who is with you, what you are wearing for it, expression, the time of day and the weather where they are, '
      + 'mood, lighting, camera angle. For a selfie never describe your face or hair (that is added for you): describe the moment.'),
    selfie: P('true when the picture shows you', 'boolean'), shape: P('square (default), portrait or landscape'),
  }, ['prompt'], (a, s, ci) => makeImage(ci, { ...a, selfie: a.selfie === true || a.selfie === 'true' }));
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
  async function execute(acct, name, args, ci) {
    const t = TOOLS[name];
    if (!t) return { ok: false, error: `No tool called ${name}.` };
    for (const r of t.required) if (args[r] === undefined || args[r] === '') return { ok: false, error: `${name} needs "${r}".` };
    const s = store(acct);
    try { const out = await t.run(args, s, ci); save(s); return out; } catch (e) { return { ok: false, error: e.message }; }
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
        + (ci.lat !== null ? ` Where they are: ${ci.place || `${ci.lat.toFixed(3)}, ${ci.lon.toFixed(3)}`} (use get_weather / get_news for anything local).` : ' Their location is unknown; ask which city for local things.'),
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
  async function resolvePending(acct, text, ci, out) {
    const p = acct.assistantPending;
    if (!p) return '';
    if (YES.test(text.trim())) { acct.assistantPending = null; const r = await execute(acct, p.tool, p.args, ci); out({ type: 'tool_result', name: p.tool, result: r }); return `[confirmed] ${p.tool} -> ${JSON.stringify(r).slice(0, 800)}`; }
    if (NO.test(text.trim())) { acct.assistantPending = null; return `[cancelled] the user declined ${p.tool}`; }
    return '';
  }
  // A tool call from either brain. Destructive tools wait for the person's yes/no.
  async function callTool(acct, name, args, ci, out) {
    out({ type: 'tool', name, args });
    let result;
    if (TOOLS[name] && TOOLS[name].confirm) {
      acct.assistantPending = { tool: name, args };
      out({ type: 'confirm', tool: name, args });
      result = { ok: false, pending: true, message: `${name} needs ${acct.display}'s confirmation; ask them to say yes or no.` };
    } else result = await execute(acct, name, args, ci);
    if (name === 'generate_image' && result.ok) out({ type: 'image', ref: result.image });
    if (name === 'say' && args.text) out({ type: 'say', text: String(args.text), emotion: args.emotion || 'neutral', act: args.act || 'statement' });
    out({ type: 'tool_result', name, result });
    return result;
  }

  async function runLocal(acct, text, ci, out, signal, opts = {}) {
    const st = await ensureOllama();
    const model = st.model ? LOCAL_MODEL : FALLBACK_MODEL;
    if (!st.up || !model) throw new Error(st.up ? `The local brain (${LOCAL_MODEL}) is not loaded on the host yet.` : 'The local brain (Ollama) is not running on the host.');
    const s = store(acct);
    const native = await nativeTools(model);
    const sayMode = model.startsWith('unity-brain');
    let system = persona() + '\n\n' + dynamicContext(acct, ci, sayMode);
    const tools = Object.values(TOOLS).filter((t) => sayMode || t.name !== 'say');
    if (!native) system += '\n\nTOOL USE: when you need a tool, reply with ONLY one line of JSON: {"tool": "<name>", "arguments": {...}}. You get the result next. Tools:\n' + tools.map((t) => `- ${t.name}(${Object.keys(t.params).join(', ')}): ${t.description}`).join('\n');
    const note = await resolvePending(acct, text, ci, out);
    if (opts.extraSystem) system += '\n\n' + opts.extraSystem;
    const messages = [{ role: 'system', content: system }, ...(opts.noHistory ? [] : s.history.slice(-HISTORY_KEEP)), { role: 'user', content: note ? `${text}\n\n(system note: ${note})` : text }];
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
        const result = seen.has(sig) ? { ok: false, error: `${c.name} already ran with these arguments this turn; use its result.` } : await callTool(acct, c.name, c.args, ci, out);
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

  async function runClaudeBrain(acct, text, ci, out, model) {
    const note = await resolvePending(acct, text, ci, out);
    return new Promise((resolve, reject) => {
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
    await learnClient(acct, body.client, body.ip);
    const ci = clientInfo(body.client, acct);
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
      s.history.push({ role: 'user', content: text }, { role: 'assistant', content: (reply || '(did it)') + (ci.images || []).map((r) => `\n[[image:${r}]]`).join('') });
      if (s.history.length > HISTORY_KEEP * 2) s.history.splice(0, s.history.length - HISTORY_KEEP * 2);
      save(s);
      out({ type: 'final', text: reply, brain, images: ci.images || [], pending: acct.assistantPending || null });
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

  const clientIp = (req) => String(req.headers['cf-connecting-ip'] || '').trim();
  function stopRun(acct) { const r = runs.get(acct.username); if (r) { r.abort.abort(); if (r.child) r.child.kill(); } }

  // The team chat's Unity: one local-brain run for whoever called her (their tools, their time and
  // place). Pictures go to Images/room (everyone in the room may see them) or the private scope given.
  async function ask(acct, text, { extraSystem = '', scope = 'room' } = {}) {
    const ci = clientInfo({}, acct); ci.scope = scope;
    const reply = await runLocal(acct, text, ci, () => {}, undefined, { extraSystem, noHistory: true });
    return { text: reply, images: ci.images || [] };
  }
  async function greeting(acct) { // facts for a sign-in hello: their local time, weather and headlines
    const ci = clientInfo({}, acct), facts = [`Their local time: ${localNow(ci)} (${ci.tz}).`];
    if (ci.lat !== null) {
      try { const w = await weather(ci, '', 1); facts.push(`Weather in ${w.place}: ${w.now}; today ${w.days[0] || ''}.`); } catch {}
      try { const n = await news(ci, '', true); if (n.items.length) facts.push(`Local news (${n.about}): ` + n.items.slice(0, 3).map((x) => x.title).join(' | ')); } catch {}
    } else facts.push('Their location is unknown (no weather or local news).');
    return facts.join('\n');
  }

  return {
    ask, greeting, learnClient,
    localReady: async () => (await ensureOllama()).model,
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
      return send(res, 200, { ok: true, result: await callTool(acct, name, args || {}, acct.assistantCi || clientInfo({}, acct), out) });
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
      if (p === '/api/assistant/chat' && req.method === 'POST') { await chat(acct, { ...(await readBody(req)), ip: clientIp(req) }, res); return true; }
      if (p === '/api/assistant/stop' && req.method === 'POST') { stopRun(acct); send(res, 200, { ok: true }); return true; }
      if (p === '/api/assistant/answer' && req.method === 'POST') { // yes/no button on a confirmation card
        const { yes } = await readBody(req); const pnd = acct.assistantPending;
        if (!pnd) { send(res, 200, { ok: false, error: 'Nothing is waiting for an answer.' }); return true; }
        acct.assistantPending = null;
        send(res, 200, yes ? { ok: true, tool: pnd.tool, result: await execute(acct, pnd.tool, pnd.args, acct.assistantCi || clientInfo({}, acct)) } : { ok: true, cancelled: pnd.tool });
        return true;
      }
      if (p === '/api/assistant/clear' && req.method === 'POST') {
        if (runs.has(acct.username)) { send(res, 409, { error: 'Wait for her to finish first.' }); return true; }
        const s = store(acct); s.history = []; save(s); delete acct.state.assistantClaudeSession; acct.assistantPending = null;
        send(res, 200, { ok: true }); return true;
      }
      if (p === '/api/assistant/client' && req.method === 'POST') { // the browser reports its clock / place after sign-in
        const { client } = await readBody(req);
        const k = await learnClient(acct, client, clientIp(req));
        send(res, 200, { ok: true, tz: k.tz, place: k.place || '' }); return true;
      }
      if (p === '/api/assistant/image' && req.method === 'GET') { // ?ref=<scope>/<id>: room pictures for everyone, private ones for their owner
        const ref = new URL(req.url, 'http://x').searchParams.get('ref') || '';
        const [scope, id] = ref.split('/');
        if (!IMG_ID.test(id || '') || !(scope === 'room' || scope === acct.username)) { send(res, 404, { error: 'No such picture.' }); return true; }
        const f = path.join(IMAGES_DIR, scope, id + '.png');
        if (!fs.existsSync(f)) { send(res, 404, { error: 'No such picture.' }); return true; }
        res.writeHead(200, { 'Content-Type': 'image/png', 'Cache-Control': 'private, max-age=86400' });
        fs.createReadStream(f).pipe(res); return true;
      }
      if (p === '/api/assistant/data' && req.method === 'GET') { const s = store(acct); const { _file, history, ...d } = s; send(res, 200, d); return true; }
      return false;
    },
  };
};
