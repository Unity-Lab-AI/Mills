// editor-core (T-205): the pure logic of a node / box canvas. No DOM, no tool knowledge: it runs in the browser (a
// global, `VrEditorCore`) and in Node (`require`). It holds the document model (items, ports, links, groups), commands
// with undo / redo, snapping, selection, clipboard, sub-graph view state, schema-driven property validation, patches
// (a list of commands as one undo step) and the plugin registry that tools register their item kinds in.
//
// PROVISIONAL DECISION (the editor-foundation plan, open question 11): a plain-JavaScript core first; the native app
// shows the same web module in its webview; a Rust port (pgm-editor) comes later and must pass the same fixtures (R453).
//
// The document (plain JSON, every unknown field is kept):
//   { schema: 1, seq: <id counter>, items: { id: Item }, links: { id: Link }, meta: {} }
//   Item  { id, kind, x, y, w, h, parent: null | <group id>, props: {...} }  (x, y are in the coordinates of its parent)
//   Link  { id, from: { item, port }, to: { item, port } }                  (an output port to an input port)
//   A group is an item of kind "group": its members have `parent` = the group; entering it shows them (view state only).
// A command is plain JSON ({ type: 'move', ids: [...], dx, dy }), so a patch from an AI is just a list of them.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.VrEditorCore = api;
})(typeof self !== 'undefined' ? self : this, function () {
  'use strict';

  const clone = (o) => (o === undefined ? undefined : JSON.parse(JSON.stringify(o)));
  // canonical JSON (sorted keys): two documents are the same when their canon strings are
  function canon(v) {
    if (Array.isArray(v)) return '[' + v.map(canon).join(',') + ']';
    if (v && typeof v === 'object') return '{' + Object.keys(v).filter((k) => v[k] !== undefined).sort().map((k) => JSON.stringify(k) + ':' + canon(v[k])).join(',') + '}';
    return JSON.stringify(v);
  }
  class CmdError extends Error {}
  const fail = (msg) => { throw new CmdError(msg); };

  // ---- schema subset: type, enum, min / max (minimum / maximum, minLength / maxLength, minItems / maxItems), required, pattern
  // A schema's `message` replaces the generated text, so an invalid value shows what the tool author wrote.
  function validateValue(schema, v) {
    if (!schema || v === undefined || v === null) return { ok: true };
    const bad = (def) => ({ ok: false, error: schema.message || def });
    const t = schema.type, label = schema.title || 'value';
    if (t === 'string' && typeof v !== 'string') return bad(label + ' must be text');
    if ((t === 'number' || t === 'integer') && (typeof v !== 'number' || !isFinite(v))) return bad(label + ' must be a number');
    if (t === 'integer' && !Number.isInteger(v)) return bad(label + ' must be a whole number');
    if (t === 'boolean' && typeof v !== 'boolean') return bad(label + ' must be yes or no');
    if (t === 'array' && !Array.isArray(v)) return bad(label + ' must be a list');
    if (t === 'object' && (typeof v !== 'object' || Array.isArray(v))) return bad(label + ' must be an object');
    if (Array.isArray(schema.enum) && !schema.enum.some((e) => canon(e) === canon(v))) return bad(label + ' must be one of: ' + schema.enum.join(', '));
    const min = schema.minimum !== undefined ? schema.minimum : schema.min, max = schema.maximum !== undefined ? schema.maximum : schema.max;
    if (typeof v === 'number') {
      if (min !== undefined && v < min) return bad(label + ' must be at least ' + min);
      if (max !== undefined && v > max) return bad(label + ' must be at most ' + max);
    }
    if (typeof v === 'string') {
      if (schema.minLength !== undefined && v.length < schema.minLength) return bad(label + ' must have at least ' + schema.minLength + ' characters');
      if (schema.maxLength !== undefined && v.length > schema.maxLength) return bad(label + ' must have at most ' + schema.maxLength + ' characters');
      if (schema.pattern !== undefined) { let re = null; try { re = new RegExp(schema.pattern); } catch { /* a bad pattern in a schema never blocks an edit */ } if (re && !re.test(v)) return bad(label + ' is not in the expected form'); }
    }
    if (Array.isArray(v)) {
      if (schema.minItems !== undefined && v.length < schema.minItems) return bad(label + ' needs at least ' + schema.minItems + ' entries');
      if (schema.maxItems !== undefined && v.length > schema.maxItems) return bad(label + ' allows at most ' + schema.maxItems + ' entries');
    }
    return { ok: true };
  }
  const isEmpty = (v) => v === undefined || v === null || v === '';
  // a whole props object against an object schema: { ok, errors: { key: message } }
  function validateProps(schema, props) {
    const errors = {}, p = (schema && schema.properties) || {};
    for (const k of Object.keys(p)) {
      const v = (props || {})[k];
      if (isEmpty(v)) { if ((schema.required || []).includes(k) || p[k].required === true) errors[k] = p[k].requiredMessage || (p[k].title || k) + ' is required'; continue; }
      const r = validateValue(p[k], v); if (!r.ok) errors[k] = r.error;
    }
    return { ok: !Object.keys(errors).length, errors };
  }

  // ---- the plugin registry: a tool registers its item kinds; the core only knows the built-in "group"
  // plugin = { id, title, kinds: [Kind], canConnect?(fromPort, toPort, fromItem, toItem) }
  // Kind   = { id, title, category?, size: [w, h], minSize?, ports: { in: [Port], out: [Port] } | (item, doc) => ports,
  //            schema?: { type: 'object', properties: {...}, required: [...] }, defaults?: { props }, render?: {...hints},
  //            boundary?: 'in' | 'out' (the kind is a port of the group that holds it), palette?: false, canConnect? }
  // Port   = { name, type?, many?, label? }
  const GROUP_KIND = { id: 'group', title: 'Group', category: 'core', size: [200, 120], minSize: [60, 40], palette: false, ports: { in: [], out: [] },
    schema: { type: 'object', properties: { label: { type: 'string', title: 'Label', maxLength: 80 } } }, defaults: { props: { label: 'Group' } }, render: { shape: 'group' } };
  function createRegistry() {
    const kinds = new Map(), plugins = new Map();
    const reg = {
      register(plugin) {
        if (!plugin || !plugin.id || !Array.isArray(plugin.kinds)) throw new Error('a plugin needs an id and kinds');
        if (plugins.has(plugin.id)) throw new Error('plugin already registered: ' + plugin.id);
        for (const k of plugin.kinds) if (!k.id || kinds.has(k.id)) throw new Error('kind id missing or taken: ' + (k.id || '?'));
        plugins.set(plugin.id, plugin);
        for (const k of plugin.kinds) kinds.set(k.id, Object.assign({}, k, { plugin: plugin.id }));
        return reg;
      },
      unregister(id) { if (id === 'core') return false; if (!plugins.delete(id)) return false; for (const [k, v] of [...kinds]) if (v.plugin === id) kinds.delete(k); return true; },
      kind: (id) => kinds.get(id) || null,
      kinds: () => [...kinds.values()],
      plugins: () => [...plugins.values()],
      palette: () => [...kinds.values()].filter((k) => k.palette !== false),
      // may an output of type a feed an input of type b? the kind's own rule, else its plugin's, else equal or "any"
      compatible(from, to, fi, ti) {
        const fk = kinds.get(fi.kind), p = fk && plugins.get(fk.plugin);
        const f = (fk && fk.canConnect) || (p && p.canConnect);
        if (f) return f(from, to, fi, ti) !== false;
        const a = from.type || 'any', b = to.type || 'any';
        return a === b || a === 'any' || b === 'any';
      },
    };
    reg.register({ id: 'core', title: 'Editor core', kinds: [GROUP_KIND] });
    return reg;
  }

  // ---- geometry helpers
  const rectOf = (it) => ({ x: it.x, y: it.y, w: it.w, h: it.h });
  const hit = (r, x, y) => x >= r.x && y >= r.y && x <= r.x + r.w && y <= r.y + r.h;
  const bboxOf = (items) => { if (!items.length) return null; let x1 = Infinity, y1 = Infinity, x2 = -Infinity, y2 = -Infinity; for (const i of items) { x1 = Math.min(x1, i.x); y1 = Math.min(y1, i.y); x2 = Math.max(x2, i.x + i.w); y2 = Math.max(y2, i.y + i.h); } return { x: x1, y: y1, w: x2 - x1, h: y2 - y1 }; };
  const snapTo = (v, g) => (g > 0 ? Math.round(v / g) * g : v);

  function newDoc() { return { schema: 1, seq: 0, items: {}, links: {}, meta: {} }; }
  function normalizeDoc(d) {
    const doc = Object.assign(newDoc(), clone(d || {}));
    doc.items = doc.items || {}; doc.links = doc.links || {}; doc.meta = doc.meta || {};
    let max = Number(doc.seq) || 0;
    for (const id of Object.keys(doc.items)) { const it = doc.items[id]; it.id = id; if (it.parent === undefined) it.parent = null; it.props = it.props || {}; const m = /(\d+)$/.exec(id); if (m) max = Math.max(max, Number(m[1])); }
    for (const id of Object.keys(doc.links)) { doc.links[id].id = id; const m = /(\d+)$/.exec(id); if (m) max = Math.max(max, Number(m[1])); }
    doc.seq = max;
    return doc;
  }

  function createEditor(opts) {
    opts = opts || {};
    const registry = opts.registry || createRegistry();
    let doc = normalizeDoc(opts.doc);
    const ed = { registry, grid: opts.grid === undefined ? 20 : opts.grid, snapDist: opts.snapDist || 6, idPrefix: opts.idPrefix || 'i', linkPrefix: opts.linkPrefix || 'l', rootLabel: opts.rootLabel || 'Top' };
    const undoStack = [], redoStack = [];
    let selection = [], path = [], openKey = null, dragN = 0;
    const handlers = {};
    const emit = (type, data) => { for (const f of (handlers[type] || []).slice()) { try { f(data); } catch (e) { if (typeof console !== 'undefined') console.error(e); } } };
    ed.on = (type, f) => { (handlers[type] = handlers[type] || []).push(f); return () => { handlers[type] = (handlers[type] || []).filter((x) => x !== f); }; };

    Object.defineProperty(ed, 'doc', { get: () => doc });
    ed.toJSON = () => clone(doc);
    // `meta` is the tool's own part of the document (a document's name, description ...): settable, not part of the history
    ed.meta = (key) => clone(doc.meta[key]);
    ed.setMeta = (key, value) => { if (canon(doc.meta[key]) === canon(value)) return false; doc.meta[key] = clone(value); emit('change', { kind: 'meta', key }); return true; };
    ed.item = (id) => doc.items[id] || null;
    ed.kindOf = (it) => registry.kind(it.kind);
    ed.level = () => (path.length ? path[path.length - 1] : null);
    ed.itemsAt = (parent) => Object.values(doc.items).filter((i) => (i.parent || null) === (parent === undefined ? ed.level() : parent));
    ed.children = (id) => Object.values(doc.items).filter((i) => i.parent === id);
    ed.descendants = (id) => { const out = []; const walk = (p) => { for (const c of ed.children(p)) { out.push(c.id); walk(c.id); } }; walk(id); return out; };
    const expand = (ids) => { const s = new Set(); for (const id of ids) if (doc.items[id]) { s.add(id); for (const d of ed.descendants(id)) s.add(d); } return s; };
    // drop ids whose ancestor is also listed (they travel with it)
    const topOnly = (ids) => { const set = new Set(ids); return ids.filter((id) => { let p = doc.items[id] && doc.items[id].parent; while (p) { if (set.has(p)) return false; p = doc.items[p] && doc.items[p].parent; } return true; }); };

    // ports: the kind's own; a group's come from the boundary items inside it (so a group is a box with the ports of what it holds)
    ed.portsOf = (it) => {
      const k = registry.kind(it.kind);
      if (!k) return { in: [], out: [] };
      let p = typeof k.ports === 'function' ? k.ports(it, doc) : k.ports || {};
      p = { in: (p.in || []).slice(), out: (p.out || []).slice() };
      if (it.kind === 'group') for (const c of ed.children(it.id)) { const ck = registry.kind(c.kind); if (ck && ck.boundary && c.props && c.props.name) (ck.boundary === 'in' ? p.in : p.out).push({ name: c.props.name, type: c.props.type || 'any' }); }
      return p;
    };
    // where a port sits (in the item's own coordinates): inputs on the left edge, outputs on the right, evenly spaced
    // the height an item is drawn at: its own, or more when its ports would not fit (a kind with many ports, or ports that depend on its settings)
    ed.effH = (it) => { const p = ed.portsOf(it); return Math.max(it.h, 22 * (Math.max(p.in.length, p.out.length) + 1)); };
    ed.portPos = (it, dir, name) => { const l = ed.portsOf(it)[dir]; const i = l.findIndex((p) => p.name === name); if (i < 0) return null; return { x: dir === 'in' ? it.x : it.x + it.w, y: it.y + ((i + 1) * ed.effH(it)) / (l.length + 1) }; };
    ed.portAt = (x, y, radius, dir) => { // nearest port within radius among the current level's items
      let best = null;
      for (const it of ed.itemsAt()) for (const d of dir ? [dir] : ['in', 'out']) for (const p of ed.portsOf(it)[d]) { const pp = ed.portPos(it, d, p.name), dist = Math.hypot(pp.x - x, pp.y - y); if (dist <= radius && (!best || dist < best.dist)) best = { item: it.id, port: p.name, dir: d, dist, x: pp.x, y: pp.y }; }
      return best;
    };
    const isFrame = (it) => { const k = registry.kind(it.kind); return it.kind === 'group' || !!(k && k.render && k.render.shape === 'frame'); };
    // the topmost item at a point; a frame (group, section) only when nothing else is there, so what sits inside a frame stays reachable
    ed.hitTest = (x, y, parent) => {
      const l = ed.itemsAt(parent); let frame = null;
      for (let i = l.length - 1; i >= 0; i--) { const r = rectOf(l[i]); r.h = ed.effH(l[i]); if (!hit(r, x, y)) continue; if (isFrame(l[i])) frame = frame || l[i].id; else return l[i].id; }
      return frame;
    };
    ed.boxSelect = (rect, o) => { // rect {x,y,w,h} (any corner order); mode 'contain' (default) or 'touch'
      o = o || {}; const r = { x: Math.min(rect.x, rect.x + rect.w), y: Math.min(rect.y, rect.y + rect.h), w: Math.abs(rect.w), h: Math.abs(rect.h) };
      return ed.itemsAt(o.parent).filter((i) => (o.mode === 'touch'
        ? i.x <= r.x + r.w && i.x + i.w >= r.x && i.y <= r.y + r.h && i.y + i.h >= r.y
        : i.x >= r.x && i.y >= r.y && i.x + i.w <= r.x + r.w && i.y + i.h <= r.y + r.h)).map((i) => i.id);
    };
    // the links to draw at a level: an end inside a closed group is drawn at the group's box; links inside one group are hidden
    ed.linksAt = (parent) => {
      parent = parent === undefined ? ed.level() : parent; const out = [];
      const up = (id) => { let it = doc.items[id], inside = false; while (it && (it.parent || null) !== parent) { it = it.parent ? doc.items[it.parent] : null; inside = true; } return it ? { item: it.id, inside } : null; };
      for (const l of Object.values(doc.links)) { const a = up(l.from.item), b = up(l.to.item); if (a && b && !(a.item === b.item)) out.push({ link: l, from: { item: a.item, port: l.from.port, inside: a.inside }, to: { item: b.item, port: l.to.port, inside: b.inside } }); }
      return out;
    };

    // ---- selection (view state: never in the history)
    ed.selection = () => selection.slice();
    ed.select = (ids, mode) => {
      ids = (Array.isArray(ids) ? ids : [ids]).filter((id) => doc.items[id]);
      if (mode === 'add') selection = [...new Set([...selection, ...ids])];
      else if (mode === 'toggle') { const s = new Set(selection); for (const id of ids) (s.has(id) ? s.delete(id) : s.add(id)); selection = [...s]; }
      else selection = [...new Set(ids)];
      emit('select', selection.slice());
    };
    ed.selectAll = () => ed.select(ed.itemsAt().map((i) => i.id));
    ed.clearSelection = () => ed.select([]);
    const pruneState = () => {
      selection = selection.filter((id) => doc.items[id]);
      let ok = 0; while (ok < path.length && doc.items[path[ok]] && doc.items[path[ok]].kind === 'group' && (doc.items[path[ok]].parent || null) === (ok ? path[ok - 1] : null)) ok++;
      if (ok < path.length) { path = path.slice(0, ok); emit('view', ed.viewState()); }
    };

    // ---- the view: sub-graph enter / leave, pan, zoom. Not the document: nothing here touches the history.
    const pan = { x: 0, y: 0 }; let zoom = 1;
    ed.viewState = () => ({ path: path.slice(), pan: { x: pan.x, y: pan.y }, zoom });
    ed.setView = (v) => { if (v.pan) { pan.x = v.pan.x; pan.y = v.pan.y; } if (v.zoom) zoom = Math.min(8, Math.max(0.1, v.zoom)); emit('view', ed.viewState()); };
    ed.enter = (id) => { const it = doc.items[id]; if (!it || it.kind !== 'group' || (it.parent || null) !== ed.level()) return false; path.push(id); selection = []; emit('view', ed.viewState()); emit('select', []); return true; };
    ed.leave = () => { if (!path.length) return false; const was = path.pop(); selection = [was]; emit('view', ed.viewState()); emit('select', selection.slice()); return true; };
    ed.goto = (depth) => { if (depth < 0 || depth > path.length) return false; path = path.slice(0, depth); selection = []; emit('view', ed.viewState()); emit('select', []); return true; };
    ed.breadcrumb = () => [{ id: null, label: ed.rootLabel, depth: 0 }].concat(path.map((id, i) => ({ id, label: (doc.items[id].props && doc.items[id].props.label) || id, depth: i + 1 })));

    // ---- transactions: every write goes through one, which remembers each touched item / link as it was
    const newTx = () => ({ items: new Map(), links: new Map(), seq: doc.seq });
    const touchI = (tx, id) => { if (!tx.items.has(id)) tx.items.set(id, clone(doc.items[id])); };
    const touchL = (tx, id) => { if (!tx.links.has(id)) tx.links.set(id, clone(doc.links[id])); };
    const putItem = (tx, it) => { touchI(tx, it.id); doc.items[it.id] = it; };
    const dropItem = (tx, id) => { touchI(tx, id); delete doc.items[id]; };
    const putLink = (tx, l) => { touchL(tx, l.id); doc.links[l.id] = l; };
    const dropLink = (tx, id) => { touchL(tx, id); delete doc.links[id]; };
    const patchItem = (tx, id, f) => { touchI(tx, id); f(doc.items[id]); };
    const rollback = (tx) => { for (const [id, b] of tx.items) { if (b === undefined) delete doc.items[id]; else doc.items[id] = clone(b); } for (const [id, b] of tx.links) { if (b === undefined) delete doc.links[id]; else doc.links[id] = clone(b); } doc.seq = tx.seq; };
    const finish = (tx) => { // the entry (before / after per touched thing), or null when nothing really changed
      const e = { items: {}, links: {}, seq: [tx.seq, doc.seq] }; let n = 0;
      for (const [id, b] of tx.items) { const a = clone(doc.items[id]); if (canon(a) !== canon(b)) { e.items[id] = { b, a }; n++; } }
      for (const [id, b] of tx.links) { const a = clone(doc.links[id]); if (canon(a) !== canon(b)) { e.links[id] = { b, a }; n++; } }
      return n ? e : null;
    };
    const nextId = (prefix) => { let id; do { doc.seq++; id = prefix + doc.seq; } while (doc.items[id] || doc.links[id]); return id; };
    const num = (v, what) => { if (typeof v !== 'number' || !isFinite(v)) fail(what + ' must be a number'); return v; };
    const needItems = (ids) => { if (!Array.isArray(ids) || !ids.length) fail('no items given'); for (const id of ids) if (!doc.items[id]) fail('no such item: ' + id); return ids; };

    // ---- snapping: to other items' edges / centres on the same level first, then the grid
    ed.snapShift = (ids, dx, dy, o) => {
      o = o || {}; const start = o.start || {}, set = new Set(expand(ids));
      const moving = topOnly(ids).map((id) => { const it = doc.items[id], s = start[id]; return { x: s ? s[0] : it.x, y: s ? s[1] : it.y, w: it.w, h: it.h }; });
      const b = bboxOf(moving); if (!b) return { dx, dy, guides: [], snapped: {} };
      const first = doc.items[topOnly(ids)[0]], others = ed.itemsAt(first.parent || null).filter((i) => !set.has(i.id));
      const out = { dx, dy, guides: [], snapped: {} }, dist = o.dist === undefined ? ed.snapDist : o.dist, grid = o.grid === undefined ? ed.grid : o.grid;
      const axis = (ax) => {
        const pos = ax === 'x' ? b.x + dx : b.y + dy, size = ax === 'x' ? b.w : b.h, mine = [pos, pos + size / 2, pos + size];
        let best = null;
        if (o.items !== false) for (const it of others) { const p = ax === 'x' ? it.x : it.y, s = ax === 'x' ? it.w : it.h; for (const c of [p, p + s / 2, p + s]) for (const m of mine) { const d = c - m; if (Math.abs(d) <= dist && (!best || Math.abs(d) < Math.abs(best.d))) best = { d, at: c }; } }
        if (best) { if (ax === 'x') out.dx += best.d; else out.dy += best.d; out.guides.push({ axis: ax, at: best.at }); out.snapped[ax] = 'item'; }
        else if (grid > 0 && o.grid !== false) { const d = snapTo(pos, grid) - pos; if (ax === 'x') out.dx += d; else out.dy += d; out.snapped[ax] = 'grid'; }
      };
      axis('x'); axis('y');
      return out;
    };

    // ---- commands. Each gets (tx, cmd), changes the document through the tx helpers, and throws CmdError to refuse.
    const parseEnd = (e, what) => { if (typeof e === 'string') { const i = e.indexOf('.'); if (i < 0) fail(what + ' must be "item.port"'); return { item: e.slice(0, i), port: e.slice(i + 1) }; } if (!e || !e.item || !e.port) fail(what + ' needs an item and a port'); return { item: e.item, port: e.port }; };
    const cmds = {
      add(tx, c) {
        const k = registry.kind(c.kind); if (!k) fail('unknown item kind: ' + c.kind);
        if (c.id !== undefined && (doc.items[c.id] || doc.links[c.id])) fail('id already used: ' + c.id);
        const parent = c.parent === undefined ? ed.level() : c.parent;
        if (parent && !(doc.items[parent] && doc.items[parent].kind === 'group')) fail('parent is not a group: ' + parent);
        const id = c.id || nextId(ed.idPrefix);
        const props = Object.assign({}, clone((k.defaults && k.defaults.props) || {}), clone(c.props || {}));
        const size = (typeof k.size === 'function' ? k.size(props) : k.size) || [120, 60];
        if (k.schema) { const v = validateProps(k.schema, props); const bad = Object.keys(v.errors).filter((x) => !isEmpty(props[x])); if (bad.length) fail(v.errors[bad[0]]); }
        const g = c.snap === false ? 0 : ed.grid;
        putItem(tx, { id, kind: c.kind, x: snapTo(num(c.x === undefined ? 0 : c.x, 'x'), g), y: snapTo(num(c.y === undefined ? 0 : c.y, 'y'), g), w: c.w || size[0], h: c.h || size[1], parent: parent || null, props });
        return { id };
      },
      move(tx, c) {
        needItems(c.ids); num(c.dx, 'dx'); num(c.dy, 'dy'); let dx = c.dx, dy = c.dy;
        if (c.snap) { const s = ed.snapShift(c.ids, dx, dy); dx = s.dx; dy = s.dy; }
        for (const id of topOnly(c.ids)) patchItem(tx, id, (it) => { it.x += dx; it.y += dy; });
        return { dx, dy };
      },
      place(tx, c) { // absolute positions: { positions: { id: [x, y] } } (what a drag sends, so a drag can be merged)
        const p = c.positions || {}; needItems(Object.keys(p));
        for (const id of Object.keys(p)) patchItem(tx, id, (it) => { it.x = num(p[id][0], 'x'); it.y = num(p[id][1], 'y'); });
      },
      resize(tx, c) {
        const it = doc.items[c.id]; if (!it) fail('no such item: ' + c.id);
        const k = registry.kind(it.kind) || {}, min = k.minSize || [20, 20], g = c.snap ? ed.grid : 0;
        const x = c.x === undefined ? it.x : snapTo(num(c.x, 'x'), g), y = c.y === undefined ? it.y : snapTo(num(c.y, 'y'), g);
        const w = c.w === undefined ? it.w : snapTo(num(c.w, 'w'), g), h = c.h === undefined ? it.h : snapTo(num(c.h, 'h'), g);
        if (w < min[0] || h < min[1]) fail('too small: at least ' + min[0] + ' x ' + min[1]);
        patchItem(tx, c.id, (i) => { i.x = x; i.y = y; i.w = w; i.h = h; });
      },
      delete(tx, c) {
        needItems(c.ids); const all = expand(c.ids);
        for (const l of Object.values(doc.links)) if (all.has(l.from.item) || all.has(l.to.item)) dropLink(tx, l.id);
        for (const id of all) dropItem(tx, id);
      },
      connect(tx, c) {
        const from = parseEnd(c.from, 'from'), to = parseEnd(c.to, 'to'), a = doc.items[from.item], b = doc.items[to.item];
        if (!a || !b) fail('no such item: ' + (a ? to.item : from.item));
        if (a.id === b.id) fail('an item cannot connect to itself');
        if (!registry.kind(a.kind) || !registry.kind(b.kind)) fail('unknown item kind');
        if ((a.parent || null) !== (b.parent || null)) fail('items in different groups cannot be connected: enter the group');
        const fp = ed.portsOf(a).out.find((p) => p.name === from.port), tp = ed.portsOf(b).in.find((p) => p.name === to.port);
        if (!fp) fail(a.id + ' has no output "' + from.port + '"');
        if (!tp) fail(b.id + ' has no input "' + to.port + '"');
        for (const l of Object.values(doc.links)) {
          if (l.from.item === a.id && l.from.port === from.port && l.to.item === b.id && l.to.port === to.port) fail('already connected');
          if (!tp.many && l.to.item === b.id && l.to.port === to.port) fail('input "' + to.port + '" of ' + b.id + ' takes one link only');
        }
        if (!registry.compatible(fp, tp, a, b)) fail('"' + (fp.type || 'any') + '" does not fit "' + (tp.type || 'any') + '"');
        if (c.id !== undefined && (doc.items[c.id] || doc.links[c.id])) fail('id already used: ' + c.id);
        const id = c.id || nextId(ed.linkPrefix);
        putLink(tx, { id, from, to }); return { id };
      },
      disconnect(tx, c) {
        let found = 0;
        for (const l of Object.values(doc.links)) {
          const m = c.id ? l.id === c.id
            : c.item ? (l.from.item === c.item || l.to.item === c.item) && (!c.port || l.from.port === c.port || l.to.port === c.port)
              : c.from && c.to ? (() => { const f = parseEnd(c.from, 'from'), t = parseEnd(c.to, 'to'); return l.from.item === f.item && l.from.port === f.port && l.to.item === t.item && l.to.port === t.port; })() : false;
          if (m) { dropLink(tx, l.id); found++; }
        }
        if (!found) fail('no such link');
      },
      group(tx, c) {
        needItems(c.ids); const ids = topOnly(c.ids), par = new Set(ids.map((id) => doc.items[id].parent || null));
        if (par.size > 1) fail('the items are in different groups');
        const parent = [...par][0], b = bboxOf(ids.map((id) => doc.items[id])), pad = c.pad === undefined ? 16 : c.pad;
        if (c.id !== undefined && (doc.items[c.id] || doc.links[c.id])) fail('id already used: ' + c.id);
        const id = c.id || nextId(ed.idPrefix), k = GROUP_KIND;
        putItem(tx, { id, kind: 'group', x: b.x - pad, y: b.y - pad, w: b.w + 2 * pad, h: b.h + 2 * pad, parent, props: Object.assign({}, clone(k.defaults.props), c.props || {}) });
        for (const m of ids) patchItem(tx, m, (it) => { it.parent = id; });
        return { id };
      },
      ungroup(tx, c) {
        needItems(c.ids);
        for (const gid of c.ids) {
          const g = doc.items[gid]; if (g.kind !== 'group') fail(gid + ' is not a group');
          for (const m of ed.children(gid)) patchItem(tx, m.id, (it) => { it.parent = g.parent || null; });
          for (const l of Object.values(doc.links)) if (l.from.item === gid || l.to.item === gid) dropLink(tx, l.id); // links to the group's own ports go with it
          dropItem(tx, gid);
        }
      },
      setProp(tx, c) {
        needItems(c.ids); const set = c.props || { [c.key]: c.value };
        if (!c.props && (typeof c.key !== 'string' || !c.key)) fail('no property given');
        const errs = {};
        for (const id of c.ids) {
          const it = doc.items[id], k = registry.kind(it.kind), schema = k && k.schema;
          for (const key of Object.keys(set)) {
            const ps = schema && schema.properties && schema.properties[key];
            if (!ps && schema && schema.additionalProperties === false) { errs[key] = 'no such property: ' + key; continue; }
            if (isEmpty(set[key])) { if (schema && ((schema.required || []).includes(key) || (ps && ps.required === true))) errs[key] = (ps && ps.requiredMessage) || ((ps && ps.title) || key) + ' is required'; continue; }
            const r = validateValue(ps, set[key]); if (!r.ok) errs[key] = r.error;
          }
        }
        const bad = Object.keys(errs); if (bad.length) { const e = new CmdError(errs[bad[0]]); e.errors = errs; e.path = bad[0]; throw e; }
        for (const id of c.ids) patchItem(tx, id, (it) => { it.props = it.props || {}; for (const key of Object.keys(set)) { if (isEmpty(set[key])) delete it.props[key]; else it.props[key] = clone(set[key]); } });
      },
      align(tx, c) {
        needItems(c.ids); const its = topOnly(c.ids).map((id) => doc.items[id]), m = c.mode;
        if (its.length < 2) fail('align needs two or more items');
        const b = bboxOf(its);
        if (m === 'distribute-h' || m === 'distribute-v') {
          if (its.length < 3) fail('distribute needs three or more items');
          const h = m === 'distribute-h', key = h ? 'x' : 'y', sz = h ? 'w' : 'h', sorted = its.slice().sort((p, q) => p[key] - q[key]);
          const total = sorted.reduce((s, i) => s + i[sz], 0), gap = ((h ? b.w : b.h) - total) / (sorted.length - 1); let at = h ? b.x : b.y;
          for (const i of sorted) { const v = at; patchItem(tx, i.id, (it) => { it[key] = v; }); at += i[sz] + gap; }
          return;
        }
        const f = { left: (i) => ({ x: b.x }), right: (i) => ({ x: b.x + b.w - i.w }), center: (i) => ({ x: b.x + (b.w - i.w) / 2 }), top: () => ({ y: b.y }), bottom: (i) => ({ y: b.y + b.h - i.h }), middle: (i) => ({ y: b.y + (b.h - i.h) / 2 }) }[m];
        if (!f) fail('unknown align mode: ' + m);
        for (const i of its) patchItem(tx, i.id, (it) => Object.assign(it, f(i)));
      },
      paste(tx, c) {
        const clip = c.clip; if (!clip || clip.format !== 'vr-editor-clip' || !Array.isArray(clip.items)) fail('nothing to paste');
        const parent = c.parent === undefined ? ed.level() : c.parent;
        if (parent && !(doc.items[parent] && doc.items[parent].kind === 'group')) fail('parent is not a group: ' + parent);
        const map = {}, inClip = new Set(clip.items.map((i) => i.id)), dx = c.dx === undefined ? ed.grid : c.dx, dy = c.dy === undefined ? ed.grid : c.dy;
        for (const it of clip.items) map[it.id] = nextId(ed.idPrefix);
        const top = [];
        for (const it of clip.items) {
          const n = clone(it); n.id = map[it.id];
          if (it.parent && inClip.has(it.parent)) n.parent = map[it.parent]; else { n.parent = parent || null; n.x += dx; n.y += dy; top.push(n.id); }
          putItem(tx, n);
        }
        const lm = {};
        for (const l of clip.links || []) if (inClip.has(l.from.item) && inClip.has(l.to.item)) { const id = nextId(ed.linkPrefix); lm[l.id] = id; putLink(tx, { id, from: { item: map[l.from.item], port: l.from.port }, to: { item: map[l.to.item], port: l.to.port } }); }
        return { ids: top, map, links: lm };
      },
    };
    ed.commandTypes = () => Object.keys(cmds);

    const apply = (tx, c) => { if (!c || typeof c.type !== 'string' || !Object.prototype.hasOwnProperty.call(cmds, c.type)) fail('unknown command: ' + (c && c.type)); return cmds[c.type](tx, c) || {}; };
    const pushHistory = (e, mergeKey) => {
      redoStack.length = 0;
      const last = undoStack[undoStack.length - 1];
      if (mergeKey && last && last.mergeKey === mergeKey && openKey === mergeKey) { // a continuous gesture: one step (first "before", last "after")
        for (const id of Object.keys(e.items)) last.items[id] = last.items[id] ? { b: last.items[id].b, a: e.items[id].a } : e.items[id];
        for (const id of Object.keys(e.links)) last.links[id] = last.links[id] ? { b: last.links[id].b, a: e.links[id].a } : e.links[id];
        last.seq = [last.seq[0], e.seq[1]];
      } else { if (mergeKey) e.mergeKey = mergeKey; undoStack.push(e); }
      openKey = mergeKey || null;
    };
    const written = (list) => { const t = {}; for (const c of list) t[c.type] = 1; return Object.keys(t); };
    // exec: one command, one undo step (unless opts.mergeKey joins it to the previous step of the same gesture)
    ed.exec = (cmd, o) => {
      o = o || {}; const tx = newTx(); let r;
      try { r = apply(tx, cmd); } catch (e) { rollback(tx); if (e instanceof CmdError) return { ok: false, error: e.message, path: e.path, errors: e.errors }; throw e; }
      const e = finish(tx); if (!e) return Object.assign({ ok: true, changed: false }, r);
      pushHistory(e, o.mergeKey); pruneState();
      emit('change', { cmds: [cmd], kind: 'do' }); emit('history', ed.history());
      return Object.assign({ ok: true, changed: true }, r);
    };
    // applyPatch: a list of commands as ONE undo step; if any fails, everything is rolled back and nothing is recorded
    ed.applyPatch = (list, o) => {
      o = o || {}; if (!Array.isArray(list) || !list.length) return { ok: false, error: 'an empty patch', index: -1 };
      const tx = newTx(), results = [];
      for (let i = 0; i < list.length; i++) {
        try { results.push(apply(tx, list[i])); } catch (e) { rollback(tx); if (e instanceof CmdError) return { ok: false, error: e.message, index: i, path: e.path, errors: e.errors }; throw e; }
      }
      const e = finish(tx); if (!e) return { ok: true, changed: false, results };
      if (o.dryRun) { rollback(tx); return { ok: true, changed: true, results, dryRun: true }; }
      pushHistory(e, null); pruneState();
      emit('change', { cmds: list.slice(), kind: 'patch' }); emit('history', ed.history());
      return { ok: true, changed: true, results };
    };
    // proposePatch: try a patch on a scratch copy; the real document and history are untouched until accept()
    ed.proposePatch = (list) => {
      const scratch = createEditor({ registry, doc, grid: ed.grid, idPrefix: ed.idPrefix, linkPrefix: ed.linkPrefix }), r = scratch.applyPatch(list);
      let settled = false;
      return { ok: r.ok, error: r.error, index: r.index, preview: r.ok ? scratch.toJSON() : null, summary: r.ok ? written(list) : [],
        accept() { if (settled) return { ok: false, error: 'already settled' }; settled = true; return r.ok ? ed.applyPatch(list) : r; },
        reject() { settled = true; return { ok: true }; } };
    };
    // a drag (or resize) is a gesture: every move is a command, but the history sees one step
    ed.beginDrag = (ids, o) => {
      o = o || {}; ids = topOnly(needItems(ids)); const key = 'drag:' + (++dragN), start = {}; for (const id of ids) start[id] = [doc.items[id].x, doc.items[id].y];
      openKey = null;
      return {
        move(dx, dy, so) {
          const s = o.snap === false ? { dx, dy, guides: [], snapped: {} } : ed.snapShift(ids, dx, dy, Object.assign({ start }, so || {})), positions = {};
          for (const id of ids) positions[id] = [start[id][0] + s.dx, start[id][1] + s.dy];
          const r = ed.exec({ type: 'place', positions }, { mergeKey: key }); return Object.assign({}, s, { ok: r.ok });
        },
        end() { openKey = null; },
      };
    };
    ed.endGesture = () => { openKey = null; };

    const applyEntry = (e, dir) => {
      for (const id of Object.keys(e.items)) { const v = clone(e.items[id][dir]); if (v === undefined) delete doc.items[id]; else doc.items[id] = v; }
      for (const id of Object.keys(e.links)) { const v = clone(e.links[id][dir]); if (v === undefined) delete doc.links[id]; else doc.links[id] = v; }
      doc.seq = dir === 'b' ? e.seq[0] : e.seq[1];
    };
    ed.undo = () => { const e = undoStack.pop(); if (!e) return false; applyEntry(e, 'b'); redoStack.push(e); openKey = null; pruneState(); emit('change', { kind: 'undo' }); emit('history', ed.history()); return true; };
    ed.redo = () => { const e = redoStack.pop(); if (!e) return false; applyEntry(e, 'a'); undoStack.push(e); openKey = null; pruneState(); emit('change', { kind: 'redo' }); emit('history', ed.history()); return true; };
    ed.history = () => ({ undo: undoStack.length, redo: redoStack.length, canUndo: undoStack.length > 0, canRedo: redoStack.length > 0 });

    // ---- clipboard: a clip is plain JSON, so it can be pasted into another document (or editor)
    ed.copy = (ids) => {
      ids = ids || selection; const all = expand(ids), items = [...all].map((id) => clone(doc.items[id])), b = bboxOf(topOnly(ids).map((id) => doc.items[id]));
      return { format: 'vr-editor-clip', v: 1, origin: b ? { x: b.x, y: b.y } : { x: 0, y: 0 }, items, links: Object.values(doc.links).filter((l) => all.has(l.from.item) && all.has(l.to.item)).map(clone) };
    };
    ed.cut = (ids) => { ids = ids || selection; const clip = ed.copy(ids); const r = ed.exec({ type: 'delete', ids: ids.slice() }); return r.ok ? clip : null; };
    ed.paste = (clip, o) => { const r = ed.exec(Object.assign({ type: 'paste', clip }, o || {})); if (r.ok && r.ids) ed.select(r.ids); return r; };
    ed.duplicate = (ids) => ed.paste(ed.copy(ids || selection));

    // ---- property panel: the fields for the selection, from the kind's json schema (the form itself is the view's job)
    ed.propertySpec = (ids) => {
      ids = (ids || selection).filter((id) => doc.items[id]); if (!ids.length) return [];
      const kinds = [...new Set(ids.map((id) => doc.items[id].kind))]; if (kinds.length !== 1) return [];
      const k = registry.kind(kinds[0]); if (!k || !k.schema || !k.schema.properties) return [];
      return Object.keys(k.schema.properties).map((key) => {
        const s = k.schema.properties[key], vals = ids.map((id) => doc.items[id].props[key]), same = vals.every((v) => canon(v) === canon(vals[0]));
        return { key, schema: s, title: s.title || key, type: s.type || 'string', enum: s.enum || null, required: (k.schema.required || []).includes(key) || s.required === true, value: same ? vals[0] : undefined, mixed: !same };
      });
    };
    ed.validateItem = (id) => { const it = doc.items[id], k = it && registry.kind(it.kind); return k && k.schema ? validateProps(k.schema, it.props) : { ok: true, errors: {} }; };

    // ---- persistence: the foundation saves nothing itself; a tool gives it a store { load(key) -> doc, save(key, doc) }
    ed.load = (d) => { doc = normalizeDoc(d); undoStack.length = 0; redoStack.length = 0; selection = []; path = []; openKey = null; emit('load', {}); emit('change', { kind: 'load' }); emit('history', ed.history()); emit('view', ed.viewState()); emit('select', []); };
    ed.save = (store, key) => Promise.resolve(store.save(key, ed.toJSON()));
    ed.open = (store, key) => Promise.resolve(store.load(key)).then((d) => { ed.load(d); return d; });
    ed.autosave = (fn, o) => { // fn(doc) after a quiet delay when the document changed; returns a stop function
      let t = null; const ms = (o && o.delay) || 800;
      const off = ed.on('change', () => { if (typeof setTimeout === 'undefined') return; clearTimeout(t); t = setTimeout(() => fn(ed.toJSON()), ms); });
      return () => { off(); if (typeof clearTimeout !== 'undefined') clearTimeout(t); };
    };
    return ed;
  }

  // stores: a Map-backed one for tests and a wrapper around any localStorage-like object (the object is passed in)
  const memoryStore = () => { const m = new Map(); return { load: (k) => (m.has(k) ? clone(m.get(k)) : Promise.reject(new Error('not found: ' + k))), save: (k, d) => { m.set(k, clone(d)); }, keys: () => [...m.keys()] }; };
  const storageStore = (storage, prefix) => ({ load: (k) => { const s = storage.getItem((prefix || 'vr-editor:') + k); return s ? JSON.parse(s) : Promise.reject(new Error('not found: ' + k)); }, save: (k, d) => { storage.setItem((prefix || 'vr-editor:') + k, JSON.stringify(d)); } });

  // what differs between two documents: item and link ids added, removed or changed (a patch preview is drawn from this)
  function diffDocs(a, b) {
    const out = { items: { added: [], removed: [], changed: [] }, links: { added: [], removed: [], changed: [] } };
    for (const k of ['items', 'links']) {
      for (const id of Object.keys(b[k] || {})) { if (!(a[k] || {})[id]) out[k].added.push(id); else if (canon(a[k][id]) !== canon(b[k][id])) out[k].changed.push(id); }
      for (const id of Object.keys(a[k] || {})) if (!(b[k] || {})[id]) out[k].removed.push(id);
    }
    return out;
  }

  return { version: '1.0.0', diffDocs, createEditor, createRegistry, newDoc, normalizeDoc, validateValue, validateProps, canon, clone, sameDoc: (a, b) => canon(a) === canon(b), memoryStore, storageStore, bboxOf, snapTo, GROUP_KIND };
});
