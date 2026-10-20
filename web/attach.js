// VibeRoller attachments, page side (hub side: bridge/files.js): the + button's tray under a message box, and how sent
// files show in the team chat and in Work. Loaded after the page script; uses its helpers at call time ($, ic, MD).
//
// Shown by what they are, never by what a file claims to be able to do:
//   pictures (png, jpeg, gif, webp, avif, bmp, svg)  inline, click for the full size (an <img>: an SVG never runs anything)
//   video / audio                                   a player, loaded when you press play
//   text: markdown, code, logs, data                a viewer: markdown rendered, code highlighted for its language
//   anything else (binary, unknown)                 its name and size only, and Download: never opened, shown or played
// Text is text only when it really is: no NUL byte and valid UTF-8 in what is read, whatever its name says.
const ATTACH = (() => {
  const MAX = 24 * 1024 * 1024, MAX_COUNT = 20, AUTO_IMG = 8 * 1024 * 1024; // pictures up to 8 MB load by themselves
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
  const size = (n) => (n < 1024 ? n + ' B' : n < 1048576 ? Math.round(n / 1024) + ' KB' : (n / 1048576).toFixed(1) + ' MB');
  const IMG = /^image\/(png|jpe?g|gif|webp|avif|bmp|svg\+xml)$/, VIDEO = /^video\/(mp4|webm|ogg|quicktime)$/, AUDIO = /^audio\/(mpeg|mp3|ogg|wav|x-wav|webm|aac|mp4|flac|x-m4a)$/;
  // file extension → the language the viewer highlights ('' = plain text)
  const LANG = { md: 'markdown', markdown: 'markdown', txt: '', log: '', csv: '', tsv: '', js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'javascript', ts: 'typescript', tsx: 'typescript',
    json: 'json', jsonl: 'json', py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java', kt: 'kotlin', cs: 'csharp', c: 'c', h: 'c', cpp: 'cpp', hpp: 'cpp', cc: 'cpp', swift: 'swift',
    php: 'php', html: 'html', htm: 'html', css: 'css', scss: 'css', xml: 'xml', svg: 'xml', yml: 'yaml', yaml: 'yaml', toml: 'toml', ini: 'ini', cfg: 'ini', conf: 'ini', env: 'ini',
    sh: 'bash', bash: 'bash', zsh: 'bash', ps1: 'powershell', bat: 'batch', cmd: 'batch', sql: 'sql', lua: 'lua', r: 'r', dart: 'dart', vue: 'html', gradle: 'groovy', dockerfile: 'dockerfile', makefile: 'makefile' };
  const EXT_MIME = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', avif: 'image/avif', bmp: 'image/bmp', svg: 'image/svg+xml',
    mp4: 'video/mp4', webm: 'video/webm', ogv: 'video/ogg', mov: 'video/quicktime', mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg', m4a: 'audio/mp4', flac: 'audio/flac', aac: 'audio/aac' };
  const ext = (n) => { const b = String(n || '').toLowerCase().split(/[\\/]/).pop(); if (b === 'dockerfile' || b === 'makefile') return b; const i = b.lastIndexOf('.'); return i > 0 ? b.slice(i + 1) : ''; };
  const mimeOf = (name, type) => (type && type !== 'application/octet-stream' ? type : EXT_MIME[ext(name)] || type || 'application/octet-stream');
  function kind(f) {
    const m = String(f.m || '').toLowerCase();
    if (IMG.test(m)) return 'image';
    if (VIDEO.test(m)) return 'video';
    if (AUDIO.test(m)) return 'audio';
    if (/^text\//.test(m) || /json|xml|javascript|x-sh|x-python|yaml|toml|markdown/.test(m) || ext(f.n) in LANG) return 'text';
    return 'binary';
  }
  const asText = (buf) => { const b = new Uint8Array(buf, 0, Math.min(buf.byteLength, 65536)); if (b.includes(0)) return null; try { return new TextDecoder('utf-8', { fatal: true }).decode(buf); } catch { return null; } };

  // ---------- the tray: files picked with the + button (or dropped / pasted), shown as chips until sent ----------
  function tray(box, onChange = () => {}) {
    const T = { files: [] };
    const draw = () => {
      box.textContent = ''; box.hidden = !T.files.length;
      T.files.forEach((f, i) => {
        const chip = el('span', 'attChip'); chip.append(ic(kind({ m: mimeOf(f.name, f.type), n: f.name }) === 'image' ? 'image' : 'file', 'lead'), el('span', 'attName', f.name), el('small', '', size(f.size)));
        const x = el('button', 'ghost tiny'); x.type = 'button'; x.title = 'Remove'; x.setAttribute('aria-label', 'Remove ' + f.name); x.append(ic('close')); x.onclick = () => { T.files.splice(i, 1); draw(); onChange(); };
        chip.append(x); box.append(chip);
      });
    };
    T.add = (list) => {
      const bad = [];
      for (const f of list) { if (T.files.length >= MAX_COUNT) { bad.push(`${f.name}: up to ${MAX_COUNT} files per message`); continue; } if (f.size > MAX) { bad.push(`${f.name}: larger than ${size(MAX)}`); continue; } if (!f.size) { bad.push(`${f.name}: empty`); continue; } T.files.push(f); }
      draw(); onChange(); return bad;
    };
    T.clear = () => { T.files = []; draw(); onChange(); };
    T.take = () => { const f = T.files; T.files = []; draw(); onChange(); return f; };
    draw();
    return T;
  }
  // wire a + button + a hidden file input + drop / paste on a message box to a tray
  function wire({ button, input, area, trayBox, onError }) {
    const T = tray(trayBox);
    const add = (list) => { const bad = T.add([...list]); if (bad.length) onError('Not attached: ' + bad.join('; ')); };
    button.onclick = () => input.click();
    input.onchange = () => { add(input.files); input.value = ''; };
    area.addEventListener('paste', (e) => { const fl = [...(e.clipboardData ? e.clipboardData.files : [])]; if (fl.length) { e.preventDefault(); add(fl); } });
    area.addEventListener('dragover', (e) => { if (e.dataTransfer && [...e.dataTransfer.types].includes('Files')) e.preventDefault(); });
    area.addEventListener('drop', (e) => { if (e.dataTransfer && e.dataTransfer.files.length) { e.preventDefault(); add(e.dataTransfer.files); } });
    return T;
  }

  // ---------- showing sent files ----------
  const cache = new Map(); // key → Promise<ArrayBuffer>
  const bytes = (f, get) => { const k = f.id || f.n + f.s; if (!cache.has(k)) cache.set(k, get(f).catch((e) => { cache.delete(k); throw e; })); return cache.get(k); };
  const blobUrl = async (f, get) => URL.createObjectURL(new Blob([await bytes(f, get)], { type: f.m }));
  function download(f, get, btn) {
    btn.disabled = true;
    bytes(f, get).then((b) => { const a = el('a'); a.href = URL.createObjectURL(new Blob([b], { type: 'application/octet-stream' })); a.download = f.n || 'file'; document.body.append(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(a.href), 60000); })
      .catch((e) => alert('Could not download ' + f.n + ': ' + e.message)).finally(() => { btn.disabled = false; });
  }
  function modal(title, body) {
    const back = el('div', 'attModal'), box = el('div', 'attBox'), bar = el('div', 'bar'), close = el('button', 'ghost', 'Close');
    bar.append(el('span', '', title), close); box.append(bar, body); back.append(box); document.body.append(back);
    const shut = () => { back.remove(); document.removeEventListener('keydown', esc); }, esc = (e) => { if (e.key === 'Escape') shut(); };
    close.onclick = shut; back.onclick = (e) => { if (e.target === back) shut(); }; document.addEventListener('keydown', esc);
    return shut;
  }
  async function viewText(f, get) {
    const body = el('div', 'attView'); body.textContent = 'Loading…';
    modal(f.n + ' · ' + size(f.s || 0), body);
    try {
      const text = asText(await bytes(f, get));
      body.textContent = '';
      if (text === null) { body.append(el('div', 'status', 'This is not text (binary content), so it is not shown. Download it to open it with the right program.')); return; }
      const lang = LANG[ext(f.n)] ?? '';
      if (lang === 'markdown') MD.render(body, text);
      else { const fence = '`'.repeat(Math.max(3, ...(text.match(/`+/g) || ['']).map((s) => s.length + 1))); MD.render(body, fence + lang + '\n' + text + '\n' + fence); }
    } catch (e) { body.textContent = 'Could not open it: ' + e.message; }
  }
  // items: [{ id, n (name), m (type), s (size) }], get(item) → Promise<ArrayBuffer>
  function render(host, items, get) {
    if (!Array.isArray(items) || !items.length) return;
    const wrap = el('div', 'atts');
    for (const f of items) {
      const k = kind(f), card = el('div', 'att att-' + k);
      const head = el('div', 'attHead'); head.append(ic(k === 'image' ? 'image' : k === 'video' ? 'play' : k === 'audio' ? 'mic' : 'file', 'lead'), el('span', 'attName', f.n || 'file'), el('small', '', size(f.s || 0)));
      const dl = el('button', 'ghost tiny'); dl.type = 'button'; dl.title = 'Download ' + (f.n || 'file'); dl.append(ic('download')); dl.onclick = (e) => { e.stopPropagation(); download(f, get, dl); };
      head.append(dl); card.append(head);
      if (k === 'image') {
        const img = el('img', 'attImg'); img.alt = f.n || ''; img.loading = 'lazy';
        const show = () => blobUrl(f, get).then((u) => { img.src = u; }).catch(() => { img.replaceWith(el('div', 'status', 'The picture could not be loaded.')); });
        if ((f.s || 0) <= AUTO_IMG) show(); else { const b = el('button', 'ghost', 'Show picture (' + size(f.s) + ')'); b.type = 'button'; b.onclick = () => { b.replaceWith(img); show(); }; card.append(b); }
        img.onclick = () => { if (img.src) { const big = el('img', 'attBig'); big.src = img.src; big.alt = f.n || ''; modal(f.n, big); } };
        if ((f.s || 0) <= AUTO_IMG) card.append(img);
      } else if (k === 'video' || k === 'audio') {
        const b = el('button', 'ghost'); b.type = 'button'; b.append(ic('play', 'lead'), k === 'video' ? 'Play video' : 'Play');
        b.onclick = () => { b.disabled = true; blobUrl(f, get).then((u) => { const p = el(k, 'att' + (k === 'video' ? 'Video' : 'Audio')); p.controls = true; p.src = u; p.autoplay = true; b.replaceWith(p); }).catch((e) => { b.disabled = false; b.textContent = 'Could not load it: ' + e.message; }); };
        card.append(b);
      } else if (k === 'text') {
        const b = el('button', 'ghost'); b.type = 'button'; b.append(ic('file', 'lead'), 'View'); b.onclick = () => viewText(f, get); card.append(b);
      } else card.append(el('small', 'attNote', 'Not a picture, video or text: kept as a file (Download).'));
      wrap.append(card);
    }
    host.append(wrap);
  }
  // one file in its own window (Files): its picture / player / viewer button, as in a conversation
  function open(f, get) { const body = el('div', 'attView'); render(body, [f], get); modal(f.n || 'file', body); }
  return { MAX, tray, wire, render, open, download, kind, mimeOf, size, asText };
})();
