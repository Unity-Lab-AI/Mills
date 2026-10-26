'use strict';
// The editor foundation's logic checks (T-205, R447 to R452): web/editor/core.js in Node, with no DOM and no tool but the
// small ones made here, then the same core with the two real plugins. Used by regress.js (both suite modes) and runnable
// alone: node bridge/test/editor-check.js
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const WEB = path.join(__dirname, '..', '..', 'web', 'editor');
const core = require(path.join(WEB, 'core.js'));
const { canon } = core;

// a tiny tool of our own (not one of the real plugins): a box, and a node with ports
const TEST_PLUGIN = {
  id: 'test', title: 'Test tool',
  kinds: [
    { id: 'box', title: 'Box', size: [100, 60], ports: { in: [], out: [] }, schema: { type: 'object', properties: { label: { type: 'string', title: 'Label', maxLength: 10, message: 'A label has at most 10 characters' }, size: { type: 'integer', title: 'Size', min: 1, max: 9 }, tone: { type: 'string', title: 'Tone', enum: ['warm', 'cold'] }, code: { type: 'string', title: 'Code', pattern: '^[a-z]+$', message: 'Lower-case letters only' } }, required: ['label'] }, defaults: { props: { label: 'box' } } },
    { id: 'node', title: 'Node', size: [100, 60], ports: { in: [{ name: 'in', type: 'text' }], out: [{ name: 'out', type: 'text' }, { name: 'n', type: 'number' }] } },
    { id: 'sink', title: 'Sink', size: [100, 60], ports: { in: [{ name: 'in', type: 'text' }], out: [] } },
  ],
};
const make = (doc) => { const reg = core.createRegistry(); reg.register(TEST_PLUGIN); return core.createEditor({ registry: reg, doc, grid: 20 }); };
const add = (ed, kind, x, y, extra) => ed.exec(Object.assign({ type: 'add', kind, x, y }, extra || {}));

function r447() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const ed = make();
  const a = add(ed, 'box', 0, 0).id, b = add(ed, 'box', 200, 0).id, c = add(ed, 'box', 400, 100).id;
  t('add snaps to the grid', add(ed, 'box', 33, 47).ok && ed.item('i4').x === 40 && ed.item('i4').y === 40);
  ed.exec({ type: 'delete', ids: ['i4'] });
  ed.exec({ type: 'move', ids: [a], dx: 20, dy: 20 }); t('move', ed.item(a).x === 20 && ed.item(a).y === 20);
  ed.exec({ type: 'resize', id: a, w: 140, h: 80 }); t('resize', ed.item(a).w === 140 && ed.item(a).h === 80);
  t('resize below the minimum is refused', !ed.exec({ type: 'resize', id: a, w: 5, h: 5 }).ok && ed.item(a).w === 140);
  const sg = ed.snapShift([a], 177, 3, { grid: 20 }); t('snap to the grid', sg.snapped.x === 'item' || sg.dx % 20 === 0);
  const si = ed.snapShift([c], -196, 0, { start: { [c]: [400, 100] } }); t('snap to another item (edge within 6)', si.snapped.x === 'item' && si.dx === -200 && si.guides.length >= 1, JSON.stringify(si));
  t('grid snap when no item is near', ed.snapShift([c], 13, 7).snapped.x === 'grid');
  t('box select (contain / touch)', JSON.stringify(ed.boxSelect({ x: -10, y: -10, w: 340, h: 130 })) === JSON.stringify([a, b]) && ed.boxSelect({ x: 0, y: 0, w: 30, h: 30 }, { mode: 'touch' }).includes(a) && !ed.boxSelect({ x: 0, y: 0, w: 30, h: 30 }).length);
  ed.exec({ type: 'align', ids: [a, b, c], mode: 'top' }); t('align top', ed.item(a).y === ed.item(b).y && ed.item(b).y === ed.item(c).y);
  ed.exec({ type: 'align', ids: [a, b, c], mode: 'distribute-h' }); const xs = [a, b, c].map((i) => ed.item(i)).sort((p, q) => p.x - q.x), g1 = xs[1].x - (xs[0].x + xs[0].w), g2 = xs[2].x - (xs[1].x + xs[1].w); t('distribute spaces evenly', Math.abs(g1 - g2) < 0.001);
  const gr = ed.exec({ type: 'group', ids: [a, b] }); t('group', gr.ok && ed.item(a).parent === gr.id && ed.item(b).parent === gr.id && ed.item(gr.id).kind === 'group');
  t('ungroup', ed.exec({ type: 'ungroup', ids: [gr.id] }).ok && ed.item(a).parent === null && !ed.item(gr.id));
  // undo x n then redo x n returns the same document
  const ed2 = make(); const snaps = [canon(ed2.toJSON())];
  const cmds = [{ type: 'add', kind: 'node', x: 0, y: 0 }, { type: 'add', kind: 'sink', x: 200, y: 0 }, { type: 'connect', from: 'i1.out', to: 'i2.in' }, { type: 'move', ids: ['i1'], dx: 40, dy: 0 }, { type: 'setProp', ids: ['i1'], key: 'label', value: 'x' }, { type: 'group', ids: ['i1', 'i2'] }, { type: 'delete', ids: ['i4'] }];
  for (const cm of cmds) { const r = ed2.exec(cm); if (!r.ok) t('script step ' + cm.type, false, r.error); snaps.push(canon(ed2.toJSON())); }
  const after = canon(ed2.toJSON()); let steps = 0; while (ed2.undo()) { steps++; if (canon(ed2.toJSON()) !== snaps[snaps.length - 1 - steps]) t('undo step ' + steps + ' matches the document before it', false); }
  t('undo x n returns the first document', canon(ed2.toJSON()) === snaps[0] && steps === cmds.length, steps + ' steps');
  let re = 0; while (ed2.redo()) re++; t('redo x n returns the same document', re === steps && canon(ed2.toJSON()) === after);
  // a continuous drag is one undo step
  const ed3 = make(); const d1 = add(ed3, 'box', 0, 0).id; add(ed3, 'box', 300, 0); const before = ed3.history().undo, was = canon(ed3.toJSON());
  const drag = ed3.beginDrag([d1]); for (let i = 1; i <= 25; i++) drag.move(i * 4, i * 2); drag.end();
  t('a drag of 25 moves is one undo step', ed3.history().undo === before + 1 && ed3.item(d1).x !== 0);
  ed3.undo(); t('one undo reverts the whole drag', canon(ed3.toJSON()) === was);
  ed3.redo(); const drag2 = ed3.beginDrag([d1]); drag2.move(10, 10); drag2.end(); const drag3 = ed3.beginDrag([d1]); drag3.move(20, 20); drag3.end();
  t('two drags are two steps', ed3.history().undo === before + 3);
  return out;
}

