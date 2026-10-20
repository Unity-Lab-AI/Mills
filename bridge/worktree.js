#!/usr/bin/env node
// Projects and per-person git worktrees, so several people can have Claude change the same project at the same
// time without stepping on each other.
//
// Layout: a "projects root" (config projectsRoot; on the lab server /srv/apps/sharedwork, the folder the whole team
// shares) holds one folder per project:
//   <projects root>/<project>/_repo          the project's main checkout (its integration branch checked out:
//                                            develop when it has one, else its default branch)
//   <projects root>/<project>/<username>     that person's worktree, branch work/<username>, made from the
//                                            integration branch on first use
// Anyone can start a new project (an empty repo with a first commit, main + develop) or pull in an existing repo by
// its https:// or ssh address. VibeRoller itself is one of the projects, with a different main checkout: the running
// hub's own folder (the host folder); its worktrees live in config worktreeRoot ("Mills-worktrees" next to the host
// folder on the first host, <projects root>/VibeRoller on the server).
//
//   node bridge/worktree.js projects                     every project: main checkout, integration branch, remote, who has a worktree
//   node bridge/worktree.js new <name>                   start a new project in the projects root
//   node bridge/worktree.js clone <url> [name]           pull in an existing repo as a project
//   node bridge/worktree.js status                       every worktree of every project, its branch and whether it has uncommitted changes
//   node bridge/worktree.js ensure <username>            create the worktree if it does not exist yet, print its path
//   node bridge/worktree.js repair [--repo <dir>]        after the host folder was renamed or moved: fix every VibeRoller worktree's
//                                                        pointer back to it (ensure does this by itself for one worktree)
//   node bridge/worktree.js check <username>             is that worktree stale? (fetches Forgejo first): behind / own commits /
//                                                        uncommitted changes / on the wrong branch, and what "update" would do
//   node bridge/worktree.js update <username>            bring it level with Forgejo: fast-forward when it can, merge Forgejo's
//                                                        integration branch into your own commits, else renew
//   node bridge/worktree.js renew <username>             a NEW worktree on a fresh work/<username> from Forgejo's integration
//                                                        branch; your own commits and uncommitted changes (untracked files too)
//                                                        are carried over; only when everything came over is the old worktree
//                                                        deleted (a backup ref keeps its commits either way)
//   node bridge/worktree.js commit <username> [msg]      stage and commit everything uncommitted in that worktree
//   node bridge/worktree.js sync <username>              merge the latest integration branch into work/<username>
//   node bridge/worktree.js merge <username>             merge work/<username> into the integration branch and push it
//                                                        (refuses with uncommitted changes; on a conflict nothing is
//                                                        left half-merged: run sync, resolve, commit, merge again)
//   node bridge/worktree.js live [--restart]             VibeRoller only. Go live: Forgejo's develop → main, pushed to Forgejo
//                                                        FIRST (nothing goes live that Forgejo does not have); the server's
//                                                        auto-deploy then ships it (health-checked, rolls back by itself);
//                                                        without an auto-deploy the host folder is fast-forwarded here and
//                                                        --restart asks the hub to restart itself gracefully on the new code
// Every command takes --project <name> (default: VibeRoller) to act on another project.
// The hub's live code is the host folder; going live stays a separate, explicit step (only when asked).
//
// Forgejo (the remote "origin") is the source of truth. Work happens on several machines (the server's worktrees, Gee's
// PC), so local branches go stale: every command that reads or writes the integration branch fetches first, levels the
// local integration branch with Forgejo's (fast-forward; local commits Forgejo lacks are pushed), and merge / live push
// to Forgejo before they change anything locally: a failed push changes nothing. Pushing uses the person's own Forgejo
// token (Settings → My Forgejo), which their Work runs carry in the environment.
'use strict';
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { MILLS_DIR, loadProfile } = require('./common');

const OWN_CFG = loadProfile('').config;
// The main checkout, even when this script runs from inside one of the worktrees. An AI host running the standalone
// "VibeRoller AI Host" package names its VibeRoller clone with VIBEROLLER_HUB_REPO (or config hubRepo).
const REPO = (() => {
  const given = process.env.VIBEROLLER_HUB_REPO || OWN_CFG.hubRepo;
  if (given) return path.resolve(given);
  try {
    const common = execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: MILLS_DIR, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    return path.dirname(path.resolve(common));
  } catch { return path.resolve(MILLS_DIR); }
})();
// Run from inside a worktree, the settings that count are the main checkout's: its gitignored config.local.json names
// the server's worktreeRoot / projectsRoot, which a worktree does not have (without this, a Work run's own
// "worktree.js" looked for worktrees in the wrong folder).
const CFG = (() => {
  if (path.resolve(REPO).toLowerCase() === path.resolve(MILLS_DIR).toLowerCase()) return OWN_CFG;
  const read = (f) => { try { return JSON.parse(fs.readFileSync(path.join(REPO, 'bridge', f), 'utf8')); } catch { return {}; } };
  return { ...OWN_CFG, ...read('config.json'), ...read('config.local.json') };
})();
const WT_ROOT = path.resolve(CFG.worktreeRoot || path.join(REPO, '..', 'Mills-worktrees'));
const PROJECTS_ROOT = path.resolve(CFG.projectsRoot || path.join(REPO, '..', 'VibeRoller-projects'));
const HUB_PROJECT = 'VibeRoller';
const REPO_DIR = '_repo'; // a project's main checkout; usernames cannot start with "_", so it never collides with a worktree
const DEVELOP = 'develop', MAIN = 'main';
const USERNAME_RE = /^[a-z0-9-]{1,32}$/;
const PROJECT_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

