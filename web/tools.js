// VibeRoller Work: the tool calls in the transcript (T-318). Calls are bundled by consecutive runs of the same kind, in
// the order they arrive (2 Reads, then an Edit, then 3 Bash calls = three groups; an answer in between starts a new
// group), the same live while a reply streams as on a restored conversation, since both add one call at a time at the
// end of the log. A group is collapsed by default ("show tools"); open, each call is one line (32 characters whole,
// longer: the first 15, "…", the last 15) and a click shows it whole. "Always expand" opens them all (the page keeps
// the choice per person). A command shown whole has its parts coloured by risk, each with a marker shape and words
// (colour-blind safe, like the session dots of web/sessions.js); nothing is blocked, it is only for seeing risk at a glance.
// The text of a call is the page's describeTool() line: "<tool name>  <its target>" (two spaces).
// Loaded before the main page script; the page keeps its old one-line chips when this file is missing. The pure part at
// the top (no DOM) and add() with a plain element API are what the suite runs in node (R528, R529).
(function () {
  'use strict';
  const W = typeof window !== 'undefined' ? window : globalThis;

  // ── pure helpers: a call's line ──
  const clean = (t) => String(t == null ? '' : t).replace(/^\u2699\uFE0F?\s*/, ''); // older saved sessions carry a gear prefix
  function parse(text) {
    const t = clean(text), at = t.indexOf('  ');
    return at < 0 ? { name: t.trim(), target: '' } : { name: t.slice(0, at).trim(), target: t.slice(at + 2) };
  }
  const kindOf = (text) => parse(text).name || 'tool';
  // one line: up to max characters whole, longer = the first keep, "…", the last keep
  function short(s, max = 32, keep = 15) {
    const t = String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
    return t.length <= max ? t : t.slice(0, keep) + '…' + t.slice(-keep);
  }
  function summary(text) { const p = parse(text); return p.name.toLowerCase() + (p.target.trim() ? ' ' + short(p.target) : ''); }
  // consecutive runs of the same kind; items are call lines or transcript entries ({ cls, text }: anything but a tool breaks a run)
  function groups(items) {
    const out = []; let open = null;
    for (const x of items || []) {
      const t = typeof x === 'string' ? x : x && x.cls === 'tool' ? x.text : null;
      if (t === null) { open = null; continue; }
      const k = kindOf(t);
      if (open && open.kind === k) open.items.push(t); else out.push(open = { kind: k, items: [t] });
    }
    return out;
  }
  // the tools whose target is a shell command (Claude Code's Bash, the linked machine's shell_run / job_start, the RE containers)
  const isCommand = (kind) => /(^|[\s_])(bash|shell_run|job_start|re_run|re_exec|powershell)$/i.test(String(kind));

  // ── the risk of a command ──
  // Levels, each with a shape and words so colour is never the only sign.
  const LEVELS = {
    safe: { rank: 0, mark: '●', name: 'looks safe' },
    unknown: { rank: 1, mark: '?', name: 'unknown' },
    caution: { rank: 2, mark: '▲', name: 'check it' },
    danger: { rank: 3, mark: '◆', name: 'possibly unsafe' },
  };
  // The rule table (kept small on purpose; anything not in it is "unknown"):
  //   possibly unsafe  rm rmdir unlink shred wipefs dd mkfs* fdisk sfdisk parted truncate reboot shutdown poweroff halt,
  //                    chmod / chown / chgrp -R, cp --remove-destination, find -delete / -exec rm, curl|wget piped into a shell,
  //                    git reset --hard, push --force / -f / --force-with-lease / --mirror / --delete / +ref / :ref,
  //                    clean -f, branch -D / -d, stash drop / clear, filter-branch / filter-repo
  //   check it         mv, chmod / chown / chgrp, cp -f, rsync --delete* / --del / --remove-source-files, sed -i, tee,
  //                    kill pkill killall, find -exec, a > that writes over a file (not >> or /dev/null), sudo / doas,
  //                    git push pull merge rebase checkout switch restore reset stash cherry-pick revert am apply rm mv
  //                    worktree submodule gc
  //   looks safe       ls cat head tail less grep rg find echo printf pwd cd wc sort uniq diff which stat file du df ps
  //                    date mkdir touch tree jq … (SAFE below), cp, rsync, sed, git status diff log show blame fetch add commit …
  //   unknown          everything else (curl / wget alone, node, cargo, a script run by sh / bash, …)
  const SAFE = new Set(('ls ll la cat head tail less more grep egrep fgrep rg ag echo printf pwd cd pushd popd wc sort uniq diff cmp comm '
    + 'which whereis type stat file du df ps date true false test [ mkdir touch tree jq yq basename dirname realpath readlink id whoami '
    + 'uname hostname sleep cut tr column nl seq md5sum sha1sum sha256sum b2sum cksum printenv export history').split(' '));
  const GIT_SAFE = new Set('status diff log show blame grep ls-files ls-tree rev-parse describe shortlog reflog fetch remote branch tag config add commit help version cat-file'.split(' '));
  const GIT_CAUTION = new Set('push pull merge rebase checkout switch restore reset stash cherry-pick revert am apply rm mv worktree submodule gc'.split(' '));
  const PREFIX = new Set('sudo doas env nohup time nice ionice command exec xargs timeout stdbuf'.split(' ')); // run the word after them
  const SHELLS = /^(sh|bash|zsh|dash|ksh)$/;
  const lvl = (level, why) => ({ level, why });
  const base = (w) => String(w).replace(/^.*\//, '');
  function git(a) {
    let i = 0; while (i < a.length && a[i].startsWith('-')) i += /^-[Cc]$/.test(a[i]) ? 2 : 1; // git -C dir / -c k=v
    const sub = a[i] || '', r = a.slice(i + 1), has = (re) => r.some((x) => re.test(x));
    if (sub === 'reset' && has(/^--hard$/)) return lvl('danger', 'throws away uncommitted changes');
    if (sub === 'push' && (has(/^(-f|--force|--force-with-lease.*|--mirror|--delete|-d)$/) || has(/^[+:]/))) return lvl('danger', 'rewrites or deletes history on the remote');
    if (sub === 'clean' && has(/^-[a-zA-Z]*f/)) return lvl('danger', 'deletes untracked files');
    if (sub === 'branch' && has(/^(-[a-zA-Z]*[Dd]|--delete)$/)) return lvl('danger', 'deletes a branch');
    if (sub === 'stash' && /^(drop|clear)$/.test(r[0] || '')) return lvl('danger', 'throws away stashed changes');
    if (/^filter-(branch|repo)$/.test(sub)) return lvl('danger', 'rewrites history');
    if (GIT_SAFE.has(sub)) return lvl('safe', 'git ' + sub);
    if (GIT_CAUTION.has(sub)) return lvl('caution', 'git ' + sub + ' changes the repository');
    return lvl('unknown', 'git ' + (sub || '') + ' is not in the rule table');
  }
  function classify(cmd, a) {
    const sf = (ch) => a.some((x) => /^-[A-Za-z]+$/.test(x) && x.includes(ch)), lf = (re) => a.some((x) => re.test(x));
    switch (cmd) {
      case 'rm': case 'rmdir': case 'unlink': case 'shred': case 'wipefs': return lvl('danger', 'deletes files');
      case 'dd': return lvl('danger', 'writes raw data (can overwrite a disk)');
      case 'fdisk': case 'sfdisk': case 'parted': return lvl('danger', 'changes disk partitions');
      case 'truncate': return lvl('danger', 'cuts files short');
      case 'reboot': case 'shutdown': case 'poweroff': case 'halt': return lvl('danger', 'stops the machine');
      case 'chmod': case 'chown': case 'chgrp': return sf('R') || lf(/^--recursive$/) ? lvl('danger', 'changes a whole tree at once (-R)') : lvl('caution', 'changes permissions or owner');
      case 'mv': return lvl('caution', 'moves files (a target is overwritten)');
      case 'cp': return lf(/^--remove-destination$/) ? lvl('danger', 'deletes each destination before copying')
        : sf('f') || lf(/^--force$/) ? lvl('caution', 'may delete a destination it cannot open (-f)') : lvl('safe', 'copies');
      case 'rsync': return lf(/^--(delete|del$|remove-source-files)/) ? lvl('caution', 'deletes files (--delete / --remove-source-files)') : lvl('safe', 'copies');
      case 'find': {
        const ex = a.findIndex((x) => /^-(exec|execdir|ok|okdir)$/.test(x));
        if (lf(/^-delete$/) || (ex >= 0 && /^(rm|rmdir|unlink|shred)$/.test(base(a[ex + 1] || '')))) return lvl('danger', 'deletes the files it finds');
        return ex >= 0 ? lvl('caution', 'runs a command on each file') : lvl('safe', 'lists files');
      }
      case 'sed': return lf(/^-[a-zA-Z]*i|^--in-place/) ? lvl('caution', 'edits files in place (-i)') : lvl('safe', 'reads');
      case 'tee': return lvl('caution', 'writes files');
      case 'kill': case 'pkill': case 'killall': return lvl('caution', 'stops processes');
      case 'git': return git(a);
      case 'curl': case 'wget': return lvl('unknown', 'fetches from the network');
    }
    if (/^mkfs(\.|$)/.test(cmd)) return lvl('danger', 'formats a disk');
    if (SAFE.has(cmd)) return lvl('safe', 'only reads or looks');
    return lvl('unknown', cmd + ' is not in the rule table');
  }
  // the commands of a line and the && || ; | & between them (not inside quotes or $( … ))
  function splitTop(s) {
    const out = []; let q = '', depth = 0, start = 0;
    const cut = (i, n) => { if (i > start) out.push({ text: s.slice(start, i) }); out.push({ text: s.slice(i, i + n), sep: true }); start = i + n; };
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (q) { if (c === '\\' && q !== "'") i++; else if (c === q) q = ''; continue; }
      if (c === '\\') { i++; continue; }
      if (c === "'" || c === '"' || c === '`') { q = c; continue; }
      if (c === '$' && s[i + 1] === '(') { depth++; i++; continue; }
      if (c === ')' && depth) { depth--; continue; }
      if (depth) continue;
      const two = s.slice(i, i + 2);
      if (two === '&&' || two === '||' || two === '|&') { cut(i, 2); i++; }
      else if (c === ';' || c === '\n' || c === '|') cut(i, 1);
      else if (c === '&' && s[i - 1] !== '>' && s[i + 1] !== '>') cut(i, 1);
    }
    if (start < s.length) out.push({ text: s.slice(start) });
    return out;
  }
  // shell words with their place in s (raw) and without quotes (v)
  function words(s) {
    const out = []; let i = 0;
    while (i < s.length) {
      while (i < s.length && /\s/.test(s[i])) i++;
      if (i >= s.length) break;
      const at = i; let v = '', q = '';
      for (; i < s.length; i++) {
        const c = s[i];
        if (q) { if (c === q) q = ''; else if (c === '\\' && q === '"' && i + 1 < s.length) v += s[++i]; else v += c; continue; }
        if (/\s/.test(c)) break;
        if (c === "'" || c === '"') q = c;
        else if (c === '\\' && i + 1 < s.length) v += s[++i];
        else v += c;
      }
      out.push({ raw: s.slice(at, i), v, at });
    }
    return out;
  }
  // a > that writes over a file (not >>, not 2>&1, not /dev/null)
  function overwrites(ws) {
    for (let k = 0; k < ws.length; k++) {
      const m = /^[0-9&]?>(?![>&])(.*)$/.exec(ws[k].raw);
      if (m && (m[1] || (ws[k + 1] && ws[k + 1].v) || '') !== '/dev/null') return true;
    }
    return false;
  }
  function command(core, prev, out) {
    const ws = words(core); let i = 0, root = false;
    while (i < ws.length) { // VAR=x, sudo, env, xargs, …: the command is the word after them
      const b = base(ws[i].v);
      if (/^[A-Za-z_]\w*=/.test(ws[i].v)) { i++; continue; }
      if (!PREFIX.has(b)) break;
      if (b === 'sudo' || b === 'doas') root = true;
      i++; while (i < ws.length && /^-/.test(ws[i].v)) i++;
      if (b === 'timeout' && i < ws.length && /^\d/.test(ws[i].v)) i++;
    }
    if (i >= ws.length) { out.push({ text: core }); return; }
    const name = base(ws[i].v).replace(/^\(+/, '');
    if (SHELLS.test(name)) {
      const j = ws.findIndex((w, k) => k > i && /^-[a-z]*c[a-z]*$/.test(w.v)), sc = j > 0 ? ws[j + 1] : null;
      if (sc && sc.raw.length >= 2 && /^['"]/.test(sc.raw) && sc.raw.endsWith(sc.raw[0])) { // sh -c '…': the wrapper stays plain, its script is looked into
        const a = sc.at + 1, z = sc.at + sc.raw.length - 1;
        out.push({ text: core.slice(0, a) }); scan(core.slice(a, z), out); out.push({ text: core.slice(z) });
        return;
      }
      if (j < 0 && /^\|&?$/.test(prev) && !ws.slice(i + 1).some((w) => !/^-/.test(w.v))) { out.push({ text: core, level: 'danger', why: 'runs what is piped into it as a script', cmd: name, piped: true }); return; }
      out.push({ text: core, level: 'unknown', why: 'runs a script', cmd: name });
      return;
    }
    let r = classify(name, ws.slice(i + 1).map((w) => w.v));
    if (r.level !== 'danger' && overwrites(ws.slice(i + 1))) r = lvl('caution', 'writes over a file (>)');
    if (root) r = lvl(LEVELS[r.level].rank < 2 ? 'caution' : r.level, r.why + ', as root');
    out.push({ text: core, level: r.level, why: r.why, cmd: name });
  }
  function scan(s, out) {
    let prev = '';
    for (const p of splitTop(s)) {
      if (p.sep) { out.push({ text: p.text }); prev = p.text.trim(); continue; }
      const lead = /^\s*/.exec(p.text)[0], core = p.text.trim(), tail = p.text.slice(lead.length + core.length);
      if (lead) out.push({ text: lead });
      if (core) command(core, prev, out);
      if (tail) out.push({ text: tail });
      prev = '';
    }
    return out;
  }
  // { level (the worst part), parts: [{ text, level?, why? }] }: the parts put together are the command again; only the
  // commands in it have a level (separators, sh -c '…' and blanks have none)
  function risk(cmd) {
    const parts = scan(String(cmd == null ? '' : cmd), []);
    parts.forEach((p, k) => { // curl … | sh: the download is part of it
      if (!p.piped) return;
      for (let m = k - 1; m >= 0; m--) if (parts[m].level) { if (/^(curl|wget)$/.test(parts[m].cmd)) Object.assign(parts[m], { level: 'danger', why: 'downloads a script that is run' }); break; }
    });
    let level = null;
    for (const p of parts) if (p.level && (!level || LEVELS[p.level].rank > LEVELS[level].rank)) level = p.level;
    const out = []; // plain stretches next to each other are one part
    for (const p of parts) {
      if (p.level) out.push({ text: p.text, level: p.level, why: p.why });
      else if (out.length && !out[out.length - 1].level) out[out.length - 1].text += p.text;
      else if (p.text) out.push({ text: p.text });
    }
    return { level: level || 'unknown', parts: out };
  }

  // ── the view: groups in a log (o.document: a stand-in in the suite) ──
  // o: { document, open(): the person's "always expand", setOpen(on): keep it }
  function add(log, text, o) {
    const doc = o.document || W.document, t = clean(text), kind = kindOf(t);
    const el = (tag, cls, txt) => { const e = doc.createElement(tag); if (cls) e.className = cls; if (txt !== undefined) e.textContent = txt; return e; };
    const mark = (level, why) => { const L = LEVELS[level], m = el('span', 'tkMark ' + level, L.mark); m.title = L.name + (why ? ': ' + why : ''); m.setAttribute('aria-label', m.title); return m; };
    const on = !!(o.open && o.open());
    const last = log.lastElementChild;
    let g = last && last.classList && last.classList.contains('tGroup') && last.dataset.kind === kind ? last : null;
    if (!g) {
      g = el('div', 'tool tGroup'); g.dataset.kind = kind;
      const head = el('button', 'tgHead'); head.type = 'button';
      const list = el('div', 'tgList');
      g.append(head, list);
      head.onclick = () => { const open = !g.classList.contains('open'); setGroup(g, open); const rows = callsOf(g); if (open && rows.length === 1) setCall(rows[0], true); };
      log.appendChild(g);
      setGroup(g, on);
    }
    // the call: its one line, and the whole of it (a command: its parts coloured, each with its marker)
    const p = parse(t), cmd = isCommand(kind) && p.target.trim() ? risk(p.target) : null;
    const c = el('div', 'tCall'); c.dataset.level = cmd ? cmd.level : ''; c.dataset.text = t;
    const sum = el('button', 'tcSum'); sum.type = 'button';
    if (cmd) sum.append(mark(cmd.level, cmd.level === 'safe' ? '' : (cmd.parts.find((x) => x.level === cmd.level) || {}).why));
    sum.append((cmd ? ' ' : '') + summary(t));
    const full = el('div', 'tcFull');
    full.append(el('span', 'tcName', p.name + (p.target ? '  ' : '')));
    if (cmd) for (const x of cmd.parts) {
      if (!x.level) { full.append(x.text); continue; }
      const s = el('span', 'tk ' + x.level); s.title = LEVELS[x.level].name + ': ' + x.why;
      s.append(mark(x.level, x.why), x.text); full.append(s);
    } else full.append(p.target);
    sum.onclick = () => setCall(c, full.hidden);
    c.append(sum, full);
    g.children[1].appendChild(c);
    setCall(c, on);
    drawHead(g, doc, o);
    return g;
  }
  const callsOf = (g) => [...g.children[1].children].filter((x) => x.classList && x.classList.contains('tCall'));
  function setCall(c, on) { c.classList.toggle('open', !!on); c.children[1].hidden = !on; }
  function setGroup(g, on) {
    g.classList.toggle('open', !!on); g.children[1].hidden = !on;
    const h = g.children[0]; h.setAttribute('aria-expanded', String(!!on));
    const tg = [...h.children].find((x) => x.classList && x.classList.contains('tgToggle')); if (tg) tg.textContent = on ? 'hide tools' : 'show tools';
  }
  // the head: kind, how many, the newest call's target, the worst marker when it is worth a look, show / hide;
  // the open list ends with "always expand" (the person's choice, for every group)
  function drawHead(g, doc, o) {
    const el = (tag, cls, txt) => { const e = doc.createElement(tag); if (cls) e.className = cls; if (txt !== undefined) e.textContent = txt; return e; };
    const rows = callsOf(g), h = g.children[0], newest = parse(rows[rows.length - 1].dataset.text || '');
    let worst = '';
    for (const c of rows) if (c.dataset.level && (!worst || LEVELS[c.dataset.level].rank > LEVELS[worst].rank)) worst = c.dataset.level;
    h.textContent = '';
    h.append(el('span', 'tgKind', g.dataset.kind));
    if (rows.length > 1) h.append(el('span', 'tgN', '×' + rows.length));
    if (newest.target.trim()) h.append(el('span', 'tgSum', short(newest.target)));
    if (worst === 'caution' || worst === 'danger') { const m = el('span', 'tkMark ' + worst, LEVELS[worst].mark + ' ' + LEVELS[worst].name); m.title = 'The riskiest call in this group: ' + LEVELS[worst].name + ' (nothing is blocked)'; h.append(m); }
    h.append(el('span', 'tgToggle', g.classList.contains('open') ? 'hide tools' : 'show tools'));
    h.title = rows.length + ' ' + g.dataset.kind + ' call' + (rows.length === 1 ? '' : 's') + ' in a row';
    const list = g.children[1];
    let foot = [...list.children].find((x) => x.classList && x.classList.contains('tgFoot'));
    if (!foot) {
      foot = el('div', 'tgFoot');
      const all = el('button', 'tgAll ghost'); all.type = 'button';
      all.onclick = () => { const v = !(o.open && o.open()); if (o.setOpen) o.setOpen(v); openAll(g.parentNode, v); };
      foot.append(all);
      if (isCommand(g.dataset.kind)) foot.append(el('span', 'tgLegend', Object.values(LEVELS).map((L) => L.mark + ' ' + L.name).join('  ·  ') + '  ·  nothing is blocked'));
    }
    list.appendChild(foot); // stays last
    foot.children[0].textContent = o.open && o.open() ? 'Collapse tool calls by default' : 'Always expand tool calls';
  }
  // every group (and every call) in a log open or closed: "always expand" switched
  function openAll(log, on) {
    if (!log || !log.children) return;
    for (const g of [...log.children]) {
      if (!(g.classList && g.classList.contains('tGroup'))) continue;
      setGroup(g, on); for (const c of callsOf(g)) setCall(c, on);
      const foot = [...g.children[1].children].find((x) => x.classList && x.classList.contains('tgFoot'));
      if (foot) foot.children[0].textContent = on ? 'Collapse tool calls by default' : 'Always expand tool calls';
    }
  }

  W.TOOLVIEW = { parse, kindOf, short, summary, groups, isCommand, LEVELS, risk, add, openAll };
})();
