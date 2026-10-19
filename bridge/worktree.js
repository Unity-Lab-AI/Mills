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
//   node bridge/worktree.js commit <username> [msg]      stage and commit everything uncommitted in that worktree
//   node bridge/worktree.js sync <username>              merge the latest integration branch into work/<username>
//   node bridge/worktree.js merge <username>             merge work/<username> into the integration branch and push it
//                                                        (refuses with uncommitted changes; on a conflict nothing is
//                                                        left half-merged: run sync, resolve, commit, merge again)
//   node bridge/worktree.js live [--restart]             VibeRoller only. Go live: develop → main in the host folder, push main +
//                                                        develop to every remote, report the Pages deploy; --restart asks the hub
//                                                        to restart itself gracefully on the new code
// Every command takes --project <name> (default: VibeRoller) to act on another project.
// The hub's live code is the host folder; going live stays a separate, explicit step (only when asked).
'use strict';
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { MILLS_DIR, loadProfile } = require('./common');

const CFG = loadProfile('').config;
// The main checkout, even when this script runs from inside one of the worktrees.
const REPO = (() => {
  try {
    const common = execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: MILLS_DIR, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    return path.dirname(path.resolve(common));
  } catch { return path.resolve(MILLS_DIR); }
})();
const WT_ROOT = path.resolve(CFG.worktreeRoot || path.join(REPO, '..', 'Mills-worktrees'));
const PROJECTS_ROOT = path.resolve(CFG.projectsRoot || path.join(REPO, '..', 'VibeRoller-projects'));
const HUB_PROJECT = 'VibeRoller';
const REPO_DIR = '_repo'; // a project's main checkout; usernames cannot start with "_", so it never collides with a worktree
const DEVELOP = 'develop', MAIN = 'main';
const USERNAME_RE = /^[a-z0-9-]{1,32}$/;
const PROJECT_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] }).trim();
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

// Where to merge into the integration branch: the folder that has it checked out (a project's _repo; for VibeRoller
// the host folder, if it is on develop), or else a temporary detached checkout that is removed again afterwards, so
// develop is never left locked away from the host folder.
function developDir(p) {
  const at = worktrees(p.repo).find((w) => w.branch === p.main);
  if (at) return { dir: at.dir, temp: false };
  const dir = path.join(p.root, '_merge-' + process.pid);
  fs.mkdirSync(p.root, { recursive: true });
  tryGit(p.repo, 'worktree', 'prune');
  git(p.repo, 'worktree', 'add', '--detach', dir, p.main);
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

function sync(username, projectName) {
  const { dir, branch, main } = ensure(username, projectName);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  const r = tryGit(dir, 'merge', '--no-edit', main);
  if (!r.ok) return `Merging ${main} into ${branch} hit conflicts in ${dir}. Resolve them there and commit (or "git merge --abort").\n${r.out}`;
  return `${branch} is up to date with ${main}.`;
}

function commit(username, message, projectName) {
  const { dir, branch } = ensure(username, projectName);
  if (!dirty(dir)) return `Nothing to commit: ${branch} is clean.`;
  git(dir, 'add', '-A');
  git(dir, ...identity(dir, username), 'commit', '-q', '-m', message || `Work from ${username}`);
  return `Committed on ${branch} (${git(dir, 'rev-parse', '--short', 'HEAD')}).`;
}

function merge(username, projectName) {
  const w = ensure(username, projectName), { dir, branch, repo, main } = w, p = project(projectName);
  if (dirty(dir)) throw new Error(`${branch} has uncommitted changes in ${dir}. Commit them first.`);
  if (git(repo, 'rev-list', '--count', `${main}..${branch}`) === '0') return `Nothing to merge: ${branch} has no commits that ${main} lacks.`;
  const { dir: dev, temp } = developDir(p);
  let head;
  try {
    if (dirty(dev)) throw new Error(`${main} has uncommitted changes in ${dev}. Somebody is mid-work there; sort that out first.`);
    const before = git(dev, 'rev-parse', 'HEAD');
    const r = tryGit(dev, ...identity(dev, username), 'merge', '--no-ff', '-m', `Merge ${branch} into ${main}`, branch);
    if (!r.ok) {
      tryGit(dev, 'merge', '--abort');
      const pj = p.hub ? '' : `--project ${p.name} `;
      return `CONFLICT: ${branch} does not merge cleanly into ${main} (nothing was changed). Run "node bridge/worktree.js ${pj}sync ${username}", resolve the conflicts in ${dir}, commit, then merge again.\n${r.out}`;
    }
    head = git(dev, 'rev-parse', 'HEAD');
    if (temp) git(repo, 'update-ref', 'refs/heads/' + main, head, before); // fails if the branch moved meanwhile
  } finally {
    if (temp) { tryGit(repo, 'worktree', 'remove', '--force', dev); tryGit(repo, 'worktree', 'prune'); }
  }
  const lines = [`Merged ${branch} into ${main} (${head.slice(0, 7)})${p.hub ? '' : ' of ' + p.name}.`, ...pushAll(repo, main)];
  const s = tryGit(dir, 'merge', '--ff-only', main); // keep the worker branch level with the integration branch
  if (s.ok) lines.push(`${branch} fast-forwarded to ${main}.`);
  return lines.join('\n');
}

function status(projectName) {
  const list = projectName ? [project(projectName)] : [hubProject(), ...projectDirs().map((d) => project(d))];
  return list.flatMap((p) => [`${p.name} (${p.main}; main checkout ${p.repo})`, ...worktrees(p.repo).map((w) => `  ${(w.branch || '(detached)').padEnd(22)} ${dirty(w.dir) ? 'uncommitted changes' : 'clean'.padEnd(19)}  ${w.dir}`)]).join('\n');
}
const projectsText = () => listProjects().map((p) => `${p.name.padEnd(24)} ${p.branch.padEnd(10)} ${p.people.length ? 'worktrees: ' + p.people.join(', ') : 'no worktrees yet'}${p.remote ? '  ← ' + p.remote : ''}\n${''.padEnd(24)} ${p.dir}`).join('\n');

// Going live (VibeRoller only): develop → main in the folder that has main checked out (the host folder, i.e. the
// running hub's own code), push main AND develop to every remote, report the GitHub Pages deploy run, and
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

module.exports = { ensure, repair, project, listProjects, create, clone, WT_ROOT, REPO, PROJECTS_ROOT, HUB_PROJECT };

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
    else if (cmd === 'commit') console.log(commit(user, rest.join(' '), pj));
    else if (cmd === 'sync') console.log(sync(user, pj));
    else if (cmd === 'merge') console.log(merge(user, pj));
    else if (cmd === 'live') { if (pj && pj.toLowerCase() !== HUB_PROJECT.toLowerCase()) throw new Error('live is for VibeRoller itself; other projects are pushed by merge.'); console.log(live({ restart: process.argv.includes('--restart') }).join('\n')); }
    else { console.error('Usage: node bridge/worktree.js [--project <name>] projects | new <name> | clone <url> [name] | status | repair [--repo <host folder>] | ensure <username> | commit <username> [message] | sync <username> | merge <username> | live [--restart]'); process.exit(1); }
  } catch (e) { console.error(e.message); process.exit(1); }
}