// never wait on a password prompt (no terminal here); network calls give up after two minutes
const GIT_ENV = { ...process.env, GIT_TERMINAL_PROMPT: '0' };
const gitRaw = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: GIT_ENV, timeout: 120000, maxBuffer: 256 * 1024 * 1024 });
const git = (cwd, ...args) => gitRaw(cwd, ...args).trim();
const tryGit = (cwd, ...args) => { try { return { ok: true, out: git(cwd, ...args) }; } catch (e) { return { ok: false, out: String(e.stderr || e.message).trim() }; } };
const same = (a, b) => path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

function worktrees(repo = REPO) {
  const list = []; let cur = null;
  for (const line of git(repo, 'worktree', 'list', '--porcelain').split(/\r?\n/)) {
    if (line.startsWith('worktree ')) list.push(cur = { dir: path.resolve(line.slice(9)), branch: '' });
    else if (cur && line.startsWith('branch ')) cur.branch = line.slice(7).replace('refs/heads/', '');
  }
  return list;
}
const dirty = (dir) => git(dir, 'status', '--porcelain').length > 0;
const branchExists = (repo, b) => tryGit(repo, 'rev-parse', '--verify', '--quiet', 'refs/heads/' + b).ok;
function checkUser(u) { if (!USERNAME_RE.test(u || '')) throw new Error('Usage: a username (lowercase letters, digits, dashes).'); return u; }
function checkProjectName(n) {
  if (!PROJECT_RE.test(n || '') || n.includes('..') || n.toLowerCase() === REPO_DIR) throw new Error('A project name is 1-64 characters: letters, digits, dots, dashes and underscores, starting with a letter or digit.');
  return n;
}
// git needs an author for commits this tool makes (a server account may have no global identity); people's own
// commits in Work runs use whatever identity the machine has
const identity = (repo, who) => (tryGit(repo, 'config', 'user.name').out ? [] : ['-c', `user.name=${who}`, '-c', `user.email=${who}@viberoller.local`]);
const remoteUrl = (repo) => tryGit(repo, 'remote', 'get-url', 'origin').ok ? tryGit(repo, 'remote', 'get-url', 'origin').out : '';
const integrationBranch = (repo) => (branchExists(repo, DEVELOP) ? DEVELOP : (tryGit(repo, 'symbolic-ref', '--short', 'HEAD').out || MAIN));
const peopleOf = (root) => (fs.existsSync(root) ? fs.readdirSync(root).filter((d) => USERNAME_RE.test(d) && fs.existsSync(path.join(root, d, '.git'))).sort() : []);

