// Desktop Unity on the website: the same Unity as in the desktop app, standing on the bottom of the page head to toe,
// doing what she is told (the team chat's @unity orders and Settings → App). A see-through layer that never takes a
// click. Her looks are filmed on a PC's GPU by the desktop app; here she wears the look built into the site.
'use strict';
(() => {
  const KEY = 'vrDeskUnityOn';
  const CLIPS = { idle: 'companion/idle.webp', walk: 'companion/walk.webp', sit: 'companion/sit.webp', wave: 'companion/wave.webp', dance: 'companion/dance.webp' };
  const SPEED = 140, ONCE_MS = 4200, WANDER_MS = 45000;
  let layer = null, her = null, x = 0, y = 0, facing = -1, clip = '', moving = null, until = 0, lastOrder = Date.now(), sitting = false, last = 0, running = false;
  const W = () => (layer ? layer.clientWidth : innerWidth), H = () => (layer ? layer.clientHeight : innerHeight);
  const isOn = () => localStorage.getItem(KEY) !== 'false';

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
    x = W() * 0.8;
    play('idle');
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
      if (d <= step) { x = moving.tx; y = moving.ty; moving = null; play('idle'); } else { x += dx / d * step; y += dy / d * step; }
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
    if (a === 'outfit') return false; // new looks are filmed by the desktop app on a PC's GPU
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
      case 'turn': facing = -facing; break;
      default: moving = null; sitting = false; play('idle');
    }
    return true;
  }
  function setOn(on) {
    localStorage.setItem(KEY, on ? 'true' : 'false');
    if (on) { build(); layer.hidden = false; start(); } else if (layer) layer.hidden = true;
  }
  addEventListener('resize', draw);
  window.webUnity = { order, setOn, isOn, quickMove };
})();
