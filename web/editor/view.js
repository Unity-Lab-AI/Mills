// editor-view (T-205): a DOM / SVG view on top of core.js. Pan / zoom, drag with snapping, resize, link drawing, a
// palette built from the registered kinds (with a search), a property panel built from the kinds' json schema, a
// breadcrumb for groups you entered, tabbed side panels (a tool adds its own: the AI chat slot is one), and overlays a
// tool can draw on items: run states, problem marks and a patch preview. Plain styling from the page's CSS variables;
// Mills does the look later. The view knows no tool.
//   const v = VrEditorView.mount(element, { editor });
//   v.aiSlot            the element for the AI chat (a side panel)
//   v.addButton(text, title, fn) · v.addPanel(id, title, element) · v.showPanel(id)
//   v.setRunStates({ id: { state, text, title } }) · v.setMarks({ id: { level, text } }) · v.setPreview({ added, changed, removed, links })
//   v.destroy();
// PROVISIONAL DECISION (open question 11): plain-JavaScript core first; native shows this same web module in its
// webview; a Rust port later must pass the same fixtures (R453).
(function (root) {
  'use strict';
  const NS = 'http://www.w3.org/2000/svg';
  const CSS = `
.ve{display:flex;flex-direction:column;height:100%;min-height:320px;background:var(--bg,#0f1115);color:var(--text,#e6e8ee);font:13px system-ui,sans-serif}
.ve *{box-sizing:border-box}
.ve-bar{display:flex;gap:6px;align-items:center;flex-wrap:wrap;padding:6px 8px;border-bottom:1px solid var(--line,#2a2f3a);background:var(--panel,#171a21)}
.ve-bar button,.ve-pal button,.ve-tabs button{background:var(--bg,#0f1115);color:var(--text,#e6e8ee);border:1px solid var(--line,#2a2f3a);border-radius:6px;padding:4px 8px;cursor:pointer;font:inherit}
.ve-bar button:disabled{opacity:.4;cursor:default}.ve-bar button:hover:not(:disabled),.ve-pal button:hover,.ve-tabs button:hover{border-color:var(--accent,#7c9cff)}
.ve-bar .sep{width:1px;align-self:stretch;background:var(--line,#2a2f3a);margin:0 2px}
.ve-crumb{display:flex;gap:4px;align-items:center;color:var(--muted,#8b93a7)}.ve-crumb button{border:0;background:none;color:var(--accent,#7c9cff);padding:2px 4px}.ve-crumb b{color:var(--text,#e6e8ee)}
.ve-msg{margin-left:auto;color:var(--bad,#ff6b6b);min-height:1em}
.ve-main{display:flex;flex:1;min-height:0}
.ve-pal{width:170px;overflow:auto;border-right:1px solid var(--line,#2a2f3a);background:var(--panel,#171a21);padding:6px}
.ve-pal input{width:100%;margin-bottom:6px;background:var(--bg,#0f1115);color:var(--text,#e6e8ee);border:1px solid var(--line,#2a2f3a);border-radius:6px;padding:4px 6px;font:inherit}
.ve-pal h4{margin:8px 0 4px;font-size:11px;text-transform:uppercase;color:var(--muted,#8b93a7)}.ve-pal button{display:block;width:100%;text-align:left;margin-bottom:4px}.ve-pal button.off{opacity:.5}
.ve-canvas{flex:1;min-width:0;position:relative}.ve-canvas svg{width:100%;height:100%;display:block;touch-action:none;outline:none}
.ve-side{width:300px;display:flex;flex-direction:column;border-left:1px solid var(--line,#2a2f3a);background:var(--panel,#171a21);min-height:0}
.ve-tabs{display:flex;gap:4px;padding:6px;border-bottom:1px solid var(--line,#2a2f3a);flex-wrap:wrap}.ve-tabs button.on{border-color:var(--accent,#7c9cff);color:var(--accent,#7c9cff)}
.ve-panel{flex:1;min-height:0;overflow:auto}.ve-panel[hidden]{display:none}
.ve-props{padding:8px}.ve-props label{display:block;margin:8px 0 2px;color:var(--muted,#8b93a7);font-size:12px}
.ve-props input,.ve-props select,.ve-props textarea{width:100%;background:var(--bg,#0f1115);color:var(--text,#e6e8ee);border:1px solid var(--line,#2a2f3a);border-radius:6px;padding:4px 6px;font:inherit}
.ve-props input[type=checkbox]{width:auto}
.ve-props .bad{border-color:var(--bad,#ff6b6b)}.ve-props .err{color:var(--bad,#ff6b6b);font-size:12px;min-height:1em}
.ve-node rect.body{fill:var(--panel,#171a21);stroke:var(--line,#2a2f3a);stroke-width:1.5}.ve-node.sel rect.body{stroke:var(--accent,#7c9cff);stroke-width:2.5}
.ve-node text{fill:var(--text,#e6e8ee);font-size:12px;pointer-events:none}.ve-node text.sub{fill:var(--muted,#8b93a7);font-size:10px}
.ve-node.group rect.body{stroke-dasharray:6 4;fill:color-mix(in srgb,var(--accent,#7c9cff) 8%,transparent)}.ve-node.frame rect.body{fill:color-mix(in srgb,var(--accent,#7c9cff) 6%,transparent)}
.ve-node.off rect.body{stroke-dasharray:3 3;opacity:.7}
.ve-node.st-running rect.body{stroke:var(--accent,#7c9cff);stroke-width:3;animation:vepulse 1s infinite}
.ve-node.st-ok rect.body{stroke:var(--good,#4fd18b);stroke-width:3}.ve-node.st-error rect.body{stroke:var(--bad,#ff6b6b);stroke-width:3}
.ve-node.st-waiting rect.body,.ve-node.st-retrying rect.body{stroke:var(--warn,#e6a14f);stroke-width:3}.ve-node.st-skipped{opacity:.45}.ve-node.st-cancelled rect.body{stroke:var(--muted,#8b93a7);stroke-width:3}
.ve-node text.state{font-size:10px;fill:var(--muted,#8b93a7)}
@keyframes vepulse{50%{stroke-opacity:.4}}
.ve-mark.error{fill:var(--bad,#ff6b6b)}.ve-mark.warn{fill:var(--warn,#e6a14f)}
.ve-port{fill:var(--bg,#0f1115);stroke:var(--muted,#8b93a7);stroke-width:1.5;cursor:crosshair}.ve-port:hover{fill:var(--accent,#7c9cff)}
.ve-port.flow{stroke-dasharray:2 2}
.ve-link{fill:none;stroke:var(--muted,#8b93a7);stroke-width:2}.ve-link.sel{stroke:var(--accent,#7c9cff);stroke-width:3}.ve-link.tmp{stroke:var(--accent,#7c9cff);stroke-dasharray:5 4}
.ve-hit{fill:none;stroke:transparent;stroke-width:12;cursor:pointer}.ve-guide{stroke:var(--good,#4fd18b);stroke-width:1;stroke-dasharray:4 3}
.ve-band{fill:color-mix(in srgb,var(--accent,#7c9cff) 15%,transparent);stroke:var(--accent,#7c9cff);stroke-dasharray:4 3}
.ve-handle{fill:var(--accent,#7c9cff);cursor:nwse-resize}
.ve-pv.added{fill:color-mix(in srgb,var(--good,#4fd18b) 15%,transparent);stroke:var(--good,#4fd18b);stroke-dasharray:5 3;stroke-width:2}
.ve-pv.changed{fill:none;stroke:var(--warn,#e6a14f);stroke-width:3}.ve-pv.removed{fill:color-mix(in srgb,var(--bad,#ff6b6b) 12%,transparent);stroke:var(--bad,#ff6b6b);stroke-dasharray:5 3;stroke-width:2}
.ve-pvlink{fill:none;stroke:var(--good,#4fd18b);stroke-width:2.5;stroke-dasharray:6 3}`;

  const el = (tag, attrs, kids) => { const e = document.createElement(tag); for (const k of Object.keys(attrs || {})) { if (k === 'text') e.textContent = attrs[k]; else if (k === 'class') e.className = attrs[k]; else e.setAttribute(k, attrs[k]); } for (const c of kids || []) e.appendChild(c); return e; };
  const sv = (tag, attrs, kids) => { const e = document.createElementNS(NS, tag); for (const k of Object.keys(attrs || {})) { if (k === 'text') e.textContent = attrs[k]; else e.setAttribute(k, attrs[k]); } for (const c of kids || []) e.appendChild(c); return e; };
  const truncate = (s, n) => (String(s).length > n ? String(s).slice(0, Math.max(1, n - 1)) + '…' : String(s));

  function mount(host, o) {
    const ed = o.editor;
    if (!document.getElementById('ve-style')) { const s = el('style', { id: 've-style' }); s.textContent = CSS; document.head.appendChild(s); }
    const bar = el('div', { class: 've-bar' }), crumb = el('div', { class: 've-crumb' }), msg = el('div', { class: 've-msg' });
    const pal = el('div', { class: 've-pal' }), palSearch = el('input', { type: 'search', placeholder: 'Search the palette', 'aria-label': 'Search the palette' }), palList = el('div');
    pal.append(palSearch, palList);
    const props = el('div', { class: 've-props' }), ai = el('div', { 'data-slot': 'ai-chat' });
    const tabs = el('div', { class: 've-tabs' }), panels = {};
    const svg = sv('svg', { tabindex: '0', 'aria-label': 'Canvas' }), world = sv('g'), gridG = sv('g'), linksG = sv('g'), itemsG = sv('g'), pvG = sv('g'), tmpG = sv('g');
    const canvas = el('div', { class: 've-canvas' }, [svg]); svg.appendChild(gridG); svg.appendChild(world); world.append(linksG, itemsG, pvG, tmpG);
    const side = el('div', { class: 've-side' }, [tabs]);
    const root_ = el('div', { class: 've' }, [bar, el('div', { class: 've-main' }, [pal, canvas, side])]);
    host.appendChild(root_);

    function addPanel(id, title, element) {
      const p = el('div', { class: 've-panel', 'data-panel': id }, [element]); p.hidden = true; panels[id] = p; side.appendChild(p);
      const b = el('button', { type: 'button', text: title, 'data-tab': id }); b.onclick = () => showPanel(id); tabs.appendChild(b); return p;
    }
    function showPanel(id) { for (const k of Object.keys(panels)) panels[k].hidden = k !== id; for (const b of tabs.children) b.classList.toggle('on', b.dataset.tab === id); }
    addPanel('props', 'Properties', props); addPanel('ai', 'AI chat', ai); showPanel('props');
    ai.appendChild(el('p', { text: 'The AI chat is not connected for this tool.', style: 'padding:8px;color:var(--muted,#8b93a7)' }));

    const btn = (text, title, fn) => { const b = el('button', { text, title, type: 'button' }); b.onclick = fn; bar.insertBefore(b, crumb); return b; };
    const say = (t) => { msg.textContent = t || ''; if (t) { clearTimeout(say.t); say.t = setTimeout(() => { msg.textContent = ''; }, 6000); } };
    const run = (cmd, opt) => { const r = ed.exec(cmd, opt); if (!r.ok) say(r.error); return r; };
    const sel = () => ed.selection();
    let selLink = null, clip = null, runStates = {}, marks = {}, preview = null, spaceDown = false;
    bar.append(crumb, msg);
    const bUndo = btn('Undo', 'Ctrl+Z', () => ed.undo()), bRedo = btn('Redo', 'Ctrl+Y', () => ed.redo());
    btn('Group', 'Ctrl+G', () => doGroup()); btn('Ungroup', 'Ctrl+Shift+G', () => doUngroup());
    btn('Open group', 'Enter, or double-click a group', () => { const g = sel().filter((id) => ed.item(id).kind === 'group'); if (g.length === 1) ed.enter(g[0]); else say('Select one group to open'); });
    btn('Delete', 'Delete', () => doDelete());
    btn('Copy', 'Ctrl+C', () => { clip = ed.copy(); }); btn('Paste', 'Ctrl+V', () => clip && ed.paste(clip, { dx: 20, dy: 20 })); btn('Duplicate', 'Ctrl+D', () => sel().length && ed.duplicate());
    for (const [t, m, ti] of [['⇤', 'left', 'Align left'], ['↔', 'center', 'Align centres'], ['⇥', 'right', 'Align right'], ['⤒', 'top', 'Align top'], ['↕', 'middle', 'Align middles'], ['⤓', 'bottom', 'Align bottom']]) btn(t, ti, () => (sel().length > 1 ? run({ type: 'align', ids: sel(), mode: m }) : say('Select two or more items to align')));
    btn('−', 'Zoom out', () => zoomBy(1 / 1.25)); btn('+', 'Zoom in', () => zoomBy(1.25)); btn('Fit', 'Show everything', () => fit());
    const doGroup = () => { if (!sel().length) return; const r = run({ type: 'group', ids: sel() }); if (r.ok) ed.select([r.id]); };
    const doUngroup = () => { const g = sel().filter((id) => ed.item(id).kind === 'group'); if (g.length) run({ type: 'ungroup', ids: g }); };
    const doDelete = () => { if (selLink) { const id = selLink; selLink = null; run({ type: 'disconnect', id }); } else if (sel().length) run({ type: 'delete', ids: sel() }); };

    // ---- coordinates
    const toWorld = (e) => { const r = svg.getBoundingClientRect(), v = ed.viewState(); return { x: (e.clientX - r.left - v.pan.x) / v.zoom, y: (e.clientY - r.top - v.pan.y) / v.zoom }; };
    function zoomBy(f, cx, cy) { const v = ed.viewState(), r = svg.getBoundingClientRect(), z = Math.min(4, Math.max(0.2, v.zoom * f)), mx = cx === undefined ? r.width / 2 : cx, my = cy === undefined ? r.height / 2 : cy; ed.setView({ zoom: z, pan: { x: mx - ((mx - v.pan.x) / v.zoom) * z, y: my - ((my - v.pan.y) / v.zoom) * z } }); }
    function fit() {
      const its = ed.itemsAt(); if (!its.length) return ed.setView({ pan: { x: 40, y: 40 }, zoom: 1 });
      const x1 = Math.min(...its.map((i) => i.x)), y1 = Math.min(...its.map((i) => i.y)), x2 = Math.max(...its.map((i) => i.x + i.w)), y2 = Math.max(...its.map((i) => i.y + ed.effH(i))), b = { x: x1, y: y1, w: x2 - x1, h: y2 - y1 }, r = svg.getBoundingClientRect();
      const z = Math.min(1.5, Math.max(0.2, Math.min((r.width - 80) / (b.w || 1), (r.height - 80) / (b.h || 1))));
      ed.setView({ zoom: z, pan: { x: (r.width - b.w * z) / 2 - b.x * z, y: (r.height - b.h * z) / 2 - b.y * z } });
    }
    const where = () => { const r = svg.getBoundingClientRect(), v = ed.viewState(); return { x: (r.width / 2 - v.pan.x) / v.zoom - 80, y: (r.height / 2 - v.pan.y) / v.zoom - 30 }; };

    // ---- palette: every registered kind, by category, with a search; click adds at the middle of the screen, drag drops where you let go
    function drawPalette() {
      palList.textContent = ''; const by = {}, q = palSearch.value.trim().toLowerCase();
      for (const k of ed.registry.palette()) { if (q && !((k.title || k.id) + ' ' + k.id + ' ' + (k.category || '')).toLowerCase().includes(q)) continue; (by[k.category || 'other'] = by[k.category || 'other'] || []).push(k); }
      for (const c of Object.keys(by)) {
        palList.appendChild(el('h4', { text: c }));
        for (const k of by[c]) {
          const off = k.render && k.render.runnable === false, b = el('button', { text: k.title || k.id, type: 'button', draggable: 'true', title: off ? 'This kind cannot run here yet' : k.id, class: off ? 'off' : '' });
          b.onclick = () => { const w = where(), r = run({ type: 'add', kind: k.id, x: w.x, y: w.y }); if (r.ok) ed.select([r.id]); };
          b.ondragstart = (e) => e.dataTransfer.setData('text/plain', 'vr-kind:' + k.id); palList.appendChild(b);
        }
      }
      if (!palList.firstChild) palList.appendChild(el('p', { text: 'Nothing matches.', style: 'color:var(--muted,#8b93a7)' }));
    }
    palSearch.oninput = drawPalette;
    svg.addEventListener('dragover', (e) => e.preventDefault());
    svg.addEventListener('drop', (e) => { const t = e.dataTransfer.getData('text/plain'); if (!/^vr-kind:/.test(t)) return; e.preventDefault(); const p = toWorld(e), r = run({ type: 'add', kind: t.slice(8), x: p.x, y: p.y }); if (r.ok) ed.select([r.id]); });

    // ---- drawing (a full redraw per change: fine for the sizes a canvas has)
    const linkPath = (a, b) => { const dx = Math.max(40, Math.abs(b.x - a.x) / 2); return `M${a.x},${a.y} C${a.x + dx},${a.y} ${b.x - dx},${b.y} ${b.x},${b.y}`; };
    const endPos = (r, dir) => { const it = ed.item(r.item); if (!it) return null; if (r.inside) return { x: dir === 'out' ? it.x + it.w : it.x, y: it.y + ed.effH(it) / 2 }; return ed.portPos(it, dir, r.port); };
    function drawGrid() {
      gridG.textContent = ''; const v = ed.viewState(), g = (ed.grid || 20) * v.zoom; if (!(g >= 8)) return;
      const id = 've-grid-' + Math.random().toString(36).slice(2, 7);
      gridG.appendChild(sv('pattern', { id, width: g, height: g, patternUnits: 'userSpaceOnUse', x: v.pan.x % g, y: v.pan.y % g }, [sv('circle', { cx: 1, cy: 1, r: 1, fill: 'var(--line,#2a2f3a)' })]));
      gridG.appendChild(sv('rect', { width: '100%', height: '100%', fill: 'url(#' + id + ')', 'data-bg': '1' }));
    }
    const applyView = () => { const v = ed.viewState(); world.setAttribute('transform', `translate(${v.pan.x},${v.pan.y}) scale(${v.zoom})`); drawGrid(); };
    function drawCrumb() {
      crumb.textContent = ''; const bc = ed.breadcrumb();
      bc.forEach((c, i) => { if (i) crumb.appendChild(el('span', { text: '›' })); if (i === bc.length - 1) crumb.appendChild(el('b', { text: c.label })); else { const b = el('button', { text: c.label, type: 'button' }); b.onclick = () => ed.goto(c.depth); crumb.appendChild(b); } });
    }
    const isFrameKind = (it, k) => it.kind === 'group' || (k.render && k.render.shape === 'frame');
    function draw() {
      linksG.textContent = ''; itemsG.textContent = ''; const s = new Set(sel());
      for (const l of ed.linksAt()) {
        const a = endPos(l.from, 'out'), b = endPos(l.to, 'in'); if (!a || !b) continue;
        const d = linkPath(a, b), g = sv('g'); g.appendChild(sv('path', { d, class: 've-link' + (selLink === l.link.id ? ' sel' : ''), 'data-link': l.link.id })); const h = sv('path', { d, class: 've-hit' });
        h.addEventListener('pointerdown', (e) => { e.stopPropagation(); selLink = l.link.id; ed.clearSelection(); draw(); svg.focus(); });
        g.appendChild(h); linksG.appendChild(g);
      }
      const all = ed.itemsAt(), frames = all.filter((it) => isFrameKind(it, ed.kindOf(it) || {})), rest = all.filter((it) => !frames.includes(it));
      for (const it of frames.concat(rest)) {
        const k = ed.kindOf(it) || {}, frame = isFrameKind(it, k), h = ed.effH(it), st = runStates[it.id], mk = marks[it.id];
        const g = sv('g', { class: 've-node ' + (it.kind === 'group' ? 'group ' : frame ? 'frame ' : '') + (s.has(it.id) ? 'sel ' : '') + (st ? 'st-' + st.state + ' ' : '') + (k.id ? '' : 'off'), 'data-id': it.id });
        const body = sv('rect', { class: 'body', x: it.x, y: it.y, width: it.w, height: h, rx: 8 }); if (k.render && k.render.color && !s.has(it.id) && !st) body.setAttribute('style', 'stroke:' + k.render.color);
        g.appendChild(body);
        const title = (it.props && (it.props.label || it.props.text || it.props.name)) || k.title || it.kind;
        g.appendChild(sv('text', { x: it.x + 10, y: it.y + 18, text: truncate(title, Math.max(6, Math.floor(it.w / 7))) }));
        g.appendChild(sv('text', { x: it.x + 10, y: it.y + 32, class: 'sub', text: k.id ? (it.kind === 'group' ? 'group (sub-graph)' : k.title || it.kind) : it.kind + ' (unknown kind)' }));
        if (st) { const t = sv('text', { x: it.x + 10, y: it.y + h - 8, class: 'state', text: st.text || st.state }); if (st.title) t.appendChild(sv('title', { text: st.title })); g.appendChild(t); }
        if (mk) { const c = sv('circle', { class: 've-mark ' + mk.level, cx: it.x + it.w - 8, cy: it.y + 8, r: 5 }); c.appendChild(sv('title', { text: mk.text })); g.appendChild(c); }
        const ports = ed.portsOf(it);
        for (const dir of ['in', 'out']) for (const p of ports[dir]) {
          const pp = ed.portPos(it, dir, p.name); if (!pp) continue;
          const c = sv('circle', { class: 've-port' + (p.type === 'flow' ? ' flow' : ''), cx: pp.x, cy: pp.y, r: 5, 'data-port': p.name, 'data-dir': dir, 'data-item': it.id }); c.appendChild(sv('title', { text: p.name + ': ' + (p.type || 'any') + (p.many ? ' (many)' : '') + (p.required ? ' (required)' : '') })); g.appendChild(c);
          g.appendChild(sv('text', { class: 'sub', x: dir === 'in' ? pp.x + 8 : pp.x - 8, y: pp.y + 3, 'text-anchor': dir === 'in' ? 'start' : 'end', text: truncate(p.label || p.name, 14) }));
        }
        if (s.has(it.id) && s.size === 1 && k.render && k.render.resizable || (s.has(it.id) && s.size === 1 && it.kind === 'group')) g.appendChild(sv('rect', { class: 've-handle', x: it.x + it.w - 8, y: it.y + h - 8, width: 12, height: 12, rx: 2, 'data-handle': it.id }));
        itemsG.appendChild(g);
      }
      drawPreview();
    }
    function drawPreview() {
      pvG.textContent = ''; if (!preview) return;
      const pitems = {}; for (const it of preview.added || []) pitems[it.id] = it;
      for (const it of preview.added || []) { const k = ed.kindOf(it) || {}; pvG.appendChild(sv('rect', { class: 've-pv added', x: it.x, y: it.y, width: it.w, height: ed.effH(it), rx: 8 })); pvG.appendChild(sv('text', { x: it.x + 10, y: it.y + 18, class: 'sub', fill: 'currentColor', text: 'new: ' + truncate((it.props && it.props.label) || k.title || it.kind, 22) })); }
      for (const id of preview.changed || []) { const it = ed.item(id); if (it) pvG.appendChild(sv('rect', { class: 've-pv changed', x: it.x - 3, y: it.y - 3, width: it.w + 6, height: ed.effH(it) + 6, rx: 10 })); }
      for (const id of preview.removed || []) { const it = ed.item(id); if (it) pvG.appendChild(sv('rect', { class: 've-pv removed', x: it.x, y: it.y, width: it.w, height: ed.effH(it), rx: 8 })); }
      const posOf = (end, dir) => { const it = pitems[end.item] || ed.item(end.item); return it ? ed.portPos(it, dir, end.port) : null; };
      for (const l of preview.links || []) { const a = posOf(l.from, 'out'), b = posOf(l.to, 'in'); if (a && b) pvG.appendChild(sv('path', { class: 've-pvlink', d: linkPath(a, b) })); }
    }

    // ---- the property panel, from the schema (the core decides the fields; invalid values show the schema's message)
    function drawProps() {
      props.textContent = ''; const ids = sel();
      if (!ids.length) { props.appendChild(el('p', { text: selLink ? 'A wire is selected: Delete removes it.' : 'Nothing selected. Click an item, shift-click for more, or drag a box.', style: 'color:var(--muted,#8b93a7)' })); return; }
      const fields = ed.propertySpec(ids);
      props.appendChild(el('div', { text: ids.length > 1 ? ids.length + ' items' : (ed.kindOf(ed.item(ids[0])) || {}).title || ed.item(ids[0]).kind, style: 'font-weight:600' }));
      if (ids.length === 1) props.appendChild(el('div', { text: 'id ' + ids[0], style: 'font-size:11px;color:var(--muted,#8b93a7)' }));
      if (!fields.length) props.appendChild(el('p', { text: ids.length > 1 ? 'Select items of one kind to edit them together.' : 'No properties.', style: 'color:var(--muted,#8b93a7)' }));
      for (const f of fields) {
        const err = el('div', { class: 'err' }); let inp;
        const toText = (v) => (f.type === 'array' ? (v || []).map((x) => (typeof x === 'string' ? x : JSON.stringify(x))).join('\n') : f.type === 'object' ? JSON.stringify(v || {}) : v === undefined ? '' : String(v));
        if (f.enum) { inp = el('select'); if (f.mixed || !f.required) inp.appendChild(el('option', { value: '', text: f.mixed ? '(mixed)' : '' })); for (const e of f.enum) inp.appendChild(el('option', { value: String(e), text: String(e) })); inp.value = f.mixed || f.value === undefined ? '' : String(f.value); }
        else if (f.type === 'boolean') { inp = el('input', { type: 'checkbox' }); inp.checked = f.value === true; }
        else if (f.type === 'number' || f.type === 'integer') { inp = el('input', { type: 'number' }); const mn = f.schema.minimum !== undefined ? f.schema.minimum : f.schema.min, mx = f.schema.maximum !== undefined ? f.schema.maximum : f.schema.max; if (mn !== undefined) inp.min = mn; if (mx !== undefined) inp.max = mx; inp.value = f.mixed ? '' : toText(f.value); if (f.mixed) inp.placeholder = '(mixed)'; }
        else if (f.type === 'array' || f.type === 'object' || !f.schema.maxLength || f.schema.maxLength > 120) { inp = el('textarea', { rows: f.type === 'string' ? 4 : 3 }); inp.value = f.mixed ? '' : toText(f.value); if (f.mixed) inp.placeholder = '(mixed)'; }
        else { inp = el('input', { type: 'text' }); inp.value = f.mixed ? '' : toText(f.value); if (f.mixed) inp.placeholder = '(mixed)'; }
        inp.onchange = () => {
          let v;
          if (f.type === 'boolean') v = inp.checked;
          else if (f.type === 'number' || f.type === 'integer') v = inp.value === '' ? undefined : Number(inp.value);
          else if (f.type === 'array') v = inp.value.split('\n').map((x) => x.trim()).filter(Boolean).map((x) => { if (/^[{["]/.test(x)) { try { return JSON.parse(x); } catch { /* a plain line */ } } return x; });
          else if (f.type === 'object') { try { v = inp.value.trim() ? JSON.parse(inp.value) : undefined; } catch { err.textContent = 'That is not valid JSON'; inp.classList.add('bad'); return; } }
          else v = inp.value;
          const r = ed.exec({ type: 'setProp', ids, key: f.key, value: v });
          if (!r.ok) { err.textContent = r.error; inp.classList.add('bad'); } else { err.textContent = ''; inp.classList.remove('bad'); }
        };
        props.appendChild(el('label', { text: f.title + (f.required ? ' *' : '') })); props.appendChild(inp); props.appendChild(err);
      }
    }
    const refresh = () => { draw(); drawCrumb(); bUndo.disabled = !ed.history().canUndo; bRedo.disabled = !ed.history().canRedo; };

    // ---- pointer: drag items, resize, draw links, box select, pan
    let gesture = null;
    const tmp = (n) => { tmpG.textContent = ''; if (n) tmpG.appendChild(n); };
    svg.addEventListener('pointerdown', (e) => {
      svg.focus(); const p = toWorld(e), t = e.target;
      if (e.button === 1 || e.button === 2 || spaceDown) { gesture = { type: 'pan', sx: e.clientX, sy: e.clientY, pan: ed.viewState().pan }; svg.setPointerCapture(e.pointerId); return; }
      const handle = t.closest && t.closest('.ve-handle');
      if (handle) { const it = ed.item(handle.dataset.handle); gesture = { type: 'resize', id: it.id, sx: p.x, sy: p.y, w: it.w, h: it.h, key: 'resize:' + it.id + ':' + Date.now() }; ed.endGesture(); svg.setPointerCapture(e.pointerId); return; }
      const portEl = t.closest && t.closest('.ve-port');
      if (portEl && portEl.dataset.dir === 'out') { gesture = { type: 'link', item: portEl.dataset.item, port: portEl.dataset.port }; svg.setPointerCapture(e.pointerId); return; }
      const nodeEl = t.closest && t.closest('.ve-node');
      if (nodeEl) {
        const id = nodeEl.dataset.id; selLink = null;
        if (e.shiftKey) ed.select([id], 'toggle'); else if (!sel().includes(id)) ed.select([id]);
        const ids = sel().filter((x) => ed.item(x)); if (!ids.length) return;
        gesture = { type: 'drag', sx: p.x, sy: p.y, drag: ed.beginDrag(ids), moved: false }; svg.setPointerCapture(e.pointerId); return;
      }
      selLink = null; if (!e.shiftKey) ed.clearSelection(); else draw();
      gesture = { type: 'band', sx: p.x, sy: p.y, add: e.shiftKey }; svg.setPointerCapture(e.pointerId);
    });
    svg.addEventListener('pointermove', (e) => {
      if (!gesture) return; const p = toWorld(e);
      if (gesture.type === 'pan') { ed.setView({ pan: { x: gesture.pan.x + e.clientX - gesture.sx, y: gesture.pan.y + e.clientY - gesture.sy } }); return; }
      if (gesture.type === 'drag') {
        const dx = p.x - gesture.sx, dy = p.y - gesture.sy; if (!gesture.moved && Math.hypot(dx, dy) * ed.viewState().zoom < 3) return; gesture.moved = true;
        const r = gesture.drag.move(dx, dy, { grid: e.altKey ? false : undefined, items: e.altKey ? false : undefined });
        tmp(null); for (const gd of r.guides || []) tmpG.appendChild(sv('line', Object.assign({ class: 've-guide' }, gd.axis === 'x' ? { x1: gd.at, x2: gd.at, y1: -1e5, y2: 1e5 } : { y1: gd.at, y2: gd.at, x1: -1e5, x2: 1e5 })));
        return;
      }
      if (gesture.type === 'resize') { const r = ed.exec({ type: 'resize', id: gesture.id, w: gesture.w + p.x - gesture.sx, h: gesture.h + p.y - gesture.sy, snap: !e.altKey }, { mergeKey: gesture.key }); if (!r.ok) say(r.error); return; }
      if (gesture.type === 'link') { const it = ed.item(gesture.item), a = ed.portPos(it, 'out', gesture.port); tmp(sv('path', { d: linkPath(a, p), class: 've-link tmp' })); return; }
      if (gesture.type === 'band') tmp(sv('rect', { class: 've-band', x: Math.min(gesture.sx, p.x), y: Math.min(gesture.sy, p.y), width: Math.abs(p.x - gesture.sx), height: Math.abs(p.y - gesture.sy) }));
    });
    const endGesture = (e) => {
      if (!gesture) return; const g = gesture; gesture = null; const p = toWorld(e); tmp(null);
      if (g.type === 'drag') g.drag.end();
      if (g.type === 'resize') ed.endGesture();
      if (g.type === 'link') { const t = ed.portAt(p.x, p.y, 14 / ed.viewState().zoom, 'in'); if (t) run({ type: 'connect', from: { item: g.item, port: g.port }, to: { item: t.item, port: t.port } }); else draw(); }
      if (g.type === 'band') { const ids = ed.boxSelect({ x: g.sx, y: g.sy, w: p.x - g.sx, h: p.y - g.sy }, { mode: 'touch' }); if (Math.hypot(p.x - g.sx, p.y - g.sy) > 3 && (ids.length || !g.add)) ed.select(ids, g.add ? 'add' : 'set'); }
    };
    svg.addEventListener('pointerup', endGesture); svg.addEventListener('pointercancel', endGesture); svg.addEventListener('contextmenu', (e) => e.preventDefault());
    svg.addEventListener('dblclick', (e) => { const n = e.target.closest && e.target.closest('.ve-node'); if (n && ed.item(n.dataset.id).kind === 'group') ed.enter(n.dataset.id); });
    svg.addEventListener('wheel', (e) => { e.preventDefault(); const r = svg.getBoundingClientRect(); zoomBy(e.deltaY < 0 ? 1.1 : 1 / 1.1, e.clientX - r.left, e.clientY - r.top); }, { passive: false });
    svg.addEventListener('keyup', (e) => { if (e.key === ' ') spaceDown = false; });
    svg.addEventListener('keydown', (e) => {
      const ctrl = e.ctrlKey || e.metaKey, k = e.key.toLowerCase(), ids = sel(); let used = true;
      if (e.key === ' ') spaceDown = true;
      else if (ctrl && k === 'z') (e.shiftKey ? ed.redo() : ed.undo()); else if (ctrl && k === 'y') ed.redo();
      else if (ctrl && k === 'a') ed.selectAll(); else if (ctrl && k === 'c') clip = ed.copy();
      else if (ctrl && k === 'x') { if (ids.length) clip = ed.cut(); } else if (ctrl && k === 'v') { if (clip) ed.paste(clip, { dx: 20, dy: 20 }); }
      else if (ctrl && k === 'd') ed.duplicate(); else if (ctrl && k === 'g') (e.shiftKey ? doUngroup() : doGroup());
      else if (e.key === 'Delete' || e.key === 'Backspace') doDelete();
      else if (e.key.startsWith('Arrow') && ids.length) { const s = e.shiftKey ? (ed.grid || 20) : 1, d = { ArrowLeft: [-s, 0], ArrowRight: [s, 0], ArrowUp: [0, -s], ArrowDown: [0, s] }[e.key]; run({ type: 'move', ids, dx: d[0], dy: d[1] }, { mergeKey: 'key:' + ids.join() }); }
      else if (e.key === 'Enter' && ids.length === 1 && ed.item(ids[0]).kind === 'group') ed.enter(ids[0]);
      else if (e.key === 'Escape') { if (selLink) { selLink = null; draw(); } else if (ids.length) ed.clearSelection(); else ed.leave(); }
      else used = false;
      if (used) e.preventDefault();
    });

    const offs = [ed.on('change', () => { drawProps(); refresh(); }), ed.on('select', () => { selLink = null; draw(); drawProps(); }), ed.on('view', () => { applyView(); draw(); drawCrumb(); }), ed.on('history', refresh), ed.on('load', () => { drawPalette(); selLink = null; preview = null; runStates = {}; marks = {}; })];
    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(() => applyView()) : null; if (ro) ro.observe(svg);
    ed.setView({ pan: { x: 40, y: 40 }, zoom: 1 }); drawPalette(); drawProps(); refresh(); applyView();
    return {
      aiSlot: ai, element: root_, bar, say, fit, showPanel, addPanel,
      addButton: (text, title, fn) => btn(text, title, fn),
      addSeparator: () => bar.insertBefore(el('span', { class: 'sep' }), crumb),
      setRunStates: (m) => { runStates = m || {}; draw(); },
      setMarks: (m) => { marks = m || {}; draw(); },
      setPreview: (p) => { preview = p || null; drawPreview(); },
      redraw: () => { drawPalette(); refresh(); drawProps(); },
      destroy() { for (const f of offs) f(); if (ro) ro.disconnect(); root_.remove(); },
    };
  }
  root.VrEditorView = { mount };
})(typeof self !== 'undefined' ? self : this);