// ---------- projects ----------
const projectDirs = () => (fs.existsSync(PROJECTS_ROOT) ? fs.readdirSync(PROJECTS_ROOT).filter((d) => PROJECT_RE.test(d) && d.toLowerCase() !== HUB_PROJECT.toLowerCase() && fs.existsSync(path.join(PROJECTS_ROOT, d, REPO_DIR, '.git'))).sort((a, b) => a.localeCompare(b)) : []);
const hubProject = () => ({ name: HUB_PROJECT, hub: true, repo: REPO, root: WT_ROOT, main: DEVELOP });
// One project by name (case-insensitive); throws when there is none.
function project(name = HUB_PROJECT) {
  checkProjectName(name);
  if (name.toLowerCase() === HUB_PROJECT.toLowerCase()) return hubProject();
  const found = projectDirs().find((d) => d.toLowerCase() === name.toLowerCase());
  if (!found) throw new Error(`No project named "${name}" in ${PROJECTS_ROOT}. "node bridge/worktree.js projects" lists them; "new <name>" starts one, "clone <url>" pulls one in.`);
  const repo = path.join(PROJECTS_ROOT, found, REPO_DIR);
  return { name: found, hub: false, repo, root: path.join(PROJECTS_ROOT, found), main: integrationBranch(repo) };
}
const exists = (name) => name.toLowerCase() === HUB_PROJECT.toLowerCase() || projectDirs().some((d) => d.toLowerCase() === name.toLowerCase()) || fs.existsSync(path.join(PROJECTS_ROOT, name));
function listProjects() {
  return [hubProject(), ...projectDirs().map((d) => { const repo = path.join(PROJECTS_ROOT, d, REPO_DIR); return { name: d, hub: false, repo, root: path.join(PROJECTS_ROOT, d), main: integrationBranch(repo) }; })]
    .map((p) => ({ name: p.name, hub: p.hub, dir: p.repo, root: p.root, branch: p.main, remote: remoteUrl(p.repo), people: peopleOf(p.root) }));
}
function shareable(repo) { tryGit(repo, 'config', 'core.sharedRepository', 'group'); } // teammates in the same group may push into it
// A new, empty project: a repo with a README as its first commit on main, and develop as the integration branch.
function create(name, who = 'VibeRoller') {
  checkProjectName(name);
  if (exists(name)) throw new Error(`A project named "${name}" exists already.`);
  const root = path.join(PROJECTS_ROOT, name), repo = path.join(root, REPO_DIR);
  fs.mkdirSync(repo, { recursive: true });
  try {
    git(repo, 'init', '-q', '-b', MAIN);
    shareable(repo);
    fs.writeFileSync(path.join(repo, 'README.md'), `# ${name}\n\nStarted in VibeRoller by ${who} on ${new Date().toISOString().slice(0, 10)}.\n`);
    git(repo, 'add', '-A');
    git(repo, ...identity(repo, who), 'commit', '-q', '-m', `New project ${name}`);
    git(repo, 'branch', DEVELOP);
    git(repo, 'checkout', '-q', DEVELOP);
  } catch (e) { fs.rmSync(root, { recursive: true, force: true }); throw e; }
  return project(name);
}
// Pull in an existing repo by address: https:// or ssh (ssh://… or git@host:path). Local paths and other helpers are
// refused (file:// only when config projectsLocalClone is on: the regression runner). env carries the person's own
// Forgejo token (bridge/forgejo.js workEnv) so their private repos clone as them.
const URL_RE = /^(https:\/\/[A-Za-z0-9._~:/?#@!$&'()*+,;=%-]+|ssh:\/\/[A-Za-z0-9._~:/?#@!$&'()*+,;=%-]+|[A-Za-z0-9._-]+@[A-Za-z0-9.-]+:[A-Za-z0-9._~/@!$&'()*+,;=%-]+)$/;
function clone(url, name, who = 'VibeRoller', env = {}) {
  url = String(url || '').trim();
  const local = /^file:\/\//i.test(url) && CFG.projectsLocalClone === true;
  if (!local && !URL_RE.test(url)) throw new Error('Give the repo\'s https:// or ssh address (https://host/owner/repo.git or git@host:owner/repo.git).');
  name = String(name || '').trim() || url.replace(/\/+$/, '').split(/[/:]/).pop().replace(/\.git$/i, '');
  checkProjectName(name);
  if (exists(name)) throw new Error(`A project named "${name}" exists already.`);
  const root = path.join(PROJECTS_ROOT, name), repo = path.join(root, REPO_DIR);
  fs.mkdirSync(root, { recursive: true });
  try {
    execFileSync('git', ['clone', '-q', '-c', 'protocol.ext.allow=never', '-c', `protocol.file.allow=${local ? 'always' : 'never'}`, '--', url, repo],
      { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], timeout: 180000, env: { ...process.env, GIT_TERMINAL_PROMPT: '0', ...env } });
    shareable(repo);
    if (!branchExists(repo, DEVELOP) && tryGit(repo, 'rev-parse', '--verify', '--quiet', 'refs/remotes/origin/' + DEVELOP).ok) git(repo, 'branch', '--track', DEVELOP, 'origin/' + DEVELOP);
    if (branchExists(repo, DEVELOP)) git(repo, 'checkout', '-q', DEVELOP);
  } catch (e) { fs.rmSync(root, { recursive: true, force: true }); throw new Error('Could not clone it: ' + String(e.stderr || e.message).trim().split('\n').pop()); }
  return project(name);
}

// A worktree's .git file names the main checkout's folder. Renaming or moving the host folder leaves every worktree
// pointing at the old place, and git refuses to work inside them until `git worktree repair <dir>` (run from the main
// checkout) rewrites the pointer. ensure() heals the one it hands out; repair() heals them all.
const healthy = (dir) => tryGit(dir, 'rev-parse', '--git-dir').ok;
function repair(repo = REPO) {
  const root = path.resolve(CFG.worktreeRoot || path.join(repo, '..', 'Mills-worktrees'));
  const dirs = fs.existsSync(root) ? fs.readdirSync(root).map((d) => path.join(root, d)).filter((d) => !path.basename(d).startsWith('_') && fs.existsSync(path.join(d, '.git'))) : [];
  const lines = [];
  for (const d of dirs) {
    if (healthy(d)) continue;
    const r = tryGit(repo, 'worktree', 'repair', d);
    lines.push(`${path.basename(d)}: ${r.ok && healthy(d) ? 'repaired (pointed back at ' + repo + ')' : 'could not repair: ' + r.out.split('\n').pop()}`);
  }
  tryGit(repo, 'worktree', 'prune');
  return lines.length ? lines : [`Every worktree under ${root} is fine.`];
}

function ensure(username, projectName) {
  checkUser(username);
  const p = project(projectName);
  const dir = path.join(p.root, username), branch = 'work/' + username;
  const existing = worktrees(p.repo).find((w) => same(w.dir, dir));
  if (existing) {
    if (!healthy(dir)) git(p.repo, 'worktree', 'repair', dir); // the main checkout moved since this worktree was made
    return { dir, branch: existing.branch || branch, created: false, project: p.name, repo: p.repo, root: p.root, main: p.main, hub: p.hub };
  }
  if (fs.existsSync(dir)) throw new Error(`${dir} exists but is not a git worktree of ${p.name}. Move it away first.`);
  fs.mkdirSync(p.root, { recursive: true });
  tryGit(p.repo, 'worktree', 'prune');
  if (branchExists(p.repo, branch)) git(p.repo, 'worktree', 'add', dir, branch);
  else git(p.repo, 'worktree', 'add', '-b', branch, dir, p.main);
  return { dir, branch, created: true, project: p.name, repo: p.repo, root: p.root, main: p.main, hub: p.hub };
}

function pushAll(dir, branch, skip = []) {
  const results = [];
  for (const remote of git(dir, 'remote').split(/\s+/).filter(Boolean)) {
    if (skip.includes(remote)) continue;
    const r = tryGit(dir, 'push', remote, branch);
    results.push(`${remote}: ${r.ok ? 'pushed' : 'push failed (' + lastLine(r.out) + ')'}`);
  }
  return results;
}

// ---------- Forgejo as the source of truth ----------
const lastLine = (s) => String(s || '').trim().split('\n').pop();
const short = (sha) => String(sha || '').slice(0, 7);
const hasOrigin = (repo) => tryGit(repo, 'remote', 'get-url', 'origin').ok;
const refExists = (repo, ref) => tryGit(repo, 'rev-parse', '--verify', '--quiet', ref).ok;
const count = (dir, range) => Number(tryGit(dir, 'rev-list', '--count', range).out) || 0;
const isAncestor = (repo, a, b) => tryGit(repo, 'merge-base', '--is-ancestor', a, b).ok;
const NO_TOKEN = 'connect your Forgejo account in Settings → My Forgejo, so your Work runs push as you';
function fetchOrigin(repo) {
  if (!hasOrigin(repo)) return { ok: true, skipped: true };
  const r = tryGit(repo, 'fetch', '-q', '--prune', 'origin');
  return { ok: r.ok, error: r.ok ? '' : lastLine(r.out) };
}
// what to build on: Forgejo's copy of a branch when there is one, else the local branch (a project without a remote)
const upstreamOf = (repo, branch) => (refExists(repo, 'refs/remotes/origin/' + branch) ? 'origin/' + branch : branch);
// Make the local integration branch match Forgejo's: fast-forward when it is behind (in the checkout that has it, if
// any, and only when that checkout is clean); push when it holds commits Forgejo lacks (they were meant to be there);
// leave it alone and say so when the two have diverged. Returns lines for the person.
function level(p) {
  const up = upstreamOf(p.repo, p.main);
  if (up === p.main || !branchExists(p.repo, p.main)) return [];
  const local = git(p.repo, 'rev-parse', p.main), remote = git(p.repo, 'rev-parse', up);
  if (local === remote) return [];
  const at = worktrees(p.repo).find((w) => w.branch === p.main);
  if (isAncestor(p.repo, local, remote)) {
    if (at) {
      if (dirty(at.dir)) return [`${p.main} in ${at.dir} has uncommitted changes, so it was not brought level with Forgejo (${short(remote)}).`];
      const r = tryGit(at.dir, 'merge', '--ff-only', '-q', up);
      return [r.ok ? `${p.main} brought level with Forgejo (${short(local)} → ${short(remote)}).` : `Could not fast-forward ${p.main} in ${at.dir}: ${lastLine(r.out)}`];
    }
    git(p.repo, 'update-ref', 'refs/heads/' + p.main, remote, local);
    return [`${p.main} brought level with Forgejo (${short(local)} → ${short(remote)}).`];
  }
  const ahead = count(p.repo, `${up}..${p.main}`);
  if (isAncestor(p.repo, remote, local)) {
    const r = tryGit(p.repo, 'push', 'origin', `${p.main}:${p.main}`);
    return [r.ok ? `${p.main} had ${ahead} commit(s) Forgejo did not have; pushed them.` : `${p.main} has ${ahead} commit(s) Forgejo does not have and pushing them failed (${lastLine(r.out)}); ${NO_TOKEN}.`];
  }
  return [`${p.main} here and on Forgejo have diverged (${ahead} here only, ${count(p.repo, `${p.main}..${up}`)} on Forgejo only); left as it is. Forgejo's is the one people build on.`];
}

// ---------- freshness: is a worktree stale, and bring it level ----------
// own commits: the ones on HEAD whose changes Forgejo's integration branch does not have yet (merges left out)
const ownCommits = (dir, up) => tryGit(dir, 'rev-list', '--reverse', '--no-merges', '--cherry-pick', '--right-only', `${up}...HEAD`).out.split('\n').filter(Boolean);
function freshness(username, projectName, { fetch = true } = {}) {
  const p = project(projectName), w = ensure(username, projectName);
  const fetched = fetch ? fetchOrigin(p.repo) : { ok: true };
  const up = upstreamOf(p.repo, p.main);
  const behind = count(w.dir, `HEAD..${up}`), own = ownCommits(w.dir, up).length;
  const changes = git(w.dir, 'status', '--porcelain').split('\n').filter(Boolean);
  const actual = tryGit(w.dir, 'symbolic-ref', '--short', '-q', 'HEAD').out; // '' when detached (ensure() names the expected branch instead)
  const rightBranch = actual === 'work/' + username;
  return { ...w, branch: actual, upstream: up, fetched: fetched.ok, fetchError: fetched.error || '', behind, own, changes, rightBranch, stale: behind > 0 || !rightBranch };
}
function describe(f) {
  const bits = [`${f.project}: ${f.dir} on ${f.branch || '(detached)'}`];
  if (!f.fetched) bits.push(`could not reach Forgejo (${f.fetchError}); judged against what is here`);
  bits.push(f.behind ? `${f.behind} commit(s) behind ${f.upstream}` : `level with ${f.upstream}`);
  if (f.own) bits.push(`${f.own} commit(s) of your own not merged yet`);
  if (f.changes.length) bits.push(`${f.changes.length} uncommitted change(s)`);
  if (!f.rightBranch) bits.push(`on the wrong branch (should be work/${f.dir.split(/[\\/]/).pop()})`);
  bits.push(!f.stale ? 'FRESH' : !f.rightBranch || f.changes.length ? 'STALE: "update" renews it (your changes come along)' : 'STALE: "update" brings it level');
  return bits.join(' · ');
}
function update(username, projectName) {
  const p = project(projectName), lines = [];
  const fetched = fetchOrigin(p.repo);
  if (!fetched.ok) lines.push(`Could not reach Forgejo (${fetched.error}); using what is here.`);
  lines.push(...level(p));
  const f = freshness(username, projectName, { fetch: false });
  if (!f.stale) return [...lines, `${f.branch} is level with ${f.upstream}${f.own ? ` (and has ${f.own} commit(s) of yours not merged yet)` : ''}. Nothing to do.`];
  if (f.rightBranch && !f.changes.length) {
    if (!f.own) {
      const r = tryGit(f.dir, 'merge', '--ff-only', '-q', f.upstream);
      if (r.ok) return [...lines, `${f.branch} fast-forwarded to ${f.upstream} (${short(git(f.dir, 'rev-parse', 'HEAD'))}).`];
    } else {
      const r = tryGit(f.dir, ...identity(f.dir, username), 'merge', '--no-edit', '-q', f.upstream);
      if (r.ok) return [...lines, `${f.upstream} merged into ${f.branch}; your ${f.own} commit(s) stay on top.`];
      tryGit(f.dir, 'merge', '--abort');
      return [...lines, `${f.upstream} does not merge cleanly into your ${f.own} commit(s) (nothing was changed). Run "node bridge/worktree.js ${p.hub ? '' : `--project ${p.name} `}sync ${username}" and resolve the conflicts in ${f.dir}, then commit.`, lastLine(r.out)];
    }
  }
  return [...lines, ...renew(username, projectName, { fetched: true })];
}
// A new worktree from Forgejo's integration branch, with everything of the old one carried over: own commits
// (cherry-picked in order; ones Forgejo already has are skipped), uncommitted changes (applied with a 3-way merge) and
// untracked files. The old one is moved aside to <root>/_old-<username>-<time> first and deleted only when everything
// came over; its commits and uncommitted state are kept under refs/viberoller/renewed/ either way (the last 10 per
// person), so nothing is ever lost.
function renew(username, projectName, { fetched = false } = {}) {
  checkUser(username);
  const p = project(projectName), lines = [];
  if (!fetched) { const r = fetchOrigin(p.repo); if (!r.ok) lines.push(`Could not reach Forgejo (${r.error}); renewing from what is here.`); lines.push(...level(p)); }
  const w = ensure(username, projectName), up = upstreamOf(p.repo, p.main), branch = 'work/' + username;
  if (tryGit(w.dir, 'rev-parse', '-q', '--verify', 'MERGE_HEAD').ok || tryGit(w.dir, 'rev-parse', '-q', '--verify', 'CHERRY_PICK_HEAD').ok) throw new Error(`${w.dir} is in the middle of a merge or cherry-pick. Finish it (commit) or abort it first.`);
  const elsewhere = worktrees(p.repo).find((x) => x.branch === branch && !same(x.dir, w.dir));
  if (elsewhere) throw new Error(`${branch} is checked out in another worktree too (${elsewhere.dir}, an older layout). Nothing was changed. Bring anything you need from there, then remove it (git worktree remove ${elsewhere.dir}) and run renew again.`);
  const stamp = new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d+Z$/, '').replace('T', '-');
  const oldHead = git(w.dir, 'rev-parse', 'HEAD'), oldBranch = tryGit(w.dir, 'symbolic-ref', '--short', '-q', 'HEAD').out, own = ownCommits(w.dir, up);
  const patch = gitRaw(w.dir, 'diff', 'HEAD', '--binary');
  const untracked = gitRaw(w.dir, 'ls-files', '--others', '--exclude-standard', '-z').split('\0').filter(Boolean);
  // keep it all reachable before anything moves
  const backup = `refs/viberoller/renewed/${username}-${stamp}`;
  git(p.repo, 'update-ref', backup, oldHead);
  const wip = tryGit(w.dir, 'stash', 'create').out; // a commit of the uncommitted state; the stash list is not touched
  if (wip) git(p.repo, 'update-ref', backup + '-wip', wip);
  const keep = git(p.repo, 'for-each-ref', '--format=%(refname)', `refs/viberoller/renewed/`).split('\n').filter((r) => r && r.startsWith(`refs/viberoller/renewed/${username}-`) && !r.endsWith('-wip')).sort();
  for (const r of keep.slice(0, -10)) { tryGit(p.repo, 'update-ref', '-d', r); tryGit(p.repo, 'update-ref', '-d', r + '-wip'); }
  // free the branch, move the old worktree aside, make the new one where the old one was
  const oldDir = path.join(p.root, `_old-${username}-${stamp}`);
  git(w.dir, 'checkout', '-q', '--detach');
  git(p.repo, 'worktree', 'move', w.dir, oldDir);
  try { git(p.repo, 'worktree', 'add', '-q', '-B', branch, w.dir, up); }
  catch (e) { // put the old one back exactly as it was
    tryGit(p.repo, 'worktree', 'move', oldDir, w.dir); tryGit(w.dir, 'checkout', '-q', oldBranch || oldHead);
    throw new Error(`Could not make the new worktree (${lastLine(e.stderr || e.message)}); the old one is back as it was.`);
  }
  lines.push(`New worktree ${w.dir} on ${branch} from ${up} (${short(git(w.dir, 'rev-parse', 'HEAD'))}); the old one is at ${oldDir} until everything has come over.`);
  const failed = [];
  let picked = 0, skipped = 0;
  for (const c of own) {
    const r = tryGit(w.dir, ...identity(w.dir, username), 'cherry-pick', '-x', c);
    if (r.ok) { picked++; continue; }
    if (!git(w.dir, 'diff', '--name-only', '--diff-filter=U')) { tryGit(w.dir, 'cherry-pick', '--skip'); skipped++; continue; } // nothing left to add: Forgejo has it
    tryGit(w.dir, 'cherry-pick', '--abort');
    failed.push(`commit ${short(c)} "${git(p.repo, 'log', '-1', '--format=%s', c).slice(0, 80)}" (and any after it) conflicts with ${up}`);
    break;
  }
  if (own.length) lines.push(`Commits carried over: ${picked}${skipped ? `, ${skipped} already on ${up}` : ''}${failed.length ? ', stopped at a conflict' : ''}.`);
  if (!failed.length && patch.trim()) {
    const file = path.join(p.root, `_renew-${username}-${stamp}.patch`);
    fs.writeFileSync(file, patch);
    const r = tryGit(w.dir, 'apply', '--3way', '--whitespace=nowarn', file);
    if (r.ok) { tryGit(w.dir, 'reset', '-q'); lines.push('Uncommitted changes carried over.'); fs.rmSync(file, { force: true }); }
    else { tryGit(w.dir, 'reset', '-q', '--hard'); failed.push(`the uncommitted changes do not apply on ${up} (the patch is kept at ${file})`); }
  }
  if (!failed.length && untracked.length) {
    let n = 0;
    for (const f of untracked) { const to = path.join(w.dir, f); if (fs.existsSync(to)) continue; fs.mkdirSync(path.dirname(to), { recursive: true }); fs.cpSync(path.join(oldDir, f), to, { recursive: true }); n++; }
    lines.push(`Untracked files carried over: ${n}${n < untracked.length ? ` (${untracked.length - n} exist in the new one already, left as they are)` : ''}.`);
  }
  if (failed.length) {
    lines.push(`NOT everything came over, so the old worktree stays at ${oldDir}: ${failed.join('; ')}. Bring the rest over by hand (or ask Claude to), then delete it with: git worktree remove --force ${oldDir}`);
  } else {
    const r = tryGit(p.repo, 'worktree', 'remove', '--force', oldDir);
    tryGit(p.repo, 'worktree', 'prune');
    lines.push(r.ok ? `Everything came over; the old worktree is deleted (its state stays recoverable as ${backup}).` : `Everything came over, but deleting ${oldDir} failed (${lastLine(r.out)}); delete it by hand.`);
  }
  return lines;
}

