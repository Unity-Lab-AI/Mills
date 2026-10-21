//! The board ↔ Forgejo issue mirror (moved with the board from bridge/forgejo.js; "My Forgejo", the personal tokens
//! people save in Settings, stays in the JavaScript hub, and this module reads them from bridge/forgejo-users.json).
//! Off until bridge/forgejo.json names a repo: { url, repo, board, pollMinutes, token (the hub's, optional), since }.
//!   board → Forgejo  a new task becomes an issue (title, body + "Mills task T-n"); title / body changes update it;
//!                    completing closes it; moving a done task back reopens it; deleting closes it with a comment. As the
//!                    person who made the change (their token), else with the hub token, else skipped (logged).
//!   Forgejo → board  with the hub token, every pollMinutes: closed issues complete their task, reopened ones put it back
//!                    in To do, title / body changes land on the task, an issue nobody mirrored becomes a task ("forgejo").
//! Loops are avoided by remembering, per task, the issue's updated_at last seen or produced.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::board::{Actor, Board, Change};

pub struct Mirror {
    root: PathBuf,
    cfg: Value,
    pub on: bool,
    url: String,
    base: String,
    hub_token: String,
    pub board_id: String,
    last_poll: Mutex<String>,
    busy: Mutex<HashSet<String>>,
    again: Mutex<HashMap<String, Option<Actor>>>,
    http: reqwest::Client,
}
fn strip_mark(s: &str) -> String {
    // the "Mills task T-n" line this mirror appends, at the very end
    if let Some(i) = s.rfind("\n\n---\n_Mills task T-") { let tail = &s[i..]; if tail.trim_end().ends_with('_') && !tail[5..].contains("\n\n") { return s[..i].to_string(); } }
    s.to_string()
}
impl Mirror {
    pub fn new(root: PathBuf, cfg_hub: &super::Config) -> Mirror {
        let cfg: Value = std::fs::read_to_string(root.join("forgejo.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
        let url = cfg["url"].as_str().or(cfg_hub.get("forgejoUrl").and_then(Value::as_str)).unwrap_or("https://git.unityailab.com").trim_end_matches('/').to_string();
        let on = cfg["repo"].as_str().map(|r| !r.is_empty()).unwrap_or(false);
        let base = if on { format!("{url}/api/v1/repos/{}", cfg["repo"].as_str().unwrap_or("").trim_matches('/')) } else { String::new() };
        Mirror { hub_token: cfg["token"].as_str().unwrap_or("").to_string(), board_id: cfg["board"].as_str().unwrap_or("team").to_string(), last_poll: Mutex::new(cfg["since"].as_str().unwrap_or("1970-01-01T00:00:00.000Z").to_string()),
            busy: Mutex::default(), again: Mutex::default(), http: vr_core::hub::http_client(Duration::from_secs(15)).expect("http"), root, cfg, on, url, base }
    }
    pub fn status(&self) -> Value { json!({ "on": self.on, "repo": if self.on { self.cfg["repo"].clone() } else { json!("") }, "board": self.board_id, "lastPoll": *self.last_poll.lock().unwrap(), "poll": self.on && !self.hub_token.is_empty() }) }
    fn token_for(&self, user: &str) -> String {
        std::fs::read_to_string(self.root.join("forgejo-users.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["users"][user]["token"].as_str().map(String::from)).unwrap_or_default()
    }
    async fn api(&self, method: &str, p: &str, body: Option<Value>, token: &str) -> Result<Value, String> {
        let mut rb = self.http.request(reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET), format!("{}{}", self.base, p))
            .header("Authorization", format!("token {token}")).header("Accept", "application/json").timeout(Duration::from_secs(15));
        if let Some(b) = body { rb = rb.json(&b); }
        let r = rb.send().await.map_err(|e| e.to_string())?;
        let s = r.status().as_u16();
        if !(200..300).contains(&s) { return Err(format!("Forgejo {method} {p} → {s} {}", r.text().await.unwrap_or_default().chars().take(200).collect::<String>())); }
        if s == 204 { return Ok(Value::Null); }
        r.json().await.map_err(|e| e.to_string())
    }
    fn issue_body(t: &Value) -> String { format!("{}\n\n---\n_Mills task {} — mirrored by Mills; edit it in either place._", strip_mark(t["body"].as_str().unwrap_or("")), t["id"].as_str().unwrap_or("")) }

    /// Changes made on the board, mirrored (each in the background, one call per task at a time).
    pub fn changed(self: &Arc<Self>, board: &Arc<Board>, changes: Vec<Change>) {
        if !self.on { return; }
        for (what, t, actor) in changes { let (m, b) = (self.clone(), board.clone()); tokio::spawn(async move { m.outbound(&b, what, t, actor).await; }); }
    }
    fn outbound<'a>(self: &'a Arc<Self>, board: &'a Arc<Board>, what: String, t: Value, actor: Option<Actor>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if t["board"].as_str() != Some(self.board_id.as_str()) { return; }
            let id = t["id"].as_str().unwrap_or("").to_string();
            if !self.busy.lock().unwrap().insert(id.clone()) { self.again.lock().unwrap().insert(id, actor); return; } // re-synced when the call in flight is done
            let token = actor.as_ref().map(|a| self.token_for(&a.user)).filter(|t| !t.is_empty()).unwrap_or_else(|| self.hub_token.clone());
            if token.is_empty() { println!("[vr-hub] forgejo: {id} not mirrored: {} has no Forgejo token and the hub has none.", actor.as_ref().map(|a| a.user.as_str()).unwrap_or("nobody")); self.busy.lock().unwrap().remove(&id); return; }
            if let Err(e) = self.sync(board, &what, &t, &token).await { println!("[vr-hub] forgejo: {e}"); }
            self.busy.lock().unwrap().remove(&id);
            let next = self.again.lock().unwrap().remove(&id);
            if let Some(a) = next { if let Some(cur) = board.get(&id) { let w = if cur["deleted"].as_bool() == Some(true) { "deleted" } else { "updated" }; self.outbound(board, w.to_string(), cur, a).await; } }
        })
    }
    async fn sync(&self, board: &Board, what: &str, t: &Value, token: &str) -> Result<(), String> {
        let id = t["id"].as_str().unwrap_or("");
        let ext = board.external(id);
        if what == "deleted" {
            if let Some(x) = ext { let n = x["number"].as_i64().unwrap_or(0); self.api("POST", &format!("/issues/{n}/comments"), Some(json!({ "body": format!("Deleted on the Mills board ({id}).") })), token).await?; self.api("PATCH", &format!("/issues/{n}"), Some(json!({ "state": "closed" })), token).await?; }
            return Ok(());
        }
        let done = t["status"] == json!("done");
        let Some(ext) = ext else {
            if what == "completed" && done { return Ok(()); }
            let is = self.api("POST", "/issues", Some(json!({ "title": t["title"], "body": Self::issue_body(t), "labels": [] })), token).await?;
            let n = is["number"].as_i64().unwrap_or(0);
            board.set_external(id, Some(json!({ "number": n, "url": is["html_url"], "updatedAt": is["updated_at"], "title": t["title"], "body": t["body"].as_str().unwrap_or(""), "state": if done { "closed" } else { "open" } })));
            if done { let c = self.api("PATCH", &format!("/issues/{n}"), Some(json!({ "state": "closed" })), token).await?; let mut e = board.external(id).unwrap_or(json!({})); e["updatedAt"] = c["updated_at"].clone(); e["state"] = json!("closed"); board.set_external(id, Some(e)); }
            return Ok(());
        };
        let mut patch = serde_json::Map::new();
        if t["title"] != ext["title"] { patch.insert("title".into(), t["title"].clone()); }
        if t["body"].as_str().unwrap_or("") != ext["body"].as_str().unwrap_or("") { patch.insert("body".into(), json!(Self::issue_body(t))); }
        let want = if done { "closed" } else { "open" };
        if ext["state"].as_str() != Some(want) { patch.insert("state".into(), json!(want)); }
        if patch.is_empty() { return Ok(()); }
        let n = ext["number"].as_i64().unwrap_or(0);
        let r = self.api("PATCH", &format!("/issues/{n}"), Some(Value::Object(patch)), token).await?;
        let mut e = ext.clone(); e["updatedAt"] = r["updated_at"].clone(); e["title"] = t["title"].clone(); e["body"] = json!(t["body"].as_str().unwrap_or("")); e["state"] = json!(want);
        board.set_external(id, Some(e));
        Ok(())
    }
    /// Forgejo → board, every pollMinutes (at least 3 s), with the hub token. `ops` runs a board operation as "Forgejo".
    pub async fn poll(&self, board: &Board, ops: &(dyn Fn(&str, &Value) -> Value + Send + Sync)) {
        if !self.on || self.hub_token.is_empty() { return; }
        let since = self.last_poll.lock().unwrap().clone();
        let started = super::auth::iso_now();
        let issues = match self.api("GET", &format!("/issues?state=all&type=issues&since={}&limit=50", vr_core::hub::urlencode(&since)), None, &self.hub_token).await { Ok(v) => v, Err(e) => { println!("[vr-hub] forgejo poll: {e}"); return; } };
        let ts = |s: &Value| chrono::DateTime::parse_from_rfc3339(s.as_str().unwrap_or("")).map(|d| d.timestamp_millis()).unwrap_or(0);
        for is in issues.as_array().cloned().unwrap_or_default() {
            if !is["pull_request"].is_null() { continue; }
            let n = is["number"].as_i64().unwrap_or(-1);
            if let Some(t) = board.by_external(n) {
                let ext = t["forgejo"].clone();
                if !ext["updatedAt"].is_null() && ts(&is["updated_at"]) <= ts(&ext["updatedAt"]) { continue; } // our own change, or nothing new
                let body = strip_mark(is["body"].as_str().unwrap_or(""));
                let mut patch = serde_json::Map::new();
                if is["title"].as_str().map(|x| !x.is_empty()).unwrap_or(false) && is["title"] != t["title"] { patch.insert("title".into(), is["title"].clone()); }
                if body != t["body"].as_str().unwrap_or("") { patch.insert("body".into(), json!(body)); }
                if is["state"] == json!("closed") && t["status"] != json!("done") { patch.insert("status".into(), json!("done")); }
                if is["state"] == json!("open") && t["status"] == json!("done") { patch.insert("status".into(), json!("todo")); }
                let mut e = ext.clone(); e["updatedAt"] = is["updated_at"].clone(); e["title"] = is["title"].clone(); e["body"] = json!(body); e["state"] = is["state"].clone();
                let tid = t["id"].as_str().unwrap_or("").to_string();
                board.set_external(&tid, Some(e));
                if !patch.is_empty() { patch.insert("id".into(), json!(tid)); patch.insert("skipMirror".into(), json!(true)); ops("update", &Value::Object(patch)); }
            } else if is["state"] == json!("open") && !is["body"].as_str().unwrap_or("").contains("_Mills task T-") {
                let mut labels = vec![json!("forgejo")]; for l in is["labels"].as_array().cloned().unwrap_or_default() { labels.push(l["name"].clone()); }
                let r = ops("create", &json!({ "title": is["title"], "body": format!("{}\n\n---\n_From Forgejo issue [#{n}]({})._", is["body"].as_str().unwrap_or(""), is["html_url"].as_str().unwrap_or("")), "board": self.board_id, "status": "todo", "labels": labels, "skipMirror": true }));
                if let Some(id) = r["task"]["id"].as_str() { board.set_external(id, Some(json!({ "number": n, "url": is["html_url"], "updatedAt": is["updated_at"], "title": is["title"], "body": is["body"].as_str().unwrap_or(""), "state": "open" }))); }
            }
        }
        *self.last_poll.lock().unwrap() = started.clone();
        let mut c = self.cfg.clone(); c["since"] = json!(started);
        let _ = std::fs::write(self.root.join("forgejo.json"), serde_json::to_string_pretty(&c).unwrap_or_default());
    }
    pub fn every(&self) -> Option<Duration> { if self.on && !self.hub_token.is_empty() { Some(Duration::from_millis((self.cfg["pollMinutes"].as_f64().filter(|m| *m > 0.0).unwrap_or(2.0) * 60000.0).max(3000.0) as u64)) } else { None } }
    #[allow(dead_code)]
    pub fn url(&self) -> &str { &self.url }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mark_comes_off() {
        assert_eq!(super::strip_mark("hello\n\n---\n_Mills task T-4 — mirrored by Mills; edit it in either place._"), "hello");
        assert_eq!(super::strip_mark("no mark"), "no mark");
    }
}
