//! The store and run API's checks at the library level (plans/hyper-workflows.md section 16, T-204: R437 to R444),
//! with a fake directory of people and the fake host. The hub's own routes are checked over HTTP by
//! bridge/test/regress.js (--rust-hub).
use pgm_workflow_engine_internal::fake::{FakeHost, Reply};
use pgm_workflow_engine_internal::service::{Answer, Caller, Directory, Service};
use pgm_workflow_engine_internal::{Engine, WorkflowHost};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Dir {
    users: Vec<&'static str>,
    admins: Vec<&'static str>,
    teams: Vec<(&'static str, Vec<&'static str>)>,
    host: Mutex<Arc<FakeHost>>,
    changes: Mutex<Vec<(String, Value)>>,
}

impl Directory for Dir {
    fn exists(&self, u: &str) -> bool { self.users.contains(&u) }
    fn is_admin(&self, u: &str) -> bool { self.admins.contains(&u) }
    fn scopes_of(&self, u: &str) -> Vec<String> {
        let mut out = vec![format!("me-{u}")];
        for (t, members) in &self.teams { if members.contains(&u) { out.push(format!("team-{t}")); } }
        out.push("company".into());
        out
    }
    fn host_for(&self, _u: &str) -> Arc<dyn WorkflowHost> { self.host.lock().unwrap().clone() }
    fn changed(&self, u: &str, e: &Value) { self.changes.lock().unwrap().push((u.to_string(), e.clone())); }
    fn secret_values(&self) -> Vec<String> { vec!["hub-key-0000".into()] }
}