function sync(username, projectName) {
  const { dir, branch, repo } = ensure(username, projectName), p = project(projectName);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  const f = fetchOrigin(repo), lines = f.ok ? level(p) : [`Could not reach Forgejo (${f.error}); merging what is here.`];
  const up = upstreamOf(repo, p.main);
  const r = tryGit(dir, ...identity(dir, username), 'merge', '--no-edit', up);
  if (!r.ok) return [...lines, `Merging ${up} into ${branch} hit conflicts in ${dir}. Resolve them there and commit (or "git merge --abort").`, r.out].join('\n');
  return [...lines, `${branch} is up to date with ${up}.`].join('\n');
}

function commit(username, message, projectName) {
  const { dir, branch } = ensure(username, projectName);
  if (!dirty(dir)) return `Nothing to commit: ${branch} is clean.`;
  git(dir, 'add', '-A');
  git(dir, ...identity(dir, username), 'commit', '-q', '-m', message || `Work from ${username}`);
  return `Committed on ${branch} (${git(dir, 'rev-parse', '--short', 'HEAD')}).`;
}

// The versioning law (LAWS.md): a project with a versions.json moves its version up with every merge into the
// integration branch. If the branch is not above what is there now (forgot to bump, or someone else merged the same
// number first), nothing is merged and the message says which numbers would be right.
const semver = (s) => { const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(String(s || '').trim()); return m ? m.slice(1).map(Number) : null; };
const semverCmp = (a, b) => { for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] - b[i]; return 0; };
function projectVersion(repo, ref) {
  const r = tryGit(repo, 'show', `${ref}:versions.json`);
  if (!r.ok) return null;
  try { return semver(JSON.parse(r.out).project); } catch { return null; }
}
function versionGate(repo, base, branch) {
  const there = projectVersion(repo, base), mine = projectVersion(repo, branch);
  if (!there) return ''; // no versions.json on the integration branch: nothing to enforce
  if (mine && semverCmp(mine, there) > 0) return '';
  const [a, b, c] = there;
  return `Not merged: ${branch} is at ${mine ? mine.join('.') : '(no version)'} but ${base} is already at ${there.join('.')}, and every merge moves the version up (LAWS.md, versioning law). `
    + `Set versions.json (and the CHANGELOG section) to ${a}.${b}.${c + 1} for fixes and small changes, ${a}.${b + 1}.0 for moderate to large features or anything that could break, or ${a + 1}.0.0 for a rework of half the codebase or more; commit, then merge again.`;
}

