#!/usr/bin/env node
// Per-account git worktrees for working ON the Mills project, so several people can have Claude
// change Mills at the same time without stepping on each other.
//
//   <Desktop>/Mills-worktrees/<username>   branch work/<username>, created from develop on first use
//   <Desktop>/Mills-worktrees/_develop     develop, used only to merge into (unless develop is
//                                          already checked out somewhere else, then that folder)
//   Mills/ (the host folder)               the "shared folder": the running hub's own checkout
//
//   node bridge/worktree.js status                  every worktree, its branch and whether it has uncommitted changes
//   node bridge/worktree.js ensure <username>       create the worktree if it does not exist yet, print its path
//   node bridge/worktree.js sync <username>         merge the latest develop into work/<username>
//   node bridge/worktree.js merge <username>        merge work/<username> into develop and push develop
//                                                   (refuses with uncommitted changes; on a conflict nothing is
//                                                   left half-merged: run sync, resolve, commit, merge again)
// The hub's live code is the host folder; going live (develop → main there + restart) stays a
// separate, explicit step.
'use strict';
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { MILLS_DIR, loadProfile } = require('./common');

const REPO = path.resolve(MILLS_DIR);
const WT_ROOT = path.resolve(loadProfile('').config.worktreeRoot || path.join(REPO, '..', 'Mills-worktrees'));
const DEVELOP = 'develop';
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

function developDir() {
  const at = worktrees().find((w) => w.branch === DEVELOP);
  if (at) return at.dir;
  const dir = path.join(WT_ROOT, '_develop');
  fs.mkdirSync(WT_ROOT, { recursive: true });
  tryGit(REPO, 'worktree', 'prune');
  git(REPO, 'worktree', 'add', dir, DEVELOP);
  return dir;
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

function merge(username) {
  const { dir, branch } = ensure(username);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  const dev = developDir();
  if (dirty(dev)) throw new Error(`develop has uncommitted changes in ${dev}. Somebody is mid-work there; sort that out first.`);
  if (git(REPO, 'rev-list', '--count', `${DEVELOP}..${branch}`) === '0') return `Nothing to merge: ${branch} has no commits that develop lacks.`;
  const r = tryGit(dev, 'merge', '--no-ff', '-m', `Merge ${branch} into develop`, branch);
  if (!r.ok) {
    tryGit(dev, 'merge', '--abort');
    return `CONFLICT: ${branch} does not merge cleanly into develop (nothing was changed). Run "node bridge/worktree.js sync ${username}", resolve the conflicts in ${dir}, commit, then merge again.\n${r.out}`;
  }
  const lines = [`Merged ${branch} into develop (${git(dev, 'rev-parse', '--short', 'HEAD')}).`, ...pushAll(dev, DEVELOP)];
  const s = tryGit(dir, 'merge', '--ff-only', DEVELOP); // keep the worker branch level with develop
  if (s.ok) lines.push(`${branch} fast-forwarded to develop.`);
  return lines.join('\n');
}

function status() {
  return worktrees().map((w) => `${(w.branch || '(detached)').padEnd(22)} ${dirty(w.dir) ? 'uncommitted changes' : 'clean'.padEnd(19)}  ${w.dir}`).join('\n');
}

module.exports = { ensure, WT_ROOT, REPO };

if (require.main === module) {
  const [cmd, user] = process.argv.slice(2);
  try {
    if (cmd === 'status') console.log(status());
    else if (cmd === 'ensure') { const w = ensure(user); console.log(`${w.created ? 'Created' : 'Exists'}: ${w.dir} (${w.branch})`); }
    else if (cmd === 'sync') console.log(sync(user));
    else if (cmd === 'merge') console.log(merge(user));
    else { console.error('Usage: node bridge/worktree.js status | ensure <username> | sync <username> | merge <username>'); process.exit(1); }
  } catch (e) { console.error(e.message); process.exit(1); }
}
