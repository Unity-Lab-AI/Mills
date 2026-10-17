// Runs web/md.js (the chat markdown renderer) against a tiny fake DOM and checks what it builds.
// Used by regress.js (R67); also runnable on its own: node bridge/test/md-check.js
'use strict';
const fs = require('fs');
const path = require('path');
const vm = require('vm');

class Text { constructor(s) { this.data = String(s); this.parentNode = null; } get textContent() { return this.data; } }
class El {
  constructor(tag) {
    this.tagName = tag.toUpperCase(); this.childNodes = []; this.parentNode = null; this.className = ''; this.style = {}; this.dataset = {}; this.attributes = [];
    this.classList = { add: (c) => { if (!this.className.split(' ').includes(c)) this.className = (this.className + ' ' + c).trim(); } };
  }
  _adopt(n) { if (typeof n === 'string') n = new Text(n); if (n.parentNode) n.parentNode.removeChild(n); n.parentNode = this; return n; }
  append(...ns) { for (const n of ns) this.childNodes.push(this._adopt(n)); }
  appendChild(n) { this.append(n); return n; }
  removeChild(n) { const i = this.childNodes.indexOf(n); if (i >= 0) this.childNodes.splice(i, 1); n.parentNode = null; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  replaceWith(...ns) {
    const p = this.parentNode, list = [...ns];
    const at = p.childNodes.indexOf(this); p.removeChild(this);
    const nodes = list.map((n) => (typeof n === 'string' ? new Text(n) : n));
    for (const n of nodes) { if (n.parentNode) n.parentNode.removeChild(n); n.parentNode = p; }
    p.childNodes.splice(at, 0, ...nodes);
  }
  get children() { return this.childNodes.filter((n) => n instanceof El); }
  set textContent(v) { for (const c of this.childNodes) c.parentNode = null; this.childNodes = []; if (v !== '' && v != null) this.append(String(v)); }
  get textContent() { return this.childNodes.map((c) => c.textContent).join(''); }
  addEventListener() {}
  querySelector() { return null; }
  querySelectorAll() { return []; }
  contains() { return false; }
}
function html(n) {
  if (n instanceof Text) return n.data.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  const tag = n.tagName.toLowerCase();
  const attrs = [n.className && `class="${n.className}"`, n.href && `href="${n.href}"`, n.type && `type="${n.type}"`, n.checked && 'checked', n.style.textAlign && `align="${n.style.textAlign}"`, n.start && `start="${n.start}"`].filter(Boolean);
  return `<${tag}${attrs.length ? ' ' + attrs.join(' ') : ''}>${n.childNodes.map(html).join('')}</${tag}>`;
}

function load() {
  const head = new El('head');
  const ctx = {
    document: { createElement: (t) => new El(t), head, querySelectorAll: () => [] },
    location: { href: 'https://example.test/' }, CSS: { escape: (s) => s }, URL, setTimeout, Promise,
  };
  ctx.window = ctx;
  vm.createContext(ctx);
  vm.runInContext(fs.readFileSync(path.join(__dirname, '..', '..', 'web', 'md.js'), 'utf8'), ctx);
  return { MD: ctx.MD, render: (text, opts) => html(ctx.MD.render(new El('div'), text, opts)), head };
}

async function run() {
  const { render, head, MD } = load();
  const cases = [
    ['heading + bold/italic/strike/highlight/code', '# Title\nSome **bold**, *it*, ~~gone~~, ==hi== and `x<y`',
      (h) => h.includes('<h1 class="mdh">Title</h1>') && h.includes('<strong>bold</strong>') && h.includes('<em>it</em>') && h.includes('<del>gone</del>') && h.includes('<mark>hi</mark>') && h.includes('<code class="mdc">x&lt;y</code>')],
    ['line breaks kept (chat style)', 'one\ntwo', (h) => h.includes('one<br></br>two')],
    ['raw HTML stays text', '<img src=x onerror=alert(1)><script>alert(1)</script>', (h) => !/<img|<script/.test(h) && h.includes('&lt;script&gt;')],
    ['javascript: link refused', '[click](javascript:alert(1))', (h) => !h.includes('<a') && h.includes('javascript:alert(1)')],
    ['http link + autolink', '[site](https://a.test/x) and https://b.test/y.', (h) => h.includes('href="https://a.test/x"') && h.includes('href="https://b.test/y"') && /y<\/a>\.<\/p>/.test(h)],
    ['snake_case not italic', 'call my_var_name now', (h) => !h.includes('<em>')],
    ['nested + task lists', '- a\n  - a1\n- [x] done\n- [ ] todo\n\n3. three\n4. four',
      (h) => /<ul class="mdlist"><li>a<ul class="mdlist"><li>a1<\/li><\/ul><\/li>/.test(h) && h.includes('type="checkbox" checked') && h.includes('<ol class="mdlist" start="3">')],
    ['table with alignment', '| a | b |\n|:--|--:|\n| 1 | `x|y` |', (h) => h.includes('<th align="left">a</th>') && h.includes('<td align="right"><code class="mdc">x|y</code></td>')],
    ['quote + alert', '> plain\n\n> [!WARNING]\n> careful', (h) => h.includes('<blockquote><p>plain</p></blockquote>') && h.includes('mdalert-warning') && h.includes('Warning')],
    ['code block: label, copy, colouring', '```js\nconst x = "s"; // c\n```', (h) => h.includes('<span>js</span>') && h.includes('Copy') && h.includes('<span class="hl-kw">const</span>') && h.includes('<span class="hl-str">"s"</span>') && h.includes('<span class="hl-com">// c</span>')],
    ['unclosed fence (still streaming) renders', '```py\nprint(1)', (h) => h.includes('mdcode') && h.includes('print')],
    ['mermaid block → diagram box with source', '```mermaid\ngraph TD; A-->B\n```', (h) => h.includes('mdmermaid') && h.includes('Drawing diagram') && h.includes('graph TD; A--&gt;B')],
    ['math block shown as source', '$$\na^2+b^2\n$$', (h) => h.includes('<pre class="mdmath">a^2+b^2</pre>')],
    ['footnotes', 'Fact[^1].\n\n[^1]: Source.', (h) => h.includes('<sup class="mdfnref">[1]</sup>') && h.includes('mdfns') && h.includes('Source.')],
    ['hr', 'a\n\n---\n\nb', (h) => h.includes('<hr></hr>')],
    ['mention hook', 'hi @sam and @nobody', (h) => h.includes('<b class="m">@sam</b>') && h.includes('@nobody'), { mention: (x) => { if (x !== 'sam') return null; const b = new El('b'); b.className = 'm'; b.textContent = '@' + x; return b; } }],
    ['escapes', '\\*not italic\\*', (h) => !h.includes('<em>') && h.includes('*not italic*')],
    ['https image loads (lazy, no referrer, opens full size)', '![a cat](https://img.test/cat.png)', (h) => /<a class="mdimgwrap" href="https:\/\/img\.test\/cat\.png"><img class="mdimg"><\/img><\/a>/.test(h)],
    ['data image loads inline', '![](data:image/png;base64,iVBORw0KGgo=)', (h) => /<span class="mdimgwrap"><img class="mdimg">/.test(h)],
    ['http (not https) image is tried over https (link kept as the fallback)', '![x](http://img.test/x.png)', (h) => /<a class="mdimgwrap" href="http:\/\/img\.test\/x\.png"><img class="mdimg">/.test(h)],
    ['relative image path is handed to the page resolver (folder on your PC)', '![shot](out/shot.png)', (h) => /<span class="mdimgwrap mdimgwait"><img class="mdimg">/.test(h)],
    ['relative image path without a resolver stays text', '![shot](out/shot.png)', (h) => !h.includes('<img') && h.includes('![shot](out/shot.png)'), undefined, { resolveImage: null }],
    ['javascript: image refused', '![x](javascript:alert(1))', (h) => !h.includes('<img') && !h.includes('<a')],
  ];
  const failed = [];
  MD.resolveImage = () => Promise.resolve(null); // the page sets this; cases may override it per run (5th element)
  for (const [name, input, ok, opts, over] of cases) {
    let out = '';
    const saved = { ...over };
    for (const k of Object.keys(saved)) { saved[k] = MD[k]; MD[k] = over[k]; }
    try { out = render(input, opts); } catch (e) { out = 'THREW ' + e.message; }
    for (const k of Object.keys(saved)) MD[k] = saved[k];
    if (!ok(out)) failed.push(name + ' → ' + out.slice(0, 300));
  }
  await new Promise((r) => setTimeout(r, 50)); // the diagram library is requested asynchronously
  if (!head.childNodes.some((s) => s.src === 'vendor/mermaid.min.js')) failed.push('mermaid library not requested on demand');
  if (head.childNodes.length !== 1) failed.push('mermaid library requested more than once');
  return { total: cases.length + 2, failed };
}

module.exports = { run };
if (require.main === module) {
  run().then((r) => {
  console.log(r.failed.length ? 'FAILED:\n' + r.failed.join('\n') : `md-check: all ${r.total} cases pass`);
  process.exit(r.failed.length ? 1 : 0);
  });
}