// A throwaway detached checkout of one commit (so nobody's checkout is touched), removed again by done().
function scratch(p, at) {
  const dir = path.join(p.root, `_merge-${process.pid}-${Date.now()}`);
  fs.mkdirSync(p.root, { recursive: true });
  tryGit(p.repo, 'worktree', 'prune');
  git(p.repo, 'worktree', 'add', '-q', '--detach', dir, at);
  return { dir, done: () => { tryGit(p.repo, 'worktree', 'remove', '--force', dir); tryGit(p.repo, 'worktree', 'prune'); } };
}
// With Forgejo: merge onto Forgejo's integration branch in a scratch checkout, push that to Forgejo, and only then
// move the local branches (a failed push changes nothing anywhere). Without a remote: merge locally as before.
function merge(username, projectName) {
  const w = ensure(username, projectName), { dir, branch, repo, main } = w, p = project(projectName);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  const pj = p.hub ? '' : `--project ${p.name} `;
  const remote = hasOrigin(repo), lines = [];
  if (remote) {
    const f = fetchOrigin(repo);
    if (!f.ok) throw new Error(`Could not reach Forgejo (${f.error}); nothing was merged. Try again in a moment.`);
    lines.push(...level(p));
  }
  const base = upstreamOf(repo, main);
  if (count(repo, `${base}..${branch}`) === 0) return [...lines, `Nothing to merge: ${branch} has no commits that ${base} lacks.`].join('\n');
  const vErr = versionGate(repo, base, branch);
  if (vErr) throw new Error(vErr);
  const s = scratch(p, base);
  let head;
  try {
    const r = tryGit(s.dir, ...identity(s.dir, username), 'merge', '--no-ff', '-m', `Merge ${branch} into ${main}`, branch);
    if (!r.ok) {
      tryGit(s.dir, 'merge', '--abort');
      return [...lines, `CONFLICT: ${branch} does not merge cleanly into ${base} (nothing was changed). Run "node bridge/worktree.js ${pj}update ${username}" (or sync), resolve the conflicts in ${dir}, commit, then merge again.`, r.out].join('\n');
    }
    head = git(s.dir, 'rev-parse', 'HEAD');
    if (remote) {
      const pushed = tryGit(s.dir, 'push', 'origin', `HEAD:refs/heads/${main}`);
      if (!pushed.ok) throw new Error(`Pushing to Forgejo failed, so nothing was merged anywhere (${lastLine(pushed.out)}). ${/could not read|auth|denied|403|401/i.test(pushed.out) ? NO_TOKEN[0].toUpperCase() + NO_TOKEN.slice(1) + ', then merge again.' : 'If Forgejo moved meanwhile, merge again.'}`);
      lines.push(`Merged ${branch} into ${main} (${short(head)})${p.hub ? '' : ' of ' + p.name} and pushed it to Forgejo.`);
      fetchOrigin(repo);
      lines.push(...level(p), ...pushAll(repo, main, ['origin']));
    } else {
      const before = git(repo, 'rev-parse', main), at = worktrees(repo).find((x) => x.branch === main);
      if (at) { if (dirty(at.dir)) throw new Error(`${main} has uncommitted changes in ${at.dir}; nothing was merged.`); git(at.dir, 'merge', '--ff-only', '-q', head); }
      else git(repo, 'update-ref', 'refs/heads/' + main, head, before); // fails if the branch moved meanwhile
      lines.push(`Merged ${branch} into ${main} (${short(head)})${p.hub ? '' : ' of ' + p.name}.`);
    }
  } finally { s.done(); }
  const ff = tryGit(dir, 'merge', '--ff-only', '-q', upstreamOf(repo, main)); // keep the worker branch level with the integration branch
  if (ff.ok) lines.push(`${branch} fast-forwarded to ${main}.`);
  return lines.join('\n');
}

