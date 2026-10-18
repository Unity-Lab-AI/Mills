// VibeRoller profiles, page side (hub side: bridge/user-profile.js; routes /api/profile in bridge/server.js).
// Loaded after the page script and web/guilds.js; uses the page's helpers at call time ($, json, me, nickOf,
// colorOf, PALETTE, lightTheme, roomMembers, selectConv, dmConv, currentConv, pickedColor, refreshMembers, GUILDS).
//
// Settings is split in three tabs: Profile (what others see: nickname, colour, bio, pronouns, status, location,
// links, banner colour, local time; saved with "Save profile"), App settings (how VibeRoller behaves for you, saved with
// "Save app settings", private) and Connections (API key, AIs, Forgejo, the Chat app).
// A profile card opens when you click a name (in messages and the member column): banner in the person's accent
// (by default their own colour), name in their colour (inside a server: their highest coloured role), nickname
// there, pronouns, status, bio, location, links, their local time, their roles in this server, and Message.
const PROFILE = (() => {
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const memberOf = (u) => roomMembers.find((m) => m.username === u) || null;
  const profOf = (u) => ((memberOf(u) || {}).profile) || {};
  const accentOf = (u, prof) => { const p = prof || profOf(u); return Number.isInteger(p.accent) && PALETTE[p.accent] ? PALETTE[p.accent][lightTheme ? 1 : 0] : colorOf(u); };
  const localTime = (tz) => { try { return new Date().toLocaleTimeString([], { timeZone: tz, hour: '2-digit', minute: '2-digit' }); } catch { return ''; } };
  const linkify = (box, text) => { // plain text with https links clickable; never HTML
    for (const part of String(text).split(/(https:\/\/[^\s]+)/g)) {
      if (/^https:\/\//.test(part)) { const a = el('a', '', part); a.href = part; a.target = '_blank'; a.rel = 'noopener noreferrer'; box.append(a); } else box.append(part);
    }
  };
  // ── the card itself (used for the popup and the live preview in Settings) ──
  function cardFor(u, prof, opts = {}) {
    const p = prof || profOf(u), m = memberOf(u) || { username: u, display: u };
    const card = el('div', 'pCard');
    const banner = el('div', 'pBanner'); banner.style.background = accentOf(u, p); card.append(banner);
    const av = el('div', 'pAvatar', (opts.nick || nickOf(u) || u).slice(0, 2).toUpperCase()); av.style.background = colorOf(u); // initials in their colour, or their picture
    const pic = opts.avatar !== undefined ? opts.avatar : p.avatar;
    if (pic) { av.textContent = ''; const img = el('img'); img.src = pic; img.alt = ''; av.append(img); }
    card.append(av);
    const body = el('div', 'pBody');
    const inServer = !opts.preview && GUILDS.colorIn(currentConv, u);
    const name = el('b', 'pName', (!opts.preview && GUILDS.nickIn(currentConv, u)) || opts.nick || nickOf(u)); name.style.color = inServer || (opts.color != null ? PALETTE[opts.color][lightTheme ? 1 : 0] : colorOf(u));
    body.append(name);
    const sub = el('div', 'pSub', '@' + u + (p.pronouns ? ' · ' + p.pronouns : '') + (m.agent ? ' · BOT' : '')); body.append(sub);
    if (p.status) body.append(el('div', 'pStatus', p.status));
    if (p.bio) { const b = el('div', 'pBio'); linkify(b, p.bio); body.append(el('div', 'pHead', 'About'), b); }
    const fact = (icon, t) => { const d = el('div', 'pFacts'); d.append(ic(icon, 'lead'), t); body.append(d); };
    if (p.location) fact('globe', p.location);
    if (p.timezone) fact('clock', localTime(p.timezone) + ' local time');
    if ((p.links || []).length) { const l = el('div', 'pLinks'); for (const x of p.links) { const a = el('a', ''); a.append(ic('link', 'lead'), x.replace(/^https:\/\//, '')); a.href = x; a.target = '_blank'; a.rel = 'noopener noreferrer'; l.append(a); } body.append(l); }
    if (!opts.preview) {
      const f = GUILDS.find(currentConv), s = f && f.kind === 'ch' ? f.server : null;
      if (s && s.members.includes(u)) {
        const roles = GUILDS.rolesOfUser(s, u);
        if (roles.length) { const r = el('div', 'pRoles'); for (const x of roles) { const c = el('span', 'roleChip', x.name); if (x.color) { c.style.borderColor = x.color; c.style.color = x.color; } r.append(c); } body.append(el('div', 'pHead', 'Roles in ' + s.name), r); }
      }
      const acts = el('div', 'row');
      if (u === me.username) { const e = el('button', '', 'Edit profile'); e.type = 'button'; e.onclick = () => { hide(); openSettings(); tab('profile'); }; acts.append(e); }
      else { const d = el('button', '', 'Message'); d.type = 'button'; d.onclick = () => { hide(); selectConv(dmConv(u)); }; acts.append(d); }
      body.append(acts);
    }
    card.append(body);
    return card;
  }
  function hide() { const c = $('profileCard'); c.hidden = true; c.textContent = ''; }
  function show(u, ev) {
    if (!u || !me) return;
    const c = $('profileCard'); c.textContent = ''; c.append(cardFor(u)); c.hidden = false;
    const x = ev && ev.clientX != null ? ev.clientX : innerWidth / 2, y = ev && ev.clientY != null ? ev.clientY : innerHeight / 3;
    const w = c.offsetWidth, h = c.offsetHeight;
    c.style.left = Math.max(8, Math.min(x, innerWidth - w - 8)) + 'px'; c.style.top = Math.max(8, Math.min(y, innerHeight - h - 8)) + 'px';
  }
  document.addEventListener('click', (e) => { const c = $('profileCard'); if (c && !c.hidden && !e.target.closest('#profileCard') && !e.target.closest('[data-profile]')) hide(); });
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') hide(); });

  // ── Settings tabs ──
  function tab(name) {
    for (const b of document.querySelectorAll('[data-settab]')) b.classList.toggle('on', b.dataset.settab === name);
    for (const p of document.querySelectorAll('[data-setpane]')) p.hidden = p.dataset.setpane !== name;
  }
  for (const b of document.querySelectorAll('[data-settab]')) b.onclick = () => tab(b.dataset.settab);

  // ── the Profile tab: editor + live preview ──
  let draft = {}, picture = '';
  // a picture is cropped to the middle square and shrunk to 256 px here, in the browser; only that small image is sent
  async function shrink(file) {
    if (!/^image\//.test(file.type)) throw new Error('Pick a picture file.');
    if (file.size > 25e6) throw new Error('That file is very large; pick one under 25 MB.');
    const bmp = await createImageBitmap(file);
    const side = Math.min(bmp.width, bmp.height), size = Math.min(256, side);
    const c = document.createElement('canvas'); c.width = c.height = size;
    c.getContext('2d').drawImage(bmp, (bmp.width - side) / 2, (bmp.height - side) / 2, side, side, 0, 0, size, size);
    bmp.close && bmp.close();
    let url = c.toDataURL('image/webp', 0.86);
    if (!url.startsWith('data:image/webp')) url = c.toDataURL('image/jpeg', 0.88); // browsers without WebP encoding
    return url;
  }
  function pictureEditor(box) {
    const wrap = el('div', 'setSection'); wrap.append(el('b', '', 'Profile picture'));
    wrap.append(el('small', '', 'Shown on your card, in the member list and next to your messages. It is cropped to a square and shrunk to 256 px in this browser before upload, so the original never leaves your PC. Without one, your initials show in your colour.'));
    const file = el('input'); file.type = 'file'; file.accept = 'image/png,image/jpeg,image/webp,image/gif'; file.hidden = true;
    const up = el('button', '', picture ? 'Change picture' : 'Upload picture'); up.type = 'button'; up.onclick = () => file.click();
    const rm = el('button', 'ghost', 'Remove picture'); rm.type = 'button'; rm.hidden = !picture;
    const msg = el('span', 'status');
    file.onchange = async () => {
      const f = file.files && file.files[0]; file.value = ''; if (!f) return;
      msg.textContent = 'Shrinking and uploading…'; msg.className = 'status';
      try {
        const d = await json('/api/profile/avatar', { method: 'POST', body: JSON.stringify({ image: await shrink(f) }) });
        picture = d.profile.avatar || ''; up.textContent = 'Change picture'; rm.hidden = !picture;
        msg.textContent = 'Picture saved: everyone sees it now.'; msg.className = 'status on'; preview();
        await refreshMembers(); selectConv(currentConv);
      } catch (e) { msg.textContent = e.message; msg.className = 'status'; }
    };
    rm.onclick = async () => {
      if (rm.dataset.armed !== '1') { rm.dataset.armed = '1'; rm.textContent = 'Click again to remove'; rm.classList.add('armed'); return; }
      try { await json('/api/profile/avatar/remove', { method: 'POST', body: '{}' }); picture = ''; rm.hidden = true; rm.dataset.armed = ''; rm.classList.remove('armed'); rm.textContent = 'Remove picture'; up.textContent = 'Upload picture'; msg.textContent = 'Picture removed.'; msg.className = 'status on'; preview(); await refreshMembers(); selectConv(currentConv); }
      catch (e) { msg.textContent = e.message; msg.className = 'status'; }
    };
    const r = el('div', 'row'); r.append(up, rm, file, msg); wrap.append(r);
    box.append(wrap);
  }
  const avatarOf = (u) => profOf(u).avatar || '';
  function preview() {
    const box = $('profilePreview'); if (!box || !me) return;
    box.textContent = ''; box.append(cardFor(me.username, draft, { preview: true, nick: $('setNick').value.trim() || me.display, color: pickedColor, avatar: picture }));
  }
  async function open() {
    const box = $('profileBox'); if (!box) return;
    tab('profile');
    let cur; try { cur = await json('/api/profile'); } catch (e) { box.textContent = e.message; return; }
    picture = cur.profile.avatar || ''; draft = { ...cur.profile }; delete draft.avatar; // the picture has its own buttons
    box.textContent = '';
    pictureEditor(box);
    const field = (label, node) => { const l = el('label', '', label); l.append(node); box.append(l); return node; };
    const inp = (v, max, ph) => { const i = el('input'); i.value = v || ''; i.maxLength = max; if (ph) i.placeholder = ph; return i; };
    const status = field('Status (a short line under your name)', inp(draft.status, 80, 'e.g. heads down until 5'));
    const pron = field('Pronouns', inp(draft.pronouns, 40, 'e.g. they/them'));
    const bio = el('textarea'); bio.value = draft.bio || ''; bio.maxLength = 300; bio.rows = 4; bio.placeholder = 'a few words about you (https links become clickable)'; field('About me (bio, up to 300 characters)', bio);
    const loc = field('Location', inp(draft.location, 60, 'optional'));
    const links = [0, 1, 2].map((i) => inp((draft.links || [])[i], 200, 'https:// link (optional)'));
    const lw = el('div', 'pLinkInputs'); lw.append(...links); field('Links', lw);
    const tz = inp(draft.timezone, 60, 'e.g. Europe/Berlin (empty = do not show my local time)'); tz.setAttribute('list', 'tzList');
    const dl = el('datalist'); dl.id = 'tzList'; try { for (const z of Intl.supportedValuesOf('timeZone')) { const o = el('option'); o.value = z; dl.append(o); } } catch {}
    const mine = el('button', 'ghost', 'Use this browser\'s'); mine.type = 'button'; mine.onclick = () => { tz.value = Intl.DateTimeFormat().resolvedOptions().timeZone || ''; sync(); };
    const tzRow = el('div', 'row'); tzRow.append(tz, dl, mine); field('Show my local time', tzRow);
    // banner colour: by default follows your colour
    const sw = el('div', 'swatches'); let accent = Number.isInteger(draft.accent) ? draft.accent : null;
    const follow = el('button', '', 'follow my colour'); follow.type = 'button'; follow.dataset.idx = ''; sw.append(follow);
    PALETTE.forEach((pair, i) => { const b = el('button'); b.type = 'button'; b.dataset.idx = i; b.style.background = pair[lightTheme ? 1 : 0]; b.title = 'Colour ' + (i + 1); sw.append(b); });
    const mark = () => sw.querySelectorAll('button').forEach((b) => b.classList.toggle('on', String(accent ?? '') === b.dataset.idx));
    sw.querySelectorAll('button').forEach((b) => { b.onclick = () => { accent = b.dataset.idx === '' ? null : Number(b.dataset.idx); mark(); sync(); }; });
    mark(); const sl = el('div', '', 'Profile banner colour'); sl.append(sw); box.append(sl);
    const msg = el('span', 'status');
    const save = el('button', '', 'Save profile'); save.type = 'button';
    const sync = () => { draft = { status: status.value, pronouns: pron.value, bio: bio.value, location: loc.value, links: links.map((l) => l.value.trim()).filter(Boolean), timezone: tz.value.trim(), accent }; preview(); };
    for (const n of [status, pron, bio, loc, tz, ...links]) n.oninput = sync;
    if (!open.wired) { open.wired = true; $('setNick').addEventListener('input', preview); $('setColors').addEventListener('click', () => setTimeout(preview, 0)); }
    save.onclick = async () => {
      sync();
      try {
        const d = await json('/api/profile', { method: 'POST', body: JSON.stringify({ nick: $('setNick').value, color: pickedColor, profile: draft }) });
        me.prefs = d.prefs; msg.textContent = 'Profile saved: everyone sees it now.'; msg.className = 'status on';
        await refreshMembers(); selectConv(currentConv);
      } catch (e) { msg.textContent = e.message; msg.className = 'status'; }
    };
    const r = el('div', 'row'); r.append(save, msg); box.append(r);
    sync();
  }
  return { open, show, hide, tab, cardFor, avatarOf };
})();
