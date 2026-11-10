//! Projects and per-person worktrees, as the hub needs them for Work runs (bridge/worktree.js is the full tool and stays
//! the one people and Work runs call: check / update / renew / commit / merge / live; starting and pulling in projects
//! goes through it too). Here: which projects there are, and each person's worktree of one (made on first use), the
//! same layout: <projects root>/<project>/_repo + /<username>; VibeRoller's worktrees in config worktreeRoot.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const HUB_PROJECT: &str = "VibeRoller";
/// What a session with no project is called in lists and the run's state (T-238); not a valid project name.
pub const NO_PROJECT: &str = "(none)";
const REPO_DIR: &str = "_repo";

#[derive(Clone, Debug)]
pub struct Project { pub name: String, pub hub: bool, pub repo: PathBuf, pub root: PathBuf, pub main: String }
#[derive(Clone, Debug)]
pub struct Wt { pub dir: PathBuf, pub branch: String, pub project: String, pub repo: PathBuf, pub root: PathBuf, pub main: String, pub hub: bool }

pub struct Worktrees { pub repo: PathBuf, pub wt_root: PathBuf, pub projects_root: PathBuf, pub node: String, pub script: PathBuf }
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = { #[allow(unused_mut)] let mut c = std::process::Command::new("git"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}
fn valid_user(u: &str) -> bool { !u.is_empty() && u.len() <= 32 && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }
fn valid_project(n: &str) -> bool { !n.is_empty() && n.len() <= 64 && n.as_bytes()[0].is_ascii_alphanumeric() && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !n.contains("..") && !n.eq_ignore_ascii_case(REPO_DIR) }

impl Worktrees {
    pub fn new(root: &Path, cfg: &pgm_core_internal::config::Config) -> Worktrees {
        let mills = root.join("..");
        let given = std::env::var("VIBEROLLER_HUB_REPO").ok().or_else(|| cfg.get("hubRepo").and_then(Value::as_str).map(String::from));
        let repo = match given { Some(g) => std::path::absolute(g).unwrap_or_default(), None => git(&mills, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok().and_then(|c| PathBuf::from(c).parent().map(Path::to_path_buf)).unwrap_or_else(|| std::path::absolute(&mills).unwrap_or(mills.clone())) };
        let wt_root = cfg.get("worktreeRoot").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| repo.join("..").join("Mills-worktrees"));
        let projects_root = cfg.get("projectsRoot").and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| repo.join("..").join("VibeRoller-projects"));
        let node = std::env::var("VR_NODE").unwrap_or_else(|_| "node".into());
        Worktrees { repo, wt_root: std::path::absolute(&wt_root).unwrap_or(wt_root), projects_root: std::path::absolute(&projects_root).unwrap_or(projects_root), node, script: std::path::absolute(root.join("worktree.js")).unwrap_or_default() }
    }
    fn branch_exists(repo: &Path, b: &str) -> bool { git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]).is_ok() }
    fn integration(repo: &Path) -> String { if Self::branch_exists(repo, "develop") { "develop".into() } else { git(repo, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|_| "main".into()) } }
    fn project_dirs(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.projects_root).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        v.retain(|d| valid_project(d) && !d.eq_ignore_ascii_case(HUB_PROJECT) && self.projects_root.join(d).join(REPO_DIR).join(".git").exists());
        v.sort_by_key(|a| a.to_lowercase()); v
    }
    fn hub_project(&self) -> Project { Project { name: HUB_PROJECT.into(), hub: true, repo: self.repo.clone(), root: self.wt_root.clone(), main: "develop".into() } }
    /// One project by name (any case).
    pub fn project(&self, name: &str) -> Result<Project, String> {
        let name = if name.is_empty() { HUB_PROJECT } else { name };
        if !valid_project(name) { return Err("A project name is 1-64 characters: letters, digits, dots, dashes and underscores, starting with a letter or digit.".into()); }
        if name.eq_ignore_ascii_case(HUB_PROJECT) { return Ok(self.hub_project()); }
        let found = self.project_dirs().into_iter().find(|d| d.eq_ignore_ascii_case(name)).ok_or_else(|| format!("No project named \"{name}\" in {}. \"node bridge/worktree.js projects\" lists them; \"new <name>\" starts one, \"clone <url>\" pulls one in.", self.projects_root.display()))?;
        let repo = self.projects_root.join(&found).join(REPO_DIR);
        Ok(Project { main: Self::integration(&repo), name: found.clone(), hub: false, root: self.projects_root.join(&found), repo })
    }
    /// Is this a name worktree.js accepts for a project (T-238)?
    pub fn valid_name(name: &str) -> bool { valid_project(name) }
    /// Is there a project (or a folder in the projects area) of that name, any case (worktree.js exists)?
    pub fn name_taken(&self, name: &str) -> bool { name.eq_ignore_ascii_case(HUB_PROJECT) || self.project(name).is_ok() || self.projects_root.join(name).exists() }
    /// A session with no project (T-238): the person's own scratch folder, never a git repository of a project, kept
    /// out of the projects list (its name starts with "_"). The run's cwd for such a session.
    pub fn scratch(&self, user: &str) -> Result<Wt, String> {
        if !valid_user(user) { return Err("Usage: a username (lowercase letters, digits, dashes).".into()); }
        let root = self.projects_root.join("_scratch"); let dir = root.join(user);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
        Ok(Wt { dir: dir.clone(), branch: String::new(), project: NO_PROJECT.into(), repo: dir, root, main: String::new(), hub: false })
    }
    /// Every project: VibeRoller first, then by name (worktree.js listProjects).
    pub fn list(&self) -> Vec<Value> {
        let mut ps = vec![self.hub_project()];
        for d in self.project_dirs() { let repo = self.projects_root.join(&d).join(REPO_DIR); ps.push(Project { main: Self::integration(&repo), name: d.clone(), hub: false, root: self.projects_root.join(&d), repo }); }
        ps.into_iter().map(|p| {
            let people: Vec<String> = { let mut v: Vec<String> = std::fs::read_dir(&p.root).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|d| valid_user(d) && p.root.join(d).join(".git").exists()).collect()).unwrap_or_default(); v.sort(); v };
            json!({ "name": p.name, "hub": p.hub, "dir": p.repo.display().to_string(), "root": p.root.display().to_string(), "branch": p.main, "remote": git(&p.repo, &["remote", "get-url", "origin"]).unwrap_or_default(), "people": people })
        }).collect()
    }
    /// That person's worktree of the project, made on first use (worktree.js ensure).
    pub fn ensure(&self, user: &str, project: &str) -> Result<Wt, String> {
        if !valid_user(user) { return Err("Usage: a username (lowercase letters, digits, dashes).".into()); }
        let p = self.project(project)?;
        self.ensure_at(&p, user, &format!("work/{user}"), &p.main.clone())
    }
    /// The worktree of one of that person's Work sessions (plans/multi-session.md § 2a): <project>/<user>.<sid> on
    /// work/<user>.<sid>, made on first use from the person's own work/<user> (so it carries their unmerged work; from
    /// the integration branch while they have none). The dot keeps it apart from people's worktrees.
    pub fn ensure_session(&self, user: &str, sid: &str, project: &str) -> Result<Wt, String> {
        if !valid_user(user) { return Err("Usage: a username (lowercase letters, digits, dashes).".into()); }
        if sid.is_empty() || sid.len() > 16 || !sid.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) { return Err("Bad session id.".into()); }
        let p = self.project(project)?;
        let own = format!("work/{user}");
        let base = if Self::branch_exists(&p.repo, &own) { own } else { p.main.clone() };
        self.ensure_at(&p, &format!("{user}.{sid}"), &format!("work/{user}.{sid}"), &base)
    }
    /// The session's worktree path and branch, whether it exists or not (None for a bad name or project).
    pub fn session_path(&self, user: &str, sid: &str, project: &str) -> Option<(PathBuf, String, PathBuf)> { let p = self.project(project).ok()?; Some((p.root.join(format!("{user}.{sid}")), format!("work/{user}.{sid}"), p.repo)) }
    fn ensure_at(&self, p: &Project, name: &str, branch: &str, base: &str) -> Result<Wt, String> {
        let dir = p.root.join(name); let branch = branch.to_string();
        let listed = git(&p.repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
        let mut cur: Option<(PathBuf, String)> = None; let mut found: Option<String> = None;
        for line in listed.lines().chain(std::iter::once("")) {
            if let Some(d) = line.strip_prefix("worktree ") { if let Some((pd, b)) = cur.take() { if same(&pd, &dir) { found = Some(b); } } cur = Some((PathBuf::from(d), String::new())); }
            else if let Some(b) = line.strip_prefix("branch ") { if let Some(c) = cur.as_mut() { c.1 = b.trim_start_matches("refs/heads/").to_string(); } }
        }
        if let Some((pd, b)) = cur { if same(&pd, &dir) { found = Some(b); } }
        let mk = |branch: String| Wt { dir: dir.clone(), branch, project: p.name.clone(), repo: p.repo.clone(), root: p.root.clone(), main: p.main.clone(), hub: p.hub };
        if let Some(b) = found {
            if git(&dir, &["rev-parse", "--git-dir"]).is_err() { let _ = git(&p.repo, &["worktree", "repair", &dir.display().to_string()]); }
            return Ok(mk(if b.is_empty() { branch } else { b }));
        }
        if dir.exists() { return Err(format!("{} exists but is not a git worktree of {}. Move it away first.", dir.display(), p.name)); }
        let _ = std::fs::create_dir_all(&p.root);
        let _ = git(&p.repo, &["worktree", "prune"]);
        let d = dir.display().to_string();
        if Self::branch_exists(&p.repo, &branch) { git(&p.repo, &["worktree", "add", &d, &branch])?; } else { git(&p.repo, &["worktree", "add", "-b", &branch, &d, base])?; }
        Ok(mk(branch))
    }
    /// Removes a session's worktree (the session was deleted). Uncommitted work is committed first; a branch with commits
    /// the person's work/<user> does not have is kept as refs/viberoller/sessions/<user>/<sid>, so nothing is lost (the
    /// worktree law). Says what happened ("" = there was nothing).
    pub fn remove_session(&self, user: &str, sid: &str, project: &str) -> String {
        let Some((dir, branch, repo)) = self.session_path(user, sid, project) else { return String::new() };
        if !Self::branch_exists(&repo, &branch) && !dir.exists() { return String::new(); }
        if dir.exists() && git(&dir, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false) {
            let _ = git(&dir, &["add", "-A"]);
            let _ = git(&dir, &["-c", "user.name=VibeRoller hub", "-c", "user.email=hub@viberoller.invalid", "commit", "-q", "-m", "Work in progress (its Work session was deleted)"]);
        }
        let own = format!("work/{user}");
        let base = if Self::branch_exists(&repo, &own) { own } else { Self::integration(&repo) };
        let unmerged = Self::branch_exists(&repo, &branch) && git(&repo, &["rev-list", "--count", &format!("{base}..{branch}")]).map(|n| n != "0").unwrap_or(true);
        let kept = if unmerged { let r = format!("refs/viberoller/sessions/{user}/{sid}"); git(&repo, &["update-ref", &r, &branch]).ok().map(|_| r) } else { None };
        if unmerged && kept.is_none() { return format!("kept {} (its branch could not be saved elsewhere)", dir.display()); }
        if dir.exists() { let _ = git(&repo, &["worktree", "remove", "--force", &dir.display().to_string()]); }
        let _ = git(&repo, &["worktree", "prune"]);
        let _ = git(&repo, &["branch", "-D", &branch]);
        match kept { Some(r) => format!("its unmerged work is kept as {r}"), None => "removed (everything on it was merged)".into() }
    }
    /// Whose worktrees a project holds: the owner of every folder in it that is a git worktree (a session's
    /// <user>.<sid> counts for <user>), sorted, each once.
    pub fn owners(&self, p: &Project) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&p.root).map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|d| d != REPO_DIR && p.root.join(d).join(".git").exists()).filter_map(|d| { let o = d.split('.').next().unwrap_or("").to_string(); if valid_user(&o) { Some(o) } else { None } }).collect()).unwrap_or_default();
        v.sort(); v.dedup(); v
    }
    /// Removes that person's OWN worktree of a project (never anyone else's, never VibeRoller's): uncommitted work is
    /// committed first, an unmerged branch is kept as refs/viberoller/removed/<user>/<stamp> (the worktree law: nothing is
    /// lost). Ok = what happened.
    pub fn remove_own(&self, user: &str, project: &str) -> Result<String, String> {
        if !valid_user(user) { return Err("Bad username.".into()); }
        let p = self.project(project)?;
        if p.hub { return Err("VibeRoller's own worktree is not removed from here.".into()); }
        let dir = p.root.join(user);
        if !dir.join(".git").exists() { return Err(format!("You have no worktree of {}.", p.name)); }
        let listed = git(&p.repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
        if !listed.lines().filter_map(|l| l.strip_prefix("worktree ")).any(|d| same(Path::new(d), &dir)) { return Err(format!("{} is not a git worktree of {}; nothing was changed.", dir.display(), p.name)); }
        let mut notes: Vec<String> = vec![];
        if git(&dir, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(true) {
            git(&dir, &["add", "-A"]).map_err(|e| format!("Could not save your uncommitted work, so nothing was removed: {e}"))?;
            git(&dir, &["-c", "user.name=VibeRoller hub", "-c", "user.email=hub@viberoller.invalid", "commit", "-q", "-m", "Work in progress (worktree removed)"]).map_err(|e| format!("Could not commit your uncommitted work, so nothing was removed: {e}"))?;
            notes.push("your uncommitted work was committed first".into());
        }
        let branch = format!("work/{user}"); let base = p.main.clone();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let unmerged = |rev: &str| git(&p.repo, &["rev-list", "--count", &format!("{base}..{rev}")]).map(|n| n != "0").unwrap_or(true);
        let head = git(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
        let mut keep: Vec<(String, String)> = vec![];
        if Self::branch_exists(&p.repo, &branch) && unmerged(&branch) { keep.push((format!("refs/viberoller/removed/{user}/{stamp}"), branch.clone())); }
        let tip = git(&p.repo, &["rev-parse", &format!("refs/heads/{branch}")]).unwrap_or_default();
        if !head.is_empty() && head != tip && unmerged(&head) { keep.push((format!("refs/viberoller/removed/{user}/{stamp}-head"), head)); }
        for (r, to) in &keep { git(&p.repo, &["update-ref", r, to]).map_err(|e| format!("Could not keep your unmerged work as {r}, so nothing was removed: {e}"))?; notes.push(format!("unmerged work kept as {r}")); }
        git(&p.repo, &["worktree", "remove", "--force", &dir.display().to_string()]).map_err(|e| format!("Could not remove the worktree: {e}"))?;
        let _ = git(&p.repo, &["worktree", "prune"]);
        if Self::branch_exists(&p.repo, &branch) { let _ = git(&p.repo, &["branch", "-D", &branch]); }
        if keep.is_empty() { notes.push("everything on it was already merged".into()); }
        Ok(format!("Removed your worktree of {} ({}).", p.name, notes.join("; ")))
    }
    /// Moves a whole project aside to <projects root>/_trash/<name>-<stamp> (never deleted). Refused for VibeRoller and
    /// while anyone but `by` has a worktree in it. Ok = where it went.
    pub fn trash_project(&self, project: &str, by: &str) -> Result<PathBuf, String> {
        let p = self.project(project)?;
        if p.hub { return Err("VibeRoller is not a project that can be deleted.".into()); }
        let others: Vec<String> = self.owners(&p).into_iter().filter(|o| o != by).collect();
        if !others.is_empty() { return Err(format!("{} still has worktrees of {}: they remove those first.", p.name, others.join(", "))); }
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let trash = self.projects_root.join("_trash");
        std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
        let to = trash.join(format!("{}-{stamp}", p.name));
        if to.exists() { return Err("That trash folder exists already; try again in a second.".into()); }
        std::fs::rename(&p.root, &to).map_err(|e| format!("Could not move it aside: {e}"))?;
        Ok(to)
    }
    /// worktree.js itself, for what stays there (new / clone a project; the background freshness check).
    pub fn tool(&self, args: &[&str], env: &[(String, String)]) -> (bool, String, String) {
        let mut c = { #[allow(unused_mut)] let mut c = std::process::Command::new(&self.node); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c };
        c.arg(&self.script).args(args).current_dir(self.script.parent().and_then(Path::parent).unwrap_or(Path::new("."))).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
        for (k, v) in env { c.env(k, v); }
        match c.output() { Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string()), Err(e) => (false, String::new(), e.to_string()) }
    }
}
fn same(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| std::path::absolute(p).map(|x| x.display().to_string().to_lowercase()).unwrap_or_default();
    n(a) == n(b)
}