function status(projectName) {
  const list = projectName ? [project(projectName)] : [hubProject(), ...projectDirs().map((d) => project(d))];
  return list.flatMap((p) => [`${p.name} (${p.main}; main checkout ${p.repo})`, ...worktrees(p.repo).map((w) => `  ${(w.branch || '(detached)').padEnd(22)} ${dirty(w.dir) ? 'uncommitted changes' : 'clean'.padEnd(19)}  ${w.dir}`)]).join('\n');
}
const projectsText = () => listProjects().map((p) => `${p.name.padEnd(24)} ${p.branch.padEnd(10)} ${p.people.length ? 'worktrees: ' + p.people.join(', ') : 'no worktrees yet'}${p.remote ? '  ← ' + p.remote : ''}\n${''.padEnd(24)} ${p.dir}`).join('\n');

// Going live (VibeRoller only): Forgejo's develop → Forgejo's main (merged in a scratch checkout and pushed first),
// then the host folder follows: the server's auto-deploy fetches it (health check, automatic rollback), or, on a hub
// without one, the host folder is fast-forwarded here and (with --restart) the hub asked to restart itself gracefully
// (same as node bridge/restart.js: it waits until nobody has a reply in progress, nobody is signed out). Refuses if
// main's folder has uncommitted changes. This is the one go-live command, so a Work session can ship when asked.
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
  if (!hasOrigin(REPO)) { // no Forgejo (a hub on its own): merge in the host folder as before
    if (count(REPO, `${MAIN}..${DEVELOP}`) === 0) return [`Nothing to ship: ${MAIN} (${short(git(REPO, 'rev-parse', MAIN))}) already has everything on ${DEVELOP}.`, ...(opts.restart ? [restart()] : [])];
    const r = tryGit(at.dir, 'merge', '--no-ff', '-m', `Merge ${DEVELOP} into ${MAIN}`, DEVELOP);
    if (!r.ok) { tryGit(at.dir, 'merge', '--abort'); throw new Error(`CONFLICT merging ${DEVELOP} into ${MAIN} (nothing was changed):\n${r.out}`); }
    return [`Merged ${DEVELOP} into ${MAIN} (${short(git(at.dir, 'rev-parse', 'HEAD'))}) in ${at.dir}.`, ...(opts.restart ? [restart()] : [])];
  }
  // Forgejo first: what goes live is exactly what Forgejo's main has (a server-only main once made the auto-deploy
  // restart the hub every minute and every AI host lost its line). A failed push changes nothing.
  const f = fetchOrigin(REPO);
  if (!f.ok) throw new Error(`Could not reach Forgejo (${f.error}); nothing went live.`);
  const hp = hubProject(), lines = [...level(hp)];
  const dev = upstreamOf(REPO, DEVELOP), mainUp = upstreamOf(REPO, MAIN);
  // the server's auto-deploy (deploy/autodeploy.sh, every minute) ships Forgejo's main by itself
  const autoDeploy = process.platform !== 'win32' && (CFG.autoDeploy === true || fs.existsSync(path.join(path.dirname(at.dir), 'autodeploy.log')));
  const bringHere = () => { // no auto-deploy: fast-forward the host folder to Forgejo's main here
    if (autoDeploy) return [`The server's auto-deploy ships ${short(git(REPO, 'rev-parse', mainUp))} within a minute: it checks the new version comes up and goes back to the last good one by itself if not.`];
    const r = tryGit(at.dir, 'merge', '--ff-only', '-q', mainUp);
    return [r.ok ? `${at.dir} is now on ${short(git(at.dir, 'rev-parse', 'HEAD'))}.` : `Could not fast-forward ${at.dir} to ${mainUp}: ${lastLine(r.out)}`, ...(r.ok && opts.restart ? [restart()] : [])];
  };
  if (count(REPO, `${mainUp}..${dev}`) === 0) return [...lines, `Nothing to ship: Forgejo's ${MAIN} (${short(git(REPO, 'rev-parse', mainUp))}) already has everything on ${DEVELOP}.`, ...bringHere()];
  const s = scratch(hp, mainUp);
  try {
    const r = tryGit(s.dir, ...identity(s.dir, 'VibeRoller'), 'merge', '--no-ff', '-m', `Merge ${DEVELOP} into ${MAIN}`, dev);
    if (!r.ok) { tryGit(s.dir, 'merge', '--abort'); throw new Error(`CONFLICT merging ${DEVELOP} into ${MAIN} (nothing was changed):\n${r.out}`); }
    const pushed = tryGit(s.dir, 'push', 'origin', `HEAD:refs/heads/${MAIN}`);
    if (!pushed.ok) throw new Error(`Pushing ${MAIN} to Forgejo failed, so nothing went live (${lastLine(pushed.out)}). ${/could not read|auth|denied|403|401/i.test(pushed.out) ? NO_TOKEN[0].toUpperCase() + NO_TOKEN.slice(1) + ', then run live again.' : ''}`);
    lines.push(`Merged ${DEVELOP} into ${MAIN} (${short(git(s.dir, 'rev-parse', 'HEAD'))}) and pushed it to Forgejo.`);
  } finally { s.done(); }
  fetchOrigin(REPO);
  for (const b of [DEVELOP, MAIN]) lines.push(...pushAll(REPO, `${upstreamOf(REPO, b)}:refs/heads/${b}`, ['origin']).map((x) => `${b} → ${x}`));
  lines.push(...bringHere(), deploy());
  return lines;
}

