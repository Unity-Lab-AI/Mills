// Plugin for the editor foundation (T-205): the Hyper Workflows tool (plans/hyper-workflows.md sections 3, 5, 12, 13, 14, 15).
// The core (core.js) knows nothing of this file: `install(registry, manifests)` is the only contact. Everything here is
// logic with no DOM, so it runs in the page and in Node (the suite):
//   - the node kinds: the manifests the hub gives (GET /api/hyperwork/kinds: built-ins and custom kinds, with trust),
//     or the static copy below when the hub is out of reach; ports that depend on a node's settings (switch cases,
//     jev choices, script outputs ...) follow the same rules as the library's DefaultRegistry;
//   - fromWorkflow / toWorkflow: the section 13 document <-> the editor's document (unknown fields are kept);
//   - client(request): load, save a draft, validate, publish, delete, duplicate, test-run a draft, the run log, cancel,
//     answer, the AI side chat (POST ask), triggers: all through /api/hyperwork/ as built;
//   - overlay: the live state of each node, reduced from the run log (section 14);
//   - listing helpers (search, categories) and the side chat's proposal / accept / reject.
// The screen that draws it is workflows-ui.js. PROVISIONAL DECISION (open question 11): plain-JavaScript core first;
// the native app shows this web module in its webview; a Rust port later must pass the same fixtures (R453).
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.VrKindsWorkflow = api;
})(typeof self !== 'undefined' ? self : this, function () {
  'use strict';
  const clone = (o) => (o === undefined ? undefined : JSON.parse(JSON.stringify(o)));
  const NAME_RE = '^[a-z][a-z0-9_-]*$';

  // ---------------------------------------------------------------- the kinds
  // a port as text: "name:type", "!" at the end = required, "*" = takes many wires
  const port = (s) => { const m = /^([\w-]+)(?::([\w<>]+))?([!*]*)$/.exec(s); return { name: m[1], type: m[2] || 'json', required: m[3].includes('!') || undefined, many: m[3].includes('*') || undefined }; };
  const M = (kind, category, title, ins, outs, props, required) => ({ kind, version: '1.0.0', category, title, trust: 'built-in', runnable: true, ports: { in: ins.map(port), out: outs.map(port) }, config: { type: 'object', properties: props || {}, required: required || [] } });
  const S = { type: 'string' }, N = { type: 'number' }, I = { type: 'integer' }, O = { type: 'object' }, A = { type: 'array' };
  const E = (...v) => ({ enum: v });
  // the static copy of the library's built-in kinds (native/crates/pgm-workflow-internal/src/kinds.rs)
  const STATIC_MANIFESTS = [
    M('start', 'Flow', 'Start', [], ['prompt:text', 'inputs:json', 'files:list<file>', 'sender:text', 'project:text']),
    M('end', 'Flow', 'End', ['result:json*'], [], { name: Object.assign({ pattern: NAME_RE, message: 'Use lower-case letters, digits, - or _, starting with a letter' }, S) }),
    M('input', 'Flow', 'Sub-graph input', [], ['value:json'], { name: S, type: S }),
    M('output', 'Flow', 'Sub-graph output', ['value:json!'], [], { name: S, type: S }),
    M('note', 'Flow', 'Note', [], [], { text: { type: 'string', maxLength: 4000 } }),
    M('comment', 'Flow', 'Comment', [], [], { text: { type: 'string', maxLength: 4000 } }),
    M('subgraph', 'Flow', 'Sub-graph', [], [], { ref: S }, ['ref']),
    M('prompt', 'AI', 'Prompt', ['in:json!'], ['text:text', 'json:json', 'usage:json', 'error:error'], { system: S, user: S, model: O, temperature: { type: 'number', minimum: 0 }, maxTokens: { type: 'integer', minimum: 1 }, responseFormat: E('text', 'json', 'json-schema'), stream: { type: 'boolean' } }),
    M('chat', 'AI', 'Chat', ['in:json!'], ['messages:messages', 'last:text', 'turns:number', 'stopped:text', 'error:error'], { system: S, opening: S, maxTurns: { type: 'integer', minimum: 1 }, stopWhen: S }),
    M('summarise', 'AI', 'Summarise', ['text:text!'], ['summary:text', 'kept:list<text>', 'error:error'], { target: O, style: S, keep: { type: 'array', items: S } }),
    M('jev', 'AI', 'JEV decision', ['context:json!', 'question:text'], ['choice:text', 'reason:text', 'confidence:number', 'error:error'], { question: S, mode: E('pick', 'yes-no', 'score', 'rank'), model: O, choices: A, minConfidence: { type: 'number', minimum: 0, maximum: 1 }, onLowConfidence: E('ask-person', 'second-opinion', 'default', 'error'), votes: { type: 'integer', minimum: 1 } }),
    M('tool', 'Tools', 'Tool call', ['args:json'], ['result:json', 'error:error'], { tool: S }),
    M('script', 'Tools', 'Script', ['in:json'], ['error:error'], { lang: S, code: S, outputs: A }),
    M('rea', 'Tools', 'Reverse engineering (REA)', ['args:json', 'copy:text'], ['result:json', 'text:text', 'output:json', 'files:json', 'copy:text', 'error:error'],
      { tool: S, args: O, exec: { type: ['string', 'array'] }, files: A, get: { type: 'array', items: S }, purpose: S, copy: S, keep: { type: 'boolean' }, stop: { type: 'boolean' }, timeoutSec: { type: 'integer', minimum: 1 }, queueSec: { type: 'integer', minimum: 0 }, failOnExit: { type: 'boolean' } }),
    M('agent', 'Agents', 'Agent', ['task:text!'], ['result:json', 'text:text', 'error:error'], { timeoutSec: { type: 'number', minimum: 1 } }),
    M('orchestrate', 'Agents', 'Orchestrate', ['goal:text!'], ['result:json', 'error:error']),
    M('sub-workflow', 'Agents', 'Sub-workflow', ['in:json'], ['result:json', 'outcome:text', 'error:error'], { name: S, range: S, inputs: O }, ['name']),
    M('approve', 'People', 'Approve', ['in:json!'], ['approved:flow', 'rejected:flow', 'answer:json', 'text:text', 'error:error'], { who: S, timeoutSec: { type: 'number', minimum: 1 }, onTimeout: S, questions: A }),
    M('if', 'Flow', 'If', ['value:json!'], ['true:flow', 'false:flow'], { condition: S }),
    M('switch', 'Flow', 'Switch', ['value:json!'], ['default:flow'], { cases: A }),
    M('parallel', 'Flow', 'Parallel', ['in:json!'], ['out:json'], { mode: E('copies', 'split', 'fixed'), n: { type: 'integer', minimum: 1 }, lines: A }),
    M('join', 'Flow', 'Join', ['lines:json!*'], ['result:json', 'error:error'], { mode: E('all', 'any', 'first', 'n-of-m', 'merge'), onFail: E('fail', 'skip', 'collect') }),
    M('for-each', 'Flow', 'For each', ['items:list<json>!'], ['results:list<json>', 'error:error'], { ref: S }),
    M('loop', 'Flow', 'Loop', ['in:json!', 'back:json*'], ['again:json', 'done:json', 'limit:json', 'round:number', 'error:error'], { mode: E('while', 'until', 'count'), condition: S, count: { type: 'integer', minimum: 1 }, maxIterations: { type: 'integer', minimum: 1, maximum: 100, message: 'A loop needs a limit between 1 and 100' }, maxSeconds: { type: 'number', minimum: 1 }, maxCost: { type: 'number', minimum: 0 } }),
    M('wait', 'Flow', 'Wait', ['in:json'], ['out:json', 'error:error'], { seconds: { type: 'number', minimum: 0 }, until: S, timeoutSec: { type: 'number', minimum: 1 } }),
    M('set', 'Flow', 'Set state', ['value:json!'], ['out:json'], { key: S }),
    M('emit', 'Flow', 'Emit', ['message:text!'], ['done:flow']),
  ];

  // a list of names from a setting: strings, or objects with one of the given keys (the library's `names`)
  const namesOf = (v, keys) => (Array.isArray(v) ? v : []).map((x, i) => (typeof x === 'string' ? { name: x } : x && typeof x === 'object' ? { name: keys.map((k) => x[k]).find((y) => typeof y === 'string') || 'case' + (i + 1), type: x.type } : { name: 'case' + (i + 1) }));
  // the ports of a built-in kind that depend on the node's settings: the same rules as the library's DefaultRegistry::ports
  function dynamicPorts(id, props, base) {
    const ins = base.in.map((p) => Object.assign({}, p)), outs = base.out.map((p) => Object.assign({}, p)), cfg = props || {};
    const typeOf = () => (typeof cfg.type === 'string' && cfg.type ? cfg.type : 'json');
    if (id === 'input') outs[0].type = typeOf();
    else if (id === 'output') ins[0].type = typeOf();
    else if (id === 'script') { let o = namesOf(cfg.outputs, ['name']); if (!o.length) o = [{ name: 'out' }]; outs.splice(0, 0, ...o.map((x) => ({ name: x.name, type: x.type || 'text' }))); }
    else if (id === 'switch') outs.splice(0, 0, ...namesOf(cfg.cases, ['port', 'name', 'id']).map((x) => ({ name: x.name, type: 'flow' })));
    else if (id === 'parallel') { if (cfg.mode === 'fixed') { const l = namesOf(cfg.lines, ['name', 'id']); if (l.length) { outs.length = 0; for (const x of l) outs.push({ name: x.name, type: 'json' }); } } }
    else if (id === 'jev') {
      const mode = cfg.mode || 'pick';
      const extra = mode === 'yes-no' ? [{ name: 'yes', type: 'flow' }, { name: 'no', type: 'flow' }] : mode === 'score' ? [{ name: 'score', type: 'number' }] : mode === 'rank' ? [{ name: 'ranking', type: 'list<text>' }] : namesOf(cfg.choices, ['id', 'name']).map((x) => ({ name: x.name, type: 'flow' }));
      outs.splice(0, 0, ...extra);
    } else if (id === 'sub-workflow') { if (cfg.inputs && typeof cfg.inputs === 'object') for (const k of Object.keys(cfg.inputs)) ins.push({ name: k, type: 'json' }); }
    return { in: ins, out: outs };
  }

  // section 3.1, as the library's flows_into: the same type, anything into json, number into text, lists by item
  function flowsInto(a, b) {
    a = a || 'json'; b = b || 'json';
    if (a === b || b === 'json') return true;
    if (a === 'number' && b === 'text') return true;
    const la = /^list<(.+)>$/.exec(a), lb = /^list<(.+)>$/.exec(b);
    return !!(la && lb && flowsInto(la[1], lb[1]));
  }

  const COLORS = { Flow: '#8b93a7', AI: '#7c9cff', Tools: '#4fd18b', Agents: '#c78bff', People: '#e6a14f' };
  const labelProp = { type: 'string', title: 'Label', maxLength: 80 };
  const titleCase = (k) => k.replace(/([A-Z])/g, ' $1').replace(/^./, (c) => c.toUpperCase());
  const BOUNDARY = { input: 'in', output: 'out' };
  const RESIZABLE = { note: [160, 80], section: [320, 200] };

  // an editor kind from a manifest (the hub's, or the static copy)
  function kindFromManifest(m) {
    const id = m.kind, builtIn = m.trust === 'built-in' || m.scope === undefined && m.trust === undefined;
    const base = { in: ((m.ports && m.ports.in) || []).map((p) => ({ name: p.name, type: p.type || 'json', required: p.required || undefined, many: p.many || undefined })), out: ((m.ports && m.ports.out) || []).map((p) => ({ name: p.name, type: p.type || 'json' })) };
    const props = {};
    props.label = labelProp;
    const cfg = m.config && typeof m.config === 'object' ? m.config : {};
    for (const k of Object.keys(cfg.properties || {})) { const s = Object.assign({}, cfg.properties[k]); if (!s.title) s.title = titleCase(k); props[k] = s; }
    const ports = builtIn ? (it) => dynamicPorts(id, it.props, base) : () => base;
    const count = (it) => { const p = ports(it); return Math.max(p.in.length, p.out.length); };
    const trust = m.trust && m.trust !== 'built-in' && m.trust !== 'reviewed' ? ' (' + m.trust + ')' : '';
    const kind = {
      id, title: (m.title || id) + trust, category: m.category || 'Other', ports, manifest: m,
      size: (p) => [id === 'start' || id === 'end' ? 120 : 170, Math.max(60, 22 * (count({ props: p }) + 1))],
      schema: { type: 'object', properties: props, required: Array.isArray(cfg.required) ? cfg.required.slice() : [] },
      render: { color: COLORS[m.category] || '#7c9cff', runnable: m.runnable !== false },
      palette: !['subgraph', 'comment'].includes(id) && m.trust !== 'blocked',
    };
    if (BOUNDARY[id]) kind.boundary = BOUNDARY[id];
    if (RESIZABLE[id]) { kind.render.resizable = true; kind.minSize = [80, 40]; kind.size = RESIZABLE[id]; }
    return kind;
  }
  // a section is a frame in the document, not a node (section 3): an editor-only kind
  const SECTION_KIND = { id: 'section', title: 'Section', category: 'Flow', size: RESIZABLE.section, minSize: [80, 60], ports: { in: [], out: [] }, render: { shape: 'frame', color: '#4a7', resizable: true },
    schema: { type: 'object', properties: { label: labelProp, color: { type: 'string', title: 'Colour', pattern: '^#[0-9a-fA-F]{3,8}$', message: 'A colour like #4a7' }, model: { type: 'object', title: 'Model' }, laws: { type: 'array', title: 'Laws' } } } };

  // the manifests the hub gave (several versions of one custom kind: the first, newest, wins), or the static copy
  function kindsFrom(manifests) {
    const seen = new Set(), list = [];
    for (const m of (manifests && manifests.length ? manifests : STATIC_MANIFESTS)) { if (!m || !m.kind || seen.has(m.kind)) continue; seen.add(m.kind); list.push(m); }
    for (const m of STATIC_MANIFESTS) if (!seen.has(m.kind) && ['start', 'end', 'input', 'output', 'note', 'subgraph'].includes(m.kind)) { seen.add(m.kind); list.push(m); } // a short answer never leaves the editor without its specials
    return list.map(kindFromManifest).concat([SECTION_KIND]);
  }
  const majors = {};
  function makePlugin(manifests) {
    const kinds = kindsFrom(manifests); current = kinds;
    for (const k of kinds) if (k.manifest) majors[k.id] = parseInt(String(k.manifest.version || '1').split('.')[0], 10) || 1;
    return { id: 'workflow', title: 'Hyper Workflows', kinds, canConnect: (from, to) => flowsInto(from.type, to.type) };
  }
  // register (or re-register, when the hub's kinds arrive) the workflow kinds in a registry
  function install(registry, manifests) { registry.unregister('workflow'); return registry.register(makePlugin(manifests)); }
  const register = (registry, manifests) => install(registry, manifests);

  // ---------------------------------------------------------------- section 13: the workflow document <-> the editor document
  const NODE_OWN = ['id', 'kind', 'label', 'config', 'pos'];
  const SECTION_OWN = ['id', 'label', 'color', 'nodes', 'pos', 'model', 'laws'];
  const TOP_OWN = ['nodes', 'edges', 'sections', 'subgraphs'];
  let current = null;
  const sizeOf = (kind, props) => { const k = (current || kinds0()).find((x) => x.id === kind); return k ? k.size(props || {}) : [140, 60]; };
  let cache = null; const kinds0 = () => (cache = cache || kindsFrom(null));
  const endsOf = (e) => { const f = e.from.lastIndexOf('.'), t = e.to.lastIndexOf('.'); return { from: { item: e.from.slice(0, f), port: e.from.slice(f + 1) }, to: { item: e.to.slice(0, t), port: e.to.slice(t + 1) } }; };

  function fromWorkflow(json) {
    const doc = { schema: 1, seq: 0, items: {}, links: {}, meta: { wf: {} } };
    for (const k of Object.keys(json)) if (!TOP_OWN.includes(k)) doc.meta.wf[k] = clone(json[k]);
    const usedI = new Set(), usedL = new Set(), refs = new Set();
    const claim = (set, id, what) => { if (set.has(id)) throw new Error('this document uses the ' + what + ' id "' + id + '" twice; the editor needs ids that are unique in the whole document'); set.add(id); return id; };
    const level = (g, parent) => {
      for (const n of g.nodes || []) {
        claim(usedI, n.id, 'node'); const keep = {}; for (const k of Object.keys(n)) if (!NODE_OWN.includes(k)) keep[k] = clone(n[k]);
        const pos = n.pos || [0, 0], isSub = n.kind === 'subgraph';
        if (isSub) {
          const cfg = Object.assign({}, n.config || {}), ref = cfg.ref || n.id; delete cfg.ref; refs.add(ref);
          doc.items[n.id] = { id: n.id, kind: 'group', x: pos[0], y: pos[1], w: pos[2] || 220, h: pos[3] || 140, parent, props: Object.assign({}, cfg, n.label !== undefined ? { label: n.label } : {}, { ref }), keep };
          const sg = (json.subgraphs || {})[ref];
          if (sg) { const extra = {}; for (const k of Object.keys(sg)) if (!['nodes', 'edges', 'inputs', 'outputs'].includes(k)) extra[k] = clone(sg[k]); if (Object.keys(extra).length) doc.items[n.id].keep.sub = extra; level(sg, n.id); }
        } else {
          const rz = RESIZABLE[n.kind], sz = rz && pos[2] ? [pos[2], pos[3]] : sizeOf(n.kind, n.config);
          doc.items[n.id] = { id: n.id, kind: n.kind, x: pos[0], y: pos[1], w: sz[0], h: sz[1], parent, props: Object.assign({}, clone(n.config || {}), n.label !== undefined ? { label: n.label } : {}), keep };
        }
      }
      for (const e of g.edges || []) { claim(usedL, e.id, 'wire'); const x = endsOf(e), keep = {}; for (const k of Object.keys(e)) if (!['id', 'from', 'to'].includes(k)) keep[k] = clone(e[k]); doc.links[e.id] = Object.assign({ id: e.id }, x, Object.keys(keep).length ? { keep } : {}); }
    };
    level(json, null);
    for (const s of json.sections || []) {
      claim(usedI, s.id, 'section'); const p = s.pos || [0, 0, 320, 200], keep = {}; for (const k of Object.keys(s)) if (!SECTION_OWN.includes(k)) keep[k] = clone(s[k]);
      const props = { label: s.label, color: s.color, model: clone(s.model), laws: clone(s.laws) };
      for (const k of Object.keys(props)) if (props[k] === undefined) delete props[k];
      doc.items[s.id] = { id: s.id, kind: 'section', x: p[0], y: p[1], w: p[2] || 320, h: p[3] || 200, parent: null, props, keep };
    }
    // sub-graphs no node of kind "subgraph" points at (a for-each's or loop's `ref`): kept as they are, not drawn
    const hidden = {}; for (const k of Object.keys(json.subgraphs || {})) if (!refs.has(k)) hidden[k] = clone(json.subgraphs[k]);
    if (Object.keys(hidden).length) doc.meta.hiddenSubgraphs = hidden;
    return doc;
  }

  // the editor document -> { json, warnings }. A wire that crosses a group's edge (not through an input / output of the group) cannot be written.
  function toWorkflow(doc) {
    const warnings = [], items = Object.values(doc.items), subgraphs = {};
    const kids = (p) => items.filter((i) => (i.parent || null) === p && i.kind !== 'section');
    const decl = (c) => ({ name: (c.props || {}).name, type: (c.props || {}).type || 'json' });
    const portsOfGroup = (g) => ({ inputs: items.filter((i) => i.parent === g.id && i.kind === 'input').map(decl), outputs: items.filter((i) => i.parent === g.id && i.kind === 'output').map(decl) });
    const edgesOf = (p) => { const out = []; for (const l of Object.values(doc.links)) { const a = doc.items[l.from.item], b = doc.items[l.to.item]; if (a && b && (a.parent || null) === p && (b.parent || null) === p) out.push(Object.assign({ id: l.id, from: l.from.item + '.' + l.from.port, to: l.to.item + '.' + l.to.port }, clone(l.keep || {}))); } return out; };
    const nodeOf = (it) => {
      const keep = clone(it.keep || {}), props = clone(it.props || {}), lab = props.label; delete props.label;
      if (it.kind === 'group') {
        const ref = props.ref || it.id; delete props.ref; const sub = keep.sub; delete keep.sub;
        subgraphs[ref] = Object.assign({}, sub || {}, { nodes: kids(it.id).map(nodeOf), edges: edgesOf(it.id) }, portsOfGroup(it));
        const n = Object.assign({ id: it.id, kind: 'subgraph' }, keep); if (lab !== undefined) n.label = lab; n.config = Object.assign({ ref }, props); n.pos = [it.x, it.y, it.w, it.h]; if (n.kindVersion === undefined) n.kindVersion = 1; return n;
      }
      const n = Object.assign({ id: it.id, kind: it.kind }, keep); if (lab !== undefined) n.label = lab; if (Object.keys(props).length) n.config = props;
      n.pos = RESIZABLE[it.kind] ? [it.x, it.y, it.w, it.h] : [it.x, it.y];
      if (n.kindVersion === undefined && majors[it.kind]) n.kindVersion = majors[it.kind];
      return n;
    };
    for (const l of Object.values(doc.links)) { const a = doc.items[l.from.item], b = doc.items[l.to.item]; if (!a || !b) warnings.push({ link: l.id, message: 'a wire ends at an item that is not there; not saved' }); else if ((a.parent || null) !== (b.parent || null)) warnings.push({ link: l.id, message: 'a wire crosses a group edge; connect it through an input / output of the group; not saved' }); }
    const json = Object.assign({}, clone((doc.meta && doc.meta.wf) || {}));
    json.schema = json.schema || 1;
    const sections = items.filter((i) => i.kind === 'section' && !i.parent), top = kids(null);
    if (sections.length || json.sections) json.sections = sections.map((s) => {
      const inside = top.filter((n) => { const cx = n.x + n.w / 2, cy = n.y + n.h / 2; return cx >= s.x && cy >= s.y && cx <= s.x + s.w && cy <= s.y + s.h; }).map((n) => n.id);
      const p = s.props || {}, o = Object.assign({ id: s.id }, clone(s.keep || {})); if (p.label !== undefined) o.label = p.label; if (p.color !== undefined) o.color = p.color; o.nodes = inside; if (p.model) o.model = clone(p.model); if (p.laws) o.laws = clone(p.laws); o.pos = [s.x, s.y, s.w, s.h]; return o;
    });
    json.nodes = top.map(nodeOf); json.edges = edgesOf(null);
    json.subgraphs = Object.assign({}, clone((doc.meta && doc.meta.hiddenSubgraphs) || {}), subgraphs);
    if (!Object.keys(json.subgraphs).length && !(doc.meta && doc.meta.wf && doc.meta.wf.subgraphs)) delete json.subgraphs;
    return { json, warnings };
  }

  // a new workflow: a start, an end, and the prompt wired to the result
  function blank(name, o) {
    return Object.assign({ schema: 1, name, title: (o && o.title) || '', nodes: [{ id: 'n1', kind: 'start', kindVersion: 1, pos: [40, 100] }, { id: 'n2', kind: 'end', kindVersion: 1, config: { name: 'done' }, pos: [420, 100] }], edges: [{ id: 'e1', from: 'n1.prompt', to: 'n2.result' }] }, (o && o.extra) || {});
  }
  const nameOk = (n) => typeof n === 'string' && /^[A-Za-z0-9_.-]+$/.test(n);

  // ---------------------------------------------------------------- the hub's routes (/api/hyperwork/)
  // request(method, path, body) -> Promise<{ status, data, text }>, given by the page (its api()) or by a test
  function client(request, o) {
    o = o || {};
    const P = '/api/hyperwork/';
    const send = (method, path, body) => Promise.resolve(request(method, P + path, body));
    const must = (r) => {
      if (r.status >= 200 && r.status < 300) return r.data;
      const e = new Error((r.data && (r.data.error || r.data.message)) || 'The hub answered ' + r.status + '.'); e.status = r.status; e.data = r.data || {}; e.problems = (r.data && r.data.problems) || []; throw e;
    };
    const ndjson = (t) => String(t || '').split('\n').filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
    const enc = encodeURIComponent;
    const c = {
      // ---- the list
      list: async (q, scope) => must(await send('GET', 'list' + (q || scope ? '?' + [q ? 'q=' + enc(q) : '', scope ? 'scope=' + enc(scope) : ''].filter(Boolean).join('&') : ''))).items || [],
      kinds: async () => must(await send('GET', 'kinds')).kinds || [],
      // ---- one workflow: the draft if the person has one (and may edit), else the published version
      open: async (id, version) => {
        const d = must(await send('GET', 'get?id=' + enc(id) + (version ? '&version=' + enc(version) : '')));
        return { id, doc: fromWorkflow(d.doc), json: d.doc, etag: d.etag || '', version: d.version, rights: d.rights || {}, meta: d.meta || {}, draft: !!d.etag };
      },
      save: async (id, editorDoc, so) => {
        so = so || {}; const w = toWorkflow(editorDoc), body = { doc: w.json };
        if (id) body.id = id; if (so.etag) body.etag = so.etag; if (so.scope && !id) body.scope = so.scope;
        const d = must(await send('POST', 'save', body));
        return { id: d.id, etag: d.etag, problems: d.problems || [], warnings: w.warnings };
      },
      create: async (name, scope, extra) => { if (!nameOk(name)) throw new Error('A workflow name has letters, digits and - _ . only (it is what /hyperwork types).'); const d = must(await send('POST', 'save', { doc: blank(name, { extra }), scope })); return { id: d.id, etag: d.etag, problems: d.problems || [] }; },
      // the hub's validate needs an id in the document; a workflow not saved yet has none (the hub gives it one on save)
      validate: async (editorDoc, id) => { const j = toWorkflow(editorDoc).json; if (!j.id) j.id = id || 'wf-unsaved'; return must(await send('POST', 'validate', { doc: j })).problems || []; },
      publish: async (id, message) => must(await send('POST', 'publish', { id, message })),
      remove: async (id) => must(await send('POST', 'delete', { id })),
      versions: async (id) => must(await send('GET', 'versions?id=' + enc(id))),
      restore: async (id, version) => must(await send('POST', 'restore', { id, version })),
      // a copy as a new workflow (same scope), under a new name; what only the original had (version, publish stamp) is dropped
      duplicate: async (id, name) => {
        const open = await c.open(id), wf = open.doc; const meta = wf.meta.wf; for (const k of ['id', 'version', 'publishedAt', 'publishedBy', 'message', 'createdAt', 'updatedAt', 'owner', 'scope']) delete meta[k];
        meta.name = name; if (meta.title) meta.title = meta.title + ' (copy)';
        return c.save(null, wf, { scope: (open.meta && open.meta.scope) || undefined });
      },
      // ---- runs (a test run is the editor's own draft)
      run: async (id, r) => { r = r || {}; const body = { id, prompt: r.prompt || '', draft: r.draft !== false }; if (r.inputs) body.inputs = r.inputs; if (r.project) body.project = r.project; return must(await send('POST', 'run', body)); },
      runMeta: async (run) => must(await send('GET', 'run?id=' + enc(run))),
      runs: async (workflow) => must(await send('GET', 'runs?mine=1' + (workflow ? '&workflow=' + enc(workflow) : ''))).runs || [],
      log: async (run, from) => { const r = await send('GET', 'log?id=' + enc(run) + '&from=' + (from || 0)); if (r.status !== 200) must(r); return ndjson(r.text); },
      cancel: async (run) => must(await send('POST', 'cancel', { id: run })),
      answer: async (run, node, answers) => must(await send('POST', 'answer', { run, node, answers })),
      // the events from `from` on, until the run ends or stops to wait: live (o.stream gives the line reader of the page) or by asking again
      watch(run, onEvent, wo) {
        wo = wo || {}; let stopped = false, from = wo.from || 0;
        const done = (async () => {
          if (o.stream) {
            try { await o.stream(P + 'log?id=' + enc(run) + '&from=' + from + '&follow=1', (line) => { let ev; try { ev = JSON.parse(line); } catch { return; } from = (ev.seq || from) + 1; onEvent(ev); }, () => stopped); return; } catch { /* fall back to polling */ }
          }
          for (let i = 0; i < (wo.maxPolls || 1200) && !stopped; i++) {
            const evs = await c.log(run, from).catch(() => []);
            for (const ev of evs) { from = Math.max(from, (ev.seq || 0) + 1); onEvent(ev); if (ev.type === 'run_end' || ev.type === 'snapshot') return; }
            await new Promise((r) => setTimeout(r, wo.pollMs || 600));
          }
        })();
        return { stop() { stopped = true; }, done };
      },
      // ---- the AI side chat (T-206): answers with text and an optional patch; writes nothing
      ask: async (editorDoc, message, selection) => { const body = { doc: toWorkflow(editorDoc).json, message }; if (selection && selection.length) body.selection = selection; return must(await send('POST', 'ask', body)); },
      // ---- triggers (T-186)
      triggers: async (workflowId) => must(await send('GET', 'triggers?workflow=' + enc(workflowId))).triggers || [],
      triggerSave: async (t) => must(await send('POST', 'trigger', t)),
      triggerPause: async (id) => must(await send('POST', 'trigger/pause', { id })),
      triggerResume: async (id) => must(await send('POST', 'trigger/resume', { id })),
      triggerDelete: async (id) => must(await send('POST', 'trigger/delete', { id })),
    };
    return c;
  }

  // ---------------------------------------------------------------- the run overlay (section 14)
  // states per node id: running | ok | error | retrying | skipped | waiting | cancelled, from node_start / node_end /
  // node_retry / wait / answer / run_end
  function overlay() {
    const o = { nodes: {}, run: { id: null, status: 'idle', outcome: null, result: null, error: null }, events: 0 };
    const at = (id) => (o.nodes[id] = o.nodes[id] || { state: 'idle', runs: 0 });
    o.reset = () => { o.nodes = {}; o.run = { id: null, status: 'idle', outcome: null, result: null, error: null }; o.events = 0; };
    o.apply = (ev) => {
      o.events++;
      const n = ev.node ? at(ev.node) : null;
      switch (ev.type) {
        case 'run_start': o.run.id = ev.run || o.run.id; o.run.status = 'running'; break;
        case 'node_start': n.state = 'running'; n.attempt = ev.attempt; n.runs++; if (ev.iteration != null) n.iteration = ev.iteration; n.since = ev.at; break;
        case 'node_end': n.attempt = ev.attempt; n.durationMs = ev.durationMs; n.outputs = ev.outputs;
          if (ev.status === 'ok') n.state = 'ok';
          else if (ev.status === 'skipped') n.state = 'skipped';
          else if (ev.status === 'cancelled') n.state = 'cancelled';
          else if (ev.status === 'waiting') n.state = 'waiting';
          else { n.state = ev.retry ? 'retrying' : 'error'; n.error = ev.error || null; }
          break;
        case 'node_retry': n.state = 'retrying'; break;
        case 'wait': if (n) n.state = 'waiting'; o.run.status = 'waiting'; break;
        case 'answer': if (n) n.state = 'running'; o.run.status = 'running'; break;
        case 'run_end':
          o.run.status = ev.status || 'ok'; o.run.outcome = ev.outcome || null; o.run.result = ev.result === undefined ? null : ev.result; o.run.error = ev.error || null; o.run.durationMs = ev.durationMs; o.run.totals = ev.totals;
          for (const k of Object.keys(o.nodes)) if (['running', 'waiting', 'retrying'].includes(o.nodes[k].state)) o.nodes[k].state = 'cancelled';
          break;
        case 'snapshot': if (o.run.status === 'running') o.run.status = 'waiting'; break;
        default: break;
      }
      return o;
    };
    o.apply.all = (list) => { for (const e of list) o.apply(e); return o; };
    o.ended = () => !['idle', 'running', 'waiting'].includes(o.run.status);
    // what a view draws: { itemId: { state, text, title } }
    o.marks = () => { const out = {}; for (const k of Object.keys(o.nodes)) { const n = o.nodes[k]; out[k] = { state: n.state, text: n.state + (n.durationMs ? ' · ' + (n.durationMs < 1000 ? n.durationMs + ' ms' : Math.round(n.durationMs / 100) / 10 + ' s') : ''), title: n.error ? n.error.message || n.error.code : (n.outputs ? JSON.stringify(n.outputs).slice(0, 300) : '') }; } return out; };
    return o;
  }

  // validation problems -> what a view draws on the items: { itemId: { level, text } } (the worst level wins)
  function problemMarks(problems) {
    const out = {};
    for (const p of problems || []) {
      const id = p.node; if (!id) continue;
      const cur = out[id]; const level = p.level === 'error' ? 'error' : 'warn';
      if (!cur) out[id] = { level, text: p.message || p.code }; else { cur.text += '\n' + (p.message || p.code); if (level === 'error') cur.level = 'error'; }
    }
    return out;
  }

  // ---------------------------------------------------------------- the list: search and categories (labels)
  const norm = (s) => String(s == null ? '' : s).toLowerCase();
  function filterList(items, o) {
    o = o || {}; const q = norm(o.q).trim().split(/\s+/).filter(Boolean);
    return (items || []).filter((w) => {
      if (o.category && o.category !== '*') {
        if (o.category === '@drafts') { if (!w.draft) return false; } else if (o.category === '@mine') { if (!/^me-/.test(w.scope || '')) return false; } else if (o.category === '@none') { if ((w.labels || []).length) return false; } else if (!(w.labels || []).includes(o.category)) return false;
      }
      const hay = norm([w.name, w.shown, w.title, w.description, (w.labels || []).join(' '), w.scope].join(' '));
      return q.every((x) => hay.includes(x));
    });
  }
  function categories(items) {
    const m = new Map(); let none = 0, drafts = 0, mine = 0;
    for (const w of items || []) { for (const l of w.labels || []) m.set(l, (m.get(l) || 0) + 1); if (!(w.labels || []).length) none++; if (w.draft) drafts++; if (/^me-/.test(w.scope || '')) mine++; }
    const out = [{ id: '*', label: 'All', count: (items || []).length }];
    if (drafts) out.push({ id: '@drafts', label: 'My drafts', count: drafts });
    if (mine) out.push({ id: '@mine', label: 'Personal', count: mine });
    for (const [l, n] of [...m].sort((a, b) => a[0].localeCompare(b[0]))) out.push({ id: l, label: l, count: n });
    if (none) out.push({ id: '@none', label: 'No category', count: none });
    return out;
  }
  // the next free name for a copy: "name-copy", "name-copy2" ...
  function copyName(name, taken) { const t = new Set(taken || []); let n = name + '-copy', i = 2; while (t.has(n)) n = name + '-copy' + i++; return n; }

  // ---------------------------------------------------------------- triggers: what the form sends and how a row reads
  function triggerBody(workflow, f) {
    const b = { workflow, kind: f.kind, title: f.title || undefined, prompt: f.prompt || '' };
    if (f.kind === 'timer') { b.timer = f.recur ? { recur: f.recur } : { every: { n: Number(f.n) || 1, unit: f.unit || 'hours' } }; if (f.at) b.timer.at = f.at; if (f.missed) b.missed = f.missed; }
    else if (f.kind === 'webhook') b.webhook = { events: (f.events || '').split(/[\s,]+/).filter(Boolean) };
    else if (f.kind === 'event') b.event = { name: f.name };
    if (f.project) b.project = f.project;
    return b;
  }
  function triggerSummary(t) {
    const when = (ms) => (ms ? new Date(ms).toLocaleString() : 'never');
    if (t.kind === 'timer') { const tm = t.timer || {}; return (tm.recur ? tm.recur : 'every ' + (tm.every ? tm.every.n + ' ' + tm.every.unit : '?')) + (tm.at ? ' at ' + tm.at : '') + (t.paused ? ' (paused)' : ' · next ' + when(t.next)); }
    if (t.kind === 'webhook') return 'webhook /hooks/hyperwork/' + t.id + (t.webhook && t.webhook.events && t.webhook.events.length ? ' for ' + t.webhook.events.join(', ') : '') + (t.paused ? ' (paused)' : '');
    if (t.kind === 'event') return 'event "' + ((t.event || {}).name || '?') + '"' + (t.paused ? ' (paused)' : '');
    return t.kind || '?';
  }

  // ---------------------------------------------------------------- the AI side chat: a conversation whose answers may carry a patch
  // chat(client, editor): send(text) asks the hub (with the document as it is now and the selection); a patch is tried on a
  // scratch copy (editor.proposePatch) and waits for accept() / reject(); nothing reaches the document before accept
  function chat(cl, editor, o) {
    o = o || {}; const c = { messages: [], pending: null, busy: false, noAi: false }; const subs = [];
    const emit = () => { for (const f of subs.slice()) f(c); };
    c.on = (f) => { subs.push(f); return () => subs.splice(subs.indexOf(f), 1); };
    const settle = () => { c.pending = null; emit(); };
    c.send = async (text) => {
      text = String(text || '').trim(); if (!text || c.busy) return null;
      c.messages.push({ who: 'me', text }); c.busy = true; emit();
      try {
        if (c.pending) { c.pending.reject(); c.pending = null; }
        const a = await cl.ask(editor.toJSON(), text, editor.selection());
        c.noAi = !!a.noAi; const m = { who: 'ai', text: a.text || '', problems: a.problems || [], patch: a.patch || [] };
        if (m.patch.length && !a.noAi) {
          const prop = editor.proposePatch(m.patch);
          if (prop.ok) { m.proposal = prop; c.pending = prop; m.summary = prop.summary; m.preview = prop.preview; m.changes = root_core().diffDocs(editor.toJSON(), prop.preview); }
          else m.rejected = 'The suggested change does not fit this document (command ' + ((prop.index || 0) + 1) + '): ' + prop.error;
        }
        c.messages.push(m); return m;
      } catch (e) { const m = { who: 'ai', text: '', error: e.message }; c.messages.push(m); return m; }
      finally { c.busy = false; emit(); }
    };
    // accept: into the draft as one undo step (at the top level: a patch speaks of the whole workflow)
    c.accept = () => { if (!c.pending) return { ok: false, error: 'nothing to accept' }; editor.goto(0); const r = c.pending.accept(); settle(); return r; };
    c.reject = () => { if (!c.pending) return { ok: true }; const r = c.pending.reject(); settle(); return r; };
    return c;
  }
  const root_core = () => (typeof module === 'object' && module.exports ? require('./core.js') : root_global().VrEditorCore);
  const root_global = () => (typeof self !== 'undefined' ? self : globalThis);

  return {
    id: 'workflow', STATIC_MANIFESTS, makePlugin, install, register, kindFromManifest, kindsFrom, dynamicPorts, flowsInto,
    fromWorkflow, toWorkflow, blank, nameOk, client, overlay, problemMarks, filterList, categories, copyName, triggerBody, triggerSummary, chat,
  };
});