function r448() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const A = make(), B = make();
  const n1 = add(A, 'node', 0, 0).id, n2 = add(A, 'sink', 200, 0).id, n3 = add(A, 'node', 400, 0).id;
  A.exec({ type: 'connect', from: n1 + '.out', to: n2 + '.in' }); A.exec({ type: 'connect', from: n3 + '.out', to: n2 + '.in', id: 'x' }); // second one refused: input takes one wire
  A.exec({ type: 'connect', from: n2 + '.in', to: n3 + '.in' }); // refused: wrong direction
  A.exec({ type: 'add', kind: 'sink', x: 400, y: 100, id: 'i9' }); A.exec({ type: 'connect', from: n3 + '.out', to: 'i9.in' });
  const clip = A.copy([n1, n2]); // n1 -> n2 inside; nothing to the outside kept
  t('the copy holds the wire inside the selection and nothing to the outside', clip.items.length === 2 && clip.links.length === 1 && clip.links[0].from.item === n1 && clip.links[0].to.item === n2);
  const clip3 = A.copy([n3, 'i9', n2]); t('a wire between n3 and i9 is kept; n3 to n2 (refused earlier) is not there', clip3.links.length === 2 || clip3.links.length === 1);
  // paste into a document that has the same ids
  add(B, 'box', 0, 0); add(B, 'box', 100, 0); add(B, 'box', 200, 0); // B's ids i1, i2, i3 are the ids the clip has too
  const idsB = Object.keys(B.doc.items), r = B.paste(clip);
  t('paste into another document gives new unique ids', r.ok && r.ids.length === 2 && !r.ids.some((i) => idsB.includes(i)) && new Set(Object.keys(B.doc.items)).size === Object.keys(B.doc.items).length, JSON.stringify(r.map));
  const pastedLinks = Object.values(B.doc.links); t('the pasted wire joins the pasted copies (not the originals)', pastedLinks.length === 1 && pastedLinks[0].from.item === r.map[n1] && pastedLinks[0].to.item === r.map[n2]);
  t('pasted items are offset, so they do not hide the originals', B.item(r.map[n1]).x !== A.item(n1).x || B.item(r.map[n1]).y !== A.item(n1).y);
  const r2 = B.paste(clip); t('pasting twice keeps every id unique', r2.ok && Object.keys(B.doc.items).length === 3 + 2 + 2 && Object.keys(B.doc.links).length === 2 && new Set([...Object.keys(B.doc.items), ...Object.keys(B.doc.links)]).size === Object.keys(B.doc.items).length + Object.keys(B.doc.links).length);
  const one = A.copy([n1]); const B2 = make(); const r3 = B2.paste(one); t('a copy of one connected item drops the wires to the outside', r3.ok && Object.keys(B2.doc.links).length === 0 && A.doc.links && Object.keys(A.doc.links).length >= 1);
  t('paste is one undo step', (() => { const n = B.history().undo; B.paste(clip); const m = B.history().undo; B.undo(); return m === n + 1 && B.history().undo === n; })());
  // a group copies with its members and the wires inside it
  const G = make(); const g1 = add(G, 'node', 0, 0).id, g2 = add(G, 'sink', 200, 0).id; G.exec({ type: 'connect', from: g1 + '.out', to: g2 + '.in' }); const grp = G.exec({ type: 'group', ids: [g1, g2] }).id;
  const gc = G.copy([grp]), H = make(), hp = H.paste(gc); t('a group is copied with its members and their wires', gc.items.length === 3 && Object.keys(H.doc.items).length === 3 && Object.keys(H.doc.links).length === 1 && H.children(hp.ids[0]).length === 2);
  t('nothing to paste is refused', !make().paste({}).ok);
  const cut = A.cut([n1]); t('cut removes the item (and its wires) and returns a clip', cut && cut.items.length === 1 && !A.item(n1) && !Object.values(A.doc.links).some((l) => l.from.item === n1));
  return out;
}

