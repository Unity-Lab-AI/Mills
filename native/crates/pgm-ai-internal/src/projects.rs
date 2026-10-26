//! A person's project index (T-239): which projects of the shared projects area they have a worktree in, when they last
//! worked in each (from their Work sessions), what they pinned or hid, what they have not used yet (discovery: the area's
//! other projects; their Forgejo repos, only listed), and the management: pin / hide, remove MY worktree, delete a whole
//! project (admin only, moved to <projectsRoot>/_trash, never deleted). Stored add-only in <profile>/projects.json =
//! { projects: { <name>: { pinned, hidden, usedAt } } }. Worktrees go away only through pgm-collab-internal's
//! worktree functions (the worktree law: nothing is lost, nobody else's is reachable: every path comes from the
//! signed-in person and the project's own folder).
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::hub::{answer, AiHub, Resp};
use crate::sessions::{self, Sx};
use pgm_core_internal::json::{read_json, s, write_json};
use pgm_core_internal::now_ms;
use pgm_identity_internal::Account;
use pgm_net_internal::http::Req;

static LOCK: Mutex<()> = Mutex::new(());
fn file(a: &Account) -> PathBuf { a.dir.join("projects.json") }
fn load(a: &Account) -> Value { read_json(&file(a)).filter(|v| v["projects"].is_object()).unwrap_or(json!({ "projects": {} })) }
/// Changes one project's entry (made when missing; other keys stay).
fn edit(a: &Account, project: &str, f: impl FnOnce(&mut Map<String, Value>)) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = load(a);
    let e = v["projects"].as_object_mut().unwrap().entry(project.to_string()).or_insert(json!({}));
    if let Some(m) = e.as_object_mut() { f(m); }
    write_json(&file(a), &v);
}
/// The person worked in this project just now (Work page: switching to it).
pub fn used(a: &Account, project: &str) { edit(a, project, |m| { m.insert("usedAt".into(), json!(now_ms())); }); }

/// The projects their Work sessions are in, with when each was last active.
fn session_use(w: &crate::work::Work, a: &Account) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    let v = sessions::index(a); let primary = s(&v, "primary");
    for (sid, m) in v["sessions"].as_object().cloned().unwrap_or_default() {
        let sx = Sx { key: if sid == primary { a.username.clone() } else { format!("{}#{sid}", a.username) }, primary: sid == primary, sid };
        let p = s(&sessions::sstate(w, a, &sx), "project");
        let p = if p.is_empty() { pgm_collab_internal::worktree::HUB_PROJECT.to_string() } else { p };
        let at = m["lastActivityAt"].as_u64().unwrap_or(0);
        let e = out.entry(p.to_lowercase()).or_insert(0); if at > *e { *e = at; }
    }
    out
}

fn norm_url(u: &str) -> String { u.trim().trim_end_matches('/').trim_end_matches(".git").trim_end_matches('/').to_lowercase() }

/// The lines `worktree.js repos` prints → [{ name, url, private, fork }].
pub fn parse_repos(out: &str) -> Vec<Value> {
    out.lines().filter(|l| !l.starts_with(char::is_whitespace) && !l.trim().is_empty()).filter_map(|l| {
        let url = l.split_whitespace().last().filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
        let name = l.split_whitespace().next()?;
        Some(json!({ "name": name, "url": url, "private": l.contains("(private)"), "fork": l.contains("(fork)") }))
    }).collect()
}

