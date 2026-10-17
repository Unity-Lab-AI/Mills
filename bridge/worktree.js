#!/usr/bin/env node
// Per-account git worktrees for working ON the Mills project, so several people can have Claude
// change Mills at the same time without stepping on each other.
//
//   <Desktop>/Mills-worktrees/<username>   branch work/<username>, created from develop on first use
//   merges into develop happen where develop is checked out (e.g. the host folder), or else in a
//   temporary detached checkout (Mills-worktrees/_merge-<pid>) removed right after, so develop
//   stays free for the host folder to check out
//   Mills/ (the host folder)               the "shared folder": the running hub's own checkout
//
//   node bridge/worktree.js status                  every worktree, its branch and whether it has uncommitted changes
//   node bridge/worktree.js ensure <username>       create the worktree if it does not exist yet, print its path
//   node bridge/worktree.js commit <username> [msg] stage and commit everything uncommitted in that worktree
//   node bridge/worktree.js sync <username>        merge the latest develop into work/<username>
//   node bridge/worktree.js merge <username>        merge work/<username> into develop and push develop
//                                                   (refuses with uncommitted changes; on a conflict nothing is
//                                                   left half-merged: run sync, resolve, commit, merge again)
//   node bridge/worktree.js live [--restart]        go live: develop → main in the host folder, push main + develop
//                                                   to every remote, report the Pages deploy; --restart asks the hub
//                                                   to restart itself gracefully on the new code
// The hub's live code is the host folder; going live stays a separate, explicit step (only when asked).
'use strict';
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { MILLS_DIR, loadProfile } = require('./common');

// The main checkout, even when this script runs from inside one of the worktrees.
const REPO = (() => {
  try {
    const common = execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: MILLS_DIR, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    return path.dirname(path.resolve(common));
  } catch { return path.resolve(MILLS_DIR); }
})();
const WT_ROOT = path.resolve(loadProfile('').config.worktreeRoot || path.join(REPO, '..', 'Mills-worktrees'));
const DEVELOP = 'develop', MAIN = 'main';
const USERNAME_RE = /^[a-z0-9-]{1,32}$/;

const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }).trim();
const tryGit = (cwd, ...args) => { try { return { ok: true, out: git(cwd, ...args) }; } catch (e) { return { ok: false, out: String(e.stderr || e.message).trim() }; } };
const same = (a, b) => path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

function worktrees() {
  const list = []; let cur = null;
  for (const line of git(REPO, 'worktree', 'list', '--porcelain').split(/\r?\n/)) {
    if (line.startsWith('worktree ')) list.push(cur = { dir: path.resolve(line.slice(9)), branch: '' });
    else if (cur && line.startsWith('branch ')) cur.branch = line.slice(7).replace('refs/heads/', '');
  }
  return list;
}
const dirty = (dir) => git(dir, 'status', '--porcelain').length > 0;
const branchExists = (b) => tryGit(REPO, 'rev-parse', '--verify', '--quiet', 'refs/heads/' + b).ok;
function checkUser(u) { if (!USERNAME_RE.test(u || '')) throw new Error('Usage: a username (lowercase letters, digits, dashes).'); return u; }

function ensure(username) {
  checkUser(username);
  const dir = path.join(WT_ROOT, username), branch = 'work/' + username;
  const existing = worktrees().find((w) => same(w.dir, dir));
  if (existing) return { dir, branch: existing.branch || branch, created: false };
  if (fs.existsSync(dir)) throw new Error(`${dir} exists but is not a git worktree of Mills. Move it away first.`);
  fs.mkdirSync(WT_ROOT, { recursive: true });
  tryGit(REPO, 'worktree', 'prune');
  if (branchExists(branch)) git(REPO, 'worktree', 'add', dir, branch);
  else git(REPO, 'worktree', 'add', '-b', branch, dir, DEVELOP);
  return { dir, branch, created: true };
}

// Where to merge into develop: the folder that has develop checked out, or else a temporary detached
// checkout that is removed again afterwards, so develop is never left locked away from the host folder.
function developDir() {
  const at = worktrees().find((w) => w.branch === DEVELOP);
  if (at) return { dir: at.dir, temp: false };
  const dir = path.join(WT_ROOT, '_merge-' + process.pid);
  fs.mkdirSync(WT_ROOT, { recursive: true });
  tryGit(REPO, 'worktree', 'prune');
  git(REPO, 'worktree', 'add', '--detach', dir, DEVELOP);
  return { dir, temp: true };
}

function pushAll(dir, branch) {
  const results = [];
  for (const remote of git(dir, 'remote').split(/\s+/).filter(Boolean)) {
    const r = tryGit(dir, 'push', remote, branch);
    results.push(`${remote}: ${r.ok ? 'pushed' : 'push failed (' + r.out.split('\n').pop() + ')'}`);
  }
  return results;
}

function sync(username) {
  const { dir, branch } = ensure(username);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  const r = tryGit(dir, 'merge', '--no-edit', DEVELOP);
  if (!r.ok) return `Merging develop into ${branch} hit conflicts in ${dir}. Resolve them there and commit (or "git merge --abort").\n${r.out}`;
  return `${branch} is up to date with develop.`;
}

function commit(username, message) {
  const { dir, branch } = ensure(username);
  if (!dirty(dir)) return `Nothing to commit: ${branch} is clean.`;
  git(dir, 'add', '-A');
  git(dir, 'commit', '-q', '-m', message || `Work from ${username}`);
  return `Committed on ${branch} (${git(dir, 'rev-parse', '--short', 'HEAD')}).`;
}