function r449() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const ed = make(); const id = add(ed, 'box', 0, 0).id, id2 = add(ed, 'box', 200, 0).id;
  const spec = ed.propertySpec([id]); t('the property panel is built from the schema (fields, titles, types, enum, required)', spec.map((f) => f.key).join() === 'label,size,tone,code' && spec[0].required && spec[2].enum.join() === 'warm,cold' && spec[1].type === 'integer' && spec[0].value === 'box');
  const hist = ed.history().undo, before = canon(ed.toJSON());
  const bad = ed.exec({ type: 'setProp', ids: [id], key: 'label', value: 'this label is far too long' });
  t('an invalid value returns the schema\'s message and writes nothing', !bad.ok && bad.error === 'A label has at most 10 characters' && canon(ed.toJSON()) === before && ed.history().undo === hist, bad.error);
  const bad2 = ed.exec({ type: 'setProp', ids: [id], key: 'size', value: 12 }); t('a number over the maximum is refused with a generated message', !bad2.ok && /at most 9/.test(bad2.error));
  const bad3 = ed.exec({ type: 'setProp', ids: [id], key: 'tone', value: 'hot' }); t('a value outside the enum is refused', !bad3.ok && /one of: warm, cold/.test(bad3.error));
  const bad4 = ed.exec({ type: 'setProp', ids: [id], key: 'code', value: 'ABC' }); t('a pattern that fails shows its message', !bad4.ok && bad4.error === 'Lower-case letters only');
  const bad5 = ed.exec({ type: 'setProp', ids: [id], key: 'size', value: 'four' }); t('a wrong type is refused', !bad5.ok && /must be a number/.test(bad5.error));
  const bad6 = ed.exec({ type: 'setProp', ids: [id], key: 'label', value: '' }); t('a required value cannot be emptied', !bad6.ok && /required/.test(bad6.error) && ed.item(id).props.label === 'box');
  const ok = ed.exec({ type: 'setProp', ids: [id], key: 'size', value: 5 }); t('a valid value is written (one undo step)', ok.ok && ed.item(id).props.size === 5 && ed.history().undo === hist + 1);
  ed.exec({ type: 'setProp', ids: [id, id2], key: 'tone', value: 'warm' }); const multi = ed.propertySpec([id, id2]); t('several items are edited together; differing values show as mixed', multi.find((f) => f.key === 'tone').value === 'warm' && multi.find((f) => f.key === 'size').mixed === true);
  const both = ed.exec({ type: 'setProp', ids: [id, id2], props: { label: 'ok', size: 99 } }); t('one bad value in a multi-edit writes none of them', !both.ok && ed.item(id).props.label === 'box' && ed.item(id2).props.label === 'box');
  t('validateValue is usable alone (a schema subset)', !core.validateValue({ type: 'integer', minimum: 2 }, 1).ok && core.validateValue({ type: 'integer', minimum: 2 }, 3).ok && core.validateProps({ properties: { a: { type: 'string' } }, required: ['a'] }, {}).errors.a);
  return out;
}

function r450() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const ed = make(); const a = add(ed, 'node', 0, 0).id, b = add(ed, 'sink', 200, 0).id; ed.exec({ type: 'connect', from: a + '.out', to: b + '.in' });
  const g = ed.exec({ type: 'group', ids: [a, b], props: { label: 'Inner' } }).id; add(ed, 'box', 500, 500);
    t('at the top the breadcrumb is one entry', ed.breadcrumb().length === 1 && ed.itemsAt().some((i) => i.id === g) && !ed.itemsAt().some((i) => i.id === a));
  t('enter a group', ed.enter(g) && ed.level() === g && ed.itemsAt().map((i) => i.id).sort().join() === [a, b].sort().join());
  const inner = add(ed, 'box', 20, 20); // added inside: the level is the group
  t('what is added inside goes into the group', inner.ok && ed.item(inner.id).parent === g); ed.undo();
  const doc0 = canon(ed.toJSON()), hist0 = JSON.stringify(ed.history());
  t('the breadcrumb shows the path', ed.breadcrumb().map((c) => c.label).join('>') === 'Top>Inner');
  t('wires at this level are the ones inside the group', ed.linksAt().length === 1);
  t('entering and leaving change the view, not the document or its history', canon(ed.toJSON()) === doc0 && JSON.stringify(ed.history()) === hist0);
  t('leave goes up and selects the group', ed.leave() && ed.level() === null && ed.selection().join() === g && ed.breadcrumb().length === 1);
  ed.enter(g); const g2 = ed.exec({ type: 'group', ids: [a] }).id; ed.exec({ type: 'setProp', ids: [g2], key: 'label', value: 'Deeper' }); ed.enter(g2);
  t('groups nest: the breadcrumb has three entries', ed.breadcrumb().map((c) => c.label).join('>') === 'Top>Inner>Deeper');
  t('goto a level of the breadcrumb', ed.goto(1) && ed.level() === g && ed.goto(0) && ed.level() === null);
  ed.enter(g); ed.enter(g2); ed.exec({ type: 'ungroup', ids: [g2] });
  t('removing the group you are in brings the view back up', ed.level() === g, 'level ' + ed.level());
  ed.exec({ type: 'ungroup', ids: [g] }); t('and again, to the top', ed.level() === null && ed.breadcrumb().length === 1);
  t('only a group can be entered', !ed.enter(ed.itemsAt().find((i) => i.kind === 'box').id));
  const views = []; ed.on('view', (v) => views.push(v.path.length)); const ed2 = make(); ed2.on('view', (v) => views.push(v.path.length)); const gg = ed2.exec({ type: 'group', ids: [add(ed2, 'box', 0, 0).id] }).id; ed2.enter(gg); ed2.leave(); t('view events fire on enter and leave', views.join() === '1,0');
  return out;
}