fn index(hub: &Arc<dyn AiHub>, a: &Account, with_forgejo: bool) -> Value {
    let w = hub.work(); let u = &a.username;
    let mine = load(a); let used = session_use(w, a);
    let list = w.wt.list();
    let mut rows: Vec<Value> = vec![]; let mut discover: Vec<String> = vec![];
    for p in list.iter() {
        let name = s(p, "name"); let hub_p = p["hub"] == json!(true);
        let has = p["people"].as_array().map(|x| x.iter().any(|n| n == &json!(u))).unwrap_or(false);
        let e = &mine["projects"][&name];
        let used_at = used.get(&name.to_lowercase()).copied().unwrap_or(0).max(e["usedAt"].as_u64().unwrap_or(0));
        let (pinned, hidden) = (e["pinned"] == json!(true), e["hidden"] == json!(true));
        let mut row = json!({ "name": name, "hub": hub_p, "dir": p["dir"], "remote": p["remote"], "branch": p["branch"], "people": p["people"], "mine": has || hub_p, "lastUsed": used_at, "pinned": pinned, "hidden": hidden,
            "working": w.working_in(Some(u), &name), "canDelete": a.admin && !hub_p });
        if has && !hub_p {
            if let Ok(pj) = w.wt.project(&name) {
                let dir = pj.root.join(u);
                let dirty = pgm_collab_internal::worktree::git(&dir, &["status", "--porcelain"]).map(|x| !x.is_empty()).unwrap_or(false);
                let ahead = pgm_collab_internal::worktree::git(&dir, &["rev-list", "--count", &format!("{}..HEAD", pj.main)]).ok().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                let on = pgm_collab_internal::worktree::git(&dir, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
                row["worktree"] = json!({ "dir": dir.display().to_string(), "branch": on, "dirty": dirty, "ahead": ahead });
                row["othersWorktrees"] = json!(w.wt.owners(&pj).into_iter().filter(|o| o != u).collect::<Vec<_>>());
            }
        } else if !hub_p { row["othersWorktrees"] = p["people"].clone(); }
        if !hub_p && !has && used_at == 0 && !hidden { discover.push(name.clone()); }
        rows.push(row);
    }
    // pinned first, then the most recently used, then by name
    rows.sort_by(|x, y| (y["pinned"] == json!(true)).cmp(&(x["pinned"] == json!(true))).then(y["lastUsed"].as_u64().cmp(&x["lastUsed"].as_u64())).then(s(x, "name").to_lowercase().cmp(&s(y, "name").to_lowercase())));
    let mut out = json!({ "projects": rows, "discover": discover, "admin": a.admin, "trash": w.wt.projects_root.join("_trash").display().to_string() });
    if with_forgejo {
        out["forgejo"] = if w.forgejo_login(u).is_none() { json!({ "available": false, "note": "Save your Forgejo token in Settings to see your repos here.", "repos": [] }) } else {
            let env = w.work_env(u); let wt = &w.wt;
            let (ok, text, err) = tokio::task::block_in_place(|| wt.tool(&["repos"], &env));
            if !ok { json!({ "available": true, "error": err.trim().lines().last().unwrap_or("Forgejo did not answer.").to_string(), "repos": [] }) } else {
                let have: Vec<String> = list.iter().map(|p| norm_url(&s(p, "remote"))).filter(|x| !x.is_empty()).collect();
                let repos: Vec<Value> = parse_repos(&text).into_iter().map(|mut r| { r["inArea"] = json!(have.contains(&norm_url(&s(&r, "url")))); r }).collect();
                json!({ "available": true, "repos": repos, "note": "Listed only: pull one in with Pull in a repo." })
            }
        };
    }
    out
}

/// The project routes (signed in as `a`); None = not one of them.
pub async fn route(hub: &Arc<dyn AiHub>, r: &Req, a: &Account) -> Option<Resp> {
    let p = r.path.as_str();
    if !p.starts_with("/api/projects/") { return None; }
    let w = hub.work(); let u = &a.username;
    let (get, post) = (r.method == hyper::Method::GET, r.method == hyper::Method::POST);
    let b = if post { r.json() } else { json!({}) };
    // the project a request names, as the area calls it (any case); never VibeRoller for what changes things
    let named = |need_other: bool| -> Result<pgm_collab_internal::worktree::Project, Resp> {
        let pj = w.wt.project(&s(&b, "project")).map_err(|e| answer(404, json!({ "error": e })))?;
        if pj.hub && need_other { return Err(answer(400, json!({ "error": "VibeRoller is the hub's own project: it cannot be changed here." }))); }
        Ok(pj)
    };
    Some(match p {
        "/api/projects/mine" if get => answer(200, index(hub, a, r.param("forgejo").as_deref() == Some("1"))),
        "/api/projects/pin" | "/api/projects/hide" if post => {
            let pj = match named(false) { Ok(x) => x, Err(e) => return Some(e) };
            let key = if p.ends_with("pin") { "pinned" } else { "hidden" };
            let on = b["on"].as_bool().unwrap_or(true);
            edit(a, &pj.name, |m| { m.insert(key.into(), json!(on)); });
            answer(200, json!({ "ok": true, "project": pj.name, key: on }))
        }
        "/api/projects/remove-worktree" if post => {
            let pj = match named(true) { Ok(x) => x, Err(e) => return Some(e) };
            if w.working_in(Some(u), &pj.name) { return Some(answer(409, json!({ "error": "One of your sessions is working in this project. Wait for it (or stop it), then try again." }))); }
            let wt = &w.wt; let (user, name) = (u.clone(), pj.name.clone());
            match tokio::task::block_in_place(|| wt.remove_own(&user, &name)) {
                Ok(msg) => { w.projects_changed(); answer(200, json!({ "ok": true, "project": pj.name, "message": msg })) }
                Err(e) => answer(409, json!({ "error": e })),
            }
        }
        "/api/projects/delete" if post => {
            if !a.admin { return Some(answer(403, json!({ "error": "Only an admin can delete a project." }))); }
            let pj = match named(true) { Ok(x) => x, Err(e) => return Some(e) };
            if w.working_in(None, &pj.name) { return Some(answer(409, json!({ "error": "A session is working in this project. Wait for it to finish, then try again." }))); }
            let wt = &w.wt; let (by, name) = (u.clone(), pj.name.clone());
            match tokio::task::block_in_place(|| wt.trash_project(&name, &by)) {
                Ok(to) => { w.projects_changed(); answer(200, json!({ "ok": true, "project": pj.name, "trash": to.display().to_string(), "message": format!("{} was moved aside to {} (not deleted): move it back to restore it.", pj.name, to.display()) })) }
                Err(e) => answer(409, json!({ "error": e })),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repos_lines_parse() {
        let out = "me/app (private)  https://git.example/me/app.git\n    a description\norg/lib (fork)  https://git.example/org/lib.git\nNo repo matches.\n";
        let v = parse_repos(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["name"], "me/app"); assert_eq!(v[0]["private"], true); assert_eq!(v[1]["fork"], true);
        assert!(parse_repos("No repo matches.").is_empty());
    }
}
