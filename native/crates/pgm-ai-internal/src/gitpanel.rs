//! Git at a glance on the Work page (T-240): the person's OWN worktree of the session that is open (the first session
//! unless a request names `session`): what changed, stage / unstage, commit as them, and update / merge, which only go
//! through bridge/worktree.js (its laws stay in force: versioning, Forgejo first; never a raw push, never a force).
//! Refused (409) while that session's run is working; another person's worktree is never reachable (the path comes
//! from the signed-in person's own session, never from the request).
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::hub::{answer, AiHub, Resp};
use crate::sessions::{self, Sx};
use pgm_core_internal::json::s;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

/// One git call, output not trimmed (porcelain status starts with a space). (ok, stdout, stderr).
fn run(dir: &Path, args: &[String]) -> (bool, String, String) {
    match { #[allow(unused_mut)] let mut c = std::process::Command::new("git"); #[cfg(windows)] { { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }; } /* CREATE_NO_WINDOW */ c }.args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).output() {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => (false, String::new(), e.to_string()),
    }
}
fn g(dir: &Path, args: &[&str]) -> (bool, String, String) { run(dir, &args.iter().map(|x| x.to_string()).collect::<Vec<_>>()) }

/// A file name from the page: relative, inside the worktree, never an option.
fn safe_path(p: &str) -> bool {
    !p.is_empty() && p.len() <= 1024 && !p.starts_with('-') && !p.contains('\0') && !p.starts_with('/') && !p.contains(":\\") && !p.starts_with('\\')
        && !p.split(['/', '\\']).any(|c| c == "..")
}

struct Target { sx: Sx, project: String, dir: PathBuf, branch: String, base: String, own: bool, exists: bool }

