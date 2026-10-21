// VibeRoller Unity section: Unity as a personal assistant for every account, with tools, running entirely on
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

module.exports = function createAssistant({ config, ROOT, MILLS_DIR, readJson, writeJson, send, readBody, accounts, INTERNAL_SECRET, onIdle, aiNodes = null }) {
  const A = config.assistant || {};
  // behind the Rust hub with its unity module, Unity is its own (native/crates/vr-hub/src/modules/unity*.rs): this one
  // starts no engines and fires no reminders (assistant.json is the Rust hub's then)
  const MOVED = !!process.env.VR_BACK_PORT && String(process.env.VR_FRONT_MODULES || '').split(',').includes('unity');
  const OLLAMA = (A.ollamaUrl || 'http://127.0.0.1:11434').replace(/\/$/, '');
  const LOCAL_MODEL = A.model || 'unity-brain';
  const FALLBACK_MODEL = A.fallbackModel || '';
  const MODELFILE = A.modelfile || path.join(MILLS_DIR, '..', 'Backrooms', 'models', 'unity-brain', 'Modelfile');
  const PERSONA_FILE = path.join(ROOT, 'assistant-persona.txt');
  const IMAGE_URL = (A.imageUrl || 'http://127.0.0.1:5124').replace(/\/$/, '');
  const VOICE_URL = (A.voiceUrl || 'http://127.0.0.1:5123').replace(/\/$/, '');
  const EARS_URL = (A.speechUrl || 'http://127.0.0.1:5125').replace(/\/$/, '');
  // Hub elsewhere (the lab server), engines on the host's PC: with "via": "node" every call to one of Unity's four
  // engines goes through a connected AI node (bridge/ai-nodes.js); every other fetch (web search, pages) goes out
  // as usual. The node is handed only the engine's name and the path, never this hub's idea of an address.
  const VIA_NODE = A.via === 'node' && !!aiNodes;
  const ENGINE_BASES = [['ollama', OLLAMA], ['image', IMAGE_URL], ['voice', VOICE_URL], ['speech', EARS_URL]];
  const fetch = (url, init) => {
    if (VIA_NODE) for (const [engine, base] of ENGINE_BASES) if (String(url).startsWith(base + '/')) return aiNodes.engineFetch(engine, String(url).slice(base.length), init || {});
    return globalThis.fetch(url, init);
  };
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
      if (!(await ollamaUp()) && !VIA_NODE) { // start Ollama here and give it time; via a node its host runs it, and a missing node must not hold anyone up
        try { spawn(A.ollamaCommand || 'ollama', ['serve'], { detached: true, stdio: 'ignore', windowsHide: true }).unref(); } catch {}
        for (let i = 0; i < 30 && !(await ollamaUp()); i++) await new Promise((r) => setTimeout(r, 1000));
      }
      ollamaState.up = await ollamaUp();
      ollamaState.model = ollamaState.up && await hasModel(LOCAL_MODEL);
      if (!VIA_NODE && ollamaState.up && !ollamaState.model && fs.existsSync(MODELFILE)) {
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
  if (config.assistant !== false && !config.claudeCommand && !MOVED) ensureOllama(); // warm up at hub start (not in test hubs)
  // Python services on the host (Python 3.10 with torch): the image engine (bridge/image-engine.py),
  // Unity's voice (unity-voice-v2/serve.py) and the speech engine (bridge/speech-engine.py, whisper).
  // Each is started detached when it does not answer, so they survive hub restarts.
  // pyw on Windows: the windowless launcher, so the services run in the background with no console windows
  const PY = A.python || (process.platform === 'win32' ? 'pyw' : 'python3');
  const PY_ARGS = A.pythonArgs || (process.platform === 'win32' && !A.python ? ['-3.10'] : []);
  const VOICE_DIR = A.voiceDir || path.join(MILLS_DIR, '..', 'Backrooms', 'models', 'unity-voice-v2');
  const WHISPER = A.whisperModel || path.join(MILLS_DIR, '..', 'Unity 3D Equational Model', 'Unity 18+', 'models', 'whisper-small');
  const starting = new Map();
  async function up(url) { try { return (await fetch(url + '/health', { signal: AbortSignal.timeout(2000) })).ok; } catch { return false; } }
  function ensureService(url, args, cwd) {
    if (starting.has(url)) return starting.get(url);
    const p = (async () => {
      if (await up(url)) return true;
      // Windowless Python has no console, so the script's prints would crash it: run it through a
      // two-line wrapper that sends its output nowhere first (the scripts themselves stay unchanged).
      const [script, ...rest] = args;
      const boot = 'import os,sys,runpy; sys.stdout = sys.stderr = open(os.devnull, "w"); sys.argv = sys.argv[1:]; sys.path.insert(0, os.path.dirname(sys.argv[0])); runpy.run_path(sys.argv[0], run_name="__main__")';
      if (!VIA_NODE) { try { spawn(PY, [...PY_ARGS, '-c', boot, script, ...rest], { cwd, detached: true, stdio: 'ignore', windowsHide: true }).unref(); } catch {} } // via a node: its host runs the engines
      for (let i = 0; i < 60; i++) { if (await up(url)) return true; await new Promise((r) => setTimeout(r, 1000)); }
      return false;
    })().finally(() => starting.delete(url));
    starting.set(url, p);
    return p;
  }
  const port = (url, d) => new URL(url).port || d;
  const ensureImage = () => ensureService(IMAGE_URL, [path.join(ROOT, 'image-engine.py'), '--port', port(IMAGE_URL, '5124'), '--checkpoint', IMAGE_CKPT], ROOT);
  const ensureVoice = () => ensureService(VOICE_URL, [path.join(VOICE_DIR, 'serve.py'), '--port', port(VOICE_URL, '5123')], VOICE_DIR);
  const ensureEars = () => ensureService(EARS_URL, [path.join(ROOT, 'speech-engine.py'), '--port', port(EARS_URL, '5125'), '--model', WHISPER], ROOT);
  if (config.assistant !== false && !config.claudeCommand && !MOVED) { ensureImage(); ensureVoice(); ensureEars(); }
  const EMOTIONS = ['neutral', 'warm', 'happy', 'amused', 'excited', 'sad', 'concerned', 'frustrated', 'surprised'];
  const ACTS = ['statement', 'yes_no_question', 'wh_question', 'exclamation', 'instruction', 'reassurance', 'aside'];

  // ---------- the world around the person: place, weather, news (keyless public services) ----------
  const cache = new Map();
  async function getJson(url, ttl = 600000, headers = {}) {
    const hit = cache.get(url);
    if (hit && Date.now() - hit.t < ttl) return hit.v;
    const r = await fetch(url, { headers: { 'User-Agent': 'VibeRoller/1.0 (team app)', ...headers }, signal: AbortSignal.timeout(10000) });
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

  // ---------- the web: search, read a page, YouTube (keyless; the host does the fetching) ----------
  const UA = { 'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130 Safari/537.36', 'Accept-Language': 'en-US,en;q=0.8' };
  const SEARCH_URL = A.searchUrl || 'https://www.bing.com/search';
  const YT_URL = A.youtubeUrl || 'https://www.youtube.com/results';
  const unhtml = (t) => String(t || '').replace(/<script[\s\S]*?<\/script>|<style[\s\S]*?<\/style>|<noscript[\s\S]*?<\/noscript>/gi, ' ').replace(/<[^>]+>/g, ' ')
    .replace(/&nbsp;/g, ' ').replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#0?39;/g, "'").replace(/&#(\d+);/g, (m, n) => String.fromCharCode(n)).replace(/\s+/g, ' ').trim();
  function realLink(href) { // Bing wraps results as /ck/a?...&u=a1<base64url of the address>
    href = href.replace(/&amp;/g, '&');
    const u = /[?&]u=a1([^&]+)/.exec(href);
    if (u) { try { return Buffer.from(u[1].replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8'); } catch {} }
    return href;
  }
  async function webSearch(q) {
    const r = await fetch(`${SEARCH_URL}?q=${encodeURIComponent(q)}&setlang=en`, { headers: UA, signal: AbortSignal.timeout(12000) });
    if (!r.ok) throw new Error('search answered ' + r.status);
    const html = await r.text();
    const results = [];
    for (const m of html.matchAll(/<li class="b_algo"[\s\S]*?<\/li>/g)) {
      const a = /<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/.exec(m[0]);
      if (!a) continue;
      const snip = /<p[^>]*>([\s\S]*?)<\/p>/.exec(m[0]);
      results.push({ title: unhtml(a[2]), url: realLink(a[1]), snippet: unhtml(snip ? snip[1] : '').slice(0, 300) });
      if (results.length >= 8) break;
    }
    return { ok: true, query: q, results };
  }
  // read_page never reaches the host's own network (no localhost, private ranges, link-local, odd ports)
  // unless the hub config allows it (only the regression runner's throwaway hub does).
  async function publicUrl(raw) {
    let u; try { u = new URL(raw); } catch { throw new Error('Not a web address.'); }
    if (!/^https?:$/.test(u.protocol)) throw new Error('Only http and https pages.');
    if (A.allowPrivateFetch) return u;
    if (u.port && !['80', '443', '8080'].includes(u.port)) throw new Error('That port is not allowed.');
    const ips = /^\[?[\d.:a-f]+\]?$/i.test(u.hostname) ? [u.hostname.replace(/[[\]]/g, '')] : (await require('dns').promises.lookup(u.hostname, { all: true })).map((x) => x.address);
    const bad = (ip) => /^(127\.|10\.|0\.|169\.254\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.)/.test(ip) || /^(::1|::$|f[cd]|fe80|::ffff:(127|10|192\.168|169\.254)\.)/i.test(ip) || ip === 'localhost';
    if (!ips.length || ips.some(bad) || /^localhost$/i.test(u.hostname)) throw new Error('That address is on a private network; only public pages.');
    return u;
  }
  async function readPage(raw) {
    let u = await publicUrl(raw);
    for (let hop = 0; hop < 4; hop++) { // follow redirects ourselves so every hop is checked
      const r = await fetch(u, { headers: UA, redirect: 'manual', signal: AbortSignal.timeout(15000) });
      if (r.status >= 300 && r.status < 400 && r.headers.get('location')) { u = await publicUrl(new URL(r.headers.get('location'), u).href); continue; }
      if (!r.ok) throw new Error('The page answered ' + r.status);
      const type = r.headers.get('content-type') || '';
      if (!/text|html|json|xml/.test(type)) throw new Error('Not a text page (' + type + ').');
      const body = (await r.text()).slice(0, 2e6);
      const title = unhtml((/<title[^>]*>([\s\S]*?)<\/title>/i.exec(body) || [])[1] || '');
      const main = /<(article|main)[\s\S]*?<\/\1>/i.exec(body);
      return { ok: true, url: u.href, title, text: unhtml(main ? main[0] : body).slice(0, 6000) };
    }
    throw new Error('Too many redirects.');
  }
  async function youtube(q) {
    const r = await fetch(`${YT_URL}?search_query=${encodeURIComponent(q)}`, { headers: UA, signal: AbortSignal.timeout(12000) });
    const html = await r.text();
    const m = /"videoRenderer":\{"videoId":"([\w-]{11})"[\s\S]*?"title":\{"runs":\[\{"text":"((?:[^"\\]|\\.)*)"/.exec(html) || /"videoId":"([\w-]{11})"/.exec(html);
    if (!m) throw new Error('No video found for ' + q);
    return { id: m[1], title: m[2] ? JSON.parse('"' + m[2] + '"') : q };
  }

  // ---------- images ----------
  const IMG_ID = /^[a-z0-9-]{1,40}$/;
  // A message that asks for a picture (any wording, including one-word asks) and whether it is about her.
  const PIC_WORDS = /\b(pics?|pictures?|photos?|selfies?|images?|snaps?|nudes?|draw|paint|render|show me|send me|let me see|titties|tits|boobs|ass|booty|body)\b/i;
  const wantsPicture = (t) => PIC_WORDS.test(String(t || ''));
  const aboutHer = (t) => !/(of|draw|paint|render|make)\s+(a|an|the|some|my|this|that|his|her)/i.test(String(t || '')) || /(you|your|yourself|selfie)/i.test(String(t || ''));
  async function makeImage(ci, args) {
    if (!(await ensureImage())) return { ok: false, error: 'The camera app on the host is not starting (image engine down).' };
    const shape = String(args.shape || args.orientation || 'square').toLowerCase();
    const [width, height] = /portrait|tall|vertical/.test(shape) ? [576, 1024] : /landscape|wide|horizontal/.test(shape) ? [1024, 576] : [768, 768];
    const scene = String(args.prompt || '').slice(0, 1000).trim();
    const prompt = args.selfie ? `${scene}, she is a ${SELFIE_LOOK}, candid photo` : scene;
    return render(ci, '/txt2img', { prompt, width, height });
  }
  async function render(ci, route, body) {
    const r = await fetch(IMAGE_URL + route, { method: 'POST', body: JSON.stringify(body), signal: AbortSignal.timeout(300000) });
    if (!r.ok) return { ok: false, error: 'Image engine: ' + ((await r.json().catch(() => ({}))).error || r.status) };
    const ref = savePicture(ci.scope || 'room', Buffer.from(await r.arrayBuffer()));
    (ci.images || (ci.images = [])).push(ref);
    if (ci.acct) ci.acct.lastImage = ref;
    return { ok: true, image: ref, shown: 'The picture is attached to your reply automatically.', seconds: r.headers.get('x-seconds') };
  }
  function savePicture(scope, buf) {
    const id = Date.now().toString(36) + '-' + require('crypto').randomBytes(3).toString('hex');
    fs.mkdirSync(path.join(IMAGES_DIR, scope), { recursive: true });
    fs.writeFileSync(path.join(IMAGES_DIR, scope, id + '.png'), buf);
    return scope + '/' + id;
  }
  // A picture this person may use: theirs, a room one, or "last" (the last one made or sent in their chat).
  function readPicture(acct, ref) {
    if (!ref || ref === 'last' || ref === 'camera') ref = ref === 'camera' ? acct.lastPhoto : acct.lastImage;
    const [scope, id] = String(ref || '').split('/');
    if (!IMG_ID.test(id || '') || !(scope === 'room' || scope === acct.username)) return null;
    const f = path.join(IMAGES_DIR, scope, id + '.png');
    return fs.existsSync(f) ? fs.readFileSync(f).toString('base64') : null;
  }
  async function editImage(ci, a, route) {
    if (!(await ensureImage())) return { ok: false, error: 'The camera app on the host is not starting (image engine down).' };
    const src = readPicture(ci.acct, a.image);
    if (!src) return { ok: false, error: 'No picture to work from: send one (camera / file) or make one first.' };
    const body = { prompt: String(a.prompt || '').slice(0, 1000), image: src, strength: Number(a.strength) || undefined };
    if (a.keep_face === true || a.keep_face === 'true') body.face = true;
    if (route === '/inpaint') {
      const box = Array.isArray(a.box) ? a.box.map(Number) : String(a.box || '').split(/[ ,]+/).filter(Boolean).map(Number);
      if (box.length !== 4 || box.some((v) => !Number.isFinite(v))) return { ok: false, error: 'box needs 4 numbers 0-1: left, top, right, bottom.' };
      body.box = box;
    }
    return render(ci, route, body);
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
      place: browserLoc ? '' : k.place || '', country: browserLoc ? '' : k.country || '', scope: acct ? acct.username : 'room', acct };
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
    text: P('What to say'), emotion: P(EMOTIONS.join(', ')), act: P(ACTS.join(', ')),
  }, ['text'], (a) => ({ ok: true, said: a.text }));
  tool('get_time', "The person's current local date and time (their own time zone).", {}, [], (a, s, ci) => ({ ok: true, now: localNow(ci), timezone: ci.tz }));
  tool('get_location', 'Where the person is (their browser location if shared, otherwise roughly from their connection).', {}, [], async (a, s, ci) => {
    if (ci.lat === null) return { ok: false, error: 'No location for them; their time zone is ' + ci.tz };
    const at = await locate(ci); return { ok: true, place: at.place, latitude: at.lat, longitude: at.lon, timezone: ci.tz };
  });
  tool('get_weather', "Current weather and today's outlook where the person is, or in a named city.", { city: P('City, optional (default: where they are)') }, [], (a, s, ci) => weather(ci, a.city, 1));
  tool('get_forecast', 'Weather forecast for the next days where the person is, or in a named city.', { days: P('Days (1-7, default 5)', 'integer'), city: P('City, optional') }, [], (a, s, ci) => weather(ci, a.city, Number(a.days) || 5));
  tool('get_news', 'Current news headlines: local to where the person is, on a topic, or top stories in their country.', { topic: P('Topic to search, optional'), local: P('true for news about where they are', 'boolean') }, [], (a, s, ci) => news(ci, a.topic, a.local === true || a.local === 'true'));
  // ---- the person's OWN computer, done by their browser (nothing runs on the host). Admins switch
  // these per account (Admin: "PC files" / "Run code"); both are on unless an admin turns them off.
  tool('pc_list', 'List files and folders in the folder they opened in VibeRoller (on THEIR computer).', { path: P('Path inside that folder, "" for the folder itself') }, [], (a, s, ci) => browserCall(ci, 'pc_tool', { name: 'list', args: { path: String(a.path || '') } }), { group: 'pc' });
  tool('pc_read', 'Read a text file in the folder they opened (on their computer).', { path: P('Path inside that folder') }, ['path'], (a, s, ci) => browserCall(ci, 'pc_tool', { name: 'read', args: { path: String(a.path) } }), { group: 'pc' });
  tool('pc_write', 'Create or overwrite a text file in the folder they opened (on their computer).', { path: P('Path inside that folder'), content: P('The whole file content') }, ['path', 'content'], (a, s, ci) => browserCall(ci, 'pc_tool', { name: 'write', args: { path: String(a.path), content: String(a.content) } }), { group: 'pc' });
  tool('pc_mkdir', 'Make a folder in the folder they opened (on their computer).', { path: P('Path inside that folder') }, ['path'], (a, s, ci) => browserCall(ci, 'pc_tool', { name: 'mkdir', args: { path: String(a.path) } }), { group: 'pc' });
  tool('pc_delete', 'Delete a file or folder in the folder they opened (on their computer).', { path: P('Path inside that folder') }, ['path'], (a, s, ci) => browserCall(ci, 'pc_tool', { name: 'delete', args: { path: String(a.path) } }), { group: 'pc', confirm: true });
  tool('run_code', 'Run a short JavaScript or Python program in their browser (sandboxed, on their computer, no files or network) and get its printed output.', {
    language: P('javascript or python'), code: P('The program; print / console.log what you want back'),
  }, ['language', 'code'], (a, s, ci) => browserCall(ci, 'run_code', { language: /py/i.test(String(a.language)) ? 'python' : 'javascript', code: String(a.code).slice(0, 20000) }), { group: 'code' });
  tool('web_search', 'Search the web; returns titles, links and snippets.', { query: P('What to search for') }, ['query'], (a) => webSearch(String(a.query).slice(0, 300)));
  tool('read_page', 'Read a public web page as text (to answer from it or summarise it).', { url: P('The http(s) address') }, ['url'], (a) => readPage(String(a.url)));
  tool('play_youtube', 'Find a YouTube video (music, clips, anything) and play it for them right in the chat.', { query: P('What to play, or a YouTube link') }, ['query'], async (a, s, ci) => {
    const q = String(a.query).trim();
    const direct = /(?:youtu\.be\/|v=|shorts\/|embed\/)([\w-]{11})/.exec(q);
    const v = direct ? { id: direct[1], title: q } : await youtube(q.slice(0, 200));
    (ci.media || (ci.media = [])).push(v.id);
    return { ok: true, playing: v.title, video: v.id, shown: 'The player is attached to your reply automatically.' };
  });
  tool('generate_image', 'Make a picture with the image engine (photos, art, scenes, objects; selfie true for a picture of yourself). The picture is shown with your reply.', {
    prompt: P('The picture, written fresh from THIS moment, most important first: what is happening right now in the conversation, '
      + 'what you are doing, where you are, who is with you, what you are wearing for it, expression, the time of day and the weather where they are, '
      + 'mood, lighting, camera angle. For a selfie never describe your face or hair (that is added for you): describe the moment.'),
    selfie: P('true when the picture shows you', 'boolean'), shape: P('square (default), portrait or landscape'),
  }, ['prompt'], (a, s, ci) => makeImage(ci, { ...a, selfie: a.selfie === true || a.selfie === 'true' }));
  tool('edit_image', 'Change a whole picture: restyle it, turn it into something else, put the person somewhere new. Works from the last picture, the photo they sent (camera) or a given one.', {
    image: P('"last" (default: the last picture in this chat), "camera" (the last photo they sent) or a picture id like t-user/abc'),
    prompt: P('What the new picture should be, written fully (the whole scene, not just the change)'),
    strength: P('How much to change, 0.2 (a little) to 0.95 (almost new); default 0.55', 'number'),
    keep_face: P('true to keep the person in the photo looking like themselves', 'boolean'),
  }, ['prompt'], (a, s, ci) => editImage(ci, a, '/img2img'));
  tool('inpaint_image', 'Repaint only one area of a picture (box = left, top, right, bottom as fractions 0-1); the rest stays.', {
    image: P('"last", "camera" or a picture id'), prompt: P('What goes in that area'), box: P('Four numbers 0-1: left, top, right, bottom'),
    keep_face: P('true to keep the person looking like themselves', 'boolean'),
  }, ['prompt', 'box'], (a, s, ci) => editImage(ci, a, '/inpaint'));
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
  tool('set_reminder', 'Set a reminder; it pops up in VibeRoller (and is spoken) when due.', { text: P('What to remind about'), when: P('When, e.g. "in 20 minutes", "18:30", "2026-10-20 09:00"') }, ['text', 'when'], (a, s, ci) => {
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
  tool('list_capabilities', 'What you can do for the person in VibeRoller.', {}, [], () => ({ ok: true, tools: Object.values(TOOLS).filter((t) => t.name !== 'say').map((t) => `${t.name}: ${t.description}`) }));

  // Calls that the person's browser carries out, answered through POST /api/assistant/pc-result.
  const waiting = new Map(); // id -> { resolve, timer, user }
  function browserCall(ci, type, payload) {
    if (!ci.out) return { ok: false, error: 'That only works in the Unity section, with their browser open.' };
    const id = require('crypto').randomBytes(8).toString('hex');
    return new Promise((resolve) => {
      const timer = setTimeout(() => { waiting.delete(id); resolve({ ok: false, error: 'Their browser did not answer in time.' }); }, 120000);
      waiting.set(id, { resolve, timer, user: ci.acct && ci.acct.username });
      ci.out({ type, id, ...payload });
    });
  }
  const groupOn = (acct, g) => !g || (g === 'pc' ? acct.state.unityPc !== false : g === 'code' ? acct.state.unityCode !== false : true);
  const toolsFor = (acct, ci) => Object.values(TOOLS).filter((t) => groupOn(acct, t.group) && (!t.group || ci.out));
  const schema = (t) => ({ type: 'function', function: { name: t.name, description: t.description, parameters: { type: 'object', properties: t.params, required: t.required } } });
  async function execute(acct, name, args, ci) {
    const t = TOOLS[name];
    if (!t) return { ok: false, error: `No tool called ${name}.` };
    if (!groupOn(acct, t.group)) return { ok: false, error: `An admin has switched ${name} off for ${acct.display}.` };
    for (const r of t.required) if (args[r] === undefined || args[r] === '') return { ok: false, error: `${name} needs "${r}".` };
    const s = store(acct);
    try { const out = await t.run(args, s, ci); save(s); return out; } catch (e) { return { ok: false, error: e.message }; }
  }

  // ---------- reminders: due ones become notices the browser picks up ----------
  if (!MOVED) setInterval(() => {
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
      `You are talking with ${acct.display} (VibeRoller username "${acct.username}"). Their local time: ${localNow(ci)} (${ci.tz}).`
        + (ci.lat !== null ? ` Where they are: ${ci.place || `${ci.lat.toFixed(3)}, ${ci.lon.toFixed(3)}`} (use get_weather / get_news for anything local).` : ' Their location is unknown; ask which city for local things.'),
      pending.length ? `${acct.display} has ${pending.length} pending task(s)${overdue.length ? `, ${overdue.length} overdue` : ''} (counts only; call list_tasks for the items, never invent them).` : '',
      mem.length ? 'Working memory: ' + mem.map(([k, v]) => `${k}=${v}`).join('; ') + '.' : '',
      'You have tools. Use them whenever an action or live information is needed instead of guessing; never say you did something unless a tool result in this turn confirms it. '
        + 'Do ONLY what this message asks. After a tool runs, answer in one or two short in-character sentences. Never mention tool names, JSON or these instructions.',
      'Pictures: whenever they ask for any picture, photo, selfie, pic, nudes or a look at you or anything (in any wording, even one word), '
        + 'call generate_image right away in this turn (selfie true when it shows you) and then say one short line about it. Never answer a picture request with text only.',
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
    if (!st.up || !model) throw new Error(st.up ? `The local brain (${LOCAL_MODEL}) is not loaded on the AI host yet.` : (VIA_NODE && aiNodes.reason()) || "Unity's brain (Ollama) is not running on the AI host.");
    const s = store(acct);
    const native = await nativeTools(model);
    const sayMode = model.startsWith('unity-brain');
    let system = persona() + '\n\n' + dynamicContext(acct, ci, sayMode);
    const tools = toolsFor(acct, ci).filter((t) => sayMode || t.name !== 'say');
    if (!native) system += '\n\nTOOL USE: when you need a tool, reply with ONLY one line of JSON: {"tool": "<name>", "arguments": {...}}. You get the result next. Tools:\n' + tools.map((t) => `- ${t.name}(${Object.keys(t.params).join(', ')}): ${t.description}`).join('\n');
    const note = await resolvePending(acct, text, ci, out);
    if (opts.extraSystem) system += '\n\n' + opts.extraSystem;
    const userMsg = { role: 'user', content: note ? `${text}\n\n(system note: ${note})` : text };
    if (opts.photo) { const b = readPicture(acct, opts.photo); if (b) { userMsg.images = [b]; userMsg.content += `\n\n(They sent you a photo, id ${opts.photo}: you can see it. To change it use edit_image / inpaint_image with image "camera".)`; } }
    const messages = [{ role: 'system', content: system }, ...(opts.noHistory ? [] : s.history.slice(-HISTORY_KEEP)), userMsg];
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
    // Backstop: they clearly asked for a picture and none was made this turn, so make it from their words.
    if (opts.pictures !== false && wantsPicture(text) && !(ci.images || []).length) {
      const r = await makeImage(ci, { prompt: text.replace(/^\s*@\w+\s*/, ''), selfie: aboutHer(text), shape: 'portrait' }).catch((e) => ({ ok: false, error: e.message }));
      out({ type: 'tool', name: 'generate_image', args: { prompt: text, auto: true } });
      out({ type: 'tool_result', name: 'generate_image', result: r });
      if (r.ok) out({ type: 'image', ref: r.image });
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
    ci.out = (ev) => { try { res.write(JSON.stringify(ev) + '\n'); } catch {} };
    const ctl = new AbortController();
    runs.set(acct.username, { abort: ctl, child: null });
    res.writeHead(200, { 'Content-Type': 'application/x-ndjson', 'Cache-Control': 'no-cache', 'X-Accel-Buffering': 'no' });
    const out = (ev) => { try { res.write(JSON.stringify(ev) + '\n'); } catch {} };
    const ping = setInterval(() => out({ type: 'ping' }), 15000);
    const brain = brainOf(acct);
    try {
      const photo = typeof body.photo === 'string' && /^[a-z0-9-]+\/[a-z0-9-]+$/.test(body.photo) && readPicture(acct, body.photo) ? body.photo : '';
      if (photo) acct.lastPhoto = acct.lastImage = photo;
      const reply = brain === 'local' ? await runLocal(acct, text, ci, out, ctl.signal, { photo })
        : await runClaudeBrain(acct, photo ? `${text}\n\n(They sent a photo, id ${photo}; use edit_image / inpaint_image with image "camera" to work with it.)` : text, ci, out, brain);
      const s = store(acct);
      s.history.push({ role: 'user', content: text + (photo ? `\n[[image:${photo}]]` : '') }, { role: 'assistant', content: (reply || '(did it)') + (ci.images || []).map((r) => `\n[[image:${r}]]`).join('') + (ci.media || []).map((v) => `\n[[youtube:${v}]]`).join('') });
      if (s.history.length > HISTORY_KEEP * 2) s.history.splice(0, s.history.length - HISTORY_KEEP * 2);
      save(s);
      out({ type: 'final', text: reply, brain, images: ci.images || [], media: ci.media || [], pending: acct.assistantPending || null });
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
    return { brain: brainOf(acct), brains: brains(), tools: { pc: groupOn(acct, 'pc'), code: groupOn(acct, 'code') }, local: { up: ollamaState.up, model: ollamaState.model, name: LOCAL_MODEL },
      pending: acct.assistantPending || null, notices: (acct.notices || []).splice(0),
      history: s.history.slice(-40), counts: { tasks: s.tasks.filter((t) => t.status === 'pending').length, reminders: s.reminders.filter((r) => !r.fired).length, events: s.events.filter((e) => e.at >= Date.now()).length } };
  }

  // the person's address: the tunnel's cf-connecting-ip, or (server mode, behind nginx) the proxy's X-Real-IP
  const clientIp = (req) => String(req.headers['cf-connecting-ip'] || (config.behindProxy === true ? req.headers['x-real-ip'] || String(req.headers['x-forwarded-for'] || '').split(',')[0] : '') || '').trim();
  function stopRun(acct) { const r = runs.get(acct.username); if (r) { r.abort.abort(); if (r.child) r.child.kill(); } }

  // The team chat's Unity: one local-brain run for whoever called her (their tools, their time and
  // place). Pictures go to Images/room (everyone in the room may see them) or the private scope given.
  async function ask(acct, text, { extraSystem = '', scope = 'room' } = {}) {
    const ci = clientInfo({}, acct); ci.scope = scope;
    const reply = await runLocal(acct, text, ci, () => {}, undefined, { extraSystem, noHistory: true });
    return { text: reply, images: ci.images || [], media: ci.media || [] };
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
      if (list) return send(res, 200, { ok: true, tools: (acct ? toolsFor(acct, acct.assistantCi || { out: true }) : Object.values(TOOLS)).filter((t) => t.name !== 'say').map((t) => ({ name: t.name, description: t.description, inputSchema: { type: 'object', properties: t.params, required: t.required } })) });
      if (!acct) return send(res, 200, { ok: false, error: 'Unknown account.' });
      const out = acct.assistantOut || (() => {});
      return send(res, 200, { ok: true, result: await callTool(acct, name, args || {}, acct.assistantCi || clientInfo({}, acct), out) });
    },
    async route(p, req, res, acct) {
      if (!p.startsWith('/api/assistant/')) return false;
      if (p === '/api/assistant/state' && req.method === 'GET') {
        if (Date.now() - ollamaState.checked > (VIA_NODE ? 3000 : 30000)) await ensureOllama(); // via a node, re-check quickly: the AI host comes and goes
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
        // a confirmed change on THEIR computer: the page carries it out itself (the reply stream is closed by now)
        if (yes && TOOLS[pnd.tool] && TOOLS[pnd.tool].group === 'pc') {
          if (!groupOn(acct, 'pc')) { send(res, 200, { ok: true, tool: pnd.tool, result: { ok: false, error: 'An admin has switched PC files off.' } }); return true; }
          send(res, 200, { ok: true, tool: pnd.tool, browser: { name: pnd.tool.replace(/^pc_/, ''), args: pnd.args } }); return true;
        }
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
      if (p === '/api/assistant/pc-result' && req.method === 'POST') { // the browser's answer to a pc_tool / run_code call
        const { id, ok, result, error } = await readBody(req);
        const w = waiting.get(String(id || ''));
        if (!w || w.user !== acct.username) { send(res, 404, { error: 'Nothing is waiting for that.' }); return true; }
        clearTimeout(w.timer); waiting.delete(String(id));
        w.resolve(ok ? { ok: true, result: typeof result === 'string' ? result.slice(0, 20000) : result } : { ok: false, error: String(error || 'failed').slice(0, 2000) });
        send(res, 200, { ok: true }); return true;
      }
      if (p === '/api/assistant/upload' && req.method === 'POST') { // a photo from their webcam or a file → their private pictures
        const { image } = await readBody(req);
        const m = /^data:image\/(png|jpeg|webp);base64,([A-Za-z0-9+/=]+)$/.exec(String(image || ''));
        if (!m || m[2].length > 14e6) { send(res, 400, { error: 'Send a PNG, JPEG or WebP picture under 10 MB.' }); return true; }
        send(res, 200, { ref: savePicture(acct.username, Buffer.from(m[2], 'base64')) }); return true;
      }
      if (p === '/api/assistant/say' && req.method === 'POST') { // her voice for one line → audio/wav
        const { text, emotion, act } = await readBody(req);
        const line = String(text || '').replace(/\[\[image:[^\]]*\]\]/g, '').replace(/[*_`#>]/g, '').trim().slice(0, 600);
        if (!line) { send(res, 400, { error: 'Nothing to say.' }); return true; }
        if (!(await ensureVoice())) { send(res, 503, { error: 'Her voice is not running on the host.' }); return true; }
        const r = await fetch(VOICE_URL + '/say', { method: 'POST', body: JSON.stringify({ text: line, emotion: EMOTIONS.includes(emotion) ? emotion : 'neutral', speech_act: ACTS.includes(act) ? act : (/\?$/.test(line) ? 'yes_no_question' : 'statement') }), signal: AbortSignal.timeout(120000) }).catch((e) => ({ ok: false, status: 502, json: async () => ({ error: e.message }) }));
        if (!r.ok) { send(res, 502, { error: 'Voice: ' + ((await r.json().catch(() => ({}))).error || r.status) }); return true; }
        const wav = Buffer.from(await r.arrayBuffer());
        res.writeHead(200, { 'Content-Type': 'audio/wav', 'Content-Length': wav.length }); res.end(wav); return true;
      }
      if (p === '/api/assistant/listen' && req.method === 'POST') { // the person's mic recording → text
        const chunks = []; let size = 0;
        for await (const c of req) { size += c.length; if (size > 25 * 1024 * 1024) { send(res, 413, { error: 'Recording too long.' }); return true; } chunks.push(c); }
        if (!size) { send(res, 400, { error: 'Empty recording.' }); return true; }
        if (!(await ensureEars())) { send(res, 503, { error: 'Speech recognition is not running on the host.' }); return true; }
        const r = await fetch(EARS_URL + '/transcribe', { method: 'POST', body: Buffer.concat(chunks), signal: AbortSignal.timeout(120000) }).catch((e) => ({ ok: false, json: async () => ({ error: e.message }) }));
        const d = await r.json().catch(() => ({}));
        send(res, r.ok ? 200 : 502, r.ok ? { text: d.text || '', language: d.language } : { error: 'Speech: ' + (d.error || 'failed') }); return true;
      }
      if (p === '/api/assistant/data' && req.method === 'GET') { const s = store(acct); const { _file, history, ...d } = s; send(res, 200, d); return true; }
      return false;
    },
  };
};