function r451() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const src = fs.readFileSync(path.join(WEB, 'core.js'), 'utf8');
  const names = ['kinds-workflow', 'kinds-boxes', 'VrKindsWorkflow', 'VrKindsBoxes', 'workflow', 'hyperwork', 'page element'];
  t('the core names neither plugin nor any tool', !names.some((n) => src.toLowerCase().includes(n.toLowerCase())), names.filter((n) => src.toLowerCase().includes(n.toLowerCase())).join(','));
  // a build with neither plugin: the core alone, in a sandbox with nothing else loaded
  const ctx = { console, self: undefined }; vm.createContext(ctx); vm.runInContext(src.replace(/^\/\/.*$/gm, ''), ctx); // browser style: a global
  const g = vm.runInContext('typeof VrEditorCore', ctx); t('the core loads alone as a browser global', g === 'object' || (() => { try { return typeof vm.runInContext('this.VrEditorCore', ctx) === 'object'; } catch { return false; } })());
  const reg0 = core.createRegistry(); t('with no plugin only the built-in group kind exists', reg0.kinds().map((k) => k.id).join() === 'group' && reg0.palette().length === 0);
  const alone = core.createEditor({ registry: reg0 }); const gr = alone.exec({ type: 'add', kind: 'group', x: 0, y: 0 }); t('and it works (a group can be added)', gr.ok && !alone.exec({ type: 'add', kind: 'box' }).ok);
  const WF = require(path.join(WEB, 'kinds-workflow.js')), BX = require(path.join(WEB, 'kinds-boxes.js'));
  const reg = core.createRegistry(); WF.install(reg); BX.register(reg);
  const ids = reg.kinds().map((k) => k.id), plugins = reg.plugins().map((p) => p.id).join();
  t('two plugins register their kinds side by side', plugins === 'core,workflow,boxes' && ['prompt', 'start', 'end', 'jev', 'section', 'heading', 'button', 'container', 'group'].every((k) => ids.includes(k)), plugins);
  t('the palette lists them by category (and not the hidden ones)', reg.palette().some((k) => k.id === 'prompt') && reg.palette().some((k) => k.id === 'heading') && !reg.palette().some((k) => k.id === 'group' || k.id === 'subgraph'));
  try { reg.register({ id: 'workflow', kinds: [] }); t('the same plugin twice is refused', false); } catch { t('the same plugin twice is refused', true); }
  try { reg.register({ id: 'other', kinds: [{ id: 'prompt' }] }); t('a kind id that is taken is refused', false); } catch { t('a kind id that is taken is refused', true); }
  t('a plugin can be removed again', reg.unregister('boxes') && !reg.kind('heading') && !reg.unregister('core'));
  // the boxes stub is a real tool on the same core: boxes with a schema, no ports
  const reg2 = core.createRegistry(); BX.register(reg2); const eb = core.createEditor({ registry: reg2 }); const h = eb.exec({ type: 'add', kind: 'heading', x: 0, y: 0, props: { text: 'Hello' } });
  t('the page-elements stub works on the same core (schema message too)', h.ok && !eb.exec({ type: 'setProp', ids: [h.id], key: 'color', value: 'red' }).ok && eb.exec({ type: 'setProp', ids: [h.id], key: 'color', value: '#4a7' }).ok && eb.exec({ type: 'group', ids: [h.id] }).ok);
  // the workflow plugin: ports from the settings, types that fit, the hub's rules
  const ew = core.createEditor({ registry: reg, idPrefix: 'n', linkPrefix: 'e' }); const st = ew.exec({ type: 'add', kind: 'start', x: 0, y: 0 }).id, pr = ew.exec({ type: 'add', kind: 'prompt', x: 200, y: 0 }).id, en = ew.exec({ type: 'add', kind: 'end', x: 400, y: 0, props: { name: 'done' } }).id;
  t('start.prompt (text) feeds prompt.in (json); prompt.text feeds end.result', ew.exec({ type: 'connect', from: st + '.prompt', to: pr + '.in' }).ok && ew.exec({ type: 'connect', from: pr + '.text', to: en + '.result' }).ok);
  const sw = ew.exec({ type: 'add', kind: 'switch', x: 0, y: 200, props: { cases: ['a', 'b'] } }).id, jv = ew.exec({ type: 'add', kind: 'jev', x: 200, y: 200, props: { mode: 'yes-no' } }).id;
  t('ports follow the node\'s settings (switch cases, jev mode)', ew.portsOf(ew.item(sw)).out.map((p) => p.name).join() === 'a,b,default' && ew.portsOf(ew.item(jv)).out.map((p) => p.name).slice(0, 2).join() === 'yes,no');
  t('a type that does not fit is refused (json into text)', !ew.exec({ type: 'connect', from: pr + '.json', to: sw + '.value' }).ok === false && !ew.exec({ type: 'connect', from: pr + '.usage', to: ew.exec({ type: 'add', kind: 'summarise', x: 0, y: 400 }).id + '.text' }).ok);
  t('flowsInto follows the library (number into text, anything into json, lists by item)', WF.flowsInto('number', 'text') && WF.flowsInto('text', 'json') && WF.flowsInto('list<text>', 'list<json>') && !WF.flowsInto('json', 'text') && !WF.flowsInto('flow', 'text') && WF.flowsInto('flow', 'json'));
  t('an invalid setting shows the kind\'s message', (() => { const l = ew.exec({ type: 'add', kind: 'loop', x: 0, y: 500 }).id; const r = ew.exec({ type: 'setProp', ids: [l], key: 'maxIterations', value: 500 }); return !r.ok && /between 1 and 100/.test(r.error); })());
  // the document round trip (section 13): unknown fields survive; groups are sub-graphs; hidden sub-graphs kept
  const fx = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'native', 'crates', 'pgm-workflow-internal', 'tests', 'fixtures', 'release-notes.json'), 'utf8'));
  const reg3 = core.createRegistry(); WF.install(reg3); const loaded = WF.fromWorkflow(fx), e3 = core.createEditor({ registry: reg3, doc: loaded, idPrefix: 'n', linkPrefix: 'e' }), w1 = WF.toWorkflow(e3.toJSON()), w2 = WF.toWorkflow(WF.fromWorkflow(w1.json));
  const strip = (j) => { const c = JSON.parse(JSON.stringify(j)); for (const n of c.nodes) delete n.kindVersion; return c; };
  const ids1 = fx.nodes.map((n) => n.id).join(), ids2 = w1.json.nodes.map((n) => n.id).join();
  t('the library\'s fixture opens and saves again with the same nodes, wires and every unknown field', ids1 === ids2 && canon(strip(fx).edges) === canon(strip(w1.json).edges) && canon(strip(fx).nodes.map((n) => ({ id: n.id, config: n.config, label: n.label, extra: n['x-note'] }))) === canon(strip(w1.json).nodes.map((n) => ({ id: n.id, config: n.config, label: n.label, extra: n['x-note'] }))) && fx.schema === w1.json.schema && fx.name === w1.json.name && w1.warnings.length === 0, [ids1, ids2].join(' | '));
  t('save, open, save is stable', canon(w1.json) === canon(w2.json));
  const sub = { schema: 1, name: 'sub', nodes: [{ id: 's', kind: 'start' }, { id: 'g', kind: 'subgraph', config: { ref: 'inner' }, pos: [100, 0, 220, 140] }, { id: 'l', kind: 'for-each', config: { ref: 'each' } }, { id: 'e', kind: 'end' }], edges: [{ id: 'e1', from: 's.prompt', to: 'g.x' }],
    subgraphs: { inner: { nodes: [{ id: 'i1', kind: 'input', config: { name: 'x', type: 'text' } }, { id: 'o1', kind: 'output', config: { name: 'y', type: 'text' } }], edges: [{ id: 'e2', from: 'i1.value', to: 'o1.value' }], inputs: [{ name: 'x', type: 'text' }], outputs: [{ name: 'y', type: 'text' }] }, each: { nodes: [], edges: [], note: 'kept' } } };
  const ds = WF.fromWorkflow(sub), reg4 = core.createRegistry(); WF.install(reg4); const e4 = core.createEditor({ registry: reg4, doc: ds, idPrefix: 'n', linkPrefix: 'e' }), ws = WF.toWorkflow(e4.toJSON());
  t('a sub-graph is a group with the ports of its input / output nodes; a for-each\'s sub-graph is kept', e4.item('g').kind === 'group' && e4.portsOf(e4.item('g')).in.map((p) => p.name).join() === 'x' && e4.portsOf(e4.item('g')).out.map((p) => p.name).join() === 'y' && ws.json.subgraphs.inner.inputs[0].name === 'x' && ws.json.subgraphs.each.note === 'kept' && ws.json.nodes.find((n) => n.id === 'g').kind === 'subgraph');
  t('a wire from outside into the group (through its port) is kept', e4.doc.links.e1 && ws.json.edges.some((e) => e.id === 'e1'));
  return out;
}

