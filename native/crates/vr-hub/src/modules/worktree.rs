//! Projects and per-person worktrees, as the hub needs them for Work runs (bridge/worktree.js is the full tool and stays
//! the one people and Work runs call: check / update / renew / commit / merge / live; starting and pulling in projects
//! goes through it too). Here: which projects there are, and each person's worktree of one (made on first use), the
//! same layout: <projects root>/<project>/_repo + /<username>; VibeRoller's worktrees in config worktreeRoot.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const HUB_PROJECT: &str = "VibeRoller";
const REPO_DIR: &str = "_repo";

#[derive(Clone, Debug)]
pub struct Project { pub name: String, pub hub: bool, pub repo: PathBuf, pub root: PathBuf, pub main: String }
#[derive(Clone, Debug)]
pub struct Wt { pub dir: PathBuf, pub branch: String, pub project: String, pub repo: PathBuf, pub root: PathBuf, pub main: String, pub hub: bool }

pub struct Worktrees { pub repo: PathBuf, pub wt_root: PathBuf, pub projects_root: PathBuf, pub node: String, pub script: PathBuf }
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = std::process::Command::new("git").args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}
fn valid_user(u: &str) -> bool { !u.is_empty() && u.len() <= 32 && u.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }
fn valid_project(n: &str) -> bool { !n.is_empty() && n.len() <= 64 && n.as_bytes()[0].is_ascii_alphanumeric() && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !n.contains("..") && !n.eq_ignore_ascii_case(REPO_DIR) }

impl Worktrees {
    pub fn new(root: &Path, cfg: &super::Config) -> Worktrees {
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
        let dir = p.root.join(user); let branch = format!("work/{user}");
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
        if Self::branch_exists(&p.repo, &branch) { git(&p.repo, &["worktree", "add", &d, &branch])?; } else { git(&p.repo, &["worktree", "add", "-b", &branch, &d, &p.main])?; }
        Ok(mk(branch))
    }
    /// worktree.js itself, for what stays there (new / clone a project; the background freshness check).
    pub fn tool(&self, args: &[&str], env: &[(String, String)]) -> (bool, String, String) {
        let mut c = std::process::Command::new(&self.node);
        c.arg(&self.script).args(args).current_dir(self.script.parent().and_then(Path::parent).unwrap_or(Path::new("."))).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
        for (k, v) in env { c.env(k, v); }
        match c.output() { Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string()), Err(e) => (false, String::new(), e.to_string()) }
    }
}
fn same(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| std::path::absolute(p).map(|x| x.display().to_string().to_lowercase()).unwrap_or_default();
    n(a) == n(b)
}