fn target(hub: &Arc<dyn AiHub>, r: &Req, a: &Account, b: &Value) -> Result<Target, Resp> {
    let w = hub.work();
    let Some(sx) = sessions::resolve(a, &sessions::wanted(r, b)) else { return Err(answer(404, json!({ "error": "No such session." }))) };
    let project = w.project_in(a, &sx);
    if w.is_none(a, &sx) { return Err(answer(400, json!({ "error": "This session has no project, so there is no git worktree to show.", "noProject": true }))); }
    if !sx.own { // the person's own worktree (a promoted first session keeps its own, T-341)
        let wd = w.wt.ensure(&a.username, &project).map_err(|e| answer(500, json!({ "error": format!("Could not prepare your worktree: {e}") })))?;
        let up = format!("origin/{}", wd.main);
        let base = if g(&wd.repo, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{up}")]).0 { up } else { wd.main.clone() };
        Ok(Target { sx, project, dir: wd.dir, branch: wd.branch, base, own: true, exists: true })
    } else {
        let Some((dir, branch, _)) = w.wt.session_path(&a.username, sx.hex(), &project) else { return Err(answer(400, json!({ "error": "Unknown project." }))) };
        let exists = dir.join(".git").exists();
        Ok(Target { sx, project, dir, branch, base: format!("work/{}", a.username), own: false, exists })
    }
}

/// Changed files: {path, from?, index, tree, staged, unstaged, state}.
fn files(dir: &Path) -> Vec<Value> {
    let (_, out, _) = g(dir, &["status", "--porcelain=v1", "-z", "-uall"]);
    let mut v = vec![]; let mut it = out.split('\0');
    while let Some(e) = it.next() {
        if e.len() < 4 { continue; }
        let (x, y, path) = (e.as_bytes()[0] as char, e.as_bytes()[1] as char, e[3..].to_string());
        let from = if x == 'R' || x == 'C' { it.next().map(String::from) } else { None };
        let untracked = x == '?';
        let state = if untracked { "untracked" } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') { "conflict" }
            else { match if y != ' ' { y } else { x } { 'M' => "modified", 'A' => "added", 'D' => "deleted", 'R' => "renamed", 'C' => "copied", 'T' => "typechange", _ => "changed" } };
        let mut f = json!({ "path": path, "index": x.to_string(), "tree": y.to_string(), "staged": !untracked && x != ' ' && x != '!', "unstaged": untracked || y != ' ', "state": state });
        if let Some(fr) = from { f["from"] = json!(fr); }
        v.push(f);
    }
    v
}

fn status(hub: &Arc<dyn AiHub>, a: &Account, t: &Target) -> Value {
    let w = hub.work();
    let mut out = json!({ "session": t.sx.sid, "primary": t.sx.primary, "project": t.project, "dir": t.dir.display().to_string(), "branch": t.branch, "base": t.base,
        "kind": if t.own { "own" } else { "session" }, "working": w.busy_key(&t.sx.key), "exists": t.exists, "files": [], "ahead": 0, "behind": 0 });
    if !t.exists { out["note"] = json!("This session has no worktree yet; it is made when its first run starts."); return out; }
    let (ok, head, _) = g(&t.dir, &["symbolic-ref", "--short", "-q", "HEAD"]);
    let actual = if ok { head.trim().to_string() } else { String::new() };
    out["actual"] = json!(actual); out["rightBranch"] = json!(actual == t.branch);
    let (ok, c, _) = g(&t.dir, &["rev-list", "--left-right", "--count", &format!("HEAD...{}", t.base)]);
    if ok { let mut n = c.split_whitespace().map(|x| x.parse::<u64>().unwrap_or(0)); out["ahead"] = json!(n.next().unwrap_or(0)); out["behind"] = json!(n.next().unwrap_or(0)); } else { out["noBase"] = json!(true); }
    let fl = files(&t.dir);
    out["clean"] = json!(fl.is_empty()); out["files"] = json!(fl);
    let (_, log, _) = g(&t.dir, &["log", "-1", "--format=%h %s"]);
    out["last"] = json!(log.trim());
    let _ = a;
    out
}

/// Who a commit is by: their Forgejo login if they saved one, else their username (the email is never theirs to set).
fn identity(w: &crate::work::Work, a: &Account) -> Vec<String> {
    let login = w.forgejo_login(&a.username).unwrap_or_else(|| a.username.clone());
    let name = if a.display.trim().is_empty() { login.clone() } else { a.display.trim().to_string() };
    vec!["-c".into(), format!("user.name={name}"), "-c".into(), format!("user.email={login}@viberoller.local")]
}

/// The git panel routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let p = r.path.as_str();
    if !p.starts_with("/api/work/git") { return None; }
    let w = hub.work(); let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = if post { r.json() } else { json!({}) };
    let t = match target(hub, r, a, &b) { Ok(t) => t, Err(e) => return Some(e) };
    Some(match p {
        "/api/work/git" if get => answer(200, status(hub, a, &t)),
        "/api/work/git/stage" | "/api/work/git/unstage" | "/api/work/git/commit" | "/api/work/git/update" | "/api/work/git/merge" if post => {
            if w.busy_key(&t.sx.key) { return Some(answer(409, json!({ "error": "That session is working. Wait for its reply (or stop it), then try again." }))); }
            if !t.exists { return Some(answer(409, json!({ "error": "This session has no worktree yet." }))); }
            let what = p.rsplit('/').next().unwrap_or("");
            let text: String; let mut ok = true; let mut code = 200;
            match what {
                "stage" | "unstage" => {
                    let list: Vec<String> = b["files"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect();
                    let all = b["all"] == json!(true);
                    if !all && (list.is_empty() || list.len() > 5000 || !list.iter().all(|f| safe_path(f))) { return Some(answer(400, json!({ "error": "Name the files to " .to_string() + what + " (paths inside the worktree), or all: true." }))); }
                    let mut args: Vec<String> = if what == "stage" { vec!["add".into(), "-A".into()] } else { vec!["reset".into(), "-q".into()] };
                    args.push("--".into());
                    if all { args.push(".".into()); } else { args.extend(list); }
                    let (o, out, err) = tokio::task::block_in_place(|| run(&t.dir, &args));
                    if !o { ok = false; code = 500; text = err.trim().to_string(); } else { text = out.trim().to_string(); }
                }
                "commit" => {
                    let msg: String = s(&b, "message").trim().chars().take(4000).collect();
                    if msg.is_empty() { return Some(answer(400, json!({ "error": "A commit needs a message." }))); }
                    let staged = tokio::task::block_in_place(|| files(&t.dir)).iter().filter(|f| f["staged"] == json!(true)).count();
                    if staged == 0 { return Some(answer(400, json!({ "error": "Nothing is staged; tick the files to commit first." }))); }
                    let mut args = identity(w, a); args.extend(["commit".into(), "-q".into(), "-m".into(), msg]);
                    let (o, out, err) = tokio::task::block_in_place(|| run(&t.dir, &args));
                    if o { text = format!("Committed on {} ({}).", t.branch, g(&t.dir, &["rev-parse", "--short", "HEAD"]).1.trim()); } else { ok = false; code = 500; text = if err.trim().is_empty() { out.trim().to_string() } else { err.trim().to_string() }; }
                }
                _ => {
                    // update / merge: only worktree.js (Forgejo first, the versioning law, no force). A session's own worktree
                    // updates from the person's work/<user> and "merges" into it ("home"); the person's own one goes to develop.
                    let mut env = w.work_env(&a.username); env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
                    let u = a.username.clone(); let hex = t.sx.hex().to_string();
                    let mut args: Vec<&str> = vec!["--project", &t.project];
                    match (what, t.own) {
                        ("update", true) => args.extend(["update", &u]),
                        ("update", false) => args.extend(["update", &u, "--session", &hex]),
                        (_, true) => args.extend(["merge", &u]),
                        (_, false) => args.extend(["home", &u, "--session", &hex]),
                    }
                    let (o, out, err) = tokio::task::block_in_place(|| w.wt.tool(&args, &env));
                    text = [out.trim(), err.trim()].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join("\n");
                    ok = o && !text.contains("does not merge cleanly") && !text.contains("CONFLICT") && !text.contains("Not merged");
                    if !ok { code = 409; }
                }
            }
            let mut v = status(hub, a, &t); v["ok"] = json!(ok); v["text"] = json!(text);
            if !ok { v["error"] = json!(text); }
            answer(code, v)
        }
        _ => answer(404, json!({ "error": "Unknown endpoint" })),
    })
}