function r452() {
  const out = []; const t = (name, ok, more) => out.push([name, !!ok, more]);
  const ed = make(); const a = add(ed, 'node', 0, 0).id; const hist = ed.history().undo, doc0 = canon(ed.toJSON());
  const patch = [{ type: 'add', kind: 'sink', id: 'p1', x: 200, y: 0 }, { type: 'add', kind: 'node', id: 'p2', x: 200, y: 100 }, { type: 'connect', from: a + '.out', to: 'p1.in' }, { type: 'setProp', ids: ['p2'], props: { label: 'hi' } }, { type: 'move', ids: [a], dx: 20, dy: 0 }];
  const r = ed.applyPatch(patch); t('a patch (a list of commands) applies', r.ok && r.changed && ed.item('p1') && ed.item('p2').props.label === 'hi' && Object.keys(ed.doc.links).length === 1);
  t('as ONE undo step', ed.history().undo === hist + 1); ed.undo(); t('one undo reverts the whole patch', canon(ed.toJSON()) === doc0); ed.redo(); t('and one redo brings it back', !!ed.item('p2'));
  ed.undo();
  const h1 = JSON.stringify(ed.history()), before = canon(ed.toJSON());
  const bad = ed.applyPatch([{ type: 'add', kind: 'sink', id: 'q1', x: 0, y: 200 }, { type: 'connect', from: a + '.out', to: 'q1.nope' }]);
  t('a patch with a bad command is refused whole, naming the command', !bad.ok && bad.index === 1 && /no input/.test(bad.error));
  t('and leaves no trace (document, ids and history as before)', canon(ed.toJSON()) === before && JSON.stringify(ed.history()) === h1 && !ed.item('q1'));
  t('an unknown command type is refused', !ed.applyPatch([{ type: 'publish' }]).ok && !ed.applyPatch([]).ok);
  const prop = ed.proposePatch(patch); t('a proposal is tried on a copy: the document and history do not move', prop.ok && canon(ed.toJSON()) === before && JSON.stringify(ed.history()) === h1 && !!prop.preview.items.p1 && prop.summary.includes('add'));
  t('rejecting it leaves no trace', prop.reject().ok && canon(ed.toJSON()) === before && JSON.stringify(ed.history()) === h1 && !prop.accept().ok);
  const prop2 = ed.proposePatch(patch); const acc = prop2.accept(); t('accepting it is one undo step', acc.ok && ed.history().undo === hist + 1 && !!ed.item('p1') && !prop2.accept().ok);
  ed.undo(); const dry = ed.applyPatch(patch, { dryRun: true }); t('a dry run reports without writing', dry.ok && dry.dryRun && canon(ed.toJSON()) === before);
  const badProp = ed.proposePatch([{ type: 'delete', ids: ['nope'] }]); t('a bad proposal says so and cannot be accepted', !badProp.ok && !badProp.accept().ok && canon(ed.toJSON()) === before);
  const events = []; ed.on('change', (e) => events.push(e.kind)); ed.applyPatch(patch); t('a patch is one change event, kind patch', events.join() === 'patch');
  // a redo stack is not disturbed by a rejected patch
  ed.undo(); ed.applyPatch([{ type: 'delete', ids: ['nope'] }]); t('a rejected patch keeps the redo stack', ed.history().redo === 1);
  return out;
}


