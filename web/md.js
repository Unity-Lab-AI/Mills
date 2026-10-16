// Mills markdown renderer, shared by the Work (Claude) chat and the team chat.
//
// It builds DOM nodes directly and never assigns HTML strings, so markdown from anyone in the team
// chat cannot inject markup or script. Raw HTML in a message is shown as text.
//
// Supported: headings, paragraphs (single newlines kept as line breaks, chat style), **bold**,
// *italic*, ~~strike~~, ==highlight==, `code`, links (http / https / mailto only), bare and <auto>
// links, images (https and inline data images load lazily without a referrer; anything else is a link), > quotes, GitHub alerts
// (> [!NOTE] / TIP / IMPORTANT / WARNING / CAUTION), nested bullet / numbered / task lists, GFM tables
// with column alignment, --- rules, footnotes ([^1] … [^1]: text), $$ math blocks (shown as source),
// fenced code with a language label, light syntax colouring and a Copy button, and ```mermaid diagrams.
//
// Mermaid is the project's own copy in vendor/mermaid.min.js, loaded only when a diagram appears, run
// with securityLevel "strict" and without HTML labels; its SVG is parsed (not assigned as HTML) and
// stripped of scripts, event handlers and outside links before it is shown.
(function () {
  'use strict';
  const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };

  function safeHref(u) {
    try { const x = new URL(u, location.href); return /^(https?:|mailto:)$/.test(x.protocol) ? x.href : null; } catch { return null; }
  }
  function link(href, kids) {
    const a = el('a'); a.href = href; a.target = '_blank'; a.rel = 'noopener noreferrer nofollow'; a.title = href;
    a.addEventListener('click', (e) => e.stopPropagation()); // inside a chat message: open the link, not the message menu
    if (typeof kids === 'string') a.textContent = kids; else a.append(...kids);
    return a;
  }

  // ── inline ──
  const INLINE = new RegExp([
    '\\\\(?<esc>[\\\\`*_{}\\[\\]()#+\\-.!~=|<>@$])',
    '(?<tick>`+)(?<code>[\\s\\S]*?[^`])\\k<tick>(?!`)',
    '!\\[(?<ialt>[^\\]]*)\\]\\((?<isrc>[^)\\s]+)(?:\\s+"[^"]*")?\\)',
    '\\[\\^(?<fn>[^\\]\\s]+)\\]',
    '\\[(?<ltext>(?:[^\\[\\]\\n]|\\[[^\\]\\n]*\\])+)\\]\\((?<lhref>[^)\\s]+)(?:\\s+"[^"]*")?\\)',
    '<(?<alink>(?:https?:\\/\\/|mailto:)[^>\\s]+)>',
    '(?<url>https?:\\/\\/[^\\s<]*[^\\s<.,:;"\'\\)\\]!?*_~])',
    '\\*\\*(?<b1>\\S(?:[\\s\\S]*?\\S)?)\\*\\*',
    '(?<![\\w])__(?<b2>\\S(?:[\\s\\S]*?\\S)?)__(?![\\w])',
    '~~(?<s>\\S(?:[\\s\\S]*?\\S)?)~~',
    '==(?<mk>\\S(?:[\\s\\S]*?\\S)?)==',
    '\\*(?<i1>[^\\s*](?:[\\s\\S]*?[^\\s*])?)\\*',
    '(?<![\\w])_(?<i2>[^\\s_](?:[\\s\\S]*?[^\\s_])?)_(?![\\w])',
    '(?<mention>(?<![\\w])@[\\w.-]*\\w)',
    '(?<br>\\n)',
  ].join('|'), 'g');

  function inline(parent, text, ctx, noLinks) {
    const re = new RegExp(INLINE.source, 'g');
    let last = 0, m;
    while ((m = re.exec(text))) {
      const g = m.groups;
      if (g.url && noLinks) continue;
      if (m.index > last) parent.append(text.slice(last, m.index));
      last = re.lastIndex;
      if (g.esc !== undefined) parent.append(g.esc);
      else if (g.code !== undefined) parent.append(el('code', 'mdc', g.code.length > 2 && g.code[0] === ' ' && g.code.at(-1) === ' ' ? g.code.slice(1, -1) : g.code));
      else if (g.isrc !== undefined) {
        const href = safeHref(g.isrc);
        if (/^data:image\/(png|gif|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(g.isrc)) { const img = el('img', 'mdimg'); img.src = g.isrc; img.alt = g.ialt; parent.append(img); }
        else if (href) parent.append(link(href, MD.icon ? [MD.icon('image', 'lead'), g.ialt || 'image'] : 'Image: ' + (g.ialt || 'image')));
        else parent.append(m[0]);
      } else if (g.fn !== undefined) {
        const n = ctx.fnRef(g.fn), s = el('sup', 'mdfnref', '[' + n + ']');
        s.title = 'Footnote ' + n; s.onclick = (e) => { e.stopPropagation(); const t = ctx.root.querySelector('[data-fn="' + CSS.escape(g.fn) + '"]'); if (t) t.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); };
        parent.append(s);
      } else if (g.lhref !== undefined) {
        const href = noLinks ? null : safeHref(g.lhref);
        if (!href) { parent.append(m[0]); continue; }
        const a = link(href, []); inline(a, g.ltext, ctx, true); parent.append(a);
      } else if (g.alink !== undefined || g.url !== undefined) {
        const u = g.alink || g.url, href = safeHref(u);
        parent.append(href ? link(href, u) : u);
      } else if (g.b1 !== undefined || g.b2 !== undefined) { const b = el('strong'); inline(b, g.b1 ?? g.b2, ctx, noLinks); parent.append(b); }
      else if (g.s !== undefined) { const s = el('del'); inline(s, g.s, ctx, noLinks); parent.append(s); }
      else if (g.mk !== undefined) { const s = el('mark'); inline(s, g.mk, ctx, noLinks); parent.append(s); }
      else if (g.i1 !== undefined || g.i2 !== undefined) { const i = el('em'); inline(i, g.i1 ?? g.i2, ctx, noLinks); parent.append(i); }
      else if (g.mention !== undefined) { const node = ctx.mention && ctx.mention(g.mention.slice(1)); parent.append(node || g.mention); }
      else if (g.br !== undefined) parent.append(el('br'));
    }
    if (last < text.length) parent.append(text.slice(last));
  }

  // ── code: light syntax colouring (comments, strings, numbers, keywords) ──
  const KW = new Set(('if else for while do switch case default break continue return function const let var class extends new delete typeof instanceof in of try catch finally throw async await yield import export from as this super null undefined true false void ' +
    'def elif lambda pass with global nonlocal None True False and or not is raise except assert print self ' +
    'fn mut pub struct enum impl trait match use mod crate move ref where loop type interface implements package private protected public static final abstract ' +
    'func go chan defer select map range nil int float double char bool string long short unsigned signed sizeof struct union typedef namespace using template ' +
    'echo then fi done esac local export SELECT FROM WHERE INSERT INTO VALUES UPDATE SET DELETE CREATE TABLE JOIN ON AND OR NOT NULL ORDER BY GROUP LIMIT').split(' '));
  const HL_LANGS = /^(js|javascript|jsx|ts|typescript|tsx|mjs|cjs|json|py|python|rs|rust|go|java|kt|kotlin|c|h|cpp|cc|hpp|cs|csharp|swift|php|rb|ruby|sh|bash|zsh|shell|ps1|powershell|sql|lua|dart|scala|yaml|yml|toml|ini)$/i;
  const TOKEN = /(\/\*[\s\S]*?\*\/|\/\/[^\n]*|#[^\n]*|--[^\n]*)|("(?:\\[\s\S]|[^"\\\n])*"|'(?:\\[\s\S]|[^'\\\n])*'|`(?:\\[\s\S]|[^`\\])*`)|(\b\d[\d_]*(?:\.\d+)?(?:e[+-]?\d+)?\b|\b0x[\da-f]+\b)|([A-Za-z_$][\w$]*)/gi;
  function highlight(code, lang, pre) {
    const l = (lang || '').toLowerCase();
    if (l === 'diff' || l === 'patch') {
      for (const line of code.split('\n')) {
        const s = el('span', /^\+/.test(line) ? 'hl-add' : /^-/.test(line) ? 'hl-del' : /^@@/.test(line) ? 'hl-kw' : '', line + '\n'); pre.append(s);
      }
      return;
    }
    if (!HL_LANGS.test(l)) { pre.textContent = code; return; }
    const hashComments = /^(py|python|sh|bash|zsh|shell|ps1|powershell|rb|ruby|yaml|yml|toml|ini)$/.test(l), dashComments = /^(sql|lua)$/.test(l);
    let last = 0, m;
    const re = new RegExp(TOKEN.source, 'gi');
    while ((m = re.exec(code))) {
      let cls = '';
      if (m[1]) { if ((m[1][0] === '#' && !hashComments) || (m[1].startsWith('--') && !dashComments)) { re.lastIndex = m.index + 1; continue; } cls = 'hl-com'; }
      else if (m[2]) cls = 'hl-str';
      else if (m[3]) cls = 'hl-num';
      else if (m[4]) cls = KW.has(m[4]) ? 'hl-kw' : '';
      if (!cls) continue;
      if (m.index > last) pre.append(code.slice(last, m.index));
      pre.append(el('span', cls, m[0])); last = re.lastIndex;
    }
    if (last < code.length) pre.append(code.slice(last));
  }
  function copyButton(getText) {
    const b = el('button', 'mdbtn', 'Copy'); b.type = 'button';
    b.onclick = async (e) => {
      e.stopPropagation();
      try { await navigator.clipboard.writeText(getText()); b.textContent = 'Copied'; } catch { b.textContent = 'Copy failed'; }
      setTimeout(() => { b.textContent = 'Copy'; }, 1400);
    };
    return b;
  }
  function codeBlock(code, lang) {
    const box = el('div', 'mdcode'), head = el('div', 'mdhead');
    head.append(el('span', '', lang || 'text'), copyButton(() => code));
    const pre = el('pre'), c = el('code'); highlight(code, lang, c); pre.append(c);
    box.append(head, pre);
    return box;
  }

  // ── mermaid ──
  let mermaidLoading = null, renderChain = Promise.resolve(), diagramN = 0;
  const svgCache = new Map();
  function loadMermaid() {
    if (window.mermaid) return Promise.resolve(window.mermaid);
    if (!mermaidLoading) {
      mermaidLoading = new Promise((resolve, reject) => {
        const s = document.createElement('script'); s.src = MD.mermaidSrc; s.async = true;
        s.onload = () => (window.mermaid ? resolve(window.mermaid) : reject(new Error('the diagram library did not load')));
        s.onerror = () => { mermaidLoading = null; s.remove(); reject(new Error('the diagram library (vendor/mermaid.min.js) is not installed')); };
        document.head.append(s);
      });
    }
    return mermaidLoading;
  }
  function cleanSvg(node) { // defence in depth on top of mermaid's own strict mode
    for (const bad of node.querySelectorAll('script, iframe, object, embed, link, meta, foreignObject iframe')) bad.remove();
    for (const n of [node, ...node.querySelectorAll('*')]) {
      for (const a of [...n.attributes]) {
        const name = a.name.toLowerCase(), v = a.value.trim().toLowerCase();
        if (name.startsWith('on')) n.removeAttribute(a.name);
        else if ((name === 'href' || name === 'xlink:href' || name === 'src') && !v.startsWith('#')) n.removeAttribute(a.name);
        else if (name === 'style' && /url\s*\(|expression|javascript:/.test(v)) n.removeAttribute(a.name);
      }
    }
    return node;
  }
  function mermaidBlock(code) {
    const box = el('div', 'mdcode mdmermaid'), head = el('div', 'mdhead'), body = el('div', 'mdmbody', 'Drawing diagram…');
    const src = el('pre', 'mdmsrc', code); src.hidden = true;
    const toggle = el('button', 'mdbtn', 'Source'); toggle.type = 'button';
    toggle.onclick = (e) => { e.stopPropagation(); src.hidden = !src.hidden; toggle.textContent = src.hidden ? 'Source' : 'Hide source'; };
    head.append(el('span', '', 'mermaid diagram'), toggle, copyButton(() => code));
    box.append(head, body, src);
    const dark = MD.isDark();
    const key = (dark ? 'd:' : 'l:') + code;
    const show = (svgText) => {
      const doc = new DOMParser().parseFromString(svgText, 'image/svg+xml');
      const svg = doc.documentElement;
      if (!svg || svg.nodeName.toLowerCase() !== 'svg') throw new Error('no diagram produced');
      body.textContent = ''; body.append(cleanSvg(document.importNode(svg, true)));
    };
    if (svgCache.has(key)) { try { show(svgCache.get(key)); } catch {} return box; }
    renderChain = renderChain.then(async () => {
      try {
        const mermaid = await loadMermaid();
        mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default', htmlLabels: false, flowchart: { htmlLabels: false }, fontFamily: 'inherit' });
        const id = 'mdm' + (++diagramN);
        try {
          const { svg } = await mermaid.render(id, code);
          svgCache.set(key, svg); if (svgCache.size > 200) svgCache.delete(svgCache.keys().next().value);
          show(svg);
        } finally { for (const leftover of document.querySelectorAll('#' + id + ', #d' + id)) if (!body.contains(leftover)) leftover.remove(); }
      } catch (err) {
        body.textContent = ''; body.append(el('div', 'mderr', 'Diagram not drawn: ' + String(err && err.message || err).split('\n')[0]));
        src.hidden = false; toggle.textContent = 'Hide source';
      }
    });
    return box;
  }

  // ── blocks ──
  const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)[^`]*$/;
  const HEADING = /^ {0,3}(#{1,6})(?:\s+(.*?))?\s*#*\s*$/;
  const HR = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
  const QUOTE = /^ {0,3}>\s?(.*)$/;
  const LIST = /^( *)([-*+]|\d{1,9}[.)])[ \t]+(.*)$/;
  const FNDEF = /^ {0,3}\[\^([^\]\s]+)\]:\s?(.*)$/;
  const TABLE_SEP = /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/;
  const ALERT = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*$/i;
  const indentOf = (s) => s.match(/^ */)[0].length;
  const startsBlock = (line, next) => FENCE.test(line) || HEADING.test(line) || HR.test(line) || QUOTE.test(line) || LIST.test(line) || line.trim() === '$$'
    || (line.includes('|') && next !== undefined && TABLE_SEP.test(next) && next.includes('-'));

  function splitRow(line) {
    let s = line.trim(); if (s.startsWith('|')) s = s.slice(1); if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
    const cells = []; let cur = '', tick = false;
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (c === '\\' && s[i + 1] === '|') { cur += '|'; i++; continue; }
      if (c === '`') tick = !tick;
      if (c === '|' && !tick) { cells.push(cur.trim()); cur = ''; continue; }
      cur += c;
    }
    cells.push(cur.trim());
    return cells;
  }
  function table(lines, i, ctx) {
    const head = splitRow(lines[i]), aligns = splitRow(lines[i + 1]).map((c) => (/^:-+:$/.test(c) ? 'center' : /^-+:$/.test(c) ? 'right' : /^:-+$/.test(c) ? 'left' : ''));
    const wrap = el('div', 'mdtable'), t = el('table'), thead = el('thead'), tbody = el('tbody'), hr = el('tr');
    head.forEach((c, k) => { const th = el('th'); if (aligns[k]) th.style.textAlign = aligns[k]; inline(th, c, ctx); hr.append(th); });
    thead.append(hr); t.append(thead);
    i += 2;
    while (i < lines.length && lines[i].trim() && lines[i].includes('|')) {
      const tr = el('tr'), cells = splitRow(lines[i]);
      for (let k = 0; k < head.length; k++) { const td = el('td'); if (aligns[k]) td.style.textAlign = aligns[k]; inline(td, cells[k] || '', ctx); tr.append(td); }
      tbody.append(tr); i++;
    }
    t.append(tbody); wrap.append(t);
    return { node: wrap, i };
  }
  function list(lines, i, ctx) {
    const first = LIST.exec(lines[i]), base = first[1].length, ordered = /\d/.test(first[2]);
    const node = el(ordered ? 'ol' : 'ul', 'mdlist');
    if (ordered && parseInt(first[2], 10) !== 1) node.start = parseInt(first[2], 10);
    const items = []; let cur = null;
    while (i < lines.length) {
      const line = lines[i], m = LIST.exec(line);
      if (m && m[1].length === base && /\d/.test(m[2]) === ordered) { cur = { lines: [m[3]], pad: base + m[2].length + 1, loose: false }; items.push(cur); i++; continue; }
      if (!line.trim()) {
        let j = i + 1; while (j < lines.length && !lines[j].trim()) j++;
        if (j >= lines.length) break;
        const mm = LIST.exec(lines[j]);
        if (indentOf(lines[j]) > base || (mm && mm[1].length === base && /\d/.test(mm[2]) === ordered)) { cur.loose = cur.loose || indentOf(lines[j]) > base; cur.lines.push(''); i++; continue; }
        break;
      }
      if (indentOf(line) > base) { cur.lines.push(line.slice(Math.min(indentOf(line), cur.pad))); i++; continue; }
      if (!startsBlock(line, lines[i + 1])) { cur.lines.push(line); i++; continue; } // lazy continuation
      break;
    }
    for (const it of items) {
      const li = el('li'), task = /^\[([ xX])\]\s+/.exec(it.lines[0]);
      if (task) {
        li.className = 'mdtask'; it.lines[0] = it.lines[0].slice(task[0].length);
        const cb = el('input'); cb.type = 'checkbox'; cb.disabled = true; cb.checked = task[1] !== ' '; li.append(cb);
      }
      const holder = el('div'); blocks(holder, it.lines, ctx);
      if (!it.loose) for (const p of [...holder.children]) if (p.tagName === 'P') p.replaceWith(...p.childNodes);
      li.append(...holder.childNodes);
      node.append(li);
    }
    return { node, i };
  }
  function blocks(parent, lines, ctx) {
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (!line.trim()) { i++; continue; }
      let m;
      if ((m = FENCE.exec(line))) {
        const fence = m[1], lang = m[2] || '', body = []; i++;
        while (i < lines.length && !(lines[i].trim().startsWith(fence[0].repeat(fence.length)) && /^ {0,3}(`{3,}|~{3,})\s*$/.test(lines[i]))) body.push(lines[i++]);
        i++; // closing fence (or the end, while a reply is still streaming in)
        const code = body.join('\n');
        if (/^mermaid$/i.test(lang)) parent.append(mermaidBlock(code));
        else if (/^(math|latex|tex|katex)$/i.test(lang)) parent.append(el('pre', 'mdmath', code));
        else parent.append(codeBlock(code, lang));
        continue;
      }
      if (line.trim() === '$$') {
        const body = []; i++;
        while (i < lines.length && lines[i].trim() !== '$$') body.push(lines[i++]);
        i++; parent.append(el('pre', 'mdmath', body.join('\n'))); continue;
      }
      if ((m = HEADING.exec(line))) { const h = el('h' + m[1].length, 'mdh'); inline(h, m[2] || '', ctx); parent.append(h); i++; continue; }
      if (HR.test(line)) { parent.append(el('hr')); i++; continue; }
      if (line.includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes('-')) { const r = table(lines, i, ctx); parent.append(r.node); i = r.i; continue; }
      if (QUOTE.test(line)) {
        const inner = [];
        while (i < lines.length && (m = QUOTE.exec(lines[i]))) { inner.push(m[1]); i++; }
        const a = inner.length && ALERT.exec(inner[0].trim());
        const q = el('blockquote', a ? 'mdalert mdalert-' + a[1].toLowerCase() : '');
        if (a) { q.append(el('div', 'mdalert-t', a[1][0] + a[1].slice(1).toLowerCase())); inner.shift(); }
        blocks(q, inner, ctx); parent.append(q); continue;
      }
      if (LIST.test(line)) { const r = list(lines, i, ctx); parent.append(r.node); i = r.i; continue; }
      if ((m = FNDEF.exec(line))) {
        const body = [m[2]]; i++;
        while (i < lines.length && (/^ {2,}\S/.test(lines[i]) || (!lines[i].trim() && /^ {2,}\S/.test(lines[i + 1] || '')))) body.push(lines[i++].trim());
        ctx.fnDefs.set(m[1], body.join('\n')); continue;
      }
      const para = [line]; i++;
      while (i < lines.length && lines[i].trim() && !startsBlock(lines[i], lines[i + 1]) && !FNDEF.test(lines[i])) para.push(lines[i++]);
      const p = el('p'); inline(p, para.join('\n'), ctx); parent.append(p);
    }
  }

  const MD = {
    mermaidSrc: 'vendor/mermaid.min.js',
    isDark: () => true,
    // Render markdown text into el (replacing its content). opts.mention(handle) → a Node for an
    // @handle, or null to leave it as text.
    render(target, text, opts = {}) {
      target.textContent = '';
      target.classList.add('md');
      const fnOrder = [];
      const ctx = { root: target, mention: opts.mention, fnDefs: new Map(), fnRef: (k) => { let n = fnOrder.indexOf(k); if (n < 0) { fnOrder.push(k); n = fnOrder.length - 1; } return n + 1; } };
      blocks(target, String(text || '').replace(/\r\n?/g, '\n').replace(/\t/g, '    ').split('\n'), ctx);
      const used = fnOrder.filter((k) => ctx.fnDefs.has(k));
      if (used.length) {
        const sec = el('div', 'mdfns'), ol = el('ol');
        for (const k of used) { const li = el('li'); li.dataset.fn = k; inline(li, ctx.fnDefs.get(k), ctx); ol.append(li); }
        sec.append(el('hr'), ol); target.append(sec);
      }
      return target;
    },
  };
  window.MD = MD;
})();
