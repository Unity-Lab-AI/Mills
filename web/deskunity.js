// Desktop Unity on the website: the same Unity as in the desktop app, standing on the bottom of the page head to toe,
// doing what she is told (the team chat's @unity orders and Settings → App). A see-through layer that never takes a
// click. A new look is filmed by the image engine on the AI host (the hub asks it); until one is ready she wears the
// look built into the site.
'use strict';
(() => {
  const KEY = 'vrDeskUnityOn';
  const CLIPS = { idle: 'companion/idle.webp', walk: 'companion/walk.webp', sit: 'companion/sit.webp', wave: 'companion/wave.webp', dance: 'companion/dance.webp' };
  const SPEED = 140, ONCE_MS = 4200, WANDER_MS = 45000;
  let layer = null, her = null, x = 0, y = 0, facing = -1, clip = '', moving = null, until = 0, lastOrder = Date.now(), sitting = false, last = 0, running = false;
  const W = () => (layer ? layer.clientWidth : innerWidth), H = () => (layer ? layer.clientHeight : innerHeight);
  const isOn = () => localStorage.getItem(KEY) !== 'false';
  const PLACE = 'vrDeskUnityPlace';
  const keep = () => { try { localStorage.setItem(PLACE, JSON.stringify({ fx: x / Math.max(1, W()), fy: y / Math.max(1, H()), facing })); } catch {} };
  const BUILTIN = { ...CLIPS };
  let own = 0, poll = 0, info = { busy: false, step: '' };
  const hub = (p, o) => (typeof window.api === 'function' ? window.api(p, o) : fetch(p, o));
  // her own look (filmed on the AI host): its clips replace the built-in ones
  async function loadOwn() {
    try {
      const st = await (await hub('/api/assistant/look')).json();
      info = { busy: !!st.busy, step: (st.status && st.status.step) || '', look: (st.status && st.status.look) || '' };
      if (st.busy) watch();
      if (!st.clips || st.clips.length < 5) { if (own) { for (const k of Object.keys(CLIPS)) { if (CLIPS[k] !== BUILTIN[k]) URL.revokeObjectURL(CLIPS[k]); CLIPS[k] = BUILTIN[k]; } own = 0; if (her) { const was = clip; clip = ''; play(was || 'idle'); } } return; }
      if (st.stamp === own) return;
      const got = {};
      for (const n of st.clips) { const r = await hub('/api/assistant/look-clip?name=' + n + '&t=' + st.stamp); if (!r.ok) return; got[n] = URL.createObjectURL(await r.blob()); }
      for (const [k, v] of Object.entries(got)) { if (CLIPS[k] !== BUILTIN[k]) URL.revokeObjectURL(CLIPS[k]); CLIPS[k] = v; }
      own = st.stamp;
      if (her) { const was = clip; clip = ''; play(was || 'idle'); }
    } catch {}
  }
  function watch() { if (poll) return; poll = setInterval(async () => { await loadOwn(); if (!info.busy) { clearInterval(poll); poll = 0; } }, 10000); }
  async function newLook(look) {
    const r = await hub('/api/assistant/look', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ look }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || 'failed');
    info = { busy: true, step: 'starting', look }; watch(); return true;
  }

  function build() {
    if (layer) return;
    layer = document.createElement('div');
    layer.id = 'deskUnity';
    layer.setAttribute('aria-hidden', 'true');
    layer.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483000;overflow:hidden';
    her = document.createElement('img');
    her.alt = '';
    her.style.cssText = 'position:absolute;bottom:0;height:30vh;max-height:340px;min-height:180px;transform-origin:50% 100%;will-change:left,bottom,transform;filter:drop-shadow(0 6px 10px rgba(0,0,0,.35));user-select:none;pointer-events:none';
    layer.appendChild(her);
    document.body.appendChild(layer);
    // where she stands is kept from one visit to the next (fractions of the page, so a resize keeps her place)
    let p = {}; try { p = JSON.parse(localStorage.getItem(PLACE) || '{}'); } catch {}
    x = W() * (Number.isFinite(p.fx) ? p.fx : 0.8); y = H() * (Number.isFinite(p.fy) ? p.fy : 0); facing = p.facing === 1 ? 1 : -1;
    play('idle');
    loadOwn();
  }
  function play(name) {
    if (clip === name) return;
    clip = name;
    her.src = (CLIPS[name] || CLIPS.idle) + (name === 'sit' ? '?' + Date.now() : ''); // sit plays once: a fresh load replays it
  }
  function draw() {
    if (!her) return;
    const w = her.offsetWidth || 160;
    x = Math.max(w / 2, Math.min(W() - w / 2, x));
    y = Math.max(0, Math.min(H() - her.offsetHeight, y));
    her.style.left = (x - w / 2) + 'px';
    her.style.bottom = y + 'px';
    her.style.transform = facing < 0 ? 'scaleX(-1)' : 'none'; // the walk clip faces right
  }
  function walkTo(tx, ty) {
    sitting = false;
    moving = { tx: Math.max(0, Math.min(W(), tx)), ty: Math.max(0, Math.min(H(), ty)) };
    if (Math.abs(moving.tx - x) > 2) facing = moving.tx > x ? 1 : -1;
    play('walk');
  }
  function tick(now) {
    if (!layer || layer.hidden) { running = false; return; }
    const dt = Math.min(0.1, (now - (last || now)) / 1000); last = now;
    if (moving) {
      const dx = moving.tx - x, dy = moving.ty - y, d = Math.hypot(dx, dy), step = SPEED * dt;
      if (d <= step) { x = moving.tx; y = moving.ty; moving = null; play('idle'); keep(); } else { x += dx / d * step; y += dy / d * step; }
    } else if (until && Date.now() > until) { until = 0; play('idle'); }
    else if (!sitting && !until && Date.now() - lastOrder > WANDER_MS) { lastOrder = Date.now(); walkTo(x + (Math.random() < 0.5 ? -1 : 1) * (120 + Math.random() * 260), y); }
    draw();
    requestAnimationFrame(tick);
  }
  function start() { if (running) return; running = true; last = 0; requestAnimationFrame(tick); }

  // a few words that are a walk or a move she has ("walk left", "come here", "sit down")
  function quickMove(t) {
    t = String(t || '').toLowerCase(); const words = t.split(/\s+/).filter(Boolean).length;
    if (!words || words > 5) return null;
    const has = (ks) => ks.some((k) => new RegExp('\\b' + k + '\\b').test(t));
    const walkish = has(['walk', 'go', 'move', 'run', 'head', 'step', 'scoot']) || words <= 2;
    if (walkish && has(['left'])) return 'walk_left'; if (walkish && has(['right'])) return 'walk_right';
    if (walkish && has(['up'])) return 'up'; if (walkish && has(['down']) && !has(['sit'])) return 'down';
    if (has(['here', 'middle', 'center', 'centre'])) return 'come_here'; if (has(['sit'])) return 'sit';
    if (has(['wave'])) return 'wave'; if (has(['dance'])) return 'dance'; if (has(['turn'])) return 'turn';
    if (has(['stop', 'stand', 'idle'])) return 'stop'; if (has(['hide'])) return 'hide';
    return null;
  }

  // an order: {action, amount?, look?} (the same orders the desktop app takes)
  function order(o) {
    let a = String((o && o.action) || '').toLowerCase(); const n = Number(o && o.amount) || 0;
    if (a === 'show') { setOn(true); return true; }
    if (a === 'hide') { setOn(false); return true; }
    if (a === 'do') { a = quickMove(o.look) || ''; if (!a) return false; if (a === 'hide') { setOn(false); return true; } }
    if (a === 'outfit') { if (o.look) newLook(String(o.look)).catch(() => {}); return true; }
    if (!isOn()) return false;
    build(); layer.hidden = false; start();
    lastOrder = Date.now(); until = 0;
    switch (a) {
      case 'walk_left': case 'left': walkTo(x - (n || 320), y); break;
      case 'walk_right': case 'right': walkTo(x + (n || 320), y); break;
      case 'up': case 'walk_up': walkTo(x, y + (n || 140)); break;
      case 'down': case 'walk_down': walkTo(x, y - (n || 140)); break;
      case 'come_here': case 'center': walkTo(W() / 2, 0); break;
      case 'go_left_edge': walkTo(0, y); break;
      case 'go_right_edge': walkTo(W(), y); break;
      case 'sit': moving = null; sitting = true; play('sit'); break;
      case 'wave': case 'dance': moving = null; sitting = false; play(a); until = Date.now() + ONCE_MS; break;
      case 'turn': facing = -facing; keep(); break;
      default: moving = null; sitting = false; play('idle');
    }
    return true;
  }
  function setOn(on) {
    localStorage.setItem(KEY, on ? 'true' : 'false');
    if (on) { build(); layer.hidden = false; start(); } else if (layer) layer.hidden = true;
  }
  addEventListener('resize', draw);
  // Reset: the built-in her at once, in the middle, while a fresh one of herself is filmed
  async function reset() {
    for (const k of Object.keys(CLIPS)) { if (CLIPS[k] !== BUILTIN[k]) URL.revokeObjectURL(CLIPS[k]); CLIPS[k] = BUILTIN[k]; }
    own = 0; moving = null; sitting = false; x = W() / 2; y = 0; facing = -1; keep();
    if (her) { clip = ''; play('idle'); }
    const r = await hub('/api/assistant/look', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ reset: true }) });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || 'failed');
    info = { busy: true, step: 'starting', look: 'herself' }; watch(); return true;
  }
  window.webUnity = { reset, order, setOn, isOn, quickMove, newLook, status: () => info, refresh: loadOwn };
})();