fn dir(host: &Arc<FakeHost>) -> Arc<Dir> {
    Arc::new(Dir { users: vec!["gee", "sponge", "mills", "outsider"], admins: vec!["sponge"], teams: vec![("core", vec!["gee", "sponge", "mills"])], host: Mutex::new(host.clone()), changes: Mutex::new(vec![]) })
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hw-svc-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn svc(root: &std::path::Path, d: &Arc<Dir>) -> Arc<Service> { Service::new(root.to_path_buf(), Engine::with_builtins(), d.clone()) }

async fn call_as(s: &Service, user: &str, run_only: bool, method: &str, path: &str, q: &[(&str, &str)], body: Value) -> (u16, Value) {
    let query: HashMap<String, String> = q.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    match s.handle(&Caller { user: user.into(), run_only }, method, path, &query, &body).await {
        Answer::Json(st, v) => (st, v),
        Answer::Lines(mut rx) => {
            let mut lines = vec![];
            while let Some(l) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.ok().flatten() { lines.push(serde_json::from_str::<Value>(l.trim()).unwrap()); }
            (200, Value::Array(lines))
        }
    }
}
async fn call(s: &Service, user: &str, method: &str, path: &str, q: &[(&str, &str)], body: Value) -> (u16, Value) { call_as(s, user, false, method, path, q, body).await }

fn notes_doc(name: &str) -> Value {
    json!({ "schema": 1, "name": name, "nodes": [{ "id": "s", "kind": "start" }, { "id": "p", "kind": "prompt" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p.in" }, { "id": "e2", "from": "p.text", "to": "e.result" }] })
}

async fn until_done(s: &Service, user: &str, run: &str) -> Value {
    for _ in 0..200 {
        let (_, m) = call(s, user, "GET", "run", &[("id", run)], json!({})).await;
        if !matches!(m["status"].as_str(), Some("running")) { return m; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the run {run} did not finish");
}

/// gee makes `name` in `scope` and publishes it; returns its id.
async fn published(s: &Service, user: &str, scope: &str, name: &str) -> String {
    let (st, saved) = call(s, user, "POST", "save", &[], json!({ "doc": notes_doc(name), "scope": scope })).await;
    assert_eq!(st, 200, "{saved}");
    let id = saved["id"].as_str().unwrap().to_string();
    let (st, p) = call(s, user, "POST", "publish", &[], json!({ "id": id, "message": "first" })).await;
    assert_eq!(st, 200, "{p}");
    id
}

#[tokio::test]
async fn r437_save_get_versions_restore_publish_diff_and_a_stale_etag() {
    let root = tmp("r437");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let (st, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": notes_doc("notes") })).await;
    assert_eq!(st, 200, "{saved}");
    let (id, etag) = (saved["id"].as_str().unwrap().to_string(), saved["etag"].as_str().unwrap().to_string());
    assert!(saved["problems"].as_array().unwrap().iter().all(|p| p["level"] != "error"), "{saved}");
    let (st, g) = call(&s, "gee", "GET", "get", &[("id", &id)], json!({})).await;
    assert_eq!((st, g["etag"].as_str()), (200, Some(etag.as_str())), "the editor gets the draft");
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": id })).await.0, 400, "publishing needs a message");
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": id, "message": "v1" })).await.1["version"], 1);
    let mut d2 = notes_doc("notes");
    d2["nodes"].as_array_mut().unwrap().push(json!({ "id": "c", "kind": "comment" }));
    let (st, s2) = call(&s, "gee", "POST", "save", &[], json!({ "id": id, "doc": d2, "etag": etag })).await;
    assert_eq!(st, 200, "{s2}");
    let (st, stale) = call(&s, "gee", "POST", "save", &[], json!({ "id": id, "doc": notes_doc("notes"), "etag": etag })).await;
    assert_eq!(st, 409, "an old etag is refused: {stale}");
    assert_eq!(call(&s, "gee", "POST", "publish", &[], json!({ "id": id, "message": "v2" })).await.1["version"], 2);
    let (_, vs) = call(&s, "gee", "GET", "versions", &[("id", &id)], json!({})).await;
    assert_eq!(vs["versions"].as_array().unwrap().iter().map(|v| v["message"].clone()).collect::<Vec<_>>(), [json!("v1"), json!("v2")]);
    let (_, d) = call(&s, "gee", "GET", "diff", &[("id", &id), ("a", "1"), ("b", "2")], json!({})).await;
    assert!(d["summary"].to_string().contains('c'), "{d}");
    assert_eq!(call(&s, "gee", "POST", "restore", &[], json!({ "id": id, "version": 1 })).await.0, 200);
    let (_, g) = call(&s, "gee", "GET", "get", &[("id", &id), ("version", "draft")], json!({})).await;
    assert_eq!(g["doc"]["nodes"].as_array().unwrap().len(), 3, "version 1 is the draft again");
    let (_, g) = call(&s, "gee", "GET", "get", &[("id", &id), ("version", "1")], json!({})).await;
    assert_eq!(g["version"], 1);
    let bad = json!({ "schema": 1, "name": "broken", "nodes": [{ "id": "s", "kind": "start" }, { "id": "s2", "kind": "start" }], "edges": [] });
    let (_, b) = call(&s, "gee", "POST", "save", &[], json!({ "doc": bad })).await;
    let (st, p) = call(&s, "gee", "POST", "publish", &[], json!({ "id": b["id"], "message": "x" })).await;
    assert_eq!(st, 422, "a draft with errors is saved but not published: {p}");
}

#[tokio::test]
async fn r438_rights_viewer_editor_run_only() {
    let root = tmp("r438");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let id = published(&s, "gee", "team-core", "review").await;
    // mills is in the team: may run and view, not edit
    let (st, _) = call(&s, "mills", "POST", "save", &[], json!({ "id": id, "doc": notes_doc("review") })).await;
    assert_eq!(st, 403, "a viewer cannot save");
    call(&s, "gee", "POST", "perms", &[], json!({ "id": id, "perms": { "edit": ["mills"] } })).await;
    assert_eq!(call(&s, "mills", "POST", "save", &[], json!({ "id": id, "doc": notes_doc("review") })).await.0, 200, "an editor saves");
    assert_eq!(call(&s, "mills", "POST", "publish", &[], json!({ "id": id, "message": "x" })).await.0, 403, "an editor cannot publish");
    // a run-only client (jcode) as gee, the owner
    for (path, body) in [("save", json!({ "id": id, "doc": notes_doc("review") })), ("publish", json!({ "id": id, "message": "x" })), ("delete", json!({ "id": id })), ("perms", json!({ "id": id, "perms": {} }))] {
        assert_eq!(call_as(&s, "gee", true, "POST", path, &[], body).await.0, 403, "run-only: {path}");
    }
    let (st, l) = call_as(&s, "gee", true, "GET", "list", &[], json!({})).await;
    assert_eq!((st, l["items"][0]["name"].as_str()), (200, Some("review")));
    assert_eq!(l["items"][0]["rights"]["edit"], false);
    let (st, g) = call_as(&s, "gee", true, "GET", "get", &[("id", &id)], json!({})).await;
    assert_eq!((st, g["version"].as_u64()), (200, Some(1)), "run-only gets the published version, not a draft");
    let (st, r) = call_as(&s, "gee", true, "POST", "run", &[], json!({ "id": id, "prompt": "go" })).await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(until_done(&s, "gee", r["run"].as_str().unwrap()).await["status"], "ok");
    // the team's admin may set rights and delete
    assert_eq!(call(&s, "mills", "POST", "delete", &[], json!({ "id": id })).await.0, 403);
    assert_eq!(call(&s, "sponge", "POST", "delete", &[], json!({ "id": id })).await.0, 200);
    assert_eq!(call(&s, "gee", "GET", "get", &[("id", &id)], json!({})).await.0, 404, "deleted (archived)");
}

#[tokio::test]
async fn r439_scopes_team_personal_same_name_and_promote() {
    let root = tmp("r439");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let team = published(&s, "gee", "team-core", "test").await;
    let mine = published(&s, "gee", "me-gee", "test").await;
    assert_eq!(call(&s, "mills", "GET", "get", &[("id", &team)], json!({})).await.0, 200, "a team member sees the team's");
    assert_eq!(call(&s, "outsider", "GET", "get", &[("id", &team)], json!({})).await.0, 404, "someone outside does not");
    assert_eq!(call(&s, "outsider", "POST", "run", &[], json!({ "id": team, "prompt": "x" })).await.0, 404);
    assert_eq!(call(&s, "sponge", "GET", "get", &[("id", &mine)], json!({})).await.0, 404, "a personal scope is private (even to an admin)");
    let (_, l) = call(&s, "gee", "GET", "list", &[("q", "te")], json!({})).await;
    let shown: Vec<(String, String)> = l["items"].as_array().unwrap().iter().map(|i| (i["scope"].as_str().unwrap().into(), i["shown"].as_str().unwrap().into())).collect();
    assert_eq!(shown, [("me-gee".to_string(), "test".to_string()), ("team-core".to_string(), "core/test".to_string())], "both list, the nearer by name, the other with its scope");
    let (_, l) = call(&s, "mills", "GET", "list", &[("q", "te")], json!({})).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    assert_eq!(call(&s, "gee", "POST", "save", &[], json!({ "doc": notes_doc("test"), "scope": "team-core" })).await.0, 409, "names are unique within a scope");
    assert_eq!(call(&s, "outsider", "POST", "save", &[], json!({ "doc": notes_doc("x"), "scope": "team-core" })).await.0, 403);
    let (st, p) = call(&s, "gee", "POST", "promote", &[], json!({ "id": mine, "scope": "company" })).await;
    assert_eq!(st, 200, "{p}");
    let (_, vs) = call(&s, "outsider", "GET", "versions", &[("id", p["id"].as_str().unwrap())], json!({})).await;
    assert_eq!(vs["versions"].as_array().unwrap().len(), 1, "promoted with its history, visible company-wide");
    let (_, l) = call(&s, "gee", "GET", "list", &[("since", &l["rev"].to_string())], json!({})).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 1, "since: only what changed (the promoted copy)");
}

#[tokio::test]
async fn r440_a_run_pins_its_version_and_its_log_streams() {
    let root = tmp("r440");
    let host = FakeHost::new();
    host.delay("p", 150);
    let s = svc(&root, &dir(&host));
    let id = published(&s, "gee", "me-gee", "notes").await;
    let (st, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "notes", "prompt": "hi" })).await;
    assert_eq!((st, r["runner"].as_str()), (202, Some("hub")), "{r}");
    let run = r["run"].as_str().unwrap().to_string();
    // a new version while it runs changes nothing for it
    call(&s, "gee", "POST", "save", &[], json!({ "id": id, "doc": notes_doc("notes") })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": id, "message": "v2" })).await;
    let (_, lines) = call(&s, "gee", "GET", "log", &[("id", &run), ("follow", "1")], json!({})).await;
    let types: Vec<&str> = lines.as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types.first(), Some(&"run_start"));
    assert_eq!(types.last(), Some(&"run_end"), "follow streams to the end: {types:?}");
    assert_eq!(lines[0]["workflow"]["version"], 1, "pinned at the version it started with");
    let m = until_done(&s, "gee", &run).await;
    assert_eq!((m["status"].as_str(), m["result"].as_str()), (Some("ok"), Some("ai:p:hi")));
    let (_, again) = call(&s, "mills", "GET", "log", &[("id", &run)], json!({})).await;
    assert!(again.get("error").is_some(), "another person does not see a personal run's log");
    let (_, again) = call(&s, "gee", "GET", "log", &[("id", &run), ("from", "3")], json!({})).await;
    assert_eq!(again[0]["seq"], 3, "a page that reloads attaches from where it was");
    let (_, runs) = call(&s, "gee", "GET", "runs", &[("mine", "1")], json!({})).await;
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn r441_preflight_problems_answer_422_and_start_nothing() {
    let root = tmp("r441");
    let host = FakeHost::new();
    let s = svc(&root, &dir(&host));
    let mut d = notes_doc("needs");
    d["needsProject"] = json!(true);
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": d })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    let (st, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "needs", "prompt": "x" })).await;
    assert_eq!(st, 422, "{r}");
    assert_eq!(r["problems"][0]["code"], "engine.no-project");
    let (_, runs) = call(&s, "gee", "GET", "runs", &[], json!({})).await;
    assert!(runs["runs"].as_array().unwrap().is_empty(), "nothing started");
    let (st, _) = call(&s, "gee", "POST", "run", &[], json!({ "name": "needs", "prompt": "x", "project": "VibeRoller" })).await;
    assert_eq!(st, 202);
}

#[tokio::test]
async fn r442_an_approval_waits_and_the_answer_route_wakes_it() {
    let root = tmp("r442");
    let host = FakeHost::new();
    let d = dir(&host);
    let s = svc(&root, &d);
    let doc = json!({ "schema": 1, "name": "ship", "nodes": [{ "id": "s", "kind": "start" }, { "id": "a", "kind": "approve", "config": { "timeoutSec": 600 } }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "a.in" }, { "id": "e2", "from": "a.approved", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": doc })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "ship", "prompt": "v5" })).await;
    let run = r["run"].as_str().unwrap().to_string();
    let m = until_done(&s, "gee", &run).await;
    assert_eq!((m["status"].as_str(), m["waiting"][0]["key"].as_str()), (Some("waiting"), Some("a")), "{m}");
    assert!(d.changes.lock().unwrap().iter().any(|(u, e)| u == "gee" && e["type"] == "hyper_question" && e["key"] == "a"), "the person's pages hear the question");
    assert_eq!(call(&s, "mills", "POST", "answer", &[], json!({ "run": run, "node": "a", "answers": { "approved": true } })).await.0, 404);
    assert_eq!(call(&s, "gee", "POST", "answer", &[], json!({ "run": run, "node": "a", "answers": { "approved": true } })).await.0, 200);
    let m = until_done(&s, "gee", &run).await;
    assert_eq!((m["status"].as_str(), m["result"].as_str()), (Some("ok"), Some("v5")), "{m}");
    assert_eq!(call(&s, "gee", "POST", "answer", &[], json!({ "run": run, "node": "a", "answers": {} })).await.0, 409);
}

#[tokio::test]
async fn r443_a_hub_restart_mid_run_resumes_from_the_log() {
    let root = tmp("r443");
    let first = FakeHost::new();
    first.reply("p2", Reply::Hang);
    let d = dir(&first);
    let s = svc(&root, &d);
    let doc = json!({ "schema": 1, "name": "two", "nodes": [{ "id": "s", "kind": "start" }, { "id": "p1", "kind": "prompt" }, { "id": "p2", "kind": "prompt" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.prompt", "to": "p1.in" }, { "id": "e2", "from": "p1.text", "to": "p2.in" }, { "id": "e3", "from": "p2.text", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": doc })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "two", "prompt": "hi" })).await;
    let run = r["run"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(150)).await;
    // the hub goes away (its run is cut) and comes back: a new service on the same files, a new host
    let second = FakeHost::new();
    *d.host.lock().unwrap() = second.clone();
    let s2 = svc(&root, &d);
    s2.resume_all(true);
    let m = until_done(&s2, "gee", &run).await;
    assert_eq!((m["status"].as_str(), m["result"].as_str()), (Some("ok"), Some("ai:p2:ai:p1:hi")), "{m}");
    assert!(second.calls_of("p1").is_empty(), "the finished node was not run again");
    let (_, lines) = call(&s2, "gee", "GET", "log", &[("id", &run)], json!({})).await;
    assert!(lines.as_array().unwrap().iter().any(|e| e["type"] == "run_resume"), "the log says it resumed");
}

#[tokio::test]
async fn r444_a_secret_is_never_in_the_log_or_an_answer() {
    let root = tmp("r444");
    let host = FakeHost::new();
    host.secret("FORGEJO", "sekrit-token-123");
    host.reply("p", Reply::text("the hub key is hub-key-0000"));
    let s = svc(&root, &dir(&host));
    let doc = json!({ "schema": 1, "name": "leak", "nodes": [{ "id": "s", "kind": "start" },
            { "id": "t", "kind": "tool", "config": { "http": { "url": "https://forgejo.example/api", "headers": { "authorization": "token {{secret:FORGEJO}}" } } } },
            { "id": "p", "kind": "prompt" }, { "id": "e", "kind": "end" }],
        "edges": [{ "id": "e1", "from": "s.inputs", "to": "t.args" }, { "id": "e2", "from": "t.result", "to": "p.in" }, { "id": "e3", "from": "p.text", "to": "e.result" }] });
    let (_, saved) = call(&s, "gee", "POST", "save", &[], json!({ "doc": doc })).await;
    call(&s, "gee", "POST", "publish", &[], json!({ "id": saved["id"], "message": "x" })).await;
    let (_, r) = call(&s, "gee", "POST", "run", &[], json!({ "name": "leak", "prompt": "x" })).await;
    let run = r["run"].as_str().unwrap().to_string();
    until_done(&s, "gee", &run).await;
    assert_eq!(host.http_sent()[0].headers[0].1, "token sekrit-token-123", "the call itself had it");
    let (_, lines) = call(&s, "gee", "GET", "log", &[("id", &run)], json!({})).await;
    let text = lines.to_string();
    assert!(!text.contains("sekrit-token-123") && !text.contains("hub-key-0000"), "{text}");
    assert!(text.contains("***"), "scrubbed, not dropped");
    let file = std::fs::read_to_string(root.join("runs").join(&run).join("log.ndjson")).unwrap();
    assert!(!file.contains("sekrit-token-123") && !file.contains("hub-key-0000"));
}