module.exports = { ensure, repair, project, listProjects, create, clone, freshness, describe, update, renew, WT_ROOT, REPO, PROJECTS_ROOT, HUB_PROJECT };

if (require.main === module) {
  const argv = process.argv.slice(2);
  const take = (flag) => { const i = argv.indexOf(flag); return i >= 0 ? argv.splice(i, 2)[1] || '' : ''; };
  const repoArg = take('--repo'), pj = take('--project') || undefined;
  const [cmd, user, ...rest] = argv;
  try {
    if (cmd === 'status') console.log(status(pj));
    else if (cmd === 'projects') console.log(projectsText());
    else if (cmd === 'new') { const p = create(user, process.env.USERNAME || process.env.USER || 'VibeRoller'); console.log(`Created project ${p.name}: ${p.repo} (${p.main})`); }
    else if (cmd === 'clone') { const p = clone(user, rest[0], process.env.USERNAME || process.env.USER || 'VibeRoller'); console.log(`Pulled in ${p.name}: ${p.repo} (${p.main})`); }
    else if (cmd === 'repair') {
      const repo = repoArg ? path.resolve(repoArg) : REPO;
      if (!tryGit(repo, 'rev-parse', '--git-dir').ok) throw new Error(`${repo} is not a git checkout. From a worktree whose pointer is broken, name the host folder: node bridge/worktree.js repair --repo <host folder>`);
      console.log(repair(repo).join('\n'));
    }
    else if (cmd === 'ensure') { const w = ensure(user, pj); console.log(`${w.created ? 'Created' : 'Exists'}: ${w.dir} (${w.branch} of ${w.project})`); }
    else if (cmd === 'check') { const f = freshness(checkUser(user), pj); console.log(describe(f)); process.exitCode = f.stale ? 3 : 0; }
    else if (cmd === 'auto') { // the hub's background check: fast-forward only a clean worktree with nothing of its own, else just report
      let f = freshness(checkUser(user), pj); const done = [];
      if (f.stale && f.rightBranch && !f.changes.length && !f.own && tryGit(f.dir, 'merge', '--ff-only', '-q', f.upstream).ok) { done.push(`fast-forwarded to ${f.upstream} by itself`); f = freshness(user, pj, { fetch: false }); }
      console.log([describe(f), ...done].join(' · '));
    }
    else if (cmd === 'update') console.log(update(checkUser(user), pj).join('\n'));
    else if (cmd === 'renew') console.log(renew(user, pj).join('\n'));
    else if (cmd === 'commit') console.log(commit(user, rest.join(' '), pj));
    else if (cmd === 'sync') console.log(sync(user, pj));
    else if (cmd === 'merge') console.log(merge(user, pj));
    else if (cmd === 'live') { if (pj && pj.toLowerCase() !== HUB_PROJECT.toLowerCase()) throw new Error('live is for VibeRoller itself; other projects are pushed by merge.'); console.log(live({ restart: process.argv.includes('--restart') }).join('\n')); }
    else { console.error('Usage: node bridge/worktree.js [--project <name>] projects | new <name> | clone <url> [name] | status | repair [--repo <host folder>] | ensure <username> | check <username> | update <username> | renew <username> | commit <username> [message] | sync <username> | merge <username> | live [--restart]'); process.exit(1); }
  } catch (e) { console.error(e.message); process.exit(1); }
}
