// Mills Chat service worker: lets the page install as an app and keeps the app shell (this site's own
// files) opening even on a slow connection. Network first, cache as the fallback, and NEVER anything
// that is not this site's own static file: hub answers (/api, /v1, the tunnel address) always go to the
// network untouched, so nothing stale or private is ever served from a cache.
const SHELL = 'mills-shell-v2';
const OWN = ['index.html', 'polish.css', 'md.js', 'board.js', 'calls.js', 'guilds.js', 'guild-settings.js', 'profile.js', 'notes.js', 'app.js', 'manifest.webmanifest', 'icons/mills.svg', 'icons/mills-maskable.svg'];
self.addEventListener('install', (e) => { e.waitUntil(caches.open(SHELL).then((c) => c.addAll(OWN.map((f) => new URL(f, self.registration.scope).href)).catch(() => {}))); self.skipWaiting(); });
self.addEventListener('activate', (e) => { e.waitUntil(caches.keys().then((ks) => Promise.all(ks.filter((k) => k !== SHELL).map((k) => caches.delete(k)))).then(() => self.clients.claim())); });
self.addEventListener('fetch', (e) => {
  const url = new URL(e.request.url);
  const own = url.origin === self.location.origin && e.request.method === 'GET' && !url.pathname.includes('/api/') && !url.pathname.includes('/v1/');
  if (!own) return; // the hub, the tunnel, GitHub's API, YouTube: straight to the network
  e.respondWith(fetch(e.request).then((r) => { if (r.ok) { const copy = r.clone(); caches.open(SHELL).then((c) => c.put(e.request, copy)).catch(() => {}); } return r; })
    .catch(() => caches.match(e.request, { ignoreSearch: url.pathname.endsWith('index.html') || url.pathname.endsWith('/') }).then((hit) => hit || new Response('Mills Chat is offline and this file is not cached yet.', { status: 503, headers: { 'Content-Type': 'text/plain' } }))));
});