function merge(username) {
  const { dir, branch } = ensure(username);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  if (git(REPO, 'rev-list', '--count', `${DEVELOP}..${branch}`) === '0') return `Nothing to merge: ${branch} has no commits that develop lacks.`;
  const { dir: dev, temp } = developDir();
  let head;
  try {
    if (dirty(dev)) throw new Error(`develop has uncommitted changes in ${dev}. Somebody is mid-work there; sort that out first.`);
    const before = git(dev, 'rev-parse', 'HEAD');
    const r = tryGit(dev, 'merge', '--no-ff', '-m', `Merge ${branch} into develop`, branch);
    if (!r.ok) {
      tryGit(dev, 'merge', '--abort');
      return `CONFLICT: ${branch} does not merge cleanly into develop (nothing was changed). Run "node bridge/worktree.js sync ${username}", resolve the conflicts in ${dir}, commit, then merge again.\n${r.out}`;
    }
    head = git(dev, 'rev-parse', 'HEAD');
    if (temp) git(REPO, 'update-ref', 'refs/heads/' + DEVELOP, head, before); // fails if develop moved meanwhile
  } finally {
    if (temp) { tryGit(REPO, 'worktree', 'remove', '--force', dev); tryGit(REPO, 'worktree', 'prune'); }
  }
  const lines = [`Merged ${branch} into develop (${head.slice(0, 7)}).`, ...pushAll(REPO, DEVELOP)];
  const s = tryGit(dir, 'merge', '--ff-only', DEVELOP); // keep the worker branch level with develop
  if (s.ok) lines.push(`${branch} fast-forwarded to develop.`);
  return lines.join('\n');
}

function status() {
  return worktrees().map((w) => `${(w.branch || '(detached)').padEnd(22)} ${dirty(w.dir) ? 'uncommitted changes' : 'clean'.padEnd(19)}  ${w.dir}`).join('\n');
}

// Going live: develop → main in the folder that has main checked out (the host folder, i.e. the running
// hub's own code), push main AND develop to every remote, report the GitHub Pages deploy run, and
// (with --restart) ask the hub to restart itself gracefully (same as node bridge/restart.js: it waits
// until nobody has a reply in progress, nobody is signed out). Refuses if main's folder has uncommitted
// changes. This is the one go-live command, so a Work session can ship when its person asks.
function live(opts = {}) {
  const at = worktrees().find((w) => w.branch === MAIN);
  if (!at) throw new Error(`No checkout has ${MAIN} checked out; the host folder should.`);
  if (dirty(at.dir)) throw new Error(`${MAIN} has uncommitted changes in ${at.dir}. Somebody is mid-work there; sort that out first.`);
  const deploy = () => { // the latest Pages deploy run for main (best effort: gh may be missing or logged out)
    try {
      const out = execFileSync('gh', ['run', 'list', '-R', 'Unity-Lab-AI/Mills', '-L', '1', '--json', 'status,conclusion,url,headSha'], { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
      const run = JSON.parse(out)[0];
      return run ? `Pages deploy of ${String(run.headSha).slice(0, 7)}: ${run.status}${run.conclusion ? ' / ' + run.conclusion : ''} ${run.url}` : 'Pages deploy: no run found';
    } catch { return 'Pages deploy: could not ask gh; check https://github.com/Unity-Lab-AI/Mills/actions'; }
  };
  // The restart flag must land in the RUNNING hub's bridge folder: the main checkout's (at.dir), never this
  // script's own copy (a worktree's bridge/.restart restarts nothing — that mistake shipped once).
  const restart = () => {
    const flag = path.join(at.dir, 'bridge', '.restart');
    fs.writeFileSync(flag, new Date().toISOString() + '\n');
    return `Hub restart requested (${flag}): it restarts itself (same address, nobody signed out) as soon as no reply is in progress.`;
  };
  if (git(REPO, 'rev-list', '--count', `${MAIN}..${DEVELOP}`) === '0') return [`Nothing to ship: ${MAIN} (${git(REPO, 'rev-parse', '--short', MAIN)}) already has everything on ${DEVELOP}.`, deploy(), ...(opts.restart ? [restart()] : [])];
  const r = tryGit(at.dir, 'merge', '--no-ff', '-m', `Merge ${DEVELOP} into ${MAIN}`, DEVELOP);
  if (!r.ok) { tryGit(at.dir, 'merge', '--abort'); throw new Error(`CONFLICT merging ${DEVELOP} into ${MAIN} (nothing was changed):\n${r.out}`); }
  const lines = [`Merged ${DEVELOP} into ${MAIN} (${git(at.dir, 'rev-parse', '--short', 'HEAD')}) in ${at.dir}.`];
  for (const b of [DEVELOP, MAIN]) lines.push(...pushAll(REPO, b).map((s) => `${b} → ${s}`));
  lines.push(deploy() + ' (the run for this push may still be starting; run "node bridge/worktree.js live" again to see it)');
  if (opts.restart) lines.push(restart());
  return lines;
}

module.exports = { ensure, WT_ROOT, REPO };

if (require.main === module) {
  const [cmd, user, ...rest] = process.argv.slice(2);
  try {
    if (cmd === 'status') console.log(status());
    else if (cmd === 'ensure') { const w = ensure(user); console.log(`${w.created ? 'Created' : 'Exists'}: ${w.dir} (${w.branch})`); }
    else if (cmd === 'commit') console.log(commit(user, rest.join(' ')));
    else if (cmd === 'sync') console.log(sync(user));
    else if (cmd === 'merge') console.log(merge(user));
    else if (cmd === 'live') console.log(live({ restart: process.argv.includes('--restart') }).join('\n'));
    else { console.error('Usage: node bridge/worktree.js status | ensure <username> | commit <username> [message] | sync <username> | merge <username> | live [--restart]'); process.exit(1); }
  } catch (e) { console.error(e.message); process.exit(1); }
}
