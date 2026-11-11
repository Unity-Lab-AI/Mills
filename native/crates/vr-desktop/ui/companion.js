// Unity on the desktop: she stands on the bottom of the screen and does what she is told (the hub's page passes on
// her orders: walk left / right / up / down, sit, wave, dance, come here, stop). Her moves are short see-through clips
// (companion/*.webp); walking plays the walk clip, mirrored for the way she faces, while she moves across the screen.
'use strict';
const T = window.__TAURI__;
const her = document.getElementById('her');
const CLIPS = { idle: 'companion/idle.webp', walk: 'companion/walk.webp', sit: 'companion/sit.webp', wave: 'companion/wave.webp', dance: 'companion/dance.webp' };
const SPEED = 140;          // px a second when she walks
const ONCE_MS = 4200;       // a wave / dance plays this long, then she goes back to standing
const WANDER_MS = 45000;    // left alone this long, she strolls a little on her own
let x = innerWidth * 0.8, y = 0, facing = -1, clip = '', moving = null, until = 0, lastOrder = Date.now(), sitting = false;

function play(name) {
  if (clip === name) return;
  clip = name;
  her.src = (CLIPS[name] || CLIPS.idle) + (name === 'sit' && !String(CLIPS.sit).startsWith('data:') ? '?' + Date.now() : ''); // sit plays once: a fresh load replays it
}
function draw() {
  const w = her.offsetWidth || 160;
  x = Math.max(w / 2, Math.min(innerWidth - w / 2, x));
  y = Math.max(0, Math.min(innerHeight - her.offsetHeight, y));
  her.style.left = (x - w / 2) + 'px';
  her.style.bottom = y + 'px';
  her.style.transform = facing < 0 ? 'scaleX(-1)' : 'none'; // the walk clip faces right
}
function walkTo(tx, ty) {
  sitting = false;
  moving = { tx: Math.max(0, Math.min(innerWidth, tx)), ty: Math.max(0, Math.min(innerHeight, ty)) };
  if (Math.abs(moving.tx - x) > 2) facing = moving.tx > x ? 1 : -1;
  play('walk');
}
let last = performance.now();
function tick(now) {
  const dt = Math.min(0.1, (now - last) / 1000); last = now;
  if (moving) {
    const dx = moving.tx - x, dy = moving.ty - y, d = Math.hypot(dx, dy), step = SPEED * dt;
    if (d <= step) { x = moving.tx; y = moving.ty; moving = null; play('idle'); }
    else { x += dx / d * step; y += dy / d * step; }
  } else if (until && Date.now() > until) { until = 0; play('idle'); }
  else if (!sitting && !until && Date.now() - lastOrder > WANDER_MS) { // a little stroll on her own
    lastOrder = Date.now();
    walkTo(x + (Math.random() < 0.5 ? -1 : 1) * (120 + Math.random() * 260), y);
  }
  draw();
  requestAnimationFrame(tick);
}

function order(o) {
  const a = String((o && o.action) || '').toLowerCase(), n = Number(o && o.amount) || 0;
  lastOrder = Date.now();
  until = 0;
  switch (a) {
    case 'walk_left': case 'left': walkTo(x - (n || 320), y); break;
    case 'walk_right': case 'right': walkTo(x + (n || 320), y); break;
    case 'up': case 'walk_up': walkTo(x, y + (n || 140)); break;
    case 'down': case 'walk_down': walkTo(x, y - (n || 140)); break;
    case 'come_here': case 'center': walkTo(innerWidth / 2, 0); break;
    case 'go_left_edge': walkTo(0, y); break;
    case 'go_right_edge': walkTo(innerWidth, y); break;
    case 'sit': moving = null; sitting = true; play('sit'); break;
    case 'wave': case 'dance': moving = null; sitting = false; play(a); until = Date.now() + ONCE_MS; break;
    case 'turn': facing = -facing; break;
    case 'clip': if (o.url) { moving = null; sitting = false; clip = 'custom'; her.src = o.url; until = Date.now() + 2200; } break;
    case 'stop': case 'idle': case 'stand': case 'show': default: moving = null; sitting = false; play('idle');
  }
}

draw();
requestAnimationFrame(tick);
addEventListener('resize', draw);
const note = (m) => { try { T.core.invoke('companion_note', { m: String(m) }); } catch {} };
addEventListener('error', (e) => note('error: ' + e.message));
her.addEventListener('error', () => note('clip failed to load: ' + String(her.src).slice(0, 60)));
async function loadClips() {
  try { const c = await T.core.invoke('companion_clips'); const n = Object.keys(c || {}); for (const [k, v] of Object.entries(c || {})) CLIPS[k] = v; note('own clips: ' + (n.join(',') || 'none, the built-in look')); const was = clip; clip = ''; play(was || 'idle'); } catch (e) { note('clips: ' + e); }
}
if (T) {
  loadClips();
  T.event.listen('companion-clips', () => loadClips());
  T.event.listen('companion', (e) => order(e.payload));
  T.core.invoke('companion_last').then((o) => { if (o && o.action) order(o); }).catch(() => {});
}
