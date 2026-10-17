// Mills Chat as an app: the same page in "app mode" (index.html?app=1), installed as a Progressive Web
// App from Chrome or Edge on Windows and Linux, so it runs in its own window with its own icon and a
// Start-menu / launcher entry. App mode shows only the chat side of Mills (servers, channels, groups,
// private messages, calls, Unity as a bot); Work, Board, Calendar and Admin stay on the site.
// Loaded after the page script; uses its helpers at call time.
const APP_MODE = new URLSearchParams(location.search).get('app') === '1' || (window.matchMedia && window.matchMedia('(display-mode: standalone)').matches && new URLSearchParams(location.search).has('app'));
if (APP_MODE) document.body.classList.add('appMode');
let installPrompt = null; // Chrome / Edge hand us the install prompt once the page qualifies
window.addEventListener('beforeinstallprompt', (e) => { e.preventDefault(); installPrompt = e; const b = $('appInstall'); if (b) b.hidden = false; });
window.addEventListener('appinstalled', () => { installPrompt = null; const b = $('appInstall'); if (b) b.hidden = true; });
// The service worker makes the install possible and keeps the app shell opening even when the site is
// slow; it never caches hub answers (everything under /api, /v1 and the hub's own address goes to the network).
if ('serviceWorker' in navigator && (location.protocol === 'https:' || location.hostname === 'localhost' || location.hostname === '127.0.0.1')) {
  navigator.serviceWorker.register('sw.js').catch(() => {});
}
// Which operating system this browser runs on, for the Settings download / install card (Windows and
// Linux are the two the team uses; the app itself is the same on both).
function appOs() {
  const p = String((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '').toLowerCase(), ua = navigator.userAgent.toLowerCase();
  if (p.includes('win') || ua.includes('windows')) return 'Windows';
  if (p.includes('linux') || ua.includes('linux') || ua.includes('x11')) return ua.includes('android') ? 'Android' : 'Linux';
  if (p.includes('mac') || ua.includes('mac os')) return 'macOS';
  return 'your computer';
}
const appUrl = () => location.origin + location.pathname.replace(/[^/]*$/, '') + 'index.html?app=1';
function renderAppCard() { // Settings → "Mills Chat app"
  const box = $('appBox'); if (!box) return;
  box.textContent = '';
  const os = appOs(), chromium = /chrome|edg|chromium|brave/i.test(navigator.userAgent) && !/firefox/i.test(navigator.userAgent);
  const line = (...kids) => { const d = document.createElement('div'); d.className = 'row'; d.append(...kids); box.append(d); return d; };
  const p = document.createElement('div');
  p.textContent = `Detected: ${os}. Mills Chat installs as its own app (own window, own icon, launcher entry) on Windows and Linux from Chrome, Edge or Brave; it is the chat side of Mills only (servers, channels, groups, private messages, calls, Unity), always signed in with your Mills account.`;
  box.append(p);
  const inst = document.createElement('button'); inst.type = 'button'; inst.id = 'appInstall'; inst.textContent = 'Install Mills Chat for ' + os; inst.hidden = !installPrompt;
  inst.onclick = async () => { if (!installPrompt) return; const ev = installPrompt; installPrompt = null; try { await ev.prompt(); } catch {} renderAppCard(); };
  const open = document.createElement('button'); open.type = 'button'; open.className = 'ghost'; open.textContent = 'Open Mills Chat (app view)';
  open.onclick = () => { window.open(appUrl(), '_blank', 'noopener'); };
  const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'ghost'; copy.textContent = 'Copy app link';
  copy.onclick = async () => { try { await navigator.clipboard.writeText(appUrl()); copy.textContent = 'Copied'; } catch { copy.textContent = appUrl(); } };
  line(inst, open, copy);
  const how = document.createElement('div'); how.className = 'status';
  how.textContent = installPrompt ? 'Your browser is ready to install it: press the button.'
    : chromium ? `To install: open the app view, then use the install icon at the right of the address bar (or the browser menu → "Install Mills Chat"). ${os === 'Linux' ? 'On Linux it appears in your application menu; ' : os === 'Windows' ? 'On Windows it appears in the Start menu and can be pinned to the taskbar; ' : ''}uninstall from the app's own menu.`
    : 'Installing needs Chrome, Edge or Brave (Firefox can open the app view but cannot install it as an app).';
  box.append(how);
}
if (APP_MODE) { // app mode: the chat is the whole app
  document.title = 'Mills Chat';
  const back = () => { for (const b of document.querySelectorAll('[data-back]')) b.hidden = true; };
  back();
}