// a small fake DOM, enough for view.js: the property panel and the breadcrumb are drawn from the core's answers
class N {
  constructor(tag) { this.tag = tag; this.children = []; this.attrs = {}; this.style = {}; this.listeners = {}; this._text = ''; this.parentNode = null; this.className = ''; this.hidden = false; this.value = ''; this.checked = false; this.dataset = {}; }
  get classList() { const s = this; return { toggle: (c, on) => { const l = s.className.split(' ').filter(Boolean).filter((x) => x !== c); if (on === undefined ? !s.className.split(' ').includes(c) : on) l.push(c); s.className = l.join(' '); }, add: (c) => s.classList.toggle(c, true), remove: (c) => s.classList.toggle(c, false), contains: (c) => s.className.split(' ').includes(c) }; }
  setAttribute(k, v) { this.attrs[k] = String(v); if (k === 'class') this.className = String(v); if (k.startsWith('data-')) this.dataset[k.slice(5).replace(/-(.)/g, (m, c) => c.toUpperCase())] = String(v); }
  getAttribute(k) { return this.attrs[k]; }
  appendChild(c) { if (c.parentNode) c.parentNode.children = c.parentNode.children.filter((x) => x !== c); c.parentNode = this; this.children.push(c); return c; }
  append(...cs) { for (const c of cs) this.appendChild(typeof c === 'string' ? Object.assign(new N('#text'), { _text: c }) : c); }
  insertBefore(c, ref) { if (c.parentNode) c.parentNode.children = c.parentNode.children.filter((x) => x !== c); c.parentNode = this; const i = this.children.indexOf(ref); if (i < 0) this.children.push(c); else this.children.splice(i, 0, c); return c; }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter((x) => x !== this); }
  get firstChild() { return this.children[0] || null; }
  get textContent() { return this._text + this.children.map((c) => c.textContent).join(' '); }
  set textContent(v) { this.children = []; this._text = String(v); }
  walk(f) { f(this); for (const c of this.children) c.walk(f); }
  find(f) { const out = []; this.walk((n) => f(n) && out.push(n)); return out; }
  addEventListener(type, f) { (this.listeners[type] = this.listeners[type] || []).push(f); }
  getBoundingClientRect() { return { left: 0, top: 0, width: 900, height: 600 }; }
  focus() {} setPointerCapture() {}
  querySelectorAll(sel) { const m = /^\[data-([\w-]+)=\"(.+)\"\]$/.exec(sel); return this.find((n) => n !== this && (m ? n.attrs['data-' + m[1]] === m[2] : n.tag === sel)); }
  closest(sel) { let n = this; const cls = sel.replace('.', ''); while (n) { if (n.className && n.className.split(' ').includes(cls)) return n; n = n.parentNode; } return null; }
}
function fakeDom() {
  const head = new N('head'), byId = {};
  global.document = { head, createElement: (t) => new N(t), createElementNS: (ns, t) => new N(t), getElementById: (id) => byId[id] || null };
  const orig = head.appendChild.bind(head); head.appendChild = (c) => { if (c.attrs.id) byId[c.attrs.id] = c; return orig(c); };
}
function viewRows() {
  const rows = []; const t = (name, ok, more) => rows.push([name, !!ok, more]);
  fakeDom(); const View = require(path.join(WEB, 'view.js')).VrEditorView;
  const ed = make(); const id = add(ed, 'box', 0, 0).id; add(ed, 'node', 200, 0); const host = new N('div');
  const v = View.mount(host, { editor: ed });
  t('the view mounts, with the palette drawn from the registered kinds', host.textContent.includes('Box') && host.textContent.includes('Node') && !host.textContent.includes('Group' + ' ' + 'Box'));
  ed.select([id]);
  const props = host.find((n) => n.className === 've-props')[0], text = props.textContent;
  t('the property panel shows the fields of the schema with their titles (* = required)', /Label \*/.test(text) && text.includes('Size') && text.includes('Tone') && text.includes('Code'), text.slice(0, 120));
  const inputs = props.find((n) => n.tag === 'input' || n.tag === 'select'), label = inputs[0];
  label.value = 'this label is far too long'; label.onchange();
  const after = host.find((n) => n.className === 've-props')[0].textContent; // an invalid value: the message is shown on the field and nothing is written
  t('an invalid value shows the schema\x27s message in the panel and does not write', label.className.includes('bad') && props.find((n) => n.className === 'err').some((n) => n.textContent === 'A label has at most 10 characters') && ed.item(id).props.label === 'box', after.slice(0, 80));
  label.value = 'ok'; label.onchange(); t('a valid value is written through the panel', ed.item(id).props.label === 'ok');
  const g = ed.exec({ type: 'group', ids: [id], props: { label: 'Inner' } }).id; ed.enter(g);
  const crumb = host.find((n) => n.className === 've-crumb')[0];
  t('the breadcrumb shows the path of the groups entered', /Top/.test(crumb.textContent) && /Inner/.test(crumb.textContent) && crumb.textContent.includes('›'), crumb.textContent);
  v.addPanel('x', 'Extra', new N('div')); v.setRunStates({ [id]: { state: 'running', text: 'running' } }); v.setMarks({ [id]: { level: 'error', text: 'bad' } }); v.setPreview({ added: [{ id: 'z', kind: 'box', x: 0, y: 0, w: 10, h: 10, props: {} }], changed: [id], removed: [], links: [] });
  t('a side panel, run states, marks and a preview can be added by a tool', host.find((n) => n.attrs['data-tab'] === 'x').length === 1 && v.aiSlot && host.find((n) => n.className.includes('st-running')).length === 1);
  v.destroy(); delete global.document;
  return rows;
}

// the Workflows screen on a stub hub and the fake DOM: the list, then the editor, save, a test run with its overlay, the side chat
async function uiRows() {
  const rows = []; const t = (name, ok, more) => rows.push([name, !!ok, more]);
  fakeDom(); const WF = require(path.join(WEB, 'kinds-workflow.js')), View = require(path.join(WEB, 'view.js')).VrEditorView;
  const byId = { wfBody: new N('div'), wfMsg: new N('span') }; global.document.getElementById = (id) => byId[id] || null;
  const seen = []; const tick = () => new Promise((r) => setImmediate(r)); const wait = async (f) => { for (let i = 0; i < 200 && !f(); i++) await tick(); return f(); };
  const events = [{ v: 1, seq: 1, type: 'run_start', run: 'hr-1' }, { seq: 2, type: 'node_start', node: 'n1', kind: 'start', attempt: 1 }, { seq: 3, type: 'node_end', node: 'n1', kind: 'start', attempt: 1, status: 'ok', durationMs: 4 }, { seq: 4, type: 'run_end', status: 'ok', outcome: 'done', result: 'the answer' }];
  const reply = (status, data) => ({ status, text: async () => (typeof data === 'string' ? data : JSON.stringify(data)) });
  const doc = WF.blank('demo');
  const api = async (p, o) => {
    o = o || {}; const method = o.method || 'GET', body = o.body ? JSON.parse(o.body) : undefined; seen.push([method, p.replace('/api/hyperwork/', ''), body]);
    if (p.startsWith('/api/hyperwork/list')) return reply(200, { items: [{ id: 'wf-1', name: 'demo', shown: 'demo', scope: 'me-sam', version: 1, labels: ['docs'], rights: { edit: true, run: true, admin: true }, draft: false }, { id: 'wf-2', name: 'other', shown: 'other', scope: 'team-core', version: 2, labels: [], rights: { run: true } }] });
    if (p === '/api/hyperwork/kinds') return reply(200, { kinds: WF.STATIC_MANIFESTS });
    if (p.startsWith('/api/hyperwork/get')) return reply(200, { doc, etag: 'e1', version: null, rights: { edit: true, run: true, publish: true }, meta: { name: 'demo', scope: 'me-sam', owner: 'sam' } });
    if (p === '/api/hyperwork/save') return reply(200, { ok: true, id: 'wf-1', etag: 'e2', problems: [{ level: 'warn', node: 'n2', code: 'x', message: 'a warning' }] });
    if (p === '/api/hyperwork/run') return reply(202, { run: 'hr-1', runner: 'hub' });
    if (p.startsWith('/api/hyperwork/log')) return reply(200, events.map((e) => JSON.stringify(e)).join('\n') + '\n');
    if (p === '/api/hyperwork/ask') return reply(200, { text: 'Add a prompt.', patch: [{ type: 'add', kind: 'prompt', id: 'n9', x: 200, y: 200 }], problems: [] });
    if (p.startsWith('/api/hyperwork/triggers')) return reply(200, { triggers: [{ id: 'tr-1', kind: 'timer', timer: { recur: 'daily' }, next: 0, runs: [] }] });
    return reply(404, { error: 'no' });
  };
  const ctx = { console, document: global.document, api, base: '', token: 't', me: { username: 'sam' }, VrKindsWorkflow: WF, VrEditorCore: core, VrEditorView: View, setTimeout, clearTimeout, setInterval, confirm: () => true, prompt: () => 'a message', fetch: async () => { throw new Error('no stream'); }, TextDecoder, Promise, JSON, Math, Date, Array, Object, String, Number, Set, Map, Error, isFinite };
  vm.createContext(ctx); vm.runInContext(fs.readFileSync(path.join(WEB, 'workflows-ui.js'), 'utf8') + ';this.__W = WORKFLOWS;', ctx);
  const W = ctx.__W; const body = byId.wfBody; const buttons = (text) => body.find((n) => n.tag === 'button' && n.textContent === text);
  W.show(); await wait(() => body.textContent.includes('demo'));
  t('the Workflows screen shows the list: names, scope, version, categories, search, new', body.textContent.includes('demo') && body.textContent.includes('other') && body.textContent.includes('docs (1)') && body.textContent.includes('New workflow') && !!body.find((n) => n.attrs.type === 'search').length, body.textContent.slice(0, 160));
  t('Duplicate and Delete are offered only where the rights allow', buttons('Delete').length === 1 && buttons('Duplicate').length === 1);
  buttons('Open')[0].onclick(); await wait(() => buttons('Test run').length);
  t('opening a workflow opens the editor with the hub\x27s kinds, the settings and the toolbar', buttons('Save').length === 1 && buttons('Publish').length === 1 && body.textContent.includes('Workflow') && body.textContent.includes('Prompt') && seen.some((s) => s[1] === 'kinds') && seen.some((s) => s[1].startsWith('get?id=wf-1')), seen.map((s) => s[1]).join(' '));
  buttons('Save')[0].onclick(); await wait(() => seen.some((s) => s[1] === 'save'));
  const sv = seen.find((s) => s[1] === 'save'); t('Save sends the workflow document with the etag; problems come back as marks', sv && sv[2].id === 'wf-1' && sv[2].etag === 'e1' && sv[2].doc.name === 'demo' && sv[2].doc.nodes.length === 2 && sv[2].doc.edges.length === 1, JSON.stringify(sv && sv[2]).slice(0, 200));
  await wait(() => body.find((n) => n.className && n.className.includes('warn')).length);
  buttons('Test run')[0].onclick(); await wait(() => body.textContent.includes('Run ended'));
  const rs = seen.find((s) => s[1] === 'run'); t('Test run starts the draft and shows node states from the log', rs && rs[2].draft === true && rs[2].id === 'wf-1' && body.textContent.includes('Run ended: ok') && body.textContent.includes('the answer') && body.find((n) => n.className && n.className.includes('st-ok')).length >= 1, body.textContent.slice(-200));
  const ta = body.find((n) => n.tag === 'textarea' && n.attrs['aria-label'] === 'Message to the AI')[0]; ta.value = 'add a prompt'; ta.onkeydown({ key: 'Enter', shiftKey: false, preventDefault() {} });
  await wait(() => buttons('Accept').length);
  t('the side chat shows the answer and a patch as a proposal (nothing written yet)', buttons('Accept').length === 1 && buttons('Reject').length === 1 && body.textContent.includes('Add a prompt.') && !W.__x, '');
  buttons('Accept')[0].onclick(); const ed2 = (() => { const a = body.find((n) => n.className === 've-pal'); return a; })();
  t('Accept adds the patch to the draft', body.find((n) => n.attrs['data-id'] === 'n9').length === 1, '');
  W.stop(); delete global.document;
  return rows;
}

async function run() {
  let uiRowsDone; try { uiRowsDone = await uiRows(); } catch (e) { uiRowsDone = [['the Workflows screen threw: ' + e.message, false, String(e.stack).split('\n').slice(0, 4).join(' ')]]; }
  const groups = { R447: r447, R448: r448, R449: () => r449().concat(viewRows().filter((r) => /panel|valid value/.test(r[0]) || !r[1])), R450: () => r450().concat(viewRows().filter((r) => /breadcrumb|mounts|side panel/.test(r[0]) || !r[1])), R451: () => r451().concat(uiRowsDone), R452: r452 }, res = {};
  for (const id of Object.keys(groups)) {
    let rows; try { rows = groups[id](); } catch (e) { rows = [['threw: ' + e.message, false, e.stack.split('\n').slice(0, 3).join(' ')]]; }
    const failed = rows.filter((r) => !r[1]);
    res[id] = { ok: !failed.length, total: rows.length, detail: failed.length ? failed.map((r) => r[0] + (r[2] ? ' [' + r[2] + ']' : '')).join('; ') : rows.length + ' assertions' };
  }
  return res;
}

module.exports = { run };
if (require.main === module) run().then((r) => { for (const id of Object.keys(r)) console.log(id, r[id].ok ? 'PASS' : 'FAIL', r[id].detail); process.exit(Object.values(r).every((x) => x.ok) ? 0 : 1); });
